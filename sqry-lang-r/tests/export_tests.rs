//! Tests for R package export edge creation.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_r::RPlugin;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = RPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.R");
    fs::write(&file, source).expect("write test source");
    let tree = plugin.parse_ast(source).expect("parse source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

fn has_export_edge(staging: &StagingGraph, exported_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

// ===== Export Edge Tests =====

#[test]
fn test_public_functions_exported() {
    let content = b"\
# Public functions (no dot prefix)
process_data <- function(x) {
  x * 2
}

calculate_mean <- function(values) {
  mean(values)
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "process_data"),
        "Expected export edge for process_data"
    );
    assert!(
        has_export_edge(&staging, "calculate_mean"),
        "Expected export edge for calculate_mean"
    );
}

#[test]
fn test_private_functions_not_exported() {
    let content = b"\
# Public function
public_function <- function(x) {
  .internal_helper(x)
}

# Private function (dot prefix)
.internal_helper <- function(x) {
  x + 1
}

.another_internal <- function() {
  42
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "public_function"),
        "Expected export edge for public_function"
    );
    assert!(
        !has_export_edge(&staging, ".internal_helper"),
        "Should NOT have export edge for .internal_helper (private)"
    );
    assert!(
        !has_export_edge(&staging, ".another_internal"),
        "Should NOT have export edge for .another_internal (private)"
    );
}

#[test]
fn test_s3_methods_exported() {
    let content = b"\
# S3 generic and method
print.myclass <- function(x) {
  cat(\"MyClass object\\n\")
}

# Regular function
create_myclass <- function(value) {
  structure(list(value = value), class = \"myclass\")
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "print.myclass"),
        "Expected export edge for S3 method print.myclass"
    );
    assert!(
        has_export_edge(&staging, "create_myclass"),
        "Expected export edge for create_myclass"
    );
}

#[test]
fn test_mixed_visibility() {
    let content = b"\
# Public API
api_function <- function(x) {
  .validate(x)
  .process(x)
}

# Internal helpers
.validate <- function(x) {
  !is.null(x)
}

.process <- function(x) {
  x * 2
}

# Another public function
export_this <- function() {
  \"exported\"
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "api_function"),
        "Expected export edge for api_function"
    );
    assert!(
        has_export_edge(&staging, "export_this"),
        "Expected export edge for export_this"
    );
    assert!(
        !has_export_edge(&staging, ".validate"),
        "Should NOT have export edge for .validate (private)"
    );
    assert!(
        !has_export_edge(&staging, ".process"),
        "Should NOT have export edge for .process (private)"
    );
}
