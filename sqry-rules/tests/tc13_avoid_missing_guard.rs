//! TC13: L1 Primitive B (`PathQuery.avoid`) + the `missing_guard` /
//! `trust_boundary` detectors, run end to end through the production backend.
//!
//! The load-bearing negative: a sink reachable ONLY through the guard yields NO
//! guard-avoiding path (the detector reports nothing), while a sink also
//! reachable on a guard-free path IS reported.

mod common;

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::{
    CodeGraph, EdgeKind, GraphSnapshot, NodeId, NodeKind, ResolvedVia,
};
use sqry_db::planner::StringPattern;

use sqry_rules::ir::{PathKind, RuleEndpoint, RuleNode};
use sqry_rules::rules::security::missing_guard;
use sqry_rules::{RuleBuilder, RuleEngine, RuleOutput, SqryDbRuleBackend};

fn func(
    graph: &mut CodeGraph,
    file: sqry_core::graph::unified::file::FileId,
    name: &str,
) -> NodeId {
    let nid = graph.strings_mut().intern(name).unwrap();
    let id = graph
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, nid, file).with_qualified_name(nid))
        .unwrap();
    graph
        .indices_mut()
        .add(id, NodeKind::Function, nid, Some(nid), file);
    id
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

fn path_count(snapshot: Arc<GraphSnapshot>, plan: &sqry_rules::ir::RulePlan) -> usize {
    run_paths(snapshot, plan).0
}

/// Runs a path plan, returning (surviving path count, whether any
/// `PathConstructed` witness step was emitted).
fn run_paths(snapshot: Arc<GraphSnapshot>, plan: &sqry_rules::ir::RulePlan) -> (usize, bool) {
    let db = common::query_db_for(snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let run = RuleEngine::new().run(&backend, plan).unwrap();
    let witnessed = run
        .witness
        .steps
        .iter()
        .any(|step| matches!(step, sqry_rules::witness::RuleStep::PathConstructed { .. }));
    match run.output {
        RuleOutput::Paths(paths) => (paths.len(), witnessed),
        other => panic!("expected Paths, got {other:?}"),
    }
}

// ---- Primitive B: PathQuery.avoid ----

#[test]
fn avoid_drops_paths_that_pass_through_the_avoid_set() {
    // entry -> sink (direct), and entry -> guard -> sink.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let entry = func(&mut g, file, "entry");
    let guard = func(&mut g, file, "guard");
    let sink = func(&mut g, file, "sink");
    call(&mut g, file, entry, sink);
    call(&mut g, file, entry, guard);
    call(&mut g, file, guard, sink);
    let snapshot = Arc::new(g.snapshot());

    let scan = |name: &str| {
        RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: Some(StringPattern::exact(name)),
        }))
    };

    // Without avoid: both paths (direct + via guard) are enumerated.
    let all = RuleBuilder::path_query(scan("entry"), scan("sink"), PathKind::Calls, 5, Some(32))
        .build()
        .unwrap();
    assert_eq!(
        path_count(Arc::clone(&snapshot), &all),
        2,
        "direct + via-guard"
    );

    // With avoid=guard: only the direct, guard-free path survives.
    let avoiding = RuleBuilder::path_query_avoiding(
        scan("entry"),
        scan("sink"),
        scan("guard"),
        PathKind::Calls,
        5,
        Some(32),
    )
    .build()
    .unwrap();
    assert_eq!(
        path_count(snapshot, &avoiding),
        1,
        "only the guard-avoiding path remains"
    );
}

// ---- D3: missing_guard ----

#[test]
fn missing_guard_reports_a_sink_reachable_without_the_guard() {
    // entry -> sink (guard-free), and entry -> guard -> sink.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let entry = func(&mut g, file, "entry");
    let guard = func(&mut g, file, "checkperm");
    let sink = func(&mut g, file, "writefile");
    call(&mut g, file, entry, sink);
    call(&mut g, file, entry, guard);
    call(&mut g, file, guard, sink);

    let def = missing_guard::definition(
        "test.mg",
        StringPattern::contains("entry"),
        StringPattern::contains("writefile"),
        StringPattern::contains("checkperm"),
        6,
    );
    assert_eq!(
        path_count(Arc::new(g.snapshot()), &def.plan),
        1,
        "the guard-free entry->sink path is a finding"
    );
}

#[test]
fn missing_guard_surviving_paths_exclude_the_guard_node() {
    // entry -> sink (guard-free), and entry -> guard -> sink. The one surviving
    // finding must not traverse the guard.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let entry = func(&mut g, file, "entry");
    let guard = func(&mut g, file, "checkperm");
    let sink = func(&mut g, file, "writefile");
    call(&mut g, file, entry, sink);
    call(&mut g, file, entry, guard);
    call(&mut g, file, guard, sink);

    let def = missing_guard::definition(
        "test.mg",
        StringPattern::contains("entry"),
        StringPattern::contains("writefile"),
        StringPattern::contains("checkperm"),
        6,
    );
    let db = common::query_db_for(Arc::new(g.snapshot()));
    let backend = SqryDbRuleBackend::new(&db);
    let RuleOutput::Paths(paths) = RuleEngine::new().run(&backend, &def.plan).unwrap().output
    else {
        panic!("expected Paths");
    };
    assert_eq!(paths.len(), 1);
    assert!(
        !paths[0].nodes.contains(&guard),
        "the guard-avoiding finding must not traverse the guard node"
    );
}

#[test]
fn missing_guard_is_empty_when_every_path_passes_the_guard() {
    // The ONLY route to the sink goes through the guard: entry -> guard -> sink.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let entry = func(&mut g, file, "entry");
    let guard = func(&mut g, file, "checkperm");
    let sink = func(&mut g, file, "writefile");
    call(&mut g, file, entry, guard);
    call(&mut g, file, guard, sink);

    let def = missing_guard::definition(
        "test.mg",
        StringPattern::contains("entry"),
        StringPattern::contains("writefile"),
        StringPattern::contains("checkperm"),
        6,
    );
    assert_eq!(
        path_count(Arc::new(g.snapshot()), &def.plan),
        0,
        "every path passes the guard, so there is no missing-guard finding"
    );
}

#[test]
fn missing_guard_filtered_paths_emit_no_path_constructed_witness() {
    // Every route passes the guard, so every path is filtered: no findings AND
    // no per-path witness (filtered paths are `continue`d before witnessing).
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let entry = func(&mut g, file, "entry");
    let guard = func(&mut g, file, "checkperm");
    let sink = func(&mut g, file, "writefile");
    call(&mut g, file, entry, guard);
    call(&mut g, file, guard, sink);

    let def = missing_guard::definition(
        "test.mg",
        StringPattern::contains("entry"),
        StringPattern::contains("writefile"),
        StringPattern::contains("checkperm"),
        6,
    );
    let (count, witnessed) = run_paths(Arc::new(g.snapshot()), &def.plan);
    assert_eq!(count, 0);
    assert!(
        !witnessed,
        "filtered paths must not emit a PathConstructed step"
    );
}

#[test]
fn trust_boundary_is_empty_when_every_path_passes_the_validator() {
    // The only route to the sink is validated: recv -> validate -> exec.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let recv = func(&mut g, file, "recv");
    let validate = func(&mut g, file, "validate");
    let sink = func(&mut g, file, "exec");
    call(&mut g, file, recv, validate);
    call(&mut g, file, validate, sink);

    let def = missing_guard::trust_boundary(
        "test.tb",
        StringPattern::contains("recv"),
        StringPattern::contains("exec"),
        StringPattern::contains("validate"),
        6,
    );
    assert_eq!(
        path_count(Arc::new(g.snapshot()), &def.plan),
        0,
        "every boundary crossing is validated, so there is no finding"
    );
}

#[test]
fn trust_boundary_wrapper_computes_the_same_guard_avoiding_reachability() {
    // recv -> sink (unvalidated), and recv -> validate -> sink.
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("src/lib.rs")).unwrap();
    let recv = func(&mut g, file, "recv");
    let validate = func(&mut g, file, "validate");
    let sink = func(&mut g, file, "exec");
    call(&mut g, file, recv, sink);
    call(&mut g, file, recv, validate);
    call(&mut g, file, validate, sink);

    let def = missing_guard::trust_boundary(
        "test.tb",
        StringPattern::contains("recv"),
        StringPattern::contains("exec"),
        StringPattern::contains("validate"),
        6,
    );
    assert_eq!(
        path_count(Arc::new(g.snapshot()), &def.plan),
        1,
        "the unvalidated recv->exec path crosses the boundary without the validator"
    );
}
