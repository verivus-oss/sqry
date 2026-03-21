// Phase0 Tests: Enum and Mixin Node Creation
//
// These tests validate that the graph builder correctly creates enum and mixin nodes.

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_dart::DartPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_node_by_kind(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return true;
            }
        }
    }
    false
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
// ENUM TESTS
// ========================================

#[test]
fn test_enum_basic() {
    let source = b"enum Color { red, green, blue }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "Color", NodeKind::Enum),
        "Color enum should be created"
    );
}

#[test]
fn test_enum_with_values() {
    let source = b"enum Status { pending, active, completed }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "Status", NodeKind::Enum),
        "Status enum should be created"
    );
}

#[test]
fn test_private_enum() {
    let source = b"enum _InternalState { init, running, stopped }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "_InternalState", NodeKind::Enum),
        "_InternalState enum should be created (even if private)"
    );
}

// ========================================
// MIXIN TESTS
// ========================================

#[test]
fn test_mixin_basic() {
    let source = b"mixin Logging { void log(String msg) { print(msg); } }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "Logging", NodeKind::Trait),
        "Logging mixin should be created as Trait"
    );
}

#[test]
fn test_mixin_empty() {
    let source = b"mixin EmptyMixin { }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "EmptyMixin", NodeKind::Trait),
        "EmptyMixin should be created"
    );
}

#[test]
fn test_private_mixin() {
    let source = b"mixin _InternalMixin { void helper() { } }";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "_InternalMixin", NodeKind::Trait),
        "_InternalMixin should be created (even if private)"
    );
}

// ========================================
// COMBINED TESTS
// ========================================

#[test]
fn test_enum_and_mixin_together() {
    let source = b"
enum Color { red, green, blue }
mixin Logging { void log(String msg) { } }
class MyClass { }
";
    let staging = build_graph(source);

    assert!(
        find_node_by_kind(&staging, "Color", NodeKind::Enum),
        "Color enum should be created"
    );
    assert!(
        find_node_by_kind(&staging, "Logging", NodeKind::Trait),
        "Logging mixin should be created as Trait"
    );
    assert!(
        find_node_by_kind(&staging, "MyClass", NodeKind::Class),
        "MyClass should be created"
    );
}
