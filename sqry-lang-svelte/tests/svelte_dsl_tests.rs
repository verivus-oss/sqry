//! Tests for Svelte DSL constructs (Component, Props, Stores).

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_svelte::SvelteGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_svelte(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_svelte_sqry::language())
        .expect("Failed to load Svelte grammar");

    let content = source.as_bytes().to_vec();
    let tree = parser.parse(&content, None).expect("Failed to parse");
    (tree, content)
}

/// Count nodes of a specific kind in the staging graph
fn count_nodes_by_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                entry.kind == kind
            } else {
                false
            }
        })
        .count()
}

/// Count Contains edges in the staging graph
fn count_contains_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Contains,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn test_component_node_created() {
    // Test that a Component node is created for the Svelte file
    let source = r"
<script>
  let count = 0;

  function increment() {
    count += 1;
  }
</script>

<button on:click={increment}>
  Count: {count}
</button>
";

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("Counter.svelte"), &mut staging)
        .unwrap();

    // Should create a Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected at least 1 Component node"
    );
}

#[test]
fn test_props_export_let() {
    // Test props defined with export let syntax
    let source = r"
<script>
  export let name;
  export let age = 0;
  export let email = 'example@test.com';
</script>

<div>
  <p>{name} - {age}</p>
  <p>{email}</p>
</div>
";

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("User.svelte"), &mut staging)
        .unwrap();

    // Should create Variable nodes for each prop
    let variable_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        variable_count >= 3,
        "Expected at least 3 Variable nodes for props, got {variable_count}"
    );

    // Should have Contains edges from component to props
    let contains_count = count_contains_edges(&staging);
    assert!(
        contains_count >= 3,
        "Expected at least 3 Contains edges for props"
    );
}

#[test]
fn test_props_with_types() {
    // Test props with TypeScript types
    let source = r#"
<script lang="ts">
  export let name: string;
  export let age: number = 0;
  export let optional: string | undefined;
</script>

<div>{name} - {age}</div>
"#;

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            &content,
            Path::new("TypedProps.svelte"),
            &mut staging,
        )
        .unwrap();

    // Should create Variable nodes for props with types
    let variable_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        variable_count >= 3,
        "Expected at least 3 Variable nodes for typed props"
    );
}

#[test]
fn test_reactive_statements() {
    // Test Svelte reactive statements with $:
    let source = r"
<script>
  let count = 0;

  // Reactive statement
  $: doubled = count * 2;
  $: console.log('count is', count);

  function increment() {
    count += 1;
  }
</script>

<button on:click={increment}>
  {count} (doubled: {doubled})
</button>
";

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("Reactive.svelte"), &mut staging)
        .unwrap();

    // Should have Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected Component node"
    );

    // Should have Function node for increment
    let function_count = count_nodes_by_kind(&staging, NodeKind::Function);
    assert!(
        function_count >= 1,
        "Expected at least 1 Function node for increment"
    );
}

#[test]
fn test_stores_usage() {
    // Test Svelte stores with $ prefix
    let source = r"
<script>
  import { writable } from 'svelte/store';

  const count = writable(0);

  function increment() {
    $count += 1;
  }

  function decrement() {
    count.update(n => n - 1);
  }
</script>

<button on:click={increment}>+</button>
<span>{$count}</span>
<button on:click={decrement}>-</button>
";

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("Store.svelte"), &mut staging)
        .unwrap();

    // Should have Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected Component node"
    );

    // Should have Function nodes
    let function_count = count_nodes_by_kind(&staging, NodeKind::Function);
    assert!(
        function_count >= 2,
        "Expected at least 2 Function nodes (increment, decrement)"
    );
}

#[test]
fn test_complete_component() {
    // Test a complete component with props, functions, and event handlers
    let source = r"
<script>
  export let title;
  export let items = [];

  let selectedIndex = 0;

  $: selectedItem = items[selectedIndex];

  function selectNext() {
    selectedIndex = (selectedIndex + 1) % items.length;
  }

  function selectPrevious() {
    selectedIndex = (selectedIndex - 1 + items.length) % items.length;
  }
</script>

<div>
  <h1>{title}</h1>
  <button on:click={selectPrevious}>Previous</button>
  <p>Selected: {selectedItem}</p>
  <button on:click={selectNext}>Next</button>
</div>
";

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("Selector.svelte"), &mut staging)
        .unwrap();

    // Should have Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected Component node"
    );

    // Should have Variable nodes for props
    let variable_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        variable_count >= 2,
        "Expected at least 2 Variable nodes for props (title, items)"
    );

    // Should have Function nodes
    let function_count = count_nodes_by_kind(&staging, NodeKind::Function);
    assert!(
        function_count >= 2,
        "Expected at least 2 Function nodes (selectNext, selectPrevious)"
    );

    // Should have Contains edges
    let contains_count = count_contains_edges(&staging);
    assert!(contains_count >= 2, "Expected multiple Contains edges");
}

#[test]
fn test_module_context_exports() {
    // Test module context script with exports
    let source = r#"
<script context="module">
  export function sharedHelper() {
    return 'shared';
  }

  export const VERSION = '1.0.0';
</script>

<script>
  export let name;

  function useShared() {
    return sharedHelper();
  }
</script>

<div>{name} - v{VERSION}</div>
"#;

    let (tree, content) = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            &content,
            Path::new("ModuleContext.svelte"),
            &mut staging,
        )
        .unwrap();

    // Should have Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected Component node"
    );

    // Should have Variable node for prop
    let variable_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        variable_count >= 1,
        "Expected at least 1 Variable node for prop (name)"
    );

    // Should have Export edges
    let export_count: usize = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        })
        .count();
    assert!(
        export_count >= 2,
        "Expected at least 2 Export edges (sharedHelper, VERSION)"
    );
}
