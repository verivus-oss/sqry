//! Graph builder tests for the Zig language plugin.
//!
//! Covers:
//! - Function node extraction (top-level and pub)
//! - Call edge detection (direct, qualified, method)
//! - Import edge detection (@import)
//! - Export edge detection (pub declarations)
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_zig::ZigGraphBuilder;
use std::path::Path;

fn parse_zig(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .expect("failed to set Zig language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Zig code")
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
fn test_function_extraction() {
    let source = r"
fn greet(name: []const u8) void {
    _ = name;
}

fn add(a: i32, b: i32) i32 {
    return a + b;
}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.zig"),
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
fn test_pub_function_extraction() {
    let source = r"
pub fn init(allocator: std.mem.Allocator) !void {
    _ = allocator;
}

pub fn deinit(self: *Self) void {
    _ = self;
}

fn private_helper() void {}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("server.zig"),
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

#[test]
fn test_function_nodes_have_function_kind() {
    let source = r"
fn standalone() void {}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.zig"),
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
fn test_direct_call_detection() {
    let source = r"
fn helper() void {}

fn caller() void {
    helper();
}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("calls.zig"),
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
fn test_qualified_call_detection() {
    let source = r#"
const std = @import("std");

fn main() !void {
    const stdout = std.io.getStdOut().writer();
    try stdout.print("Hello, World!\n", .{});
}
"#;
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.zig"),
            &mut staging,
        )
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge for qualified calls, got {}",
        call_count
    );
}

#[test]
fn test_nested_function_calls() {
    let source = r"
fn level3() i32 {
    return 1;
}

fn level2() i32 {
    return level3() * 2;
}

fn level1() i32 {
    return level2() + 1;
}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("nested.zig"),
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

// ==================== Import Edge Detection ====================

#[test]
fn test_std_import() {
    let source = r#"
const std = @import("std");

pub fn main() !void {
    std.debug.print("Hello!\n", .{});
}
"#;
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("hello.zig"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for @import, got {}",
        import_count
    );
    assert!(
        has_interned_string_containing(&staging, "std"),
        "Expected 'std' in import edges"
    );
}

#[test]
fn test_local_file_import() {
    let source = r#"
const utils = @import("utils.zig");
const types = @import("types.zig");

pub fn process(input: types.Input) types.Output {
    return utils.transform(input);
}
"#;
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.zig"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges, got {}",
        import_count
    );
}

#[test]
fn test_multiple_imports() {
    let source = r#"
const std = @import("std");
const mem = std.mem;
const ArrayList = std.ArrayList;
const builtin = @import("builtin");
const config = @import("config.zig");
"#;
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("imports.zig"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges, got {}",
        import_count
    );
}

// ==================== Export Edges ====================

#[test]
fn test_pub_function_export() {
    let source = r"
pub fn publicApi() void {}

pub fn anotherApi(x: i32) i32 {
    return x * 2;
}

fn internal() void {}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("lib.zig"), &mut staging)
        .unwrap();

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for pub functions, got {}",
        export_count
    );
}

// ==================== Struct Functions ====================

#[test]
fn test_struct_with_methods() {
    let source = r#"
const std = @import("std");

const Counter = struct {
    count: u32,

    pub fn init() Counter {
        return Counter{ .count = 0 };
    }

    pub fn increment(self: *Counter) void {
        self.count += 1;
    }

    pub fn value(self: Counter) u32 {
        return self.count;
    }
};

pub fn main() !void {
    var c = Counter.init();
    c.increment();
    std.debug.print("{}\n", .{c.value()});
}
"#;
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("counter.zig"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 nodes (struct methods), got {}",
        stats.nodes_staged
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = ZigGraphBuilder::default();
    assert_eq!(builder.language(), Language::Zig);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ZigGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Zig file should succeed");
}

#[test]
fn test_malformed_zig() {
    // Incomplete Zig - tree-sitter is error-tolerant
    let source = r"
fn broken(
"; // incomplete
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.zig"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
// This is a comment
/// Documentation comment
// Another comment
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.zig"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Zig file should succeed");
}

#[test]
fn test_generic_function() {
    let source = r"
fn max(comptime T: type, a: T, b: T) T {
    if (a > b) return a;
    return b;
}

pub fn main() void {
    const x = max(i32, 3, 7);
    const y = max(f64, 1.5, 2.5);
    _ = x;
    _ = y;
}
";
    let tree = parse_zig(source);
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("generic.zig"),
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
