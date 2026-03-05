//! Tests for Perl package export edge creation.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_perl::PerlPlugin;
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
    let plugin = PerlPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.pl");
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
fn test_public_subroutines_exported() {
    let content = b"\
package MyModule;

sub public_sub {
    my $self = shift;
    return 42;
}

sub another_public {
    return 'hello';
}

1;
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "MyModule::public_sub"),
        "Expected export edge for MyModule::public_sub"
    );
    assert!(
        has_export_edge(&staging, "MyModule::another_public"),
        "Expected export edge for MyModule::another_public"
    );
}

#[test]
fn test_private_subroutines_not_exported() {
    let content = b"\
package MyModule;

sub public_sub {
    return 42;
}

sub _private_sub {
    return 'secret';
}

1;
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "MyModule::public_sub"),
        "Expected export edge for MyModule::public_sub"
    );
    assert!(
        !has_export_edge(&staging, "MyModule::_private_sub"),
        "Should NOT have export edge for MyModule::_private_sub (private)"
    );
}

#[test]
fn test_main_package_exports() {
    let content = b"\
# No explicit package declaration = main package

sub process_data {
    my $data = shift;
    return $data * 2;
}

sub _internal_helper {
    return 1;
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "main::process_data"),
        "Expected export edge for main::process_data"
    );
    assert!(
        !has_export_edge(&staging, "main::_internal_helper"),
        "Should NOT have export edge for main::_internal_helper (private)"
    );
}

#[test]
fn test_multiple_packages() {
    let content = b"\
package FirstPackage;

sub first_public {
    return 1;
}

package SecondPackage;

sub second_public {
    return 2;
}

sub _second_private {
    return 3;
}

1;
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "FirstPackage::first_public"),
        "Expected export edge for FirstPackage::first_public"
    );
    assert!(
        has_export_edge(&staging, "SecondPackage::second_public"),
        "Expected export edge for SecondPackage::second_public"
    );
    assert!(
        !has_export_edge(&staging, "SecondPackage::_second_private"),
        "Should NOT have export edge for SecondPackage::_second_private (private)"
    );
}
