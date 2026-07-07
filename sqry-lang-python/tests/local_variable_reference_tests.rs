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

/// Declaration start bytes that `References` edges to `name` point at.
///
/// A qualified variable target is `name@<decl_start_byte>`; parsing the byte
/// pins which declaration a reference resolves to. A phantom function-local
/// binding and the true outer/module binding have different `decl_start_byte`,
/// so asserting the target byte pins the global/nonlocal semantics exactly.
fn ref_target_bytes(edges: &[(String, String)], name: &str) -> Vec<usize> {
    let prefix = format!("{name}@");
    edges
        .iter()
        .filter_map(|(_, to)| to.strip_prefix(&prefix))
        .filter_map(|b| b.parse::<usize>().ok())
        .collect()
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

// ============================================================
// global / nonlocal scope wiring (issue #465)
// ============================================================

#[test]
fn test_global_no_module_binding_has_no_local_ref() {
    // AC2: a `global` name with no module-level binding must not resolve to a
    // phantom function-local declaration.
    let content = load_fixture("global_no_binding.py");
    let staging = build_staging_graph(&content, "global_no_binding.py");
    let edges = collect_reference_edges(&staging);
    assert!(
        !has_local_ref(&edges, "config"),
        "global-declared config with no module binding must not resolve to a \
         local declaration: {edges:?}"
    );
}

#[test]
fn test_global_with_module_binding_resolves_to_module() {
    // AC3: a `global` name with a module-level binding resolves to the module
    // declaration, not the in-function assignment.
    let content = load_fixture("global_with_binding.py");
    let staging = build_staging_graph(&content, "global_with_binding.py");
    let edges = collect_reference_edges(&staging);

    let module_byte = content.find("config = 0").expect("module decl present");
    let targets = ref_target_bytes(&edges, "config");
    assert!(
        !targets.is_empty(),
        "expected references to config: {edges:?}"
    );
    assert!(
        targets.iter().all(|&b| b == module_byte),
        "every config reference must resolve to the module declaration at {module_byte}, got {targets:?}"
    );
}

#[test]
fn test_nonlocal_resolves_to_enclosing() {
    // AC4: a `nonlocal` name resolves to the enclosing function's declaration,
    // never to an inner phantom-local.
    let content = load_fixture("nonlocal_nested.py");
    let staging = build_staging_graph(&content, "nonlocal_nested.py");
    let edges = collect_reference_edges(&staging);

    let outer_byte = content.find("total = 0").expect("outer decl present");
    let inner_decl = content
        .find("total = total + 1")
        .expect("inner assignment present");

    let targets = ref_target_bytes(&edges, "total");
    assert!(
        !targets.is_empty(),
        "expected references to total: {edges:?}"
    );
    assert!(
        targets.iter().all(|&b| b == outer_byte),
        "every total reference must resolve to outer's declaration at {outer_byte}, got {targets:?}"
    );
    assert!(
        !targets.contains(&inner_decl),
        "no reference may resolve to the inner phantom-local at {inner_decl}: {targets:?}"
    );
}

#[test]
fn test_nested_global_does_not_suppress_outer() {
    // AC5a: an inner `global` declaration must not suppress the outer function's
    // local of the same name.
    let content = load_fixture("nested_scope_independence.py");
    let staging = build_staging_graph(&content, "nested_scope_independence.py");
    let edges = collect_reference_edges(&staging);

    let outer_byte = content.find("value = 1").expect("outer decl present");
    let targets = ref_target_bytes(&edges, "value");
    assert!(
        targets.iter().all(|&b| b == outer_byte),
        "outer's local value must survive inner's global declaration; \
         references must resolve to {outer_byte}, got {targets:?}"
    );
    assert!(
        targets.contains(&outer_byte),
        "outer return value must resolve to the outer local at {outer_byte}: {edges:?}"
    );
}

#[test]
fn test_module_level_global_is_noop() {
    // AC6: module-level `global x` is a Python no-op; the module binding of `x`
    // stays intact. Regression-only guard (green pre- and post-fix).
    let content = load_fixture("module_global_noop.py");
    let staging = build_staging_graph(&content, "module_global_noop.py");
    let edges = collect_reference_edges(&staging);

    let module_byte = content.find("x = 5").expect("module decl present");
    let targets = ref_target_bytes(&edges, "x");
    assert!(
        targets.iter().all(|&b| b == module_byte) && !targets.is_empty(),
        "module-level global is a no-op; read_it must resolve x to {module_byte}, got {targets:?}"
    );
}

#[test]
fn test_outer_global_does_not_suppress_inner_local() {
    // AC5b: an outer `global` declaration must not leak into and suppress an
    // inner function's own local of the same name.
    let content = load_fixture("nested_scope_independence_outer.py");
    let staging = build_staging_graph(&content, "nested_scope_independence_outer.py");
    let edges = collect_reference_edges(&staging);

    let inner_byte = content.find("value = 10").expect("inner decl present");
    let outer_decl_byte = content.find("value = 1").expect("outer decl present");
    let targets = ref_target_bytes(&edges, "value");

    // AC5b (no outer -> inner leak): inner's own local value must survive
    // outer's global declaration, so inner's `return value` resolves to
    // inner_byte.
    assert!(
        targets.contains(&inner_byte),
        "inner's own local value must survive outer's global declaration; \
         inner's reference must resolve to {inner_byte}, got {targets:?}"
    );

    // Discriminating #465 assertion (fails on broken code): outer's `value = 1`
    // is `global`, so it must be suppressed. outer's `outer_read = value` must
    // NOT resolve to the phantom outer-local `value = 1` byte.
    assert!(
        !targets.contains(&outer_decl_byte),
        "outer's `global value` must suppress the `value = 1` local; outer's read \
         must not resolve to the phantom outer-local at {outer_decl_byte}, got {targets:?}"
    );
}

#[test]
fn test_with_as_global_has_no_local_ref() {
    // AC8: a `with ... as` target declared `global` (with no module binding)
    // must not resolve to a phantom local.
    let content = load_fixture("with_as_global.py");
    let staging = build_staging_graph(&content, "with_as_global.py");
    let edges = collect_reference_edges(&staging);
    assert!(
        !has_local_ref(&edges, "handle"),
        "global-declared with-as target handle must not resolve to a local \
         declaration: {edges:?}"
    );
}

#[test]
fn test_except_as_global_has_no_local_ref() {
    // AC9: an `except ... as` target declared `global` (with no module binding)
    // must not resolve to a phantom local.
    let content = load_fixture("except_as_global.py");
    let staging = build_staging_graph(&content, "except_as_global.py");
    let edges = collect_reference_edges(&staging);
    assert!(
        !has_local_ref(&edges, "err"),
        "global-declared except-as target err must not resolve to a local \
         declaration: {edges:?}"
    );
}

#[test]
fn test_with_as_nonlocal_resolves_to_enclosing() {
    // AC8 (nonlocal variant): a `with ... as` target declared `nonlocal`
    // resolves to the enclosing function's declaration.
    let content = load_fixture("with_as_nonlocal.py");
    let staging = build_staging_graph(&content, "with_as_nonlocal.py");
    let edges = collect_reference_edges(&staging);

    let outer_byte = content.find("handle = None").expect("outer decl present");
    // `bind_with_item` records the declaration at the `handle` identifier start,
    // not at the `as` keyword, so target the identifier byte. Targeting the `as`
    // byte would make the negative assertion trivially true.
    let inner_decl = content
        .find("as handle")
        .map(|b| b + "as ".len())
        .expect("inner with-as target present");

    let targets = ref_target_bytes(&edges, "handle");
    assert!(
        !targets.is_empty(),
        "expected references to handle: {edges:?}"
    );
    assert!(
        targets.iter().all(|&b| b == outer_byte),
        "every handle reference must resolve to outer's declaration at {outer_byte}, got {targets:?}"
    );
    assert!(
        !targets.contains(&inner_decl),
        "no reference may resolve to the inner phantom-local with-as target: {targets:?}"
    );
}

#[test]
fn test_except_as_nonlocal_resolves_to_enclosing() {
    // AC9 (nonlocal variant): an `except ... as` target declared `nonlocal`
    // resolves to the enclosing function's declaration.
    let content = load_fixture("except_as_nonlocal.py");
    let staging = build_staging_graph(&content, "except_as_nonlocal.py");
    let edges = collect_reference_edges(&staging);

    let outer_byte = content.find("err = None").expect("outer decl present");
    let targets = ref_target_bytes(&edges, "err");
    assert!(!targets.is_empty(), "expected references to err: {edges:?}");
    assert!(
        targets.iter().all(|&b| b == outer_byte),
        "every err reference must resolve to outer's declaration at {outer_byte}, got {targets:?}"
    );
}

#[test]
fn test_walrus_global_has_no_local_ref() {
    // AC10: a walrus (`:=`) target declared `global` (with no module binding)
    // must not resolve to a phantom local. Exercises the bind_walrus binder.
    let content = load_fixture("walrus_global.py");
    let staging = build_staging_graph(&content, "walrus_global.py");
    let edges = collect_reference_edges(&staging);
    assert!(
        !has_local_ref(&edges, "cached"),
        "global-declared walrus target cached must not resolve to a local \
         declaration: {edges:?}"
    );
}

#[test]
fn test_nested_global_read_resolves_to_intermediate_binding() {
    // AC11 (limitation pin): the shared resolver has no `global` short-circuit,
    // so a nested `global` read resolves to the enclosing intermediate binding,
    // not the module binding. Pins the documented limitation.
    let content = load_fixture("nested_global_intermediate.py");
    let staging = build_staging_graph(&content, "nested_global_intermediate.py");
    let edges = collect_reference_edges(&staging);

    let module_byte = content.find("value = 100").expect("module decl present");
    // `rfind` avoids the prefix collision with `value = 100` on line 1: plain
    // `find("value = 1")` would match that module decl at byte 0, not outer's
    // intermediate binding. rfind lands on outer's `value = 1`.
    let inter_byte = content
        .rfind("value = 1")
        .expect("intermediate decl present");
    let targets = ref_target_bytes(&edges, "value");
    assert!(
        !targets.is_empty(),
        "expected references to value: {edges:?}"
    );
    // Documented limitation: inner's `return value` resolves to outer's
    // intermediate binding, not module. A future resolver-aware fix flips this
    // to `module_byte`; update this test then.
    assert!(
        targets.iter().all(|&b| b == inter_byte),
        "nested global read currently resolves to the intermediate binding at \
         {inter_byte} (limitation), got {targets:?}"
    );
    assert!(
        !targets.contains(&module_byte),
        "resolver has no global short-circuit yet; read does not reach module \
         {module_byte}: {targets:?}"
    );
}

#[test]
fn test_for_target_global_has_no_local_ref() {
    // AC10b: a `for` target declared `global` (with no module binding) must not
    // resolve to a phantom local. Exercises the for_statement dispatch through
    // bind_for_variable -> bind_pattern.
    let content = load_fixture("for_target_global.py");
    let staging = build_staging_graph(&content, "for_target_global.py");
    let edges = collect_reference_edges(&staging);
    assert!(
        !has_local_ref(&edges, "item"),
        "global-declared for-target item must not resolve to a local \
         declaration: {edges:?}"
    );
}
