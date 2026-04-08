//! Graph builder tests for the Shell language plugin.
//!
//! Covers:
//! - Function node extraction (POSIX and Bash syntax)
//! - Call edge detection
//! - Source/. import edges
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_shell::ShellGraphBuilder;
use std::path::Path;

fn parse_shell(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("failed to set Bash language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse shell code")
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

fn count_import_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Imports { .. }))
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
fn test_posix_function_extraction() {
    let source = r#"
#!/bin/sh

greet() {
    echo "Hello, $1"
}

add() {
    echo $(($1 + $2))
}

greet "World"
add 3 4
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("script.sh"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 function nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        has_interned_string_containing(&staging, "greet"),
        "Expected 'greet' function"
    );
}

#[test]
fn test_bash_function_keyword() {
    let source = r#"
#!/bin/bash

function setup() {
    mkdir -p /tmp/test
    echo "Setup done"
}

function teardown() {
    rm -rf /tmp/test
    echo "Cleanup done"
}
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.sh"), &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 function nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        has_interned_string_containing(&staging, "setup"),
        "Expected 'setup' function"
    );
}

#[test]
fn test_mixed_function_styles() {
    let source = r#"
#!/bin/bash

# POSIX style
posix_func() {
    echo "posix"
}

# Bash style
function bash_func {
    echo "bash"
}

posix_func
bash_func
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("mixed.sh"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 function nodes, got {}",
        stats.nodes_staged
    );
}

// ==================== Call Edge Detection ====================

#[test]
fn test_call_edge_detection() {
    let source = r#"
#!/bin/bash

helper() {
    echo "I am helper"
}

main() {
    helper
    echo "Done"
}

main
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("main.sh"), &mut staging)
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 1,
        "Expected at least 1 call edge, got {call_count}"
    );
}

#[test]
fn test_nested_function_calls() {
    let source = r#"
#!/bin/bash

level3() {
    echo "level 3"
}

level2() {
    level3
    echo "level 2"
}

level1() {
    level2
    echo "level 1"
}
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("levels.sh"),
            &mut staging,
        )
        .unwrap();

    let call_count = count_call_edges(&staging);
    assert!(
        call_count >= 2,
        "Expected at least 2 call edges, got {call_count}"
    );
}

// ==================== Source/. Import Edges ====================

#[test]
fn test_source_import() {
    let source = r#"
#!/bin/bash

source ./utils.sh
source /etc/environment

main() {
    log_message "Starting"
}
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("main.sh"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for source, got {import_count}"
    );
}

#[test]
fn test_dot_source_import() {
    let source = r"
#!/bin/sh

. ./lib/helpers.sh
. ./lib/config.sh

run() {
    init_config
    do_work
}
";
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new("run.sh"), &mut staging)
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for . sourcing, got {import_count}"
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = ShellGraphBuilder::default();
    assert_eq!(builder.language(), Language::Shell);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ShellGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.sh"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty shell file should succeed");
}

#[test]
fn test_malformed_shell() {
    // Incomplete shell - tree-sitter is error-tolerant
    let source = r"
function broken(
"; // incomplete
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.sh"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
# This is a comment
# Another comment
## Section header
";
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.sh"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only shell file should succeed");
}

#[test]
fn test_script_without_functions() {
    let source = r#"
#!/bin/bash

set -euo pipefail

echo "Starting script"
mkdir -p /tmp/output
cp /etc/hosts /tmp/output/
echo "Done"
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("simple.sh"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Script without functions should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_complete_script() {
    let source = r#"
#!/bin/bash

source ./config.sh

log() {
    echo "[$(date)] $1"
}

check_deps() {
    command -v curl >/dev/null 2>&1 || { echo "curl required"; exit 1; }
    command -v jq >/dev/null 2>&1 || { echo "jq required"; exit 1; }
}

download() {
    local url="$1"
    local dest="$2"
    curl -fsSL "$url" -o "$dest"
}

main() {
    check_deps
    log "Starting download"
    download "https://example.com/file" "/tmp/file"
    log "Done"
}

main "$@"
"#;
    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("deploy.sh"),
            &mut staging,
        )
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 function nodes, got {}",
        stats.nodes_staged
    );
}
