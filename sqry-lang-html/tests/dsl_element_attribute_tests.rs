//! Tests for HTML DSL constructs: Element and Attribute nodes
//!
//! Verifies that HTML elements and attributes are properly extracted as graph nodes.

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, edge::EdgeKind, node::NodeKind},
};
use sqry_lang_html::HtmlGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Parser;

fn parse_html(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .unwrap();
    parser.parse(source.as_bytes(), None).unwrap()
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
fn test_simple_element() {
    let source = r"<div></div>";

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have at least 1 Element node
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 1,
        "Expected at least 1 Element node, got {element_count}"
    );
}

#[test]
fn test_element_with_attributes() {
    let source = r#"<div class="container" id="main"></div>"#;

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Element node
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 1,
        "Expected at least 1 Element node, got {element_count}"
    );

    // Should have Attribute nodes (class and id)
    let attribute_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        attribute_count >= 2,
        "Expected at least 2 Attribute nodes (class, id), got {attribute_count}"
    );

    // Should have Contains edges (element -> attributes)
    let contains_count = count_contains_edges(&staging);
    assert!(
        contains_count >= 2,
        "Expected at least 2 Contains edges, got {contains_count}"
    );
}

#[test]
fn test_nested_elements() {
    let source = r"<div><span>text</span></div>";

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have multiple Element nodes (div and span)
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 2,
        "Expected at least 2 Element nodes (div, span), got {element_count}"
    );
}

#[test]
fn test_self_closing_element() {
    let source = r#"<img src="test.jpg" alt="test"/>"#;

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Element node for img
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 1,
        "Expected at least 1 Element node (img), got {element_count}"
    );

    // Should have Attribute nodes (src and alt)
    let attribute_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        attribute_count >= 2,
        "Expected at least 2 Attribute nodes (src, alt), got {attribute_count}"
    );
}

#[test]
fn test_multiple_elements_with_attributes() {
    let source = r#"
<button class="btn" onclick="handleClick()">Click</button>
<input type="text" id="name" placeholder="Enter name">
"#;

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have multiple Element nodes (button and input)
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 2,
        "Expected at least 2 Element nodes (button, input), got {element_count}"
    );

    // Should have multiple Attribute nodes
    let attribute_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        attribute_count >= 4,
        "Expected at least 4 Attribute nodes, got {attribute_count}"
    );
}

#[test]
fn test_complex_html_structure() {
    let source = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>Test Page</title>
</head>
<body>
    <div class="container">
        <h1 id="title">Welcome</h1>
        <p class="text">Content</p>
    </div>
</body>
</html>
"#;

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have multiple Element nodes
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 7,
        "Expected at least 7 Element nodes (html, head, meta, title, body, div, h1, p), got {element_count}"
    );

    // Should have Attribute nodes
    let attribute_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        attribute_count >= 4,
        "Expected at least 4 Attribute nodes, got {attribute_count}"
    );

    // Should have Contains edges
    let contains_count = count_contains_edges(&staging);
    assert!(contains_count > 0, "Expected Contains edges for hierarchy");
}

#[test]
fn test_element_without_attributes() {
    let source = r"<span>text content</span>";

    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();
    let file = PathBuf::from("test.html");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Should have Element node
    let element_count = count_nodes_by_kind(&staging, NodeKind::CallSite);
    assert!(
        element_count >= 1,
        "Expected at least 1 Element node, got {element_count}"
    );

    // May have 0 Attribute nodes if element has no attributes
    let attribute_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        attribute_count == 0,
        "Expected 0 Attribute nodes for element without attributes, got {attribute_count}"
    );
}
