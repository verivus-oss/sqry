//! Visibility tests for Svelte language plugin
//!
//! Svelte components are self-contained units where functions defined in script blocks
//! are part of the component's public API (accessible via component instance).
//! All functions are treated as public by default.

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, node::NodeKind},
};
use sqry_lang_svelte::SvelteGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_svelte(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_svelte_sqry::language())
        .expect("Failed to set Svelte language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Svelte code")
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

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_script_function_public() {
    // Functions in script blocks are public
    let source = r"
<script>
    function handleClick() {
        console.log('clicked');
    }

    function initialize() {
        console.log('init');
    }
</script>

<button on:click={handleClick}>Click me</button>
";

    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let handle_click_visibility = find_function_visibility(&staging, "handleClick");
    assert_eq!(
        handle_click_visibility,
        Some("public".to_string()),
        "Function handleClick should be public"
    );

    let initialize_visibility = find_function_visibility(&staging, "initialize");
    assert_eq!(
        initialize_visibility,
        Some("public".to_string()),
        "Function initialize should be public"
    );
}

#[test]
fn test_exported_function_public() {
    // Exported functions are public
    let source = r"
<script>
    export function publicApi() {
        return 'public';
    }

    function helperFunction() {
        return 'helper';
    }
</script>
";

    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let public_api_visibility = find_function_visibility(&staging, "publicApi");
    assert_eq!(
        public_api_visibility,
        Some("public".to_string()),
        "Exported function should be public"
    );

    let helper_visibility = find_function_visibility(&staging, "helperFunction");
    assert_eq!(
        helper_visibility,
        Some("public".to_string()),
        "Helper function should be public (component internal API)"
    );
}

#[test]
fn test_module_context_function_public() {
    // Functions in module context script are public
    let source = r#"
<script context="module">
    export function moduleFunction() {
        return 'module';
    }
</script>

<script>
    function instanceFunction() {
        return 'instance';
    }
</script>
"#;

    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let module_visibility = find_function_visibility(&staging, "moduleFunction");
    assert_eq!(
        module_visibility,
        Some("public".to_string()),
        "Module context function should be public"
    );

    let instance_visibility = find_function_visibility(&staging, "instanceFunction");
    assert_eq!(
        instance_visibility,
        Some("public".to_string()),
        "Instance function should be public"
    );
}

#[test]
fn test_typescript_function_public() {
    // TypeScript functions in Svelte are also public
    let source = r#"
<script lang="ts">
    function typedFunction(value: number): string {
        return value.toString();
    }

    const arrowFunction = (x: number): number => x * 2;
</script>
"#;

    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let typed_visibility = find_function_visibility(&staging, "typedFunction");
    assert_eq!(
        typed_visibility,
        Some("public".to_string()),
        "TypeScript function should be public"
    );
}

#[test]
fn test_event_handler_function_public() {
    // Functions used as event handlers are public
    let source = r#"
<script>
    function onClick(event) {
        event.preventDefault();
    }

    function onSubmit() {
        console.log('submit');
    }
</script>

<button on:click={onClick}>Click</button>
<form on:submit|preventDefault={onSubmit}>
    <input type="submit">
</form>
"#;

    let tree = parse_svelte(source);
    let mut staging = StagingGraph::new();
    let builder = SvelteGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.svelte"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let on_click_visibility = find_function_visibility(&staging, "onClick");
    assert_eq!(
        on_click_visibility,
        Some("public".to_string()),
        "Event handler onClick should be public"
    );

    let on_submit_visibility = find_function_visibility(&staging, "onSubmit");
    assert_eq!(
        on_submit_visibility,
        Some("public".to_string()),
        "Event handler onSubmit should be public"
    );
}
