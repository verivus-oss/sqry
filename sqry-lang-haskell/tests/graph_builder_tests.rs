//! Graph builder integration tests for the Haskell plugin.

use sqry_core::graph::Language;
use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_haskell::HaskellPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
            && let Some(name) = staging.resolve_node_display_name(Language::Haskell, entry)
        {
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_staging_from_fixture(name: &str) -> StagingGraph {
    let plugin = HaskellPlugin::default();
    let path = PathBuf::from(format!("tests/fixtures/{name}"));
    let content = std::fs::read(&path).expect("read fixture");
    let (prepared_content, tree) = plugin.prepare_ast(&content).expect("parse fixture");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, prepared_content.as_ref(), &path, &mut staging)
        .expect("build graph");
    staging
}

fn has_function_node(staging: &StagingGraph, name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == NodeKind::Function)
}

fn has_import_edge(staging: &StagingGraph, import_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge { target, kind, .. } = op {
            if !matches!(kind, EdgeKind::Imports { .. }) {
                continue;
            }
            if let Some((target_name, NodeKind::Import)) = nodes.get(target)
                && target_name == import_name
            {
                return true;
            }
        }
    }
    false
}

fn has_import_edge_with_wildcard(
    staging: &StagingGraph,
    import_name: &str,
    expected_wildcard: bool,
) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge { target, kind, .. } = op {
            let EdgeKind::Imports { is_wildcard, .. } = kind else {
                continue;
            };
            if let Some((target_name, NodeKind::Import)) = nodes.get(target)
                && target_name == import_name
                && *is_wildcard == expected_wildcard
            {
                return true;
            }
        }
    }
    false
}

#[test]
fn test_function_nodes_from_basic_fixture() {
    let staging = build_staging_from_fixture("basic.hs");

    assert!(has_function_node(&staging, "Sample.foo"));
    assert!(has_function_node(&staging, "Sample.bar"));
}

#[test]
fn test_import_edges_from_basic_fixture() {
    let staging = build_staging_from_fixture("basic.hs");

    assert!(has_import_edge(&staging, "qualified:Data.List"));
    assert!(has_import_edge(&staging, "Data.List"));
    assert!(has_import_edge(&staging, "Control.Monad"));
}

#[test]
fn test_literate_fixture_nodes() {
    let staging = build_staging_from_fixture("literate.lhs");

    assert!(has_function_node(&staging, "Literate.answer"));
}

#[test]
fn test_import_hiding_wildcard_false() {
    let staging = build_staging_from_fixture("imports_hiding.hs");

    assert!(has_import_edge(&staging, "Data.Maybe"));
    assert!(has_import_edge_with_wildcard(&staging, "Data.Maybe", false));
}
