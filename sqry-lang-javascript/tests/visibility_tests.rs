//! Tests for JavaScript visibility inference.

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::{StagingGraph, StringId};
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_javascript::JavaScriptGraphBuilder;
use std::collections::HashMap;
use std::path::PathBuf;

fn parse_js(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .unwrap();
    parser.parse(source.as_bytes(), None).unwrap()
}

/// Build a map from `StringId` to string value from staging operations
fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((*local_id, value.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn has_visibility_metadata(staging: &StagingGraph, name: &str, expected_visibility: &str) -> bool {
    let string_map = build_string_map(staging);

    // Visibility assertions should use canonical graph identity, not the raw leaf name.
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && let Some(node_name) = staging.resolve_node_canonical_name(entry)
            && node_name == name
            && let Some(vis_id) = entry.visibility
            && let Some(vis_str) = string_map.get(&vis_id)
            && vis_str == expected_visibility
        {
            return true;
        }
    }
    false
}

fn has_display_name(
    staging: &StagingGraph,
    canonical_name: &str,
    expected_display_name: &str,
) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            staging.resolve_node_canonical_name(entry) == Some(canonical_name)
                && staging
                    .resolve_node_display_name(Language::JavaScript, entry)
                    .as_deref()
                    == Some(expected_display_name)
        } else {
            false
        }
    })
}

#[test]
fn test_public_function_convention() {
    let source = r"
function publicFunction() {
    return 42;
}
";

    let tree = parse_js(source);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();
    let file = PathBuf::from("test.js");

    let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_visibility_metadata(&staging, "publicFunction", "public"),
        "Regular functions should have public visibility"
    );
}

#[test]
fn test_private_function_convention() {
    let source = r"
function _privateFunction() {
    return 42;
}
";

    let tree = parse_js(source);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();
    let file = PathBuf::from("test.js");

    let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_visibility_metadata(&staging, "_privateFunction", "private"),
        "Functions with leading underscore should have private visibility"
    );
}

#[test]
fn test_class_method_visibility() {
    let source = r"
class MyClass {
    publicMethod() {
        return this._privateMethod();
    }

    _privateMethod() {
        return 42;
    }
}
";

    let tree = parse_js(source);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();
    let file = PathBuf::from("test.js");

    let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_visibility_metadata(&staging, "MyClass::publicMethod", "public"),
        "Regular methods should have public visibility"
    );

    assert!(
        has_visibility_metadata(&staging, "MyClass::_privateMethod", "private"),
        "Methods with leading underscore should have private visibility"
    );

    assert!(
        has_display_name(&staging, "MyClass::publicMethod", "MyClass.publicMethod"),
        "Class methods should display with JavaScript native dot syntax"
    );
    assert!(
        has_display_name(
            &staging,
            "MyClass::_privateMethod",
            "MyClass._privateMethod"
        ),
        "Private class methods should display with JavaScript native dot syntax"
    );
}

#[test]
fn test_async_function_visibility() {
    let source = r"
async function publicAsync() {
    return await _privateAsync();
}

async function _privateAsync() {
    return 42;
}
";

    let tree = parse_js(source);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();
    let file = PathBuf::from("test.js");

    let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_visibility_metadata(&staging, "publicAsync", "public"),
        "Async functions should follow same visibility rules"
    );

    assert!(
        has_visibility_metadata(&staging, "_privateAsync", "private"),
        "Async functions with underscore should be private"
    );
}

#[test]
fn test_arrow_function_visibility() {
    let source = r"
const publicArrow = () => 42;
const _privateArrow = () => 42;
";

    let tree = parse_js(source);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();
    let file = PathBuf::from("test.js");

    let result = builder.build_graph(&tree, source.as_bytes(), &file, &mut staging);
    assert!(result.is_ok());

    assert!(
        has_visibility_metadata(&staging, "publicArrow", "public"),
        "Arrow functions should follow visibility conventions"
    );

    assert!(
        has_visibility_metadata(&staging, "_privateArrow", "private"),
        "Arrow functions with underscore should be private"
    );
}
