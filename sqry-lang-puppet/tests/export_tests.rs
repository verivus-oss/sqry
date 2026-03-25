//! Tests for Puppet module export edge creation.

use sqry_core::graph::Language;
use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_puppet::PuppetPlugin;
use std::fs;
use tempfile::TempDir;

fn build_node_lookup(staging: &StagingGraph) -> Vec<(NodeId, String, String, NodeKind)> {
    let mut nodes = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let canonical_name = staging
                .resolve_node_canonical_name(entry)
                .map(str::to_owned)
                .unwrap_or_default();
            let display_name = staging
                .resolve_node_display_name(Language::Puppet, entry)
                .unwrap_or_else(|| canonical_name.clone());
            nodes.push((*node_id, canonical_name, display_name, entry.kind));
        }
    }
    nodes
}

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = PuppetPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.pp");
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
            let target_name = nodes.iter().find_map(|(node_id, canonical_name, _, _)| {
                (*node_id == *target).then_some(canonical_name.as_str())
            });
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

fn has_display_export_edge(staging: &StagingGraph, exported_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let target_name = nodes.iter().find_map(|(node_id, _, display_name, _)| {
                (*node_id == *target).then_some(display_name.as_str())
            });
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

// ===== Export Edge Tests =====

#[test]
fn test_class_definition_exported() {
    let content = b"\
class apache {
  package { 'apache2':
    ensure => installed,
  }
}

class nginx {
  package { 'nginx':
    ensure => installed,
  }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "apache"),
        "Expected export edge for class apache"
    );
    assert!(
        has_export_edge(&staging, "nginx"),
        "Expected export edge for class nginx"
    );
}

#[test]
fn test_defined_type_exported() {
    let content = b"\
define webserver::vhost (
  $port,
  $docroot,
) {
  file { \"${docroot}\":
    ensure => directory,
  }
}

define database::user (
  $username,
  $password,
) {
  exec { \"create-user-${username}\":
    command => \"/usr/bin/createuser ${username}\",
  }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "webserver::vhost"),
        "Expected export edge for defined type webserver::vhost"
    );
    assert!(
        has_export_edge(&staging, "database::user"),
        "Expected export edge for defined type database::user"
    );
    assert!(
        has_display_export_edge(&staging, "webserver.vhost"),
        "Expected display/native export edge for defined type webserver.vhost"
    );
}

#[test]
fn test_class_with_inheritance_exported() {
    let content = b"\
class apache::base {
  package { 'apache2':
    ensure => installed,
  }
}

class apache::ssl inherits apache::base {
  package { 'apache2-ssl':
    ensure => installed,
  }
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "apache::base"),
        "Expected export edge for base class"
    );
    assert!(
        has_export_edge(&staging, "apache::ssl"),
        "Expected export edge for derived class"
    );
}

#[test]
fn test_mixed_classes_and_defines() {
    let content = b"\
class mymodule {
  notify { 'hello': }
}

define mymodule::resource (
  $param,
) {
  file { $param:
    ensure => present,
  }
}

class mymodule::subclass {
  include mymodule
}
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "mymodule"),
        "Expected export edge for class mymodule"
    );
    assert!(
        has_export_edge(&staging, "mymodule::resource"),
        "Expected export edge for defined type mymodule::resource"
    );
    assert!(
        has_export_edge(&staging, "mymodule::subclass"),
        "Expected export edge for class mymodule::subclass"
    );
}
