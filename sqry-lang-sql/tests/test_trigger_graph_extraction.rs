//! Unit tests for trigger extraction (graph-native).
//!
//! Verifies that CREATE TRIGGER statements produce trigger nodes and
//! `TriggeredBy` edges in the staging graph.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sql::SqlPlugin;
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

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    name: &str,
    kind: NodeKind,
) -> Option<&'a NodeEntry> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return Some(entry);
            }
        }
    }
    None
}

fn build_graph(source: &[u8]) -> StagingGraph {
    let plugin = SqlPlugin::default();
    let file = PathBuf::from("test.sql");
    let tree = plugin.parse_ast(source).expect("parse failed");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
fn test_trigger_graph_extraction() {
    let sql = br"
        CREATE TRIGGER audit_log_trigger
        AFTER INSERT ON users
        FOR EACH ROW
        EXECUTE FUNCTION log_user_changes();
    ";

    let staging = build_graph(sql);
    let nodes = build_node_lookup(&staging);

    assert!(
        find_node_entry(&staging, "audit_log_trigger", NodeKind::Function).is_some(),
        "Expected trigger node 'audit_log_trigger'"
    );

    let mut matched_edge = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && matches!(kind, EdgeKind::TriggeredBy { .. })
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            let source_kind = nodes.get(source).map(|(_, kind)| *kind);
            let target_kind = nodes.get(target).map(|(_, kind)| *kind);

            if source_name == Some("audit_log_trigger")
                && target_name == Some("users")
                && source_kind == Some(NodeKind::Function)
                && target_kind == Some(NodeKind::Variable)
            {
                matched_edge = true;
                break;
            }
        }
    }

    assert!(
        matched_edge,
        "Expected TriggeredBy edge from audit_log_trigger to users"
    );
}

#[test]
fn test_multiple_trigger_extraction() {
    let sql = br"
        CREATE TRIGGER before_update_trigger
        BEFORE UPDATE ON accounts
        FOR EACH ROW
        EXECUTE FUNCTION validate_balance();

        CREATE TRIGGER after_delete_trigger
        AFTER DELETE ON orders
        FOR EACH ROW
        EXECUTE FUNCTION archive_order();
    ";

    let staging = build_graph(sql);

    assert!(
        find_node_entry(&staging, "before_update_trigger", NodeKind::Function).is_some(),
        "Expected before_update_trigger trigger node"
    );
    assert!(
        find_node_entry(&staging, "after_delete_trigger", NodeKind::Function).is_some(),
        "Expected after_delete_trigger trigger node"
    );
}

#[test]
fn test_function_and_trigger_extraction_together() {
    let sql = br"
        CREATE FUNCTION calculate_total(order_id INT)
        RETURNS DECIMAL(10,2)
        AS $$
            SELECT SUM(amount) FROM order_items WHERE order_id = $1;
        $$ LANGUAGE sql;

        CREATE TRIGGER update_total_trigger
        AFTER INSERT ON order_items
        FOR EACH ROW
        EXECUTE FUNCTION calculate_total();
    ";

    let staging = build_graph(sql);

    assert!(
        find_node_entry(&staging, "calculate_total", NodeKind::Function).is_some(),
        "Expected function node 'calculate_total'"
    );
    assert!(
        find_node_entry(&staging, "update_total_trigger", NodeKind::Function).is_some(),
        "Expected trigger node 'update_total_trigger'"
    );
}
