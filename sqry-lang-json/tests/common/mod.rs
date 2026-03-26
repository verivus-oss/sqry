//! Shared test helpers for JSON graph builder tests.

#![allow(dead_code)]

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::{EdgeKind, NodeId, StagingGraph};
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_json::JsonPlugin;
use std::collections::HashMap;
use std::path::Path;

pub fn build_staging_from_source(content: &[u8], filename: &str) -> StagingGraph {
    let plugin = JsonPlugin::new();
    let tree = plugin.parse_ast(content).expect("parse_ast should succeed");
    let mut staging = StagingGraph::new();
    let path = Path::new(filename);
    let builder = plugin.graph_builder().expect("graph builder");
    builder
        .build_graph(&tree, content, path, &mut staging)
        .expect("build_graph should succeed");
    staging
}

pub fn build_node_name_map(staging: &StagingGraph) -> HashMap<NodeId, String> {
    let mut node_names = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = staging.resolve_node_canonical_name(entry)
        {
            node_names.insert(*expected_id, name.to_owned());
        }
    }
    node_names
}

pub fn has_node(staging: &StagingGraph, name: &str) -> bool {
    build_node_name_map(staging).values().any(|n| n == name)
}

pub fn has_edge_between<F>(staging: &StagingGraph, from: &str, to: &str, predicate: F) -> bool
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_name_map(staging);
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            if !predicate(kind) {
                return false;
            }
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            matches!(
                (source_name, target_name),
                (Some(s), Some(t)) if s == from && t == to
            )
        } else {
            false
        }
    })
}

pub fn count_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { .. }))
        .count()
}

pub fn node_kind_for(
    staging: &StagingGraph,
    name: &str,
) -> Option<sqry_core::graph::unified::node::NodeKind> {
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && let Some(n) = staging.resolve_node_canonical_name(entry)
            && n == name
        {
            return Some(entry.kind);
        }
    }
    None
}
