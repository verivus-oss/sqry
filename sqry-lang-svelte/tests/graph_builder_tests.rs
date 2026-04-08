//! Graph builder tests for the Svelte language plugin.
//!
//! Covers:
//! - Script block extraction (instance and module)
//! - Function node extraction from script blocks
//! - Import edge detection
//! - Template event handler call edges
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_svelte::SvelteGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_svelte(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_svelte_sqry::language())
        .expect("Failed to load Svelte grammar");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Svelte code")
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

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Svelte file should succeed");
}

#[test]
fn test_html_only_component() {
    let source = r"
<div>
  <h1>Hello World</h1>
  <p>No script block here</p>
</div>
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("hello.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "HTML-only Svelte component should succeed");
}

// ==================== Script Block Extraction ====================

#[test]
fn test_script_block_functions() {
    let source = r"
<script>
  function greet(name) {
    return 'Hello, ' + name;
  }

  function handleClick() {
    console.log(greet('World'));
  }
</script>

<button on:click={handleClick}>Click me</button>
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("button.svelte"),
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
fn test_script_module_block() {
    let source = r#"
<script context="module">
  export async function load({ fetch }) {
    const res = await fetch('/api/data');
    const data = await res.json();
    return { props: { data } };
  }
</script>

<script>
  export let data;
</script>

<div>{data}</div>
"#;
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("page.svelte"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Script module block should succeed: {:?}",
        result.err()
    );
}

// ==================== Import Edge Detection ====================

#[test]
fn test_es6_imports() {
    let source = r"
<script>
  import { onMount } from 'svelte';
  import MyComponent from './MyComponent.svelte';
  import { writable } from 'svelte/store';

  let count = writable(0);

  onMount(() => {
    console.log('mounted');
  });
</script>

<MyComponent />
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("app.svelte"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge, got {import_count}"
    );
    assert!(
        has_interned_string_containing(&staging, "svelte")
            || has_interned_string_containing(&staging, "MyComponent"),
        "Expected import module names in staging"
    );
}

#[test]
fn test_multiple_imports() {
    let source = r"
<script>
  import { onMount, onDestroy } from 'svelte';
  import { goto } from '$app/navigation';
  import Header from './Header.svelte';
  import Footer from './Footer.svelte';
</script>

<Header />
<slot />
<Footer />
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("layout.svelte"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges, got {import_count}"
    );
}

// ==================== Event Handler Call Edges ====================

#[test]
fn test_on_click_handler() {
    let source = r"
<script>
  function handleClick() {
    alert('clicked');
  }

  function handleSubmit(event) {
    event.preventDefault();
  }
</script>

<button on:click={handleClick}>Click</button>
<form on:submit={handleSubmit}>
  <input type='submit' />
</form>
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("form.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "Event handler click should succeed");
}

#[test]
fn test_multiple_event_handlers() {
    let source = r"
<script>
  function onKeyDown(e) {
    if (e.key === 'Enter') submit();
  }

  function submit() {
    console.log('submitted');
  }

  function onMouseOver() {
    console.log('hover');
  }
</script>

<input on:keydown={onKeyDown} on:mouseover={onMouseOver} />
<button on:click={submit}>Submit</button>
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("input.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "Multiple event handlers should succeed");
}

// ==================== TypeScript Script Blocks ====================

#[test]
fn test_typescript_script_block() {
    let source = r#"
<script lang="ts">
  import type { User } from './types';

  export let user: User;

  function greet(name: string): string {
    return `Hello, ${name}!`;
  }

  const message: string = greet(user.name);
</script>

<p>{message}</p>
"#;
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("typed.svelte"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "TypeScript script block should succeed: {:?}",
        result.err()
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = SvelteGraphBuilder::default();
    assert_eq!(builder.language(), Language::Svelte);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SvelteGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_svelte() {
    // Malformed Svelte - tree-sitter is error-tolerant
    let source = r"
<script>
  function broken(
"; // incomplete
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.svelte"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
<!-- This is an HTML comment -->
<!-- Another comment -->
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Svelte file should succeed");
}

#[test]
fn test_reactive_statements() {
    let source = r"
<script>
  let count = 0;
  let doubled;

  $: doubled = count * 2;

  function increment() {
    count += 1;
  }
</script>

<button on:click={increment}>Count: {count}</button>
<p>Doubled: {doubled}</p>
";
    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("reactive.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "Reactive statements should succeed");
}
