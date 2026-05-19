//! Integration tests for the binding-plane post-filter used by CLI, MCP, and
//! LSP unused-symbol boundaries.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::UnusedScope;
use sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter;
use sqry_db::queries::{
    EntryPointsQuery, IsNodeUnusedQuery, ReachableFromEntryPointsQuery, UnusedKey, UnusedQuery,
};
use sqry_db::{QueryDb, QueryDbConfig};

struct NodeSpec<'a> {
    name: &'a str,
    qualified_name: Option<&'a str>,
    kind: NodeKind,
    file: &'a str,
    visibility: Option<&'a str>,
}

fn alloc_node(graph: &mut CodeGraph, spec: &NodeSpec<'_>) -> NodeId {
    let file_id = graph.files_mut().register(Path::new(spec.file)).unwrap();
    let name_id = graph.strings_mut().intern(spec.name).unwrap();
    let mut entry = NodeEntry::new(spec.kind, name_id, file_id);

    if let Some(qualified_name) = spec.qualified_name {
        let qualified_name_id = graph.strings_mut().intern(qualified_name).unwrap();
        entry = entry.with_qualified_name(qualified_name_id);
    }
    if let Some(visibility) = spec.visibility {
        let visibility_id = graph.strings_mut().intern(visibility).unwrap();
        entry = entry.with_visibility(visibility_id);
    }

    let node_id = graph.nodes_mut().alloc(entry).unwrap();
    graph.files_mut().record_node(file_id, node_id);
    node_id
}

fn add_call(graph: &mut CodeGraph, source: NodeId, target: NodeId) {
    let file_id = graph.nodes().get(source).unwrap().file;
    graph.edges_mut().add_edge(
        source,
        target,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_id,
    );
}

fn build_db(
    graph: &CodeGraph,
) -> (
    Arc<sqry_core::graph::unified::concurrent::GraphSnapshot>,
    QueryDb,
) {
    let snapshot = Arc::new(graph.snapshot());
    let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    db.register::<EntryPointsQuery>();
    db.register::<ReachableFromEntryPointsQuery>();
    db.register::<UnusedQuery>();
    db.register::<IsNodeUnusedQuery>();
    (snapshot, db)
}

fn raw_and_filtered(
    graph: &CodeGraph,
) -> (
    Vec<NodeId>,
    Vec<NodeId>,
    Arc<sqry_core::graph::unified::concurrent::GraphSnapshot>,
) {
    let (snapshot, db) = build_db(graph);
    let raw = db.get::<UnusedQuery>(&UnusedKey {
        scope: UnusedScope::All,
        max_results: 1_000_000,
    });
    let filtered = apply_binding_plane_post_filter(&raw, &snapshot, &db);
    (raw.as_ref().clone(), filtered, snapshot)
}

fn main_node(graph: &mut CodeGraph) -> NodeId {
    alloc_node(
        graph,
        &NodeSpec {
            name: "main",
            qualified_name: Some("main"),
            kind: NodeKind::Function,
            file: "lib.rs",
            visibility: None,
        },
    )
}

fn bare_helper(
    graph: &mut CodeGraph,
    file: &'static str,
    visibility: Option<&'static str>,
) -> NodeId {
    alloc_node(
        graph,
        &NodeSpec {
            name: "helper",
            qualified_name: Some("helper"),
            kind: NodeKind::Function,
            file,
            visibility,
        },
    )
}

fn qualified_helper(graph: &mut CodeGraph, file: &'static str) -> NodeId {
    alloc_node(
        graph,
        &NodeSpec {
            name: "helper",
            qualified_name: Some("module::helper"),
            kind: NodeKind::Function,
            file,
            visibility: None,
        },
    )
}

#[test]
fn suppresses_qualified_def_when_reachable_bare_peer_exists_in_same_file() {
    let mut graph = CodeGraph::new();
    let main = main_node(&mut graph);
    let phantom = bare_helper(&mut graph, "lib.rs", None);
    let real_def = qualified_helper(&mut graph, "lib.rs");
    add_call(&mut graph, main, phantom);

    let (raw, filtered, _snapshot) = raw_and_filtered(&graph);

    assert!(
        raw.contains(&real_def),
        "raw UnusedQuery must expose the graph-exact qualified definition"
    );
    assert!(
        !filtered.contains(&real_def),
        "post-filter must suppress the qualified definition masked by a reachable bare peer"
    );
    assert!(
        !raw.contains(&phantom),
        "reachable phantom should not itself appear as unused"
    );
}

#[test]
fn preserves_qualified_def_when_bare_peer_is_in_another_file() {
    let mut graph = CodeGraph::new();
    let main = main_node(&mut graph);
    let phantom = bare_helper(&mut graph, "a.rs", None);
    let real_def = qualified_helper(&mut graph, "b.rs");
    add_call(&mut graph, main, phantom);

    let (raw, filtered, _snapshot) = raw_and_filtered(&graph);

    assert!(raw.contains(&real_def));
    assert!(filtered.contains(&real_def));
}

#[test]
fn preserves_qualified_def_when_bare_peer_has_visibility() {
    let mut graph = CodeGraph::new();
    let main = main_node(&mut graph);
    let visible_peer = bare_helper(&mut graph, "lib.rs", Some("private"));
    let real_def = qualified_helper(&mut graph, "lib.rs");
    add_call(&mut graph, main, visible_peer);

    let (raw, filtered, _snapshot) = raw_and_filtered(&graph);

    assert!(raw.contains(&real_def));
    assert!(filtered.contains(&real_def));
}

#[test]
fn preserves_qualified_def_when_peer_kind_differs() {
    let mut graph = CodeGraph::new();
    let main = main_node(&mut graph);
    let phantom = alloc_node(
        &mut graph,
        &NodeSpec {
            name: "helper",
            qualified_name: Some("helper"),
            kind: NodeKind::Method,
            file: "lib.rs",
            visibility: None,
        },
    );
    let real_def = qualified_helper(&mut graph, "lib.rs");
    add_call(&mut graph, main, phantom);

    let (raw, filtered, _snapshot) = raw_and_filtered(&graph);

    assert!(raw.contains(&real_def));
    assert!(filtered.contains(&real_def));
}

#[test]
fn preserves_raw_order_while_removing_suppressed_nodes() {
    let mut graph = CodeGraph::new();
    let main = main_node(&mut graph);
    let phantom = bare_helper(&mut graph, "lib.rs", None);
    let real_def = qualified_helper(&mut graph, "lib.rs");
    let independent_unused = alloc_node(
        &mut graph,
        &NodeSpec {
            name: "other",
            qualified_name: Some("module::other"),
            kind: NodeKind::Function,
            file: "lib.rs",
            visibility: None,
        },
    );
    add_call(&mut graph, main, phantom);

    let (raw, filtered, _snapshot) = raw_and_filtered(&graph);
    let expected: Vec<NodeId> = raw
        .iter()
        .copied()
        .filter(|node_id| *node_id != real_def)
        .collect();

    assert!(raw.contains(&independent_unused));
    assert_eq!(filtered, expected);
}
