//! Cached index metadata and access tracking for session management.
//!
//! `CachedIndex` wraps a `CodeGraph` (unified graph format) with lightweight
//! bookkeeping so the session manager can implement LRU and idle eviction
//! without cloning large data structures.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::graph::CodeGraph;

/// Wrapper around a `CodeGraph` with access metadata for eviction policies.
pub struct CachedIndex {
    /// Shared handle to the unified code graph.
    pub graph: Arc<CodeGraph>,
    /// Time the graph was first loaded into cache.
    pub loaded_at: Instant,
    /// Last time the graph was accessed.
    last_accessed: AtomicInstant,
    /// Modification time of the on-disk graph file when loaded.
    pub file_mtime: SystemTime,
    /// Number of queries served from this cache entry.
    pub query_count: AtomicU64,
}

impl CachedIndex {
    /// Create a new cached index wrapper with timestamps initialised to now.
    #[must_use]
    pub fn new(graph: Arc<CodeGraph>, file_mtime: SystemTime) -> Self {
        let now = Instant::now();
        Self {
            graph,
            loaded_at: now,
            last_accessed: AtomicInstant::new(now),
            file_mtime,
            query_count: AtomicU64::new(0),
        }
    }

    /// Record a cache access, updating timestamps and counters.
    pub fn access(&self) {
        self.last_accessed.store(Instant::now());
        self.query_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Retrieve the last access time.
    pub fn last_accessed(&self) -> Instant {
        self.last_accessed.load()
    }

    /// Force-set the last access time (used in tests to simulate staleness).
    #[cfg(test)]
    pub fn set_last_accessed(&self, instant: Instant) {
        self.last_accessed.store(instant);
    }

    /// Number of queries served by this cached entry.
    pub fn query_count(&self) -> u64 {
        self.query_count.load(Ordering::Relaxed)
    }
}

/// Minimal atomic wrapper for `Instant` using a mutex (Instant lacks atomics).
pub struct AtomicInstant {
    inner: Mutex<Instant>,
}

impl AtomicInstant {
    fn new(instant: Instant) -> Self {
        Self {
            inner: Mutex::new(instant),
        }
    }

    fn store(&self, instant: Instant) {
        // Recover from poisoned lock - timestamp data remains valid
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = instant;
    }

    fn load(&self) -> Instant {
        // Recover from poisoned lock - timestamp data remains valid
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn access_updates_tracking() {
        let graph = Arc::new(CodeGraph::new());
        let cached = CachedIndex::new(graph, SystemTime::now());

        assert_eq!(cached.query_count(), 0);
        let before = cached.last_accessed();

        cached.access();

        assert_eq!(cached.query_count(), 1);
        assert!(cached.last_accessed() >= before);
    }

    #[test]
    fn set_last_accessed_allows_manual_adjustment() {
        let graph = Arc::new(CodeGraph::new());
        let cached = CachedIndex::new(graph, SystemTime::now());
        let past = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();

        cached.set_last_accessed(past);

        assert!(cached.last_accessed() <= Instant::now());
    }
}
