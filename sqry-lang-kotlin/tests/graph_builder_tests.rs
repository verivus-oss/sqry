//! Graph builder tests for the Kotlin language plugin.
//!
//! Covers:
//! - Class/object/interface node extraction
//! - Function/method node extraction
//! - Call edge detection
//! - Import edge detection
//! - OOP edges (inheritance, interface implementation)
//! - Suspend function detection
//! - Extension function extraction
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_kotlin::relations::KotlinGraphBuilder;
use std::path::Path;

fn parse_kotlin(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_sqry::language())
        .expect("failed to set Kotlin language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Kotlin code")
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
fn test_basic_function_extraction() {
    let source = r#"
fun greet(name: String): String {
    return "Hello, $name!"
}

fun add(a: Int, b: Int): Int {
    return a + b
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
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
fn test_class_extraction() {
    let source = r#"
class Animal(val name: String) {
    fun speak() {
        println("$name makes a sound")
    }
}

data class User(val id: Int, val name: String)

object Singleton {
    fun getInstance() = this
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("classes.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 nodes (class, data class, object), got {}",
        stats.nodes_staged
    );
    assert!(
        has_node_of_kind(&staging, NodeKind::Class),
        "Expected Class nodes"
    );
}

#[test]
fn test_interface_extraction() {
    let source = r"
interface Drawable {
    fun draw()
    fun resize(factor: Double)
}

interface Printable {
    fun print()
}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("interfaces.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 interface nodes, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_function_nodes_have_function_kind() {
    let source = r"
fun standalone() {}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
        .unwrap();

    assert!(
        has_node_of_kind(&staging, NodeKind::Function),
        "Expected at least one Function-kind node"
    );
}

// ==================== Call Edge Detection ====================

#[test]
fn test_call_edge_detection() {
    let source = r#"
fun helper() {
    println("helper called")
}

fun main() {
    helper()
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge, got {}",
        call_count
    );
}

#[test]
fn test_method_call_in_class() {
    let source = r#"
class Service {
    fun process() {
        helper()
    }

    private fun helper() {
        println("helping")
    }
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("service.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected class + method nodes, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_nested_calls() {
    let source = r"
fun level3(): Int = 1
fun level2(): Int = level3() * 2
fun level1(): Int = level2() + 1
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
        .unwrap();

    let call_count = count_call_edges(&staging);
    // level2 calls level3, level1 calls level2 → exactly 2 call edges
    assert_eq!(
        call_count, 2,
        "Expected exactly 2 call edges (level2→level3, level1→level2), got {}",
        call_count
    );
}

// ==================== Import Edge Detection ====================

#[test]
fn test_import_statements() {
    let source = r"
import kotlin.math.sqrt
import kotlin.collections.HashMap
import java.util.Date

fun compute(x: Double): Double {
    return sqrt(x)
}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("compute.kt"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge, got {}",
        import_count
    );
}

#[test]
fn test_wildcard_import() {
    let source = r"
import kotlin.math.*

fun compute(x: Double): Double = sqrt(x)
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 wildcard import edge, got {}",
        import_count
    );
}

#[test]
fn test_no_imports_code_only() {
    let source = r"
fun foo() {}
fun bar(x: Int) = x * 2
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 0, "No imports should produce no import edges");
}

// ==================== OOP Edges ====================

#[test]
fn test_inheritance_detection() {
    let source = r"
open class Base {
    open fun method() {}
}

class Derived : Base() {
    override fun method() {}
}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("inherit.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least Base and Derived class nodes"
    );

    let has_oop_edge = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge { kind, .. } = op {
            matches!(kind, EdgeKind::Inherits | EdgeKind::Implements)
        } else {
            false
        }
    });
    assert!(
        has_oop_edge,
        "Expected Inherits or Implements edge for class inheritance"
    );
}

#[test]
fn test_interface_implementation() {
    let source = r"
interface Serializable {
    fun serialize(): String
}

class User(val name: String) : Serializable {
    override fun serialize() = name
}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("user.kt"), &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least interface + class nodes"
    );
    // User implements Serializable: must produce an Implements edge
    let has_implements = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge { kind, .. } = op {
            matches!(kind, EdgeKind::Implements)
        } else {
            false
        }
    });
    assert!(
        has_implements,
        "Expected Implements edge for interface implementation"
    );
}

// ==================== Suspend Functions ====================

#[test]
fn test_suspend_function_detection() {
    let source = r#"
import kotlinx.coroutines.*

suspend fun fetchData(): String {
    delay(1000)
    return "data"
}

suspend fun processAsync() {
    val result = fetchData()
    println(result)
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("async.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 suspend function nodes, got {}",
        stats.nodes_staged
    );
    // Both suspend functions must be present — not just one of them
    assert!(
        has_interned_string_containing(&staging, "fetchData"),
        "Expected 'fetchData' suspend function to be staged"
    );
    assert!(
        has_interned_string_containing(&staging, "processAsync"),
        "Expected 'processAsync' suspend function to be staged"
    );
}

// ==================== Extension Functions ====================

#[test]
fn test_extension_function_extraction() {
    let source = r#"
fun String.shout(): String = this.uppercase() + "!"

fun Int.isPositive(): Boolean = this > 0

fun main() {
    println("hello".shout())
    println(5.isPositive())
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("extensions.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected extension function nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Data Classes ====================

#[test]
fn test_data_class_extraction() {
    let source = r"
data class Point(val x: Int, val y: Int)

data class Person(
    val name: String,
    val age: Int,
    val email: String
)
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("models.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 data class nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Sealed Classes ====================

#[test]
fn test_sealed_class_extraction() {
    let source = r"
sealed class Result<out T> {
    data class Success<T>(val value: T) : Result<T>()
    data class Error(val message: String) : Result<Nothing>()
    object Loading : Result<Nothing>()
}
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("result.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected sealed class + subclass nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = KotlinGraphBuilder::new();
    assert_eq!(builder.language(), Language::Kotlin);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<KotlinGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.kt"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Kotlin file should succeed");

    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should produce no nodes");
}

#[test]
fn test_malformed_kotlin() {
    // Incomplete Kotlin - tree-sitter is error-tolerant
    let source = r"
class Broken {
    fun method(
"; // incomplete
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.kt"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
// This is a comment
/* Block comment */
/** KDoc comment */
";
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.kt"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Kotlin file should succeed");
}

// ==================== Package Declaration ====================

#[test]
fn test_package_declaration() {
    let source = r#"
package com.example.service

import java.util.Date

class UserService {
    fun getUser(id: Int): String {
        return "User$id"
    }
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("UserService.kt"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}
