/// Integration tests for Swift `GraphBuilder`
#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_swift::SwiftGraphBuilder;
use sqry_test_support::graph_helpers::{assert_has_call_edge, build_node_name_lookup};
use std::fs;
use std::path::Path;
use support::unique_swift_path;
use tree_sitter::Parser;

fn parse_swift(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("error loading Swift grammar");
    parser.parse(source, None).expect("swift parse failed")
}

fn collect_ffi_call_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::FfiCall { .. },
                ..
            } = op
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn graph_builder_extracts_async_and_sync_calls() {
    let source = fs::read_to_string("tests/fixtures/graph/async_controller.swift")
        .expect("load swift fixture");
    let tree = parse_swift(&source);
    let file = unique_swift_path("async_controller");
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::new(4);

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let awaited_call = assert_has_call_edge(
        &staging,
        "UserController::create",
        "UserController::sendWelcomeEmail",
    );
    assert!(
        awaited_call.is_async,
        "Expected awaited call to be marked async"
    );
    assert_has_call_edge(
        &staging,
        "UserController::sendWelcomeEmail",
        "Mailer::deliver",
    );
    assert_has_call_edge(&staging, "UserController::audit", "UserController::log");
}

#[test]
fn graph_builder_detects_c_functions_via_bridging_header() {
    use sqry_lang_swift::{BridgingHeaderLocator, SwiftBridgingIndex};

    // Clear caches to ensure clean test
    BridgingHeaderLocator::clear_cache();
    SwiftBridgingIndex::clear();

    // Parse Swift file - the builder should automatically discover and index the bridging header
    let source = fs::read_to_string("tests/fixtures/graph/bridging_example.swift")
        .expect("load bridging fixture");
    let tree = parse_swift(&source);
    let file = Path::new("tests/fixtures/graph/bridging_example.swift");
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::new(4);

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let ffi_edges = collect_ffi_call_edges(&staging);
    assert!(
        ffi_edges.len() >= 3,
        "should detect FFI edges for C function calls, got {ffi_edges:?}"
    );
    assert!(
        ffi_edges
            .iter()
            .any(|(_, callee)| callee.contains("C::initialize_c_library")),
        "should detect initialize_c_library call, got {ffi_edges:?}"
    );
    assert!(
        ffi_edges
            .iter()
            .any(|(_, callee)| callee.contains("C::process_data")),
        "should detect process_data call, got {ffi_edges:?}"
    );
    assert!(
        ffi_edges
            .iter()
            .any(|(_, callee)| callee.contains("C::cleanup_resources")),
        "should detect cleanup_resources call, got {ffi_edges:?}"
    );
}

#[test]
fn graph_builder_handles_protocol_extensions() {
    let source = fs::read_to_string("tests/fixtures/graph/protocol_extensions.swift")
        .expect("load protocol extension fixture");
    let tree = parse_swift(&source);
    let file = unique_swift_path("protocol_extensions");
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::new(4);

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "DataProcessor::validate", "process");
    assert_has_call_edge(
        &staging,
        "DataProcessor::transform",
        "DataProcessor::validate",
    );
    assert_has_call_edge(&staging, "Cache::retrieve", "Cache::store");
}

#[test]
fn graph_builder_captures_throws_and_visibility() {
    let source = fs::read_to_string("tests/fixtures/graph/throws_patterns.swift")
        .expect("load throws fixture");
    let tree = parse_swift(&source);
    let file = unique_swift_path("throws_patterns");
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::new(4);

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    let node_names = build_node_name_lookup(&staging);
    assert!(
        node_names
            .values()
            .any(|name| name.contains("ErrorHandler::process")),
        "should extract private process method"
    );
    assert!(
        node_names
            .values()
            .any(|name| name.contains("ErrorHandler::publicMethod")),
        "should extract publicMethod"
    );
    assert_has_call_edge(&staging, "ErrorHandler::validate", "ErrorHandler::process");
    assert_has_call_edge(
        &staging,
        "ErrorHandler::publicMethod",
        "ErrorHandler::validate",
    );
}

#[test]
fn graph_builder_handles_malformed_swift_gracefully() {
    let source = fs::read_to_string("tests/fixtures/graph/malformed_syntax.swift")
        .expect("load malformed fixture");

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("error loading Swift grammar");

    let tree = parser
        .parse(&source, None)
        .expect("tree-sitter should parse despite errors");

    assert!(
        tree.root_node().has_error(),
        "malformed fixture should produce a tree with error nodes"
    );

    let file = unique_swift_path("malformed");
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::new(4);

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);

    result.expect("builder should gracefully handle malformed Swift without returning errors");

    let node_names = build_node_name_lookup(&staging);
    let has_method = node_names.values().any(|name| {
        name.contains("anotherMethod") || name.contains("test") || name.contains("incompleteMethod")
    });
    assert!(
        has_method,
        "should extract at least one method from malformed Swift, got {node_names:?}"
    );
}

#[test]
fn graph_builder_respects_depth_limit() {
    let source = fs::read_to_string("tests/fixtures/graph/deep_nesting.swift")
        .expect("load deep nesting fixture");
    let tree = parse_swift(&source);
    let file = unique_swift_path("deep_nesting");
    let mut staging = StagingGraph::new();

    let builder = SwiftGraphBuilder::new(4);

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with deep nesting");

    let node_names = build_node_name_lookup(&staging);
    assert!(
        node_names
            .values()
            .any(|name| name.contains("Level4") && name.contains("method1")),
        "should extract Level4 within depth limit"
    );
    assert!(
        node_names.values().any(|name| name.contains("Shallow")),
        "should extract shallow classes"
    );
    assert!(
        !node_names.values().any(|name| name.contains("deepMethod")),
        "should skip deepMethod beyond depth limit, got {node_names:?}"
    );
}
