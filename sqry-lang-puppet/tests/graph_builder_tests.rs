//! Graph builder tests for the Puppet language plugin.
//!
//! Covers:
//! - Class node extraction
//! - Include/require dependency edges
//! - Class inheritance
//! - Defined type extraction
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_puppet::PuppetGraphBuilder;
use std::path::Path;

fn parse_puppet(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_puppet::LANGUAGE.into())
        .expect("failed to set Puppet language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse Puppet code")
}

fn count_edges_of_kind(staging: &StagingGraph, kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

fn count_call_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Calls { .. }))
}

fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::InternString { value, .. } = op {
            value.contains(pattern)
        } else {
            false
        }
    })
}

// ==================== Basic Node Extraction ====================

#[test]
fn test_class_extraction() {
    let source = r"
class myapp::webserver {
    package { 'nginx':
        ensure => installed,
    }

    service { 'nginx':
        ensure  => running,
        require => Package['nginx'],
    }
}
";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("webserver.pp"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 class node, got {}",
        stats.nodes_staged
    );
    assert!(
        has_interned_string_containing(&staging, "myapp::webserver")
            || has_interned_string_containing(&staging, "webserver"),
        "Expected class name in staging"
    );
}

#[test]
fn test_multiple_class_extraction() {
    let source = r"
class myapp::base {
    file { '/etc/myapp':
        ensure => directory,
    }
}

class myapp::config {
    file { '/etc/myapp/config.yml':
        ensure  => file,
        content => 'key: value',
    }
}
";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("init.pp"), &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 class nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Include/Require Edges ====================

#[test]
fn test_include_dependency() {
    let source = r"
class myapp::app {
    include myapp::base
    include myapp::webserver

    file { '/var/www/myapp':
        ensure => directory,
    }
}
";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("app.pp"), &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );

    // Should have call edges for include statements
    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected include edges to produce call edges, got {}",
        call_count
    );
}

#[test]
fn test_require_dependency() {
    let source = r"
class myapp::database {
    require myapp::base

    package { 'postgresql':
        ensure => installed,
    }
}
";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("database.pp"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}

// ==================== Class Inheritance ====================

#[test]
fn test_class_inheritance() {
    let source = r#"
class myapp::base {
    $config_dir = '/etc/myapp'
}

class myapp::advanced inherits myapp::base {
    file { "${config_dir}/advanced.yml":
        ensure => file,
    }
}
"#;
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("classes.pp"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 class nodes, got {}",
        stats.nodes_staged
    );

    // Should have Inherits edge
    let has_inherits = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge { kind, .. } = op {
            matches!(kind, EdgeKind::Inherits)
        } else {
            false
        }
    });
    assert!(has_inherits, "Expected Inherits edge for class inheritance");
}

// ==================== Defined Types ====================

#[test]
fn test_defined_type_extraction() {
    let source = r#"
define myapp::vhost (
    String $server_name,
    Integer $port = 80,
) {
    file { "/etc/nginx/sites-enabled/${server_name}": }
}
"#;
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("vhost.pp"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 defined type node, got {}",
        stats.nodes_staged
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = PuppetGraphBuilder::new();
    assert_eq!(builder.language(), Language::Puppet);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PuppetGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.pp"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty Puppet file should succeed");
}

#[test]
fn test_malformed_puppet() {
    // Incomplete Puppet - tree-sitter is error-tolerant
    let source = r"
class myapp::broken {
    include
"; // incomplete
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.pp"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
# This is a comment
# Another comment
";
    let tree = parse_puppet(source);
    let mut staging = StagingGraph::new();
    let builder = PuppetGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.pp"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only Puppet file should succeed");
}
