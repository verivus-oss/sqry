//! Graph builder integration tests for the Perl plugin.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_perl::PerlPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = staging
                .resolve_node_canonical_name(entry)
                .map(str::to_owned)
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_staging_from_fixture(name: &str) -> StagingGraph {
    let plugin = PerlPlugin::default();
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

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn has_import_edge(staging: &StagingGraph, import_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Imports { .. },
            ..
        } = op
            && let Some((target_name, NodeKind::Import)) = nodes.get(target)
            && target_name == import_name
        {
            return true;
        }
    }
    false
}

#[allow(clippy::similar_names)] // Test graph variables
fn has_call_edge(staging: &StagingGraph, caller: &str, callee: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            if !matches!(kind, EdgeKind::Calls { .. }) {
                continue;
            }
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(caller) && target_name == Some(callee) {
                return true;
            }
        }
    }
    false
}

#[test]
fn test_function_nodes_from_fixture() {
    let staging = build_staging_from_fixture("test_graph.pl");

    assert!(find_node(
        &staging,
        "MyApp::Utils::helper",
        NodeKind::Function
    ));
    assert!(find_node(
        &staging,
        "MyApp::Utils::calculate",
        NodeKind::Function
    ));
    assert!(find_node(
        &staging,
        "MyApp::Service::process",
        NodeKind::Function
    ));
    assert!(find_node(
        &staging,
        "MyApp::Service::validate",
        NodeKind::Function
    ));
    assert!(find_node(&staging, "main::run", NodeKind::Function));
    assert!(find_node(&staging, "main::startup", NodeKind::Function));
}

#[test]
fn test_import_edges_from_fixture() {
    let staging = build_staging_from_fixture("basic.pl");

    assert!(has_import_edge(&staging, "strict"));
    assert!(has_import_edge(&staging, "warnings"));
    assert!(has_import_edge(&staging, "Moose"));
    assert!(has_import_edge(&staging, "List::Util"));
    assert!(has_import_edge(&staging, "Carp"));
}

#[test]
fn test_call_edges_from_fixture() {
    let staging = build_staging_from_fixture("test_graph.pl");

    assert!(has_call_edge(&staging, "MyApp::Utils::calculate", "helper"));
    assert!(has_call_edge(
        &staging,
        "MyApp::Service::process",
        "MyApp::Utils::helper"
    ));
}
