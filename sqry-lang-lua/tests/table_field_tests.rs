//! Tests for Lua table constructor and field access tracking
//!
//! Verifies that table fields and field accesses are correctly tracked as Property nodes.

use std::path::Path;

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Span};
use sqry_lang_lua::relations::LuaGraphBuilder;
use tree_sitter::Parser;

/// Helper to parse Lua code
fn parse_lua(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .expect("Failed to set Lua language");
    let content = source.as_bytes().to_vec();
    let tree = parser
        .parse(&content, None)
        .expect("Failed to parse Lua code");
    (tree, content)
}

use std::collections::HashMap;

/// Build string lookup from staging operations
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

/// Helper to extract Property nodes from staging operations
fn extract_property_nodes(staging: &StagingGraph) -> Vec<(String, Option<Span>)> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Property)
            {
                let name = strings
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                let span = Span::from_bytes(entry.start_byte as usize, entry.end_byte as usize);
                return Some((name, Some(span)));
            }
            None
        })
        .collect()
}

#[test]
fn test_table_constructor_fields() {
    let source = r#"
        local config = {
            host = "localhost",
            port = 8080,
            debug = true
        }
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    // Should track all three fields
    assert!(
        properties.len() >= 3,
        "Expected at least 3 property nodes for table fields"
    );

    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    assert!(
        field_names.contains(&"host".to_string()),
        "Should track 'host' field"
    );
    assert!(
        field_names.contains(&"port".to_string()),
        "Should track 'port' field"
    );
    assert!(
        field_names.contains(&"debug".to_string()),
        "Should track 'debug' field"
    );
}

#[test]
fn test_nested_table_constructors() {
    let source = r#"
        local config = {
            database = {
                host = "localhost",
                port = 5432
            },
            server = {
                host = "0.0.0.0",
                port = 8080
            }
        }
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    // Should track all fields including nested ones
    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    assert!(
        field_names.contains(&"database".to_string()),
        "Should track 'database' field"
    );
    assert!(
        field_names.contains(&"server".to_string()),
        "Should track 'server' field"
    );
    assert!(
        field_names.contains(&"host".to_string()),
        "Should track nested 'host' fields"
    );
    assert!(
        field_names.contains(&"port".to_string()),
        "Should track nested 'port' fields"
    );
}

#[test]
fn test_dot_field_access() {
    let source = r#"
        local module = {}

        function module.init()
            local value = module.config
            module.state = value
        end
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    assert!(
        field_names.contains(&"config".to_string()),
        "Should track 'config' field access"
    );
    assert!(
        field_names.contains(&"state".to_string()),
        "Should track 'state' field access"
    );
}

#[test]
fn test_bracket_field_access_string() {
    let source = r#"
        local data = {}

        function process()
            data["field_name"] = 42
            local value = data["other_field"]
        end
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    assert!(
        field_names.contains(&"field_name".to_string()),
        "Should track bracket notation string field"
    );
    assert!(
        field_names.contains(&"other_field".to_string()),
        "Should track bracket notation string field"
    );
}

#[test]
fn test_mixed_field_access_patterns() {
    let source = r#"
        local obj = {
            x = 10,
            y = 20
        }

        function update()
            obj.x = obj.x + 1
            obj["y"] = obj["y"] + 1
            local z = obj.z
        end
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    // Should track fields from both constructor and accesses
    assert!(
        field_names.contains(&"x".to_string()),
        "Should track 'x' field"
    );
    assert!(
        field_names.contains(&"y".to_string()),
        "Should track 'y' field"
    );
    assert!(
        field_names.contains(&"z".to_string()),
        "Should track 'z' field access"
    );
}

#[test]
fn test_table_as_module_pattern() {
    let source = r#"
        local M = {}

        M.version = "1.0"
        M.config = {
            enabled = true,
            timeout = 30
        }

        function M.initialize()
            M.status = "ready"
        end

        return M
    "#;

    let (tree, content) = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.lua"), &mut staging)
        .unwrap();

    let properties = extract_property_nodes(&staging);

    let field_names: Vec<String> = properties.iter().map(|(name, _)| name.clone()).collect();

    // Should track module fields
    assert!(
        field_names.contains(&"version".to_string()),
        "Should track 'version' field"
    );
    assert!(
        field_names.contains(&"config".to_string()),
        "Should track 'config' field"
    );
    assert!(
        field_names.contains(&"enabled".to_string()),
        "Should track nested 'enabled' field"
    );
    assert!(
        field_names.contains(&"timeout".to_string()),
        "Should track nested 'timeout' field"
    );
    assert!(
        field_names.contains(&"status".to_string()),
        "Should track 'status' field set in function"
    );
}
