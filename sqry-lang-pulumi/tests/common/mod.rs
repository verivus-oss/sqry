//! Shared test helpers for Pulumi graph builder tests.

#![allow(dead_code)]

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::{EdgeKind, NodeId, StagingGraph};
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_pulumi::PulumiPlugin;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pulumi")
        .join(name)
}

pub fn build_staging_graph_from_fixture(name: &str) -> StagingGraph {
    let content = std::fs::read_to_string(fixture_path(name)).expect("fixture should be readable");
    let plugin = PulumiPlugin::default();
    let tree = plugin
        .parse_ast(content.as_bytes())
        .expect("parse_ast should succeed");
    let mut staging = StagingGraph::new();
    let file_path = fixture_path(name);
    let builder = plugin.graph_builder().expect("graph builder");
    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("build_graph should succeed");
    staging
}

pub fn build_staging_from_yaml(source: &str) -> StagingGraph {
    build_staging_from_source(source.as_bytes(), "Pulumi.yaml")
}

pub fn build_staging_from_json(source: &str) -> StagingGraph {
    build_staging_from_source(source.as_bytes(), "Pulumi.json")
}

fn build_staging_from_source(content: &[u8], filename: &str) -> StagingGraph {
    let plugin = PulumiPlugin::default();
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

pub fn build_display_name_map(staging: &StagingGraph) -> HashMap<NodeId, String> {
    let mut node_names = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = staging.resolve_node_display_name(Language::Pulumi, entry)
        {
            node_names.insert(*expected_id, name.to_owned());
        }
    }
    node_names
}

pub fn has_node(staging: &StagingGraph, name: &str) -> bool {
    build_node_name_map(staging).values().any(|n| n == name)
}

pub fn has_display_node(staging: &StagingGraph, name: &str) -> bool {
    build_display_name_map(staging).values().any(|n| n == name)
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

pub fn count_edges<F>(staging: &StagingGraph, predicate: F) -> usize
where
    F: Fn(&EdgeKind) -> bool,
{
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                predicate(kind)
            } else {
                false
            }
        })
        .count()
}

pub fn count_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { .. }))
        .count()
}

/// Count nodes whose name starts with the given prefix.
pub fn count_nodes_with_prefix(staging: &StagingGraph, prefix: &str) -> usize {
    build_node_name_map(staging)
        .values()
        .filter(|name| name.starts_with(prefix))
        .count()
}

/// Count Reference edges from a specific source to a specific target.
pub fn count_reference_edges_between(staging: &StagingGraph, from: &str, to: &str) -> usize {
    let node_names = build_node_name_map(staging);
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                matches!(kind, EdgeKind::References)
                    && node_names.get(source).map(String::as_str) == Some(from)
                    && node_names.get(target).map(String::as_str) == Some(to)
            } else {
                false
            }
        })
        .count()
}
