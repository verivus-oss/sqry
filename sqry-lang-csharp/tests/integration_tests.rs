//! Integration tests for C# language plugin (graph-native).
//!
//! Validates:
//! - Class/interface/method node extraction
//! - Async/static flags on methods
//! - Inherits/Implements edges
//! - Import edges from using directives
//! - Export edges for public members

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_csharp::CSharpPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

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
            let name = entry
                .qualified_name
                .and_then(|id| strings.get(&id.index()))
                .cloned()
                .or_else(|| strings.get(&entry.name.index()).cloned())
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    name: &str,
    kind: NodeKind,
) -> Option<&'a NodeEntry> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
        {
            let node_name = entry
                .qualified_name
                .and_then(|id| strings.get(&id.index()))
                .or_else(|| strings.get(&entry.name.index()));
            if node_name.is_some_and(|n| n == name) {
                return Some(entry);
            }
        }
    }
    None
}

fn canonical_csharp_member(owner: &str, member: &str) -> String {
    format!("{owner}::{member}")
}

fn build_graph(source: &[u8]) -> StagingGraph {
    let plugin = CSharpPlugin::default();
    let file = PathBuf::from("test.cs");
    let tree = plugin.parse_ast(source).expect("parse failed");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
fn test_class_and_methods() {
    let source = br#"
public class MyClass
{
    public void PublicMethod() { }
    private void PrivateMethod() { }
}
"#;
    let staging = build_graph(source);

    assert!(
        find_node_entry(&staging, "MyClass", NodeKind::Class).is_some(),
        "MyClass class node not found"
    );
    assert!(
        find_node_entry(
            &staging,
            &canonical_csharp_member("MyClass", "PublicMethod"),
            NodeKind::Method,
        )
        .is_some(),
        "PublicMethod method node not found"
    );
    assert!(
        find_node_entry(
            &staging,
            &canonical_csharp_member("MyClass", "PrivateMethod"),
            NodeKind::Method,
        )
        .is_some(),
        "PrivateMethod method node not found"
    );
}

#[test]
fn test_async_static_methods() {
    let source = br#"
using System.Threading.Tasks;

public class AsyncService
{
    public async Task<int> GetDataAsync()
    {
        await Task.Delay(100);
        return 42;
    }

    public static async Task ProcessAsync()
    {
        await Task.Delay(100);
    }
}
"#;
    let staging = build_graph(source);

    let get_data = find_node_entry(
        &staging,
        &canonical_csharp_member("AsyncService", "GetDataAsync"),
        NodeKind::Method,
    )
    .expect("GetDataAsync method not found");
    assert!(get_data.is_async, "GetDataAsync should be async");
    assert!(!get_data.is_static, "GetDataAsync should not be static");

    let process = find_node_entry(
        &staging,
        &canonical_csharp_member("AsyncService", "ProcessAsync"),
        NodeKind::Method,
    )
    .expect("ProcessAsync method not found");
    assert!(process.is_async, "ProcessAsync should be async");
    assert!(process.is_static, "ProcessAsync should be static");
}

#[test]
fn test_inherits_and_implements_edges() {
    let source = br#"
public interface IService
{
    void Execute();
}

public class Base
{
}

public class Derived : Base, IService
{
    public void Execute() { }
}
"#;
    let staging = build_graph(source);
    let nodes = build_node_lookup(&staging);

    let mut inherits = false;
    let mut implements = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            match kind {
                EdgeKind::Inherits => {
                    let source_name = nodes.get(source).map(|(name, _)| name.as_str());
                    let target_name = nodes.get(target).map(|(name, _)| name.as_str());
                    if source_name == Some("Derived") && target_name == Some("Base") {
                        inherits = true;
                    }
                }
                EdgeKind::Implements => {
                    let source_name = nodes.get(source).map(|(name, _)| name.as_str());
                    let target_name = nodes.get(target).map(|(name, _)| name.as_str());
                    if source_name == Some("Derived") && target_name == Some("IService") {
                        implements = true;
                    }
                }
                _ => {}
            }
        }
    }

    assert!(inherits, "Expected Derived to inherit Base");
    assert!(implements, "Expected Derived to implement IService");
}

#[test]
fn test_import_edges_from_using() {
    let source = br#"
using System;
using System.Collections.Generic;
using IO = System.IO;

public class Importer
{
    public void Use() { }
}
"#;
    let staging = build_graph(source);

    let mut import_count = 0;
    for op in staging.operations() {
        if let StagingOp::AddEdge { kind, .. } = op
            && matches!(kind, EdgeKind::Imports { .. })
        {
            import_count += 1;
        }
    }

    assert!(import_count >= 3, "Expected at least 3 import edges");
}

#[test]
fn test_export_edges_for_public_members() {
    let source = br#"
public class Exported
{
    public int Count { get; set; }
    public void PublicMethod() { }
    private void PrivateMethod() { }
}
"#;
    let staging = build_graph(source);
    let nodes = build_node_lookup(&staging);

    let mut exports = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && matches!(kind, EdgeKind::Exports { .. })
        {
            let source_name = nodes.get(source).map(|(name, _)| name.clone());
            let target_name = nodes.get(target).map(|(name, _)| name.clone());
            exports.push((source_name, target_name));
        }
    }

    let exported_method = exports.iter().any(|(_, target)| {
        target.as_deref() == Some(canonical_csharp_member("Exported", "PublicMethod").as_str())
    });
    let exported_property = exports.iter().any(|(_, target)| {
        target.as_deref() == Some(canonical_csharp_member("Exported", "Count").as_str())
    });

    assert!(exported_method, "Expected export edge for PublicMethod");
    assert!(exported_property, "Expected export edge for Count property");
}
