//! Tests for `LuaJIT` FFI edge detection

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::FfiConvention;
use sqry_lang_lua::LuaGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Parser;

#[allow(dead_code)]
fn debug_ast(content: &[u8]) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(content, None).unwrap();
    debug_node(tree.root_node(), content, 0);
}

#[allow(dead_code)]
fn debug_node(node: tree_sitter::Node, content: &[u8], indent: usize) {
    let indent_str = "  ".repeat(indent);
    let text = node.utf8_text(content).ok().and_then(|t| {
        let trimmed = t.trim();
        if trimmed.len() > 50 || trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    eprintln!(
        "{}{}{}",
        indent_str,
        node.kind(),
        text.map(|t| format!(" [{t}]")).unwrap_or_default()
    );

    if indent < 15 {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            debug_node(child, content, indent + 1);
        }
    }
}

/// Helper to build graph from Lua code
fn build_graph_from_lua(content: &[u8]) -> StagingGraph {
    let builder = LuaGraphBuilder::default();
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(content, None).unwrap();

    let mut staging = StagingGraph::default();
    let file = PathBuf::from("test.lua");

    builder
        .build_graph(&tree, content, &file, &mut staging)
        .unwrap();

    staging
}

/// Helper to count FFI edges
fn count_ffi_edges(staging: &StagingGraph) -> usize {
    staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .count()
}

/// Helper to get FFI edge targets
fn get_ffi_targets(staging: &StagingGraph) -> Vec<String> {
    staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .filter_map(|e| {
            staging
                .nodes()
                .find(|n| n.expected_id == Some(e.target))
                .and_then(|n| staging.resolve_node_name(n.entry))
                .map(std::string::ToString::to_string)
        })
        .collect()
}

// ============================================================================
// Debug Tests
// ============================================================================

#[test]
#[ignore = "debug helper for AST inspection"]
fn debug_alias_ast() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
C.puts("test")
"#;
    debug_ast(content);
    panic!("Debug test - check output above");
}

// ============================================================================
// Positive Tests - FFI Detection
// ============================================================================

#[test]
fn test_ffi_require_detection() {
    let content = br#"
local ffi = require("ffi")
"#;

    let staging = build_graph_from_lua(content);
    // require("ffi") itself doesn't create FFI edges, just enables FFI
    assert_eq!(count_ffi_edges(&staging), 0);
}

#[test]
fn test_ffi_c_direct_call() {
    let content = br#"
local ffi = require("ffi")
ffi.cdef[[
    int printf(const char *fmt, ...);
]]
ffi.C.printf("Hello")
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 1, "Expected 1 FfiCall edge for ffi.C.printf");

    let targets = get_ffi_targets(&staging);
    assert!(
        targets.contains(&"native::printf".to_string()),
        "Expected native::printf target, found: {targets:?}"
    );
}

#[test]
fn test_ffi_c_multiple_calls() {
    let content = br#"
local ffi = require("ffi")
ffi.cdef[[
    void *malloc(size_t size);
    void free(void *ptr);
]]
ffi.C.malloc(1024)
ffi.C.free(ptr)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 2, "Expected 2 FfiCall edges");

    let targets = get_ffi_targets(&staging);
    assert!(targets.contains(&"native::malloc".to_string()));
    assert!(targets.contains(&"native::free".to_string()));
}

#[test]
fn test_ffi_load_library() {
    let content = br#"
local ffi = require("ffi")
local mylib = ffi.load("mylib")
ffi.cdef[[ int my_func(int x); ]]
mylib.my_func(42)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(
        ffi_edges, 2,
        "Expected 2 FfiCall edges (ffi.load + mylib.my_func)"
    );

    let targets = get_ffi_targets(&staging);
    assert!(
        targets.contains(&"native::mylib::my_func".to_string()),
        "Expected native::mylib::my_func, found: {targets:?}"
    );
    assert!(
        targets.iter().any(|t| t.contains("mylib")),
        "Expected ffi.load edge to mylib"
    );
}

#[test]
fn test_ffi_alias_c() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
ffi.cdef[[ int puts(const char *s); ]]
C.puts("Hello")
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 1, "Expected 1 FfiCall edge for C.puts");

    let targets = get_ffi_targets(&staging);
    assert!(targets.contains(&"native::puts".to_string()));
}

#[test]
fn test_ffi_alias_library() {
    let content = br#"
local ffi = require("ffi")
local m = ffi.load("m")
ffi.cdef[[ double cos(double x); ]]
m.cos(0)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 2, "Expected 2 FfiCall edges (ffi.load + m.cos)");

    let targets = get_ffi_targets(&staging);
    assert!(
        targets.contains(&"native::m::cos".to_string()),
        "Expected native::m::cos, found: {targets:?}"
    );
}

#[test]
fn test_ffi_alias_chain() {
    let content = br#"
local ffi2 = require("ffi")
local C2 = ffi2.C
ffi2.cdef[[ void exit(int status); ]]
C2.exit(0)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 1, "Expected 1 FfiCall edge for C2.exit");

    let targets = get_ffi_targets(&staging);
    assert!(targets.contains(&"native::exit".to_string()));
}

#[test]
fn test_ffi_complex_workflow() {
    let content = br#"
local ffi = require("ffi")
ffi.cdef[[
    int printf(const char *fmt, ...);
    void *malloc(size_t size);
    void free(void *ptr);
]]

local C = ffi.C
C.printf("Test\n")

local ptr = C.malloc(1024)
C.free(ptr)

local mylib = ffi.load("mylib")
ffi.cdef[[ int my_func(int x); ]]
mylib.my_func(42)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(
        ffi_edges, 5,
        "Expected 5 FfiCall edges (printf, malloc, free, ffi.load, my_func)"
    );

    let targets = get_ffi_targets(&staging);
    assert!(targets.contains(&"native::printf".to_string()));
    assert!(targets.contains(&"native::malloc".to_string()));
    assert!(targets.contains(&"native::free".to_string()));
    assert!(targets.contains(&"native::mylib::my_func".to_string()));
}

#[test]
fn test_ffi_in_function() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
ffi.cdef[[ int puts(const char *s); ]]

function test_func()
    C.puts("Hello from function")
end
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 1, "Expected 1 FfiCall edge");

    // Verify caller is test_func
    let ffi_edge = staging
        .edges()
        .find(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .expect("Should have FFI edge");

    let caller = staging
        .nodes()
        .find(|n| n.expected_id == Some(ffi_edge.source))
        .unwrap();
    let caller_name = staging.resolve_node_name(caller.entry).unwrap();
    assert!(
        caller_name.contains("test_func"),
        "Caller should be test_func, got: {caller_name}"
    );
}

#[test]
fn test_ffi_multiple_libraries() {
    let content = br#"
local ffi = require("ffi")
local lib1 = ffi.load("lib1")
local lib2 = ffi.load("lib2")

ffi.cdef[[
    int lib1_func(int x);
    int lib2_func(int y);
]]

lib1.lib1_func(1)
lib2.lib2_func(2)
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(
        ffi_edges, 4,
        "Expected 4 FfiCall edges (2x ffi.load + 2x lib function calls)"
    );

    let targets = get_ffi_targets(&staging);
    assert!(targets.contains(&"native::lib1::lib1_func".to_string()));
    assert!(targets.contains(&"native::lib2::lib2_func".to_string()));
    // Also verify the ffi.load edges exist
    assert!(targets.iter().any(|t| t.contains("lib1")));
    assert!(targets.iter().any(|t| t.contains("lib2")));
}

// ============================================================================
// Negative Tests - No False Positives
// ============================================================================

#[test]
fn test_no_ffi_regular_calls() {
    let content = br#"
print("Hello")
local x = math.sqrt(4)
local s = string.format("%s", "test")
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Regular Lua calls should not create FFI edges"
    );
}

#[test]
fn test_no_ffi_without_require() {
    let content = br#"
-- C is not defined, this should not create FFI edges
C.printf("Hello")
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Undefined C should not create FFI edges"
    );
}

#[test]
fn test_no_ffi_similar_names() {
    let content = br"
local myC = {}
myC.printf = function() end
myC.printf()
";

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Regular table should not create FFI edges"
    );
}

#[test]
fn test_no_ffi_in_comments() {
    let content = br#"
-- local ffi = require("ffi")
-- ffi.C.printf("test")
print("actual code")
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Commented FFI code should not create edges"
    );
}

#[test]
fn test_no_ffi_in_strings() {
    let content = br#"
local s = "ffi.C.printf('test')"
print(s)
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "FFI code in strings should not create edges"
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_ffi_empty_cdef() {
    let content = br#"
local ffi = require("ffi")
ffi.cdef[[]]
ffi.C.printf("test")
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    // Empty cdef is OK, the call should still be detected
    assert_eq!(ffi_edges, 1);
}

#[test]
fn test_ffi_nested_scopes() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
ffi.cdef[[ int puts(const char *s); ]]

function outer()
    function inner()
        C.puts("nested")
    end
    inner()
end
"#;

    let staging = build_graph_from_lua(content);
    let ffi_edges = count_ffi_edges(&staging);
    assert_eq!(ffi_edges, 1, "Nested function FFI call should be detected");
}

#[test]
fn test_ffi_edge_convention() {
    let content = br#"
local ffi = require("ffi")
ffi.C.printf("test")
"#;

    let staging = build_graph_from_lua(content);

    let ffi_edge = staging
        .edges()
        .find(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .expect("Should have FFI edge");

    if let EdgeKind::FfiCall { convention } = ffi_edge.kind {
        assert_eq!(*convention, FfiConvention::C, "FFI convention should be C");
    } else {
        panic!("Edge should be FfiCall");
    }
}

#[test]
fn test_ffi_target_names() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
local lib = ffi.load("testlib")

ffi.cdef[[
    int c_func(int x);
    int lib_func(int y);
]]

C.c_func(1)
lib.lib_func(2)
"#;

    let staging = build_graph_from_lua(content);
    let targets = get_ffi_targets(&staging);

    assert!(
        targets.contains(&"native::c_func".to_string()),
        "C library calls should use native::func format"
    );
    assert!(
        targets.contains(&"native::testlib::lib_func".to_string()),
        "Loaded library calls should use native::lib::func format"
    );
}

#[test]
fn test_ffi_cdef_no_edge() {
    let content = br#"
local ffi = require("ffi")
ffi.cdef[[
    int printf(const char *fmt, ...);
]]
-- Note: No actual call to printf, just declaration
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "ffi.cdef should not create FFI edges (it's just a declaration)"
    );
}

#[test]
fn test_ffi_edge_count_accuracy() {
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
ffi.cdef[[
    int f1(int x);
    int f2(int x);
    int f3(int x);
]]

C.f1(1)
C.f2(2)
C.f3(3)
"#;

    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        3,
        "Should detect exactly 3 FFI calls"
    );

    let targets = get_ffi_targets(&staging);
    assert_eq!(targets.len(), 3, "Should have 3 unique targets");
}

// ============================================================================
// Iteration 2 Tests (Codex Review Fixes)
// ============================================================================

#[test]
fn test_no_ffi_literal_ffi_without_require() {
    // Use literal "ffi" identifier without require("ffi") - should create NO edges
    // This tests the H1 fix: removal of unconditional "ffi" fallback
    let content = br#"
-- No require("ffi")
ffi.C.printf("no require")
"#;
    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Literal ffi identifier without require should not create edges"
    );
}

#[test]
fn test_no_ffi_similar_require() {
    // require() with similar but non-FFI modules (tests H2 fix: exact string matching)
    let content = br#"
local office = require("office")
local ffi_utils = require("ffi_utils")
office.print("test")
ffi_utils.helper()
"#;
    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        0,
        "Similar module names should not trigger FFI detection"
    );
}

#[test]
fn test_ffi_multi_level_alias_chain() {
    // Test multi-level alias resolution (M1 fix)
    let content = br#"
local ffi = require("ffi")
local C = ffi.C
local D = C  -- Multi-level alias chain
ffi.cdef[[int printf(const char *fmt, ...);]]
D.printf("chained alias")
"#;
    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        1,
        "Should detect FFI call through multi-level alias chain"
    );

    let targets = get_ffi_targets(&staging);
    assert_eq!(targets, vec!["native::printf"]);
}

#[test]
fn test_ffi_malformed_syntax() {
    // Incomplete code - should not panic (H3 test plan requirement)
    let content = br#"
local ffi = require("ffi")
local lib = ffi.load("mylib"
"#;
    let staging = build_graph_from_lua(content);
    // Should handle gracefully (partial edges acceptable due to incomplete parse)
    let edge_count = count_ffi_edges(&staging);
    assert!(
        edge_count <= 1,
        "Malformed syntax should be handled gracefully, got {edge_count} edges"
    );
}

#[test]
fn test_ffi_full_example() {
    // Comprehensive example demonstrating multiple FFI patterns (H3 test plan requirement)
    let content = br#"
local ffi = require("ffi")

ffi.cdef[[
    int printf(const char *fmt, ...);
    void *malloc(size_t size);
    void free(void *ptr);
    double cos(double x);
]]

-- Direct C library calls
ffi.C.printf("Hello from LuaJIT FFI!\n")
ffi.C.printf("Value: %d\n", 42)

-- C library alias
local C = ffi.C
C.printf("Using alias C\n")

-- Load external library
local m = ffi.load("m")
local result = m.cos(0)
C.printf("cos(0) = %f\n", result)

-- Memory management
local ptr = C.malloc(1024)
C.free(ptr)

-- Multiple library loads
local m2 = ffi.load("m")
local result2 = m2.cos(1.5708)

-- FFI in function
function test_func()
    C.printf("FFI in function\n")
    m.cos(3.14159)
end
test_func()
"#;

    let staging = build_graph_from_lua(content);

    // Expected edges:
    // - 2x ffi.load (m, m2)
    // - 5x C.printf calls (direct + via alias)
    // - 1x C.malloc
    // - 1x C.free
    // - 3x m/m2.cos calls
    // Total: 12 edges
    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 10,
        "Expected at least 10 FFI edges from comprehensive example, got {ffi_count}"
    );

    let targets = get_ffi_targets(&staging);
    assert!(
        targets.iter().any(|t| t.contains("printf")),
        "Should include printf calls"
    );
    assert!(
        targets.iter().any(|t| t.contains("malloc")),
        "Should include malloc call"
    );
    assert!(
        targets.iter().any(|t| t.contains("cos")),
        "Should include cos calls"
    );
}

#[test]
fn test_ffi_load_creates_edge() {
    // Verify that ffi.load() itself creates an edge (C1 fix: spec compliance)
    let content = br#"
local ffi = require("ffi")
local mylib = ffi.load("mylib")
-- Note: Only the load call, no function calls through mylib
"#;
    let staging = build_graph_from_lua(content);
    assert_eq!(
        count_ffi_edges(&staging),
        1,
        "ffi.load() should create an FFI edge to the library"
    );

    let targets = get_ffi_targets(&staging);
    assert!(
        targets.contains(&"native::mylib".to_string()),
        "ffi.load target should be exactly 'native::mylib' per spec, got: {targets:?}"
    );
}
