//! Integration tests for FR-RUST relation tracking
//!
//! Tests all 16 scenarios from the implementation plan:
//! - SC-01 to SC-07: Export edge detection
//! - SC-08 to SC-12: `FieldAccess` edge detection
//! - SC-13 to SC-14: Stdlib/external import classification
//! - SC-15 to SC-16: Call edge metadata
//!

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_rust::relations::RustGraphBuilder;
use sqry_test_support::graph_helpers::{build_node_name_lookup, build_string_lookup};
use std::path::PathBuf;
use tree_sitter::Tree;

// ========== Helper Functions ==========

/// Load the relation scenarios fixture
fn load_scenarios_fixture() -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test-fixtures")
        .join("rust")
        .join("relation_scenarios.rs");
    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| panic!("Failed to load fixture: {e}"))
}

/// Parse Rust source code into a tree-sitter Tree
fn parse_rust(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Rust grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Rust code")
}

/// Build a `StagingGraph` from Rust source code
fn build_graph(content: &str) -> StagingGraph {
    build_graph_with_file(content, "relation_scenarios.rs")
}

/// Build a `StagingGraph` from Rust source code with a specific file name
fn build_graph_with_file(content: &str, file_name: &str) -> StagingGraph {
    let tree = parse_rust(content);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();
    let file_path = PathBuf::from(file_name);

    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

fn collect_edges(staging: &StagingGraph) -> Vec<(String, String, EdgeKind)> {
    let node_names = build_node_name_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
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
                Some((source_name, target_name, kind.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn assert_export_edge(staging: &StagingGraph, target_substring: &str) {
    let edges = collect_edges(staging);
    assert!(
        edges.iter().any(|(source, target, kind)| {
            matches!(kind, EdgeKind::Exports { .. })
                && source.contains("<file_module>")
                && target.contains(target_substring)
        }),
        "expected export edge to {target_substring}, got {edges:?}"
    );
}

fn assert_import_edge(staging: &StagingGraph, target_substring: &str) {
    let edges = collect_edges(staging);
    assert!(
        edges.iter().any(|(source, target, kind)| {
            matches!(kind, EdgeKind::Imports { .. })
                && source.contains("<file_module>")
                && target.contains(target_substring)
        }),
        "expected import edge to {target_substring}, got {edges:?}"
    );
}

fn assert_reference_edge(staging: &StagingGraph, source_substring: &str, target_substring: &str) {
    let edges = collect_edges(staging);
    assert!(
        edges.iter().any(|(source, target, kind)| {
            matches!(kind, EdgeKind::References)
                && source.contains(source_substring)
                && target.contains(target_substring)
        }),
        "expected reference edge {source_substring} -> {target_substring}, got {edges:?}"
    );
}

fn find_call_edge(
    staging: &StagingGraph,
    source_substring: &str,
    target_substring: &str,
) -> Option<(u8, bool)> {
    collect_edges(staging)
        .into_iter()
        .find_map(|(source, target, kind)| {
            if source.contains(source_substring)
                && target.contains(target_substring)
                && let EdgeKind::Calls {
                    argument_count,
                    is_async,
                } = kind
            {
                return Some((argument_count, is_async));
            }
            None
        })
}

// ========== Test Suite ==========

#[test]
fn test_scenario_sc01_pub_fn() {
    let content = "pub fn exposed_func() {}";
    let staging = build_graph(content);
    assert_export_edge(&staging, "exposed_func");
}

#[test]
fn test_scenario_sc02_pub_struct() {
    let content = "pub struct PublicType {}";
    let staging = build_graph(content);
    assert_export_edge(&staging, "PublicType");
}

#[test]
fn test_scenario_sc03_pub_enum() {
    let content = "pub enum PublicEnum {}";
    let staging = build_graph(content);
    assert_export_edge(&staging, "PublicEnum");
}

#[test]
fn test_scenario_sc04_pub_trait() {
    let content = "pub trait PublicTrait {}";
    let staging = build_graph(content);
    assert_export_edge(&staging, "PublicTrait");
}

#[test]
fn test_scenario_sc05_pub_type() {
    let content = "pub type PublicType = u32;";
    let staging = build_graph(content);
    assert_export_edge(&staging, "PublicType");
}

#[test]
fn test_scenario_sc06_pub_use() {
    let content = "pub use std::collections::HashMap;";
    let staging = build_graph(content);
    assert_export_edge(&staging, "std::collections::HashMap");
}

#[test]
fn test_scenario_sc07_pub_use_alias() {
    let content = "pub use std::collections::HashMap as Map;";
    let staging = build_graph(content);
    assert_export_edge(&staging, "std::collections::HashMap");

    let strings = build_string_lookup(&staging);
    let edges = collect_edges(&staging);
    let alias = edges.iter().find_map(|(_, target, kind)| {
        if target.contains("std::collections::HashMap")
            && let EdgeKind::Exports { alias, .. } = kind
        {
            return alias.and_then(|id| strings.get(&id.index()).cloned());
        }
        None
    });

    assert_eq!(alias.as_deref(), Some("Map"));
}

#[test]
fn test_scenario_sc08_field_access_struct() {
    let content = r"
struct Point { x: i32, y: i32 }
fn test() { let p = Point { x: 0, y: 0 }; let _ = p.x; }
";
    let staging = build_graph(content);
    assert_reference_edge(&staging, "test", "<field:p.x>");
}

#[test]
fn test_scenario_sc09_field_access_tuple() {
    let content = r"
fn test() { let t = (0, 1); let _ = t.0; }
";
    let staging = build_graph(content);
    assert_reference_edge(&staging, "test", "<field:t.0>");
}

#[test]
fn test_scenario_sc10_field_access_method() {
    let content = r"
struct S { f: i32 }
impl S { fn get_f(&self) -> i32 { self.f } }
";
    let staging = build_graph(content);
    assert_reference_edge(&staging, "get_f", "<field:self.f>");
}

#[test]
fn test_scenario_sc11_field_access_nested() {
    let content = r"
struct Outer { inner: Inner }
struct Inner { value: i32 }
fn test() { let o = Outer { inner: Inner { value: 0 } }; let _ = o.inner.value; }
";
    let staging = build_graph(content);
    assert_reference_edge(&staging, "test", "<field:o.inner>");
    assert_reference_edge(&staging, "test", "<field:o.inner.value>");
}

#[test]
fn test_scenario_sc12_field_access_generic() {
    let content = r"
struct Container<T> { item: T }
fn test<T>(c: Container<T>) -> T { c.item }
";
    let staging = build_graph(content);
    assert_reference_edge(&staging, "test", "<field:c.item>");
}

#[test]
fn test_scenario_sc13_stdlib_import() {
    let content = "use std::collections::HashMap;";
    let staging = build_graph(content);
    assert_import_edge(&staging, "std::collections::HashMap");
}

#[test]
fn test_scenario_sc14_external_import() {
    let content = "use serde::Deserialize;";
    let staging = build_graph(content);
    assert_import_edge(&staging, "serde::Deserialize");
}

#[test]
fn test_scenario_sc15_call_extras_method() {
    let content = r"
struct Point;
impl Point { fn new() -> Self { Point } fn distance(&self) -> f64 { 0.0 } }
fn test() { let p = Point::new(); let _ = p.distance(); }
";
    let staging = build_graph(content);
    let (arg_count, is_async) =
        find_call_edge(&staging, "test", "Point::new").expect("call to Point::new");
    assert_eq!(arg_count, 0);
    assert!(!is_async);

    let (arg_count, is_async) =
        find_call_edge(&staging, "test", "distance").expect("call to distance");
    assert_eq!(arg_count, 0);
    assert!(!is_async);
}

#[test]
fn test_scenario_sc16_call_extras_async() {
    let content = r"
async fn fetch() -> String { String::new() }
async fn test() { let _ = fetch().await; }
";
    let staging = build_graph(content);
    let (arg_count, is_async) = find_call_edge(&staging, "test", "fetch").expect("call to fetch");
    assert_eq!(arg_count, 0);
    assert!(is_async);
}

#[test]
fn test_integration_all_scenarios() {
    let content = load_scenarios_fixture();
    let staging = build_graph(&content);
    assert!(
        !collect_edges(&staging).is_empty(),
        "expected staged edges for full scenario fixture"
    );
}
