//! Tests for Vue DSL constructs (Component, Props, Computed, Watch).

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_vue::VueGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_vue(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vue_sqry::language())
        .expect("Failed to load Vue grammar");

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
    // Test that a Component node is created for the Vue SFC
    let source = r"
<script>
export default {
  name: 'MyComponent'
}
</script>

<template>
  <div>Hello</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("MyComponent.vue"), &mut staging)
        .unwrap();

    // Should create a Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected at least 1 Component node"
    );
}

#[test]
fn test_props_array_syntax() {
    // Test props defined as array: props: ['name', 'age']
    let source = r"
<script>
export default {
  props: ['name', 'age', 'email']
}
</script>

<template>
  <div>{{ name }}</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
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
fn test_props_object_syntax() {
    // Test props defined as object: props: { name: String, age: Number }
    let source = r"
<script>
export default {
  props: {
    name: String,
    age: {
      type: Number,
      default: 0
    },
    email: {
      type: String,
      required: true
    }
  }
}
</script>

<template>
  <div>{{ name }} - {{ age }}</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
        .unwrap();

    // Should create Variable nodes for each prop
    let variable_count = count_nodes_by_kind(&staging, NodeKind::Variable);
    assert!(
        variable_count >= 3,
        "Expected at least 3 Variable nodes for props, got {variable_count}"
    );
}

#[test]
fn test_computed_properties() {
    // Test computed properties: computed: { fullName() { ... } }
    let source = r"
<script>
export default {
  data() {
    return {
      firstName: 'John',
      lastName: 'Doe'
    }
  },
  computed: {
    fullName() {
      return this.firstName + ' ' + this.lastName;
    },
    displayName: {
      get() {
        return this.fullName;
      },
      set(value) {
        const parts = value.split(' ');
        this.firstName = parts[0];
        this.lastName = parts[1];
      }
    }
  }
}
</script>

<template>
  <div>{{ fullName }}</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
        .unwrap();

    // Should create Method nodes for computed properties
    let method_count = count_nodes_by_kind(&staging, NodeKind::Method);
    assert!(
        method_count >= 2,
        "Expected at least 2 Method nodes for computed properties, got {method_count}"
    );
}

#[test]
fn test_watch_properties() {
    // Test watchers: watch: { count(newVal, oldVal) { ... } }
    let source = r"
<script>
export default {
  data() {
    return {
      count: 0,
      message: 'Hello'
    }
  },
  watch: {
    count(newVal, oldVal) {
      console.log('count changed from', oldVal, 'to', newVal);
    },
    message: {
      handler(newVal, oldVal) {
        console.log('message changed');
      },
      immediate: true
    }
  }
}
</script>

<template>
  <div>{{ count }}</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
        .unwrap();

    // Should create Method nodes for watchers
    let method_count = count_nodes_by_kind(&staging, NodeKind::Method);
    assert!(
        method_count >= 2,
        "Expected at least 2 Method nodes for watchers, got {method_count}"
    );
}

#[test]
fn test_complete_component() {
    // Test a complete component with props, computed, watch, and methods
    let source = r"
<script>
export default {
  name: 'UserProfile',
  props: {
    userId: {
      type: Number,
      required: true
    },
    initialName: String
  },
  data() {
    return {
      name: this.initialName || 'Anonymous',
      age: 0
    }
  },
  computed: {
    displayName() {
      return `User #${this.userId}: ${this.name}`;
    },
    isAdult() {
      return this.age >= 18;
    }
  },
  watch: {
    userId(newId) {
      this.loadUserData(newId);
    }
  },
  methods: {
    loadUserData(id) {
      // Fetch user data
    },
    updateName(newName) {
      this.name = newName;
    }
  }
}
</script>

<template>
  <div>{{ displayName }}</div>
</template>
";

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("UserProfile.vue"), &mut staging)
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
        "Expected at least 2 Variable nodes for props"
    );

    // Should have Method nodes for computed, watch, and methods
    let method_count = count_nodes_by_kind(&staging, NodeKind::Method);
    assert!(
        method_count >= 6, // 2 computed + 1 watch + 2 methods + data()
        "Expected at least 6 Method nodes, got {method_count}"
    );

    // Should have Contains edges
    let contains_count = count_contains_edges(&staging);
    assert!(contains_count >= 5, "Expected multiple Contains edges");
}

#[test]
fn test_vue3_composition_api() {
    // Test Vue 3 Composition API with <script setup>
    let source = r#"
<script setup>
import { ref, computed, watch } from 'vue';

const props = defineProps({
  userId: Number,
  name: String
});

const count = ref(0);
const doubled = computed(() => count.value * 2);

watch(count, (newVal) => {
  console.log('count changed to', newVal);
});

function increment() {
  count.value++;
}
</script>

<template>
  <div>
    <p>{{ name }}: {{ count }} (doubled: {{ doubled }})</p>
    <button @click="increment">+</button>
  </div>
</template>
"#;

    let (tree, content) = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.vue"), &mut staging)
        .unwrap();

    // Should have Component node
    assert!(
        count_nodes_by_kind(&staging, NodeKind::Component) >= 1,
        "Expected Component node"
    );

    // Should have Function nodes for composition API functions
    let function_count = count_nodes_by_kind(&staging, NodeKind::Function);
    assert!(
        function_count >= 1,
        "Expected at least 1 Function node for increment"
    );
}
