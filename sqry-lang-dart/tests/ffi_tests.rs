//! FFI edge detection tests for Dart plugin.
//!
//! Tests all FFI patterns:
//! - DynamicLibrary.{open, executable, process}
//! - `lookup().asFunction()`
//! - `lookupFunction()`
//! - @Native annotation
//! - @`FfiNative` annotation

use sqry_core::graph::{GraphBuilder, unified::StagingGraph, unified::edge::kind::EdgeKind};
use sqry_lang_dart::DartGraphBuilder;
use std::path::Path;
use tree_sitter::{Parser, Tree};

/// Parse Dart source code into AST.
fn parse_dart(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_dart::language()).unwrap();
    parser.parse(source.as_bytes(), None).unwrap()
}

/// Count FFI call edges in the staging graph.
fn count_ffi_call_edges(staging: &StagingGraph) -> usize {
    staging
        .edges()
        .filter(|edge| matches!(edge.kind, EdgeKind::FfiCall { .. }))
        .count()
}

/// Check if staging graph contains a node with the given name.
fn has_node_with_name(staging: &StagingGraph, name: &str) -> bool {
    staging
        .nodes()
        .any(|node| staging.resolve_node_name(node.entry) == Some(name))
}

// ================================
// Positive Tests - DynamicLibrary
// ================================

#[test]
fn test_dynamic_library_open_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void loadLib() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for DynamicLibrary.open, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:DynamicLibrary.open>"),
        "Expected FFI target node <ffi:DynamicLibrary.open>"
    );
}

#[test]
fn test_dynamic_library_executable_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void loadLib() {
  final dylib = ffi.DynamicLibrary.executable();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for DynamicLibrary.executable, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:DynamicLibrary.executable>"),
        "Expected FFI target node <ffi:DynamicLibrary.executable>"
    );
}

#[test]
fn test_dynamic_library_process_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void loadLib() {
  final dylib = ffi.DynamicLibrary.process();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for DynamicLibrary.process, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:DynamicLibrary.process>"),
        "Expected FFI target node <ffi:DynamicLibrary.process>"
    );
}

// ================================
// Positive Tests - lookup/asFunction
// ================================

#[test]
fn test_lookup_asfunction_chain_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void callNative() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
  final hello = dylib.lookup<ffi.NativeFunction<ffi.Int32 Function(ffi.Int32)>>('hello')
                     .asFunction<int Function(int)>();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    // Should have 2 edges: DynamicLibrary.open + lookup/asFunction
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected 2 FfiCall edges (DynamicLibrary.open + lookup), found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:hello>"),
        "Expected FFI target node <ffi:hello>"
    );
}

#[test]
fn test_lookup_function_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void callNative() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
  final hello = dylib.lookupFunction<ffi.NativeFunction<ffi.Int32 Function(ffi.Int32)>,
                                      int Function(int)>('hello');
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    // Should have 2 edges: DynamicLibrary.open + lookupFunction
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected 2 FfiCall edges (DynamicLibrary.open + lookupFunction), found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:hello>"),
        "Expected FFI target node <ffi:hello>"
    );
}

// ================================
// Positive Tests - @Native Annotation
// ================================

#[test]
fn test_native_annotation_with_symbol_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

@ffi.Native<ffi.Int32 Function(ffi.Int32)>(symbol: 'add')
external int nativeAdd(int a, int b);
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @Native with symbol, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:add>"),
        "Expected FFI target node <ffi:add>"
    );
}

#[test]
fn test_native_annotation_inferred_symbol_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

@ffi.Native<ffi.Int32 Function()>()
external int getValue();
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @Native with inferred symbol, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:getValue>"),
        "Expected FFI target node <ffi:getValue> (inferred from function name)"
    );
}

// ================================
// Positive Tests - @FfiNative Annotation
// ================================

#[test]
fn test_ffi_native_annotation_creates_ffi_edge() {
    let source = r"
import 'dart:ffi' as ffi;

@ffi.FfiNative<ffi.Int32 Function(ffi.Int32)>('multiply')
external int nativeMultiply(int a, int b);
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @FfiNative, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:multiply>"),
        "Expected FFI target node <ffi:multiply>"
    );
}

#[test]
fn test_ffi_native_without_type_params_creates_ffi_edge() {
    // Test @FfiNative with arguments but no type parameters
    let source = r"
import 'dart:ffi' as ffi;

@ffi.FfiNative('subtract')
external int nativeSubtract(int a, int b);
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @FfiNative without type params, found {ffi_count}"
    );

    assert!(
        has_node_with_name(&staging, "<ffi:subtract>"),
        "Expected FFI target node <ffi:subtract>"
    );
}

// ================================
// Positive Tests - Multiple FFI Calls
// ================================

#[test]
fn test_multiple_ffi_calls_in_function() {
    let source = r"
import 'dart:ffi' as ffi;

void multipleFFI() {
  final dylib1 = ffi.DynamicLibrary.open('lib1.so');
  final dylib2 = ffi.DynamicLibrary.open('lib2.so');
  final dylib3 = ffi.DynamicLibrary.executable();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for multiple FFI calls, found {ffi_count}"
    );
}

#[test]
fn test_ffi_call_in_method() {
    let source = r"
import 'dart:ffi' as ffi;

class NativeWrapper {
  void loadLibrary() {
    final dylib = ffi.DynamicLibrary.open('libwrapper.so');
  }
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for FFI call in method, found {ffi_count}"
    );
}

#[test]
fn test_ffi_call_in_nested_function() {
    let source = r"
import 'dart:ffi' as ffi;

void outerFunction() {
  void nestedFunction() {
    final dylib = ffi.DynamicLibrary.open('libnested.so');
  }
  nestedFunction();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for FFI call in nested function, found {ffi_count}"
    );
}

// ================================
// Negative Tests
// ================================

#[test]
fn test_regular_function_no_ffi_edge() {
    let source = r"
int add(int a, int b) {
  return a + b;
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for regular function, found {ffi_count}"
    );
}

#[test]
fn test_comment_containing_native_no_edge() {
    let source = r"
// This is a native function call
void regularFunction() {
  print('hello');
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for comment containing 'native', found {ffi_count}"
    );
}

#[test]
fn test_string_containing_ffi_no_edge() {
    let source = r#"
void regularFunction() {
  print("Using dart:ffi for native calls");
}
"#;
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for string containing 'ffi', found {ffi_count}"
    );
}

#[test]
fn test_external_without_annotation_no_edge() {
    let source = r"
external int someFunction(int x);
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for external without FFI annotation, found {ffi_count}"
    );
}

#[test]
fn test_native_callable_annotation_no_ffi_edge() {
    // @NativeCallable is NOT an FFI call annotation (it marks Dart callbacks for native code)
    // This test verifies that our annotation matching is exact and doesn't match substrings
    let source = r"
import 'dart:ffi' as ffi;

@ffi.NativeCallable()
void dartCallback() {
  print('Called from native code');
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for @NativeCallable (not an FFI call), found {ffi_count}"
    );
}

#[test]
fn test_annotation_without_external_no_edge() {
    // @Native annotation on a non-external function should not create an FFI edge
    let source = r"
import 'dart:ffi' as ffi;

@ffi.Native<ffi.Int32 Function()>()
int getValue() {
  return 42;
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for @Native without external, found {ffi_count}"
    );
}

// ================================
// Edge Case Tests
// ================================

#[test]
fn test_malformed_lookup_no_panic() {
    let source = r"
import 'dart:ffi' as ffi;

void badLookup() {
  final dylib = ffi.DynamicLibrary.open('lib.so');
  final func = dylib.lookup();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Build should not panic on malformed lookup");
}

#[test]
fn test_missing_symbol_argument_no_panic() {
    let source = r"
import 'dart:ffi' as ffi;

void badLookupFunction() {
  final dylib = ffi.DynamicLibrary.open('lib.so');
  final func = dylib.lookupFunction<int Function(), int Function()>();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.dart"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Build should not panic on missing symbol argument"
    );
}

#[test]
fn test_empty_symbol_name_no_edge() {
    let source = r"
import 'dart:ffi' as ffi;

void emptySymbol() {
  final dylib = ffi.DynamicLibrary.open('lib.so');
  final func = dylib.lookup<int Function()>('').asFunction<int Function()>();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    // Should have 1 edge for DynamicLibrary.open, but NOT for empty symbol lookup
    let ffi_count = count_ffi_call_edges(&staging);
    // We expect only DynamicLibrary.open edge, not the empty symbol lookup
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge (DynamicLibrary.open only), found {ffi_count}"
    );

    // Should NOT have empty symbol FFI target
    assert!(
        !has_node_with_name(&staging, "<ffi:>"),
        "Should not create FFI edge for empty symbol name"
    );
}
