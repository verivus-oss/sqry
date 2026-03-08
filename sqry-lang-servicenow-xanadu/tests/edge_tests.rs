//! Tests for ServiceNow Xanadu edge creation (calls, imports, exports, inherits).

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin;
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

fn has_call_edge(staging: &StagingGraph, from: &str, to: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Calls { .. },
            ..
        } = op
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(from) && target_name == Some(to) {
                return true;
            }
        }
    }
    false
}

fn has_import_edge(staging: &StagingGraph, from_module: &str, imported_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Imports { .. },
            ..
        } = op
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(from_module) && target_name == Some(imported_name) {
                return true;
            }
        }
    }
    false
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

fn has_inherits_edge(staging: &StagingGraph, child: &str, parent: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Inherits,
            ..
        } = op
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(child) && target_name == Some(parent) {
                return true;
            }
        }
    }
    false
}

// ===== Call Edge Tests =====

#[test]
fn test_function_call_edge() {
    let content = br"
function caller() {
    callee();
}

function callee() {
    return 42;
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_call_edge(&staging, "caller", "callee"),
        "Expected call edge from caller to callee"
    );
}

#[test]
fn test_method_call_edge() {
    let content = br"
class MyClass {
    caller() {
        this.callee();
    }

    callee() {
        return 42;
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_call_edge(&staging, "MyClass.caller", "MyClass.callee"),
        "Expected call edge from caller method to callee method"
    );
}

#[test]
fn test_nested_function_call_edge() {
    let content = br"
function outer() {
    middle();
}

function middle() {
    inner();
}

function inner() {
    return 42;
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_call_edge(&staging, "outer", "middle"),
        "Expected call edge from outer to middle"
    );
    assert!(
        has_call_edge(&staging, "middle", "inner"),
        "Expected call edge from middle to inner"
    );
}

// ===== Import Edge Tests =====

#[test]
fn test_es6_import_edge() {
    let content = br"
import { processData } from './utils';

function handleRequest() {
    processData();
}
";

    let staging = build_graph_from_source(content);

    // Edge target is module source, not binding name
    assert!(
        has_import_edge(&staging, "test", "./utils"),
        "Expected import edge for ES6 import with module source target"
    );
}

#[test]
fn test_require_import_edge() {
    let content = br"
var utils = require('./utils');

function handleRequest() {
    utils.processData();
}
";

    let staging = build_graph_from_source(content);

    // Edge target is module source
    assert!(
        has_import_edge(&staging, "test", "./utils"),
        "Expected import edge for require() with module source target"
    );
}

// ===== Export Edge Tests =====

#[test]
fn test_top_level_function_export() {
    let content = br"
function processRequest(request) {
    return {status: 'ok'};
}

function handleError(error) {
    gs.error('Error: ' + error);
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "processRequest"),
        "Expected export edge for top-level function processRequest"
    );
    assert!(
        has_export_edge(&staging, "handleError"),
        "Expected export edge for top-level function handleError"
    );
}

#[test]
fn test_top_level_class_export() {
    let content = br"
class IncidentProcessor {
    process(incident) {
        return incident;
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "IncidentProcessor"),
        "Expected export edge for top-level class IncidentProcessor"
    );
}

#[test]
fn test_class_create_export() {
    let content = br"
var MyScriptInclude = Class.create();
MyScriptInclude.prototype = {
    execute: function() {
        return 42;
    }
};
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "MyScriptInclude"),
        "Expected export edge for Class.create() class"
    );
}

#[test]
#[ignore = "ES6/CommonJS export handling not implemented - ServiceNow uses Script Includes"]
fn test_module_exports_edge() {
    let content = br"
function processData() {
    return 42;
}

module.exports = processData;
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "processData"),
        "Expected export edge for module.exports"
    );
}

#[test]
#[ignore = "Export handling not yet implemented"]
fn test_exports_property_edge() {
    let content = br"
exports.processData = function() {
    return 42;
};
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "processData"),
        "Expected export edge for exports.property"
    );
}

// ===== Inherits Edge Tests =====

#[test]
fn test_class_extends_edge() {
    let content = br"
class BaseHandler {
    handle() {
        return 'base';
    }
}

class IncidentHandler extends BaseHandler {
    handle() {
        return 'incident';
    }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_inherits_edge(&staging, "IncidentHandler", "BaseHandler"),
        "Expected inherits edge from IncidentHandler to BaseHandler"
    );
}

#[test]
fn test_class_create_inheritance() {
    let content = br"
var BaseClass = Class.create();
BaseClass.prototype = {
    baseMethod: function() {
        return 'base';
    }
};

var DerivedClass = Class.create(BaseClass);
DerivedClass.prototype = {
    derivedMethod: function() {
        return 'derived';
    }
};
";

    let staging = build_graph_from_source(content);

    assert!(
        has_inherits_edge(&staging, "DerivedClass", "BaseClass"),
        "Expected inherits edge from DerivedClass to BaseClass"
    );
}
