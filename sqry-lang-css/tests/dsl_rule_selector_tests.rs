//! Tests for CSS DSL constructs: Rule and Selector nodes
//!
//! Verifies that CSS rules and selectors are properly extracted as graph nodes.

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, edge::EdgeKind, node::NodeKind},
};
use sqry_lang_css::CssGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Parser;

fn parse_css(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

/// Helper to count nodes of a specific kind in staging operations
fn count_nodes_by_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddNode { entry, .. } if entry.kind == kind
            )
        })
        .count()
}

/// Helper to count Contains edges in staging operations
fn count_contains_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Contains,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn test_simple_rule_with_class_selector() {
    let source = r".button { color: red; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have at least 1 Rule node
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(
        rule_count >= 1,
        "Expected at least 1 Rule node, got {rule_count}"
    );

    // Should have at least 1 Selector node
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 1,
        "Expected at least 1 Selector node, got {selector_count}"
    );

    // Should have Contains edges
    let contains_count = count_contains_edges(&staging);
    assert!(
        contains_count >= 2,
        "Expected at least 2 Contains edges (module->rule, rule->selector), got {contains_count}"
    );
}

#[test]
fn test_rule_with_id_selector() {
    let source = r"#main { width: 100%; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Rule and Selector nodes
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(rule_count >= 1, "Expected at least 1 Rule node");

    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(selector_count >= 1, "Expected at least 1 Selector node");
}

#[test]
fn test_multiple_selectors_in_rule() {
    let source = r"div, span { margin: 0; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have 1 Rule node
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(rule_count >= 1, "Expected at least 1 Rule node");

    // Should have 2 Selector nodes (div and span)
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 2,
        "Expected at least 2 Selector nodes, got {selector_count}"
    );
}

#[test]
fn test_multiple_rules() {
    let source = r"
.container { display: flex; }
#header { background: blue; }
.button { color: red; }
";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have 3 Rule nodes
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(
        rule_count >= 3,
        "Expected at least 3 Rule nodes, got {rule_count}"
    );

    // Should have 3 Selector nodes
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 3,
        "Expected at least 3 Selector nodes, got {selector_count}"
    );
}

#[test]
fn test_nested_selectors() {
    let source = r".container > .item { padding: 10px; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have 1 Rule node
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(rule_count >= 1, "Expected at least 1 Rule node");

    // Should have Selector nodes for both .container and .item
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 2,
        "Expected at least 2 Selector nodes, got {selector_count}"
    );
}

#[test]
fn test_complex_css_structure() {
    let source = r"
:root {
    --primary-color: #007bff;
}

.button {
    color: var(--primary-color);
}

#main {
    width: 100%;
}

.container > .item {
    display: flex;
}
";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have multiple Rule nodes
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(
        rule_count >= 4,
        "Expected at least 4 Rule nodes, got {rule_count}"
    );

    // Should have Selector nodes
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 4,
        "Expected at least 4 Selector nodes, got {selector_count}"
    );
}

#[test]
fn test_element_selector() {
    let source = r"div { margin: 0; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Rule and Selector nodes
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(rule_count >= 1, "Expected at least 1 Rule node");

    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(selector_count >= 1, "Expected at least 1 Selector node");
}

#[test]
fn test_pseudo_class_selector() {
    let source = r".button:hover { background: blue; }";

    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;
    let file = PathBuf::from("test.css");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Rule node
    let rule_count = count_nodes_by_kind(&staging, NodeKind::Module);
    assert!(rule_count >= 1, "Expected at least 1 Rule node");

    // Should have Selector nodes (at least for .button)
    let selector_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        selector_count >= 1,
        "Expected at least 1 Selector node, got {selector_count}"
    );
}
