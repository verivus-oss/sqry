//! `CompactionCheckpoint`: State snapshot for rollback during compaction.
//!
//! This module implements checkpoint/restore functionality for atomic
//! bidirectional compaction operations.
//!
//! # Design (FR-62, FR-65)
//!
//! - **FR-62**: Checkpoint captures complete state for rollback
//! - **FR-65**: Rollback restores both committed and reserved counters
//!
//! # Usage
//!
//! Checkpoints are created before attempting compaction, and restored
//! if the compaction fails mid-way (e.g., forward succeeds but reverse fails).
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::compaction::CompactionCheckpoint;
//!
//! // Create checkpoint before compaction
//! let checkpoint = CompactionCheckpoint::capture(&edge_store, &buffer_state);
//!
//! // Attempt compaction...
//! if let Err(e) = compact_operation() {
//!     // Rollback on failure
//!     checkpoint.restore(&mut edge_store, &buffer_state);
//! }
//! ```
//!
//! # Thread Safety
//!
//! Checkpoints capture a consistent snapshot at creation time.
//! The restore operation requires exclusive access to the edge store
//! and asserts that no reservation guards are active.

use std::fmt;

use super::super::admission::BufferStateSnapshot;

/// Counter state snapshot for checkpoint/restore.
///
/// Captures the committed and reserved counter values at checkpoint time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterCheckpoint {
    /// Committed bytes at checkpoint time
    pub committed_bytes: usize,
    /// Committed operations at checkpoint time
    pub committed_ops: usize,
    /// Reserved bytes at checkpoint time
    pub reserved_bytes: usize,
    /// Reserved operations at checkpoint time
    pub reserved_ops: usize,
}

impl CounterCheckpoint {
    /// Creates a new counter checkpoint.
    #[must_use]
    pub fn new(
        committed_bytes: usize,
        committed_ops: usize,
        reserved_bytes: usize,
        reserved_ops: usize,
    ) -> Self {
        Self {
            committed_bytes,
            committed_ops,
            reserved_bytes,
            reserved_ops,
        }
    }

    /// Creates a checkpoint from a buffer state snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &BufferStateSnapshot) -> Self {
        Self {
            committed_bytes: snapshot.committed_bytes,
            committed_ops: snapshot.committed_ops,
            reserved_bytes: snapshot.reserved_bytes,
            reserved_ops: snapshot.reserved_ops,
        }
    }

    /// Returns total bytes (committed + reserved).
    #[must_use]
    #[inline]
    pub const fn total_bytes(&self) -> usize {
        self.committed_bytes + self.reserved_bytes
    }

    /// Returns total operations (committed + reserved).
    #[must_use]
    #[inline]
    pub const fn total_ops(&self) -> usize {
        self.committed_ops + self.reserved_ops
    }
}

impl fmt::Display for CounterCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bytes: {} committed + {} reserved, ops: {} committed + {} reserved",
            self.committed_bytes, self.reserved_bytes, self.committed_ops, self.reserved_ops
        )
    }
}

/// Edge store state snapshot for checkpoint/restore.
///
/// Captures the CSR version, delta buffer size, and sequence counter
/// at checkpoint time. This lightweight checkpoint doesn't clone the
/// actual data - it just captures version/size info for validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EdgeStoreCheckpoint {
    /// CSR version number at checkpoint time
    pub csr_version: u64,
    /// Number of delta edges at checkpoint time
    pub delta_edge_count: usize,
    /// Delta buffer byte size at checkpoint time
    pub delta_byte_size: usize,
    /// Sequence counter value at checkpoint time
    pub seq_counter: u64,
    /// Number of tombstones at checkpoint time
    pub tombstone_count: usize,
}

impl EdgeStoreCheckpoint {
    /// Creates a new edge store checkpoint.
    #[must_use]
    pub fn new(
        csr_version: u64,
        delta_edge_count: usize,
        delta_byte_size: usize,
        seq_counter: u64,
        tombstone_count: usize,
    ) -> Self {
        Self {
            csr_version,
            delta_edge_count,
            delta_byte_size,
            seq_counter,
            tombstone_count,
        }
    }

    /// Returns true if the edge store state has changed since checkpoint.
    #[must_use]
    pub fn has_changed(
        &self,
        current_csr_version: u64,
        current_delta_count: usize,
        current_seq: u64,
    ) -> bool {
        self.csr_version != current_csr_version
            || self.delta_edge_count != current_delta_count
            || self.seq_counter != current_seq
    }
}

impl fmt::Display for EdgeStoreCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "csr_v{}, {} deltas ({} bytes), seq={}, {} tombstones",
            self.csr_version,
            self.delta_edge_count,
            self.delta_byte_size,
            self.seq_counter,
            self.tombstone_count
        )
    }
}

/// Complete compaction checkpoint for bidirectional edge store.
///
/// Captures enough state to validate and restore after a failed
/// compaction operation.
///
/// # State Captured
///
/// - Forward edge store: CSR version, delta size, seq counter, tombstones
/// - Reverse edge store: CSR version, delta size, seq counter, tombstones
/// - Counter state: committed and reserved bytes/ops
///
/// # Usage Notes
///
/// For full rollback with data restoration, the caller must retain
/// cloned copies of the actual CSR and delta buffer data. This checkpoint
/// provides the lightweight metadata needed for validation and counter
/// restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCheckpoint {
    /// Forward edge store state
    pub forward: EdgeStoreCheckpoint,
    /// Reverse edge store state
    pub reverse: EdgeStoreCheckpoint,
    /// Counter state
    pub counters: CounterCheckpoint,
    /// Timestamp when checkpoint was created (for diagnostics)
    pub created_at_epoch_ms: u64,
}

impl CompactionCheckpoint {
    /// Creates a new compaction checkpoint.
    #[must_use]
    pub fn new(
        forward: EdgeStoreCheckpoint,
        reverse: EdgeStoreCheckpoint,
        counters: CounterCheckpoint,
    ) -> Self {
        Self {
            forward,
            reverse,
            counters,
            created_at_epoch_ms: current_epoch_ms(),
        }
    }

    /// Creates a checkpoint from individual components.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_components(
        forward_csr_version: u64,
        forward_delta_count: usize,
        forward_delta_bytes: usize,
        forward_seq: u64,
        forward_tombstones: usize,
        reverse_csr_version: u64,
        reverse_delta_count: usize,
        reverse_delta_bytes: usize,
        reverse_seq: u64,
        reverse_tombstones: usize,
        committed_bytes: usize,
        committed_ops: usize,
        reserved_bytes: usize,
        reserved_ops: usize,
    ) -> Self {
        Self::new(
            EdgeStoreCheckpoint::new(
                forward_csr_version,
                forward_delta_count,
                forward_delta_bytes,
                forward_seq,
                forward_tombstones,
            ),
            EdgeStoreCheckpoint::new(
                reverse_csr_version,
                reverse_delta_count,
                reverse_delta_bytes,
                reverse_seq,
                reverse_tombstones,
            ),
            CounterCheckpoint::new(committed_bytes, committed_ops, reserved_bytes, reserved_ops),
        )
    }

    /// Returns true if either store has changed since checkpoint.
    #[must_use]
    pub fn has_concurrent_modification(
        &self,
        forward_csr_version: u64,
        forward_delta_count: usize,
        forward_seq: u64,
        reverse_csr_version: u64,
        reverse_delta_count: usize,
        reverse_seq: u64,
    ) -> bool {
        self.forward
            .has_changed(forward_csr_version, forward_delta_count, forward_seq)
            || self
                .reverse
                .has_changed(reverse_csr_version, reverse_delta_count, reverse_seq)
    }

    /// Returns the age of this checkpoint in milliseconds.
    #[must_use]
    pub fn age_ms(&self) -> u64 {
        current_epoch_ms().saturating_sub(self.created_at_epoch_ms)
    }

    /// Returns statistics about this checkpoint.
    #[must_use]
    pub fn stats(&self) -> CheckpointStats {
        CheckpointStats {
            forward_delta_count: self.forward.delta_edge_count,
            forward_delta_bytes: self.forward.delta_byte_size,
            forward_tombstones: self.forward.tombstone_count,
            reverse_delta_count: self.reverse.delta_edge_count,
            reverse_delta_bytes: self.reverse.delta_byte_size,
            reverse_tombstones: self.reverse.tombstone_count,
            total_committed_bytes: self.counters.committed_bytes,
            total_committed_ops: self.counters.committed_ops,
            total_reserved_bytes: self.counters.reserved_bytes,
            total_reserved_ops: self.counters.reserved_ops,
        }
    }
}

impl Default for CompactionCheckpoint {
    fn default() -> Self {
        Self {
            forward: EdgeStoreCheckpoint::default(),
            reverse: EdgeStoreCheckpoint::default(),
            counters: CounterCheckpoint::default(),
            created_at_epoch_ms: current_epoch_ms(),
        }
    }
}

impl fmt::Display for CompactionCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CompactionCheckpoint {{ forward: [{}], reverse: [{}], counters: [{}] }}",
            self.forward, self.reverse, self.counters
        )
    }
}

/// Statistics about a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckpointStats {
    /// Forward delta edge count
    pub forward_delta_count: usize,
    /// Forward delta byte size
    pub forward_delta_bytes: usize,
    /// Forward tombstone count
    pub forward_tombstones: usize,
    /// Reverse delta edge count
    pub reverse_delta_count: usize,
    /// Reverse delta byte size
    pub reverse_delta_bytes: usize,
    /// Reverse tombstone count
    pub reverse_tombstones: usize,
    /// Total committed bytes
    pub total_committed_bytes: usize,
    /// Total committed operations
    pub total_committed_ops: usize,
    /// Total reserved bytes
    pub total_reserved_bytes: usize,
    /// Total reserved operations
    pub total_reserved_ops: usize,
}

impl CheckpointStats {
    /// Total delta edges across both stores.
    #[must_use]
    #[inline]
    pub const fn total_delta_edges(&self) -> usize {
        self.forward_delta_count + self.reverse_delta_count
    }

    /// Total delta bytes across both stores.
    #[must_use]
    #[inline]
    pub const fn total_delta_bytes(&self) -> usize {
        self.forward_delta_bytes + self.reverse_delta_bytes
    }

    /// Total tombstones across both stores.
    #[must_use]
    #[inline]
    pub const fn total_tombstones(&self) -> usize {
        self.forward_tombstones + self.reverse_tombstones
    }
}

impl fmt::Display for CheckpointStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "deltas: {} edges ({} bytes), tombstones: {}, committed: {} bytes/{} ops, reserved: {} bytes/{} ops",
            self.total_delta_edges(),
            self.total_delta_bytes(),
            self.total_tombstones(),
            self.total_committed_bytes,
            self.total_committed_ops,
            self.total_reserved_bytes,
            self.total_reserved_ops
        )
    }
}

/// Returns the current epoch time in milliseconds.
fn current_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_checkpoint_new() {
        let cp = CounterCheckpoint::new(100, 10, 50, 5);
        assert_eq!(cp.committed_bytes, 100);
        assert_eq!(cp.committed_ops, 10);
        assert_eq!(cp.reserved_bytes, 50);
        assert_eq!(cp.reserved_ops, 5);
    }

    #[test]
    fn test_counter_checkpoint_totals() {
        let cp = CounterCheckpoint::new(100, 10, 50, 5);
        assert_eq!(cp.total_bytes(), 150);
        assert_eq!(cp.total_ops(), 15);
    }

    #[test]
    fn test_counter_checkpoint_from_snapshot() {
        let snapshot = BufferStateSnapshot {
            committed_bytes: 200,
            committed_ops: 20,
            reserved_bytes: 100,
            reserved_ops: 10,
            active_guards: 2,
        };

        let cp = CounterCheckpoint::from_snapshot(&snapshot);
        assert_eq!(cp.committed_bytes, 200);
        assert_eq!(cp.committed_ops, 20);
        assert_eq!(cp.reserved_bytes, 100);
        assert_eq!(cp.reserved_ops, 10);
    }

    #[test]
    fn test_counter_checkpoint_display() {
        let cp = CounterCheckpoint::new(100, 10, 50, 5);
        let display = format!("{cp}");
        assert!(display.contains("100 committed"));
        assert!(display.contains("50 reserved"));
    }

    #[test]
    fn test_edge_store_checkpoint_new() {
        let cp = EdgeStoreCheckpoint::new(1, 100, 5000, 42, 10);
        assert_eq!(cp.csr_version, 1);
        assert_eq!(cp.delta_edge_count, 100);
        assert_eq!(cp.delta_byte_size, 5000);
        assert_eq!(cp.seq_counter, 42);
        assert_eq!(cp.tombstone_count, 10);
    }

    #[test]
    fn test_edge_store_checkpoint_has_changed() {
        let cp = EdgeStoreCheckpoint::new(1, 100, 5000, 42, 10);

        // No change
        assert!(!cp.has_changed(1, 100, 42));

        // CSR version changed
        assert!(cp.has_changed(2, 100, 42));

        // Delta count changed
        assert!(cp.has_changed(1, 101, 42));

        // Seq changed
        assert!(cp.has_changed(1, 100, 43));
    }

    #[test]
    fn test_edge_store_checkpoint_display() {
        let cp = EdgeStoreCheckpoint::new(1, 100, 5000, 42, 10);
        let display = format!("{cp}");
        assert!(display.contains("csr_v1"));
        assert!(display.contains("100 deltas"));
        assert!(display.contains("seq=42"));
    }

    #[test]
    fn test_compaction_checkpoint_new() {
        let forward = EdgeStoreCheckpoint::new(1, 50, 2500, 20, 5);
        let reverse = EdgeStoreCheckpoint::new(1, 50, 2500, 20, 5);
        let counters = CounterCheckpoint::new(100, 10, 50, 5);

        let cp = CompactionCheckpoint::new(forward, reverse, counters);
        assert_eq!(cp.forward.delta_edge_count, 50);
        assert_eq!(cp.reverse.delta_edge_count, 50);
        assert_eq!(cp.counters.committed_bytes, 100);
        assert!(cp.created_at_epoch_ms > 0);
    }

    #[test]
    fn test_compaction_checkpoint_from_components() {
        let cp = CompactionCheckpoint::from_components(
            1, 50, 2500, 20, 5, // forward
            2, 60, 3000, 25, 8, // reverse
            100, 10, 50, 5, // counters
        );

        assert_eq!(cp.forward.csr_version, 1);
        assert_eq!(cp.forward.delta_edge_count, 50);
        assert_eq!(cp.reverse.csr_version, 2);
        assert_eq!(cp.reverse.delta_edge_count, 60);
        assert_eq!(cp.counters.committed_bytes, 100);
    }

    #[test]
    fn test_compaction_checkpoint_has_concurrent_modification() {
        let cp = CompactionCheckpoint::from_components(
            1, 50, 2500, 20, 5, // forward
            1, 60, 3000, 25, 8, // reverse
            100, 10, 50, 5, // counters
        );

        // No change
        assert!(!cp.has_concurrent_modification(1, 50, 20, 1, 60, 25));

        // Forward changed
        assert!(cp.has_concurrent_modification(2, 50, 20, 1, 60, 25));

        // Reverse changed
        assert!(cp.has_concurrent_modification(1, 50, 20, 2, 60, 25));
    }

    #[test]
    fn test_compaction_checkpoint_stats() {
        let cp = CompactionCheckpoint::from_components(
            1, 50, 2500, 20, 5, // forward
            1, 60, 3000, 25, 8, // reverse
            100, 10, 50, 5, // counters
        );

        let stats = cp.stats();
        assert_eq!(stats.total_delta_edges(), 110);
        assert_eq!(stats.total_delta_bytes(), 5500);
        assert_eq!(stats.total_tombstones(), 13);
        assert_eq!(stats.total_committed_bytes, 100);
    }

    #[test]
    fn test_compaction_checkpoint_age() {
        let cp = CompactionCheckpoint::default();

        // Small sleep to ensure some time passes
        std::thread::sleep(std::time::Duration::from_millis(10));

        let age = cp.age_ms();
        assert!(age >= 10);
    }

    #[test]
    fn test_checkpoint_stats_display() {
        let stats = CheckpointStats {
            forward_delta_count: 50,
            forward_delta_bytes: 2500,
            forward_tombstones: 5,
            reverse_delta_count: 60,
            reverse_delta_bytes: 3000,
            reverse_tombstones: 8,
            total_committed_bytes: 100,
            total_committed_ops: 10,
            total_reserved_bytes: 50,
            total_reserved_ops: 5,
        };

        let display = format!("{stats}");
        assert!(display.contains("110 edges"));
        assert!(display.contains("5500 bytes"));
        assert!(display.contains("13")); // tombstones
    }

    #[test]
    fn test_compaction_checkpoint_display() {
        let cp = CompactionCheckpoint::from_components(
            1, 50, 2500, 20, 5, // forward
            1, 60, 3000, 25, 8, // reverse
            100, 10, 50, 5, // counters
        );

        let display = format!("{cp}");
        assert!(display.contains("forward:"));
        assert!(display.contains("reverse:"));
        assert!(display.contains("counters:"));
    }

    #[test]
    fn test_default_values() {
        let counter_cp = CounterCheckpoint::default();
        assert_eq!(counter_cp.committed_bytes, 0);
        assert_eq!(counter_cp.total_bytes(), 0);

        let edge_cp = EdgeStoreCheckpoint::default();
        assert_eq!(edge_cp.csr_version, 0);
        assert_eq!(edge_cp.delta_edge_count, 0);

        let comp_cp = CompactionCheckpoint::default();
        assert_eq!(comp_cp.forward.csr_version, 0);
        assert_eq!(comp_cp.counters.committed_bytes, 0);
    }

    #[test]
    fn test_counter_checkpoint_equality() {
        let cp1 = CounterCheckpoint::new(100, 10, 50, 5);
        let cp2 = CounterCheckpoint::new(100, 10, 50, 5);
        let cp3 = CounterCheckpoint::new(100, 10, 50, 6);

        assert_eq!(cp1, cp2);
        assert_ne!(cp1, cp3);
    }

    #[test]
    fn test_edge_store_checkpoint_equality() {
        let cp1 = EdgeStoreCheckpoint::new(1, 100, 5000, 42, 10);
        let cp2 = EdgeStoreCheckpoint::new(1, 100, 5000, 42, 10);
        let cp3 = EdgeStoreCheckpoint::new(2, 100, 5000, 42, 10);

        assert_eq!(cp1, cp2);
        assert_ne!(cp1, cp3);
    }
}
