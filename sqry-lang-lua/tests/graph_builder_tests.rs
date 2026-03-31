//! Graph builder tests for the Lua language plugin.
//!
//! Covers:
//! - Function node extraction (global, local, module methods)
//! - Call edge detection
//! - LuaJIT FFI detection
//! - Export edge detection
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_lua::LuaGraphBuilder;
use std::path::Path;

fn parse_lua(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .expect("failed to set Lua language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Lua code")
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

fn count_export_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Exports { .. }))
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

fn has_node_of_kind(staging: &StagingGraph, kind: NodeKind) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            entry.kind == kind
        } else {
            false
        }
    })
}

// ==================== Basic Node Extraction ====================

#[test]
fn test_global_function_extraction() {
    let source = r"
function greet(name)
    return 'Hello, ' .. name
end

function add(a, b)
    return a + b
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.lua"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 function nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        has_interned_string_containing(&staging, "greet"),
        "Expected 'greet' function"
    );
    assert!(
        has_interned_string_containing(&staging, "add"),
        "Expected 'add' function"
    );
}

#[test]
fn test_local_function_extraction() {
    let source = r"
local function helper()
    return 42
end

local function compute(x)
    return helper() * x
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("local.lua"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 local function nodes, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_module_method_extraction() {
    let source = r"
local M = {}

function M.greet(name)
    return 'Hello, ' .. name
end

function M.farewell(name)
    return 'Goodbye, ' .. name
end

return M
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("module.lua"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 module method nodes, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_function_nodes_have_function_kind() {
    let source = r"
function standalone()
    return true
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.lua"),
            &mut staging,
        )
        .unwrap();

    assert!(
        has_node_of_kind(&staging, NodeKind::Function),
        "Expected at least one Function-kind node"
    );
}

// ==================== Call Edge Detection ====================

#[test]
fn test_call_edge_detection() {
    let source = r"
function helper()
    print('helper')
end

function caller()
    helper()
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.lua"),
            &mut staging,
        )
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge, got {}",
        call_count
    );
}

#[test]
fn test_method_call_detection() {
    let source = r"
local obj = {}

function obj:method1()
    return 1
end

function obj:method2()
    return self:method1() + 1
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("obj.lua"), &mut staging);
    assert!(result.is_ok(), "Method calls should succeed");
}

#[test]
fn test_nested_calls() {
    let source = r"
function level3()
    return 1
end

function level2()
    return level3()
end

function level1()
    return level2()
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("nested.lua"),
            &mut staging,
        )
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 2,
        "Expected at least 2 call edges, got {}",
        call_count
    );
}

// ==================== Export Edge Detection ====================

#[test]
fn test_module_export() {
    let source = r"
local M = {}

function M.public_api()
    return 'hello'
end

return M
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("module.lua"),
            &mut staging,
        )
        .unwrap();

    // Module methods should be exported
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for module method, got {}",
        export_count
    );
}

// ==================== LuaJIT FFI Detection ====================

#[test]
fn test_luajit_ffi_detection() {
    let source = r"
local ffi = require('ffi')

ffi.cdef[[
    int printf(const char *fmt, ...);
    void *malloc(size_t size);
]]

local function use_c()
    ffi.C.printf('hello\n')
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("ffi.lua"), &mut staging);
    assert!(result.is_ok(), "LuaJIT FFI should succeed");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = LuaGraphBuilder::default();
    assert_eq!(builder.language(), Language::Lua);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LuaGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.lua"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Lua file should succeed");

    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should produce no nodes");
}

#[test]
fn test_malformed_lua() {
    // Incomplete Lua - tree-sitter is error-tolerant
    let source = r"
function broken(
"; // incomplete
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.lua"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
-- This is a comment
--[[
    Multi-line comment
]]
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.lua"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Lua file should succeed");
}

#[test]
fn test_table_functions() {
    // Lua table-based OOP
    let source = r"
local MyClass = {}
MyClass.__index = MyClass

function MyClass.new(value)
    local self = setmetatable({}, MyClass)
    self.value = value
    return self
end

function MyClass:getValue()
    return self.value
end

function MyClass:double()
    return self:getValue() * 2
end
";
    let tree = parse_lua(source);
    let mut staging = StagingGraph::new();
    let builder = LuaGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("class.lua"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 function nodes, got {}",
        stats.nodes_staged
    );
}
