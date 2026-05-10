//! `ComparativeQueryDb` for cross-snapshot operations.
//!
//! `semantic_diff` operates across two separate snapshots (different git refs
//! or worktrees). This does not fit the single-snapshot `QueryDb` model because
//! cross-snapshot results cannot be meaningfully invalidated.
//!
//! `ComparativeQueryDb` is a lightweight wrapper holding two snapshots. It
//! bypasses the `ShardedCache` entirely — comparative queries are inherently
//! one-shot, uncached operations.
//!
//! # DB20 design choice (Option A)
//!
//! The `diff` computation is exposed as an inherent method on
//! `ComparativeQueryDb` rather than as a `DerivedQuery` (which would require
//! a single-snapshot key) or as a free function with separate snapshots. This:
//!
//! 1. Keeps the public surface minimal (one wrapper, one method).
//! 2. Co-locates cross-snapshot logic with the type that owns the two
//!    snapshots.
//! 3. Avoids an awkward "uncached `DerivedQuery`" abstraction that would
//!    contradict the three-tier cache invariants.
//!
//! See `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md`
//! section M6 for the rationale.

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;

pub mod diff;

pub use diff::{ChangeType, DiffOptions, DiffOutput, DiffSummary, NodeChange, NodeLocation};

/// A lightweight wrapper holding two `GraphSnapshot`s for cross-snapshot
/// operations like `semantic_diff`.
///
/// Comparative queries are NOT cached (they are inherently one-shot
/// cross-snapshot operations), so this type bypasses the `ShardedCache`
/// entirely.
///
/// # Usage
///
/// ```rust,ignore
/// use sqry_db::comparative::{ComparativeQueryDb, DiffOptions};
///
/// let cmp = ComparativeQueryDb::new(old_snapshot, new_snapshot);
/// let out = cmp.diff(&DiffOptions::default()); // uncached, one-shot
/// ```
pub struct ComparativeQueryDb {
    /// The "before" snapshot (e.g., older commit).
    old: Arc<GraphSnapshot>,
    /// The "after" snapshot (e.g., newer commit).
    new: Arc<GraphSnapshot>,
}

impl ComparativeQueryDb {
    /// Creates a new comparative DB from two snapshots.
    #[must_use]
    pub fn new(old: Arc<GraphSnapshot>, new: Arc<GraphSnapshot>) -> Self {
        Self { old, new }
    }

    /// Returns the "before" snapshot.
    #[inline]
    #[must_use]
    pub fn old(&self) -> &GraphSnapshot {
        &self.old
    }

    /// Returns the "after" snapshot.
    #[inline]
    #[must_use]
    pub fn new_snapshot(&self) -> &GraphSnapshot {
        &self.new
    }

    /// Returns the "before" snapshot as an `Arc`.
    #[inline]
    #[must_use]
    pub fn old_arc(&self) -> Arc<GraphSnapshot> {
        Arc::clone(&self.old)
    }

    /// Returns the "after" snapshot as an `Arc`.
    #[inline]
    #[must_use]
    pub fn new_arc(&self) -> Arc<GraphSnapshot> {
        Arc::clone(&self.new)
    }

    /// Computes the semantic diff between the two snapshots.
    ///
    /// This is an uncached, one-shot operation. Results are NOT memoized by
    /// the `ShardedCache` because cross-snapshot results have no meaningful
    /// invalidation criterion (the two snapshots are immutable, and no
    /// single-snapshot dependency bump would ever reinvalidate their diff).
    ///
    /// Pass the worktree roots via [`DiffOptions`] so per-file paths are
    /// prefixed back to absolute worktree locations. Callers that do not
    /// need worktree prefixing (e.g. unit tests or CLI callers that already
    /// hold workspace-relative paths) may use [`Self::diff_default`].
    #[must_use]
    pub fn diff(&self, opts: &DiffOptions) -> DiffOutput {
        // Defensive fast-path: when both sides point at the literal same
        // snapshot Arc, the diff is provably empty. Callers that resolve
        // identical git refs upstream (sqry-cli and sqry-mcp do) avoid
        // the cost of materializing two snapshots in the first place;
        // this guard catches in-process callers (tests, SDK users) that
        // pass the same Arc into `new()` directly. (verivus-oss/sqry#213)
        if Arc::ptr_eq(&self.old, &self.new) {
            return DiffOutput::default();
        }
        diff::compute_diff(&self.old, &self.new, opts)
    }

    /// Computes the semantic diff using default options (no worktree
    /// prefixing). See [`Self::diff`] for the full semantics.
    #[must_use]
    pub fn diff_default(&self) -> DiffOutput {
        self.diff(&DiffOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ComparativeQueryDb is a pure wrapper — construction and accessor tests
    // are covered in integration tests that build real graph snapshots.
    // This module verifies the type is Send + Sync (required for cross-thread
    // usage in MCP handlers).
    #[test]
    fn comparative_query_db_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ComparativeQueryDb>();
    }

    /// `Arc::ptr_eq` fast-path: when both sides are the same `Arc`, `diff`
    /// must return an empty `DiffOutput` without entering `compute_diff`.
    /// (verivus-oss/sqry#213)
    ///
    /// Building a real `GraphSnapshot` here would require a workspace
    /// fixture; instead we round-trip through a default-constructed
    /// snapshot via `CodeGraph::new().snapshot()`, which is the cheapest
    /// non-trivial snapshot the public API exposes.
    #[test]
    fn diff_short_circuits_when_both_sides_share_arc() {
        use sqry_core::graph::unified::concurrent::CodeGraph;
        let graph = CodeGraph::new();
        let snap = Arc::new(graph.snapshot());
        let cmp = ComparativeQueryDb::new(Arc::clone(&snap), Arc::clone(&snap));
        let out = cmp.diff(&DiffOptions::default());
        assert!(out.changes.is_empty(), "expected empty diff on shared Arc");
        assert_eq!(out.summary, DiffSummary::default());
    }
}
