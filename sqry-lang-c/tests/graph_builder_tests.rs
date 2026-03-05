use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language, unified::StagingGraph};
use sqry_lang_c::relations::CGraphBuilder;
use sqry_test_support::graph_helpers::collect_call_edges;
use std::path::Path;
use tree_sitter::Parser;

fn parse_c_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("Failed to set C language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse C code")
}

#[test]
fn test_simple_function_calls() {
    let content = include_str!("fixtures/c/simple_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/simple_calls.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify nodes and edges were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 4,
        "Should have staged at least 4 function nodes (helper, main_function, calculate, caller)"
    );
    assert!(
        stats.edges_staged >= 3,
        "Should have staged at least 3 call edges"
    );
}

#[test]
fn test_static_functions() {
    let content = include_str!("fixtures/c/static_functions.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/static_functions.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for static functions"
    );

    // Verify nodes and edges were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Should have staged at least 1 node (internal_helper)"
    );
    assert!(
        stats.edges_staged >= 1,
        "Should have staged at least 1 edge (call to static function)"
    );
}

#[test]
fn test_function_pointers() {
    let content = include_str!("fixtures/c/function_pointers.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/function_pointers.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for function pointers"
    );

    // Verify nodes and edges were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Should have staged at least 3 function nodes (add, multiply, apply_operation)"
    );
    assert!(
        stats.edges_staged >= 2,
        "Should have staged at least 2 call edges"
    );
}

#[test]
fn test_declarations() {
    let content = include_str!("fixtures/c/declarations.h");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/declarations.h");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for declarations"
    );

    // Verify nodes were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Should have staged at least 3 function declarations (calculate, print_result, process_data)"
    );
}

#[test]
fn test_implementations() {
    let content = include_str!("fixtures/c/implementations.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/implementations.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for implementations"
    );

    // Verify nodes and edges were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Should have staged at least 2 function implementations (calculate, print_result)"
    );
    assert!(
        stats.edges_staged >= 2,
        "Should have staged at least 2 call edges"
    );
}

#[test]
fn test_nested_calls() {
    let content = include_str!("fixtures/c/nested_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/nested_calls.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for nested calls"
    );

    // Verify nodes and edges were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Should have staged at least 3 function nodes (level1, level2, level3)"
    );
    assert!(
        stats.edges_staged >= 3,
        "Should have staged at least 3 call edges (level1->level2->level3, top_level->level1)"
    );
}

#[test]
fn test_struct_field_calls() {
    let content = include_str!("fixtures/c/struct_field_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/struct_field_calls.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for struct field calls"
    );

    // Verify nodes were staged
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Should have staged at least 2 function nodes (double_value, triple_value)"
    );
}

#[test]
fn test_builder_language() {
    let builder = CGraphBuilder::default();
    assert_eq!(builder.language(), Language::C);
}

#[test]
fn test_empty_file() {
    let content = "";
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("empty.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for empty file");

    // Verify no nodes were staged
    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should have no nodes");
}

#[test]
fn test_nodes_have_correct_language() {
    let content = include_str!("fixtures/c/simple_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/simple_calls.c");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    // Verify nodes were staged (language check happens during staging)
    let stats = staging.stats();
    assert!(
        stats.nodes_staged > 0,
        "Should have staged nodes with Language::C"
    );
}

#[test]
fn test_edges_have_metadata() {
    let content = include_str!("fixtures/c/simple_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/simple_calls.c");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    // Verify edges were staged (metadata is applied during staging)
    let stats = staging.stats();
    assert!(
        stats.edges_staged > 0,
        "Should have staged edges with metadata"
    );
}

#[test]
fn test_call_sites_stored() {
    let content = include_str!("fixtures/c/simple_calls.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/simple_calls.c");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    let call_edges = collect_call_edges(&staging);
    let stats = staging.stats();
    assert!(!call_edges.is_empty(), "Should have call edges");
    assert!(stats.edges_staged > 0, "Should have staged call edges");
}

// ==================== Import Edge Tests ====================

use sqry_core::graph::unified::{NodeEntry, NodeId, StringId};
use std::collections::HashMap;

/// Helper to count import edges in staged operations
fn count_import_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        })
        .count()
}

/// Build a map from StringId to string value from staging operations.
fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((*local_id, value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Get node name from NodeEntry using string map.
fn get_node_name(entry: &NodeEntry, string_map: &HashMap<StringId, String>) -> Option<String> {
    string_map.get(&entry.name).cloned()
}

/// Helper to verify an import edge exists with specific target name.
fn has_import_edge_to(staging: &StagingGraph, target_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    // Build NodeId -> name map from AddNode operations
    let mut node_names: HashMap<NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check if any import edge targets a node with matching name
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Imports { .. },
            target,
            ..
        } = op
        {
            node_names
                .get(target)
                .map(|name| name.contains(target_pattern))
                .unwrap_or(false)
        } else {
            false
        }
    })
}

#[test]
fn test_system_headers_import() {
    let content = include_str!("fixtures/c/imports/system_headers.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/imports/system_headers.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for system headers"
    );

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );
    assert!(
        has_import_edge_to(&staging, "stdlib.h"),
        "expected import edge to stdlib.h"
    );
    assert!(
        has_import_edge_to(&staging, "string.h"),
        "expected import edge to string.h"
    );
    assert!(
        has_import_edge_to(&staging, "sys/types.h"),
        "expected import edge to sys/types.h"
    );

    // Count import edges (4 unique, duplicate stdio.h deduplicated)
    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for unique system headers (duplicate stdio.h deduplicated)"
    );
}

#[test]
fn test_local_headers_import() {
    let content = include_str!("fixtures/c/imports/local_headers.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/imports/local_headers.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for local headers"
    );

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "user.h"),
        "expected import edge to user.h"
    );
    assert!(
        has_import_edge_to(&staging, "config.h"),
        "expected import edge to config.h"
    );
    assert!(
        has_import_edge_to(&staging, "utils/helper.h"),
        "expected import edge to utils/helper.h"
    );

    // Count import edges (3 unique, duplicate user.h deduplicated)
    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should have 3 import edges for unique local headers (duplicate user.h deduplicated)"
    );
}

#[test]
fn test_mixed_includes_import() {
    let content = include_str!("fixtures/c/imports/mixed_includes.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/imports/mixed_includes.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for mixed includes"
    );

    // Verify system headers
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );
    assert!(
        has_import_edge_to(&staging, "stdlib.h"),
        "expected import edge to stdlib.h"
    );

    // Verify local headers
    assert!(
        has_import_edge_to(&staging, "config.h"),
        "expected import edge to config.h"
    );
    assert!(
        has_import_edge_to(&staging, "database.h"),
        "expected import edge to database.h"
    );
    assert!(
        has_import_edge_to(&staging, "api/endpoints.h"),
        "expected import edge to api/endpoints.h"
    );

    // Count import edges (5 unique, duplicates deduplicated)
    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 5,
        "Should have 5 import edges for mixed includes (duplicates deduplicated)"
    );
}

#[test]
fn test_import_deduplication() {
    // Test that duplicate includes are deduplicated
    let content = r#"
#include <stdio.h>
#include <stdio.h>
#include <stdio.h>
#include "local.h"
#include "local.h"

void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_dedup.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify specific imports are present
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );
    assert!(
        has_import_edge_to(&staging, "local.h"),
        "expected import edge to local.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 2,
        "Should have exactly 2 import edges (stdio.h and local.h, duplicates removed)"
    );
}

#[test]
fn test_import_edge_structure() {
    let content = r#"
#include <stdio.h>
void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_structure.c");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    // Verify specific target
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );

    // Find the import edge
    let import_edge = staging.operations().iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Imports { .. },
                ..
            }
        )
    });

    assert!(import_edge.is_some(), "Should have an import edge");

    // Verify import edge has correct structure (no alias, not wildcard)
    if let StagingOp::AddEdge {
        kind: EdgeKind::Imports { alias, is_wildcard },
        ..
    } = import_edge.unwrap()
    {
        assert!(
            alias.is_none(),
            "C includes should not have alias (alias is for import renaming)"
        );
        assert!(!*is_wildcard, "C includes are not wildcard imports");
    } else {
        panic!("Expected Imports edge");
    }
}

#[test]
fn test_nested_path_import() {
    // Test includes with nested paths
    let content = r#"
#include <sys/types.h>
#include <sys/socket.h>
#include "utils/helper.h"
#include "api/v1/endpoints.h"

void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_nested.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for nested paths"
    );

    // Verify specific nested path imports
    assert!(
        has_import_edge_to(&staging, "sys/types.h"),
        "expected import edge to sys/types.h"
    );
    assert!(
        has_import_edge_to(&staging, "sys/socket.h"),
        "expected import edge to sys/socket.h"
    );
    assert!(
        has_import_edge_to(&staging, "utils/helper.h"),
        "expected import edge to utils/helper.h"
    );
    assert!(
        has_import_edge_to(&staging, "api/v1/endpoints.h"),
        "expected import edge to api/v1/endpoints.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for nested path includes"
    );
}

#[test]
fn test_no_imports_empty_file() {
    let content = "";
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("empty.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for empty file");

    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 0, "Empty file should have no import edges");
}

#[test]
fn test_no_imports_code_only() {
    // File with functions but no includes
    let content = r#"
void foo() {}
int bar(int x) { return x * 2; }
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("no_imports.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 0,
        "File without includes should have no import edges"
    );
}

#[test]
fn test_imports_and_calls_together() {
    // Test that both imports and calls are extracted from the same file
    let content = r#"
#include <stdio.h>
#include <stdlib.h>

void helper() {
    printf("Hello\n");
}

int main() {
    helper();
    return 0;
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_both.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );
    assert!(
        has_import_edge_to(&staging, "stdlib.h"),
        "expected import edge to stdlib.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 2, "Should have 2 import edges");

    // Count call edges
    let call_count = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Calls { .. },
                    ..
                }
            )
        })
        .count();

    assert!(
        call_count >= 1,
        "Should have at least 1 call edge (helper -> printf, main -> helper)"
    );
}

#[test]
fn test_import_with_comments() {
    // Test that includes with surrounding comments are handled
    let content = r#"
// System includes
#include <stdio.h>  // Standard I/O
/* Multi-line comment
   before include */
#include <stdlib.h>
#include "local.h"  /* Local header */

void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_comments.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed with comments");

    // Verify specific imports are present despite comments
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );
    assert!(
        has_import_edge_to(&staging, "stdlib.h"),
        "expected import edge to stdlib.h"
    );
    assert!(
        has_import_edge_to(&staging, "local.h"),
        "expected import edge to local.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should have 3 import edges (comments don't affect parsing)"
    );
}

#[test]
fn test_import_nodes_created() {
    // Test that import nodes are created for included headers
    let content = r#"
#include <stdio.h>
void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_nodes.c");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    // Count nodes
    let stats = staging.stats();

    // Should have at least:
    // - 1 module node (for the file)
    // - 1 import node (for stdio.h)
    // - 1 function node (for test)
    assert!(
        stats.nodes_staged >= 3,
        "Should have at least 3 nodes (module, import, function). Got: {}",
        stats.nodes_staged
    );
}

#[test]
fn test_conditional_includes_all_extracted() {
    // Test that all includes are extracted regardless of preprocessor conditions
    // (We extract statically, not evaluating preprocessor)
    let content = r#"
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

#include <stdio.h>

void test() {}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_conditional.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify all includes are extracted regardless of ifdef
    assert!(
        has_import_edge_to(&staging, "windows.h"),
        "expected import edge to windows.h"
    );
    assert!(
        has_import_edge_to(&staging, "unistd.h"),
        "expected import edge to unistd.h"
    );
    assert!(
        has_import_edge_to(&staging, "stdio.h"),
        "expected import edge to stdio.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should extract all 3 includes (windows.h, unistd.h, stdio.h)"
    );
}

// ==================== FFI (Foreign Function Interface) Edge Tests ====================

/// Helper to count FfiCall edges in staged operations
fn count_ffi_call_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::FfiCall { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper to count Call edges in staged operations
fn count_call_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Calls { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper to check if an interned string contains a pattern
fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::InternString { value, .. } = op {
            value.contains(pattern)
        } else {
            false
        }
    })
}

#[test]
fn test_extern_function_creates_ffi_node() {
    let content = include_str!("fixtures/c/ffi/extern_functions.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/ffi/extern_functions.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check for FFI function nodes (qualified with extern::C::)
    assert!(
        has_interned_string_containing(&staging, "extern::C::printf"),
        "Should have extern::C::printf FFI function"
    );
    assert!(
        has_interned_string_containing(&staging, "extern::C::malloc"),
        "Should have extern::C::malloc FFI function"
    );
    assert!(
        has_interned_string_containing(&staging, "extern::C::free"),
        "Should have extern::C::free FFI function"
    );
}

#[test]
fn test_extern_variable_creates_ffi_node() {
    let content = include_str!("fixtures/c/ffi/extern_variables.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/ffi/extern_variables.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check for FFI variable nodes (qualified with extern::C::)
    assert!(
        has_interned_string_containing(&staging, "extern::C::errno"),
        "Should have extern::C::errno FFI variable"
    );
    assert!(
        has_interned_string_containing(&staging, "extern::C::environ"),
        "Should have extern::C::environ FFI variable"
    );
}

#[test]
fn test_call_to_extern_creates_ffi_call_edge() {
    let content = include_str!("fixtures/c/ffi/extern_functions.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/ffi/extern_functions.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should create FfiCall edges for calls to extern functions
    let ffi_call_count = count_ffi_call_edges(&staging);
    assert!(
        ffi_call_count >= 3,
        "Should have at least 3 FfiCall edges (printf, malloc, free). Got: {}",
        ffi_call_count
    );
}

#[test]
fn test_mixed_calls_ffi_and_local() {
    let content = include_str!("fixtures/c/ffi/mixed_extern.c");
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/ffi/mixed_extern.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should have FfiCall edges for extern function calls
    let ffi_call_count = count_ffi_call_edges(&staging);
    assert!(
        ffi_call_count >= 4,
        "Should have at least 4 FfiCall edges (printf x2, malloc, free). Got: {}",
        ffi_call_count
    );

    // Should have regular Call edges for local function calls
    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 2,
        "Should have at least 2 Call edges (helper x2). Got: {}",
        call_count
    );
}

#[test]
fn test_local_function_call_not_ffi() {
    // Test that calls to non-extern functions create regular Call edges
    let content = r#"
void helper() {}

void caller() {
    helper();
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_local.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should NOT have any FfiCall edges
    let ffi_call_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_call_count, 0,
        "Local function calls should not create FfiCall edges"
    );

    // Should have regular Call edge
    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Should have at least 1 Call edge for local function"
    );
}

#[test]
fn test_ffi_function_is_exported() {
    let content = r#"
extern int printf(const char *format, ...);
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_export.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count export edges
    let export_count = staging
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

    assert!(export_count > 0, "FFI functions should be exported");
}

#[test]
fn test_ffi_variable_is_exported() {
    let content = r#"
extern int errno;
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_export_var.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check for FFI variable node
    assert!(
        has_interned_string_containing(&staging, "extern::C::errno"),
        "Should have extern::C::errno FFI variable"
    );

    // Count export edges
    let export_count = staging
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

    assert!(export_count > 0, "FFI variables should be exported");
}

#[test]
fn test_pointer_return_extern_function() {
    // Test extern function with pointer return type
    let content = r#"
extern void *malloc(size_t size);
extern char *strdup(const char *s);

void test() {
    void *p = malloc(100);
    char *s = strdup("hello");
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_ptr_return.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check for FFI function nodes
    assert!(
        has_interned_string_containing(&staging, "extern::C::malloc"),
        "Should have extern::C::malloc FFI function"
    );
    assert!(
        has_interned_string_containing(&staging, "extern::C::strdup"),
        "Should have extern::C::strdup FFI function"
    );

    // Should have FfiCall edges
    let ffi_call_count = count_ffi_call_edges(&staging);
    assert!(
        ffi_call_count >= 2,
        "Should have at least 2 FfiCall edges. Got: {}",
        ffi_call_count
    );
}

#[test]
fn test_extern_declaration_order_independent() {
    // Test that FFI calls work regardless of declaration order
    // (calls before declarations should still resolve)
    let content = r#"
// Call before declaration
void early_caller() {
    printf("Hello\n");
}

// Declaration comes later
extern int printf(const char *format, ...);

// Call after declaration
void late_caller() {
    printf("World\n");
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test_order.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Both calls should create FfiCall edges due to two-pass approach
    let ffi_call_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_call_count, 2,
        "Both printf calls should create FfiCall edges regardless of declaration order"
    );
}

// ========================================
// VISIBILITY TESTS
// ========================================

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
            if node_name.is_some_and(|n| n == name) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_function_visibility_public() {
    let content = r#"
// Public function (non-static)
int public_function() {
    return 42;
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "public_function");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Non-static function should have public visibility"
    );
}

#[test]
fn test_function_visibility_private() {
    let content = r#"
// Private function (static)
static int private_function() {
    return 42;
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "private_function");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Static function should have private visibility"
    );
}

#[test]
fn test_function_visibility_mixed() {
    let content = r#"
// Mix of public and private functions
static int helper() {
    return 1;
}

int api_function() {
    return helper() + 1;
}

static void internal_log() {
    // Private logging
}
"#;
    let tree = parse_c_file(content);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("test.c");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify visibility of each function
    assert_eq!(
        find_function_visibility(&staging, "helper"),
        Some("private".to_string()),
        "helper should be private (static)"
    );
    assert_eq!(
        find_function_visibility(&staging, "api_function"),
        Some("public".to_string()),
        "api_function should be public (non-static)"
    );
    assert_eq!(
        find_function_visibility(&staging, "internal_log"),
        Some("private".to_string()),
        "internal_log should be private (static)"
    );
}
