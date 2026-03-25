//! Tests for Swift extension node creation.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{StagingGraph, StringId};
use sqry_lang_swift::SwiftGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

fn parse_swift(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    parser.parse(source.as_bytes(), None).unwrap()
}

/// Build a map from StringId to string value from staging operations
fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((*local_id, value.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn has_extension_node(staging: &StagingGraph, extended_type: &str) -> bool {
    let string_map = build_string_map(staging);
    let extension_name = format!("extension {extended_type}");

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Module
            && let Some(node_name) = string_map.get(&entry.name)
            && node_name == &extension_name
        {
            return true;
        }
    }
    false
}

#[test]
fn test_extension_creates_node() {
    let source = r#"
extension String {
    func reversed() -> String {
        return String(self.reversed())
    }
}
"#;

    let tree = parse_swift(source);
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::default();
    let file = Path::new("test.swift");

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_extension_node(&staging, "String"),
        "Extension should create a Module node with name 'extension String'"
    );
}

#[test]
fn test_multiple_extensions() {
    let source = r#"
extension Array {
    func sum() -> Int {
        return 0
    }
}

extension Dictionary {
    func merge() {
        // merge logic
    }
}
"#;

    let tree = parse_swift(source);
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::default();
    let file = Path::new("test.swift");

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_extension_node(&staging, "Array"),
        "Extension should create a Module node for Array"
    );

    assert!(
        has_extension_node(&staging, "Dictionary"),
        "Extension should create a Module node for Dictionary"
    );
}

#[test]
fn test_protocol_extension() {
    let source = r#"
protocol DataProcessor {
    func process()
}

extension DataProcessor {
    func validate() {
        // default implementation
    }
}
"#;

    let tree = parse_swift(source);
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::default();
    let file = Path::new("test.swift");

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_extension_node(&staging, "DataProcessor"),
        "Protocol extension should create a Module node"
    );
}

#[test]
fn test_extension_with_where_clause() {
    let source = r#"
extension Array where Element: Equatable {
    func removeDuplicates() -> [Element] {
        return []
    }
}
"#;

    let tree = parse_swift(source);
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::default();
    let file = Path::new("test.swift");

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_extension_node(&staging, "Array"),
        "Constrained extension should create a Module node"
    );
}
