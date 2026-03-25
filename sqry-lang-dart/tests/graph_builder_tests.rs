//! Graph builder tests for the Dart language plugin.
//!
//! Covers:
//! - Function/method node extraction
//! - Class node extraction
//! - Call edge detection
//! - Import edge detection
//! - Async call detection
//! - Flutter widget extraction
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_dart::relations::DartGraphBuilder;
use std::path::Path;

fn parse_dart(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_dart::language())
        .expect("failed to set Dart language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Dart code")
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
    let source = r"
void greet(String name) {
  print('Hello, $name!');
}

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
    let source = r"
class Animal {
  String name;

  Animal(this.name);

  void speak() {
    print('$name makes a sound');
  }
}

class Dog extends Animal {
  Dog(String name) : super(name);

  @override
  void speak() {
    print('$name barks');
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
            Path::new("animals.dart"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 class nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        has_node_of_kind(&staging, NodeKind::Class),
        "Expected Class nodes"
    );
}

#[test]
fn test_function_nodes_have_function_kind() {
    let source = r"
void doWork() {
  // implementation
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

    assert!(
        has_node_of_kind(&staging, NodeKind::Function),
        "Expected at least one Function-kind node"
    );
}

// ==================== Call Edge Detection ====================

#[test]
fn test_call_edge_detection() {
    let source = r"
void helper() {
  print('helper called');
}

void main() {
  helper();
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
class Calculator {
  int add(int a, int b) {
    return a + b;
  }

  int compute(int x) {
    return add(x, 10);
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
            Path::new("calc.dart"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 method nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Async Detection ====================

#[test]
fn test_async_function_detection() {
    let source = r"
Future<String> fetchData() async {
  await Future.delayed(Duration(seconds: 1));
  return 'data';
}

Future<void> processAsync() async {
  final result = await fetchData();
  print(result);
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("async.dart"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 async function nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        has_interned_string_containing(&staging, "fetchData")
            || has_interned_string_containing(&staging, "processAsync"),
        "Expected async function names"
    );
}

// ==================== Import Edge Detection ====================

#[test]
fn test_import_dart_package() {
    let source = r#"
import 'dart:math';
import 'dart:async';
import 'package:flutter/material.dart';

void main() {
  final r = Random();
  print(r.nextInt(10));
}
"#;
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.dart"),
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
fn test_import_local_file() {
    let source = r"
import 'models/user.dart';
import 'services/api_service.dart';

void main() {}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.dart"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for local files, got {}",
        import_count
    );
}

// ==================== Flutter Widget Detection ====================

#[test]
fn test_stateless_widget_extraction() {
    let source = r"
import 'package:flutter/material.dart';

class MyWidget extends StatelessWidget {
  const MyWidget({Key? key}) : super(key: key);

  @override
  Widget build(BuildContext context) {
    return Container(
      child: Text('Hello'),
    );
  }
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("widget.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Flutter StatelessWidget should succeed");

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node for widget class, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_stateful_widget_extraction() {
    let source = r"
import 'package:flutter/material.dart';

class CounterWidget extends StatefulWidget {
  const CounterWidget({Key? key}) : super(key: key);

  @override
  State<CounterWidget> createState() => _CounterWidgetState();
}

class _CounterWidgetState extends State<CounterWidget> {
  int _count = 0;

  @override
  Widget build(BuildContext context) {
    return Text('$_count');
  }
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("counter.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Flutter StatefulWidget should succeed");
}

// ==================== Private Visibility ====================

#[test]
fn test_private_visibility_underscore() {
    let source = r"
void _privateFunction() {
  print('private');
}

void publicFunction() {
  _privateFunction();
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("visibility.dart"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected both private and public functions, got {}",
        stats.nodes_staged
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = DartGraphBuilder::new();
    assert_eq!(builder.language(), Language::Dart);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DartGraphBuilder>();
}

#[test]
fn test_builder_with_custom_scope_depth() {
    let builder = DartGraphBuilder::with_max_scope_depth(2);
    assert_eq!(builder.language(), Language::Dart);
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Dart file should succeed");

    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should produce no nodes");
}

#[test]
fn test_malformed_incomplete_dart() {
    // Incomplete Dart - tree-sitter is error-tolerant
    let source = r"
class Broken {
  void method(
"; // incomplete
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.dart"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
// This is just a comment
/* Another comment */
/// Dart doc comment
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Dart file should succeed");
}

// ==================== Cascade Notation ====================

#[test]
fn test_cascade_notation() {
    let source = r"
class Builder {
  String name = '';
  int value = 0;

  Builder setName(String n) {
    name = n;
    return this;
  }

  Builder setValue(int v) {
    value = v;
    return this;
  }
}

void main() {
  var b = Builder()
    ..setName('test')
    ..setValue(42);
}
";
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("cascade.dart"),
        &mut staging,
    );
    assert!(result.is_ok(), "Cascade notation should succeed");
}
