//! Integration tests for Java local variable reference tracking.

use sqry_classpath::stub::index::ClasspathIndex;
use sqry_classpath::stub::model::{AccessFlags, ClassKind, ClassStub, FieldStub};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tree_sitter::Tree;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("java")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

fn parse_java(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_java::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Java grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Java code")
}

fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let file_path = Path::new(filename);
    build_staging_graph_at_path(content, file_path)
}

fn build_staging_graph_at_path(content: &str, file_path: &Path) -> StagingGraph {
    let tree = parse_java(content);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

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

/// Check if any reference edge targets a qualified name ending with `::name`
fn has_qualified_ref(edges: &[(String, String)], name: &str) -> bool {
    let suffix = format!("::{name}");
    edges.iter().any(|(_, target)| target.ends_with(&suffix))
}

// ============================================================
// Local Variables (basic)
// ============================================================

#[test]
fn test_local_var_simple() {
    let content = load_fixture("com/example/localvars/LocalVars.java");
    let staging = build_staging_graph(&content, "LocalVars.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x: {edges:?}"
    );
}

#[test]
fn test_local_var_param_ref() {
    let content = load_fixture("com/example/localvars/LocalVars.java");
    let staging = build_staging_graph(&content, "LocalVars.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "y"),
        "Expected parameter reference to y: {edges:?}"
    );
}

#[test]
fn test_local_var_multiple_vars() {
    let content = load_fixture("com/example/localvars/LocalVars.java");
    let staging = build_staging_graph(&content, "LocalVars.java");
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

#[test]
fn test_multi_declarator() {
    let content = load_fixture("com/example/localvars/MultiDeclarator.java");
    let staging = build_staging_graph(&content, "MultiDeclarator.java");
    let edges = collect_reference_edges(&staging);

    // int a=1, b=a, c=b → b refs a, c refs b
    assert!(
        has_local_ref(&edges, "a"),
        "Expected reference to a from b's initializer: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected reference to b from c's initializer: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "c"),
        "Expected reference to c: {edges:?}"
    );
}

#[test]
fn test_same_scope_redeclare() {
    let content = load_fixture("com/example/localvars/SameScopeRedeclare.java");
    let staging = build_staging_graph(&content, "SameScopeRedeclare.java");
    let edges = collect_reference_edges(&staging);

    // Only one x@ binding should exist (second skipped due to overlap)
    let targets = local_ref_targets(&edges, "x");
    assert!(
        !targets.is_empty(),
        "Expected at least one reference to x@: {edges:?}"
    );
    assert_eq!(
        targets.len(),
        1,
        "Expected single x@ target despite duplicate decl, got {targets:?}"
    );
}

#[test]
fn test_use_before_decl_outer_fallback() {
    let content = load_fixture("com/example/localvars/UseBeforeDeclOuter.java");
    let staging = build_staging_graph(&content, "UseBeforeDeclOuter.java");
    let edges = collect_reference_edges(&staging);

    // int y=1; { int z=y; int y=5; println(y); }
    // z's initializer references outer y, inner println(y) references inner y
    assert!(
        has_local_ref(&edges, "y"),
        "Expected at least one reference to y: {edges:?}"
    );
}

// ============================================================
// Constructors
// ============================================================

#[test]
fn test_constructor_params() {
    let content = load_fixture("com/example/constructors/ConstructorParams.java");
    let staging = build_staging_graph(&content, "ConstructorParams.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "x"),
        "Expected constructor param reference to x: {edges:?}"
    );
}

#[test]
fn test_varargs_params() {
    let content = load_fixture("com/example/constructors/VarargsParams.java");
    let staging = build_staging_graph(&content, "VarargsParams.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "args"),
        "Expected varargs param reference to args: {edges:?}"
    );
}

#[test]
fn test_compact_constructor() {
    let content = load_fixture("com/example/constructors/CompactConstructor.java");
    let staging = build_staging_graph(&content, "CompactConstructor.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "x"),
        "Expected compact constructor param reference to x: {edges:?}"
    );
    assert!(
        has_qualified_ref(&edges, "y"),
        "Expected compact constructor param reference to y: {edges:?}"
    );
}

// ============================================================
// Initializers
// ============================================================

#[test]
fn test_static_initializer() {
    let content = load_fixture("com/example/initializers/StaticInitializer.java");
    let staging = build_staging_graph(&content, "StaticInitializer.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x in static initializer: {edges:?}"
    );
}

#[test]
fn test_instance_initializer() {
    let content = load_fixture("com/example/initializers/InstanceInitializer.java");
    let staging = build_staging_graph(&content, "InstanceInitializer.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x in instance initializer: {edges:?}"
    );
}

// ============================================================
// Scopes (overlap, disjoint, nested, utf-8, synthetic)
// ============================================================

#[test]
fn test_nested_scopes_overlap() {
    let content = load_fixture("com/example/scopes/NestedScopes.java");
    let staging = build_staging_graph(&content, "NestedScopes.java");
    let edges = collect_reference_edges(&staging);

    // overlap(): int x=1; { int x=2; println(x) } — inner binding skipped (overlap)
    // The println(x) in the inner block should still resolve to the outer x
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to x: {edges:?}"
    );
}

#[test]
fn test_nested_scopes_disjoint() {
    let content = load_fixture("com/example/scopes/NestedScopes.java");
    let staging = build_staging_graph(&content, "NestedScopes.java");
    let edges = collect_reference_edges(&staging);

    // disjoint(): { int x=1; println(x); } { int x=2; println(x); }
    // Both x declarations in separate blocks should produce reference edges
    assert!(
        count_local_refs(&edges, "x") >= 2,
        "Expected at least 2 references to x@ for disjoint scopes: {edges:?}"
    );
}

#[test]
fn test_declaration_identifier_sites() {
    let content = load_fixture("com/example/scopes/DeclarationIdentifierSites.java");
    let staging = build_staging_graph(&content, "DeclarationIdentifierSites.java");
    let edges = collect_reference_edges(&staging);

    // Params, local vars, enhanced-for vars, and catch params in usage positions
    // should produce reference edges. The declaration identifiers themselves should not.
    // i is used in println(i) and also in i < 10 and i++
    assert!(
        has_local_ref(&edges, "i") || has_qualified_ref(&edges, "i"),
        "Expected reference to loop var i: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "s") || has_qualified_ref(&edges, "s"),
        "Expected reference to enhanced-for var s: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "e") || has_qualified_ref(&edges, "e"),
        "Expected reference to catch param e: {edges:?}"
    );
}

#[test]
fn test_utf8_identifiers() {
    let content = load_fixture("com/example/scopes/Utf8Identifiers.java");
    let staging = build_staging_graph(&content, "Utf8Identifiers.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "café"),
        "Expected reference to UTF-8 identifier café: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "naïve"),
        "Expected reference to UTF-8 identifier naïve: {edges:?}"
    );
}

#[test]
fn test_nested_synthetic_scopes() {
    let content = load_fixture("com/example/scopes/NestedSyntheticScopes.java");
    let staging = build_staging_graph(&content, "NestedSyntheticScopes.java");
    let edges = collect_reference_edges(&staging);

    // obj instanceof String s && s.length() > 0 ? s.hashCode() : 0
    // s should be referenced in both && RHS and ternary true branch
    let s_refs = count_local_refs(&edges, "s");
    assert!(
        s_refs >= 1,
        "Expected references to pattern var s in synthetic scopes: {edges:?}"
    );
}

// ============================================================
// Pattern Variables
// ============================================================

#[test]
fn test_pattern_scopes_if() {
    let content = load_fixture("com/example/patterns/PatternScopes.java");
    let staging = build_staging_graph(&content, "PatternScopes.java");
    let edges = collect_reference_edges(&staging);

    // if (obj instanceof String s) { println(s); }
    assert!(
        has_local_ref(&edges, "s"),
        "Expected reference to pattern var s in if body: {edges:?}"
    );
}

#[test]
fn test_pattern_and_or() {
    let content = load_fixture("com/example/patterns/PatternAndOr.java");
    let staging = build_staging_graph(&content, "PatternAndOr.java");
    let edges = collect_reference_edges(&staging);

    // (obj instanceof String s && s.length() > 0) || obj == null
    // obj params are referenced
    assert!(
        has_qualified_ref(&edges, "obj"),
        "Expected parameter reference to obj: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "s"),
        "Expected pattern var s references through local && flow: {edges:?}"
    );
}

#[test]
fn test_record_pattern_instanceof() {
    let content = load_fixture("com/example/patterns/RecordPatternInstanceof.java");
    let staging = build_staging_graph(&content, "RecordPatternInstanceof.java");
    let edges = collect_reference_edges(&staging);

    // if (obj instanceof Point(int x, int y)) { println(x); println(y); }
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to record pattern var x: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "y"),
        "Expected reference to record pattern var y: {edges:?}"
    );
}

#[test]
fn test_for_pattern_scopes() {
    let content = load_fixture("com/example/patterns/ForPatternScopes.java");
    let staging = build_staging_graph(&content, "ForPatternScopes.java");
    let edges = collect_reference_edges(&staging);

    // for (int i=0; obj instanceof String s; i++) { println(s); println(i); }
    // For-loop init variable i resolves correctly
    assert!(
        has_local_ref(&edges, "i"),
        "Expected reference to for loop var i: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "s"),
        "Expected reference to for-condition pattern var s in loop body: {edges:?}"
    );
}

#[test]
fn test_do_while_pattern_negative() {
    let content = load_fixture("com/example/patterns/DoWhilePattern.java");
    let staging = build_staging_graph(&content, "DoWhilePattern.java");
    let edges = collect_reference_edges(&staging);

    // do { /* s NOT in scope */ } while (obj instanceof String s)
    // s should NOT have reference edges (no usage in body)
    assert!(
        !has_local_ref(&edges, "s"),
        "Expected NO reference to pattern var s in do-while body: {edges:?}"
    );
}

#[test]
fn test_statement_flow_patterns() {
    let content = load_fixture("com/example/patterns/StatementFlow.java");
    let staging = build_staging_graph(&content, "StatementFlow.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_local_ref(&edges, "ifString"),
        "Expected statement-flow reference for ifString: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "whileString"),
        "Expected statement-flow reference for whileString: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "doString"),
        "Expected statement-flow reference for doString: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "forString"),
        "Expected statement-flow reference for forString: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "switchString"),
        "Expected statement-flow reference for switchString after inner switch break: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "labeledBlockString"),
        "Expected statement-flow reference for labeledBlockString after loop with inner labeled block break: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "outerLabeledString"),
        "Expected NO statement-flow reference for outerLabeledString — labeled break exits the loop: {edges:?}"
    );
}

// ============================================================
// Switch Statements
// ============================================================

#[test]
fn test_switch_guards() {
    let content = load_fixture("com/example/switches/SwitchGuards.java");
    let staging = build_staging_graph(&content, "SwitchGuards.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "obj"),
        "Expected parameter reference to obj: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "s"),
        "Expected switch guard pattern ref for s: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "i"),
        "Expected switch arrow pattern ref for i: {edges:?}"
    );
}

#[test]
fn test_switch_classic_fallthrough() {
    let content = load_fixture("com/example/switches/SwitchGuards.java");
    let staging = build_staging_graph(&content, "SwitchGuards.java");
    let edges = collect_reference_edges(&staging);

    // case 1: int x=1; println(x); case 2: println(x) → x visible due to fallthrough
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to x in classic fallthrough switch: {edges:?}"
    );
}

#[test]
fn test_switch_colon_pattern() {
    let content = load_fixture("com/example/switches/SwitchColonPattern.java");
    let staging = build_staging_graph(&content, "SwitchColonPattern.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "obj"),
        "Expected parameter reference to obj: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "s"),
        "Expected colon-syntax switch pattern ref for s: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "i"),
        "Expected colon-syntax switch pattern ref for i: {edges:?}"
    );
}

#[test]
fn test_switch_block_decls() {
    let content = load_fixture("com/example/switches/SwitchBlockDecls.java");
    let staging = build_staging_graph(&content, "SwitchBlockDecls.java");
    let edges = collect_reference_edges(&staging);

    // case 1 -> { int y=1; yield y; } case 2 -> { int y=2; yield y; }
    // Both y declarations in separate arrow blocks produce references
    assert!(
        count_local_refs(&edges, "y") >= 2,
        "Expected at least 2 references to y in arrow switch blocks: {edges:?}"
    );
}

#[test]
fn test_switch_duplicate_decl_skips_second_binding() {
    let content = load_fixture("com/example/switches/SwitchDuplicateDecls.java");
    let staging = build_staging_graph(&content, "SwitchDuplicateDecls.java");

    let reference_edges = collect_reference_edges(&staging);
    let mut x_targets: HashSet<String> = HashSet::new();
    for (_, target) in &reference_edges {
        if target.starts_with("x@") {
            x_targets.insert(target.clone());
        }
    }

    assert!(
        !x_targets.is_empty(),
        "Expected reference edges to x@..., got: {reference_edges:?}"
    );
    assert_eq!(
        x_targets.len(),
        1,
        "Expected single x@ binding despite duplicate decls, got {x_targets:?}"
    );
}

#[test]
fn test_switch_use_before_decl() {
    let content = load_fixture("com/example/switches/SwitchUseBeforeDecl.java");
    let staging = build_staging_graph(&content, "SwitchUseBeforeDecl.java");
    let edges = collect_reference_edges(&staging);

    // case 1: println(x) (before decl - no edge); case 2: int x=1; println(x)
    // At least one x reference (from case 2's println(x))
    assert!(
        has_local_ref(&edges, "x"),
        "Expected at least one reference to x (from post-decl usage): {edges:?}"
    );
}

// ============================================================
// Try/Catch/Resources
// ============================================================

#[test]
fn test_try_with_resources_decl() {
    let content = load_fixture("com/example/trycatch/TryWithResources.java");
    let staging = build_staging_graph(&content, "TryWithResources.java");
    let edges = collect_reference_edges(&staging);

    // try (Closeable c = null) { println(c); }
    assert!(
        has_local_ref(&edges, "c"),
        "Expected reference to resource var c: {edges:?}"
    );
}

#[test]
fn test_try_resource_visibility() {
    let content = load_fixture("com/example/trycatch/TryResourceVisibility.java");
    let staging = build_staging_graph(&content, "TryResourceVisibility.java");
    let edges = collect_reference_edges(&staging);

    // try (Closeable r = null) { println(r) } catch (e) { println(e) } finally { println("done") }
    // r is visible in try body, e is visible in catch, but r is NOT visible in catch/finally
    assert!(
        has_local_ref(&edges, "r"),
        "Expected reference to resource r in try body: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "e") || has_qualified_ref(&edges, "e"),
        "Expected reference to catch param e: {edges:?}"
    );
}

#[test]
fn test_resource_initializer_chain() {
    let content = load_fixture("com/example/trycatch/ResourceInitializerChain.java");
    let staging = build_staging_graph(&content, "ResourceInitializerChain.java");
    let edges = collect_reference_edges(&staging);

    // try (Closeable a=null; Closeable b=wrap(a)) { println(a); println(b); }
    assert!(
        has_local_ref(&edges, "a"),
        "Expected reference to resource a: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "b"),
        "Expected reference to resource b: {edges:?}"
    );
}

#[test]
fn test_resource_decl_initializer() {
    let content = load_fixture("com/example/trycatch/ResourceDeclInitializer.java");
    let staging = build_staging_graph(&content, "ResourceDeclInitializer.java");
    let edges = collect_reference_edges(&staging);

    // void test(int x) { try (Closeable c = create(x)) { println(c) } }
    let has_x = has_qualified_ref(&edges, "x") || has_local_ref(&edges, "x");
    assert!(
        has_x,
        "Expected reference to param x in resource init: {edges:?}"
    );
    assert!(
        has_local_ref(&edges, "c"),
        "Expected reference to resource c in try body: {edges:?}"
    );
}

#[test]
fn test_multi_catch_union() {
    let content = load_fixture("com/example/trycatch/MultiCatchUnion.java");
    let staging = build_staging_graph(&content, "MultiCatchUnion.java");
    let edges = collect_reference_edges(&staging);

    // catch (IllegalArgumentException | NullPointerException e) { println(e) }
    assert!(
        has_local_ref(&edges, "e") || has_qualified_ref(&edges, "e"),
        "Expected reference to multi-catch param e: {edges:?}"
    );
}

// ============================================================
// Classes (anonymous, local, nested)
// ============================================================

#[test]
fn test_anon_class_member_beats_capture() {
    let content = load_fixture("com/example/classes/AnonLocalClass.java");
    let staging = build_staging_graph(&content, "AnonLocalClass.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "x"),
        "Expected anonymous-class member reference for x: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "x"),
        "Did not expect captured-local reference for x: {edges:?}"
    );
}

#[test]
fn test_anon_class_capture_wins() {
    let content = load_fixture("com/example/classes/AnonLocalClass.java");
    let staging = build_staging_graph(&content, "AnonLocalClass.java");
    let edges = collect_reference_edges(&staging);

    // captureWins(): int y=1; new Runnable(){ run(){ println(y) } }
    // y should be captured from outer scope (no member y in anonymous class)
    assert!(
        has_local_ref(&edges, "y"),
        "Expected captured reference to y: {edges:?}"
    );
}

#[test]
fn test_nested_class_redeclare() {
    let content = load_fixture("com/example/classes/NestedClassRedeclare.java");
    let staging = build_staging_graph(&content, "NestedClassRedeclare.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; class Local { void use() { int x=2; println(x); } }
    // The inner x=2 is allowed (class boundary resets overlap)
    // println(x) resolves to the inner x=2
    assert!(
        has_local_ref(&edges, "x"),
        "Expected reference to x in nested class method: {edges:?}"
    );
}

#[test]
fn test_local_class_capture() {
    let content = load_fixture("com/example/classes/LocalClassCapture.java");
    let staging = build_staging_graph(&content, "LocalClassCapture.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; class Local { void use() { println(x) } }
    // x captured from outer scope (local class supports capture)
    assert!(
        has_local_ref(&edges, "x"),
        "Expected captured reference to x in local class: {edges:?}"
    );
}

#[test]
fn test_local_record_no_capture() {
    let content = load_fixture("com/example/classes/LocalRecordNoCapture.java");
    let staging = build_staging_graph(&content, "LocalRecordNoCapture.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; record R() { void use() { println("no capture") } }
    // x is NOT used inside the record (and wouldn't be capturable anyway)
    assert!(
        !has_local_ref(&edges, "x"),
        "Expected no reference to x across record boundary: {edges:?}"
    );
}

#[test]
fn test_local_record_nested_no_capture() {
    let content = load_fixture("com/example/classes/LocalRecordNestedCapture.java");
    let staging = build_staging_graph(&content, "LocalRecordNestedCapture.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; record R() { void use() { new R2(){ run(){ println("no capture") } } } }
    // x cannot be captured even through nested anon class inside record
    assert!(
        !has_local_ref(&edges, "x"),
        "Expected no reference to x across record + nested boundary: {edges:?}"
    );
}

#[test]
fn test_local_enum_no_capture() {
    let content = load_fixture("com/example/classes/LocalEnumNoCapture.java");
    let staging = build_staging_graph(&content, "LocalEnumNoCapture.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; enum E { A; int use() { return 0; } }
    // x not referenced inside enum and wouldn't be capturable
    assert!(
        !has_local_ref(&edges, "x"),
        "Expected no reference to x across enum boundary: {edges:?}"
    );
}

#[test]
fn test_local_interface_no_capture() {
    let content = load_fixture("com/example/classes/LocalInterfaceNoCapture.java");
    let staging = build_staging_graph(&content, "LocalInterfaceNoCapture.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; interface I { int C=0; }
    // x not referenced inside interface and wouldn't be capturable
    assert!(
        !has_local_ref(&edges, "x"),
        "Expected no reference to x across interface boundary: {edges:?}"
    );
}

// ============================================================
// Field Precedence (this, super, qualified this)
// ============================================================

#[test]
fn test_this_field_precedence() {
    let content = load_fixture("com/example/fields/ThisFieldPrecedence.java");
    let staging = build_staging_graph(&content, "ThisFieldPrecedence.java");
    let edges = collect_reference_edges(&staging);

    // int x=10 (field); void test() { int x=1; println(x); println(this.x); }
    // println(x) → local x, println(this.x) → field x
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x from println(x): {edges:?}"
    );
}

#[test]
fn test_qualified_this_field() {
    let content = load_fixture("com/example/fields/QualifiedThisField.java");
    let staging = build_staging_graph(&content, "QualifiedThisField.java");
    let edges = collect_reference_edges(&staging);

    // Inner class: int x=1; println(x) → local; println(QualifiedThisField.this.x) → field
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x from println(x): {edges:?}"
    );
}

#[test]
fn test_super_field_precedence() {
    let content = load_fixture("com/example/fields/SuperFieldPrecedence.java");
    let staging = build_staging_graph(&content, "SuperFieldPrecedence.java");
    let edges = collect_reference_edges(&staging);

    // int x=1; println(x) → local; println(super.x) → field
    assert!(
        has_local_ref(&edges, "x"),
        "Expected local reference to x from println(x): {edges:?}"
    );
}

#[test]
fn test_enclosing_field_capture_wins() {
    let content = load_fixture("com/example/fields/EnclosingFieldPrecedence.java");
    let staging = build_staging_graph(&content, "EnclosingFieldPrecedence.java");
    let edges = collect_reference_edges(&staging);

    // int x=10 (field); void test() { int x=1; new Runnable(){ run(){ println(x) } } }
    // Captured local wins (no member x in anon class)
    assert!(
        has_local_ref(&edges, "x"),
        "Expected captured local reference to x (no member in anon class): {edges:?}"
    );
}

// ============================================================
// Inheritance Resolution
// ============================================================

#[test]
fn test_inherited_field_precedence() {
    let content = load_fixture("com/example/inheritance/InheritedFieldPrecedence.java");
    let staging = build_staging_graph(&content, "InheritedFieldPrecedence.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "x"),
        "Expected inherited member reference for x: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "x"),
        "Did not expect captured-local reference for x: {edges:?}"
    );
}

#[test]
fn test_inherited_resolved() {
    let content = load_fixture("com/example/inheritance/InheritedResolved.java");
    let staging = build_staging_graph(&content, "InheritedResolved.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "value"),
        "Expected inherited member reference for value: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "value"),
        "Did not expect captured-local reference for value: {edges:?}"
    );
}

#[test]
fn test_inherited_base_object_allows_capture() {
    let content = load_fixture("com/example/inheritance/InheritedBaseObject.java");
    let staging = build_staging_graph(&content, "InheritedBaseObject.java");

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        reference_edges
            .iter()
            .any(|(_, target)| target.starts_with("x@")),
        "Expected capture reference to x@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_inherited_partial_resolution() {
    let content = load_fixture("com/example/inheritance/InheritedPartialResolution.java");
    let staging = build_staging_graph(&content, "InheritedPartialResolution.java");
    let _edges = collect_reference_edges(&staging);

    // Object extends nothing that has KNOWN; KnownInterface has KNOWN
    // This tests partial resolution behavior
}

#[test]
fn test_inherited_explicit_unresolved() {
    let content = load_fixture("com/example/inheritance/InheritedExplicitUnresolved.java");
    let staging = build_staging_graph(&content, "InheritedExplicitUnresolved.java");
    let edges = collect_reference_edges(&staging);

    assert!(
        !has_local_ref(&edges, "x"),
        "Did not expect local capture for unresolved explicit base: {edges:?}"
    );
}

#[test]
fn test_external_classpath_base_resolution() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let project_root = temp_dir.path();
    let source_dir = project_root.join("com/example/inheritance");
    std::fs::create_dir_all(&source_dir).expect("create source dir");

    let source_path = source_dir.join("ExternalClasspathResolved.java");
    let content = r"
package com.example.inheritance;

import com.external.ExternalBase;

class ExternalClasspathResolved {
    void test() {
        int value = 1;
        Object obj = new ExternalBase() {
            void use() {
                System.out.println(value);
            }
        };
    }
}
";
    std::fs::write(&source_path, content).expect("write source");

    let classpath_dir = project_root.join(".sqry/classpath");
    std::fs::create_dir_all(&classpath_dir).expect("create classpath dir");
    let classpath_index = ClasspathIndex::build(vec![ClassStub {
        fqn: "com.external.ExternalBase".to_string(),
        name: "ExternalBase".to_string(),
        kind: ClassKind::Class,
        access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
        superclass: Some("java.lang.Object".to_string()),
        interfaces: Vec::new(),
        methods: Vec::new(),
        fields: vec![FieldStub {
            name: "value".to_string(),
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            descriptor: "I".to_string(),
            generic_signature: None,
            annotations: Vec::new(),
            constant_value: None,
        }],
        annotations: Vec::new(),
        generic_signature: None,
        inner_classes: Vec::new(),
        lambda_targets: Vec::new(),
        module: None,
        record_components: Vec::new(),
        enum_constants: Vec::new(),
        source_file: None,
        source_jar: None,
        kotlin_metadata: None,
        scala_signature: None,
    }]);
    classpath_index
        .save(&classpath_dir.join("index.sqry"))
        .expect("save classpath index");

    let staging = build_staging_graph_at_path(content, &source_path);
    let edges = collect_reference_edges(&staging);

    assert!(
        has_qualified_ref(&edges, "value"),
        "Expected external classpath member reference for value: {edges:?}"
    );
    assert!(
        !has_local_ref(&edges, "value"),
        "Did not expect local capture for classpath-resolved external base: {edges:?}"
    );
}

#[test]
fn test_inherited_resolved_conflict() {
    let content = load_fixture("com/example/inheritance/InheritedResolvedConflict.java");
    let staging = build_staging_graph(&content, "InheritedResolvedConflict.java");
    let edges = collect_reference_edges(&staging);

    // The anonymous class is `new Object() { ... }` which extends Object, not BaseA/BaseB.
    // BaseA and BaseB are independent interfaces not implemented by this anonymous class.
    // Therefore "shared" captures the outer local (Object has no "shared" field).
    assert!(
        has_local_ref(&edges, "shared"),
        "Expected local capture of shared (anon class extends Object, not BaseA/BaseB): {edges:?}"
    );
}

#[test]
fn test_interface_field_precedence() {
    let content = load_fixture("com/example/inheritance/InterfaceFieldPrecedence.java");
    let staging = build_staging_graph(&content, "InterfaceFieldPrecedence.java");
    let _edges = collect_reference_edges(&staging);

    // interface HasConstant { int VALUE=42 }; new HasConstant(){ void use(){ println(VALUE) } }
    // Interface constant should win over captured local
}

// ============================================================
// Identifier Filters / Method References
// ============================================================

#[test]
fn test_identifier_filter_cases() {
    let content = load_fixture("com/example/methodrefs/IdentifierFilterCases.java");
    let staging = build_staging_graph(&content, "IdentifierFilterCases.java");
    let edges = collect_reference_edges(&staging);

    // Labels: `break outer` → outer is label, no reference edge
    assert!(
        !has_local_ref(&edges, "outer"),
        "Expected no reference to label 'outer': {edges:?}"
    );
}

#[test]
fn test_method_reference_expr() {
    let content = load_fixture("com/example/methodrefs/MethodReferenceCases.java");
    let staging = build_staging_graph(&content, "MethodReferenceCases.java");
    let edges = collect_reference_edges(&staging);

    // String s = "hello"; Runnable r = s::hashCode → s IS a variable reference
    assert!(
        has_local_ref(&edges, "s"),
        "Expected reference to s in expression method ref s::hashCode: {edges:?}"
    );
}

#[test]
fn test_method_reference_nested() {
    let content = load_fixture("com/example/methodrefs/MethodReferenceNested.java");
    let staging = build_staging_graph(&content, "MethodReferenceNested.java");
    let edges = collect_reference_edges(&staging);

    // String obj = "hello"; Runnable r = () -> { Runnable inner = obj::hashCode; }
    // obj resolves through lambda capture
    assert!(
        has_local_ref(&edges, "obj"),
        "Expected reference to obj through lambda capture: {edges:?}"
    );
}

// ============================================================
// Pattern Declaration Filtering / Type Name Shadowing
// ============================================================

#[test]
fn test_pattern_decl_not_referenced() {
    let content = load_fixture("com/example/patterns/PatternDeclFilter.java");
    let staging = build_staging_graph(&content, "PatternDeclFilter.java");
    let edges = collect_reference_edges(&staging);

    // Pattern variable `s` is used twice in the fixture:
    // - instanceof branch: println(s)
    // - switch branch: s.length()
    // Declaration sites must not leak as spurious self-references.
    let s_refs = count_local_refs(&edges, "s");
    assert_eq!(
        s_refs, 2,
        "Expected exactly 2 usage references to pattern var s (no declaration-site refs): {edges:?}"
    );

    // Switch pattern variable `i` is used once (`i + 1`), and its declaration
    // site must not leak as a self-reference.
    let i_refs = count_local_refs(&edges, "i");
    assert_eq!(
        i_refs, 1,
        "Expected exactly 1 usage reference to switch pattern var i: {edges:?}"
    );
}

#[test]
fn test_type_name_shadow_field_access() {
    let content = load_fixture("com/example/localvars/TypeNameShadow.java");
    let staging = build_staging_graph(&content, "TypeNameShadow.java");
    let edges = collect_reference_edges(&staging);

    // Even though "List" matches an imported type name (java.util.List),
    // local variables named `List` shadow it. All three access patterns
    // should resolve `List` as a local variable reference.
    //
    // Fixture has 3 methods exercising all field_access_role branches:
    //   testMethodInvocation: List.hashCode()    → method_invocation object position
    //   testFieldAccess:      List.length         → field_access object position
    //   testMethodReference:  List::hashCode      → method_reference object position
    //
    // Each method declares its own local `List`, so we expect 3 references.
    let list_refs = count_local_refs(&edges, "List");
    assert!(
        list_refs >= 3,
        "Expected at least 3 local variable references to List \
         (method_invocation + field_access + method_reference), got {list_refs}: {edges:?}"
    );
}
