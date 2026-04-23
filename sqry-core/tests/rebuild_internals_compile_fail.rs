//! [A2 §H, Gate 0c] Trybuild harness for compile-fail fixtures proving
//! the invariants the `RebuildGraph` type-split is meant to enforce.
//!
//! Each fixture under `tests/rebuild_internals_compile_fail/` is
//! compiled with `sqry-core`'s `rebuild-internals` feature enabled
//! (mimicking how `sqry-daemon` will consume the surface) and is
//! *expected to fail* with the error captured in its sibling
//! `.stderr` file.
//!
//! # What this harness proves (and what it does not)
//!
//! The plan §H "Type-enforced publish path" guarantees that the only
//! Rust path from `RebuildGraph` to `Arc<CodeGraph>` is
//! `RebuildGraph::finalize().map(Arc::new)`. Five concrete bypass
//! paths could theoretically exist in an external crate; this harness
//! rejects each of them by a distinct compiler diagnostic:
//!
//! | Fixture                                                     | Bypass attempt                                                    | Rejected by |
//! |-------------------------------------------------------------|-------------------------------------------------------------------|-------------|
//! | `rebuild_graph_no_public_assembly.rs`                       | Call `CodeGraph::__assemble_from_rebuild_parts_internal` directly | `E0624`     |
//! | `rebuild_graph_fields_private.rs`                           | Destructure `RebuildGraph` fields and hand them to the private ctor | `E0616` (×17 fields) |
//! | `no_external_from_rebuildgraph_for_codegraph.rs`            | `impl From<RebuildGraph> for CodeGraph` in a downstream crate     | `E0117` orphan rule |
//! | `no_external_from_rebuildgraph_for_arc_codegraph.rs`        | `impl From<RebuildGraph> for Arc<CodeGraph>` in a downstream crate | `E0117` orphan rule |
//! | `edge_store_mut_unreachable_without_feature.rs`             | Call the four rebuild-only edge-storage mutators (`CsrGraph::edge_kind_mut`, `EdgeStore::csr_mut`, `DeltaBuffer::iter_mut`, `BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`) directly | `E0624` (×4) |
//! | `graph_mutation_target_is_crate_private.rs`                 | Name `GraphMutationTarget` from a downstream crate — the mutation-plane trait that Task 4 Step 4 Phase 1 introduced — so an external crate could implement the trait and smuggle mutations into `CodeGraph` / `RebuildGraph` | `E0603` on `mutation_target` module |
//!
//! These six fixtures collectively cover every *Rust-language-level*
//! bypass an external crate could attempt with today's `sqry-core`
//! public surface and with `rebuild-internals` enabled. Specifically
//! proven:
//!
//! - The private constructor is unreachable from any external call
//!   site (fixture 1).
//! - Piecewise field access is unreachable (fixture 2), so a downstream
//!   crate cannot assemble a fresh `CodeGraph` from `RebuildGraph`'s
//!   guts field-by-field.
//! - No trait-impl bypass exists for the two relevant target types
//!   (fixtures 3 & 4), so `From` / `Into` specialisation cannot be
//!   retrofitted from a downstream crate.
//! - The four rebuild-only edge-storage mutators used by finalize
//!   step 1 (`CsrGraph::edge_kind_mut`, `EdgeStore::csr_mut`,
//!   `DeltaBuffer::iter_mut`,
//!   `BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`)
//!   are `pub(crate)` and therefore unreachable from any external
//!   crate — even with `rebuild-internals` enabled (fixture 5). This
//!   closes the iter-4 Codex-BLOCK finding: committed edge storage
//!   can only be mutated through `RebuildGraph::finalize()`.
//! - The `GraphMutationTarget` trait (Task 4 Step 4 Phase 1) is
//!   `pub(crate)` — the `mutation_target` module itself is
//!   `pub(crate) mod`, so external crates cannot name the trait and
//!   therefore cannot implement it (fixture 6). This keeps the
//!   mutation-plane abstraction that lets Pass 1-5 helpers operate on
//!   `RebuildGraph` intra-crate infrastructure; external crates must
//!   continue to reach the rebuild surface only through
//!   `CodeGraph::clone_for_rebuild` + `RebuildGraph::finalize`.
//!
//! # What this harness does NOT prove
//!
//! - It cannot prove that no *future* `pub fn` or associated function
//!   on `sqry-core` will expose a new bypass. That is a code-review
//!   invariant: any future `pub fn` returning `CodeGraph` or
//!   `Arc<CodeGraph>` from `RebuildGraph` state must be weighed against
//!   §H line 711. The Gate 0c documentation and the CI-enforced
//!   whitelist in `rebuild_internals_whitelist.rs` backstop this by
//!   pinning the feature definition to `sqry-core` and the feature
//!   enabler to `sqry-daemon` — new public bypass surfaces would have
//!   to land in `sqry-core` (code-owner-gated per §H).
//! - It cannot prove the absence of `unsafe` / transmute-based bypass
//!   paths. Those are a soundness concern governed by `unsafe` review,
//!   not by compile-time diagnostics.
//! - It does not re-run the `rebuild_internals_whitelist` audit — that
//!   test lives in its own file and catches the complementary problem
//!   of the feature being enabled in an unauthorised crate.
//!
//! # Forward-compatibility note
//!
//! trybuild only stabilises `.stderr` snapshots against a specific
//! rustc version. If a future rustc rewords `E0117` / `E0603` /
//! `E0616` / `E0624` diagnostics, the snapshots need refreshing — run
//! `TRYBUILD=overwrite cargo test --test rebuild_internals_compile_fail`
//! and review the resulting diff before committing.

#[test]
fn compile_fail_suite() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/rebuild_internals_compile_fail/*.rs");
}
