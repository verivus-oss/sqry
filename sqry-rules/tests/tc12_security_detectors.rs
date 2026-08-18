//! TC12: L1 Primitive A (`EdgeTraversal.emit`) + security detectors D1/D2, run
//! end to end through the production `SqryDbRuleBackend`.
//!
//! These tests exercise the REAL negative cases the v1 design gate flagged:
//! an unsafe function with only intra-language edges must NOT appear in
//! `unsafe_ffi_reach` (seed retention would have wrongly included it).

mod common;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::edge::kind::FfiConvention;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::{
    CodeGraph, EdgeKind, GraphSnapshot, NodeId, NodeKind, ResolvedVia,
};
use sqry_db::planner::{Direction, StringPattern};

use sqry_rules::ir::{RulePlan, TraversalEmit};
use sqry_rules::rules::security::{dangerous_sink, unsafe_ffi_reach};
use sqry_rules::{RuleBuilder, RuleEngine, RuleOutput, SqryDbRuleBackend};

/// Interns a function node (optionally unsafe) and registers it in the name index.
fn func(
    graph: &mut CodeGraph,
    file: sqry_core::graph::unified::file::FileId,
    name: &str,
    is_unsafe: bool,
) -> NodeId {
    let nid = graph.strings_mut().intern(name).unwrap();
    let id = graph
        .nodes_mut()
        .alloc(
            NodeEntry::new(NodeKind::Function, nid, file)
                .with_qualified_name(nid)
                .with_unsafe(is_unsafe),
        )
        .unwrap();
    graph
        .indices_mut()
        .add(id, NodeKind::Function, nid, Some(nid), file);
    id
}

fn ffi(
    graph: &mut CodeGraph,
    file: sqry_core::graph::unified::file::FileId,
    from: NodeId,
    to: NodeId,
) {
    graph.edges_mut().add_edge(
        from,
        to,
        EdgeKind::FfiCall {
            convention: FfiConvention::C,
        },
        file,
    );
}

fn call(
    graph: &mut CodeGraph,
    file: sqry_core::graph::unified::file::FileId,
    from: NodeId,
    to: NodeId,
) {
    graph.edges_mut().add_edge(
        from,
        to,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file,
    );
}

fn run(snapshot: Arc<GraphSnapshot>, plan: &RulePlan) -> Vec<NodeId> {
    run_with_witness(snapshot, plan).0
}

/// Like `run`, but also reports whether the witness recorded a per-edge
/// `EdgeTraversed` step (i.e. the plan took the witness-bearing backend path
/// rather than lowering to the planner).
fn run_with_witness(snapshot: Arc<GraphSnapshot>, plan: &RulePlan) -> (Vec<NodeId>, bool) {
    let db = common::query_db_for(snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let run = RuleEngine::new().run(&backend, plan).unwrap();
    let witnessed = run
        .witness
        .steps
        .iter()
        .any(|step| matches!(step, sqry_rules::witness::RuleStep::EdgeTraversed { .. }));
    let nodes = match run.output {
        RuleOutput::Nodes(n) => n,
        other => panic!("expected Nodes, got {other:?}"),
    };
    (nodes, witnessed)
}

// ---- Primitive A: emit modes ----

#[test]
fn emit_modes_select_sources_targets_or_reached() {
    // a -FfiCall-> b, a -FfiCall-> c. cross_boundary: Some(true) forces the
    // manual traverse pipeline so all three emit modes are compared on the same
    // execution path (a bare default-emit traverse would lower to the planner,
    // which has different seed-inclusion semantics).
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let a = func(&mut g, file, "a", false);
    let b = func(&mut g, file, "b", false);
    let c = func(&mut g, file, "c", false);
    ffi(&mut g, file, a, b);
    ffi(&mut g, file, a, c);
    let snapshot = Arc::new(g.snapshot());

    let plan = |emit: TraversalEmit| {
        RuleBuilder::new()
            .scan_with(
                Some(NodeKind::Function),
                None,
                Some(StringPattern::exact("a")),
            )
            .traverse_emitting(Direction::Forward, None, 2, Some(true), emit)
            .build()
            .unwrap()
    };

    let reached: HashSet<NodeId> = run(Arc::clone(&snapshot), &plan(TraversalEmit::ReachedNodes))
        .into_iter()
        .collect();
    assert!(reached.contains(&a) && reached.contains(&b) && reached.contains(&c));

    // Assert the ordered Vec, not a HashSet: `a` is the source of TWO edges, so
    // a broken deduper returning [a, a] must fail here.
    let sources = run(Arc::clone(&snapshot), &plan(TraversalEmit::EdgeSources));
    assert_eq!(
        sources,
        vec![a],
        "a emitted two edges but dedups to a single source"
    );

    // Exactly two distinct targets (length guards against a broken deduper;
    // first-seen iteration order between b and c is not contractual).
    let targets = run(Arc::clone(&snapshot), &plan(TraversalEmit::EdgeTargets));
    assert_eq!(targets.len(), 2, "two distinct targets, no duplicates");
    let target_set: HashSet<NodeId> = targets.into_iter().collect();
    assert_eq!(
        target_set,
        HashSet::from([b, c]),
        "b and c are the distinct edge targets"
    );
}

#[test]
fn emit_alone_forces_the_witness_path() {
    // emit isolated from cross_boundary: a plain traverse (edge_class None,
    // cross_boundary None) with emit=EdgeSources must STILL refuse lowering and
    // take the witness path, and return only the source. Contrast with the
    // all-default plan, which lowers (no per-edge witness).
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let a = func(&mut g, file, "a", false);
    let b = func(&mut g, file, "b", false);
    call(&mut g, file, a, b);
    let snapshot = Arc::new(g.snapshot());

    let plan = |emit: TraversalEmit| {
        RuleBuilder::new()
            .scan_with(
                Some(NodeKind::Function),
                None,
                Some(StringPattern::exact("a")),
            )
            .traverse_emitting(Direction::Forward, None, 2, None, emit)
            .build()
            .unwrap()
    };

    let (sources, witnessed) =
        run_with_witness(Arc::clone(&snapshot), &plan(TraversalEmit::EdgeSources));
    assert!(
        witnessed,
        "emit=EdgeSources alone must force the witness-bearing path"
    );
    assert_eq!(sources, vec![a], "only the source of the qualifying edge");

    let (_reached, default_witnessed) =
        run_with_witness(snapshot, &plan(TraversalEmit::ReachedNodes));
    assert!(
        !default_witnessed,
        "the all-default plan lowers to the planner (no per-edge witness)"
    );
}

// ---- D1: unsafe_ffi_reach ----

/// unsafe `u` -FfiCall-> ct ; unsafe `u_intra` -Calls-> it ; safe `s` -FfiCall-> st.
fn d1_fixture() -> (Arc<GraphSnapshot>, NodeId, NodeId, NodeId) {
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let u = func(&mut g, file, "u", true);
    let ct = func(&mut g, file, "ct", false);
    let u_intra = func(&mut g, file, "u_intra", true);
    let it = func(&mut g, file, "it", false);
    let s = func(&mut g, file, "s", false);
    let st = func(&mut g, file, "st", false);
    ffi(&mut g, file, u, ct);
    call(&mut g, file, u_intra, it);
    ffi(&mut g, file, s, st);
    (Arc::new(g.snapshot()), u, u_intra, s)
}

#[test]
fn unsafe_ffi_reach_finds_only_unsafe_fns_that_cross_a_boundary() {
    let (snapshot, u, u_intra, s) = d1_fixture();
    let (nodes, witnessed) = run_with_witness(snapshot, &unsafe_ffi_reach::definition().plan);
    let result: HashSet<NodeId> = nodes.into_iter().collect();

    assert!(
        witnessed,
        "the detector traverses via the witness path and records the crossing"
    );
    assert!(
        result.contains(&u),
        "unsafe fn with an FFI edge is a finding"
    );
    assert!(
        !result.contains(&u_intra),
        "REAL NEGATIVE: unsafe fn with only intra-language edges must be excluded (seed retention would wrongly include it)"
    );
    assert!(
        !result.contains(&s),
        "safe fn is not a finding even with an FFI edge"
    );
}

#[test]
fn unsafe_ffi_reach_is_empty_when_no_unsafe_code_crosses_a_boundary() {
    // Only a safe fn with an FFI edge, and an unsafe fn with only a call edge.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let s = func(&mut g, file, "s", false);
    let st = func(&mut g, file, "st", false);
    let u = func(&mut g, file, "u", true);
    let it = func(&mut g, file, "it", false);
    ffi(&mut g, file, s, st);
    call(&mut g, file, u, it);
    let result = run(Arc::new(g.snapshot()), &unsafe_ffi_reach::definition().plan);
    assert!(
        result.is_empty(),
        "no unsafe code crosses a boundary, got {result:?}"
    );
}

#[test]
fn unsafe_ffi_reach_never_reports_a_safe_intermediate_on_a_cross_boundary_chain() {
    // The exact multi-hop shape the code gate flagged: unsafe u -ffi-> safe mid
    // -ffi-> w. A depth>1 EdgeSources traversal would emit `mid` (a source of the
    // mid->w edge) even though it is SAFE. The detector is depth-1, so only the
    // unsafe seed `u` (source of u->mid) is reported; safe `mid` is excluded.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let u = func(&mut g, file, "u", true);
    let mid = func(&mut g, file, "mid", false);
    let w = func(&mut g, file, "w", false);
    ffi(&mut g, file, u, mid);
    ffi(&mut g, file, mid, w);
    let result: HashSet<NodeId> = run(Arc::new(g.snapshot()), &unsafe_ffi_reach::definition().plan)
        .into_iter()
        .collect();
    assert_eq!(
        result,
        HashSet::from([u]),
        "only the unsafe seed, never safe mid"
    );
    assert!(
        !result.contains(&mid),
        "safe intermediate must never be reported"
    );
}

// ---- D2: dangerous_sink ----

#[test]
fn dangerous_sink_reports_reachable_and_ignores_unreachable() {
    // reachable: src -Calls-> mid -Calls-> sink ; unrelated: other (no path).
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let src = func(&mut g, file, "read_input", false);
    let mid = func(&mut g, file, "helper", false);
    let sink = func(&mut g, file, "system_exec", false);
    call(&mut g, file, src, mid);
    call(&mut g, file, mid, sink);
    let snapshot = Arc::new(g.snapshot());

    let reachable = dangerous_sink::definition(
        "test.ds",
        StringPattern::contains("read_input"),
        StringPattern::contains("system_exec"),
        5,
    );
    let db = common::query_db_for(Arc::clone(&snapshot));
    let backend = SqryDbRuleBackend::new(&db);
    let out = RuleEngine::new()
        .run(&backend, &reachable.plan)
        .unwrap()
        .output;
    match out {
        RuleOutput::Paths(paths) => assert!(!paths.is_empty(), "a call path exists"),
        other => panic!("expected Paths, got {other:?}"),
    }

    let unreachable = dangerous_sink::definition(
        "test.ds",
        StringPattern::contains("read_input"),
        StringPattern::contains("nonexistent_sink"),
        5,
    );
    let out2 = RuleEngine::new()
        .run(&backend, &unreachable.plan)
        .unwrap()
        .output;
    match out2 {
        RuleOutput::Paths(paths) => assert!(paths.is_empty(), "no path to a missing sink"),
        other => panic!("expected Paths, got {other:?}"),
    }
}

// ---- Wiring + serialization ----

#[test]
fn security_pack_is_in_shipped_rules_and_carries_metadata() {
    let shipped = sqry_rules::rules::shipped_rules();
    let rule = shipped
        .iter()
        .find(|r| r.definition.id == unsafe_ffi_reach::RULE_ID)
        .expect("unsafe_ffi_reach is in shipped_rules()");
    // The advisory security metadata actually rides the shipped definition.
    assert_eq!(
        rule.definition.severity,
        Some(sqry_rules::witness::RuleSeverity::Warning)
    );
    assert!(rule.definition.description.is_some());
    assert!(rule.definition.remediation.is_some());
}

#[test]
fn unsafe_ffi_reach_definition_round_trips_through_toml() {
    let def = unsafe_ffi_reach::definition();
    let pack = sqry_rules::dsl::RulePack {
        schema_version: sqry_rules::dsl::RULE_PACK_SCHEMA_VERSION,
        rules: vec![def.clone()],
    };
    let toml = toml::to_string(&pack).expect("serialize security pack to TOML");
    let loaded = sqry_rules::dsl::load_rule_pack_str(&toml).expect("reload security pack");
    assert_eq!(loaded.rules, vec![def]);
}

// ---- Flat-builder footgun regression (gate F4) ----

#[test]
fn flat_scan_filter_cross_boundary_builder_is_a_known_footgun() {
    // A FLAT chain [NodeScan, Filter, EdgeTraversal] does NOT compose like the
    // nested unsafe_ffi_reach shape: the Filter tail step routes the chain to
    // Sequence, then the standalone Filter errors. Security detectors must use
    // the nested shape (as unsafe_ffi_reach::definition does), not this.
    let (snapshot, _u, _ui, _s) = d1_fixture();
    let flat = RuleBuilder::new()
        .scan_with(Some(NodeKind::Function), None, None)
        .filter(sqry_db::planner::Predicate::IsUnsafe(true))
        .traverse_cross_boundary(Direction::Forward, None, 1, Some(true))
        .build()
        .unwrap();
    let db = common::query_db_for(snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let err = RuleEngine::new().run(&backend, &flat).unwrap_err();
    assert!(
        format!("{err}").contains("chain input")
            || format!("{err:?}").contains("InvalidRuleSource"),
        "flat filter+traversal chain must fail clearly, got {err:?}"
    );
}
