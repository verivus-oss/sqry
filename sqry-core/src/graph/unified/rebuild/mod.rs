//! Incremental rebuild infrastructure (sqryd daemon — A2 §F/§H/§K).
//!
//! This module houses the pre-implementation gates for the Task 4
//! incremental rebuild engine of the sqryd daemon plan
//! (`docs/superpowers/plans/2026-03-19-sqryd-daemon.md`):
//!
//! - [`coverage`] (Gate 0b, A2 §K): declares the [`coverage::NodeIdBearing`]
//!   trait plus the static-assertion coverage matrix binding it to every
//!   publish-visible NodeId-bearing structure on [`super::CodeGraph`]. This
//!   is the foundation for the `RebuildGraph::finalize()` 14-step contract
//!   (Gate 0c) and the bijection + tombstone-residue invariants (Gate 0d).
//! - [`rebuild_graph`] (Gate 0c, A2 §H): the [`rebuild_graph::RebuildGraph`]
//!   type, the `CodeGraph::clone_for_rebuild` entry point, and the canonical
//!   14-step `RebuildGraph::finalize` contract.
//!
//! Gate 0d will wire bijection + tombstone-residue invariants into
//! every publish site (the helper methods already live on
//! `super::CodeGraph` so Gate 0c's `finalize()` can call them in debug
//! builds).
//!
//! # Visibility split (Task 4 Step 1, fix on top of Gate 0c)
//!
//! The `rebuild_graph` module is `pub(crate)` **unconditionally** so
//! intra-`sqry-core` call sites (notably
//! `crate::graph::unified::build::incremental::incremental_rebuild`
//! that lands in Task 4 Step 4) can name
//! [`rebuild_graph::RebuildGraph`] without the `rebuild-internals`
//! feature being enabled. This matters because `sqry-core`'s own
//! library build never turns the feature on — the feature exists
//! solely to gate the **external** re-export below. Making the module
//! itself feature-gated would create a circular dependency: Step 4
//! would need a feature the daemon enables downstream in order to call
//! a function inside `sqry-core`.
//!
//! The only thing still gated on `rebuild-internals` is the
//! `pub use rebuild_graph::RebuildGraph;` re-export at the bottom of
//! this file. That re-export is the sole way an external crate
//! (`sqry-daemon`) can name the type. All five trybuild fixtures in
//! `sqry-core/tests/rebuild_internals_compile_fail/` continue to prove
//! that an external crate without the feature cannot reach the type;
//! the feature gate on the re-export is necessary and sufficient to
//! keep the external surface daemon-only. The
//! `rebuild_internals_whitelist.rs` CI audit still pins the feature
//! enable to `sqry-daemon`.
//!
//! Similarly, the `pub(crate) trait NodeIdBearing` in
//! [`coverage`] stays `pub(crate)` — the trait is infrastructure for
//! `finalize()` and the residue debug helper; it is not part of the
//! daemon-visible surface.

pub mod coverage;

// Gate 0c (A2 §H): `RebuildGraph` + `clone_for_rebuild` + `finalize()`.
//
// Visibility is `pub(crate)` unconditionally — sqry-core's own modules
// (`build::incremental::incremental_rebuild` in Task 4 Step 4) must be
// able to name `RebuildGraph` in non-feature-enabled builds.
// External access is gated exclusively by the feature-conditional
// `pub use` re-export below. See module docs above for the rationale.
pub(crate) mod rebuild_graph;

// External re-export: only visible to downstream crates when they enable
// the `rebuild-internals` feature. `sqry-daemon` is the only crate on
// the whitelist at `sqry-core/tests/rebuild_internals_whitelist.rs`.
#[cfg(feature = "rebuild-internals")]
#[allow(unused_imports)] // Consumer is sqry-daemon; the re-export is the
// whole point of the Gate 0c surface and must stay regardless.
pub use rebuild_graph::RebuildGraph;
