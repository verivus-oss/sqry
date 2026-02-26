//! Integration tests for TypeOf and Reference edge extraction in Zig.
//!
//! This test suite validates that the Zig graph builder correctly extracts
//! type information and creates appropriate TypeOf and Reference edges for:
//! - Variable and constant declarations
//! - Function parameters and return types
//! - Struct, union, and enum fields
//! - Complex type constructs (pointers, slices, optionals, error unions, etc.)

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_lang_zig::relations::ZigGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

// ============================================================================
// Test Helpers
// ============================================================================

fn build_test_graph(code: &str) -> StagingGraph {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .expect("Failed to load Zig grammar");

    let tree = parser.parse(code, None).expect("Failed to parse code");
    let mut staging = StagingGraph::new();
    let builder = ZigGraphBuilder::default();
    let file_path = Path::new("test.zig");

    builder
        .build_graph(&tree, code.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Build a string lookup map from staged InternString operations
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map from staged AddNode operations
fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Helper to collect TypeOf edges filtered by context
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && matches!(
                kind,
                EdgeKind::TypeOf {
                    context: Some(ctx),
                    ..
                } if *ctx == context
            )
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            edges.push((from_name, to_name));
        }
    }

    edges
}

/// Helper to collect Reference edges
fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::References,
            ..
        } = op
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            edges.push((from_name, to_name));
        }
    }

    edges
}

fn has_typeof_edge_with_context(
    graph: &StagingGraph,
    node_name: &str,
    expected_type: &str,
    expected_context: TypeOfContext,
) -> bool {
    let edges = collect_typeof_edges_by_context(graph, expected_context);
    edges
        .iter()
        .any(|(from, to)| from == node_name && to == expected_type)
}

fn has_reference_edge(graph: &StagingGraph, from_name: &str, to_type: &str) -> bool {
    let edges = collect_reference_edges(graph);
    edges
        .iter()
        .any(|(from, to)| from == from_name && to == to_type)
}

// ============================================================================
// Variable Declaration Tests
// ============================================================================

#[test]
fn test_variable_primitive_type() {
    let code = r#"
        var counter: u32 = 0;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "counter", "u32", TypeOfContext::Variable),
        "Variable should have TypeOf edge to u32 with Variable context"
    );
    assert!(
        has_reference_edge(&graph, "counter", "u32"),
        "Variable should have Reference edge to u32"
    );
}

#[test]
fn test_variable_pointer_type() {
    let code = r#"
        const User = struct { id: u32 };
        var user_ptr: *User = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "user_ptr", "*User", TypeOfContext::Variable),
        "Variable should have TypeOf edge with pointer type"
    );
    assert!(
        has_reference_edge(&graph, "user_ptr", "User"),
        "Variable should have Reference edge to User type"
    );
}

#[test]
fn test_variable_const_pointer_type() {
    let code = r#"
        var data: *const i32 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "data", "*const i32", TypeOfContext::Variable),
        "Variable should have TypeOf edge with const pointer type"
    );
    assert!(
        has_reference_edge(&graph, "data", "i32"),
        "Variable should have Reference edge to i32"
    );
}

#[test]
fn test_variable_slice_type() {
    let code = r#"
        var items: []const u8 = &[_]u8{};
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "items", "[]const u8", TypeOfContext::Variable),
        "Variable should have TypeOf edge with slice type"
    );
    assert!(
        has_reference_edge(&graph, "items", "u8"),
        "Variable should have Reference edge to u8"
    );
}

#[test]
fn test_variable_array_type() {
    let code = r#"
        var buffer: [1024]u8 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "buffer", "[1024]u8", TypeOfContext::Variable),
        "Variable should have TypeOf edge with array type"
    );
    assert!(
        has_reference_edge(&graph, "buffer", "u8"),
        "Variable should have Reference edge to u8"
    );
}

#[test]
fn test_variable_optional_type() {
    let code = r#"
        const User = struct { id: u32 };
        var maybe_user: ?User = null;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "maybe_user", "?User", TypeOfContext::Variable),
        "Variable should have TypeOf edge with optional type"
    );
    assert!(
        has_reference_edge(&graph, "maybe_user", "User"),
        "Variable should have Reference edge to User"
    );
}

#[test]
fn test_variable_error_union_anyerror() {
    let code = r#"
        var result: !i32 = 42;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "result", "!i32", TypeOfContext::Variable),
        "Variable should have TypeOf edge with error union type"
    );
    assert!(
        has_reference_edge(&graph, "result", "i32"),
        "Variable should have Reference edge to i32"
    );
}

#[test]
fn test_variable_error_union_named() {
    let code = r#"
        const FileError = error{AccessDenied, FileNotFound};
        const User = struct { id: u32 };
        var user_or_error: FileError!User = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(
            &graph,
            "user_or_error",
            "FileError!User",
            TypeOfContext::Variable
        ),
        "Variable should have TypeOf edge with named error union type"
    );
    // Note: error set names might not be extracted as separate identifiers in some tree-sitter grammars
    assert!(
        has_reference_edge(&graph, "user_or_error", "User"),
        "Variable should have Reference edge to User"
    );
}

// ============================================================================
// Constant Declaration Tests
// ============================================================================

#[test]
fn test_constant_with_type() {
    let code = r#"
        const MAX_SIZE: usize = 1024;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "MAX_SIZE", "usize", TypeOfContext::Variable),
        "Constant should have TypeOf edge with Variable context"
    );
    assert!(
        has_reference_edge(&graph, "MAX_SIZE", "usize"),
        "Constant should have Reference edge to usize"
    );
}

// Type alias test - verifies type aliases create TypeOf and Reference edges
#[test]
fn test_type_alias_constant() {
    let code = r#"
        const ByteArray = []const u8;
    "#;

    let graph = build_test_graph(code);

    // Type aliases in Zig are const declarations where the RHS is a type expression
    // const ByteArray = []const u8 should create:
    // - Variable node: ByteArray
    // - Type node: []const u8
    // - Reference edges: ByteArray → u8
    // - TypeOf edge: ByteArray → []const u8

    // Verify TypeOf edge exists
    assert!(
        has_typeof_edge_with_context(&graph, "ByteArray", "[]const u8", TypeOfContext::Variable),
        "Type alias should have TypeOf edge to []const u8"
    );

    // Verify Reference edge to u8
    assert!(
        has_reference_edge(&graph, "ByteArray", "u8"),
        "Type alias should have Reference edge to u8"
    );
}

#[test]
fn test_constant_custom_type() {
    let code = r#"
        const Point = struct { x: f32, y: f32 };
        const origin: Point = Point{ .x = 0.0, .y = 0.0 };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "origin", "Point", TypeOfContext::Variable),
        "Constant should have TypeOf edge to custom type"
    );
    assert!(
        has_reference_edge(&graph, "origin", "Point"),
        "Constant should have Reference edge to Point"
    );
}

#[test]
fn test_constant_array() {
    let code = r#"
        const PRIMES: [6]u32 = [_]u32{ 2, 3, 5, 7, 11, 13 };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "PRIMES", "[6]u32", TypeOfContext::Variable),
        "Constant array should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "PRIMES", "u32"),
        "Constant array should have Reference edge to element type"
    );
}

// ============================================================================
// Function Parameter Tests
// ============================================================================

#[test]
fn test_function_simple_parameters() {
    let code = r#"
        pub fn add(a: i32, b: i32) i32 {
            return a + b;
        }
    "#;

    let graph = build_test_graph(code);

    // Function should have TypeOf edges for both parameters
    assert!(
        has_typeof_edge_with_context(&graph, "add", "i32", TypeOfContext::Parameter),
        "Function should have TypeOf edge for parameter with Parameter context"
    );
    assert!(
        has_reference_edge(&graph, "add", "i32"),
        "Function should have Reference edge to i32 from parameters"
    );
}

#[test]
fn test_function_slice_parameter() {
    let code = r#"
        fn process(data: []const u8) void {
            _ = data;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "process", "[]const u8", TypeOfContext::Parameter),
        "Function should have TypeOf edge for slice parameter"
    );
    assert!(
        has_reference_edge(&graph, "process", "u8"),
        "Function should have Reference edge to u8 from slice parameter"
    );
}

#[test]
fn test_function_pointer_parameter() {
    let code = r#"
        const User = struct { id: u32 };
        fn updateUser(user: *User) void {
            _ = user;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "updateUser", "*User", TypeOfContext::Parameter),
        "Function should have TypeOf edge for pointer parameter"
    );
    assert!(
        has_reference_edge(&graph, "updateUser", "User"),
        "Function should have Reference edge to User from pointer parameter"
    );
}

#[test]
fn test_function_optional_parameter() {
    let code = r#"
        fn findById(id: ?u32) void {
            _ = id;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "findById", "?u32", TypeOfContext::Parameter),
        "Function should have TypeOf edge for optional parameter"
    );
    assert!(
        has_reference_edge(&graph, "findById", "u32"),
        "Function should have Reference edge to u32 from optional parameter"
    );
}

#[test]
fn test_function_multiple_parameters() {
    let code = r#"
        const Point = struct { x: f32, y: f32 };
        fn distance(p1: *const Point, p2: *const Point) f32 {
            _ = p1;
            _ = p2;
            return 0.0;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "distance", "*const Point", TypeOfContext::Parameter),
        "Function should have TypeOf edges for pointer parameters"
    );
    assert!(
        has_reference_edge(&graph, "distance", "Point"),
        "Function should have Reference edge to Point from parameters"
    );
}

// ============================================================================
// Function Return Type Tests
// ============================================================================

#[test]
fn test_function_simple_return() {
    let code = r#"
        pub fn add(a: i32, b: i32) i32 {
            return a + b;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "add", "i32", TypeOfContext::Return),
        "Function should have TypeOf edge for return type with Return context"
    );
}

#[test]
fn test_function_void_return() {
    let code = r#"
        fn doSomething() void {
            // no-op
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "doSomething", "void", TypeOfContext::Return),
        "Function should have TypeOf edge for void return"
    );
}

#[test]
fn test_function_optional_return() {
    let code = r#"
        const User = struct { id: u32 };
        fn findUser(id: u32) ?User {
            _ = id;
            return null;
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "findUser", "?User", TypeOfContext::Return),
        "Function should have TypeOf edge for optional return type"
    );
    assert!(
        has_reference_edge(&graph, "findUser", "User"),
        "Function should have Reference edge to User from return type"
    );
}

#[test]
fn test_function_error_union_return() {
    let code = r#"
        pub fn divide(a: i32, b: i32) !f32 {
            if (b == 0) return error.DivisionByZero;
            return @as(f32, @floatFromInt(a)) / @as(f32, @floatFromInt(b));
        }
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "divide", "!f32", TypeOfContext::Return),
        "Function should have TypeOf edge for error union return"
    );
    assert!(
        has_reference_edge(&graph, "divide", "f32"),
        "Function should have Reference edge to f32 from error union return"
    );
}

// ============================================================================
// Struct Field Tests
// ============================================================================

#[test]
fn test_struct_simple_fields() {
    let code = r#"
        pub const Point = struct {
            x: f32,
            y: f32,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "Point.x", "f32", TypeOfContext::Field),
        "Struct field should have TypeOf edge with Field context"
    );
    assert!(
        has_typeof_edge_with_context(&graph, "Point.y", "f32", TypeOfContext::Field),
        "Struct field should have TypeOf edge with Field context"
    );
}

#[test]
fn test_struct_pointer_fields() {
    let code = r#"
        const Node = struct {
            value: i32,
            next: ?*Node,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "Node.next", "?*Node", TypeOfContext::Field),
        "Struct field should have TypeOf edge for optional pointer"
    );
    assert!(
        has_reference_edge(&graph, "Node.next", "Node"),
        "Struct field should have Reference edge to Node"
    );
}

#[test]
fn test_struct_slice_fields() {
    let code = r#"
        pub const User = struct {
            name: []const u8,
            id: u32,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "User.name", "[]const u8", TypeOfContext::Field),
        "Struct field should have TypeOf edge for slice type"
    );
    assert!(
        has_reference_edge(&graph, "User.name", "u8"),
        "Struct field should have Reference edge to u8"
    );
}

#[test]
fn test_struct_array_fields() {
    let code = r#"
        const Buffer = struct {
            data: [1024]u8,
            len: usize,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "Buffer.data", "[1024]u8", TypeOfContext::Field),
        "Struct field should have TypeOf edge for array type"
    );
    assert!(
        has_reference_edge(&graph, "Buffer.data", "u8"),
        "Struct field should have Reference edge to u8"
    );
}

#[test]
fn test_struct_custom_type_fields() {
    let code = r#"
        const Address = struct { street: []const u8, zip: u32 };
        const User = struct {
            name: []const u8,
            address: Address,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "User.address", "Address", TypeOfContext::Field),
        "Struct field should have TypeOf edge to custom type"
    );
    assert!(
        has_reference_edge(&graph, "User.address", "Address"),
        "Struct field should have Reference edge to Address"
    );
}

// ============================================================================
// Union Field Tests
// ============================================================================

#[test]
fn test_union_simple_fields() {
    let code = r#"
        pub const Value = union(enum) {
            int: i32,
            float: f64,
            boolean: bool,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "Value.int", "i32", TypeOfContext::Field),
        "Union field should have TypeOf edge"
    );
    assert!(
        has_typeof_edge_with_context(&graph, "Value.float", "f64", TypeOfContext::Field),
        "Union field should have TypeOf edge"
    );
}

#[test]
fn test_union_custom_type_fields() {
    let code = r#"
        const User = struct { id: u32 };
        const Guest = struct { session: []const u8 };
        const Account = union(enum) {
            user: User,
            guest: Guest,
        };
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "Account.user", "User", TypeOfContext::Field),
        "Union field should have TypeOf edge to custom type"
    );
    assert!(
        has_reference_edge(&graph, "Account.user", "User"),
        "Union field should have Reference edge to User"
    );
}

// ============================================================================
// Complex Type Tests
// ============================================================================

#[test]
fn test_nested_pointers() {
    let code = r#"
        const User = struct { id: u32 };
        var ptr_to_ptr: *[*]User = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "ptr_to_ptr", "*[*]User", TypeOfContext::Variable),
        "Nested pointer should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "ptr_to_ptr", "User"),
        "Nested pointer should have Reference edge to base type"
    );
}

#[test]
fn test_optional_pointer() {
    let code = r#"
        const Node = struct { value: i32 };
        var maybe_ptr: ?*const Node = null;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "maybe_ptr", "?*const Node", TypeOfContext::Variable),
        "Optional pointer should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "maybe_ptr", "Node"),
        "Optional pointer should have Reference edge to Node"
    );
}

#[test]
fn test_many_item_pointer() {
    let code = r#"
        var items: [*]const i32 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "items", "[*]const i32", TypeOfContext::Variable),
        "Many-item pointer should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "items", "i32"),
        "Many-item pointer should have Reference edge to element type"
    );
}

#[test]
fn test_c_pointer() {
    let code = r#"
        var c_ptr: [*c]u8 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "c_ptr", "[*c]u8", TypeOfContext::Variable),
        "C pointer should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "c_ptr", "u8"),
        "C pointer should have Reference edge to element type"
    );
}

#[test]
fn test_pointer_to_array() {
    let code = r#"
        var buffer_ptr: *[1024]u8 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "buffer_ptr", "*[1024]u8", TypeOfContext::Variable),
        "Pointer to array should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "buffer_ptr", "u8"),
        "Pointer to array should have Reference edge to element type"
    );
}

#[test]
fn test_nested_slices() {
    let code = r#"
        var matrix: [][]const u8 = undefined;
    "#;

    let graph = build_test_graph(code);

    assert!(
        has_typeof_edge_with_context(&graph, "matrix", "[][]const u8", TypeOfContext::Variable),
        "Nested slices should have TypeOf edge"
    );
    assert!(
        has_reference_edge(&graph, "matrix", "u8"),
        "Nested slices should have Reference edge to base element type"
    );
}

#[test]
fn test_multiple_primitive_types() {
    let code = r#"
        var a: i8 = 0;
        var b: i16 = 0;
        var c: i32 = 0;
        var d: i64 = 0;
        var e: u8 = 0;
        var f: u16 = 0;
        var g: u32 = 0;
        var h: u64 = 0;
        var i: f32 = 0.0;
        var j: f64 = 0.0;
        var k: bool = false;
        var l: usize = 0;
    "#;

    let graph = build_test_graph(code);

    // Test a subset of primitive types
    assert!(has_typeof_edge_with_context(
        &graph,
        "a",
        "i8",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "c",
        "i32",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "g",
        "u32",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "i",
        "f32",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "k",
        "bool",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "l",
        "usize",
        TypeOfContext::Variable
    ));
}

// Additional tests for spec coverage (Finding #4)

#[test]
fn test_comptime_parameter() {
    let code = r#"
        fn generic(comptime T: type) void {}
    "#;

    let graph = build_test_graph(code);

    assert!(has_typeof_edge_with_context(
        &graph,
        "generic",
        "type",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_threadlocal_variable() {
    let code = r#"
        threadlocal var counter: u32 = 0;
    "#;

    let graph = build_test_graph(code);

    assert!(has_typeof_edge_with_context(
        &graph,
        "counter",
        "u32",
        TypeOfContext::Variable
    ));
}

#[test]
fn test_struct_with_optional_fields() {
    let code = r#"
        const User = struct {
            name: ?[]const u8,
            age: ?u32,
        };
    "#;

    let graph = build_test_graph(code);

    // Struct fields should have TypeOf edges (qualified name or field name)
    let edges = collect_typeof_edges_by_context(&graph, TypeOfContext::Field);
    let has_name_field = edges
        .iter()
        .any(|(from, to)| from.contains("name") && to.contains("?[]const u8"));
    let has_age_field = edges
        .iter()
        .any(|(from, to)| from.contains("age") && to.contains("?u32"));

    assert!(has_name_field, "Should have Field TypeOf edge for name");
    assert!(has_age_field, "Should have Field TypeOf edge for age");
}

#[test]
fn test_generic_arraylist() {
    let code = r#"
        const ArrayList = @import("std").ArrayList;
        var list: ArrayList(i32) = undefined;
    "#;

    let graph = build_test_graph(code);

    // Should have Reference edge to i32
    assert!(has_reference_edge(&graph, "list", "i32"));
}

#[test]
fn test_error_union_with_custom_error() {
    let code = r#"
        const FileError = error{ NotFound, PermissionDenied };
        var result: FileError![]const u8 = undefined;
    "#;

    let graph = build_test_graph(code);

    // Should have Reference edges
    assert!(has_reference_edge(&graph, "result", "FileError"));
    assert!(has_reference_edge(&graph, "result", "u8"));
}

#[test]
fn test_namespaced_type() {
    let code = r#"
        var allocator: std.mem.Allocator = undefined;
    "#;

    let graph = build_test_graph(code);

    // Should have TypeOf edge with full qualified name
    assert!(
        has_typeof_edge_with_context(
            &graph,
            "allocator",
            "std.mem.Allocator",
            TypeOfContext::Variable
        ),
        "Namespaced type should have TypeOf edge with full qualified name"
    );

    // Should have Reference edge to the full qualified name
    assert!(
        has_reference_edge(&graph, "allocator", "std.mem.Allocator"),
        "Namespaced type should have Reference edge to full qualified name"
    );
}

#[test]
fn test_implicit_error_union() {
    let code = r#"
        var maybe_user: !User = undefined;
    "#;

    let graph = build_test_graph(code);

    // Should have TypeOf edge
    assert!(
        has_typeof_edge_with_context(&graph, "maybe_user", "!User", TypeOfContext::Variable),
        "Implicit error union should have TypeOf edge"
    );

    // Should have Reference edge to User (payload type only, not duplicate)
    assert!(
        has_reference_edge(&graph, "maybe_user", "User"),
        "Implicit error union should have Reference edge to payload type"
    );

    // Should NOT have duplicate Reference edge
    // Count reference edges to verify exactly one
    let ref_edges = collect_reference_edges(&graph);
    let user_ref_count = ref_edges
        .iter()
        .filter(|(src, tgt)| src.contains("maybe_user") && tgt == "User")
        .count();

    assert_eq!(
        user_ref_count, 1,
        "Implicit error union should have exactly one Reference edge to User, not duplicates. Found {user_ref_count}"
    );
}

#[test]
fn test_function_with_many_parameters() {
    let code = r#"
        fn complex(a: i32, b: u32, c: f32, d: bool, e: usize) void {}
    "#;

    let graph = build_test_graph(code);

    // All parameters should have TypeOf edges
    assert!(has_typeof_edge_with_context(
        &graph,
        "complex",
        "i32",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "complex",
        "u32",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "complex",
        "f32",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "complex",
        "bool",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge_with_context(
        &graph,
        "complex",
        "usize",
        TypeOfContext::Parameter
    ));
}
