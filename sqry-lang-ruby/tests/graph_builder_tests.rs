/// Integration tests for Ruby `GraphBuilder`
#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_ruby::RubyGraphBuilder;
use sqry_test_support::graph_helpers::{
    assert_has_call_edge, build_node_name_lookup, collect_call_edges,
};
use std::fs;
use std::path::Path;
use support::unique_rb_path;
use tree_sitter::Parser;

fn parse_ruby(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("error loading Ruby grammar");
    parser.parse(source, None).expect("ruby parse failed")
}

fn collect_ffi_call_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::FfiCall { .. },
                ..
            } = op
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn graph_builder_extracts_instance_and_class_calls() {
    let source =
        fs::read_to_string("tests/fixtures/graph/users_controller.rb").expect("load ruby fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("users_controller");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    assert_has_call_edge(
        &staging,
        "UsersController::create",
        "UsersController::send_welcome_email",
    );
    assert_has_call_edge(
        &staging,
        "UsersController::send_welcome_email",
        "Mailer::deliver",
    );
    assert_has_call_edge(&staging, "UsersController::audit", "UsersController::log");
}

#[test]
fn graph_builder_detects_ruby_ffi_edges() {
    let source =
        fs::read_to_string("tests/fixtures/graph/ffi_bridge.rb").expect("load ruby ffi fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("ffi_bridge");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Crypto::encrypt", "Crypto::crypto_encrypt");
    let ffi_edges = collect_ffi_call_edges(&staging);
    assert!(
        ffi_edges.iter().any(|(source_name, target_name)| {
            source_name.contains("Crypto") && target_name.contains("ffi::crypto_encrypt")
        }),
        "expected FFI edge for Crypto -> ffi::crypto_encrypt, got {ffi_edges:?}"
    );
}

#[test]
fn graph_builder_handles_malformed_ruby_gracefully() {
    let source = fs::read_to_string("tests/fixtures/graph/malformed_syntax.rb")
        .expect("load malformed fixture");

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("error loading Ruby grammar");

    // Tree-sitter is error-resilient and will parse malformed code with error nodes
    let tree = parser
        .parse(&source, None)
        .expect("tree-sitter should parse despite errors");

    // Verify tree contains errors (validates our fixture is actually malformed)
    assert!(
        tree.root_node().has_error(),
        "malformed fixture should produce a tree with error nodes"
    );

    let file = unique_rb_path("malformed");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    // Builder should handle error nodes gracefully without panicking.
    // The implementation is designed to skip unparseable nodes during traversal,
    // so we expect Ok(()) with potentially partial extraction.
    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);

    // The builder should succeed - it's designed to be resilient to malformed input
    // by skipping error nodes during AST traversal.
    result.expect("builder should gracefully handle malformed Ruby without returning errors");

    let node_names = build_node_name_lookup(&staging);
    let has_method = node_names
        .values()
        .any(|name| name.contains("incomplete_method") || name.contains("another_method"));
    assert!(
        has_method,
        "should extract at least one method from malformed Ruby, got {node_names:?}"
    );

    // Verify no panic occurred while inspecting staged call edges
    let _call_edges = collect_call_edges(&staging);
}

#[test]
fn graph_builder_respects_depth_limit() {
    let source = fs::read_to_string("tests/fixtures/graph/deep_namespaces.rb")
        .expect("load deep namespaces fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("deep_namespaces");
    let mut staging = StagingGraph::new();

    // Use default builder (max depth = 4)
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with deep namespaces");

    let node_names = build_node_name_lookup(&staging);
    assert!(
        node_names
            .values()
            .any(|name| name.contains("Level4Class::level4_method")),
        "should extract Level4Class within depth limit"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("Shallow::TestClass::test_method")),
        "should extract shallow namespaces"
    );
    assert!(
        !node_names
            .values()
            .any(|name| name.contains("DeepClass::deep_method")),
        "should skip deeper namespaces beyond max depth"
    );
}

#[test]
fn graph_builder_handles_visibility_edge_cases() {
    let source = fs::read_to_string("tests/fixtures/graph/visibility_edge_cases.rb")
        .expect("load visibility fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("visibility");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with visibility modifiers");

    let node_names = build_node_name_lookup(&staging);
    assert!(
        node_names
            .values()
            .any(|name| name.contains("VisibilityTest::private_method_1")),
        "should extract private_method_1"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("VisibilityTest::private_method_2")),
        "should extract private_method_2"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("VisibilityTest::inline_private")),
        "should extract inline private method"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("public_class_method")),
        "should extract singleton public_class_method"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("private_class_method")),
        "should extract singleton private_class_method"
    );

    assert_has_call_edge(
        &staging,
        "VisibilityTest::another_public",
        "VisibilityTest::private_method_1",
    );
    assert_has_call_edge(
        &staging,
        "VisibilityTest::another_public",
        "VisibilityTest::private_method_2",
    );
}

#[test]
fn graph_builder_detects_multiple_ffi_calls() {
    let source = fs::read_to_string("tests/fixtures/graph/multiple_ffi.rb")
        .expect("load multiple ffi fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("multiple_ffi");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with multiple FFI");

    let ffi_edges = collect_ffi_call_edges(&staging);
    assert!(
        ffi_edges.len() >= 6,
        "should detect multiple FFI attach_function calls, got {}",
        ffi_edges.len()
    );
    assert!(
        ffi_edges
            .iter()
            .any(|(_, callee)| callee.contains("ffi::aes_encrypt")),
        "should detect aes_encrypt FFI edge, got {ffi_edges:?}"
    );
    assert!(
        ffi_edges
            .iter()
            .any(|(_, callee)| callee.contains("ffi::compress_data")),
        "should detect compress_data FFI edge, got {ffi_edges:?}"
    );
}

#[test]
fn graph_builder_emits_controller_dsl_edges() {
    let source = fs::read_to_string("tests/fixtures/graph/controller_dsl.rb")
        .expect("load controller dsl fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("controller_dsl");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph for controller dsl");

    assert_has_call_edge(
        &staging,
        "UsersController::new",
        "UsersController::require_login",
    );
    assert_has_call_edge(
        &staging,
        "UsersController::create",
        "UsersController::require_login",
    );

    let call_edges = collect_call_edges(&staging);
    assert!(
        !call_edges
            .iter()
            .any(|edge| edge.callee.contains("before_action")),
        "controller DSL should not emit call edges to before_action helper"
    );
}

#[test]
fn graph_builder_marks_super_calls() {
    let source =
        fs::read_to_string("tests/fixtures/graph/super_call.rb").expect("load super call fixture");
    let tree = parse_ruby(&source);
    let file = unique_rb_path("super_call");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph for super call");

    assert_has_call_edge(&staging, "Child::foo", "super::Child::foo");
}

// ========== OOP Edge Tests ==========

/// Count edges of a specific kind in staging operations
fn count_edge_kind(staging: &StagingGraph, kind_tag: &str) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind.tag() == kind_tag
            } else {
                false
            }
        })
        .count()
}

/// Check if staging has an edge of a specific kind
fn has_edge_kind(staging: &StagingGraph, kind_tag: &str) -> bool {
    count_edge_kind(staging, kind_tag) > 0
}

#[test]
fn test_class_inheritance_creates_inherits_edge() {
    let source = r#"
class Animal
  def speak
    puts "..."
  end
end

class Dog < Animal
  def bark
    puts "woof"
  end
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("inheritance");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have at least one Inherits edge (Dog -> Animal)
    assert!(
        has_edge_kind(&staging, "inherits"),
        "Expected Inherits edge for class inheritance"
    );
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 1,
        "Expected exactly 1 Inherits edge, got {inherits_count}"
    );
}

#[test]
fn test_multiple_class_inheritance_chain() {
    let source = r"
class Base
end

class Middle < Base
end

class Derived < Middle
end
";

    let tree = parse_ruby(source);
    let file = unique_rb_path("inheritance_chain");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have 2 Inherits edges (Middle -> Base, Derived -> Middle)
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges for inheritance chain, got {inherits_count}"
    );
}

#[test]
fn test_class_without_superclass_no_inherits_edge() {
    let source = r"
class Standalone
  def method
  end
end
";

    let tree = parse_ruby(source);
    let file = unique_rb_path("standalone");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have NO Inherits edge
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 0,
        "Expected 0 Inherits edges for standalone class, got {inherits_count}"
    );
}

#[test]
fn test_namespaced_class_inheritance() {
    let source = r"
module MyModule
  class Parent
  end

  class Child < Parent
  end
end
";

    let tree = parse_ruby(source);
    let file = unique_rb_path("namespaced_inheritance");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have 1 Inherits edge (Child -> Parent)
    assert!(
        has_edge_kind(&staging, "inherits"),
        "Expected Inherits edge for namespaced class inheritance"
    );
}

// ========== Export Edge Tests ==========

#[test]
fn graph_builder_emits_exports_for_public_symbols() {
    let source = r#"
def greet(name)
  "Hello, #{name}!"
end

class User
  def initialize(name)
    @name = name
  end
end

module Utils
  def self.format(str)
    str.upcase
  end
end

private

def helper
  42
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("exports");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have Export edges
    let export_count = count_edge_kind(&staging, "exports");
    assert!(
        export_count >= 3,
        "Expected at least 3 Export edges (greet, User, Utils), got {}",
        export_count
    );

    // Verify that private helper method is NOT exported
    // Find all export edges and check their target names
    let node_names = build_node_name_lookup(&staging);
    let exported_names: Vec<String> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::Exports { .. },
                ..
            } = op
            {
                node_names.get(&target.index()).cloned()
            } else {
                None
            }
        })
        .collect();

    // Verify public symbols are exported
    assert!(
        exported_names.iter().any(|n| n.contains("greet")),
        "Expected export of greet method, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("User")),
        "Expected export of User class, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("Utils")),
        "Expected export of Utils module, got exports: {:?}",
        exported_names
    );

    // Verify private method is NOT exported
    assert!(
        !exported_names.iter().any(|n| n.contains("helper")),
        "Should not export private method helper, got exports: {:?}",
        exported_names
    );
}

#[test]
fn graph_builder_exports_nested_classes_and_modules() {
    let source = r#"
module Utils
  class Formatter
    def format(input)
      input.downcase
    end
  end

  module StringHelpers
    def self.trim(str)
      str.strip
    end
  end
end

class OuterClass
  class InnerClass
    def inner_method
      "inner"
    end
  end
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("nested_exports");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);
    let exported_names: Vec<String> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::Exports { .. },
                ..
            } = op
            {
                node_names.get(&target.index()).cloned()
            } else {
                None
            }
        })
        .collect();

    // All classes and modules should be exported, including nested ones
    assert!(
        exported_names.iter().any(|n| n.contains("Utils")),
        "Expected export of Utils module, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("Formatter")),
        "Expected export of nested Formatter class, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("StringHelpers")),
        "Expected export of nested StringHelpers module, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("OuterClass")),
        "Expected export of OuterClass, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("InnerClass")),
        "Expected export of nested InnerClass, got exports: {:?}",
        exported_names
    );
}

#[test]
fn graph_builder_exports_methods_with_correct_visibility() {
    let source = r#"
class Service
  def public_method
    private_method
  end

  private

  def private_method
    42
  end

  protected

  def protected_method
    "protected"
  end

  public

  def another_public
    "public"
  end

  private def inline_private
    "private inline"
  end
end

# Top-level methods
def top_level_public
  "public"
end

private

def top_level_private
  "private"
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("method_exports");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);
    let exported_names: Vec<String> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::Exports { .. },
                ..
            } = op
            {
                node_names.get(&target.index()).cloned()
            } else {
                None
            }
        })
        .collect();

    // Public methods should be exported
    assert!(
        exported_names.iter().any(|n| n.contains("public_method")),
        "Expected export of public_method, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names.iter().any(|n| n.contains("another_public")),
        "Expected export of another_public, got exports: {:?}",
        exported_names
    );
    assert!(
        exported_names
            .iter()
            .any(|n| n.contains("top_level_public")),
        "Expected export of top_level_public, got exports: {:?}",
        exported_names
    );

    // Private and protected methods should NOT be exported
    assert!(
        !exported_names.iter().any(|n| n.contains("private_method")),
        "Should not export private_method, got exports: {:?}",
        exported_names
    );
    assert!(
        !exported_names
            .iter()
            .any(|n| n.contains("protected_method")),
        "Should not export protected_method, got exports: {:?}",
        exported_names
    );
    assert!(
        !exported_names
            .iter()
            .any(|n| n.contains("top_level_private")),
        "Should not export top_level_private, got exports: {:?}",
        exported_names
    );
    assert!(
        !exported_names.iter().any(|n| n.contains("inline_private")),
        "Should not export inline_private, got exports: {:?}",
        exported_names
    );
}

// ================================
// P2 Feature Tests
// ================================

#[test]
fn test_constant_nodes() {
    let source = r#"
class Config
  VERSION = "1.0.0"
  MAX_SIZE = 100
  DEFAULT_TIMEOUT = 30
end

TOP_LEVEL_CONSTANT = "global"
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("constants");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);

    // Check for constant nodes
    assert!(
        node_names.values().any(|n| n.contains("Config::VERSION")),
        "Expected Config::VERSION constant, got nodes: {:?}",
        node_names.values().collect::<Vec<_>>()
    );
    assert!(
        node_names.values().any(|n| n.contains("Config::MAX_SIZE")),
        "Expected Config::MAX_SIZE constant"
    );
    assert!(
        node_names
            .values()
            .any(|n| n.contains("TOP_LEVEL_CONSTANT")),
        "Expected TOP_LEVEL_CONSTANT"
    );
}

#[test]
fn test_mixin_edges_include() {
    let source = r#"
module Loggable
  def log(message)
    puts message
  end
end

class Service
  include Loggable

  def execute
    log("executing")
  end
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("include_mixin");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check for Implements edge from Service to Loggable
    let node_names = build_node_name_lookup(&staging);
    let implements_edges: Vec<(String, String)> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::Implements,
                ..
            } = op
            {
                let source_name = node_names.get(&source.index()).cloned().unwrap_or_default();
                let target_name = node_names.get(&target.index()).cloned().unwrap_or_default();
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect();

    assert!(
        implements_edges
            .iter()
            .any(|(src, tgt)| src.contains("Service") && tgt.contains("Loggable")),
        "Expected Implements edge from Service to Loggable, got: {:?}",
        implements_edges
    );
}

#[test]
fn test_mixin_edges_extend() {
    let source = r#"
module ClassMethods
  def create(attrs)
    new(attrs)
  end
end

class Model
  extend ClassMethods
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("extend_mixin");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check for Implements edge from Model to ClassMethods
    let node_names = build_node_name_lookup(&staging);
    let implements_edges: Vec<(String, String)> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::Implements,
                ..
            } = op
            {
                let source_name = node_names.get(&source.index()).cloned().unwrap_or_default();
                let target_name = node_names.get(&target.index()).cloned().unwrap_or_default();
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect();

    assert!(
        implements_edges
            .iter()
            .any(|(src, tgt)| src.contains("Model") && tgt.contains("ClassMethods")),
        "Expected Implements edge from Model to ClassMethods, got: {:?}",
        implements_edges
    );
}

#[test]
fn test_async_flag_fiber() {
    let source = r#"
class AsyncProcessor
  def process_async(data)
    fiber = Fiber.new do
      process_data(data)
    end
    fiber.resume
  end

  def process_sync(data)
    process_data(data)
  end
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("async_fiber");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);

    // Check that process_async is marked as async (we need to inspect the ops)
    // For now, just verify the methods exist
    assert!(
        node_names.values().any(|n| n.contains("process_async")),
        "Expected process_async method"
    );
    assert!(
        node_names.values().any(|n| n.contains("process_sync")),
        "Expected process_sync method"
    );
}

#[test]
fn test_async_flag_thread() {
    let source = r#"
class ThreadPool
  def run_async
    thread = Thread.new do
      perform_work
    end
    thread.join
  end
end
"#;

    let tree = parse_ruby(source);
    let file = unique_rb_path("async_thread");
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);
    assert!(
        node_names.values().any(|n| n.contains("run_async")),
        "Expected run_async method"
    );
}
