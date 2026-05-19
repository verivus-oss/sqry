//! Gate 0c trybuild fixture: there is no public path from
//! `RebuildGraph` to `CodeGraph` other than `RebuildGraph::finalize()`.
//!
//! If this fixture ever *compiles*, the type-enforced publish-path
//! invariant (plan §H lines 711, 738) has regressed — someone has
//! made the assembler public or added a `From` impl.

use sqry_core::graph::unified::concurrent::CodeGraph;

fn main() {
    let graph = CodeGraph::new();
    // The only Rust path from `RebuildGraph` to `CodeGraph` is
    // `RebuildGraph::finalize()`. Attempting to call the private
    // constructor directly must fail with E0624 ("method `X` is
    // private") or E0603 ("function `X` is private").
    let _: CodeGraph = CodeGraph::__assemble_from_rebuild_parts_internal(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        0u64,
        0u64,
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    let _ = graph;
}
