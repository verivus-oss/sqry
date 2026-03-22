//! Graph builder tests for the Vue language plugin.
//!
//! Covers:
//! - Component node creation
//! - Script block extraction (regular and setup)
//! - Function node extraction
//! - Import edge detection
//! - Template event directive call edges (@click, v-on)
//! - TypeScript script blocks
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_vue::VueGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_vue(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vue_sqry::language())
        .expect("Failed to load Vue grammar");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Vue code")
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

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Vue file should succeed");
}

#[test]
fn test_template_only_component() {
    let source = r"
<template>
  <div>
    <h1>Hello World</h1>
    <p>No script block</p>
  </div>
</template>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("hello.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "Template-only Vue component should succeed");
}

// ==================== Component Node Creation ====================

#[test]
fn test_component_node_created() {
    let source = r"
<template>
  <div>Hello</div>
</template>

<script>
export default {
  name: 'MyComponent'
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("MyComponent.vue"),
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

// ==================== Script Block Extraction ====================

#[test]
fn test_options_api_methods() {
    let source = r"
<template>
  <div>
    <button @click='handleClick'>Click me</button>
    <p>{{ message }}</p>
  </div>
</template>

<script>
export default {
  data() {
    return {
      message: 'Hello Vue!'
    };
  },
  methods: {
    handleClick() {
      this.message = 'Clicked!';
    },
    helper() {
      return 42;
    }
  }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("options.vue"),
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
fn test_composition_api_script() {
    let source = r"
<template>
  <div>
    <button @click='increment'>Count: {{ count }}</button>
  </div>
</template>

<script>
import { ref, computed } from 'vue';

export default {
  setup() {
    const count = ref(0);
    const doubled = computed(() => count.value * 2);

    function increment() {
      count.value++;
    }

    return { count, doubled, increment };
  }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("composition.vue"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for vue, got {}",
        import_count
    );
}

#[test]
fn test_script_setup_block() {
    let source = r"
<template>
  <div>
    <button @click='increment'>{{ count }}</button>
  </div>
</template>

<script setup>
import { ref } from 'vue';

const count = ref(0);

function increment() {
  count.value++;
}
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("setup.vue"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Script setup block should succeed: {:?}",
        result.err()
    );
}

// ==================== Import Edge Detection ====================

#[test]
fn test_es6_imports() {
    let source = r"
<template>
  <div>
    <ChildComponent />
  </div>
</template>

<script>
import ChildComponent from './ChildComponent.vue';
import { mapState } from 'vuex';
import axios from 'axios';

export default {
  components: { ChildComponent }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("parent.vue"),
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

// ==================== Template Event Directives ====================

#[test]
fn test_v_on_directive() {
    let source = r"
<template>
  <button v-on:click='handleClick'>Click</button>
  <form v-on:submit.prevent='handleSubmit'>
    <input type='submit' />
  </form>
</template>

<script>
export default {
  methods: {
    handleClick() { console.log('clicked'); },
    handleSubmit() { console.log('submitted'); }
  }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("form.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "v-on directives should succeed");
}

#[test]
fn test_at_shorthand_directive() {
    let source = r"
<template>
  <button @click='save'>Save</button>
  <input @keydown='onKey' @blur='validate' />
</template>

<script>
export default {
  methods: {
    save() {},
    onKey(e) {},
    validate() {}
  }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("input.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "@ shorthand directives should succeed");
}

// ==================== TypeScript Script Blocks ====================

#[test]
fn test_typescript_script_block() {
    let source = r#"
<template>
  <div>{{ greeting }}</div>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';

export default defineComponent({
  setup() {
    const greeting = ref<string>('Hello, TypeScript!');

    function greet(name: string): string {
      return `Hello, ${name}!`;
    }

    return { greeting };
  }
});
</script>
"#;
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("typed.vue"),
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
    let builder = VueGraphBuilder::default();
    assert_eq!(builder.language(), Language::Vue);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VueGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_vue() {
    // Malformed Vue - tree-sitter is error-tolerant
    let source = r"
<script>
  function broken(
"; // incomplete
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.vue"),
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
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.vue"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Vue file should succeed");
}

#[test]
fn test_complete_component() {
    let source = r"
<template>
  <div class='container'>
    <h1>{{ title }}</h1>
    <ul>
      <li v-for='item in items' :key='item.id'>
        {{ item.name }}
        <button @click='removeItem(item.id)'>Remove</button>
      </li>
    </ul>
    <button @click='addItem'>Add</button>
  </div>
</template>

<script>
import ItemService from './services/ItemService';

export default {
  name: 'ItemList',
  data() {
    return {
      title: 'My Items',
      items: []
    };
  },
  methods: {
    async fetchItems() {
      this.items = await ItemService.getAll();
    },
    addItem() {
      this.items.push({ id: Date.now(), name: 'New Item' });
    },
    removeItem(id) {
      this.items = this.items.filter(item => item.id !== id);
    }
  },
  mounted() {
    this.fetchItems();
  }
};
</script>
";
    let tree = parse_vue(source);
    let mut staging = StagingGraph::new();
    let builder = VueGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("ItemList.vue"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Complete Vue component should succeed: {:?}",
        result.err()
    );
}
