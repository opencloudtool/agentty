//! Foreground-owned cache of background process-accounting samples.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use crate::domain::resource::SessionResources;
use crate::domain::session::SessionId;
use crate::infra::process_identity::ProcessIdentity;
use crate::infra::resource::{self, ProcessSample, ResourceClient};

/// Samples all tracked session roots together, at most once every two seconds.
pub(super) struct ResourceMonitor {
    pub(super) values: HashMap<SessionId, SessionResources>,
    client: Arc<dyn ResourceClient>,
    deadline: Option<Instant>,
    /// First observed native identity; `None` permanently invalidates this root
    /// until its PID is removed or replaced in the session handles.
    identities: HashMap<SessionId, Option<ProcessIdentity>>,
    pending: Option<JoinHandle<Option<Vec<ProcessSample>>>>,
    requested_roots: HashMap<SessionId, u32>,
    sampled_roots: HashMap<SessionId, u32>,
}

impl ResourceMonitor {
    pub(super) fn new(client: Arc<dyn ResourceClient>) -> Self {
        Self {
            values: HashMap::new(),
            client,
            deadline: None,
            pending: None,
            requested_roots: HashMap::new(),
            sampled_roots: HashMap::new(),
            identities: HashMap::new(),
        }
    }

    /// Reduces finished samples and schedules work without waiting for host
    /// I/O. Replaced or removed roots invalidate their cached values
    /// immediately. Missing, exited, or reused roots stay unavailable until
    /// the tracked runtime changes.
    pub(super) async fn refresh(&mut self, roots: HashMap<SessionId, u32>, now: Instant) -> bool {
        let previous = self.values.clone();
        self.values.retain(|id, _| {
            roots
                .get(id)
                .is_some_and(|pid| self.sampled_roots.get(id) == Some(pid))
        });
        self.identities.retain(|id, _| {
            roots
                .get(id)
                .is_some_and(|pid| self.sampled_roots.get(id) == Some(pid))
        });
        self.sampled_roots.retain(|id, _| roots.contains_key(id));
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(pending) = self.pending.take()
        {
            self.values.clear();
            if let Ok(Some(samples)) = pending.await {
                self.apply_samples(&roots, &samples);
            }
            self.sampled_roots.clone_from(&self.requested_roots);
        }
        if self.pending.is_none()
            && !roots.is_empty()
            && self.deadline.is_none_or(|deadline| now >= deadline)
        {
            let pids = roots.values().copied().collect();
            self.requested_roots = roots;
            self.deadline = Some(now + Duration::from_secs(2));
            let client = Arc::clone(&self.client);
            self.pending = Some(tokio::spawn(async move { client.sample(pids).await }));
        }

        previous != self.values
    }

    /// Applies totals only for unchanged tracked roots with matching live
    /// process identities, permanently invalidating roots that no longer match.
    fn apply_samples(&mut self, roots: &HashMap<SessionId, u32>, samples: &[ProcessSample]) {
        for (id, pid) in &self.requested_roots {
            if roots.get(id) != Some(pid) {
                continue;
            }
            let root = samples.iter().find(|sample| sample.pid == *pid);
            let identity = root
                .filter(|sample| sample.is_alive)
                .and_then(|sample| sample.identity);
            let expected = self.identities.entry(id.clone()).or_insert(identity);
            if identity.is_some()
                && *expected == identity
                && let Some(resources) = resource::process_tree_resources(samples, *pid)
            {
                self.values.insert(id.clone(), resources);
            } else {
                *expected = None;
            }
        }
    }
}

impl Drop for ResourceMonitor {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::resource::MockResourceClient;

    async fn finish_sample(monitor: &ResourceMonitor) {
        while !monitor.pending.as_ref().expect("sample task").is_finished() {
            tokio::task::yield_now().await;
        }
    }

    fn process_sample(pid: u32, identity: u128, is_alive: bool) -> ProcessSample {
        ProcessSample {
            is_alive,
            parent_pid: 1,
            pid,
            resources: SessionResources {
                cpu_percent: 12.5,
                process_count: 1,
                resident_memory_kib: 2048,
            },
            identity: Some(ProcessIdentity(identity)),
        }
    }

    #[tokio::test]
    async fn idle_exit_or_same_second_pid_reuse_invalidates_root_until_tracking_changes() {
        // Arrange
        for invalid_root in [
            Vec::new(),
            vec![process_sample(10, 1_000_001, false)],
            vec![process_sample(10, 1_000_002, true)],
            vec![ProcessSample {
                identity: None,
                ..process_sample(10, 1_000_001, true)
            }],
        ] {
            let mut client = MockResourceClient::new();
            let mut snapshots = vec![
                Some(vec![process_sample(10, 1_000_001, true)]),
                None,
                Some(invalid_root),
                Some(vec![process_sample(10, 1_000_001, true)]),
                Some(vec![process_sample(10, 2_000_001, true)]),
                Some(vec![process_sample(20, 3_000_001, true)]),
            ]
            .into_iter();
            client
                .expect_sample()
                .times(6)
                .returning(move |_| snapshots.next().expect("expected snapshot"));
            let mut monitor = ResourceMonitor::new(Arc::new(client));
            let roots = HashMap::from([(SessionId::from("session"), 10)]);
            let now = Instant::now();

            // Act / Assert
            for (index, available) in [true, false, false, false].into_iter().enumerate() {
                let sample_time = now + Duration::from_secs(index as u64 * 2);
                monitor.refresh(roots.clone(), sample_time).await;
                finish_sample(&monitor).await;
                monitor.refresh(roots.clone(), sample_time).await;
                assert_eq!(monitor.values.contains_key("session"), available);
            }
            monitor
                .refresh(HashMap::new(), now + Duration::from_secs(7))
                .await;
            monitor
                .refresh(roots.clone(), now + Duration::from_secs(8))
                .await;
            finish_sample(&monitor).await;
            monitor.refresh(roots, now + Duration::from_secs(8)).await;
            assert_eq!(monitor.values["session"].process_count, 1);

            let replacement = HashMap::from([(SessionId::from("session"), 20)]);
            monitor
                .refresh(replacement.clone(), now + Duration::from_secs(10))
                .await;
            assert!(monitor.values.is_empty());
            finish_sample(&monitor).await;
            monitor
                .refresh(replacement, now + Duration::from_secs(10))
                .await;
            assert_eq!(monitor.values["session"].process_count, 1);
        }
    }

    #[tokio::test]
    async fn root_already_exited_at_first_sample_never_attaches_to_recycled_pid() {
        // Arrange
        let mut client = MockResourceClient::new();
        let mut snapshots = vec![
            vec![process_sample(10, 1_000_001, false)],
            vec![process_sample(10, 1_000_002, true)],
        ]
        .into_iter();
        client
            .expect_sample()
            .times(2)
            .returning(move |_| snapshots.next());
        let mut monitor = ResourceMonitor::new(Arc::new(client));
        let roots = HashMap::from([(SessionId::from("session"), 10)]);
        let now = Instant::now();

        // Act / Assert
        for offset in [0, 2] {
            let sample_time = now + Duration::from_secs(offset);
            monitor.refresh(roots.clone(), sample_time).await;
            finish_sample(&monitor).await;
            monitor.refresh(roots.clone(), sample_time).await;
            assert!(monitor.values.is_empty());
        }
    }

    #[tokio::test]
    async fn dropping_monitor_aborts_in_flight_sampling() {
        // Arrange
        let mut monitor = ResourceMonitor::new(Arc::new(MockResourceClient::new()));
        let pending =
            tokio::spawn(async { std::future::pending::<Option<Vec<ProcessSample>>>().await });
        let abort = pending.abort_handle();
        monitor.pending = Some(pending);

        // Act
        drop(monitor);
        tokio::task::yield_now().await;

        // Assert
        assert!(abort.is_finished());
    }

    #[tokio::test]
    async fn sampling_is_throttled_and_clears_exited_or_replaced_sessions() {
        // Arrange
        let mut client = MockResourceClient::new();
        client.expect_sample().times(1).returning(|_| {
            Some(vec![ProcessSample {
                is_alive: true,
                identity: Some(ProcessIdentity(1_000_001)),
                parent_pid: 1,
                pid: 10,
                resources: SessionResources {
                    process_count: 1,
                    cpu_percent: 12.5,
                    resident_memory_kib: 2048,
                },
            }])
        });
        let mut monitor = ResourceMonitor::new(Arc::new(client));
        let roots = HashMap::from([(SessionId::from("session"), 10)]);
        let now = Instant::now();

        // Act
        assert!(!monitor.refresh(roots.clone(), now).await);
        finish_sample(&monitor).await;
        assert!(monitor.refresh(roots.clone(), now).await);

        // Assert
        assert_eq!(monitor.values["session"].process_count, 1);
        assert!(!monitor.refresh(roots, now).await);
        assert!(
            monitor
                .refresh(HashMap::from([(SessionId::from("session"), 20)]), now)
                .await
        );
        assert!(monitor.values.is_empty());
        assert!(!monitor.refresh(HashMap::new(), now).await);
    }

    #[tokio::test]
    async fn missing_root_clears_previous_sample_even_when_descendants_remain() {
        // Arrange
        let mut client = MockResourceClient::new();
        let mut sequence = mockall::Sequence::new();
        let resources = SessionResources {
            cpu_percent: 12.5,
            process_count: 1,
            resident_memory_kib: 2048,
        };
        client
            .expect_sample()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                Some(vec![ProcessSample {
                    is_alive: true,
                    identity: Some(ProcessIdentity(1_000_001)),
                    parent_pid: 1,
                    pid: 10,
                    resources,
                }])
            });
        client
            .expect_sample()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                Some(vec![ProcessSample {
                    is_alive: true,
                    identity: Some(ProcessIdentity(1_000_001)),
                    parent_pid: 10,
                    pid: 20,
                    resources,
                }])
            });
        let mut monitor = ResourceMonitor::new(Arc::new(client));
        let roots = HashMap::from([(SessionId::from("session"), 10)]);
        let now = Instant::now();

        // Act
        monitor.refresh(roots.clone(), now).await;
        finish_sample(&monitor).await;
        monitor.refresh(roots.clone(), now).await;
        let previous_resources = monitor.values["session"];
        let next_sample = now + Duration::from_secs(2);
        monitor.refresh(roots.clone(), next_sample).await;
        finish_sample(&monitor).await;
        let changed = monitor.refresh(roots, next_sample).await;

        // Assert
        assert_eq!(previous_resources, resources);
        assert!(changed);
        assert!(monitor.values.is_empty());
    }

    #[tokio::test]
    async fn stale_results_and_failed_samples_are_discarded() {
        // Arrange
        let mut client = MockResourceClient::new();
        client
            .expect_sample()
            .times(1)
            .returning(|_| Some(Vec::new()));
        client.expect_sample().times(1).returning(|_| None);
        let mut monitor = ResourceMonitor::new(Arc::new(client));
        let roots = HashMap::from([(SessionId::from("session"), 10)]);
        let now = Instant::now();

        // Act
        monitor.refresh(roots.clone(), now).await;
        finish_sample(&monitor).await;
        monitor.refresh(HashMap::new(), now).await;
        monitor
            .refresh(roots.clone(), now + Duration::from_secs(2))
            .await;
        finish_sample(&monitor).await;
        monitor.refresh(roots, now + Duration::from_secs(2)).await;

        // Assert
        assert!(monitor.values.is_empty());
    }
}
