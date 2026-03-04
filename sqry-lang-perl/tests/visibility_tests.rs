use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_perl::relations::PerlGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_perl(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_perl::LANGUAGE.into())
        .expect("Failed to set Perl language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Perl code")
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

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_function_visibility_public() {
    let source = r#"
sub public_function {
    return "public";
}
"#;
    let tree = parse_perl(source);
    let mut staging = StagingGraph::new();
    let builder = PerlGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.pl"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "public_function");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Subroutine without underscore prefix should have public visibility"
    );
}

#[test]
fn test_function_visibility_private() {
    let source = r#"
sub _private_function {
    return "private";
}
"#;
    let tree = parse_perl(source);
    let mut staging = StagingGraph::new();
    let builder = PerlGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.pl"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "_private");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Subroutine with underscore prefix should have private visibility"
    );
}

#[test]
fn test_function_visibility_mixed() {
    let source = r#"
sub public_api {
    return "public";
}

sub _private_helper {
    return "private";
}
"#;
    let tree = parse_perl(source);
    let mut staging = StagingGraph::new();
    let builder = PerlGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.pl"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(
        find_function_visibility(&staging, "public_api"),
        Some("public".to_string()),
        "public_api should be public (no underscore)"
    );
    assert_eq!(
        find_function_visibility(&staging, "_private"),
        Some("private".to_string()),
        "_private_helper should be private (underscore prefix)"
    );
}
