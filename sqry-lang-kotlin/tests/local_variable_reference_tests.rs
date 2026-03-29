//! Integration tests for Kotlin local variable reference tracking.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_kotlin::relations::KotlinGraphBuilder;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tree_sitter::Tree;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

fn parse_kotlin(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_sqry::language())
        .expect("Failed to load Kotlin grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Kotlin code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_kotlin(content);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();
    let file_path = Path::new(filename);

    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
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
                && matches!(kind, EdgeKind::References)
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| format!("<unknown:{}>", source.index()));
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| format!("<unknown:{}>", target.index()));
                return Some((source_name, target_name));
            }
            None
        })
        .collect()
}

// ---- Assertion helpers ----

/// Check if any reference edge targets a local variable `name@*`
fn has_local_ref(edges: &[(String, String)], name: &str) -> bool {
    let prefix = format!("{name}@");
    edges.iter().any(|(_, target)| target.starts_with(&prefix))
}

/// Count reference edges whose target starts with `name@`
fn count_local_refs(edges: &[(String, String)], name: &str) -> usize {
    let prefix = format!("{name}@");
    edges
        .iter()
        .filter(|(_, target)| target.starts_with(&prefix))
        .count()
}

/// Get unique local-variable targets matching `name@*`
fn local_ref_targets(edges: &[(String, String)], name: &str) -> HashSet<String> {
    let prefix = format!("{name}@");
    edges
        .iter()
        .filter(|(_, target)| target.starts_with(&prefix))
        .map(|(_, target)| target.clone())
        .collect()
}

// ============================================================
// Local Variables (basic)
// ============================================================

#[test]
fn test_local_var_simple() {
    let content = load_fixture("localvars/LocalVars.kt");
    let staging = build_staging_graph(&content, "LocalVars.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_local_var_multiple() {
    let content = load_fixture("localvars/LocalVars.kt");
    let staging = build_staging_graph(&content, "LocalVars.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "a"),
        "Expected reference to a: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected reference to b: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "c"),
        "Expected reference to c: {edges:?}"
    );
}

// ============================================================
// Parameters
// ============================================================

#[test]
fn test_parameter_refs() {
    let content = load_fixture("localvars/ParameterRefs.kt");
    let staging = build_staging_graph(&content, "ParameterRefs.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "name"),
        "Expected parameter reference to name: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "greeting"),
        "Expected local reference to greeting: {edges:?}"
    );
}

#[test]
fn test_parameter_refs_multi_param() {
    let content = load_fixture("localvars/ParameterRefs.kt");
    let staging = build_staging_graph(&content, "ParameterRefs.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected parameter reference to x: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "y"),
        "Expected parameter reference to y: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "result"),
        "Expected local reference to result: {edges:?}"
    );
}

// ============================================================
// Destructuring
// ============================================================

#[test]
fn test_destructuring() {
    let content = load_fixture("localvars/Destructuring.kt");
    let staging = build_staging_graph(&content, "Destructuring.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "num"),
        "Expected reference to destructured num: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "text"),
        "Expected reference to destructured text: {edges:?}"
    );
}

// ============================================================
// Nested Scopes / Shadowing
// ============================================================

#[test]
fn test_nested_scopes_shadowing() {
    let content = load_fixture("scopes/NestedScopes.kt");
    let staging = build_staging_graph(&content, "NestedScopes.kt");
    let edges = collect_reference_edges(&staging);

    // Both usages of x should produce references.
    // The inner one targets the shadowed x, the outer targets the original.
    let targets = local_ref_targets(&edges, "x");
    assert!(
        targets.len() >= 2,
        "Expected at least 2 distinct x@ targets (outer + inner): {targets:?}"
    );
}

// ============================================================
// For Loops
// ============================================================

#[test]
fn test_for_loop_variable() {
    let content = load_fixture("scopes/ForLoops.kt");
    let staging = build_staging_graph(&content, "ForLoops.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "item"),
        "Expected reference to for loop variable item: {edges:?}"
    );
}

#[test]
fn test_for_loop_destructuring() {
    let content = load_fixture("scopes/ForLoops.kt");
    let staging = build_staging_graph(&content, "ForLoops.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "key"),
        "Expected reference to destructured key: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "value"),
        "Expected reference to destructured value: {edges:?}"
    );
}

// ============================================================
// When Expression
// ============================================================

#[test]
fn test_when_entry_isolation() {
    let content = load_fixture("scopes/WhenScopes.kt");
    let staging = build_staging_graph(&content, "WhenScopes.kt");
    let edges = collect_reference_edges(&staging);

    // len and doubled are in separate when entries
    assert!(
        has_local_ref(&edges, "len"),
        "Expected reference to len in when entry: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "doubled"),
        "Expected reference to doubled in when entry: {edges:?}"
    );
}

// ============================================================
// Try/Catch
// ============================================================

#[test]
fn test_try_catch() {
    let content = load_fixture("scopes/TryCatch.kt");
    let staging = build_staging_graph(&content, "TryCatch.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "result"),
        "Expected reference to result in try block: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "e"),
        "Expected reference to catch variable e: {edges:?}"
    );
}

// ============================================================
// Lambdas
// ============================================================

#[test]
fn test_lambda_explicit_param() {
    let content = load_fixture("lambda/LambdaExplicit.kt");
    let staging = build_staging_graph(&content, "LambdaExplicit.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to lambda parameter x: {edges:?}"
    );
}

#[test]
fn test_lambda_implicit_it() {
    let content = load_fixture("lambda/LambdaImplicitIt.kt");
    let staging = build_staging_graph(&content, "LambdaImplicitIt.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "it"),
        "Expected reference to implicit it: {edges:?}"
    );
}

// ============================================================
// Declaration Filter
// ============================================================

#[test]
fn test_declarations_not_references() {
    let content = load_fixture("localvars/DeclFilter.kt");
    let staging = build_staging_graph(&content, "DeclFilter.kt");
    let edges = collect_reference_edges(&staging);

    // x should be referenced (in y's initializer), but the declaration
    // of x itself should NOT be a reference edge target from itself
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to x from y's initializer: {edges:?}"
    );

    // Count: x is used once in `y = x + 1`, y is used once in `println(y)`
    assert_eq!(
        count_local_refs(&edges, "x"),
        1,
        "Expected exactly 1 reference to x: {edges:?}"
    );
    assert_eq!(
        count_local_refs(&edges, "y"),
        1,
        "Expected exactly 1 reference to y: {edges:?}"
    );
}

// ============================================================
// Local Class/Object (capture)
// ============================================================

#[test]
fn test_local_class_captures_outer() {
    let content = load_fixture("classes/LocalClassObject.kt");
    let staging = build_staging_graph(&content, "LocalClassObject.kt");
    let edges = collect_reference_edges(&staging);

    // The inner class captures `outer` from enclosing function
    assert!(
        has_local_ref(&edges, "outer"),
        "Expected reference to captured outer variable: {edges:?}"
    );
}

// ============================================================
// Local Class member vs outer variable (H1 fix)
// ============================================================

#[test]
fn test_local_class_member_resolution() {
    let content = load_fixture("classes/LocalClassMember.kt");
    let staging = build_staging_graph(&content, "LocalClassMember.kt");
    let edges = collect_reference_edges(&staging);

    // `name` is used in outer scope (println(name)) → should reference outer local
    assert!(
        has_local_ref(&edges, "name"),
        "Expected reference to local 'name': {edges:?}"
    );
}

// ============================================================
// For-loop iterable expression (M2 fix)
// ============================================================

#[test]
fn test_for_loop_iterable_resolves_outer() {
    let content = load_fixture("scopes/ForLoopShadow.kt");
    let staging = build_staging_graph(&content, "ForLoopShadow.kt");
    let edges = collect_reference_edges(&staging);

    // `x` in `for (item in x)` should reference the outer `x`, not the loop var
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to outer x in iterable expression: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "item"),
        "Expected reference to loop variable item: {edges:?}"
    );
}

// ============================================================
// When subject variable (M3 fix)
// ============================================================

#[test]
fn test_when_subject_variable() {
    let content = load_fixture("scopes/WhenSubject.kt");
    let staging = build_staging_graph(&content, "WhenSubject.kt");
    let edges = collect_reference_edges(&staging);

    // `x` declared in `when (val x = ...)` should be referenced in entries
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to when subject variable x: {edges:?}"
    );
}

// ============================================================
// Lambda destructuring parameters (M4 fix)
// ============================================================

#[test]
fn test_lambda_destructuring() {
    let content = load_fixture("lambda/LambdaDestructuring.kt");
    let staging = build_staging_graph(&content, "LambdaDestructuring.kt");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "num"),
        "Expected reference to destructured lambda param num: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "text"),
        "Expected reference to destructured lambda param text: {edges:?}"
    );
}

// ============================================================
// Anonymous function (M5 fix)
// ============================================================

#[test]
fn test_anonymous_function() {
    let content = load_fixture("lambda/AnonymousFunction.kt");
    let staging = build_staging_graph(&content, "AnonymousFunction.kt");
    let edges = collect_reference_edges(&staging);

    // `x` parameter of anonymous function should be referenced in body
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to anonymous function param x: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "result"),
        "Expected reference to local result: {edges:?}"
    );
}

// ============================================================
// Named argument labels (M6 fix)
// ============================================================

#[test]
fn test_named_argument_not_reference() {
    let content = load_fixture("localvars/NamedArguments.kt");
    let staging = build_staging_graph(&content, "NamedArguments.kt");
    let edges = collect_reference_edges(&staging);

    // `name` local is used as value in `greet(name = name, ...)`,
    // but the argument label `name` should NOT create a reference.
    // Only the value `name` should produce a reference.
    assert!(
        has_local_ref(&edges, "name"),
        "Expected reference to local name (as value): {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "result"),
        "Expected reference to local result: {edges:?}"
    );
    // The count of `name` references should be exactly 1 (the value usage),
    // not 2 (which would include the named argument label)
    assert_eq!(
        count_local_refs(&edges, "name"),
        1,
        "Named argument label should not create reference: {edges:?}"
    );
}

// ============================================================
// Function-typed local invocation (L7 fix)
// ============================================================

#[test]
fn test_function_typed_local_invocation() {
    let content = load_fixture("lambda/FunctionTypedLocal.kt");
    let staging = build_staging_graph(&content, "FunctionTypedLocal.kt");
    let edges = collect_reference_edges(&staging);

    // `f` is a function-typed local variable invoked as `f(5)`.
    // Should produce a Reference edge.
    assert!(
        has_local_ref(&edges, "f"),
        "Expected reference to function-typed local f: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "result"),
        "Expected reference to local result: {edges:?}"
    );
}

// ============================================================
// Local class with interface + outer capture (M1 fix)
// ============================================================

#[test]
fn test_local_class_interface_capture() {
    let content = load_fixture("classes/LocalClassInterface.kt");
    let staging = build_staging_graph(&content, "LocalClassInterface.kt");
    let edges = collect_reference_edges(&staging);

    // Local class `Printer` implements `Runnable` (unresolvable interface).
    // With the M1 fix, unresolved bases no longer suppress outer capture.
    // `captured` and `length` used inside the class body should produce Reference edges.
    assert!(
        has_local_ref(&edges, "captured"),
        "Expected reference to captured inside local class with interface: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "length"),
        "Expected reference to length inside local class with interface: {edges:?}"
    );
}

// ============================================================
// Property accessor local variables (M2 fix)
// ============================================================

#[test]
fn test_accessor_local_variables() {
    let content = load_fixture("localvars/AccessorLocals.kt");
    let staging = build_staging_graph(&content, "AccessorLocals.kt");
    let edges = collect_reference_edges(&staging);

    // Getter body: `val temp = backing; return temp`
    // `temp` is a local variable inside the getter — should be tracked.
    assert!(
        has_local_ref(&edges, "temp"),
        "Expected reference to local temp in getter body: {edges:?}"
    );

    // Setter body: `val validated = newValue; backing = validated`
    // `validated` is a local variable inside the setter — should be tracked.
    assert!(
        has_local_ref(&edges, "validated"),
        "Expected reference to local validated in setter body: {edges:?}"
    );

    // Setter parameter: `set(newValue)` — `newValue` used in body should be tracked.
    assert!(
        has_local_ref(&edges, "newValue"),
        "Expected reference to setter parameter newValue: {edges:?}"
    );
}

// ============================================================
// Parameterless lambda — no false `it` binding (L3 fix)
// ============================================================

#[test]
fn test_parameterless_lambda_no_false_it() {
    let content = load_fixture("lambda/ParameterlessLambda.kt");
    let staging = build_staging_graph(&content, "ParameterlessLambda.kt");
    let edges = collect_reference_edges(&staging);

    // `run { println(x) }` — parameterless lambda, no `it` usage.
    // `x` is an outer local variable and should have a reference.
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to x in parameterless lambda: {edges:?}"
    );

    // `items.forEach { println(it) }` — single-param lambda with implicit `it`.
    // `it` IS used here, so implicit binding should exist and `it` should be tracked.
    assert!(
        has_local_ref(&edges, "it"),
        "Expected reference to implicit it in forEach lambda: {edges:?}"
    );
}

#[test]
fn test_nested_lambda_it_boundary() {
    // Outer lambda uses implicit `it` (forEach), inner lambda uses explicit `inner`.
    // The outer `it` should resolve, and the inner lambda should NOT receive its own `it`.
    let content = load_fixture("lambda/NestedLambdaIt.kt");
    let staging = build_staging_graph(&content, "NestedLambdaIt.kt");
    let edges = collect_reference_edges(&staging);

    // Outer lambda's implicit `it` should be bound and referenced (used as `it * 2`)
    assert!(
        has_local_ref(&edges, "it"),
        "Expected reference to implicit it in outer forEach lambda: {edges:?}"
    );

    // `doubled` is a local inside the outer lambda
    assert!(
        !has_local_ref(&edges, "doubled"),
        "doubled is declared but never referenced after initialization: {edges:?}"
    );

    // Inner lambda has explicit `inner` parameter — should be referenced
    assert!(
        has_local_ref(&edges, "inner"),
        "Expected reference to explicit inner param in inner lambda: {edges:?}"
    );
}
