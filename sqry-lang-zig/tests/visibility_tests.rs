//! Visibility tests for Zig language plugin
//!
//! Zig visibility rules:
//! - `pub` keyword = public
//! - No `pub` keyword = private

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, node::NodeKind},
};
use sqry_lang_zig::ZigGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_zig(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .expect("Failed to set Zig language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Zig code")
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_public_function() {
    let source = r"
pub fn publicFunction(x: i32) i32 {
    return x * 2;
}
";

    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "publicFunction");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "pub function should be public"
    );
}

#[test]
fn test_private_function() {
    let source = r"
fn privateFunction(x: i32) i32 {
    return x + 1;
}
";

    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "privateFunction");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "non-pub function should be private"
    );
}

#[test]
fn test_mixed_visibility() {
    let source = r"
pub fn add(a: i32, b: i32) i32 {
    return helper(a) + helper(b);
}

fn helper(x: i32) i32 {
    return x * 2;
}
";

    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let add_visibility = find_function_visibility(&staging, "add");
    assert_eq!(
        add_visibility,
        Some("public".to_string()),
        "pub function add should be public"
    );

    let helper_visibility = find_function_visibility(&staging, "helper");
    assert_eq!(
        helper_visibility,
        Some("private".to_string()),
        "non-pub function helper should be private"
    );
}

#[test]
fn test_struct_with_pub_method() {
    let source = r"
const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }

    fn internal_helper(val: f32) f32 {
        return val * 2.0;
    }
};
";

    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let distance_visibility = find_function_visibility(&staging, "distance");
    assert_eq!(
        distance_visibility,
        Some("public".to_string()),
        "pub method should be public"
    );

    let helper_visibility = find_function_visibility(&staging, "internal_helper");
    assert_eq!(
        helper_visibility,
        Some("private".to_string()),
        "non-pub method should be private"
    );
}
