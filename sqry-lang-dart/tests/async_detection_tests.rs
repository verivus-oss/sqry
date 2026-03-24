// PoC Tests: FT-B.0 (Dart Async PoC) - graph-native
//
// These tests validate that the graph builder correctly marks async functions.

use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_dart::DartPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let sqry_core::graph::unified::build::staging::StagingOp::InternString {
            local_id,
            value,
        } = op
        {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    name: &str,
    kind: NodeKind,
) -> Option<&'a sqry_core::graph::unified::storage::NodeEntry> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let sqry_core::graph::unified::build::staging::StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return Some(entry);
            }
        }
    }
    None
}

fn build_graph(source: &[u8]) -> StagingGraph {
    let plugin = DartPlugin::default();
    let file = PathBuf::from("test.dart");
    let tree = plugin.parse_ast(source).expect("parse failed");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

// ========================================
// TRUE POSITIVE TESTS
// ========================================

#[test]
fn test_async_true_positive_basic() {
    let source = b"Future<void> realAsync() async { print('test'); }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "realAsync", NodeKind::Function)
        .expect("realAsync function not found");
    assert!(entry.is_async, "realAsync should be detected as async");
}

#[test]
fn test_async_true_positive_arrow() {
    let source = b"Future<String> asyncArrow() async => 'result';";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "asyncArrow", NodeKind::Function)
        .expect("asyncArrow function not found");
    assert!(entry.is_async, "asyncArrow should be detected as async");
}

#[test]
fn test_async_star_true_positive() {
    let source = b"Future<int> asyncStar() async* { yield 1; }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "asyncStar", NodeKind::Function)
        .expect("asyncStar function not found");
    assert!(entry.is_async, "asyncStar should be detected as async");
}

// ========================================
// FALSE NEGATIVE TESTS
// ========================================

#[test]
fn test_async_false_negative_comment() {
    let source = b"Future<void> commentAsync() { // async comment\n print('test'); }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "commentAsync", NodeKind::Function)
        .expect("commentAsync function not found");
    assert!(
        !entry.is_async,
        "commentAsync should NOT be detected as async"
    );
}

#[test]
fn test_async_false_negative_string() {
    let source = b"Future<void> stringAsync() { print('async function'); }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "stringAsync", NodeKind::Function)
        .expect("stringAsync function not found");
    assert!(
        !entry.is_async,
        "stringAsync should NOT be detected as async"
    );
}

#[test]
fn test_async_false_negative_identifier() {
    let source = b"Future<void> asyncHelper() { print('test'); }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "asyncHelper", NodeKind::Function)
        .expect("asyncHelper function not found");
    assert!(
        !entry.is_async,
        "asyncHelper should NOT be detected as async"
    );
}
