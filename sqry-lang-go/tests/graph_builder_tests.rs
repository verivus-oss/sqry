use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_go::relations::GoGraphBuilder;
use sqry_test_support::graph_helpers::{
    assert_call_edge_has_span_for_language, assert_has_call_edge_for_language,
    find_call_edge_for_language,
};
use std::path::Path;
use tree_sitter::Parser;

fn parse_go_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("Failed to set Go language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse Go code")
}

#[test]
fn test_simple_function_calls() {
    let content = include_str!("fixtures/go/simple_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/simple_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "main.helper");
    assert_has_call_edge_for_language(&staging, Language::Go, "main.helper", "fmt.Println");

    let helper_call =
        find_call_edge_for_language(&staging, Language::Go, "main.main", "main.helper")
            .expect("expected call edge main.main -> main.helper");
    assert_eq!(helper_call.argument_count, 0);
    assert!(!helper_call.is_async);

    let print_call =
        find_call_edge_for_language(&staging, Language::Go, "main.helper", "fmt.Println")
            .expect("expected call edge main.helper -> fmt.Println");
    assert_eq!(print_call.argument_count, 1);
    assert!(!print_call.is_async);
}

#[test]
fn test_method_calls() {
    let content = include_str!("fixtures/go/method_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/method_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed for methods");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "Increment");
    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "GetValue");

    let increment_call =
        find_call_edge_for_language(&staging, Language::Go, "main.main", "Increment")
            .expect("expected call edge main.main -> Increment");
    assert_eq!(increment_call.argument_count, 0);
}

#[test]
fn test_package_qualified_calls() {
    let content = include_str!("fixtures/go/package_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/package_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed for package calls");

    assert_has_call_edge_for_language(
        &staging,
        Language::Go,
        "utils.ProcessString",
        "strings.ToUpper",
    );
    assert_has_call_edge_for_language(&staging, Language::Go, "utils.ProcessString", "fmt.Println");
}

#[test]
fn test_receiver_types() {
    let content = include_str!("fixtures/go/receiver_types.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/receiver_types.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed for receiver types");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "Distance");
    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "Move");
}

#[test]
fn test_interfaces() {
    let content = include_str!("fixtures/go/interfaces.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/interfaces.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed for interfaces");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.processData", "Read");
    assert_has_call_edge_for_language(&staging, Language::Go, "main.processData", "Write");
}

#[test]
fn test_nodes_have_correct_language() {
    let content = include_str!("fixtures/go/simple_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/simple_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    assert!(
        has_node_display_name(&staging, "main.main"),
        "should intern main.main node name"
    );
    assert!(
        has_node_display_name(&staging, "main.helper"),
        "should intern main.helper node name"
    );
}

#[test]
fn test_edges_have_metadata() {
    let content = include_str!("fixtures/go/simple_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/simple_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let helper_call =
        find_call_edge_for_language(&staging, Language::Go, "main.main", "main.helper")
            .expect("expected call edge main.main -> main.helper");
    assert_eq!(helper_call.argument_count, 0);
    assert_call_edge_has_span_for_language(&staging, Language::Go, "main.main", "main.helper");

    let print_call =
        find_call_edge_for_language(&staging, Language::Go, "main.helper", "fmt.Println")
            .expect("expected call edge main.helper -> fmt.Println");
    assert_eq!(print_call.argument_count, 1);
}

#[test]
fn test_call_edges_have_argument_count() {
    let content = include_str!("fixtures/go/simple_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/simple_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let print_call =
        find_call_edge_for_language(&staging, Language::Go, "main.helper", "fmt.Println")
            .expect("expected call edge main.helper -> fmt.Println");
    assert_eq!(print_call.argument_count, 1);
}

#[test]
fn test_nested_functions() {
    let content = include_str!("fixtures/go/nested_functions.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/nested_functions.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build_graph should handle nested functions");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.outer", "main.inner");
    assert_has_call_edge_for_language(&staging, Language::Go, "main.outer", "main.helper");
}

#[test]
fn test_multiple_packages() {
    let content = include_str!("fixtures/go/multiple_packages.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/multiple_packages.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build_graph should handle multiple package imports");

    assert_has_call_edge_for_language(&staging, Language::Go, "config.Load", "os.Open");
    assert_has_call_edge_for_language(&staging, Language::Go, "config.Load", "io.ReadAll");
    assert_has_call_edge_for_language(&staging, Language::Go, "config.Load", "json.Unmarshal");
}

#[test]
fn test_builder_language() {
    let builder = GoGraphBuilder::default();
    assert_eq!(builder.language(), Language::Go);
}

#[test]
fn test_empty_file() {
    let content = "package main\n";
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("empty.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("Should handle empty file gracefully");

    assert_eq!(
        staging.stats().edges_staged,
        0,
        "Empty file should not stage any edges"
    );
}

#[test]
fn test_package_qualification() {
    // Test that package name is correctly extracted and used
    let content = include_str!("fixtures/go/package_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/package_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    assert!(
        has_node_display_name(&staging, "utils.ProcessString"),
        "Should qualify function names with package"
    );
}

#[test]
fn test_call_edges() {
    // Test that call edges are staged for simple calls
    let content = include_str!("fixtures/go/simple_calls.go");
    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("tests/fixtures/go/simple_calls.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    assert_has_call_edge_for_language(&staging, Language::Go, "main.main", "main.helper");
}

// ============================================================================
// FFI Tests (CGo, syscall, plugin)
// ============================================================================

use sqry_core::graph::unified::EdgeKind;
use sqry_core::graph::unified::build::StagingOp;

/// Helper to count `FfiCall` edges from staging operations
fn count_ffi_edges(staging: &StagingGraph) -> usize {
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

/// Helper to collect `FfiCall` edge (source, target) pairs to check for duplicates
fn collect_ffi_edge_pairs(staging: &StagingGraph) -> Vec<(String, String)> {
    use sqry_core::graph::unified::build::test_helpers::build_node_name_lookup;
    let name_lookup = build_node_name_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::FfiCall { .. },
                ..
            } = op
            {
                let source_name = name_lookup
                    .get(source)
                    .cloned()
                    .unwrap_or_else(|| format!("{source:?}"));
                let target_name = name_lookup
                    .get(target)
                    .cloned()
                    .unwrap_or_else(|| format!("{target:?}"));
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn test_cgo_function_call() {
    let content = r#"
package main

/*
#include <stdio.h>
*/
import "C"

func main() {
    C.puts(C.CString("Hello from C"))
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_cgo.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 2,
        "Expected at least 2 FfiCall edges for C.puts and C.CString, got {ffi_count}"
    );
}

#[test]
fn test_cgo_multiple_calls() {
    let content = r#"
package main

/*
#include <stdlib.h>
*/
import "C"

func allocateMemory() {
    ptr := C.malloc(1024)
    defer C.free(ptr)
    C.memset(ptr, 0, 1024)
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_cgo_multiple.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 3,
        "Expected at least 3 FfiCall edges for C.malloc, C.free, C.memset, got {ffi_count}"
    );
}

#[test]
fn test_syscall_syscall6() {
    let content = r#"
package main

import "syscall"

func writeToFile() {
    syscall.Syscall6(syscall.SYS_WRITE, 1, 0, 0, 0, 0, 0)
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_syscall.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for syscall.Syscall6, got {ffi_count}"
    );
}

#[test]
fn test_syscall_raw_syscall() {
    let content = r#"
package main

import "syscall"

func rawWrite() {
    syscall.RawSyscall(syscall.SYS_WRITE, 1, 0, 0)
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_raw_syscall.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for syscall.RawSyscall, got {ffi_count}"
    );
}

#[test]
fn test_plugin_open() {
    let content = r#"
package main

import "plugin"

func loadPlugin() {
    p, _ := plugin.Open("./myplugin.so")
    _ = p
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_plugin.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for plugin.Open, got {ffi_count}"
    );
}

#[test]
fn test_no_ffi_for_regular_calls() {
    let content = r#"
package main

import "fmt"

func main() {
    fmt.Println("Hello")
    fmt.Printf("%d\n", 42)
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_no_ffi.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Regular fmt calls should NOT create FFI edges
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for regular fmt calls, got {ffi_count}"
    );
}

#[test]
fn test_cgo_without_import_c() {
    // If "C" is not imported, C.xxx calls should NOT be treated as FFI
    let content = r"
package main

type C struct{}

func (c C) SomeMethod() {}

func main() {
    var c C
    c.SomeMethod()
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_no_cgo.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Without import "C", this should not create FFI edges
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges when import \"C\" is missing, got {ffi_count}"
    );
}

#[test]
fn test_combined_ffi_and_regular_calls() {
    let content = r#"
package main

/*
#include <math.h>
*/
import "C"
import "fmt"

func compute() {
    // FFI call
    result := C.sqrt(16.0)

    // Regular call
    fmt.Println(result)
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_combined.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for C.sqrt, got {ffi_count}"
    );

    // Should also have regular call edges for fmt.Println
    let stats = staging.stats();
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 edges (FFI + regular call), got {}",
        stats.edges_staged
    );
}

#[test]
fn test_nested_cgo_in_go_statement() {
    // Test that nested CGO calls inside goroutines are detected
    // Example: go C.puts(C.CString("x")) should find both C.puts and C.CString
    let content = r#"
package main

/*
#include <stdio.h>
#include <stdlib.h>
*/
import "C"

func main() {
    go C.puts(C.CString("Hello from goroutine"))
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_nested_cgo_go.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 2 FFI calls: C.puts and C.CString
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected exactly 2 FfiCall edges (C.puts and C.CString), got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: std::collections::HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn test_nested_cgo_in_defer_statement() {
    // Test that nested CGO calls inside defer statements are detected
    // Example: defer C.puts(C.CString("x")) should find both C.puts and C.CString
    let content = r#"
package main

/*
#include <stdio.h>
#include <stdlib.h>
*/
import "C"

func cleanup() {
    defer C.puts(C.CString("Cleanup message"))
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_nested_cgo_defer.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 2 FFI calls: C.puts and C.CString
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected exactly 2 FfiCall edges (C.puts and C.CString), got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: std::collections::HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn test_goroutine_async_call_edge() {
    // Test that goroutine calls create exactly one async Calls edge
    // Verifies no duplicate sync edge is created
    let content = r#"
package main

func helper() {
    println("helper")
}

func main() {
    go helper()
}
"#;
    let tree = parse_go_file(content);
    let file = Path::new("test_goroutine_async.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    use sqry_core::graph::unified::build::test_helpers::build_node_name_lookup;
    let name_lookup = build_node_name_lookup(&staging);

    // Collect all Calls edges (for debugging)
    let all_call_edges: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::Calls { is_async, .. },
                ..
            } = op
            {
                let source_name = name_lookup
                    .get(source)
                    .cloned()
                    .unwrap_or_else(|| format!("{source:?}"));
                let target_name = name_lookup
                    .get(target)
                    .cloned()
                    .unwrap_or_else(|| format!("{target:?}"));
                Some((source_name, target_name, *is_async))
            } else {
                None
            }
        })
        .collect();

    // Find calls from main to helper (checking for package-qualified names)
    let main_to_helper_calls: Vec<_> = all_call_edges
        .iter()
        .filter(|(source, target, _)| source.contains("main") && target.contains("helper"))
        .collect();

    // Should have exactly 1 Calls edge from main to helper
    assert_eq!(
        main_to_helper_calls.len(),
        1,
        "Expected exactly 1 Calls edge from main to helper, got {}. All calls: {all_call_edges:?}",
        main_to_helper_calls.len()
    );

    // The single edge should be async
    assert!(
        main_to_helper_calls[0].2,
        "Goroutine call edge should have is_async=true, got false"
    );
}

// ============================================================================
// OOP Embedding Tests (Struct and Interface Embedding)
// ============================================================================

/// Helper to count Inherits edges from staging operations
fn count_inherits_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Inherits,
                    ..
                }
            )
        })
        .count()
}

/// Helper to check if a staged node resolves to a display name containing a pattern.
fn has_node_display_name(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            staging
                .resolve_node_display_name(Language::Go, entry)
                .is_some_and(|name| name.contains(pattern))
        } else {
            false
        }
    })
}

#[test]
fn test_struct_embedding_creates_inherits_edge() {
    let content = r"
package main

type Base struct {
    Name string
}

type Child struct {
    Base
    Age int
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_embedding.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have Inherits edge: Child → Base
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 Inherits edge for struct embedding, got {inherits_count}"
    );

    // Both types should be present
    assert!(
        has_node_display_name(&staging, "main.Child"),
        "Should have main.Child struct"
    );
    assert!(
        has_node_display_name(&staging, "main.Base"),
        "Should have main.Base struct"
    );
}

#[test]
fn test_interface_embedding_creates_inherits_edge() {
    let content = r"
package main

type Reader interface {
    Read() error
}

type Writer interface {
    Write() error
}

type ReadWriter interface {
    Reader
    Writer
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_interface_embedding.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have 2 Inherits edges: ReadWriter → Reader, ReadWriter → Writer
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges for interface embedding, got {inherits_count}"
    );
}

#[test]
fn test_pointer_struct_embedding() {
    let content = r"
package main

type Parent struct {
    Value int
}

type Child struct {
    *Parent
    Extra string
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_pointer_embedding.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have 1 Inherits edge for *Parent embedding
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 Inherits edge for pointer struct embedding, got {inherits_count}"
    );
}

#[test]
fn test_multiple_struct_embedding() {
    let content = r"
package main

type A struct { X int }
type B struct { Y int }

type C struct {
    A
    B
    Z int
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_multiple_embedding.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have 2 Inherits edges: C → A, C → B
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges for multiple struct embedding, got {inherits_count}"
    );
}

#[test]
fn test_no_embedding_for_named_fields() {
    let content = r"
package main

type Parent struct {
    Value int
}

type Child struct {
    parent Parent
    Name string
}
";
    let tree = parse_go_file(content);
    let file = Path::new("test_no_embedding.go");
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Named field 'parent Parent' should NOT create Inherits edge
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 0,
        "Named fields should not create Inherits edges, got {inherits_count}"
    );
}

// ========== HTTP Route Endpoint Detection Tests ==========

use sqry_core::graph::unified::node::NodeKind;

/// Helper: check if staging has an Endpoint node whose resolved name contains the given substring.
fn has_endpoint_with_name(staging: &StagingGraph, name_substring: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Endpoint)
            && let Some(resolved) = staging.resolve_local_string(entry.name)
        {
            return resolved.contains(name_substring);
        }
        false
    })
}

/// Helper: collect all Endpoint node names from staging.
fn collect_endpoint_names(staging: &StagingGraph) -> Vec<String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Endpoint)
            {
                return staging.resolve_local_string(entry.name).map(String::from);
            }
            None
        })
        .collect()
}

#[test]
fn test_go_http_handlefunc_creates_endpoint() {
    let content = r#"
package main

import (
    "fmt"
    "net/http"
)

func handleUsers(w http.ResponseWriter, r *http.Request) {
    fmt.Fprintf(w, "users")
}

func main() {
    http.HandleFunc("/api/users", handleUsers)
}
"#;

    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("test_go_http_handlefunc.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node 'route::GET::/api/users', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_go_route_detection_rejects_non_path_args() {
    let content = r#"
package main

type Cache struct{}

func (c *Cache) GET(key string) string {
    return ""
}

func main() {
    cache := Cache{}
    cache.GET("some_key")
}
"#;

    let tree = parse_go_file(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new("test_go_no_false_route.go");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert!(
        endpoints.is_empty(),
        "cache.GET(\"some_key\") should NOT create Endpoint nodes, but found: {:?}",
        endpoints
    );
}
