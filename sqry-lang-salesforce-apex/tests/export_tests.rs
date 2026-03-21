//! Tests for Salesforce Apex export edge creation.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_salesforce_apex::SalesforceApexPlugin;
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
    let plugin = SalesforceApexPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.cls");
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
fn test_class_exported() {
    let content = b"\
public class AccountService {
    public static List<Account> getAccounts() {
        return [SELECT Id, Name FROM Account];
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "AccountService"),
        "Expected export edge for class AccountService"
    );
}

#[test]
fn test_methods_exported() {
    let content = b"\
public class ContactHandler {
    public void handleInsert(List<Contact> contacts) {
        insert contacts;
    }

    public void handleUpdate(List<Contact> contacts) {
        update contacts;
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "ContactHandler"),
        "Expected export edge for class ContactHandler"
    );
    assert!(
        has_export_edge(&staging, "handleInsert"),
        "Expected export edge for method handleInsert"
    );
    assert!(
        has_export_edge(&staging, "handleUpdate"),
        "Expected export edge for method handleUpdate"
    );
}

#[test]
fn test_trigger_exported() {
    let content = b"\
trigger AccountTrigger on Account (before insert, after update) {
    if (Trigger.isBefore && Trigger.isInsert) {
        // Handle before insert
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "AccountTrigger"),
        "Expected export edge for trigger AccountTrigger"
    );
}

#[test]
fn test_mixed_callables() {
    let content = b"\
public class OpportunityService {
    public static List<Opportunity> getOpportunities(Id accountId) {
        return [SELECT Id, Name, Amount FROM Opportunity WHERE AccountId = :accountId];
    }
}

trigger OpportunityTrigger on Opportunity (before insert) {
    // Trigger logic
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "OpportunityService"),
        "Expected export edge for class OpportunityService"
    );
    assert!(
        has_export_edge(&staging, "getOpportunities"),
        "Expected export edge for method getOpportunities"
    );
    assert!(
        has_export_edge(&staging, "OpportunityTrigger"),
        "Expected export edge for trigger OpportunityTrigger"
    );
}
