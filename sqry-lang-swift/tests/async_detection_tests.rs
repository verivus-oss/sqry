// FT-B.1 Tests: Swift Async Detection (graph-native)
//
// These tests validate that the graph builder correctly sets async flags for
// Swift functions without relying on legacy extraction.

use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_swift::SwiftPlugin;
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
            && let Some(node_name) = strings.get(&entry.name.index())
            && node_name == name
        {
            return Some(entry);
        }
    }
    None
}

fn build_graph(source: &[u8]) -> StagingGraph {
    let plugin = SwiftPlugin::default();
    let file = PathBuf::from("test.swift");
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
    let source = b"func realAsync() async -> String { return \"test\" }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "realAsync", NodeKind::Function)
        .expect("realAsync function not found");
    assert!(entry.is_async, "realAsync should be detected as async");
}

#[test]
fn test_async_throws() {
    let source = b"func asyncThrows() async throws -> Data { return Data() }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "asyncThrows", NodeKind::Function)
        .expect("asyncThrows function not found");
    assert!(entry.is_async, "asyncThrows should be detected as async");
}

// ========================================
// FALSE NEGATIVE TESTS
// ========================================

#[test]
fn test_async_false_negative_comment() {
    let source = b"func commentAsync() -> Void { // This function is not async despite the comment\n print(\"test\") }";
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
    let source = b"func stringAsync() -> Void { let msg = \"call async function\"; print(msg) }";
    let staging = build_graph(source);

    let entry = find_node_entry(&staging, "stringAsync", NodeKind::Function)
        .expect("stringAsync function not found");
    assert!(
        !entry.is_async,
        "stringAsync should NOT be detected as async"
    );
}
