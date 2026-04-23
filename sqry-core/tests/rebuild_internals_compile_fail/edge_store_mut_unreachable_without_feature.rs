//! Gate 0c trybuild fixture (iter-4 blocker fix): the four rebuild-only
//! edge-storage mutators exposed for [`RebuildGraph::finalize`] step 1
//! are `pub(crate)` and therefore unreachable from any external crate,
//! regardless of whether `rebuild-internals` is enabled.
//!
//! Iter-4 Codex review flagged (blocker, dimension 7) that these four
//! helpers leaked into the ungated public API of `sqry-core`:
//!
//! - [`CsrGraph::edge_kind_mut`]
//! - [`EdgeStore::csr_mut`]
//! - [`DeltaBuffer::iter_mut`]
//! - [`BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`]
//!
//! The fix: all four are now `pub(crate)`. External crates (including
//! `sqry-daemon` with `rebuild-internals` enabled — which this fixture
//! runs under) must go through [`RebuildGraph::finalize`] as the
//! single publish path (plan §H "Type-enforced publish path").
//!
//! If this fixture ever *compiles*, the privacy of one or more of the
//! rebuild-only edge-storage mutators has regressed and the iter-4
//! fix has been partially reverted. Expected failure: `E0624`
//! ("associated function `X` is private") for each of the four
//! accesses.
//!
//! Note: this fixture is compiled by the trybuild harness with
//! `rebuild-internals` enabled (mimicking `sqry-daemon`'s feature set).
//! The `pub(crate)` visibility is *not* feature-gated: it applies
//! unconditionally, so enabling `rebuild-internals` does not re-open
//! the API. That is the whole point — the feature is for gating the
//! `RebuildGraph` / `clone_for_rebuild` surface, not for unlocking
//! raw edge-storage mutation.

use std::collections::HashMap;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::{BidirectionalEdgeStore, DeltaBuffer, EdgeStore};
use sqry_core::graph::unified::storage::CsrGraph;
use sqry_core::graph::unified::string::StringId;

fn csr_edge_kind_mut_is_unreachable(csr: &mut CsrGraph) {
    // `CsrGraph::edge_kind_mut` is `pub(crate)` — must fail with E0624.
    let _ = csr.edge_kind_mut();
}

fn edge_store_csr_mut_is_unreachable(store: &mut EdgeStore) {
    // `EdgeStore::csr_mut` is `pub(crate)` — must fail with E0624.
    let _ = store.csr_mut();
}

fn delta_buffer_iter_mut_is_unreachable(delta: &mut DeltaBuffer) {
    // `DeltaBuffer::iter_mut` is `pub(crate)` — must fail with E0624.
    let _ = delta.iter_mut();
}

fn bidirectional_rewrite_is_unreachable(edges: &mut BidirectionalEdgeStore) {
    // `BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`
    // is `pub(crate)` — must fail with E0624.
    let remap: HashMap<StringId, StringId> = HashMap::new();
    edges.rewrite_edge_kind_string_ids_through_remap(&remap);
}

fn main() {
    // We never actually execute these — trybuild only needs the
    // compile errors. Construct the types via the public path so the
    // only errors are the four visibility failures above.
    let graph = CodeGraph::new();
    let _ = graph;
    // The `clone_for_rebuild` path gives us a `RebuildGraph`, but we
    // deliberately do NOT go through it here — the fixture's purpose
    // is to prove direct access to the mutators is blocked, which
    // means we want the trybuild harness to report exactly four
    // `E0624` diagnostics and nothing else. The function bodies
    // above are dead code from main's perspective, but trybuild
    // still type-checks (and rejects) them.
}
