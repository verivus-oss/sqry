//! Integration tests for Go local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::local_scopes::{
    collect_reference_edges, count_local_refs, has_local_ref, local_ref_targets,
};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_go::relations::GoGraphBuilder;
use std::path::Path;
use std::path::PathBuf;
use tree_sitter::Tree;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("go")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

fn parse_go(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_go::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Go grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Go code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_go(content);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
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
fn test_basic_short_var_declaration() {
    let content = load_fixture("localvars/basic.go");
    let staging = build_staging_graph(&content, "basic.go");
    let edges = collect_reference_edges(&staging);

    // x := 10; y := x + 1 → x should have a References edge
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_var_declaration_usage() {
    let content = load_fixture("localvars/basic.go");
    let staging = build_staging_graph(&content, "basic.go");
    let edges = collect_reference_edges(&staging);

    // var count int; count = 42 → count should have a References edge
    assert!(
        has_local_ref(&edges, "count"),
        "Expected local reference to count: {edges:?}"
    );
}

#[test]
fn test_multi_short_var() {
    let content = load_fixture("localvars/basic.go");
    let staging = build_staging_graph(&content, "basic.go");
    let edges = collect_reference_edges(&staging);

    // a, b := 1, 2; c := a + b → a and b should have References edges
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
    let content = load_fixture("localvars/basic.go");
    let staging = build_staging_graph(&content, "basic.go");
    let edges = collect_reference_edges(&staging);

    // func paramRef(name string, age int) { result := name; _ = age }
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
// Nested scope shadowing
// ============================================================

#[test]
fn test_shadowed_variable() {
    let content = load_fixture("localvars/shadowing.go");
    let staging = build_staging_graph(&content, "shadowing.go");
    let edges = collect_reference_edges(&staging);

    // x is declared twice (outer and inner block). Each usage should
    // resolve to its own declaration, so there should be 2 distinct targets.
    let targets = local_ref_targets(&edges, "x");
    assert!(
        targets.len() >= 2,
        "Expected at least 2 distinct x targets (outer + inner shadow): {targets:?}"
    );
}

#[test]
fn test_if_init_variable() {
    let content = load_fixture("localvars/shadowing.go");
    let staging = build_staging_graph(&content, "shadowing.go");
    let edges = collect_reference_edges(&staging);

    // if y := x + 1; y > 0 { _ = y }
    // y is declared in the if-init and used in condition + body
    assert!(
        has_local_ref(&edges, "y"),
        "Expected local reference to if-init variable y: {edges:?}"
    );
}

#[test]
fn test_for_loop_variable() {
    let content = load_fixture("localvars/shadowing.go");
    let staging = build_staging_graph(&content, "shadowing.go");
    let edges = collect_reference_edges(&staging);

    // for i := 0; i < 10; i++ { _ = i }
    assert!(
        has_local_ref(&edges, "i"),
        "Expected local reference to for-loop variable i: {edges:?}"
    );
}

// ============================================================
// Loop variables (for-range)
// ============================================================

#[test]
fn test_for_range_variables() {
    let content = load_fixture("localvars/loops.go");
    let staging = build_staging_graph(&content, "loops.go");
    let edges = collect_reference_edges(&staging);

    // for k, v := range items { _ = k; _ = v }
    assert!(
        has_local_ref(&edges, "k"),
        "Expected local reference to range key k: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "v"),
        "Expected local reference to range value v: {edges:?}"
    );
    // items is also used in range expression
    assert!(
        has_local_ref(&edges, "items"),
        "Expected local reference to items: {edges:?}"
    );
}

// ============================================================
// Multiple references to same variable
// ============================================================

#[test]
fn test_multiple_references() {
    let content = load_fixture("localvars/loops.go");
    let staging = build_staging_graph(&content, "loops.go");
    let edges = collect_reference_edges(&staging);

    // x := 1; y := x + x; z := x + y → x used 3 times
    let x_count = count_local_refs(&edges, "x");
    assert!(
        x_count >= 3,
        "Expected at least 3 references to x, got {x_count}: {edges:?}"
    );
}

// ============================================================
// Closures / function literals
// ============================================================

#[test]
fn test_closure_captures_outer_variable() {
    let content = load_fixture("localvars/closures.go");
    let staging = build_staging_graph(&content, "closures.go");
    let edges = collect_reference_edges(&staging);

    // fn := func() int { return x } → x should be captured
    assert!(
        has_local_ref(&edges, "x"),
        "Expected closure to capture outer variable x: {edges:?}"
    );
}

#[test]
fn test_method_receiver_reference() {
    let content = load_fixture("localvars/closures.go");
    let staging = build_staging_graph(&content, "closures.go");
    let edges = collect_reference_edges(&staging);

    // func (s *MyStruct) method() { v := s.Value }
    // `s` is a receiver parameter — the `s` before `.Value` should be a reference.
    // Note: `s` is resolved as a local variable (receiver param), and `Value` is
    // field access (skipped).
    assert!(
        has_local_ref(&edges, "s"),
        "Expected local reference to receiver parameter s: {edges:?}"
    );
}

#[test]
fn test_switch_case_scoped_variables() {
    let content = load_fixture("localvars/closures.go");
    let staging = build_staging_graph(&content, "closures.go");
    let edges = collect_reference_edges(&staging);

    // switch x { case 1: y := 10; _ = y; case 2: y := 20; _ = y }
    // Each y in each case should resolve to its own declaration
    let y_targets = local_ref_targets(&edges, "y");
    assert!(
        y_targets.len() >= 2,
        "Expected at least 2 distinct y targets in switch cases: {y_targets:?}"
    );
}

// ============================================================
// No false positives
// ============================================================

#[test]
fn test_no_false_positive_for_type_names() {
    let content = load_fixture("localvars/no_false_positives.go");
    let staging = build_staging_graph(&content, "no_false_positives.go");
    let edges = collect_reference_edges(&staging);

    // "int" in type conversion should NOT generate a local variable ref
    assert!(
        !has_local_ref(&edges, "int"),
        "Type name 'int' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_package_names() {
    let content = load_fixture("localvars/no_false_positives.go");
    let staging = build_staging_graph(&content, "no_false_positives.go");
    let edges = collect_reference_edges(&staging);

    // "fmt" in fmt.Println should NOT generate a local variable ref
    assert!(
        !has_local_ref(&edges, "fmt"),
        "Package name 'fmt' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_field_access() {
    let content = load_fixture("localvars/no_false_positives.go");
    let staging = build_staging_graph(&content, "no_false_positives.go");
    let edges = collect_reference_edges(&staging);

    // "Bar" in f.Bar should NOT generate a local variable ref
    assert!(
        !has_local_ref(&edges, "Bar"),
        "Field name 'Bar' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_labels() {
    let content = load_fixture("localvars/no_false_positives.go");
    let staging = build_staging_graph(&content, "no_false_positives.go");
    let edges = collect_reference_edges(&staging);

    // "outer" in `outer:` and `break outer` should NOT be local refs
    assert!(
        !has_local_ref(&edges, "outer"),
        "Label 'outer' should NOT be a local reference: {edges:?}"
    );
}
