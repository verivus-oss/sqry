//! Integration tests for TypeOf and Reference edge creation from YARD annotations
//!
//! Tests validate that edges are created for correct YARD patterns and include
//! nested namespace tests to verify full qualified name handling (Issue #1 fix).

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::build::test_helpers::collect_edges_by_kind;
use sqry_lang_ruby::RubyGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_ruby(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("error loading Ruby grammar");
    parser.parse(source, None).expect("ruby parse failed")
}

/// Helper to build graph from Ruby source
fn build_graph_from_source(source: &str) -> StagingGraph {
    let builder = RubyGraphBuilder::default();
    let path = Path::new("test.rb");
    let tree = parse_ruby(source);
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, source.as_bytes(), path, &mut staging)
        .expect("Failed to build graph");
    staging
}

/// Helper to get node name from staging operations
/// Maps StringId to actual string value by looking up InternString operations
fn get_node_name(
    staging: &StagingGraph,
    node_id: sqry_core::graph::unified::NodeId,
) -> Option<String> {
    use sqry_core::graph::unified::build::staging::StagingOp;

    // Build a map of StringId -> String from InternString operations
    let mut string_map = std::collections::HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            string_map.insert(*local_id, value.clone());
        }
    }

    // Find the AddNode operation for this NodeId
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(id),
        } = op
            && *id == node_id
        {
            // Look up the name from the string map
            return string_map.get(&entry.name).cloned();
        }
    }
    None
}

/// Assert that an edge exists from a node with the given qualified name
/// This validates that YARD edges attach to the correct nodes with proper qualification
fn assert_edge_from_node(staging: &StagingGraph, edge_kind: &str, expected_from: &str) -> bool {
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target: _,
            kind,
            ..
        } = op
        {
            let matches_kind = matches!(
                (edge_kind, kind),
                ("TypeOf", EdgeKind::TypeOf { .. }) | ("References", EdgeKind::References)
            );

            if matches_kind
                && let Some(from_name) = get_node_name(staging, *source)
                && from_name == expected_from
            {
                return true;
            }
        }
    }
    false
}

/// Count edges originating from a specific node (by qualified name)
/// This validates edge count for a specific qualified symbol
fn count_edges_from_node(staging: &StagingGraph, edge_kind: &str, from_node: &str) -> usize {
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;

    let mut count = 0;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target: _,
            kind,
            ..
        } = op
        {
            let matches_kind = matches!(
                (edge_kind, kind),
                ("TypeOf", EdgeKind::TypeOf { .. }) | ("References", EdgeKind::References)
            );

            if matches_kind
                && let Some(from_name) = get_node_name(staging, *source)
                && from_name == from_node
            {
                count += 1;
            }
        }
    }
    count
}

// ============================================================================
// Method @param Tests
// ============================================================================

#[test]
fn test_method_param_simple_type() {
    let source = r#"
class User
  # @param [String] name The user's name
  def greet(name)
    puts "Hello, #{name}"
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check that TypeOf edges were created
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Method should have TypeOf edges from @param"
    );

    // Check that References edges were created
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(!ref_edges.is_empty(), "Method should have References edges");
}

#[test]
fn test_method_param_multiple_params() {
    let source = r#"
class User
  # @param [String] first_name
  # @param [String] last_name
  # @param [Integer] age
  def create(first_name, last_name, age)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edges created for all parameters
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 3,
        "Method should have TypeOf edges for all 3 parameters"
    );
}

#[test]
fn test_method_param_custom_type() {
    let source = r#"
class Service
  # @param [User] user The user object
  def process(user)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for custom types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Method should have References edges for custom types"
    );
}

#[test]
fn test_method_param_union_type() {
    let source = r#"
class Parser
  # @param [String, Integer] value
  def parse(value)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for both union types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Method should have References edges for both union types"
    );
}

#[test]
fn test_method_param_nullable_type() {
    let source = r#"
class Service
  # @param [String, nil] name
  def greet(name)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edge (nil should be stripped)
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Method should have TypeOf edge with nil stripped"
    );
}

// ============================================================================
// Method @return Tests
// ============================================================================

#[test]
fn test_method_return_simple_type() {
    let source = r#"
class User
  # @return [String] The user's name
  def name
    @name
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edge for return type
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Method should have TypeOf edge for return type"
    );
}

#[test]
fn test_method_return_custom_type() {
    let source = r#"
class UserFactory
  # @return [User] The created user
  def create
    User.new
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edge for custom return type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Method should have References edge for custom return type"
    );
}

#[test]
fn test_method_return_array_type() {
    let source = r#"
class Repository
  # @return [Array<User>] List of users
  def all
    User.all
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Array and User
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Method should have References edges for Array and User"
    );
}

#[test]
fn test_method_return_hash_type() {
    let source = r#"
class Config
  # @return [Hash{String => Integer}] Configuration mapping
  def settings
    { timeout: 30, retries: 3 }
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Hash, String, and Integer
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Method should have References edges for Hash, String, and Integer"
    );
}

#[test]
fn test_method_return_nullable_type() {
    let source = r#"
class Finder
  # @return [User, nil] User or nil if not found
  def find(id)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check that edges are created (nil should be excluded from References)
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Method should have TypeOf edge with nil stripped"
    );
}

// ============================================================================
// Singleton Method Tests
// ============================================================================

#[test]
fn test_singleton_method_param() {
    let source = r#"
class User
  # @param [String] name
  # @return [User]
  def self.create(name)
    new(name)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edges for singleton method
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 2,
        "Singleton method should have TypeOf edges for param and return"
    );
}

#[test]
fn test_singleton_method_return() {
    let source = r#"
class Config
  # @return [Hash{Symbol => String}]
  def self.defaults
    { timeout: '30s' }
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for all types in the hash
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Singleton method should have References edges for Hash, Symbol, and String"
    );
}

#[test]
fn test_singleton_method_multiple_params() {
    let source = r#"
class Builder
  # @param [String] name
  # @param [Integer] age
  # @param [Boolean] active
  # @return [User]
  def self.build(name, age, active)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check that multiple TypeOf edges were created
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 4,
        "Singleton method should have TypeOf edges for all params and return"
    );
}

// ============================================================================
// attr_reader/attr_writer/attr_accessor Tests
// ============================================================================

#[test]
fn test_attr_reader_single() {
    let source = r#"
class User
  # @return [String]
  attr_reader :name
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edge for attr_reader
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "attr_reader should create TypeOf edge"
    );
}

#[test]
fn test_attr_reader_multiple() {
    let source = r#"
class User
  # @return [String]
  attr_reader :first_name, :last_name
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edges for both attributes
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 2,
        "attr_reader should create TypeOf edges for multiple attributes"
    );
}

#[test]
fn test_attr_writer_custom_type() {
    let source = r#"
class Service
  # @return [Logger]
  attr_writer :logger
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edge for custom type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "attr_writer should create References edge for custom type"
    );
}

#[test]
fn test_attr_accessor_array_type() {
    let source = r#"
class Repository
  # @return [Array<User>]
  attr_accessor :users
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Array and User
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "attr_accessor should create References edges for Array and User"
    );
}

#[test]
fn test_attr_accessor_hash_type() {
    let source = r#"
class Cache
  # @return [Hash{String => Object}]
  attr_accessor :data
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Hash, String, and Object
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "attr_accessor should create References edges for Hash, String, and Object"
    );
}

// ============================================================================
// attr_* with string arguments (Issue #3 fix)
// ============================================================================

#[test]
fn test_attr_reader_string_argument() {
    let source = r#"
class User
  # @return [String]
  attr_reader "username"
end
"#;

    let staging = build_graph_from_source(source);

    // Validate TypeOf edge originates from the correctly qualified attr
    let qualified_attr = "User#username";
    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_attr),
        "TypeOf edge should originate from attr with string argument: {}",
        qualified_attr
    );

    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "attr_reader should have References edge for String type"
    );
}

#[test]
fn test_attr_accessor_command_call() {
    let source = r#"
class Service
  # @return [Logger]
  self.attr_accessor :logger
end
"#;

    let staging = build_graph_from_source(source);

    // Validate TypeOf edge originates from the correctly qualified attr (command_call form)
    let qualified_attr = "Service#logger";
    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_attr),
        "TypeOf edge should originate from attr with command_call form: {}",
        qualified_attr
    );

    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "attr_accessor should have References edge for Logger type"
    );
}

// ============================================================================
// Instance Variable @type Tests
// ============================================================================

#[test]
fn test_instance_variable_type() {
    let source = r#"
class User
  def initialize
    # @type [String]
    @name = "John"
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check TypeOf edge for instance variable
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Instance variable should have TypeOf edge from @type"
    );
}

#[test]
fn test_instance_variable_custom_type() {
    let source = r#"
class Service
  def setup
    # @type [Logger]
    @logger = Logger.new
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edge for custom type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Instance variable should have References edge for custom type"
    );
}

#[test]
fn test_instance_variable_array_type() {
    let source = r#"
class Repository
  def initialize
    # @type [Array<User>]
    @users = []
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Array and User
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Instance variable should have References edges for Array and User"
    );
}

#[test]
fn test_instance_variable_hash_type() {
    let source = r#"
class Config
  def initialize
    # @type [Hash{Symbol => String}]
    @settings = {}
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for Hash, Symbol, and String
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Instance variable should have References edges for Hash, Symbol, and String"
    );
}

#[test]
fn test_instance_variable_nullable_type() {
    let source = r#"
class Finder
  def initialize
    # @type [User, nil]
    @cached_user = nil
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check that edges are created (nil should be excluded)
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Instance variable should have TypeOf edge with nil stripped"
    );
}

// ============================================================================
// Complex Type Tests
// ============================================================================

#[test]
fn test_complex_generic_type() {
    let source = r#"
class Service
  # @param [Collection<Result<Data>>] results
  def process(results)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for all nested types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Method should have References edges for Collection, Result, and Data"
    );
}

#[test]
fn test_complex_union_with_generics() {
    let source = r#"
class Parser
  # @param [Array<String>, Hash{Symbol => Integer}] value
  def parse(value)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for all types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 5,
        "Method should have References edges for Array, String, Hash, Symbol, and Integer"
    );
}

#[test]
fn test_qualified_type_names() {
    let source = r#"
class Service
  # @param [App::Models::User] user
  # @return [App::Services::Logger]
  def log_action(user)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check References edges for all namespace components
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Method should have References edges for all namespace components"
    );
}

#[test]
fn test_multiple_annotations_on_method() {
    let source = r#"
class UserService
  # @param [String] first_name
  # @param [String] last_name
  # @param [Integer] age
  # @param [Hash{Symbol => String}] metadata
  # @return [User]
  def create_user(first_name, last_name, age, metadata)
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Check multiple TypeOf edges
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 5,
        "Method should have TypeOf edges for all params and return"
    );

    // Check multiple References edges
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Method should have References edges for all types"
    );
}

// ============================================================================
// Nested Namespace Tests (Issue #1 fix - CRITICAL)
// ============================================================================

#[test]
fn test_nested_module_class_method() {
    let source = r#"
module MyModule
  class MyClass
    # @param [String] value
    # @return [Integer]
    def process(value)
    end
  end
end
"#;

    let staging = build_graph_from_source(source);

    // CRITICAL: This test validates that YARD edges use full qualified names
    // (MyModule::MyClass#process) matching the ASTGraph's method nodes

    // Validate TypeOf edges originate from the fully-qualified method
    let qualified_method = "MyModule::MyClass#process";
    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_method),
        "TypeOf edge should originate from fully-qualified method: {}",
        qualified_method
    );

    // Validate edge count for the qualified method
    let typeof_count = count_edges_from_node(&staging, "TypeOf", qualified_method);
    assert!(
        typeof_count >= 2,
        "Method should have 2+ TypeOf edges (param + return), found {}",
        typeof_count
    );

    // Validate References edges exist
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Method should have References edges for String and Integer types"
    );
}

#[test]
fn test_nested_module_singleton_method() {
    let source = r#"
module Outer
  module Inner
    class Service
      # @param [User] user
      # @return [Result]
      def self.process(user)
      end
    end
  end
end
"#;

    let staging = build_graph_from_source(source);

    // CRITICAL: Validates full qualification for nested singleton methods
    // (Outer::Inner::Service.process)
    let qualified_method = "Outer::Inner::Service.process";

    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_method),
        "TypeOf edge should originate from fully-qualified singleton method: {}",
        qualified_method
    );

    let typeof_count = count_edges_from_node(&staging, "TypeOf", qualified_method);
    assert!(
        typeof_count >= 2,
        "Singleton method should have 2+ TypeOf edges (param + return), found {}",
        typeof_count
    );

    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Method should have References edges for User and Result types"
    );
}

#[test]
fn test_nested_module_attr() {
    let source = r#"
module App
  module Models
    class User
      # @return [String]
      attr_reader :username
    end
  end
end
"#;

    let staging = build_graph_from_source(source);

    // CRITICAL: Validates full qualification for nested attr_*
    // (App::Models::User#username)
    let qualified_attr = "App::Models::User#username";

    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_attr),
        "TypeOf edge should originate from fully-qualified attr: {}",
        qualified_attr
    );

    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Nested attr should have References edge for String type"
    );
}

#[test]
fn test_nested_module_instance_variable() {
    let source = r#"
module Services
  class Cache
    def initialize
      # @type [Hash{String => Object}]
      @data = {}
    end
  end
end
"#;

    let staging = build_graph_from_source(source);

    // CRITICAL: Validates full qualification for nested instance variables
    // (Services::Cache#@data)
    let qualified_var = "Services::Cache#@data";

    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_var),
        "TypeOf edge should originate from fully-qualified instance variable: {}",
        qualified_var
    );

    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Nested instance variable should have References edges for Hash, String, Object"
    );
}

#[test]
fn test_absolute_constant_namespace() {
    let source = r#"
module Outer
  class ::AbsoluteClass
    # @param [String] value
    def method(value)
    end
  end
end
"#;

    let staging = build_graph_from_source(source);

    // CRITICAL: Validates absolute constant handling (::AbsoluteClass)
    // Should NOT include Outer:: prefix since :: makes it root-qualified
    let qualified_method = "AbsoluteClass#method";

    assert!(
        assert_edge_from_node(&staging, "TypeOf", qualified_method),
        "TypeOf edge should use absolute constant name (no Outer:: prefix): {}",
        qualified_method
    );
}

// ============================================================================
// Negative Tests (No False Positives)
// ============================================================================

#[test]
fn test_no_yard_comment_no_edges() {
    let source = r#"
class User
  def greet(name)
    puts "Hello, #{name}"
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Should NOT create TypeOf edges without YARD comments
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.is_empty(),
        "Should not create TypeOf edges without YARD comments"
    );
}

#[test]
fn test_yard_comment_too_far_away() {
    let source = r#"
class User
  # @param [String] name


  def greet(name)
    puts "Hello, #{name}"
  end
end
"#;

    let staging = build_graph_from_source(source);

    // Should NOT create edges if YARD comment is > 1 blank line away
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.is_empty(),
        "Should not create TypeOf edges if YARD comment is too far away"
    );
}
