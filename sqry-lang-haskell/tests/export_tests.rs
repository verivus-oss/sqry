//! Tests for Haskell module export edge creation.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_haskell::HaskellPlugin;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = HaskellPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.hs");
    fs::write(&file, source).expect("write test source");
    let tree = plugin.parse_ast(source).expect("parse source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

fn has_export_edge(staging: &StagingGraph, exported_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

// ===== Export Edge Tests =====

#[test]
fn test_no_export_list_exports_all() {
    let content = b"\
-- No module declaration = no export list, export all top-level
factorial :: Integer -> Integer
factorial 0 = 1
factorial n = n * factorial (n - 1)

double :: Integer -> Integer
double x = x * 2
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "factorial"),
        "Expected export edge for factorial (no export list)"
    );
    assert!(
        has_export_edge(&staging, "double"),
        "Expected export edge for double (no export list)"
    );
}

#[test]
fn test_explicit_export_list() {
    let content = b"\
module MyModule (factorial, isEven) where

factorial :: Integer -> Integer
factorial 0 = 1
factorial n = n * factorial (n - 1)

double :: Integer -> Integer
double x = x * 2

isEven :: Integer -> Bool
isEven n = n `mod` 2 == 0
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "MyModule.factorial"),
        "Expected export edge for factorial (in export list)"
    );
    assert!(
        has_export_edge(&staging, "MyModule.isEven"),
        "Expected export edge for isEven (in export list)"
    );
    assert!(
        !has_export_edge(&staging, "MyModule.double"),
        "Should NOT have export edge for double (not in export list)"
    );
}

#[test]
fn test_empty_export_list() {
    let content = b"\
module MyModule () where

factorial :: Integer -> Integer
factorial 0 = 1
factorial n = n * factorial (n - 1)

helper :: Integer -> Integer
helper x = x + 1
";

    let staging = build_graph_from_source(content);

    assert!(
        !has_export_edge(&staging, "factorial"),
        "Empty export list should not export factorial"
    );
    assert!(
        !has_export_edge(&staging, "helper"),
        "Empty export list should not export helper"
    );
}

#[test]
fn test_module_exports_with_types() {
    let content = b"\
module MyModule (MyType(..), processData) where

data MyType = Constructor1 | Constructor2

processData :: MyType -> String
processData Constructor1 = \"one\"
processData Constructor2 = \"two\"

internalHelper :: Int -> Int
internalHelper x = x * 2
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "MyModule.processData"),
        "Expected export edge for processData (in export list)"
    );
    assert!(
        !has_export_edge(&staging, "MyModule.internalHelper"),
        "Should NOT have export edge for internalHelper (not in export list)"
    );
}
