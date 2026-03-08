//! Integration tests for TypeScript import nodes and FFI edges.

#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_typescript::TypeScriptPlugin;
use std::collections::HashMap;
use support::unique_ts_path;

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

fn import_names(staging: &StagingGraph) -> Vec<String> {
    let strings = build_string_lookup(staging);
    let mut names = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Import
            && let Some(name) = strings.get(&entry.name.index())
        {
            names.push(name.clone());
        }
    }
    names
}

fn build_graph_from_source(source: &[u8], label: &str) -> StagingGraph {
    let plugin = TypeScriptPlugin::default();
    let file = unique_ts_path(label);
    let tree = plugin.parse_ast(source).expect("Failed to parse AST");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
fn test_import_nodes_created_for_specifiers() {
    let source = br#"
import foo, { bar as baz, type TypeOnly } from "./mod";
import * as ns from "./ns";
import { qux } from "./other";
import type { TypesOnly } from "./types";
"#;

    let staging = build_graph_from_source(source, "imports_nodes");
    let mut names = import_names(&staging);
    names.sort();

    let expected = ["TypeOnly", "TypesOnly", "bar", "foo", "ns", "qux"];

    for name in expected {
        assert!(
            names.contains(&name.to_string()),
            "missing import node: {name}"
        );
    }

    let has_import_edge = staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Imports { .. },
                ..
            }
        )
    });
    assert!(has_import_edge, "expected at least one import edge");
}

#[test]
fn test_ffi_edges_for_wasm_and_native_addons() {
    let source = br#"
function load() {
    WebAssembly.instantiate(fetch("./module.wasm"));
    const native = require("./binding.node");
    process.dlopen(module, "./addon.node");
}
"#;

    let staging = build_graph_from_source(source, "ffi_edges");
    let nodes = build_node_lookup(&staging);

    let mut wasm_targets = Vec::new();
    let mut ffi_targets = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source: _,
            target,
            kind,
            ..
        } = op
        {
            match kind {
                EdgeKind::WebAssemblyCall => {
                    if let Some((name, _)) = nodes.get(target) {
                        wasm_targets.push(name.clone());
                    }
                }
                EdgeKind::FfiCall { .. } => {
                    if let Some((name, _)) = nodes.get(target) {
                        ffi_targets.push(name.clone());
                    }
                }
                _ => {}
            }
        }
    }

    assert!(
        wasm_targets.iter().any(|name| name == "wasm::module.wasm"),
        "missing wasm target for module.wasm"
    );
    assert!(
        ffi_targets
            .iter()
            .any(|name| name == "native::binding.node"),
        "missing FFI target for binding.node"
    );
    assert!(
        ffi_targets.iter().any(|name| name == "native::addon.node"),
        "missing FFI target for addon.node"
    );
}
