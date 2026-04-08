//! Visibility tests for Vue language plugin
//!
//! Vue components are self-contained units. Methods defined in the
//! component options object are part of the component's public API.

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, node::NodeKind},
};
use sqry_lang_vue::VueGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_vue(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vue_sqry::language())
        .expect("Failed to set Vue language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Vue code")
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
            && matches!(entry.kind, NodeKind::Function | NodeKind::Method)
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
fn test_vue_methods_public() {
    let source = r#"
<template>
  <button @click="handleClick">Click</button>
</template>

<script>
export default {
  methods: {
    handleClick() {
      console.log('clicked');
    },
    helper() {
      return 'help';
    }
  }
}
</script>
"#;

    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let handle_click_visibility = find_function_visibility(&staging, "handleClick");
    assert_eq!(
        handle_click_visibility,
        Some("public".to_string()),
        "Vue method should be public"
    );
}

#[test]
fn test_composition_api_functions_public() {
    let source = r"
<script setup>
import { ref } from 'vue'

const count = ref(0)

function increment() {
  count.value++
}

function reset() {
  count.value = 0
}
</script>
";

    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let increment_visibility = find_function_visibility(&staging, "increment");
    assert_eq!(
        increment_visibility,
        Some("public".to_string()),
        "Composition API function should be public"
    );
}

#[test]
fn test_lifecycle_hooks_public() {
    let source = r"
<script>
export default {
  mounted() {
    console.log('mounted');
  },
  updated() {
    console.log('updated');
  }
}
</script>
";

    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Component.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let mounted_visibility = find_function_visibility(&staging, "mounted");
    assert_eq!(
        mounted_visibility,
        Some("public".to_string()),
        "Lifecycle hook should be public"
    );
}
