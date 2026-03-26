//! Graph builder tests for the ServiceNow Xanadu language plugin.
//!
//! Covers:
//! - Function/method node extraction
//! - ES6 import and CommonJS require edges
//! - GlideRecord table access
//! - gs.* API call detection
//! - Script Include (Class.create) patterns
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_servicenow_xanadu::ServiceNowGraphBuilder;
use std::path::Path;

fn parse_servicenow(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("failed to set JavaScript language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse ServiceNow code")
}

fn count_edges_of_kind(staging: &StagingGraph, kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

fn count_call_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Calls { .. }))
}

fn count_import_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Imports { .. }))
}

fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::InternString { value, .. } = op {
            value.contains(pattern)
        } else {
            false
        }
    })
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty ServiceNow file should succeed");
}

#[test]
fn test_comments_only() {
    let source = r"
// This is a comment
/* Block comment */
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only file should succeed");
}

// ==================== Function Extraction ====================

#[test]
fn test_function_extraction() {
    let source = r"
function processTicket(ticketId) {
    var gr = new GlideRecord('incident');
    gr.get(ticketId);
    return gr.short_description;
}

function updateStatus(ticketId, status) {
    var gr = new GlideRecord('incident');
    gr.get(ticketId);
    gr.state = status;
    gr.update();
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("ticket.js"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 function node, got {}",
        stats.nodes_staged
    );
}

// ==================== Import Edge Detection ====================

#[test]
fn test_es6_imports() {
    let source = r"
import { Helper } from './helpers';
import Utils from '../utils';

function doWork() {
    return Helper.process();
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("work.js"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for ES6 imports, got {}",
        import_count
    );
}

#[test]
fn test_commonjs_require() {
    let source = r"
var helper = require('./helper_script');
var constants = require('sn_constants');

function execute() {
    return helper.run();
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("execute.js"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for require(), got {}",
        import_count
    );
    assert!(
        has_interned_string_containing(&staging, "helper_script")
            || has_interned_string_containing(&staging, "sn_constants"),
        "Expected import module names in staging"
    );
}

// ==================== GlideRecord Pattern ====================

#[test]
fn test_glide_record_access() {
    let source = r"
function getIncidents() {
    var gr = new GlideRecord('incident');
    gr.addQuery('state', 1);
    gr.query();
    while (gr.next()) {
        gs.log(gr.number + ': ' + gr.short_description);
    }
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("incidents.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "GlideRecord access should succeed");
}

#[test]
fn test_glide_record_update() {
    let source = r"
function resolveIncident(sysId) {
    var gr = new GlideRecord('incident');
    if (gr.get(sysId)) {
        gr.state = 6;
        gr.resolved_at = new GlideDateTime();
        gr.update();
    }
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("resolve.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "GlideRecord update should succeed");
}

// ==================== gs.* API Calls ====================

#[test]
fn test_gs_api_calls() {
    let source = r"
function logActivity(message) {
    gs.log('Activity: ' + message, 'MyScript');
    gs.info('Info message');
    gs.warn('Warning message');
    gs.error('Error occurred: ' + message);
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("logging.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "gs.* API calls should succeed");
}

// ==================== Script Include Pattern ====================

#[test]
fn test_script_include_class_create() {
    let source = r"
var MyScriptInclude = Class.create();
MyScriptInclude.prototype = Object.extendsObject(AbstractAjaxProcessor, {
    getIncidentCount: function() {
        var gr = new GlideRecord('incident');
        gr.query();
        return gr.getRowCount();
    },

    type: 'MyScriptInclude'
});
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("script_include.js"),
        &mut staging,
    );
    assert!(result.is_ok(), "Script Include pattern should succeed");
}

// ==================== Call Edge Detection ====================

#[test]
fn test_function_call_detection() {
    let source = r"
function helper() {
    return 42;
}

function main() {
    var result = helper();
    gs.log('result: ' + result);
}
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("main.js"), &mut staging)
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge, got {}",
        call_count
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = ServiceNowGraphBuilder::new();
    assert_eq!(builder.language(), Language::ServiceNow);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ServiceNowGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_javascript() {
    // Incomplete JS - tree-sitter is error-tolerant
    let source = r"
function broken(
"; // incomplete
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.js"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_combined_servicenow_patterns() {
    let source = r"
import { BaseHelper } from './base';

var MyProcessor = Class.create();
MyProcessor.prototype = {
    processRequest: function() {
        var taskGR = new GlideRecord('sc_task');
        taskGR.addQuery('state', 'open');
        taskGR.query();

        while (taskGR.next()) {
            gs.info('Processing task: ' + taskGR.number);
            this.updateTask(taskGR);
        }
    },

    updateTask: function(gr) {
        gr.state = 'in_progress';
        gr.update();
    },

    type: 'MyProcessor'
};
";
    let tree = parse_servicenow(source);
    let mut staging = StagingGraph::new();
    let builder = ServiceNowGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("processor.js"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Combined ServiceNow patterns should succeed"
    );
}
