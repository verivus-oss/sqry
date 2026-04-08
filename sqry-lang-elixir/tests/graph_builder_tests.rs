//! Graph builder tests for the Elixir language plugin.
//!
//! Covers:
//! - Function/macro node extraction (def, defp, defmacro, defmacrop)
//! - Module/class node extraction
//! - Call edge detection (local, remote, pipe operator)
//! - Import/alias edge detection
//! - Protocol definition and implementation
//! - Erlang FFI detection
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_elixir::ElixirGraphBuilder;
use std::path::Path;

fn parse_elixir(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_elixir_sqry::language())
        .expect("failed to set Elixir language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Elixir code")
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

// ==================== Basic Node Extraction ====================

#[test]
fn test_basic_function_extraction() {
    let source = r#"
defmodule MyModule do
  def greet(name) do
    "Hello, #{name}!"
  end

  def add(a, b) do
    a + b
  end
end
"#;
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("my_module.ex"),
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
        has_interned_string_containing(&staging, "greet")
            || has_interned_string_containing(&staging, "add"),
        "Expected function names in staging"
    );
}

#[test]
fn test_module_extraction() {
    let source = r"
defmodule Calculator do
  def add(a, b), do: a + b
  def multiply(a, b), do: a * b
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("calculator.ex"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected module + functions, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_private_function_defp() {
    let source = r"
defmodule MyModule do
  def public_function do
    private_helper()
  end

  defp private_helper do
    :ok
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("my_module.ex"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected both public and private function nodes, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_macro_extraction() {
    let source = r"
defmodule MyMacros do
  defmacro my_macro(expr) do
    quote do
      IO.inspect(unquote(expr))
    end
  end

  defmacrop private_macro(x) do
    quote do: unquote(x) * 2
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("macros.ex"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected macro nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Call Edge Detection ====================

#[test]
fn test_local_call_detection() {
    let source = r"
defmodule Service do
  def run do
    result = helper()
    process(result)
  end

  defp helper do
    :ok
  end

  defp process(_result) do
    :done
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("service.ex"),
            &mut staging,
        )
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge, got {call_count}"
    );
}

#[test]
fn test_remote_call_detection() {
    let source = r#"
defmodule MyModule do
  def work do
    String.upcase("hello")
    Enum.map([1, 2, 3], &(&1 * 2))
  end
end
"#;
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("my_module.ex"),
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

#[test]
fn test_pipe_operator_calls() {
    let source = r"
defmodule Pipeline do
  def transform(data) do
    data
    |> String.trim()
    |> String.upcase()
    |> String.reverse()
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("pipeline.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Pipe operator calls should succeed");
}

// ==================== Import/Alias Detection ====================

#[test]
fn test_import_statement() {
    let source = r"
defmodule MyModule do
  import Enum
  import String, only: [upcase: 1, downcase: 1]

  def work(list) do
    map(list, &upcase/1)
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("my_module.ex"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge, got {import_count}"
    );
}

#[test]
fn test_alias_statement() {
    let source = r"
defmodule MyModule do
  alias MyApp.Users.User
  alias MyApp.Services.{AuthService, EmailService}

  def get_user(id) do
    User.find(id)
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("my_module.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Alias statements should succeed");
}

// ==================== Protocol Support ====================

#[test]
fn test_protocol_definition() {
    let source = r"
defprotocol Printable do
  def print(value)
  def to_string(value)
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("printable.ex"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected protocol node, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_protocol_implementation() {
    let source = r"
defimpl Printable, for: Integer do
  def print(value) do
    IO.puts(value)
  end

  def to_string(value) do
    Integer.to_string(value)
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("printable_int.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Protocol implementation should succeed");
}

// ==================== Erlang FFI ====================

#[test]
fn test_erlang_ffi_detection() {
    let source = r"
defmodule MyModule do
  def work do
    :erlang.now()
    :crypto.strong_rand_bytes(16)
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("erlang_ffi.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Erlang FFI calls should succeed");

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}

// ==================== GenServer Support ====================

#[test]
fn test_genserver_callbacks() {
    let source = r"
defmodule MyServer do
  use GenServer

  def start_link(init_arg) do
    GenServer.start_link(__MODULE__, init_arg, name: __MODULE__)
  end

  def init(state) do
    {:ok, state}
  end

  def handle_call({:get, key}, _from, state) do
    {:reply, Map.get(state, key), state}
  end

  def handle_cast({:put, key, value}, state) do
    {:noreply, Map.put(state, key, value)}
  end
end
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("my_server.ex"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Expected GenServer callback functions, got {}",
        stats.nodes_staged
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = ElixirGraphBuilder::default();
    assert_eq!(builder.language(), Language::Elixir);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ElixirGraphBuilder>();
}

#[test]
fn test_builder_with_custom_scope_depth() {
    let builder = ElixirGraphBuilder::new(5);
    assert_eq!(builder.language(), Language::Elixir);
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Elixir file should succeed");

    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should produce no nodes");
}

#[test]
fn test_malformed_incomplete_elixir() {
    // Incomplete Elixir - tree-sitter is error-tolerant
    let source = r"
defmodule Broken do
  def method(
"; // incomplete
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.ex"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
# This is just a comment
# Another comment
";
    let tree = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.ex"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Elixir file should succeed");
}
