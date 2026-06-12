//! Incremental binding-plane derivation.
//!
//! After `reindex_files` tombstones old nodes/edges and commits new ones,
//! the binding plane (scopes, aliases, shadows) must be updated. Per
//! Codex H8, this is a **whole-table rebuild** rather than incremental
//! patching, because `AliasTable` and `ShadowTable` are dense sorted
//! vectors with positional indices that cannot be incrementally patched
//! without an indirection layer.
//!
//! The rebuild scans BOTH the CSR (for surviving edges) AND the delta
//! buffer (for new/removed edges), producing fresh `ScopeArena`,
//! `AliasTable`, `ShadowTable`, and `ScopeProvenanceStore`.
//!
//! Cost: `O(E_binding)` where `E_binding` is the count of Contains/Defines/
//! Imports/Exports edges — typically ~30% of total edges. At 20M total
//! edges this is ~6M edges, taking ~50ms.

use crate::graph::unified::build::phase4e_binding::{
    BindingDerivationStats, derive_binding_plane, derive_binding_plane_generic,
};
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::mutation_target::GraphMutationTarget;

/// Derives the binding plane from scratch after incremental re-indexing.
///
/// This is a whole-table rebuild (not incremental patching) because the
/// binding tables use positional indexing that cannot be patched in-place.
/// The `Arc` swap on `CodeGraph` means in-flight queries see the old
/// binding tables until the swap completes — no locking needed.
///
/// # When to call
///
/// After `reindex_files` has tombstoned old nodes/edges and committed new
/// ones, call this to rebuild the binding plane. The binding plane depends
/// only on Contains/Defines/Imports/Exports edges, which are all intra-file
/// or cross-file structural edges — never cross-language edges.
///
/// # Performance
///
/// At 20M total edges with ~6M binding edges, this takes ~50ms. This is
/// acceptable because:
/// - It runs once per `reindex_files` call
/// - It is strictly cheaper than a full `build_unified_graph_inner` rebuild
/// - The binding tables are small relative to the edge store
///
/// # Public shim for `&mut CodeGraph`
///
/// The public entry point keeps the `&mut CodeGraph` signature so
/// external callers (e.g., the `reindex_files` path invoked from
/// `sqry-daemon`) do not need to depend on the `pub(crate)`
/// [`GraphMutationTarget`] trait. It delegates through the existing
/// public [`derive_binding_plane`] shim, which routes to the generic
/// implementation.
pub fn derive_binding_plane_incremental(graph: &mut CodeGraph) -> BindingDerivationStats {
    // The existing derive_binding_plane scans all edges in the graph
    // (both CSR and delta buffer via the BidirectionalEdgeStore API)
    // and rebuilds the full scope/alias/shadow tables. This is exactly
    // what we need: a whole-table rebuild that accounts for both old
    // surviving edges and new incremental edges.
    derive_binding_plane(graph)
}

/// Generic counterpart to [`derive_binding_plane_incremental`] used by
/// the intra-crate incremental rebuild dispatcher (Task 4 Step 4
/// Phase 3) that operates on a [`RebuildGraph`].
///
/// Behavioural contract is identical to the `&mut CodeGraph` variant —
/// the two are distinguishable only by their type, which is enforced at
/// the type system level through the `G: GraphMutationTarget` bound.
///
/// The Phase 2 test
/// `phase4e_binding::phase2_rebuild_tests::derive_binding_plane_incremental_runs_against_rebuild_graph`
/// keeps this symbol live under `cfg(test)`; Phase 3's production
/// consumer is `incremental_rebuild`.
///
/// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
// `expect(dead_code)` is keyed on any profile without the
// `rebuild-internals` feature — `cargo build` / `cargo clippy` without
// `--features rebuild-internals` sees no callers because the only
// Phase 3e call-site: `incremental_rebuild` invokes this helper between
// Phase 4d (cross-file edge insertion) and Pass 5 (cross-language
// linking) so the rebuild plane's binding plane is derived in the same
// order as the full-build pipeline.
pub(crate) fn derive_binding_plane_incremental_generic<G: GraphMutationTarget>(
    graph: &mut G,
) -> BindingDerivationStats {
    derive_binding_plane_generic(graph)
}

#[cfg(test)]
mod tests {
    // Integration tests for incremental binding derivation are in
    // sqry-core/tests/incremental_reindex_test.rs (proof point 1).
    // This module only provides the thin wrapper; the actual derivation
    // logic and its tests live in phase4e_binding.rs.
}
