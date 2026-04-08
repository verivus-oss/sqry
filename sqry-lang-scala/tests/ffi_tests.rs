//! Test FFI edge detection for Scala
//!
//! Scala has three main FFI mechanisms:
//! 1. @native annotation for JNI methods (like Java's `native` keyword)
//! 2. @extern for Scala Native C interop
//! 3. JNA via traits extending com.sun.jna.Library
use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, edge::EdgeKind, node::NodeKind},
};
use sqry_lang_scala::ScalaGraphBuilder;
use std::path::Path;

fn parse_and_build_graph(source: &str) -> StagingGraph {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .expect("Failed to load Scala grammar");

    let tree = parser.parse(source, None).expect("Failed to parse Scala");
    let content = source.as_bytes();
    let mut staging = StagingGraph::new();

    let builder = ScalaGraphBuilder::new();
    builder
        .build_graph(&tree, content, Path::new("test.scala"), &mut staging)
        .expect("Failed to build graph");

    staging
}

fn count_ffi_edges(staging: &StagingGraph) -> usize {
    staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .count()
}

#[test]
fn test_native_method_creates_ffi_edge() {
    // @native annotation should create an FfiCall edge
    let source = r"
class NativeLib {
  @native def nativeMethod(): Unit
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @native method, found {ffi_count}"
    );
}

#[test]
fn test_multiple_native_methods() {
    // Multiple @native methods should each create an FfiCall edge
    let source = r"
class NativeLib {
  @native def method1(): Unit
  @native def method2(x: Int): String
  @native def method3(x: Int, y: String): Long
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for 3 @native methods, found {ffi_count}"
    );
}

#[test]
#[ignore = "Scala Native @extern support not yet implemented"]
fn test_extern_object_scala_native() {
    // @extern object for Scala Native should create FFI edges for its methods
    let source = r"
import scala.scalanative.unsafe._

@extern
object CLib {
  def printf(format: CString): CInt = extern
  def malloc(size: CSize): Ptr[Byte] = extern
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    // The @extern object itself might create an edge, plus each extern method
    assert!(
        ffi_count >= 2,
        "Expected at least 2 FfiCall edges for @extern object methods, found {ffi_count}"
    );
}

#[test]
fn test_jna_trait_extending_library() {
    // Trait extending com.sun.jna.Library indicates JNA FFI
    let source = r"
import com.sun.jna.Library

trait MyLibrary extends Library {
  def someFunction(): Unit
  def anotherFunction(x: Int): String
}
";

    let staging = parse_and_build_graph(source);

    // For JNA, we might detect the trait definition or the methods
    // At minimum, we should detect the trait as an interface
    let nodes: Vec<_> = staging.nodes().collect();
    let has_interface = nodes.iter().any(|n| {
        matches!(n.entry.kind, NodeKind::Interface)
            && staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == "MyLibrary")
    });

    assert!(
        has_interface,
        "Expected JNA trait to be detected as Interface node"
    );
}

#[test]
fn test_non_native_method_no_ffi_edge() {
    // Regular methods without @native should NOT create FfiCall edges
    let source = r#"
class RegularClass {
  def normalMethod(): Unit = {}
  def anotherMethod(x: Int): String = ""
}
"#;

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for non-native methods, found {ffi_count}"
    );
}

#[test]
fn test_native_method_with_parameters() {
    // @native method with various parameter types
    let source = r"
class NativeLib {
  @native def process(x: Int, y: String, flag: Boolean): Long
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @native method with parameters, found {ffi_count}"
    );

    // Verify the FFI target has correct JVM signature
    let ffi_edge = staging
        .edges()
        .find(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .expect("Expected at least one FfiCall edge");

    let target_node = staging
        .nodes()
        .find(|n| n.expected_id == Some(ffi_edge.target))
        .expect("Expected target node");

    let target_name = staging
        .resolve_node_name(target_node.entry)
        .expect("Expected target name");

    // Verify FFI target format: <ffi:NativeLib.process__I_Ljava/lang/String_Z>
    assert!(
        target_name.starts_with("<ffi:NativeLib.process__"),
        "Expected FFI target to start with '<ffi:NativeLib.process__', got: {target_name}"
    );
    assert!(
        target_name.contains("_I_"), // Int parameter
        "Expected FFI target to contain '_I_' for Int parameter, got: {target_name}"
    );
    assert!(
        target_name.contains("_Ljava/lang/String_") || target_name.contains("_Ljava_lang_String_"),
        "Expected FFI target to contain String descriptor, got: {target_name}"
    );
    assert!(
        target_name.contains("_Z") || target_name.ends_with("_Z>"),
        "Expected FFI target to contain '_Z' for Boolean parameter, got: {target_name}"
    );
}

#[test]
fn test_private_native_method() {
    // private @native method should still create an FfiCall edge
    let source = r"
class NativeLib {
  @native private def privateNative(): Unit
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for private @native method, found {ffi_count}"
    );
}

#[test]
fn test_native_method_in_object() {
    // @native method inside object (singleton)
    let source = r"
object NativeLib {
  @native def loadLibrary(name: String): Boolean
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @native method in object, found {ffi_count}"
    );
}

#[test]
fn test_overloaded_native_methods() {
    // Overloaded @native methods should create distinct FFI targets
    let source = r"
class NativeLib {
  @native def process(x: Int): String
  @native def process(s: String): String
  @native def process(x: Int, y: Int): String
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for 3 overloaded @native methods, found {ffi_count}"
    );

    // Verify distinct FFI targets
    let ffi_edges: Vec<_> = staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .collect();

    let ffi_targets: Vec<_> = ffi_edges
        .iter()
        .map(|e| {
            // Find the target node by iterating through nodes
            let node = staging
                .nodes()
                .find(|n| n.expected_id == Some(e.target))
                .expect("Target node not found");
            staging
                .resolve_node_name(node.entry)
                .expect("Target node name not found")
                .to_string()
        })
        .collect();

    // All FFI targets should be distinct (no collisions)
    let unique_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
    assert_eq!(
        unique_targets.len(),
        ffi_targets.len(),
        "Expected all FFI targets to be distinct, but found duplicates"
    );
}

#[test]
fn test_mixed_native_and_regular_methods() {
    // Mix of @native and regular methods
    let source = r#"
class NativeLib {
  def regularMethod(): Unit = {}
  @native def nativeMethod(): Unit
  def anotherRegular(x: Int): String = ""
  @native def anotherNative(y: Long): Boolean
}
"#;

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 2,
        "Expected 2 FfiCall edges for 2 @native methods (out of 4 total), found {ffi_count}"
    );
}

#[test]
fn test_qualified_scala_primitives() {
    // Test that scala.Int, scala.Array, etc. are handled correctly
    // Covers: primitive arrays, reference arrays, multiple primitive types
    let source = r"
class NativeLib {
  @native def process(x: scala.Int): String
  @native def process(arr: scala.Array[scala.Int]): String
  @native def process(y: scala.Long, z: scala.Boolean): String
  @native def process(strArr: scala.Array[String]): String
  @native def process(longArr: scala.Array[scala.Long]): String
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 5,
        "Expected 5 FfiCall edges for qualified primitive types + arrays, found {ffi_count}"
    );

    // Verify distinct FFI targets (qualified types should normalize correctly)
    let ffi_edges: Vec<_> = staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .collect();

    let ffi_targets: Vec<_> = ffi_edges
        .iter()
        .map(|e| {
            let node = staging
                .nodes()
                .find(|n| n.expected_id == Some(e.target))
                .expect("Target node not found");
            staging
                .resolve_node_name(node.entry)
                .expect("Target node name not found")
                .to_string()
        })
        .collect();

    // All FFI targets should be distinct
    let unique_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
    assert_eq!(
        unique_targets.len(),
        ffi_targets.len(),
        "Expected all FFI targets to be distinct, but found duplicates: {ffi_targets:?}"
    );

    // Verify at least one target contains primitive descriptor (I for Int)
    assert!(
        ffi_targets.iter().any(|t| t.contains("__I")),
        "Expected at least one FFI target to contain '__I' for scala.Int, got: {ffi_targets:?}"
    );

    // Verify array descriptor for scala.Array[scala.Int] → [I
    assert!(
        ffi_targets.iter().any(|t| t.contains("__[I")),
        "Expected at least one FFI target to contain '__[I' for scala.Array[scala.Int], got: {ffi_targets:?}"
    );

    // Verify reference array descriptor for scala.Array[String] → [Ljava/lang/String;
    assert!(
        ffi_targets
            .iter()
            .any(|t| t.contains("__[Ljava/lang/String") || t.contains("__[Ljava_lang_String")),
        "Expected at least one FFI target to contain String array descriptor, got: {ffi_targets:?}"
    );

    // Verify Long array descriptor for scala.Array[scala.Long] → [J
    assert!(
        ffi_targets.iter().any(|t| t.contains("__[J")),
        "Expected at least one FFI target to contain '__[J' for scala.Array[scala.Long], got: {ffi_targets:?}"
    );
}

#[test]
fn test_qualified_native_annotation() {
    // Test that @scala.native is recognized
    let source = r"
class NativeLib {
  @scala.native def qualifiedNative(): Unit
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for @scala.native annotation, found {ffi_count}"
    );
}

#[test]
fn test_curried_parameters() {
    // Test curried function parameters (multiple parameter lists)
    let source = r"
class NativeLib {
  @native def curried(x: Int)(y: String): Long
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for curried @native method, found {ffi_count}"
    );

    // Verify the FFI target includes both parameter lists in signature
    let ffi_edge = staging
        .edges()
        .find(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .expect("Expected at least one FfiCall edge");

    let target_node = staging
        .nodes()
        .find(|n| n.expected_id == Some(ffi_edge.target))
        .expect("Expected target node");

    let target_name = staging
        .resolve_node_name(target_node.entry)
        .expect("Expected target name");

    // Should contain both Int (I) and String descriptors
    assert!(
        target_name.contains("_I_") || target_name.contains("__I_"),
        "Expected FFI target to contain Int parameter descriptor, got: {target_name}"
    );
    assert!(
        target_name.contains("_Ljava/lang/String") || target_name.contains("_Ljava_lang_String"),
        "Expected FFI target to contain String parameter descriptor, got: {target_name}"
    );
}

#[test]
fn test_anyval_anyref_normalization() {
    // Test that scala.AnyVal, scala.AnyRef, and scala.Any are normalized correctly
    // The normalization rule uses starts_with("Any"), so all three should be normalized
    let source = r"
class NativeLib {
  @native def processAny(x: scala.Any): String
  @native def processAnyRef(x: scala.AnyRef): String
  @native def processAnyVal(x: scala.AnyVal): String
}
";

    let staging = parse_and_build_graph(source);
    let ffi_count = count_ffi_edges(&staging);

    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for Any types, found {ffi_count}"
    );

    // Verify distinct FFI targets (all should normalize to unqualified forms)
    let ffi_edges: Vec<_> = staging
        .edges()
        .filter(|e| matches!(e.kind, EdgeKind::FfiCall { .. }))
        .collect();

    let ffi_targets: Vec<_> = ffi_edges
        .iter()
        .map(|e| {
            let node = staging
                .nodes()
                .find(|n| n.expected_id == Some(e.target))
                .expect("Target node not found");
            staging
                .resolve_node_name(node.entry)
                .expect("Target node name not found")
                .to_string()
        })
        .collect();

    // All FFI targets should be distinct
    let unique_targets: std::collections::HashSet<_> = ffi_targets.iter().collect();
    assert_eq!(
        unique_targets.len(),
        ffi_targets.len(),
        "Expected all FFI targets to be distinct, but found duplicates: {ffi_targets:?}"
    );

    // Verify that Any types are normalized (not kept as scala.Any)
    // They should map to Object descriptors: Ljava/lang/Object;
    for target in &ffi_targets {
        assert!(
            target.contains("Ljava/lang/Object") || target.contains("Ljava_lang_Object"),
            "Expected FFI target to contain Object descriptor for Any types, got: {target}"
        );
    }
}
