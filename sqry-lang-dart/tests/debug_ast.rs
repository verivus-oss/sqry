//! Debug AST structure for FFI patterns

use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_dart::DartGraphBuilder;
use std::path::Path;
use tree_sitter::{Parser, Tree};

fn parse_dart(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_dart::language()).unwrap();
    parser.parse(source.as_bytes(), None).unwrap()
}

#[test]
#[ignore = "Debug AST structure"]
fn debug_dynamic_library_open() {
    let source = r#"
import 'dart:ffi' as ffi;

void loadLib() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
}
"#;
    let tree = parse_dart(source);
    println!("\n=== AST for DynamicLibrary.open ===");
    println!("{}", tree.root_node().to_sexp());
}

#[test]
#[ignore = "Debug AST structure"]
fn debug_lookup_asfunction() {
    let source = r#"
import 'dart:ffi' as ffi;

void callNative() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
  final hello = dylib.lookup('hello').asFunction();
}
"#;
    let tree = parse_dart(source);
    println!("\n=== AST for lookup/asFunction ===");
    println!("{}", tree.root_node().to_sexp());
}

#[test]
#[ignore = "Debug AST structure"]
fn debug_native_annotation() {
    let source = r#"
import 'dart:ffi' as ffi;

@ffi.Native(symbol: 'add')
external int nativeAdd(int a, int b);
"#;
    let tree = parse_dart(source);
    println!("\n=== AST for @Native annotation ===");
    println!("{}", tree.root_node().to_sexp());
}

#[test]
#[ignore = "Debug - check if FFI edges are created"]
fn debug_ffi_edge_creation() {
    let source = r#"
import 'dart:ffi' as ffi;

void loadLib() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
}
"#;
    let tree = parse_dart(source);
    let mut staging = StagingGraph::new();
    let builder = DartGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.dart"),
            &mut staging,
        )
        .unwrap();

    println!("\n=== Staging Graph Stats ===");
    let stats = staging.stats();
    println!("Nodes: {}", stats.nodes_staged);
    println!("Edges: {}", stats.edges_staged);

    println!("\n=== All Nodes ===");
    for node in staging.nodes() {
        if let Some(name) = staging.resolve_node_name(node.entry) {
            println!("  - {} ({:?})", name, node.entry.kind);
        }
    }

    println!("\n=== All Edges ===");
    for edge in staging.edges() {
        println!("  - {:?}", edge.kind);
    }
}
