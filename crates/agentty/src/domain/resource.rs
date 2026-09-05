//! Display-neutral snapshots of a tracked agent process tree.

/// Resource totals for a session's agent process and its descendants.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionResources {
    /// Sum of host-reported process CPU percentages; may exceed 100%.
    pub cpu_percent: f64,
    /// Number of processes, including the tracked agent itself.
    pub process_count: usize,
    /// Sum of resident memory in kibibytes, including shared pages per process.
    pub resident_memory_kib: u64,
}
