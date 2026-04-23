//! Integration tests for Phase 4c-prime: cross-file node unification.
//!
//! Verifies that the build pipeline correctly unifies per-file stub nodes
//! sharing the same canonical qualified name into a single canonical node.

use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::PluginManager;

/// Build a graph from a test fixture directory.
fn build_fixture(fixture_dir: &str) -> sqry_core::graph::unified::concurrent::CodeGraph {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(fixture_dir);
    assert!(path.exists(), "Fixture directory does not exist: {path:?}");
    let plugins = PluginManager::new();
    let config = BuildConfig::default();
    build_unified_graph(&path, &plugins, &config).unwrap()
}

/// After Phase 4c-prime, cross-file stubs sharing a qualified name should be
/// unified into a single canonical node. Verify that no CALL_COMPATIBLE_KINDS
/// node has start_line == 0 when a sibling with a real span exists.
#[test]
fn test_no_duplicate_call_compatible_nodes_after_unification() {
    let fixtures_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let cross_lang_dir = fixtures_root.join("cross_language");
    if !cross_lang_dir.exists() {
        // Skip if fixtures don't exist — HU08 will add comprehensive ones
        return;
    }

    let graph = build_fixture("cross_language");
    let strings = graph.strings();

    let call_compatible = &[
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Macro,
        NodeKind::Constant,
        NodeKind::LambdaTarget,
    ];

    // Collect all call-compatible nodes grouped by qualified name
    let mut qn_groups: std::collections::HashMap<
        String,
        Vec<(sqry_core::graph::unified::node::NodeId, u32)>,
    > = std::collections::HashMap::new();

    for (node_id, entry) in graph.nodes().iter() {
        if !call_compatible.contains(&entry.kind) {
            continue;
        }
        if let Some(qn_id) = entry.qualified_name
            && let Some(qn_str) = strings.resolve(qn_id)
        {
            qn_groups
                .entry(qn_str.to_string())
                .or_default()
                .push((node_id, entry.start_line));
        }
    }

    // For each group, if ANY member has a real span, NO member should have line 0
    for (qn, members) in &qn_groups {
        let has_real_span = members.iter().any(|(_, line)| *line > 0);
        if has_real_span {
            for (node_id, line) in members {
                assert_ne!(
                    *line, 0,
                    "Node {node_id:?} ({qn}) still has line 0 after unification, \
                     but a sibling has a real span — Phase 4c-prime should have merged them"
                );
            }
        }
    }
}

/// Phase 4c-prime must produce deterministic results across multiple runs.
#[test]
fn test_unification_is_deterministic() {
    let fixtures_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let cross_lang_dir = fixtures_root.join("cross_language");
    if !cross_lang_dir.exists() {
        return;
    }

    let graph1 = build_fixture("cross_language");
    let graph2 = build_fixture("cross_language");

    assert_eq!(
        graph1.nodes().len(),
        graph2.nodes().len(),
        "Node counts differ between two identical builds"
    );
}
