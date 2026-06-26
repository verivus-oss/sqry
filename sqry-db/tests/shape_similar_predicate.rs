//! U09 — `shape~=<symbol>` planner predicate, end-to-end.
//!
//! Builds a graph with four function nodes and per-function shape descriptors
//! (two structurally identical, two distinct), then drives the predicate through
//! the full planner pipeline: parse -> compile -> cost-gate -> fuse -> execute.
//! Mirrors the Phase β predicate-evaluation harness.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::storage::shape::MINHASH_LANES;
use sqry_core::graph::unified::storage::{ShapeDescriptor, ShapeHash128, SignatureShape};
use sqry_db::planner::{execute_plan, format_query, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};

fn descriptor(lane_fill: u32, shape_high: u64) -> ShapeDescriptor {
    ShapeDescriptor {
        minhash: [lane_fill; MINHASH_LANES],
        signature_shape: SignatureShape::default(),
        shape_hash: ShapeHash128 {
            high: shape_high,
            low: shape_high.wrapping_add(1),
        },
        ..ShapeDescriptor::default()
    }
}

struct Fixture {
    db: QueryDb,
    probe: NodeId,
    twin: NodeId,
    other1: NodeId,
    other2: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();
        let file = graph
            .files_mut()
            .register_with_language(Path::new("app.rs"), Some(Language::Rust))
            .expect("register file");

        let names = ["probe_fn", "twin_fn", "other_one", "other_two"];
        let mut ids = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let name_id = graph.strings_mut().intern(name).expect("intern");
            let start = 10 + i as u32 * 60;
            let entry = NodeEntry::new(NodeKind::Function, name_id, file)
                .with_qualified_name(name_id)
                .with_byte_range(start, start + 50);
            let id = graph.nodes_mut().alloc(entry).expect("alloc");
            graph
                .indices_mut()
                .add(id, NodeKind::Function, name_id, Some(name_id), file);
            ids.push((id, name_id));
        }

        // probe_fn and twin_fn share a structure (identical sketch + shape_hash);
        // the other two are distinct.
        let descriptors = [
            descriptor(0xAAAA_AAAA, 0x11),
            descriptor(0xAAAA_AAAA, 0x11),
            descriptor(0x5555_5555, 0x22),
            descriptor(0x3333_3333, 0x33),
        ];
        for ((id, _), d) in ids.iter().zip(descriptors) {
            graph.macro_metadata_mut().insert_shape_descriptor(*id, d);
        }

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(snapshot, QueryDbConfig::default());
        Self {
            db,
            probe: ids[0].0,
            twin: ids[1].0,
            other1: ids[2].0,
            other2: ids[3].0,
        }
    }
}

#[test]
fn parses_shape_similar_predicate() {
    let plan = parse_query("kind:function shape~=probe_fn").expect("parse");
    // The plan's tail filter must carry a ShapeSimilar predicate.
    let text = format_query(&plan);
    assert!(
        text.contains("shape~=probe_fn"),
        "formatted query must round-trip the shape predicate, got: {text}"
    );
}

#[test]
fn format_round_trips_shape_predicate() {
    // A bare predicate built through the API formats and parses back identically
    // when prefixed with a scan (the text grammar requires a leading scan).
    let formatted = format_query(&parse_query("kind:function shape~=parse_config").expect("parse"));
    let reparsed = parse_query(&formatted).expect("reparse");
    assert_eq!(
        formatted,
        format_query(&reparsed),
        "format must be idempotent"
    );
}

#[test]
fn shape_similar_returns_structural_twin_only() {
    let fx = Fixture::build();
    let plan = parse_query("kind:function shape~=probe_fn").expect("parse");
    let result = execute_plan(&plan, &fx.db);
    // Only the structural twin; never the probe itself, never the distinct fns.
    assert_eq!(
        result,
        vec![fx.twin],
        "shape~=probe_fn must return only the twin"
    );
    assert!(!result.contains(&fx.probe), "probe excludes itself");
    assert!(!result.contains(&fx.other1));
    assert!(!result.contains(&fx.other2));
}

#[test]
fn shape_similar_unknown_symbol_is_empty_not_error() {
    let fx = Fixture::build();
    let plan = parse_query("kind:function shape~=does_not_exist").expect("parse");
    let result = execute_plan(&plan, &fx.db);
    assert!(
        result.is_empty(),
        "an unknown probe matches nothing (no panic)"
    );
}

#[test]
fn shape_similar_composes_with_and() {
    // `kind:function shape~=probe_fn` already AND-composes a scan with the
    // predicate; verify the predicate also narrows correctly when the twin is
    // additionally filtered by name (intersection is empty here).
    let fx = Fixture::build();
    let plan = parse_query("kind:function shape~=probe_fn name:other_one").expect("parse");
    let result = execute_plan(&plan, &fx.db);
    assert!(
        result.is_empty(),
        "twin is not named other_one, so the AND is empty"
    );
}
