//! Async background compaction for the graph arena.
//!
//! Compaction reclaims tombstoned node slots and rebuilds the CSR edge store
//! from the delta buffer, reducing fragmentation and restoring O(1) edge
//! lookup performance.
//!
//! # Trigger thresholds
//!
//! Compaction runs when either:
//! - Arena fragmentation exceeds 20%: `free_slots / total_slots > 0.20`
//! - Delta buffer ratio exceeds 10%: `delta_buffer.len() / csr.edge_count() > 0.10`
//!
//! # Concurrency
//!
//! Compaction runs on a background thread. Queries continue against the old
//! CSR + delta buffer during compaction. The new CSR is swapped in atomically
//! via `Arc` swap when ready.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::config::QueryDbConfig;

/// Handle to a background compaction thread.
///
/// Dropping the handle signals the thread to stop (via the `stop` flag).
/// The thread checks this flag between compaction cycles.
pub struct CompactionHandle {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<CompactionResult>>,
}

/// Result of a compaction cycle.
#[derive(Debug, Clone, Default)]
pub struct CompactionResult {
    /// Number of nodes reclaimed from tombstoned slots.
    pub nodes_reclaimed: usize,
    /// Number of edges merged from delta buffer into CSR.
    pub edges_merged: usize,
    /// Arena fragmentation before compaction (0.0–1.0).
    pub fragmentation_before: f64,
    /// Arena fragmentation after compaction (should be 0.0).
    pub fragmentation_after: f64,
    /// Whether compaction was triggered (vs. skipped because thresholds not met).
    pub triggered: bool,
}

/// Checks whether compaction thresholds are exceeded.
///
/// # Arguments
///
/// * `free_slots` — Number of vacant arena slots (tombstoned by remove).
/// * `total_slots` — Total arena slot count (occupied + vacant).
/// * `delta_count` — Number of entries in the edge delta buffer.
/// * `csr_count` — Number of edges in the CSR.
/// * `config` — Configuration with threshold values.
#[must_use]
pub fn should_compact(
    free_slots: usize,
    total_slots: usize,
    delta_count: usize,
    csr_count: usize,
    config: &QueryDbConfig,
) -> bool {
    if total_slots == 0 {
        return false;
    }

    let fragmentation = free_slots as f64 / total_slots as f64;
    if fragmentation > config.compaction_fragmentation_threshold {
        return true;
    }

    if csr_count > 0 {
        let delta_ratio = delta_count as f64 / csr_count as f64;
        if delta_ratio > config.compaction_delta_ratio_threshold {
            return true;
        }
    }

    false
}

impl CompactionHandle {
    /// Creates a new compaction handle (not yet started).
    #[must_use]
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    /// Signals the compaction thread to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Returns true if a compaction thread is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Waits for the compaction thread to finish and returns the result.
    ///
    /// Returns `None` if no thread was started or it has already been joined.
    pub fn join(mut self) -> Option<CompactionResult> {
        self.thread.take().and_then(|t| t.join().ok())
    }
}

impl Default for CompactionHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CompactionHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Don't block on join in Drop — the thread will notice the stop flag
        // and exit on its own.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_compact_fragmentation() {
        let config = QueryDbConfig::default();
        // 20% free → triggers
        assert!(should_compact(21, 100, 0, 1000, &config));
        // 19% free → does not trigger
        assert!(!should_compact(19, 100, 0, 1000, &config));
    }

    #[test]
    fn should_compact_delta_ratio() {
        let config = QueryDbConfig::default();
        // 11% delta → triggers
        assert!(should_compact(0, 100, 110, 1000, &config));
        // 9% delta → does not trigger
        assert!(!should_compact(0, 100, 90, 1000, &config));
    }

    #[test]
    fn should_compact_empty_graph() {
        let config = QueryDbConfig::default();
        assert!(!should_compact(0, 0, 0, 0, &config));
    }

    #[test]
    fn compaction_handle_lifecycle() {
        let handle = CompactionHandle::new();
        assert!(!handle.is_running());
        handle.stop();
        let result = handle.join();
        assert!(result.is_none());
    }
}
