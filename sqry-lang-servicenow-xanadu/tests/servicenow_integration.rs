//! Integration tests for `ServiceNow` Xanadu plugin (graph-native).
//!
//! Validates:
//! - Script Include class nodes from Class.create
//! - Function and method nodes
//! - Variable/function dual emit for var function expressions
//! - `GlideRecord` table read edges from callers
//! - gs.* API call nodes

use sqry_core::graph::Language;
use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn build_node_canonical_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
            && let Some(name) = staging.resolve_node_canonical_name(entry)
        {
            nodes.insert(*node_id, (name.to_owned(), entry.kind));
        }
    }
    nodes
}

fn build_node_display_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
            && let Some(name) = staging.resolve_node_display_name(Language::ServiceNow, entry)
        {
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
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = staging.resolve_node_canonical_name(entry);
            if node_name.is_some_and(|n| n == name) {
                return Some(entry);
            }
        }
    }
    None
}

fn has_node_display_name(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
            && staging
                .resolve_node_display_name(Language::ServiceNow, entry)
                .as_deref()
                == Some(name)
        {
            return true;
        }
    }
    false
}

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = ServiceNowXanaduPlugin::new();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.snjs");
    fs::write(&file, source).expect("write test source");
    let tree = plugin.parse_ast(source).expect("parse source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
fn test_extract_script_include_class() {
    let content = br"
var MyScriptInclude = Class.create();
MyScriptInclude.prototype = {
    initialize: function() {
        this.name = 'MyScriptInclude';
    },

    execute: function() {
        gs.info('Executing MyScriptInclude');
    }
};
";

    let staging = build_graph_from_source(content);

    assert!(
        find_node_entry(&staging, "MyScriptInclude", NodeKind::Class).is_some(),
        "Expected MyScriptInclude class node"
    );
}

#[test]
fn test_extract_gliderecord_table_read_edge() {
    let content = br"
var gr = new GlideRecord('incident');
gr.query();
";

    let staging = build_graph_from_source(content);
    let nodes = build_node_canonical_lookup(&staging);

    let mut found = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::TableRead { .. },
            ..
        } = op
        {
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if target_name == Some("servicenow_table:incident") {
                found = true;
                break;
            }
        }
    }

    assert!(
        found,
        "Expected TableRead edge to servicenow_table:incident"
    );
}

#[test]
fn test_extract_functions() {
    let content = br"
function handleRequest(request) {
    var response = processRequest(request);
    return response;
}

function processRequest(req) {
    return {status: 'ok'};
}
";

    let staging = build_graph_from_source(content);

    assert!(
        find_node_entry(&staging, "handleRequest", NodeKind::Function).is_some(),
        "handleRequest function not found"
    );
    assert!(
        find_node_entry(&staging, "processRequest", NodeKind::Function).is_some(),
        "processRequest function not found"
    );
}

#[test]
#[ignore = "IIFE named function expressions are not yet captured by graph builder"]
fn test_extract_business_rule_pattern() {
    let content = br"
(function executeRule(current, previous) {
    if (current.priority == '1') {
        gs.info('High priority incident');
    }
})(current, previous);
";

    let staging = build_graph_from_source(content);
    assert!(
        find_node_entry(&staging, "executeRule", NodeKind::Function).is_some(),
        "executeRule function should be present"
    );
}

#[test]
fn test_extract_es6_classes_and_methods() {
    let content = br"
class IncidentHandler {
    constructor() {
        this.priority = 1;
    }

    handleIncident(incident) {
        gs.info('Handling incident: ' + incident.number);
    }

    escalate() {
        // escalation logic
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        find_node_entry(&staging, "IncidentHandler", NodeKind::Class).is_some(),
        "IncidentHandler class not found"
    );
    assert!(
        find_node_entry(
            &staging,
            "IncidentHandler::handleIncident",
            NodeKind::Method
        )
        .is_some(),
        "handleIncident method not found"
    );
    assert!(
        find_node_entry(&staging, "IncidentHandler::escalate", NodeKind::Method).is_some(),
        "escalate method not found"
    );
    assert!(
        has_node_display_name(&staging, "IncidentHandler.handleIncident", NodeKind::Method),
        "handleIncident method native display name not found"
    );
    assert!(
        has_node_display_name(&staging, "IncidentHandler.escalate", NodeKind::Method),
        "escalate method native display name not found"
    );
}

#[test]
fn test_var_function_dual_emit() {
    let content = br"
var myFunc = function(arg) {
  gs.info('hello');
};
";

    let staging = build_graph_from_source(content);

    assert!(
        find_node_entry(&staging, "myFunc", NodeKind::Variable).is_some(),
        "Variable node expected for var function expression"
    );
    assert!(
        find_node_entry(&staging, "myFunc", NodeKind::Function).is_some(),
        "Function node expected for var function expression"
    );
}

#[test]
fn test_gliderecord_table_read_edges_from_callers() {
    let content = br"
function f1() {
  var gr = new GlideRecord('incident');
  gr.query();
}

class TicketHandler {
  handle() {
    var gr2 = new GlideRecord('task');
    gr2.query();
  }
}
";

    let staging = build_graph_from_source(content);
    let nodes = build_node_canonical_lookup(&staging);
    let display_nodes = build_node_display_lookup(&staging);

    let mut f1_edge = false;
    let mut handle_edge = false;
    let mut handle_display_edge = false;

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::TableRead { .. },
            ..
        } = op
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            let source_display_name = display_nodes.get(source).map(|(name, _)| name.as_str());
            if source_name == Some("f1") && target_name == Some("servicenow_table:incident") {
                f1_edge = true;
            }
            if source_name == Some("TicketHandler::handle")
                && target_name == Some("servicenow_table:task")
            {
                handle_edge = true;
            }
            if source_display_name == Some("TicketHandler.handle")
                && target_name == Some("servicenow_table:task")
            {
                handle_display_edge = true;
            }
        }
    }

    assert!(f1_edge, "Expected f1 to read incident table");
    assert!(
        handle_edge,
        "Expected TicketHandler.handle to read task table"
    );
    assert!(
        handle_display_edge,
        "Expected TicketHandler.handle native display name on table-read edge"
    );
}

#[test]
fn test_gliderecord_table_read_edges_extended() {
    let content = br"
function processIncidents() {
    var gr = new GlideRecord('incident');
    gr.addQuery('active', true);
    gr.query();

    while (gr.next()) {
        gs.info('Processing: ' + gr.number);
    }
}

class IncidentManager {
    queryHighPriority() {
        var gr = new GlideRecord('incident');
        gr.addQuery('priority', 1);
        gr.query();
    }
}
";

    let staging = build_graph_from_source(content);
    let nodes = build_node_canonical_lookup(&staging);
    let display_nodes = build_node_display_lookup(&staging);

    let mut process_edge = false;
    let mut method_edge = false;
    let mut method_display_edge = false;

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::TableRead { .. },
            ..
        } = op
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            let source_display_name = display_nodes.get(source).map(|(name, _)| name.as_str());
            if source_name == Some("processIncidents")
                && target_name == Some("servicenow_table:incident")
            {
                process_edge = true;
            }
            if source_name == Some("IncidentManager::queryHighPriority")
                && target_name == Some("servicenow_table:incident")
            {
                method_edge = true;
            }
            if source_display_name == Some("IncidentManager.queryHighPriority")
                && target_name == Some("servicenow_table:incident")
            {
                method_display_edge = true;
            }
        }
    }

    assert!(
        process_edge,
        "Expected processIncidents to read incident table"
    );
    assert!(
        method_edge,
        "Expected IncidentManager.queryHighPriority to read incident table"
    );
    assert!(
        method_display_edge,
        "Expected IncidentManager.queryHighPriority native display name on table-read edge"
    );
}

#[test]
fn test_extracts_gs_api() {
    let content = br"
function doLog() {
    gs.info('Hello');
}
";

    let staging = build_graph_from_source(content);

    assert!(
        find_node_entry(&staging, "gs::info", NodeKind::Function).is_some(),
        "gs.info API node not found"
    );
    assert!(
        has_node_display_name(&staging, "gs.info", NodeKind::Function),
        "gs.info API native display name not found"
    );
}
