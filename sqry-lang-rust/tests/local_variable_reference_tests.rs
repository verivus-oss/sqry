//! Integration tests for Rust local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::local_scopes::{collect_reference_edges, count_local_refs, has_local_ref};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_rust::relations::RustGraphBuilder;
use std::path::Path;
use std::path::PathBuf;
use tree_sitter::Tree;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("localvars")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

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

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_rust(content);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();
    let file_path = Path::new(filename);

    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

// ============================================================
// Basic variable declaration + usage
// ============================================================

#[test]
fn test_basic_let_variable() {
    let content = load_fixture("basic.rs");
    let staging = build_staging_graph(&content, "basic.rs");
    let edges = collect_reference_edges(&staging);

    // let x = 10; let y = x + 1; → x should have a References edge
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_const_binding() {
    let content = load_fixture("basic.rs");
    let staging = build_staging_graph(&content, "basic.rs");
    let edges = collect_reference_edges(&staging);

    // let count = 42; println!("{}", count);
    assert!(
        has_local_ref(&edges, "count"),
        "Expected local reference to count: {edges:?}"
    );
}

#[test]
fn test_mutable_variable() {
    let content = load_fixture("basic.rs");
    let staging = build_staging_graph(&content, "basic.rs");
    let edges = collect_reference_edges(&staging);

    // let mut x = 10; x += 1;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to mutable x: {edges:?}"
    );
}

// ============================================================
// Parameter references
// ============================================================

#[test]
fn test_parameter_reference() {
    let content = load_fixture("basic.rs");
    let staging = build_staging_graph(&content, "basic.rs");
    let edges = collect_reference_edges(&staging);

    // fn param_ref(name: &str, age: u32) { let result = name; ... }
    assert!(
        has_local_ref(&edges, "name"),
        "Expected local reference to parameter name: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "age"),
        "Expected local reference to parameter age: {edges:?}"
    );
}

// ============================================================
// Scoping and loops
// ============================================================

#[test]
fn test_for_loop_variable() {
    let content = load_fixture("scoping.rs");
    let staging = build_staging_graph(&content, "scoping.rs");
    let edges = collect_reference_edges(&staging);

    // for item in items { println!("{}", item); }
    assert!(
        has_local_ref(&edges, "item"),
        "Expected local reference to for-loop variable item: {edges:?}"
    );
}

#[test]
fn test_multiple_references() {
    let content = load_fixture("scoping.rs");
    let staging = build_staging_graph(&content, "scoping.rs");
    let edges = collect_reference_edges(&staging);

    // let x = 1; let y = x + x; let z = x + y;
    let x_count = count_local_refs(&edges, "x");
    assert!(
        x_count >= 3,
        "Expected at least 3 references to x, got {x_count}: {edges:?}"
    );
}

// ============================================================
// Advanced: closures, match, if-let, destructuring
// ============================================================

#[test]
fn test_closure_captures_variable() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // let x = 10; let f = |y| x + y;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected closure to capture variable x: {edges:?}"
    );
}

#[test]
fn test_match_binding() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // match value { Some(inner) => println!("{}", inner), ... }
    assert!(
        has_local_ref(&edges, "inner"),
        "Expected local reference to match binding inner: {edges:?}"
    );
}

#[test]
fn test_if_let_binding() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // if let Some(inner) = value { println!("{}", inner); }
    assert!(
        has_local_ref(&edges, "inner"),
        "Expected local reference to if-let binding inner: {edges:?}"
    );
}

#[test]
fn test_destructuring_tuple() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // let (a, b) = pair;
    assert!(
        has_local_ref(&edges, "a"),
        "Expected local reference to destructured a: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected local reference to destructured b: {edges:?}"
    );
}

#[test]
fn test_destructuring_struct() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // let Point { x, y } = p;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to destructured struct field x: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "y"),
        "Expected local reference to destructured struct field y: {edges:?}"
    );
}

#[test]
fn test_unsafe_block_variable() {
    let content = load_fixture("advanced.rs");
    let staging = build_staging_graph(&content, "advanced.rs");
    let edges = collect_reference_edges(&staging);

    // unsafe { let raw = 42; println!("{}", raw); }
    assert!(
        has_local_ref(&edges, "raw"),
        "Expected local reference to unsafe block variable raw: {edges:?}"
    );
}

// ============================================================
// No false positives
// ============================================================

#[test]
fn test_no_false_positive_for_field_access() {
    let content = load_fixture("no_false_positives.rs");
    let staging = build_staging_graph(&content, "no_false_positives.rs");
    let edges = collect_reference_edges(&staging);

    // "bar" in f.bar should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "bar"),
        "Field access 'bar' should NOT be a local reference: {edges:?}"
    );
}
