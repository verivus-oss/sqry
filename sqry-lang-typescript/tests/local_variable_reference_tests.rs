//! Integration tests for TypeScript local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::local_scopes::{collect_reference_edges, count_local_refs, has_local_ref};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_typescript::relations::TypeScriptGraphBuilder;
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

fn parse_typescript(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    parser
        .set_language(&language)
        .expect("Failed to load TypeScript grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse TypeScript code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_typescript(content);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
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
    let content = load_fixture("basic.ts");
    let staging = build_staging_graph(&content, "basic.ts");
    let edges = collect_reference_edges(&staging);

    // let x = 10; let y = x + 1; → x should have a References edge
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_const_variable() {
    let content = load_fixture("basic.ts");
    let staging = build_staging_graph(&content, "basic.ts");
    let edges = collect_reference_edges(&staging);

    // const count = 42; console.log(count); → count should have a ref
    assert!(
        has_local_ref(&edges, "count"),
        "Expected local reference to count: {edges:?}"
    );
}

#[test]
fn test_multiple_declarators() {
    let content = load_fixture("basic.ts");
    let staging = build_staging_graph(&content, "basic.ts");
    let edges = collect_reference_edges(&staging);

    // let a = 1, b = 2; let c = a + b;
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
    let content = load_fixture("basic.ts");
    let staging = build_staging_graph(&content, "basic.ts");
    let edges = collect_reference_edges(&staging);

    // function paramRef(name: string, age: number) { const result = name; ... }
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
    let content = load_fixture("scoping.ts");
    let staging = build_staging_graph(&content, "scoping.ts");
    let edges = collect_reference_edges(&staging);

    // for (let i = 0; i < 10; i++) { console.log(i); }
    assert!(
        has_local_ref(&edges, "i"),
        "Expected local reference to for-loop variable i: {edges:?}"
    );
}

#[test]
fn test_for_of_variable() {
    let content = load_fixture("scoping.ts");
    let staging = build_staging_graph(&content, "scoping.ts");
    let edges = collect_reference_edges(&staging);

    // for (const item of items) { console.log(item); }
    assert!(
        has_local_ref(&edges, "item"),
        "Expected local reference to for-of variable item: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "items"),
        "Expected local reference to items: {edges:?}"
    );
}

#[test]
fn test_for_in_variable() {
    let content = load_fixture("scoping.ts");
    let staging = build_staging_graph(&content, "scoping.ts");
    let edges = collect_reference_edges(&staging);

    // for (const key in obj) { console.log(key); }
    assert!(
        has_local_ref(&edges, "key"),
        "Expected local reference to for-in variable key: {edges:?}"
    );
}

#[test]
fn test_multiple_references() {
    let content = load_fixture("scoping.ts");
    let staging = build_staging_graph(&content, "scoping.ts");
    let edges = collect_reference_edges(&staging);

    // x = 1; y = x + x; z = x + y → x used 3 times
    let x_count = count_local_refs(&edges, "x");
    assert!(
        x_count >= 3,
        "Expected at least 3 references to x, got {x_count}: {edges:?}"
    );
}

// ============================================================
// Advanced: arrow functions, try/catch, switch, destructuring
// ============================================================

#[test]
fn test_arrow_captures_variable() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // let x = 10; const fn1 = () => x;
    assert!(
        has_local_ref(&edges, "x"),
        "Expected arrow function to capture variable x: {edges:?}"
    );
}

#[test]
fn test_try_catch_variables() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // catch (error) { console.log(error); }
    assert!(
        has_local_ref(&edges, "error"),
        "Expected local reference to catch variable error: {edges:?}"
    );
}

#[test]
fn test_switch_case_variables() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // case 1: { let y = 10; console.log(y); } case 2: { let z = 20; console.log(z); }
    assert!(
        has_local_ref(&edges, "y"),
        "Expected local reference to switch variable y: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "z"),
        "Expected local reference to switch variable z: {edges:?}"
    );
}

#[test]
fn test_destructuring_array() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // const [a, b, c] = arr; console.log(a); console.log(b); console.log(c);
    assert!(
        has_local_ref(&edges, "a"),
        "Expected local reference to destructured a: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected local reference to destructured b: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "c"),
        "Expected local reference to destructured c: {edges:?}"
    );
}

#[test]
fn test_destructuring_object() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // const { name, age } = obj; console.log(name); console.log(age);
    assert!(
        has_local_ref(&edges, "name"),
        "Expected local reference to destructured name: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "age"),
        "Expected local reference to destructured age: {edges:?}"
    );
}

#[test]
fn test_destructuring_rename() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // const { x: renamed } = obj; console.log(renamed);
    assert!(
        has_local_ref(&edges, "renamed"),
        "Expected local reference to renamed destructured variable: {edges:?}"
    );
}

#[test]
fn test_rest_params() {
    let content = load_fixture("advanced.ts");
    let staging = build_staging_graph(&content, "advanced.ts");
    let edges = collect_reference_edges(&staging);

    // function restParams(...args: number[]) { console.log(args); }
    assert!(
        has_local_ref(&edges, "args"),
        "Expected local reference to rest parameter args: {edges:?}"
    );
}

// ============================================================
// No false positives
// ============================================================

#[test]
fn test_no_false_positive_for_type_names() {
    let content = load_fixture("no_false_positives.ts");
    let staging = build_staging_graph(&content, "no_false_positives.ts");
    let edges = collect_reference_edges(&staging);

    // "number" should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "number"),
        "Type name 'number' should NOT be a local reference: {edges:?}"
    );
}

#[test]
fn test_no_false_positive_for_member_access() {
    let content = load_fixture("no_false_positives.ts");
    let staging = build_staging_graph(&content, "no_false_positives.ts");
    let edges = collect_reference_edges(&staging);

    // "length" in s.length should NOT be a local variable reference
    assert!(
        !has_local_ref(&edges, "length"),
        "Member access 'length' should NOT be a local reference: {edges:?}"
    );
}
