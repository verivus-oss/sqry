//! Gate 0c trybuild fixture: `RebuildGraph` fields are `pub(crate)`,
//! so external callers cannot destructure a `RebuildGraph` to
//! reconstruct a `CodeGraph` piecemeal.
//!
//! If this fixture ever *compiles*, the `RebuildGraph` field privacy
//! has regressed. The daemon rebuild dispatcher (Task 4 Step 4) only
//! reaches the fields through `pub(crate)` visibility inside
//! `sqry-core`; external consumption must go through `finalize()`.

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::rebuild::RebuildGraph;

fn inspect(rebuild: &RebuildGraph) {
    // Every field on `RebuildGraph` is `pub(crate)`. From outside
    // `sqry-core` these accesses must fail with E0616 ("field `X` of
    // struct `RebuildGraph` is private").
    let _ = &rebuild.nodes;
    let _ = &rebuild.edges;
    let _ = &rebuild.strings;
    let _ = &rebuild.files;
    let _ = &rebuild.indices;
    let _ = &rebuild.macro_metadata;
    let _ = &rebuild.node_provenance;
    let _ = &rebuild.edge_provenance;
    let _ = rebuild.fact_epoch;
    let _ = rebuild.epoch;
    let _ = &rebuild.confidence;
    let _ = &rebuild.scope_arena;
    let _ = &rebuild.alias_table;
    let _ = &rebuild.shadow_table;
    let _ = &rebuild.scope_provenance_store;
    let _ = &rebuild.file_segments;
    let _ = &rebuild.tombstones;
    let _ = &rebuild.drained_tombstones;
}

fn main() {
    let graph = CodeGraph::new();
    let rebuild = graph.clone_for_rebuild();
    inspect(&rebuild);
}
