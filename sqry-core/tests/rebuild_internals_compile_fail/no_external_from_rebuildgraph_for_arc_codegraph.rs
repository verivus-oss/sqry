//! Gate 0c trybuild fixture: an external crate cannot implement
//! `From<RebuildGraph> for Arc<CodeGraph>`.
//!
//! Same orphan-rule (E0117) argument as the `CodeGraph`-valued variant:
//! `Arc<CodeGraph>` is a foreign type (both `Arc` and `CodeGraph` are
//! defined outside the downstream crate), so no downstream crate can
//! install a `From` impl that bypasses `finalize()` → `Arc::new`.
//!
//! The plan §H "Type-enforced publish path" paragraph
//! (`docs/superpowers/plans/2026-03-19-sqryd-daemon.md` line 711)
//! asserts: *"The only Rust path from `RebuildGraph` to
//! `Arc<CodeGraph>` is `RebuildGraph::finalize().map(Arc::new)`."*
//! This fixture locks that invariant against the `Arc<CodeGraph>`
//! target specifically, since the `ArcSwap<CodeGraph>::store` signature
//! only accepts `Arc<CodeGraph>`.

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::rebuild::RebuildGraph;

impl From<RebuildGraph> for Arc<CodeGraph> {
    fn from(_: RebuildGraph) -> Self {
        Arc::new(CodeGraph::new())
    }
}

fn main() {}
