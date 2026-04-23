use super::*;
use crate::graph::node::Language;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::NodeKind;
use crate::graph::unified::storage::arena::NodeEntry;
use std::path::Path;
use std::sync::Arc;

#[test]
fn test_executor_new() {
    let executor = QueryExecutor::new();
    // Executor is created successfully (plugins will be registered at runtime)
    assert!(executor.plugin_manager.plugins().is_empty());
}

#[test]
fn test_execute_on_graph_nonexistent_path() {
    let executor = QueryExecutor::new();
    let result = executor.execute_on_graph("kind:function", Path::new("/nonexistent/path"));
    // Should error because no graph exists
    assert!(result.is_err());
}

// Note: Full integration tests with actual plugins are in the CLI integration tests
// where plugins are properly registered. Tests for predicate evaluation and index
// operations were removed as part of the legacy index to CodeGraph migration.

// ------------------------------------------------------------------------
// Shared fixture for `execute_on_preloaded_graph` tests.
// ------------------------------------------------------------------------
//
// Builds a small `CodeGraph` with two Function nodes and one Struct node,
// all registered in the auxiliary indices so `kind:function` predicate
// evaluation returns a non-empty result. Mirrors the pattern in
// `sqry-core/tests/unified_graph_indices_persistence_test.rs::create_test_graph`
// but minimized to the smallest set that exercises the preloaded-graph
// code path end-to-end.
fn build_small_preloaded_graph() -> CodeGraph {
    let mut graph = CodeGraph::new();

    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/lib.rs"), Some(Language::Rust))
        .expect("register file");

    let nodes: &[(&str, &str, NodeKind, u32, u32)] = &[
        ("alpha", "test::alpha", NodeKind::Function, 1, 10),
        ("beta", "test::beta", NodeKind::Function, 12, 20),
        ("Holder", "test::Holder", NodeKind::Struct, 22, 30),
    ];

    for (name, qname, kind, start_line, end_line) in nodes {
        let name_id = graph.strings_mut().intern(name).expect("intern name");
        let qname_id = graph.strings_mut().intern(qname).expect("intern qname");

        let entry = NodeEntry::new(*kind, name_id, file_id)
            .with_location(*start_line, 0, *end_line, 0)
            .with_qualified_name(qname_id);

        let node_id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");

        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );
    }

    graph
}

#[test]
fn execute_on_preloaded_graph_success() {
    let executor = QueryExecutor::new();
    let graph = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let results = executor
        .execute_on_preloaded_graph(Arc::clone(&graph), "kind:function", workspace_root, None)
        .expect("execute_on_preloaded_graph succeeds");

    // Fixture has two Function nodes.
    assert_eq!(
        results.len(),
        2,
        "expected the two Function nodes to match kind:function"
    );

    // Collect the matched names for assertion on set identity.
    let mut names: Vec<String> = results
        .iter()
        .map(|m| m.name().map(|n| n.to_string()).unwrap_or_default())
        .collect();
    names.sort();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn execute_on_preloaded_graph_bypasses_cache() {
    let executor = QueryExecutor::new();

    // Pre-condition: no graph in the executor's process-wide cache.
    assert!(
        executor.graph_cache.read().is_none(),
        "executor graph_cache should start empty"
    );

    let graph = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let _results = executor
        .execute_on_preloaded_graph(Arc::clone(&graph), "kind:function", workspace_root, None)
        .expect("execute_on_preloaded_graph succeeds");

    // Post-condition: the preloaded-graph path must NOT have written into
    // the executor's graph cache. This is the core daemon invariant — the
    // daemon holds the graph under a workspace lock and must not let the
    // executor pollute its process-wide cache with a stale reference.
    assert!(
        executor.graph_cache.read().is_none(),
        "execute_on_preloaded_graph must not populate graph_cache"
    );
}

#[test]
fn execute_on_preloaded_graph_identity() {
    let executor = QueryExecutor::new();
    let graph_arc = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let results = executor
        .execute_on_preloaded_graph(
            Arc::clone(&graph_arc),
            "kind:function",
            workspace_root,
            None,
        )
        .expect("execute_on_preloaded_graph succeeds");

    // The QueryResults constructor (results.rs line 46) takes the same
    // Arc<CodeGraph> we passed in, so the results' underlying graph must
    // be pointer-equal to the one we supplied. This guarantees the
    // preloaded-graph path is genuinely zero-copy at the graph level.
    assert!(
        Arc::ptr_eq(&graph_arc, results.graph_arc_for_test()),
        "QueryResults graph Arc must be pointer-equal to the caller-supplied Arc"
    );
}
