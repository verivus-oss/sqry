//! Tests for TypeOf and Reference edges in Elixir language plugin.
//!
//! This test suite validates that the Elixir graph builder correctly extracts:
//! - TypeOf edges: Full type signatures from @spec annotations
//! - Reference edges: Individual type names from nested type expressions
//!
//! Test organization:
//! 1. Simple Types (5 tests) - basic type annotations
//! 2. Parameters (4 tests) - function parameter types
//! 3. Return Types (4 tests) - function return type annotations
//! 4. Module-Qualified Types (4 tests) - String.t(), Enum.t(), etc.
//! 5. Complex Types (3 tests) - tuples, unions, nested types
//! 6. Integration (3 tests) - multiple specs, mixed types
//! 7. Edge Cases (2 tests) - missing specs, malformed types

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::StagingOp;
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_lang_elixir::relations::ElixirGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

// ============================================================================
// Test Helpers
// ============================================================================

fn parse_elixir(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_elixir::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to set language");
    parser.parse(source, None).expect("Failed to parse")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();
    let path = Path::new(filename);

    builder
        .build_graph(&tree, source.as_bytes(), path, &mut staging)
        .expect("Failed to build graph");

    staging
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    let string_lookup = build_string_lookup(staging);
    let mut edges = Vec::new();

    // First collect edge source/target IDs
    let mut typeof_edges = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::TypeOf {
                    context: edge_context,
                    ..
                },
            source,
            target,
            ..
        } = op
            && edge_context == &Some(context)
        {
            typeof_edges.push((*source, *target));
        }
    }

    // Find node names for each edge
    for (source_id, target_id) in typeof_edges {
        let mut source_name = String::new();
        let mut target_name = String::new();

        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                if let Some(exp_id) = expected_id
                    && *exp_id == source_id
                    && let Some(name) = string_lookup.get(&entry.name.index())
                {
                    source_name = name.clone();
                }
                if let Some(exp_id) = expected_id
                    && *exp_id == target_id
                    && let Some(name) = string_lookup.get(&entry.name.index())
                {
                    target_name = name.clone();
                }
            }
        }

        edges.push((source_name, target_name));
    }

    edges
}

fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let string_lookup = build_string_lookup(staging);
    let mut edges = Vec::new();

    // First collect edge source/target IDs
    let mut reference_edges = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::References,
            source,
            target,
            ..
        } = op
        {
            reference_edges.push((*source, *target));
        }
    }

    // Find node names for each edge
    for (source_id, target_id) in reference_edges {
        let mut source_name = String::new();
        let mut target_name = String::new();

        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                if let Some(exp_id) = expected_id
                    && *exp_id == source_id
                    && let Some(name) = string_lookup.get(&entry.name.index())
                {
                    source_name = name.clone();
                }
                if let Some(exp_id) = expected_id
                    && *exp_id == target_id
                    && let Some(name) = string_lookup.get(&entry.name.index())
                {
                    target_name = name.clone();
                }
            }
        }

        edges.push((source_name, target_name));
    }

    edges
}

// ============================================================================
// Category 1: Simple Types (5 tests)
// ============================================================================

#[test]
fn test_simple_builtin_type() {
    let source = r#"
defmodule Test do
  @spec get_count() :: integer()
  def get_count, do: 42
end
"#;
    let staging = build_test_graph(source, "test.ex");

    // Should have TypeOf edge from get_count to integer()
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_count" && to == "integer()"),
        "Expected TypeOf edge from get_count to integer(), got: {:?}",
        return_edges
    );

    // Should have Reference edge to integer
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_count" && to == "integer"),
        "Expected Reference edge to integer, got: {:?}",
        ref_edges
    );
}

#[test]
fn test_atom_type() {
    let source = r#"
defmodule Test do
  @spec get_status() :: atom()
  def get_status, do: :ok
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_status" && to == "atom()"),
        "Expected TypeOf edge to atom()"
    );
}

#[test]
fn test_boolean_type() {
    let source = r#"
defmodule Test do
  @spec is_valid() :: boolean()
  def is_valid, do: true
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "is_valid" && to == "boolean()"),
        "Expected TypeOf edge to boolean()"
    );
}

#[test]
fn test_binary_type() {
    let source = r#"
defmodule Test do
  @spec get_data() :: binary()
  def get_data, do: <<1, 2, 3>>
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_data" && to == "binary()"),
        "Expected TypeOf edge to binary()"
    );
}

#[test]
fn test_any_type() {
    let source = r#"
defmodule Test do
  @spec get_value() :: any()
  def get_value, do: nil
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_value" && to == "any()"),
        "Expected TypeOf edge to any()"
    );
}

// ============================================================================
// Category 2: Parameters (4 tests)
// ============================================================================

#[test]
fn test_single_parameter() {
    let source = r#"
defmodule Test do
  @spec greet(String.t()) :: String.t()
  def greet(name), do: "Hello, #{name}"
end
"#;
    let staging = build_test_graph(source, "test.ex");

    // Check parameter TypeOf edge
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        param_edges
            .iter()
            .any(|(from, to)| from == "greet" && to == "String.t()"),
        "Expected TypeOf edge for parameter, got: {:?}",
        param_edges
    );

    // Check Reference edge to String
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "greet" && to == "String"),
        "Expected Reference edge to String"
    );
}

#[test]
fn test_multiple_parameters() {
    let source = r#"
defmodule Test do
  @spec add(integer(), integer()) :: integer()
  def add(a, b), do: a + b
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let add_params: Vec<_> = param_edges
        .iter()
        .filter(|(from, _)| from == "add")
        .collect();

    assert_eq!(
        add_params.len(),
        2,
        "Expected 2 parameter TypeOf edges, got: {:?}",
        add_params
    );
}

#[test]
fn test_mixed_parameter_types() {
    let source = r#"
defmodule Test do
  @spec process(String.t(), integer(), atom()) :: any()
  def process(name, count, status), do: {name, count, status}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let process_params: Vec<_> = param_edges
        .iter()
        .filter(|(from, _)| from == "process")
        .collect();

    assert_eq!(process_params.len(), 3, "Expected 3 parameter TypeOf edges");

    // Check Reference edges
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "process" && to == "String"),
        "Expected Reference to String"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "process" && to == "integer"),
        "Expected Reference to integer"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "process" && to == "atom"),
        "Expected Reference to atom"
    );
}

#[test]
fn test_no_parameters() {
    let source = r#"
defmodule Test do
  @spec get_value() :: integer()
  def get_value(), do: 42
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let value_params: Vec<_> = param_edges
        .iter()
        .filter(|(from, _)| from == "get_value")
        .collect();

    assert_eq!(
        value_params.len(),
        0,
        "Expected no parameter TypeOf edges for function with no parameters"
    );
}

// ============================================================================
// Category 3: Return Types (4 tests)
// ============================================================================

#[test]
fn test_simple_return_type() {
    let source = r#"
defmodule Test do
  @spec get_name() :: String.t()
  def get_name, do: "John"
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_name" && to == "String.t()"),
        "Expected TypeOf edge to String.t()"
    );
}

#[test]
fn test_tuple_return_type() {
    let source = r#"
defmodule Test do
  @spec create() :: {:ok, String.t()}
  def create, do: {:ok, "Created"}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "create"),
        "Expected TypeOf edge for return type"
    );

    // Check Reference edge to String
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "create" && to == "String"),
        "Expected Reference edge to String from tuple"
    );
}

#[test]
fn test_list_return_type() {
    let source = r#"
defmodule Test do
  @spec get_names() :: [String.t()]
  def get_names, do: ["Alice", "Bob"]
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "get_names"),
        "Expected TypeOf edge for return type"
    );

    // Check Reference edge to String
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_names" && to == "String"),
        "Expected Reference edge to String from list"
    );
}

#[test]
fn test_union_return_type() {
    let source = r#"
defmodule Test do
  @spec fetch() :: String.t() | integer()
  def fetch, do: "data"
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "fetch"),
        "Expected TypeOf edge for return type"
    );

    // Check Reference edges to both types in union
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "fetch" && to == "String"),
        "Expected Reference edge to String"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "fetch" && to == "integer"),
        "Expected Reference edge to integer"
    );
}

// ============================================================================
// Category 4: Module-Qualified Types (4 tests)
// ============================================================================

#[test]
fn test_string_t_type() {
    let source = r#"
defmodule Test do
  @spec get_text() :: String.t()
  def get_text, do: "text"
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_text" && to == "String.t()"),
        "Expected TypeOf edge to String.t()"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_text" && to == "String"),
        "Expected Reference edge to String module"
    );
}

#[test]
fn test_enum_t_type() {
    let source = r#"
defmodule Test do
  @spec get_items() :: Enum.t()
  def get_items, do: [1, 2, 3]
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_items" && to == "Enum.t()"),
        "Expected TypeOf edge to Enum.t()"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_items" && to == "Enum"),
        "Expected Reference edge to Enum module"
    );
}

#[test]
fn test_custom_module_type() {
    let source = r#"
defmodule Test do
  @spec create_user() :: User.t()
  def create_user, do: %User{}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "User.t()"),
        "Expected TypeOf edge to User.t()"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "User"),
        "Expected Reference edge to User module"
    );
}

#[test]
fn test_nested_module_type() {
    let source = r#"
defmodule Test do
  @spec get_config() :: App.Config.t()
  def get_config, do: %App.Config{}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "get_config"),
        "Expected TypeOf edge for return type"
    );

    // Note: For nested modules like App.Config.t(), the extraction produces
    // the full qualified name "App.Config" as the reference.
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_config" && to.contains("Config")),
        "Expected Reference edge containing Config module, got: {:?}",
        ref_edges
    );
}

// ============================================================================
// Category 5: Complex Types (3 tests)
// ============================================================================

#[test]
fn test_nested_tuple_types() {
    let source = r#"
defmodule Test do
  @spec process() :: {:ok, {String.t(), integer()}}
  def process, do: {:ok, {"data", 42}}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "process"),
        "Expected TypeOf edge for return type"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "process" && to == "String"),
        "Expected Reference to String in nested tuple"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "process" && to == "integer"),
        "Expected Reference to integer in nested tuple"
    );
}

#[test]
fn test_map_type() {
    let source = r#"
defmodule Test do
  @spec get_map() :: %{name: String.t(), age: integer()}
  def get_map, do: %{name: "John", age: 30}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "get_map"),
        "Expected TypeOf edge for return type"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_map" && to == "String"),
        "Expected Reference to String in map"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_map" && to == "integer"),
        "Expected Reference to integer in map"
    );
}

#[test]
fn test_multiple_union_types() {
    let source = r#"
defmodule Test do
  @spec get_value() :: String.t() | integer() | atom()
  def get_value, do: :ok
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges.iter().any(|(from, _to)| from == "get_value"),
        "Expected TypeOf edge for return type"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_value" && to == "String"),
        "Expected Reference to String"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_value" && to == "integer"),
        "Expected Reference to integer"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "get_value" && to == "atom"),
        "Expected Reference to atom"
    );
}

// ============================================================================
// Category 6: Integration (3 tests)
// ============================================================================

#[test]
fn test_multiple_specs_in_module() {
    let source = r#"
defmodule Test do
  @spec get_name() :: String.t()
  def get_name, do: "John"

  @spec get_age() :: integer()
  def get_age, do: 30
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_name" && to == "String.t()"),
        "Expected TypeOf edge for get_name"
    );
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "get_age" && to == "integer()"),
        "Expected TypeOf edge for get_age"
    );
}

#[test]
fn test_spec_with_params_and_return() {
    let source = r#"
defmodule Test do
  @spec create_user(String.t(), integer()) :: User.t()
  def create_user(name, age), do: %User{name: name, age: age}
end
"#;
    let staging = build_test_graph(source, "test.ex");

    // Check parameter TypeOf edges
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let create_params: Vec<_> = param_edges
        .iter()
        .filter(|(from, _)| from == "create_user")
        .collect();
    assert_eq!(create_params.len(), 2, "Expected 2 parameter TypeOf edges");

    // Check return TypeOf edge
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "User.t()"),
        "Expected TypeOf edge for return type"
    );

    // Check all Reference edges
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "String"),
        "Expected Reference to String"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "integer"),
        "Expected Reference to integer"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(from, to)| from == "create_user" && to == "User"),
        "Expected Reference to User"
    );
}

#[test]
fn test_mixed_public_private_specs() {
    let source = r#"
defmodule Test do
  @spec public_func() :: String.t()
  def public_func, do: "public"

  @spec private_func() :: integer()
  defp private_func, do: 42
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "public_func" && to == "String.t()"),
        "Expected TypeOf edge for public function"
    );
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "private_func" && to == "integer()"),
        "Expected TypeOf edge for private function"
    );
}

// ============================================================================
// Category 7: Edge Cases (2 tests)
// ============================================================================

#[test]
fn test_function_without_spec() {
    let source = r#"
defmodule Test do
  def no_spec(), do: 42
end
"#;
    let staging = build_test_graph(source, "test.ex");

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    let no_spec_edges: Vec<_> = return_edges
        .iter()
        .filter(|(from, _)| from == "no_spec")
        .collect();

    assert_eq!(
        no_spec_edges.len(),
        0,
        "Expected no TypeOf edges for function without @spec"
    );
}

#[test]
fn test_spec_for_nonexistent_function() {
    let source = r#"
defmodule Test do
  @spec ghost_func() :: String.t()
  # Function definition is missing
end
"#;
    let staging = build_test_graph(source, "test.ex");

    // Should still create TypeOf edge even if function def is missing
    // (spec can exist before implementation)
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(from, to)| from == "ghost_func" && to == "String.t()"),
        "Expected TypeOf edge for spec without implementation"
    );
}
