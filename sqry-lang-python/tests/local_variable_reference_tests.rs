//! Integration tests for Python local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::local_scopes::{collect_reference_edges, count_local_refs, has_local_ref};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_python::relations::PythonGraphBuilder;
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

fn parse_python(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Python grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Python code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_python(content);
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();
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
    let content = load_fixture("basic.py");
    let staging = build_staging_graph(&content, "basic.py");
    let edges = collect_reference_edges(&staging);

    // x = 10; y = x + 1; → x should have a References edge
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_const_binding() {
    let content = load_fixture("basic.py");
    let staging = build_staging_graph(&content, "basic.py");
    let edges = collect_reference_edges(&staging);

    // count = 42; result = count + 1;
    assert!(
        has_local_ref(&edges, "count"),
        "Expected local reference to count: {edges:?}"
    );
}

#[test]
fn test_reassignment() {
    let content = load_fixture("basic.py");
    let staging = build_staging_graph(&content, "basic.py");
    let edges = collect_reference_edges(&staging);

    // x = 10; x = x + 1; → x should have references
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to reassigned x: {edges:?}"
    );
}

// ============================================================
// Parameter references
// ============================================================

#[test]
fn test_parameter_reference() {
    let content = load_fixture("basic.py");
    let staging = build_staging_graph(&content, "basic.py");
    let edges = collect_reference_edges(&staging);

    // def param_ref(name, age): result = name; total = age + 1;
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
    let content = load_fixture("scoping.py");
    let staging = build_staging_graph(&content, "scoping.py");
    let edges = collect_reference_edges(&staging);

    // for item in items: result = item + 1
    assert!(
        has_local_ref(&edges, "item"),
        "Expected local reference to for-loop variable item: {edges:?}"
    );
}

#[test]
fn test_multiple_references() {
    let content = load_fixture("scoping.py");
    let staging = build_staging_graph(&content, "scoping.py");
    let edges = collect_reference_edges(&staging);

    // x = 1; y = x + x; z = x + y;
    let x_count = count_local_refs(&edges, "x");
    assert!(
        x_count >= 3,
        "Expected at least 3 references to x, got {x_count}: {edges:?}"
    );
}

#[test]
fn test_no_block_scope() {
    let content = load_fixture("scoping.py");
    let staging = build_staging_graph(&content, "scoping.py");
    let edges = collect_reference_edges(&staging);

    // if True: inner = 42; return inner → inner accessible outside if block
    assert!(
        has_local_ref(&edges, "inner"),
        "Expected local reference to inner (no block scope): {edges:?}"
    );
}

// ============================================================
// Advanced: closures, destructuring, comprehensions
// ============================================================

#[test]
fn test_closure_captures_variable() {
    let content = load_fixture("advanced.py");
    let staging = build_staging_graph(&content, "advanced.py");
    let edges = collect_reference_edges(&staging);

    // x = 10; f = lambda y: x + y;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected closure to capture variable x: {edges:?}"
    );
}

#[test]
fn test_destructuring_tuple() {
    let content = load_fixture("advanced.py");
    let staging = build_staging_graph(&content, "advanced.py");
    let edges = collect_reference_edges(&staging);

    // a, b = pair
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
fn test_try_except_binding() {
    let content = load_fixture("advanced.py");
    let staging = build_staging_graph(&content, "advanced.py");
    let edges = collect_reference_edges(&staging);

    // except ZeroDivisionError as err: msg = str(err)
    assert!(
        has_local_ref(&edges, "err"),
        "Expected local reference to except binding err: {edges:?}"
    );
}

#[test]
fn test_nested_function_capture() {
    let content = load_fixture("advanced.py");
    let staging = build_staging_graph(&content, "advanced.py");
    let edges = collect_reference_edges(&staging);

    // outer = 10; def inner(): return outer + 1
    assert!(
        has_local_ref(&edges, "outer"),
        "Expected local reference to captured variable outer: {edges:?}"
    );
}

// ============================================================
// No false positives
// ============================================================

#[test]
fn test_no_false_positive_for_attribute_access() {
    let content = load_fixture("no_false_positives.py");
    let staging = build_staging_graph(&content, "no_false_positives.py");
    let edges = collect_reference_edges(&staging);

    // obj.get("key") — "get" should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "get"),
        "Attribute access 'get' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_type_annotations() {
    let content = load_fixture("no_false_positives.py");
    let staging = build_staging_graph(&content, "no_false_positives.py");
    let edges = collect_reference_edges(&staging);

    // def type_names(x: int, y: str) -> bool:
    // "int", "str", "bool" should NOT be local variable references
    assert!(
        !has_local_ref(&edges, "int"),
        "Type annotation 'int' should NOT be a local reference: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "str"),
        "Type annotation 'str' should NOT be a local reference: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "bool"),
        "Type annotation 'bool' should NOT be a local reference: {edges:?}"
    );
}
