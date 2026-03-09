//! Integration tests for `RustGraphBuilder` using the unified `StagingGraph` API.
//!
//! These tests validate that the Rust plugin stages expected node/edge operations.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::test_helpers::{
    assert_has_call_edge, assert_has_export_edge, assert_has_implements_edge,
    assert_has_import_edge, collect_call_edges, collect_export_edges, collect_implements_edges,
    collect_import_edges,
};
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
use sqry_lang_rust::relations::RustGraphBuilder;
use sqry_test_support::graph_helpers::build_node_name_lookup;
use std::path::PathBuf;
use tree_sitter::Tree;

// ========== Helper Functions ==========

/// Load a test fixture from tests/fixtures/rust/{name}.rs
fn load_fixture(name: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("rust")
        .join(format!("{name}.rs"));
    std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("Failed to load fixture {name}: {e}"))
}

/// Parse Rust source code into a tree-sitter Tree
fn parse_rust(content: &str) -> Tree {
    // SAFETY: tree_sitter_rust provides the Rust grammar via FFI
    // This is the standard way to access tree-sitter language grammars
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Rust grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Rust code")
}

fn build_test_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_rust(content);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();
    let file_path = PathBuf::from(filename);

    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

fn build_string_lookup(staging: &StagingGraph) -> std::collections::HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn find_call_edge_metadata(
    staging: &StagingGraph,
    source_name: &str,
    target_name: &str,
) -> Option<(u8, bool)> {
    let node_names = build_node_name_lookup(staging);
    staging.operations().iter().find_map(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind:
                UnifiedEdgeKind::Calls {
                    argument_count,
                    is_async,
                },
            ..
        } = op
            && node_names
                .get(&source.index())
                .is_some_and(|name| name == source_name)
            && node_names
                .get(&target.index())
                .is_some_and(|name| name == target_name)
        {
            Some((*argument_count, *is_async))
        } else {
            None
        }
    })
}

fn find_import_edge_metadata(
    staging: &StagingGraph,
    source_module: &str,
    target_module: &str,
) -> Option<(Option<String>, bool)> {
    let node_names = build_node_name_lookup(staging);
    let strings = build_string_lookup(staging);
    staging.operations().iter().find_map(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: UnifiedEdgeKind::Imports { alias, is_wildcard },
            ..
        } = op
            && node_names
                .get(&source.index())
                .is_some_and(|name| name == source_module)
            && node_names
                .get(&target.index())
                .is_some_and(|name| name == target_module)
        {
            let alias_value = alias
                .map(sqry_core::graph::unified::StringId::index)
                .and_then(|idx| strings.get(&idx))
                .cloned();
            Some((alias_value, *is_wildcard))
        } else {
            None
        }
    })
}

fn assert_no_import_targets_end_with_self(staging: &StagingGraph) {
    let node_names = build_node_name_lookup(staging);
    let offending: Vec<String> = collect_import_edges(staging)
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge { target, .. } = op {
                node_names.get(&target.index()).cloned()
            } else {
                None
            }
        })
        .filter(|name| name.ends_with("::self"))
        .collect();
    assert!(
        offending.is_empty(),
        "Found import targets ending with ::self (should normalize to parent module): {offending:?}"
    );
}

fn collect_ffi_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: UnifiedEdgeKind::FfiCall { .. },
                    ..
                }
            )
        })
        .collect()
}

fn count_import_edges_with_alias(staging: &StagingGraph) -> usize {
    collect_import_edges(staging)
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge {
                kind: UnifiedEdgeKind::Imports { alias, .. },
                ..
            } = op
            {
                alias.is_some()
            } else {
                false
            }
        })
        .count()
}

fn count_wildcard_import_edges(staging: &StagingGraph) -> usize {
    collect_import_edges(staging)
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge {
                kind: UnifiedEdgeKind::Imports { is_wildcard, .. },
                ..
            } = op
            {
                *is_wildcard
            } else {
                false
            }
        })
        .count()
}

// ========== Test Suite (Phase 0.4) ==========
// These validate the staged operation stream.

#[test]
fn test_simple_function_calls() {
    let content = load_fixture("simple_calls");
    let staging = build_test_graph(&content, "simple_calls.rs");
    assert_has_call_edge(&staging, "main", "greet");
    assert_has_call_edge(&staging, "process_data", "fetch");
    assert_has_call_edge(&staging, "process_data", "transform");
    assert!(
        collect_call_edges(&staging).len() >= 3,
        "Expected at least 3 call edges"
    );
}

#[test]
fn test_method_resolution() {
    let content = load_fixture("methods");
    let staging = build_test_graph(&content, "methods.rs");

    assert_has_call_edge(&staging, "Widget::process", "Widget::update");
    assert_has_call_edge(&staging, "Widget::process", "Widget::get_value");
    assert_has_call_edge(&staging, "main", "Widget::new");
}

#[test]
fn test_async_await() {
    let content = load_fixture("async_await");
    let staging = build_test_graph(&content, "async_await.rs");

    assert_has_call_edge(&staging, "process_data", "fetch_data");
    assert_has_call_edge(&staging, "process_data", "validate_data");

    let (_arg_count, is_async_fetch) =
        find_call_edge_metadata(&staging, "process_data", "fetch_data")
            .expect("process_data -> fetch_data call edge");
    assert!(
        is_async_fetch,
        "Expected awaited fetch_data() call edge to have is_async=true"
    );
}

#[test]
fn test_imports() {
    let content = load_fixture("imports");
    let staging = build_test_graph(&content, "imports.rs");

    let import_edges = collect_import_edges(&staging);
    assert_eq!(
        import_edges.len(),
        3,
        "Expected exactly 3 import edges for imports fixture"
    );
    assert_eq!(
        count_import_edges_with_alias(&staging),
        3,
        "Expected exactly 3 aliased import edges for imports fixture"
    );

    assert_has_import_edge(&staging, "<file_module>", "std::collections::HashMap");
    assert_has_import_edge(&staging, "<file_module>", "std::path::PathBuf");
    assert_has_import_edge(&staging, "<file_module>", "std::fs::File");

    let (alias, is_wildcard) =
        find_import_edge_metadata(&staging, "<file_module>", "std::collections::HashMap")
            .expect("HashMap import edge");
    assert_eq!(alias.as_deref(), Some("Map"));
    assert!(!is_wildcard);

    let (alias, is_wildcard) =
        find_import_edge_metadata(&staging, "<file_module>", "std::path::PathBuf")
            .expect("PathBuf import edge");
    assert_eq!(alias.as_deref(), Some("Path"));
    assert!(!is_wildcard);

    let (alias, is_wildcard) =
        find_import_edge_metadata(&staging, "<file_module>", "std::fs::File")
            .expect("File import edge");
    assert_eq!(alias.as_deref(), Some("FileHandle"));
    assert!(!is_wildcard);
}

#[test]
fn test_ffi_calls() {
    let content = load_fixture("ffi");
    let staging = build_test_graph(&content, "ffi.rs");
    assert!(
        collect_ffi_edges(&staging).len() >= 4,
        "Expected at least 4 FFI call edges"
    );
}

#[test]
fn test_trait_impls() {
    let content = load_fixture("trait_impl");
    let staging = build_test_graph(&content, "trait_impl.rs");
    assert!(
        !collect_implements_edges(&staging).is_empty(),
        "Expected at least 1 implements edge"
    );
    assert_has_implements_edge(&staging, "Widget", "Processable");
}

#[test]
fn test_macro_calls() {
    let content = load_fixture("macros");
    let staging = build_test_graph(&content, "macros.rs");

    // Macro invocations now go through CallSite nodes
    // Check that there's a CallSite node between function and macro
    let node_names = build_node_name_lookup(&staging);
    let edges = collect_call_edges(&staging);

    // Find edges from test_print_macros to any CallSite
    let has_callsite_edge = edges.iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: UnifiedEdgeKind::Calls { .. },
            ..
        } = op
        {
            let src = node_names.get(&source.index()).map_or("", String::as_str);
            let tgt = node_names.get(&target.index()).map_or("", String::as_str);
            src == "test_print_macros" && tgt.contains("::println@")
        } else {
            false
        }
    });

    // Find edges from CallSite to println! macro
    let has_macro_edge = edges.iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: UnifiedEdgeKind::Calls { .. },
            ..
        } = op
        {
            let src = node_names.get(&source.index()).map_or("", String::as_str);
            let tgt = node_names.get(&target.index()).map_or("", String::as_str);
            src.contains("::println@") && tgt == "println!"
        } else {
            false
        }
    });

    assert!(
        has_callsite_edge,
        "Expected call edge from test_print_macros to CallSite"
    );
    assert!(
        has_macro_edge,
        "Expected call edge from CallSite to println!"
    );
}

#[test]
fn test_generic_functions() {
    let content = load_fixture("generic_functions");
    let staging = build_test_graph(&content, "generic_functions.rs");
    assert_has_call_edge(&staging, "main", "generic");
    assert_has_call_edge(&staging, "generic", "helper");
}

#[test]
fn test_closure_calls() {
    let content = load_fixture("nested_functions");
    let staging = build_test_graph(&content, "nested_functions.rs");
    assert_has_call_edge(&staging, "with_closure_and_nested", "closure");

    // Verify call from closure using string containment
    let edges = collect_call_edges(&staging);
    let node_names = build_node_name_lookup(&staging);
    let inner_in_closure = edges.iter().any(|op| {
        if let StagingOp::AddEdge { source, target, .. } = op {
            let src = node_names.get(&source.index()).map_or("", String::as_str);
            let tgt = node_names.get(&target.index()).map_or("", String::as_str);
            src.contains("closure") && tgt.contains("inner_fn")
        } else {
            false
        }
    });

    assert!(
        inner_in_closure,
        "Expected a call to inner_fn originating from an anonymous closure"
    );
}

#[test]
fn test_qualified_calls() {
    let content = load_fixture("qualified_paths");
    let staging = build_test_graph(&content, "qualified_paths.rs");
    assert_has_call_edge(&staging, "test_mem_operations", "std::mem::drop");
}

#[test]
fn test_exported_items() {
    let content = load_fixture("modules");
    let staging = build_test_graph(&content, "modules.rs");

    assert!(
        !collect_export_edges(&staging).is_empty(),
        "Expected at least 1 export edge"
    );
    assert_has_export_edge(&staging, "<file_module>", "utils::helper");
}

#[test]
fn test_cross_module_calls() {
    let content = load_fixture("modules");
    let staging = build_test_graph(&content, "modules.rs");
    assert_has_call_edge(&staging, "processing::process_record", "utils::process");
}

#[test]
fn test_inline_module_calls() {
    let content = load_fixture("modules");
    let staging = build_test_graph(&content, "modules.rs");
    assert_has_call_edge(&staging, "utils::process", "helper");
}

#[test]
fn test_turbofish_calls() {
    let content = load_fixture("turbofish_calls");
    let staging = build_test_graph(&content, "turbofish_calls.rs");

    assert_has_call_edge(&staging, "main", "foo");
    assert_has_call_edge(&staging, "main", "Vec::new");

    // Verify no turbofish syntax in any target name
    let node_names = build_node_name_lookup(&staging);
    let edges = collect_call_edges(&staging);
    let has_turbofish = edges.iter().any(|op| {
        if let StagingOp::AddEdge { target, .. } = op {
            let tgt = node_names.get(&target.index()).map_or("", String::as_str);
            tgt.contains("::<")
        } else {
            false
        }
    });

    assert!(
        !has_turbofish,
        "Expected turbofish callee names to be normalized (no `::<...>` in qualified_name)"
    );
}

#[test]
fn test_static_method_calls() {
    let content = load_fixture("methods");
    let staging = build_test_graph(&content, "methods_static.rs");
    assert_has_call_edge(&staging, "main", "Widget::new");
}

#[test]
fn test_argument_counting() {
    let content = load_fixture("argument_counting");
    let staging = build_test_graph(&content, "argument_counting.rs");
    let (arg_count, is_async) =
        find_call_edge_metadata(&staging, "main", "two_args").expect("main -> two_args call edge");
    assert_eq!(
        arg_count, 2,
        "Expected two_args() call to have argument_count=2"
    );
    assert!(
        !is_async,
        "Expected non-awaited calls to have is_async=false"
    );
}

#[test]
fn test_async_attributes() {
    let content = load_fixture("async_attributes");
    let staging = build_test_graph(&content, "async_attributes.rs");
    let (_arg_count, primary_helper_async) =
        find_call_edge_metadata(&staging, "async_test", "helper")
            .expect("async_test -> helper call edge");
    assert!(
        primary_helper_async,
        "Expected awaited helper().await call edge to have is_async=true"
    );

    let (_arg_count, secondary_helper_async) =
        find_call_edge_metadata(&staging, "async_test", "helper2")
            .expect("async_test -> helper2 call edge");
    assert!(
        !secondary_helper_async,
        "Expected non-awaited helper2() call edge to have is_async=false (even inside async fn)"
    );
}

#[test]
fn test_wildcard_imports() {
    let content = load_fixture("wildcards");
    let staging = build_test_graph(&content, "wildcards.rs");
    assert_eq!(
        collect_import_edges(&staging).len(),
        2,
        "Expected exactly 2 import edges for wildcards fixture"
    );
    assert_eq!(
        count_wildcard_import_edges(&staging),
        2,
        "Expected exactly 2 wildcard import edges for wildcards fixture"
    );

    assert_has_import_edge(&staging, "<file_module>", "std::collections::*");
    assert_has_import_edge(&staging, "<file_module>", "std::io::*");
    assert_no_import_targets_end_with_self(&staging);
}

#[test]
fn test_grouped_imports_self_and_nested() {
    let content = load_fixture("imports_grouped_self");
    let staging = build_test_graph(&content, "imports_grouped_self.rs");

    // Expected imports (exactly these 9 edges in this fixture)
    assert_eq!(
        collect_import_edges(&staging).len(),
        9,
        "Expected exactly 9 import edges for grouped/self imports fixture"
    );
    assert_eq!(
        count_import_edges_with_alias(&staging),
        3,
        "Expected exactly 3 aliased import edges for grouped/self imports fixture"
    );

    assert_has_import_edge(&staging, "<file_module>", "std::io");
    assert_has_import_edge(&staging, "<file_module>", "std::io::Read");
    assert_has_import_edge(&staging, "<file_module>", "std::io::Write");

    let (alias, is_wildcard) =
        find_import_edge_metadata(&staging, "<file_module>", "std::io::Write")
            .expect("std::io::Write import edge");
    assert_eq!(alias.as_deref(), Some("IoWrite"));
    assert!(!is_wildcard);

    assert_has_import_edge(&staging, "<file_module>", "mymod");
    assert_has_import_edge(&staging, "<file_module>", "mymod::inner::Thing");
    assert_has_import_edge(&staging, "<file_module>", "mymod::inner::Other");
    assert_has_import_edge(&staging, "<file_module>", "mymod::util");

    let (alias, is_wildcard) = find_import_edge_metadata(&staging, "<file_module>", "mymod::util")
        .expect("mymod::util import edge");
    assert_eq!(alias.as_deref(), Some("util_alias"));
    assert!(!is_wildcard);

    assert_has_import_edge(&staging, "<file_module>", "std::collections");
    assert_has_import_edge(&staging, "<file_module>", "std::collections::HashMap");

    let (alias, is_wildcard) =
        find_import_edge_metadata(&staging, "<file_module>", "std::collections")
            .expect("std::collections import edge");
    assert_eq!(alias.as_deref(), Some("collections"));
    assert!(!is_wildcard);

    assert_no_import_targets_end_with_self(&staging);
}

// === Visibility Metadata Tests ===

/// Helper to find visibility StringId for a function node by name
fn find_function_visibility(staging: &StagingGraph, func_name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == sqry_core::graph::unified::node::NodeKind::Function
            && let Some(name) = strings.get(&entry.name.index())
            && name == func_name
        {
            return entry
                .visibility
                .and_then(|vis_id| strings.get(&vis_id.index()).cloned());
        }
    }
    None
}

#[test]
fn test_visibility_metadata_public_function() {
    let content = r#"
pub fn public_function() {}
fn private_function() {}
pub(crate) fn crate_visible() {}
"#;
    let staging = build_test_graph(content, "visibility_test.rs");

    let pub_vis = find_function_visibility(&staging, "public_function");
    assert_eq!(
        pub_vis.as_deref(),
        Some("public"),
        "public function should have 'public' visibility metadata"
    );

    let private_vis = find_function_visibility(&staging, "private_function");
    assert_eq!(
        private_vis.as_deref(),
        Some("private"),
        "private function should have 'private' visibility metadata"
    );

    let crate_vis = find_function_visibility(&staging, "crate_visible");
    assert_eq!(
        crate_vis.as_deref(),
        Some("public"),
        "pub(crate) function should have 'public' visibility metadata"
    );
}

fn find_function_is_async(staging: &StagingGraph, func_name: &str) -> Option<bool> {
    let strings = build_string_lookup(staging);

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == sqry_core::graph::unified::node::NodeKind::Function
            && let Some(name) = strings.get(&entry.name.index())
            && name == func_name
        {
            return Some(entry.is_async);
        }
    }
    None
}

#[test]
fn test_async_metadata_public_async_function() {
    let content = r#"
pub async fn async_function() {}
fn sync_function() {}
async fn private_async() {}
"#;
    let staging = build_test_graph(content, "async_test.rs");

    let async_fn_is_async = find_function_is_async(&staging, "async_function");
    assert_eq!(
        async_fn_is_async,
        Some(true),
        "async function should have is_async=true"
    );

    let sync_fn_is_async = find_function_is_async(&staging, "sync_function");
    assert_eq!(
        sync_fn_is_async,
        Some(false),
        "sync function should have is_async=false"
    );

    let private_async_is_async = find_function_is_async(&staging, "private_async");
    assert_eq!(
        private_async_is_async,
        Some(true),
        "private async function should have is_async=true"
    );
}
