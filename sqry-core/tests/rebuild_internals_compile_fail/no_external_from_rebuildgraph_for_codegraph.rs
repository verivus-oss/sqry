//! Gate 0c trybuild fixture: an external crate cannot implement
//! `From<RebuildGraph> for CodeGraph`.
//!
//! Rust's orphan rule (E0117) forbids implementing a foreign trait
//! (`core::convert::From`) for a foreign type (`CodeGraph`) outside
//! the crate that defines at least one of them. This fixture proves
//! that even with `rebuild-internals` enabled, no downstream crate can
//! add a bypass path from `RebuildGraph` back to `CodeGraph` via a
//! trait impl — the only route remains `RebuildGraph::finalize()`.
//!
//! If this fixture ever *compiles*, either the orphan rule has been
//! lifted (compiler change) or `CodeGraph` / `From<RebuildGraph>` has
//! been re-exported from the same local crate as the impl site, which
//! would itself be a sqry-core architecture regression worth catching.

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::rebuild::RebuildGraph;

impl From<RebuildGraph> for CodeGraph {
    fn from(_: RebuildGraph) -> Self {
        CodeGraph::new()
    }
}

fn main() {}
