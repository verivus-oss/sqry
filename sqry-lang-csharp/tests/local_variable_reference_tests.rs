//! Integration tests for C# local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::local_scopes::{collect_reference_edges, count_local_refs, has_local_ref};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_csharp::relations::CSharpGraphBuilder;
use std::path::Path;
use std::path::PathBuf;
use tree_sitter::Tree;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("csharp")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

fn parse_csharp(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_c_sharp::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load C# grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse C# code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_csharp(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
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
fn test_basic_local_variable() {
    let content = load_fixture("localvars/basic.cs");
    let staging = build_staging_graph(&content, "basic.cs");
    let edges = collect_reference_edges(&staging);

    // int x = 10; int y = x + 1; → x should have a References edge
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_var_keyword() {
    let content = load_fixture("localvars/basic.cs");
    let staging = build_staging_graph(&content, "basic.cs");
    let edges = collect_reference_edges(&staging);

    // var count = 42; Console.WriteLine(count); → count should have a ref
    assert!(
        has_local_ref(&edges, "count"),
        "Expected local reference to count: {edges:?}"
    );
}

#[test]
fn test_multiple_declarators() {
    let content = load_fixture("localvars/basic.cs");
    let staging = build_staging_graph(&content, "basic.cs");
    let edges = collect_reference_edges(&staging);

    // int a = 1, b = 2; int c = a + b;
    assert!(
        has_local_ref(&edges, "a"),
        "Expected local reference to a: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected local reference to b: {edges:?}"
    );
}

// ============================================================
// Parameter references
// ============================================================

#[test]
fn test_parameter_reference() {
    let content = load_fixture("localvars/basic.cs");
    let staging = build_staging_graph(&content, "basic.cs");
    let edges = collect_reference_edges(&staging);

    // void ParamRef(string name, int age) { var result = name; ... }
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
// Scoping and shadowing
// ============================================================

#[test]
fn test_for_loop_variable() {
    let content = load_fixture("localvars/scoping.cs");
    let staging = build_staging_graph(&content, "scoping.cs");
    let edges = collect_reference_edges(&staging);

    // for (int i = 0; i < 10; i++) { Console.WriteLine(i); }
    assert!(
        has_local_ref(&edges, "i"),
        "Expected local reference to for-loop variable i: {edges:?}"
    );
}

#[test]
fn test_foreach_variable() {
    let content = load_fixture("localvars/scoping.cs");
    let staging = build_staging_graph(&content, "scoping.cs");
    let edges = collect_reference_edges(&staging);

    // foreach (var item in items) { ... }
    assert!(
        has_local_ref(&edges, "item"),
        "Expected local reference to foreach variable item: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "items"),
        "Expected local reference to items: {edges:?}"
    );
}

#[test]
fn test_multiple_references() {
    let content = load_fixture("localvars/scoping.cs");
    let staging = build_staging_graph(&content, "scoping.cs");
    let edges = collect_reference_edges(&staging);

    // x = 1; y = x + x; z = x + y → x used 3 times
    let x_count = count_local_refs(&edges, "x");
    assert!(
        x_count >= 3,
        "Expected at least 3 references to x, got {x_count}: {edges:?}"
    );
}

// ============================================================
// Advanced: lambdas, try/catch, switch, using
// ============================================================

#[test]
fn test_lambda_captures_variable() {
    let content = load_fixture("localvars/advanced.cs");
    let staging = build_staging_graph(&content, "advanced.cs");
    let edges = collect_reference_edges(&staging);

    // int x = 10; Func<int> fn = () => x;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected lambda to capture variable x: {edges:?}"
    );
}

#[test]
fn test_try_catch_variables() {
    let content = load_fixture("localvars/advanced.cs");
    let staging = build_staging_graph(&content, "advanced.cs");
    let edges = collect_reference_edges(&staging);

    // catch (Exception ex) { Console.WriteLine(ex); }
    assert!(
        has_local_ref(&edges, "ex"),
        "Expected local reference to catch variable ex: {edges:?}"
    );
}

#[test]
fn test_switch_case_variables() {
    let content = load_fixture("localvars/advanced.cs");
    let staging = build_staging_graph(&content, "advanced.cs");
    let edges = collect_reference_edges(&staging);

    // switch (x) { case 1: int y = 10; ... case 2: int z = 20; ... }
    assert!(
        has_local_ref(&edges, "y"),
        "Expected local reference to switch variable y: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "z"),
        "Expected local reference to switch variable z: {edges:?}"
    );
}

// ============================================================
// No false positives
// ============================================================

#[test]
fn test_no_false_positive_for_type_names() {
    let content = load_fixture("localvars/no_false_positives.cs");
    let staging = build_staging_graph(&content, "no_false_positives.cs");
    let edges = collect_reference_edges(&staging);

    // "int" should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "int"),
        "Type name 'int' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_member_access() {
    let content = load_fixture("localvars/no_false_positives.cs");
    let staging = build_staging_graph(&content, "no_false_positives.cs");
    let edges = collect_reference_edges(&staging);

    // "Length" in s.Length should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "Length"),
        "Member access 'Length' should NOT be a local reference: {edges:?}"
    );
}
