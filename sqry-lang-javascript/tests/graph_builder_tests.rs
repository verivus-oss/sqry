// JavaScript graph builder tests - migrated to StagingGraph API
#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::build::test_helpers::{
    assert_has_call_edge, assert_has_export_edge, assert_has_import_edge, assert_has_inherits_edge,
    count_operations,
};
use sqry_core::graph::unified::edge::{EdgeKind, HttpMethod};
use sqry_lang_javascript::JavaScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_javascript(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("Error loading JavaScript grammar");
    parser.parse(source, None).expect("Error parsing")
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

fn has_http_request_edge(staging: &StagingGraph, method: HttpMethod, url: Option<&str>) -> bool {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::HttpRequest {
                    method: edge_method,
                    url: edge_url,
                },
            ..
        } = op
        {
            if *edge_method != method {
                continue;
            }

            let url_value = edge_url.and_then(|id| strings.get(&id.index()).cloned());
            if url_value.as_deref() == url {
                return true;
            }
        }
    }
    false
}

#[test]
fn graph_builder_extracts_calls_correctly() {
    let source = r"
function helper() { return 1; }
function entry() { return helper(); }
function nested() {
    const lam = () => helper();
    return lam();
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_extracts_calls_correctly.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "entry", "helper");
}

#[test]
fn graph_builder_handles_method_calls() {
    let source = r"
class Widget {
    helper() { return 1; }
    process() { return this.helper(); }
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_method_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Widget.process", "Widget.helper");
}

#[test]
fn graph_builder_handles_arrow_functions() {
    let source = r"
function helper() { return 1; }
const process = () => { return helper(); };
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_arrow_functions.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "process", "helper");
}

#[test]
fn graph_builder_handles_async_calls() {
    let source = r"
async function fetchData() {
    return await fetch('/api/data');
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_async_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "fetchData", "fetch");
}

#[test]
fn graph_builder_detects_fetch_calls() {
    let source = r"
async function getData() {
    const response = await fetch('/api/users');
    return response.json();
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_detects_fetch_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "getData", "fetch");
    assert_has_call_edge(&staging, "getData", "response.json");
}

#[test]
fn graph_builder_detects_axios_calls() {
    let source = r"
async function getUsers() {
    return await axios.get('/api/users');
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_detects_axios_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "getUsers", "axios.get");
}

#[test]
fn graph_builder_handles_constructor_calls() {
    let source = r"
function createWidget() {
    return new Widget();
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_constructor_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "createWidget", "Widget");
}

#[test]
fn graph_builder_handles_empty_file() {
    let source = "";

    let tree = parse_javascript(source);
    let file = Path::new("empty.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "Empty file should build successfully");

    let counts = count_operations(&staging);
    assert_eq!(counts.edges, 0);
    assert_eq!(counts.nodes, 0);
}

#[test]
fn graph_builder_handles_comments_and_literals() {
    let source = r#"
// This is a comment
function test() {
    const str = "fetch('/api/data')";
    const num = 123;
    /* Multi-line comment
       with code-like text: helper()
    */
    return str;
}
"#;

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_comments_and_literals.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    // Verify no call edges are created from comments/strings.
    // Note: References edges may exist from local variable tracking (e.g., `return str;`),
    // which is correct behavior — we only assert no false call/import/export edges.
    let call_edge_count = staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::Calls { .. }))
        .count();
    assert_eq!(
        call_edge_count, 0,
        "Should not detect calls from comments/strings"
    );
    let counts = count_operations(&staging);
    assert!(counts.nodes > 0, "Should detect function definition");
}

#[test]
fn graph_builder_handles_nested_calls() {
    let source = r"
function a() { return b(); }
function b() { return c(); }
function c() { return 1; }
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_nested_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "a", "b");
    assert_has_call_edge(&staging, "b", "c");
}

#[test]
fn graph_builder_handles_top_level_calls() {
    let source = r"
import { bootstrap } from './app';

// Top-level call - should be captured with <module> context
bootstrap();

// IIFE - should be captured
(function() {
    initialize();
})();

// Top-level constructor
const app = new Application();
app.start();

// Named function for reference
function initialize() {
    console.log('init');
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_handles_top_level_calls.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_call_edge(&staging, module_name, "bootstrap");
    assert_has_call_edge(&staging, module_name, "Application");
    assert_has_call_edge(&staging, module_name, "app.start");
    assert_has_call_edge(&staging, "initialize", "console.log");
}

#[test]
fn graph_builder_validates_http_edges() {
    let source = r"
async function fetchUsers() {
    const response = await fetch('/api/users', {
        method: 'POST',
        body: JSON.stringify({name: 'test'})
    });
    return response.json();
}

async function axiosCall() {
    const result = await axios.get('/api/data');
    return result.data;
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_graph_builder_validates_http_edges.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "fetchUsers", "fetch");
    assert_has_call_edge(&staging, "axiosCall", "axios.get");
    assert!(
        has_http_request_edge(&staging, HttpMethod::Post, Some("/api/users")),
        "missing HttpRequest edge for POST /api/users"
    );
    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/data")),
        "missing HttpRequest edge for GET /api/data"
    );
}

#[test]
fn graph_builder_detects_es6_imports() {
    let source = r"
import React from 'react';
import { useState, useEffect } from 'react';
import * as Utils from './utils';
import sidebar from './components/sidebar';

function MyComponent() {
    return null;
}
";

    let tree = parse_javascript(source);
    let file = Path::new("src/app/main.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_import_edge(&staging, module_name, "react");
    assert_has_import_edge(&staging, module_name, "src/app/utils");
    assert_has_import_edge(&staging, module_name, "src/app/components/sidebar");
}

// ========== Export Tests ==========

#[test]
fn graph_builder_detects_default_export_function() {
    let source = r"
export default function myFunc() {
    return 42;
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_default_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "myFunc");
}

#[test]
fn graph_builder_detects_default_export_class() {
    let source = r"
export default class MyComponent {
    render() { return null; }
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_default_class_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "MyComponent");
}

// ... other export tests follow similar pattern ...

// ========== CommonJS Export Tests ==========

#[test]
fn graph_builder_detects_commonjs_object_exports() {
    let source = r"
function helper(x) {
    return x * 2;
}

function calculate(a, b) {
    return helper(a) + helper(b);
}

module.exports = {
    helper,
    calculate
};
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_object_exports.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "helper");
    assert_has_export_edge(&staging, module_name, "calculate");
}

#[test]
fn graph_builder_detects_commonjs_default_export() {
    let source = r"
function myFunction() {
    return 42;
}

module.exports = myFunction;
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_default_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "myFunction");
}

#[test]
fn graph_builder_detects_commonjs_property_exports() {
    let source = r"
function foo() {
    return 1;
}

function bar() {
    return 2;
}

exports.foo = foo;
exports.bar = bar;
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_property_exports.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "foo");
    assert_has_export_edge(&staging, module_name, "bar");
}

#[test]
fn graph_builder_detects_commonjs_module_exports_property() {
    let source = r"
function handler() {
    return 'handled';
}

module.exports.handler = handler;
module.exports.version = '1.0.0';
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_module_exports_property.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "handler");
    assert_has_export_edge(&staging, module_name, "version");
}

#[test]
fn graph_builder_detects_commonjs_anonymous_function_export() {
    let source = r"
module.exports = function() {
    return 42;
};
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_anonymous_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "default");
}

#[test]
fn graph_builder_detects_commonjs_class_export() {
    let source = r"
class MyClass {
    constructor() {
        this.value = 42;
    }

    getValue() {
        return this.value;
    }
}

module.exports = MyClass;
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_class_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "MyClass");
}

#[test]
fn graph_builder_detects_commonjs_arrow_function_export() {
    let source = r"
module.exports = () => {
    return 'arrow';
};
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_arrow_export.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    assert_has_export_edge(&staging, module_name, "default");
}

#[test]
fn graph_builder_detects_commonjs_object_with_pair() {
    let source = r"
function internalHelper() {
    return 'helper';
}

function publicAPI() {
    return 'api';
}

module.exports = {
    helper: internalHelper,
    api: publicAPI
};
";

    let tree = parse_javascript(source);
    let file = Path::new("test_commonjs_object_with_pair.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let module_name = "<module>";
    // The export names are the keys in the object literal
    assert_has_export_edge(&staging, module_name, "helper");
    assert_has_export_edge(&staging, module_name, "api");
}

// ========== OOP / Inheritance Tests ==========

#[test]
fn graph_builder_detects_class_inheritance() {
    let source = r"
class Parent {
    method() {}
}

class Child extends Parent {
    childMethod() {}
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_class_inheritance.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_inherits_edge(&staging, "Child", "Parent");
}

#[test]
fn graph_builder_detects_qualified_extends_path() {
    let source = r"
class Child extends Module.Parent {
    method() {}
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_qualified_extends.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_inherits_edge(&staging, "Child", "Module.Parent");
}

#[test]
fn graph_builder_handles_react_component_qualified_extends() {
    let source = r"
import React from 'react';

class MyComponent extends React.Component {
    render() {
        return null;
    }
}
";

    let tree = parse_javascript(source);
    let file = Path::new("test_react_component.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_inherits_edge(&staging, "MyComponent", "React.Component");
}

// ========== Express .all() Endpoint Tests ==========

use sqry_core::graph::unified::node::NodeKind;

/// Helper: check if staging has an Endpoint node whose resolved name contains the given substring.
fn has_endpoint_with_name(staging: &StagingGraph, name_substring: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Endpoint)
            && let Some(resolved) = staging.resolve_local_string(entry.name)
        {
            return resolved.contains(name_substring);
        }
        false
    })
}

/// Helper: collect all Endpoint node names from staging.
fn collect_endpoint_names(staging: &StagingGraph) -> Vec<String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Endpoint)
            {
                return staging.resolve_local_string(entry.name).map(String::from);
            }
            None
        })
        .collect()
}

#[test]
fn test_js_express_all_creates_endpoint() {
    let source = r#"
const express = require("express");
const app = express();

app.all("/health", (req, res) => {
    res.json({ status: "ok" });
});
"#;

    let tree = parse_javascript(source);
    let file = Path::new("test_js_express_all.js");
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_endpoint_with_name(&staging, "route::ALL::/health"),
        "Expected Endpoint 'route::ALL::/health', found: {:?}",
        collect_endpoint_names(&staging)
    );
    // Verify it's NOT route::GET (that was the old bug)
    assert!(
        !has_endpoint_with_name(&staging, "route::GET::/health"),
        "app.all() should NOT create route::GET, found: {:?}",
        collect_endpoint_names(&staging)
    );
}
