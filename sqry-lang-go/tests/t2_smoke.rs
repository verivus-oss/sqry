//! AC-15 workspace-level smoke for the T2 channel / generic surface.
//!
//! The spec's AC-15 targets a real std-library-heavy Go project (etcd /
//! cockroach); this synthetic variant generates a dense single-file workspace
//! instead so it runs in CI without vendoring a third-party tree. It asserts
//! the build emits a non-trivial number of `Channel` nodes, `ChannelPeer`
//! edges, and `Instantiates` edges, and that the build does not panic.

use std::fmt::Write as _;
use std::path::Path;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_go::relations::GoGraphBuilder;
use tree_sitter::Parser;

/// Generate a dense Go file: `n` worker functions, each making and draining its
/// own channel, plus `n` generic instantiations of a local generic function.
fn generate_dense_go(n: usize) -> String {
    let mut src = String::from("package q\n\nfunc Identity[T any](x T) T { return x }\n\n");
    for i in 0..n {
        writeln!(
            src,
            "func worker{i}() {{\n    ch{i} := make(chan int, {cap})\n    ch{i} <- {i}\n    v := <-ch{i}\n    _ = v\n    close(ch{i})\n    _ = Identity[int]({i})\n}}",
            i = i,
            cap = i % 4,
        )
        .unwrap();
    }
    src
}

fn build(source: &str) -> StagingGraph {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("set Go language");
    let tree = parser
        .parse(source.as_bytes(), None)
        .expect("parse Go source");
    let mut staging = StagingGraph::new();
    GoGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("q.go"), &mut staging)
        .expect("build_graph should succeed without panic");
    staging
}

#[test]
fn ac15_dense_workspace_smoke() {
    let staging = build(&generate_dense_go(60));

    let channel_nodes = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddNode { entry, .. } if entry.kind == NodeKind::Channel
            )
        })
        .count();

    let mut channel_peers = 0usize;
    let mut instantiates = 0usize;
    for op in staging.operations() {
        if let StagingOp::AddEdge { kind, .. } = op {
            match kind {
                EdgeKind::ChannelPeer { .. } => channel_peers += 1,
                EdgeKind::Instantiates { .. } => instantiates += 1,
                _ => {}
            }
        }
    }

    // 60 workers, one channel each (send + receive + close = 3 peers each),
    // one generic instantiation each.
    assert!(
        channel_nodes >= 50,
        "expected >= 50 Channel nodes, got {channel_nodes}"
    );
    assert!(
        channel_peers >= 100,
        "expected >= 100 ChannelPeer edges, got {channel_peers}"
    );
    assert!(
        instantiates >= 50,
        "expected >= 50 Instantiates edges, got {instantiates}"
    );
}
