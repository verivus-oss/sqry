//! Integration tests for C language plugin (graph-native).

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_c::CPlugin;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
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

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn has_export_edge(staging: &StagingGraph, module: &str, target: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target: to,
            kind,
            ..
        } = op
        {
            if !matches!(kind, EdgeKind::Exports { .. }) {
                continue;
            }
            let source_name = nodes.get(source).map(|(name, kind)| (name, kind));
            let target_name = nodes.get(to).map(|(name, kind)| (name, kind));
            if let (Some((source_name, NodeKind::Module)), Some((target_name, _))) =
                (source_name, target_name)
                && source_name == module
                && target_name == target
            {
                return true;
            }
        }
    }
    false
}

fn has_import_edge(staging: &StagingGraph, module: &str, target: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target: to,
            kind,
            ..
        } = op
        {
            if !matches!(kind, EdgeKind::Imports { .. }) {
                continue;
            }
            let source_name = nodes.get(source).map(|(name, kind)| (name, kind));
            let target_name = nodes.get(to).map(|(name, kind)| (name, kind));
            if let (Some((source_name, NodeKind::Module)), Some((target_name, NodeKind::Import))) =
                (source_name, target_name)
                && source_name == module
                && target_name == target
            {
                return true;
            }
        }
    }
    false
}

fn build_graph_from_source(source: &str) -> (StagingGraph, String) {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.c");
    fs::write(&file, source).expect("write test source");

    let plugin = CPlugin::default();
    let content = fs::read(&file).expect("read test source");
    let tree = plugin.parse_ast(&content).expect("parse C source");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();

    builder
        .build_graph(&tree, &content, &file, &mut staging)
        .expect("build graph");

    let file_name = PathBuf::from("test.c")
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("test.c")
        .to_string();
    (staging, file_name)
}

#[test]
fn test_functions_and_exports() {
    let source = r"
static void helper(void) {
    // Helper function
}

void public_function(void) {
    helper();
}
";

    let (staging, module_name) = build_graph_from_source(source);

    assert!(find_node(&staging, "helper", NodeKind::Function));
    assert!(find_node(&staging, "public_function", NodeKind::Function));

    assert!(
        !has_export_edge(&staging, &module_name, "helper"),
        "static helper should not be exported"
    );
    assert!(
        has_export_edge(&staging, &module_name, "public_function"),
        "public_function should be exported"
    );
}

#[test]
fn test_struct_enum_typedef_nodes() {
    let source = r"
struct Point {
    int x;
    int y;
};

enum Color {
    RED = 0,
    GREEN = 1,
    BLUE = 2
};

typedef struct Point Point;

typedef int (*callback_fn)(int, int);
";

    let (staging, _module_name) = build_graph_from_source(source);

    assert!(find_node(&staging, "Point", NodeKind::Struct));
    assert!(find_node(&staging, "Color", NodeKind::Enum));
    assert!(find_node(&staging, "callback_fn", NodeKind::Type));
}

#[test]
fn test_macro_and_variable_nodes() {
    let source = r"
#define MAX_SIZE 100
#define MIN(a, b) ((a) < (b) ? (a) : (b))

int buffer[MAX_SIZE];
";

    let (staging, module_name) = build_graph_from_source(source);

    assert!(find_node(&staging, "MAX_SIZE", NodeKind::Constant));
    assert!(find_node(&staging, "MIN", NodeKind::Constant));
    assert!(find_node(&staging, "buffer", NodeKind::Variable));

    assert!(
        has_export_edge(&staging, &module_name, "MAX_SIZE"),
        "macro MAX_SIZE should be exported"
    );
}

#[test]
fn test_header_import_edges() {
    let source = r#"
#include <stdio.h>
#include "user.h"

int main(void) { return 0; }
"#;

    let (staging, module_name) = build_graph_from_source(source);

    assert!(has_import_edge(&staging, &module_name, "stdio.h"));
    assert!(has_import_edge(&staging, &module_name, "user.h"));
}
