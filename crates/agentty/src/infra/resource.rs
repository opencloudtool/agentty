//! Host process-table sampling behind an injectable boundary.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use crate::domain::resource::SessionResources;
use crate::infra::process_identity::ProcessIdentity;

/// One validated row in a host process-table snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ProcessSample {
    pub(crate) identity: Option<ProcessIdentity>,
    pub(crate) is_alive: bool,
    pub(crate) parent_pid: u32,
    pub(crate) pid: u32,
    pub(crate) resources: SessionResources,
}

/// Capability for obtaining a single coherent host process table.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ResourceClient: Send + Sync {
    /// Returns `None` when process accounting cannot be read reliably.
    async fn sample(&self, roots: Vec<u32>) -> Option<Vec<ProcessSample>>;
}

/// Process accounting through the macOS/Linux `ps` interface.
pub(crate) struct RealResourceClient;

impl RealResourceClient {
    /// Reads only tracked roots, off the async executor, before or after `ps`.
    async fn identities(roots: Vec<u32>) -> Option<HashMap<u32, ProcessIdentity>> {
        tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .filter_map(|pid| ProcessIdentity::read(pid).map(|identity| (pid, identity)))
                .collect()
        })
        .await
        .ok()
    }

    /// Rejects failed commands and malformed output instead of showing zeros.
    fn parse_output(output: &std::process::Output) -> Option<Vec<ProcessSample>> {
        if !output.status.success() {
            return None;
        }

        parse_process_table(std::str::from_utf8(&output.stdout).ok()?)
    }

    /// Binds accounting only to roots that remained the same process across
    /// the host snapshot. Missing identities never fall back to numeric PIDs.
    fn bind_identities(
        samples: &mut [ProcessSample],
        before: &HashMap<u32, ProcessIdentity>,
        after: &HashMap<u32, ProcessIdentity>,
    ) {
        for sample in samples {
            sample.identity = before
                .get(&sample.pid)
                .filter(|identity| after.get(&sample.pid) == Some(identity))
                .copied();
        }
    }
}

#[async_trait]
impl ResourceClient for RealResourceClient {
    async fn sample(&self, roots: Vec<u32>) -> Option<Vec<ProcessSample>> {
        let before = Self::identities(roots.clone()).await?;
        let mut command = Command::new("ps");
        command
            .args(["-A", "-o", "pid=,ppid=,pcpu=,rss=,stat="])
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(2), command.output())
            .await
            .ok()?
            .ok()?;
        let mut samples = Self::parse_output(&output)?;
        let after = Self::identities(roots).await?;
        Self::bind_identities(&mut samples, &before, &after);

        Some(samples)
    }
}

/// Parses header-free accounting and state; native identity is attached later.
fn parse_process_table(output: &str) -> Option<Vec<ProcessSample>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent_pid = fields.next()?.parse().ok()?;
            let cpu_percent: f64 = fields.next()?.parse().ok()?;
            let resident_memory_kib = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            if !cpu_percent.is_finite() || cpu_percent < 0.0 || fields.next().is_some() {
                return None;
            }

            Some(ProcessSample {
                identity: None,
                is_alive: !state.starts_with(['Z', 'X', 'x']),
                parent_pid,
                pid,
                resources: SessionResources {
                    cpu_percent,
                    process_count: 1,
                    resident_memory_kib,
                },
            })
        })
        .collect()
}

/// Totals only the root and descendants present in this snapshot.
/// Returns `None` when the tracked root is absent or exited, even if
/// descendants remain. Exited descendants do not contribute to the totals.
pub(crate) fn process_tree_resources(
    samples: &[ProcessSample],
    root: u32,
) -> Option<SessionResources> {
    let by_pid: HashMap<_, _> = samples
        .iter()
        .filter(|sample| sample.is_alive)
        .map(|sample| (sample.pid, sample))
        .collect();
    by_pid.get(&root)?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for sample in by_pid.values() {
        children
            .entry(sample.parent_pid)
            .or_default()
            .push(sample.pid);
    }
    let mut pending = vec![root];
    let mut visited = HashSet::new();
    let mut resources = SessionResources::default();
    while let Some(pid) = pending.pop() {
        if !visited.insert(pid) {
            continue;
        }
        let sample = by_pid.get(&pid)?;
        resources.process_count += 1;
        resources.cpu_percent += sample.resources.cpu_percent;
        resources.resident_memory_kib = resources
            .resident_memory_kib
            .saturating_add(sample.resources.resident_memory_kib);
        if let Some(descendants) = children.get(&pid) {
            pending.extend(descendants);
        }
    }

    Some(resources)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn failed_command_and_invalid_utf8_are_unavailable() {
        // Arrange
        let mut output = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: b"1 0 0 1024".to_vec(),
            stderr: Vec::new(),
        };

        // Act / Assert
        assert!(RealResourceClient::parse_output(&output).is_none());
        output.status = std::process::ExitStatus::from_raw(0);
        output.stdout = vec![0xff];
        assert!(RealResourceClient::parse_output(&output).is_none());
    }

    #[test]
    fn tree_totals_include_descendants_and_exclude_other_sessions() {
        // Arrange
        let samples = parse_process_table(
            "30 20 25.5 1024 S\n10 1 100.0 2048 Ss\n20 10 3.0 512 R\n40 1 90.0 8000 S\n",
        )
        .expect("valid table");

        // Act
        let resources = process_tree_resources(&samples, 10).expect("root present");

        // Assert
        assert_eq!(
            resources,
            SessionResources {
                cpu_percent: 128.5,
                process_count: 3,
                resident_memory_kib: 3584
            }
        );
        assert_eq!(process_tree_resources(&samples, 999), None);
        assert_eq!(process_tree_resources(&samples, 1), None);
    }

    #[test]
    fn malformed_tables_are_unavailable() {
        // Arrange
        let malformed = [
            "1",
            "x 0 0 1 S",
            "1 x 0 1 S",
            "1 0 x 1 S",
            "1 0 0 x S",
            "1 0 NaN 1 S",
            "1 0 inf 1 S",
            "1 0 -1 1 S",
            "1 0 0 1",
            "1 0 0 1 S extra",
        ];

        // Act / Assert
        for output in malformed {
            assert!(parse_process_table(output).is_none(), "{output}");
        }
        assert!(parse_process_table("\n ").expect("empty table").is_empty());
    }

    #[test]
    fn cycles_and_duplicate_rows_do_not_double_count() {
        // Arrange
        let samples =
            parse_process_table("1 2 1 100 S\n2 1 2 200 S\n2 1 2 200 S").expect("valid table");

        // Act
        let resources = process_tree_resources(&samples, 1).expect("root present");

        // Assert
        assert_eq!(resources.process_count, 2);
        assert_eq!(resources.resident_memory_kib, 300);
    }

    #[test]
    fn exited_roots_are_unavailable_and_exited_children_are_excluded() {
        // Arrange
        let samples = parse_process_table("1 0 1 100 S\n2 1 50 200 Z+\n3 1 50 200 X\n4 1 50 200 x")
            .expect("valid table");

        // Act
        let resources = process_tree_resources(&samples, 1).expect("live root");

        // Assert
        assert_eq!(resources.process_count, 1);
        assert_eq!(resources.cpu_percent, 1.0);
        assert_eq!(resources.resident_memory_kib, 100);
        for pid in [2, 3, 4] {
            assert!(process_tree_resources(&samples, pid).is_none());
        }
    }

    #[test]
    fn native_identity_changes_within_one_second_cannot_bind_stale_accounting() {
        // Arrange
        let original = ProcessIdentity(1_000_001);
        let reused = ProcessIdentity(1_000_002);
        let before = HashMap::from([(10, original), (20, original), (30, original)]);
        let after = HashMap::from([(10, original), (20, reused), (40, original)]);
        let mut samples =
            parse_process_table("10 1 1 100 S\n20 1 90 8192 S\n30 1 1 100 S\n40 1 1 100 S")
                .expect("accounting snapshot");

        // Act
        RealResourceClient::bind_identities(&mut samples, &before, &after);

        // Assert
        assert_eq!(original.0 / 1_000_000, reused.0 / 1_000_000);
        assert_eq!(samples[0].identity, Some(original));
        assert!(samples[1..].iter().all(|sample| sample.identity.is_none()));

        // Act: a later coherent sample identifies the new process distinctly.
        RealResourceClient::bind_identities(&mut samples, &after, &after);

        // Assert
        assert_eq!(samples[1].identity, Some(reused));
        assert_ne!(samples[1].identity, Some(original));
    }

    #[tokio::test]
    async fn real_process_table_contains_this_process() {
        // Arrange
        let client = RealResourceClient;

        // Act
        let samples = client
            .sample(vec![std::process::id(), u32::MAX])
            .await
            .expect("host ps available");
        let resources = process_tree_resources(&samples, std::process::id()).expect("root present");

        // Assert
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.pid == std::process::id())
                .expect("root")
                .identity,
            ProcessIdentity::read(std::process::id()),
        );
        assert!(resources.process_count >= 1);
        assert!(resources.resident_memory_kib > 0);
    }
}
