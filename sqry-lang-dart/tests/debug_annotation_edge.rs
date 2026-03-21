//! Debug annotation FFI edge creation

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
#[ignore = "Debug annotation edge"]
fn debug_annotation_ffi_edge() {
    let source = r#"
import 'dart:ffi' as ffi;

@ffi.Native<ffi.Int32 Function(ffi.Int32)>(symbol: 'add')
external int nativeAdd(int a, int b);
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
