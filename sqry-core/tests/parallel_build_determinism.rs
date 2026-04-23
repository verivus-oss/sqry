//! Parallel build determinism regression tests.
//!
//! Verifies that building the unified graph with different thread counts
//! produces identical results. This catches non-determinism from race
//! conditions, unordered iteration over concurrent data structures, or
//! thread-dependent insertion order.
//!
//! Tests compare sorted node names and edge endpoint pairs — not just
//! counts — to detect content drift that count-only checks would miss.

use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::plugin::PluginManager;
use sqry_lang_rust::RustPlugin;

/// Create a `PluginManager` with the Rust plugin (sufficient for the
/// `cli-basic` fixture which contains only `.rs` files).
fn create_rust_plugin_manager() -> PluginManager {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));
    manager
}

/// Resolve the `test-fixtures/cli-basic` path relative to the workspace root.
fn cli_basic_fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-core should have a parent directory")
        .join("test-fixtures/cli-basic")
}

/// A comparable snapshot of graph content, derived from a `CodeGraph`.
///
/// Uses sorted vectors so comparison is order-independent.
#[derive(Debug, PartialEq, Eq)]
struct GraphSnapshot {
    node_count: usize,
    edge_count: usize,
    /// Sorted list of `(node_name, node_kind_debug)` pairs.
    node_names: Vec<(String, String)>,
    /// Sorted list of `(source_name, target_name, edge_kind_debug)` triples.
    edge_triples: Vec<(String, String, String)>,
}

/// Build the graph with an explicit thread count and return a comparable snapshot.
fn build_with_threads(threads: usize) -> GraphSnapshot {
    let plugins = create_rust_plugin_manager();
    let config = BuildConfig {
        num_threads: Some(threads),
        ..BuildConfig::default()
    };
    let graph = build_unified_graph(&cli_basic_fixtures(), &plugins, &config)
        .expect("graph build should succeed");
    snapshot(&graph)
}

/// Extract a deterministic, sorted snapshot from a `CodeGraph`.
fn snapshot(graph: &CodeGraph) -> GraphSnapshot {
    let strings = graph.strings();

    let nodes = graph.nodes();
    let edges = graph.edges();

    let mut node_names: Vec<(String, String)> = nodes
        .iter()
        .map(|(_id, entry)| {
            let name = strings
                .resolve(entry.name)
                .map_or_else(|| "<unresolved>".to_string(), |s| s.to_string());
            let kind = format!("{:?}", entry.kind);
            (name, kind)
        })
        .collect();
    node_names.sort();

    // Build edge triples by iterating all nodes and their outgoing edges.
    let mut edge_triples: Vec<(String, String, String)> = Vec::new();
    for (src_id, src_entry) in nodes.iter() {
        let src_name = strings
            .resolve(src_entry.name)
            .map_or_else(|| format!("node#{}", src_id.index()), |s| s.to_string());
        for edge_ref in edges.edges_from(src_id) {
            let tgt_name = nodes
                .get(edge_ref.target)
                .and_then(|e| strings.resolve(e.name))
                .map_or_else(
                    || format!("node#{}", edge_ref.target.index()),
                    |s| s.to_string(),
                );
            let kind = format!("{:?}", edge_ref.kind);
            edge_triples.push((src_name.clone(), tgt_name, kind));
        }
    }
    edge_triples.sort();

    GraphSnapshot {
        node_count: graph.node_count(),
        edge_count: graph.edge_count(),
        node_names,
        edge_triples,
    }
}

/// Serial (1-thread) and parallel (4-thread) builds must produce graphs
/// with identical content.
#[test]
fn serial_and_parallel_builds_produce_same_content() {
    let serial = build_with_threads(1);
    let parallel = build_with_threads(4);

    assert!(
        serial.node_count > 0,
        "serial build should produce at least one node"
    );
    assert!(
        serial.edge_count > 0,
        "serial build should produce at least one edge"
    );

    assert_eq!(
        serial, parallel,
        "serial vs parallel(4) graph content differs"
    );
}

/// Multiple parallel builds with the same thread count must be stable
/// (no run-to-run variance).
#[test]
fn repeated_parallel_builds_are_stable() {
    let first = build_with_threads(4);
    let second = build_with_threads(4);
    let third = build_with_threads(4);

    assert_eq!(first, second, "parallel builds 1 and 2 differ");
    assert_eq!(second, third, "parallel builds 2 and 3 differ");
}

/// Different non-trivial thread counts must all agree on content.
#[test]
fn varying_thread_counts_agree() {
    let baseline = build_with_threads(1);
    for threads in [2, 3, 4, 8] {
        let result = build_with_threads(threads);
        assert_eq!(
            baseline, result,
            "thread count {threads} produced different graph content vs serial baseline"
        );
    }
}

/// After unification, no call-compatible node group should have a mix of
/// line-0 and line-nonzero members (would indicate a unification failure).
///
/// Stub-only groups (external references like `println!`) are acceptable
/// at line 0 — they reference symbols outside the workspace.
///
/// This is a line-zero holistic fix (HU08) regression guard added to the
/// existing determinism suite.
#[test]
fn no_line_zero_in_call_compatible_nodes() {
    use sqry_core::graph::unified::node::{NodeId, NodeKind};
    use std::collections::HashMap;

    let call_compatible = &[
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Macro,
        NodeKind::Constant,
        NodeKind::LambdaTarget,
    ];

    let graph = {
        let plugins = create_rust_plugin_manager();
        let config = BuildConfig::default();
        build_unified_graph(&cli_basic_fixtures(), &plugins, &config)
            .expect("graph build should succeed")
    };

    let strings = graph.strings();
    let files = graph.files();

    // Group call-compatible nodes by qualified name
    let mut groups: HashMap<String, Vec<(NodeId, u32, bool)>> = HashMap::new();
    for (node_id, entry) in graph.nodes().iter() {
        if !call_compatible.contains(&entry.kind) {
            continue;
        }
        let key = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .or_else(|| strings.resolve(entry.name))
            .map(|s| s.to_string())
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let is_external = files.is_external(entry.file);
        groups
            .entry(key)
            .or_default()
            .push((node_id, entry.start_line, is_external));
    }

    for (qn, members) in &groups {
        let has_real_span = members.iter().any(|(_, line, _)| *line > 0);
        if !has_real_span {
            continue; // Stub-only group — acceptable
        }
        for (node_id, line, is_external) in members {
            if *line > 0 || *is_external {
                continue;
            }
            panic!(
                "Node {node_id:?} ({qn}) has start_line == 0 but a sibling has a \
                 real span — unification should have eliminated this"
            );
        }
    }
}
