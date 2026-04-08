//! Visibility tests for Shell language plugin
//!
//! Shell scripts don't have formal visibility modifiers. All user-defined
//! functions are considered public as they can be called from anywhere
//! within the script or sourced by other scripts.

use sqry_core::graph::{
    GraphBuilder,
    unified::{StagingGraph, build::staging::StagingOp, node::NodeKind},
};
use sqry_lang_shell::ShellGraphBuilder;
use std::collections::HashMap;
use std::path::PathBuf;

fn parse_shell(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("failed to set language");
    parser.parse(source, None).expect("failed to parse")
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn extract_function_visibility(staging: &StagingGraph) -> Vec<(String, Option<String>)> {
    let strings = build_string_lookup(staging);
    let mut results = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Function)
        {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            let visibility = entry
                .visibility
                .and_then(|id| strings.get(&id.index()).cloned());
            results.push((name, visibility));
        }
    }

    results
}

#[test]
fn test_all_functions_are_public() {
    // In shell scripts, all user-defined functions are public
    let source = r#"
#!/bin/bash

public_func() {
    echo "public function"
}

another_func() {
    echo "also public"
}

function bash_style {
    echo "bash style also public"
}
"#;

    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();
    let file = PathBuf::from("test.sh");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let functions = extract_function_visibility(&staging);

    // All functions should be public
    for (name, visibility) in &functions {
        assert_eq!(
            visibility.as_deref(),
            Some("public"),
            "Function '{name}' should have public visibility in shell scripts"
        );
    }

    // Should have 3 functions
    assert_eq!(functions.len(), 3, "Should have 3 functions");
}

#[test]
fn test_posix_syntax_functions_public() {
    // POSIX style: foo() { ... }
    let source = r#"
deploy() {
    echo "deploying"
}

rollback() {
    echo "rolling back"
}
"#;

    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();
    let file = PathBuf::from("deploy.sh");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let functions = extract_function_visibility(&staging);

    assert_eq!(functions.len(), 2, "Should have 2 functions");
    assert!(
        functions
            .iter()
            .all(|(_, visibility)| visibility.as_deref() == Some("public")),
        "All POSIX functions should be public"
    );
}

#[test]
fn test_bash_syntax_functions_public() {
    // Bash style: function foo { ... }
    let source = r#"
function initialize {
    echo "init"
}

function cleanup() {
    echo "cleanup"
}
"#;

    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();
    let file = PathBuf::from("utils.sh");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let functions = extract_function_visibility(&staging);

    assert_eq!(functions.len(), 2, "Should have 2 functions");
    assert!(
        functions
            .iter()
            .all(|(_, visibility)| visibility.as_deref() == Some("public")),
        "All Bash-style functions should be public"
    );
}

#[test]
fn test_empty_script_no_functions() {
    // Empty script should have no functions
    let source = r"
#!/bin/bash
# Just comments, no functions
";

    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();
    let file = PathBuf::from("empty.sh");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let functions = extract_function_visibility(&staging);
    assert_eq!(functions.len(), 0, "Empty script should have no functions");
}

#[test]
fn test_nested_function_calls_public() {
    // Functions that call other functions should all be public
    let source = r#"
main() {
    setup
    process
    cleanup
}

setup() {
    echo "setup"
}

process() {
    echo "process"
}

cleanup() {
    echo "cleanup"
}
"#;

    let tree = parse_shell(source);
    let mut staging = StagingGraph::new();
    let builder = ShellGraphBuilder::default();
    let file = PathBuf::from("workflow.sh");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let functions = extract_function_visibility(&staging);

    assert_eq!(functions.len(), 4, "Should have 4 functions");
    assert!(
        functions
            .iter()
            .all(|(_, visibility)| visibility.as_deref() == Some("public")),
        "All functions should be public"
    );
}
