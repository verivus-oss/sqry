use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_haskell::relations::HaskellGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_haskell(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_haskell::LANGUAGE.into())
        .expect("Failed to set Haskell language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Haskell code")
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
fn test_function_visibility_exported() {
    let source = r"
module MyModule (exportedFunction) where

exportedFunction :: Int -> Int
exportedFunction x = x + 1

privateFunction :: Int -> Int
privateFunction x = x * 2
";
    let tree = parse_haskell(source);
    let mut staging = StagingGraph::new();
    let builder = HaskellGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.hs"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "exportedFunction");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Exported function should have public visibility"
    );
}

#[test]
fn test_function_visibility_not_exported() {
    let source = r"
module MyModule (exportedFunction) where

exportedFunction :: Int -> Int
exportedFunction x = x + 1

privateFunction :: Int -> Int
privateFunction x = x * 2
";
    let tree = parse_haskell(source);
    let mut staging = StagingGraph::new();
    let builder = HaskellGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.hs"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "privateFunction");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Non-exported function should have private visibility"
    );
}

#[test]
fn test_function_visibility_no_export_list() {
    let source = r"
module MyModule where

publicByDefault :: Int -> Int
publicByDefault x = x + 1
";
    let tree = parse_haskell(source);
    let mut staging = StagingGraph::new();
    let builder = HaskellGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.hs"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "publicByDefault");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Functions in module without export list should be public"
    );
}

#[test]
fn test_function_visibility_mixed() {
    let source = r"
module MyModule (publicApi, helper) where

publicApi :: Int -> Int
publicApi x = helper x + 1

helper :: Int -> Int
helper x = x * 2

privateHelper :: Int -> Int
privateHelper x = x - 1
";
    let tree = parse_haskell(source);
    let mut staging = StagingGraph::new();
    let builder = HaskellGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.hs"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(
        find_function_visibility(&staging, "publicApi"),
        Some("public".to_string()),
        "publicApi should be public (exported)"
    );
    assert_eq!(
        find_function_visibility(&staging, "helper"),
        Some("public".to_string()),
        "helper should be public (exported)"
    );
    assert_eq!(
        find_function_visibility(&staging, "privateHelper"),
        Some("private".to_string()),
        "privateHelper should be private (not exported)"
    );
}
