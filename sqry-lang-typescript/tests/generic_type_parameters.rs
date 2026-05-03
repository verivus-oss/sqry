//! U21 — `C2_GEN_TP_TS` — Per-type-parameter Type-node emission for
//! generic function/method/class/interface/type-alias declarations.
//!
//! Spec ref: REQ:R0030
//! Design ref: cross-language-field-emission/02_DESIGN §4.18
//!
//! Acceptance criteria covered (DAG U21):
//!   AC-1  All five walkers iterate the `type_parameters` field
//!         (`function_declaration`, `method_definition`, `class_declaration`,
//!         `interface_declaration`, `type_alias_declaration`).
//!   AC-2  Qualified name `<FunctionName>.<ParamName>` (top-level) or
//!         `<ClassName>.<MethodName>.<ParamName>` (member); spans
//!         anchored on the parameter identifier (not the full decl).
//!   AC-3  `extends` constraints emit `TypeOf{Constraint}` edges;
//!         `default_type` (`<T = string>`) emits References edges.
//!   AC-4  Mapped-type binders `{ [K in keyof T]: ... }` emit a Type
//!         node for `K` with qname `<TypeAlias>.<K>`.
//!   AC-5  Variadic tuples `<T extends unknown[]>` emit a Constraint
//!         edge to a synthetic Type node named `unknown[]`.
//!   AC-6  Conditional types: in `type R<T> = T extends X ? Y : Z`,
//!         `T` still gets a Type node; the conditional path itself
//!         emits References edges only (no Constraint).
//!
//! Note on separators: the TS plugin builds qualified names with `.`
//! (TS native source separator), but `canonicalize_graph_qualified_name`
//! in `sqry-core` rewrites them to `::` for graph-internal storage.
//! These tests assert the post-canonicalisation `::` form.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_typescript::TypeScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

// ───────────────────────── helpers ─────────────────────────

fn parse_typescript(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("Failed to load TypeScript grammar");
    parser.parse(source, None).expect("Failed to parse")
}

fn build_staging(source: &str, file_name: &str) -> StagingGraph {
    let tree = parse_typescript(source);
    let file = Path::new(file_name);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");
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

fn build_node_lookup(staging: &StagingGraph) -> HashMap<u32, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let idx = expected_id.index();
                let name_id = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_id)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_id}>"));
                Some((idx, (name, entry.kind)))
            } else {
                None
            }
        })
        .collect()
}

fn type_node_qnames(staging: &StagingGraph) -> Vec<String> {
    build_node_lookup(staging)
        .into_values()
        .filter_map(|(name, kind)| {
            if matches!(kind, NodeKind::Type) {
                Some(name)
            } else {
                None
            }
        })
        .collect()
}

fn has_type_node(staging: &StagingGraph, qname: &str) -> bool {
    type_node_qnames(staging).iter().any(|q| q == qname)
}

fn collect_constraint_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let nodes = build_node_lookup(staging);
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
                && let EdgeKind::TypeOf { context, .. } = kind
                && *context == Some(TypeOfContext::Constraint)
            {
                let s = nodes
                    .get(&source.index())
                    .map_or_else(|| format!("<u:{}>", source.index()), |(n, _)| n.clone());
                let t = nodes
                    .get(&target.index())
                    .map_or_else(|| format!("<u:{}>", target.index()), |(n, _)| n.clone());
                return Some((s, t));
            }
            None
        })
        .collect()
}

fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let nodes = build_node_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                let s = nodes
                    .get(&source.index())
                    .map_or_else(|| format!("<u:{}>", source.index()), |(n, _)| n.clone());
                let t = nodes
                    .get(&target.index())
                    .map_or_else(|| format!("<u:{}>", target.index()), |(n, _)| n.clone());
                return Some((s, t));
            }
            None
        })
        .collect()
}

/// Assert that the Type node for `qname` carries a span anchored on
/// the parameter identifier (small byte length, not the full
/// declaration).
fn assert_type_node_has_real_span(staging: &StagingGraph, qname: &str) {
    let strings = build_string_lookup(staging);
    let mut found_any = false;
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Type)
        {
            let name_id = entry.qualified_name.unwrap_or(entry.name).index();
            let name = strings.get(&name_id).cloned().unwrap_or_default();
            if name == qname {
                found_any = true;
                assert!(
                    entry.end_column > entry.start_column || entry.end_line > entry.start_line,
                    "Type-parameter node {qname} must have non-empty span"
                );
                if entry.start_line == entry.end_line {
                    let len = entry.end_column - entry.start_column;
                    assert!(
                        len <= 64,
                        "Type-parameter node {qname} span should anchor on the parameter identifier (≤64 bytes), got {len} bytes",
                    );
                }
            }
        }
    }
    assert!(
        found_any,
        "Expected at least one Type node with qname {qname}"
    );
}

// ───────── AC-1, AC-2: function declarations ─────────

#[test]
fn function_with_single_type_parameter_emits_type_node() {
    let src = r"
function identity<T>(x: T): T {
    return x;
}
";
    let staging = build_staging(src, "fn_single.ts");
    assert!(
        has_type_node(&staging, "identity::T"),
        "Expected Type node `identity::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "identity::T");
}

#[test]
fn function_with_multiple_type_parameters_emits_each() {
    let src = r"
function pair<K, V>(k: K, v: V): [K, V] {
    return [k, v];
}
";
    let staging = build_staging(src, "fn_multi.ts");
    assert!(has_type_node(&staging, "pair::K"));
    assert!(has_type_node(&staging, "pair::V"));
}

// ───────── AC-1, AC-2: class declarations ─────────

#[test]
fn class_with_single_type_parameter_emits_type_node() {
    let src = r"
class Box<T> {
    value: T;
}
";
    let staging = build_staging(src, "class_single.ts");
    assert!(
        has_type_node(&staging, "Box::T"),
        "Expected Type node `Box::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Box::T");
}

#[test]
fn class_with_multiple_type_parameters_emits_each() {
    let src = r"
class Pair<K, V> {
    key: K;
    value: V;
}
";
    let staging = build_staging(src, "class_multi.ts");
    assert!(has_type_node(&staging, "Pair::K"));
    assert!(has_type_node(&staging, "Pair::V"));
}

// ───────── AC-1, AC-2: interface declarations ─────────

#[test]
fn interface_with_type_parameter_emits_type_node() {
    let src = r"
interface IFoo<T> {
    get(): T;
}
";
    let staging = build_staging(src, "iface_single.ts");
    assert!(
        has_type_node(&staging, "IFoo::T"),
        "Expected Type node `IFoo::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "IFoo::T");
}

// ───────── AC-1, AC-2: type-alias declarations ─────────

#[test]
fn type_alias_with_type_parameter_emits_type_node() {
    let src = r"
type Box<T> = { value: T };
";
    let staging = build_staging(src, "alias.ts");
    assert!(
        has_type_node(&staging, "Box::T"),
        "Expected Type node `Box::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Box::T");
}

// ───────── AC-1, AC-2: methods on classes ─────────

#[test]
fn class_method_type_parameter_uses_class_method_qname() {
    let src = r"
class Box {
    method<T>(x: T): T {
        return x;
    }
}
";
    let staging = build_staging(src, "method.ts");
    assert!(
        has_type_node(&staging, "Box::method::T"),
        "Expected Type node `Box::method::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Box::method::T");
}

// ───────── AC-3: extends → Constraint edge ─────────

#[test]
fn function_type_parameter_extends_emits_constraint_edge() {
    let src = r"
function pick<T extends string>(x: T): T {
    return x;
}
";
    let staging = build_staging(src, "extends_fn.ts");
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges.iter().any(|(s, t)| s == "pick::T" && t == "string"),
        "Expected Constraint edge pick::T -> string, got: {edges:?}",
    );
}

#[test]
fn interface_type_parameter_extends_emits_constraint_edge() {
    let src = r"
interface IFoo<T extends Bar> {
    get(): T;
}
";
    let staging = build_staging(src, "extends_iface.ts");
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges.iter().any(|(s, t)| s == "IFoo::T" && t == "Bar"),
        "Expected Constraint edge IFoo::T -> Bar, got: {edges:?}",
    );
}

#[test]
fn class_type_parameter_extends_emits_constraint_edge() {
    let src = r"
class Box<T extends Comparable> {
    value: T;
}
";
    let staging = build_staging(src, "extends_class.ts");
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Box::T" && t == "Comparable"),
        "Expected Constraint edge Box::T -> Comparable, got: {edges:?}",
    );
}

// ───────── AC-3: default → References edge ─────────

#[test]
fn function_type_parameter_default_emits_reference_edge() {
    let src = r"
function maybe<T = number>(x?: T): T | undefined {
    return x;
}
";
    let staging = build_staging(src, "default_fn.ts");
    let edges = collect_reference_edges(&staging);
    assert!(
        edges.iter().any(|(s, t)| s == "maybe::T" && t == "number"),
        "Expected Reference edge maybe::T -> number, got: {edges:?}",
    );
}

#[test]
fn class_type_parameter_default_emits_reference_edge() {
    let src = r"
class Container<T = string> {
    value!: T;
}
";
    let staging = build_staging(src, "default_class.ts");
    let edges = collect_reference_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Container::T" && t == "string"),
        "Expected Reference edge Container::T -> string, got: {edges:?}",
    );
}

// ───────── AC-4: mapped-type binders ─────────

#[test]
fn mapped_type_binder_emits_type_node_for_key_variable() {
    let src = r"
type M<T> = { [K in keyof T]: T[K] };
";
    let staging = build_staging(src, "mapped.ts");
    assert!(
        has_type_node(&staging, "M::T"),
        "Expected Type node M::T (the type-alias's own type-parameter), got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(
        has_type_node(&staging, "M::K"),
        "Expected Type node M::K (the mapped-type binder), got: {:?}",
        type_node_qnames(&staging)
    );
}

// ───────── AC-5: variadic tuple constraint ─────────

#[test]
fn variadic_tuple_constraint_emits_constraint_edge() {
    let src = r"
function applyAll<T extends unknown[]>(fns: T): void {
    void fns;
}
";
    let staging = build_staging(src, "variadic.ts");
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "applyAll::T" && t == "unknown[]"),
        "Expected Constraint edge applyAll::T -> unknown[], got: {edges:?}",
    );
}

// ───────── AC-2: namespace-prefixed container qualified names ─────────
//
// Codex review (U21 follow-up) flagged that the class / interface /
// type-alias / mapped-type-binder paths only used the bare local
// identifier when calling the U21 helper, dropping the enclosing
// `namespace N { ... }` from the qualified name. The fix walks up the
// AST collecting namespace ancestors so a class declared inside
// `namespace N` produces `N::Box::T`, matching AC-2's
// `<module>.<ClassName>.<TypeParam>` requirement.

#[test]
fn class_inside_namespace_includes_namespace_in_qname() {
    let src = r"
namespace N {
    class Box<T> {
        value: T;
    }
}
";
    let staging = build_staging(src, "ns_class.ts");
    assert!(
        has_type_node(&staging, "N::Box::T"),
        "Expected Type node `N::Box::T`, got: {:?}",
        type_node_qnames(&staging)
    );
}

#[test]
fn interface_inside_namespace_includes_namespace_in_qname() {
    let src = r"
namespace N {
    interface IFoo<T> {
        get(): T;
    }
}
";
    let staging = build_staging(src, "ns_iface.ts");
    assert!(
        has_type_node(&staging, "N::IFoo::T"),
        "Expected Type node `N::IFoo::T`, got: {:?}",
        type_node_qnames(&staging)
    );
}

#[test]
fn type_alias_inside_namespace_includes_namespace_in_qname() {
    let src = r"
namespace N {
    type Wrapper<T> = T[];
}
";
    let staging = build_staging(src, "ns_alias.ts");
    assert!(
        has_type_node(&staging, "N::Wrapper::T"),
        "Expected Type node `N::Wrapper::T`, got: {:?}",
        type_node_qnames(&staging)
    );
}

#[test]
fn mapped_type_binder_inside_namespace_includes_namespace_in_qname() {
    let src = r"
namespace N {
    type M<T> = { [K in keyof T]: T[K] };
}
";
    let staging = build_staging(src, "ns_mapped.ts");
    // Both the type-alias's own type-parameter and the mapped-type
    // binder must carry the namespace prefix.
    assert!(
        has_type_node(&staging, "N::M::T"),
        "Expected Type node `N::M::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(
        has_type_node(&staging, "N::M::K"),
        "Expected Type node `N::M::K`, got: {:?}",
        type_node_qnames(&staging)
    );
}

#[test]
fn class_inside_nested_namespaces_includes_full_path_in_qname() {
    let src = r"
namespace A {
    namespace B {
        class Box<T> {
            value: T;
        }
    }
}
";
    let staging = build_staging(src, "nested_ns_class.ts");
    assert!(
        has_type_node(&staging, "A::B::Box::T"),
        "Expected Type node `A::B::Box::T`, got: {:?}",
        type_node_qnames(&staging)
    );
}

// ───────── method_signature (interface methods) — Issue 2 ─────────
//
// Interface methods in TypeScript are `method_signature` AST nodes
// (distinct from `method_definition` on classes). The U21 walker arm
// did not match `method_signature`, so generic interface methods such
// as `wrap<T>(x: T): T` did not emit per-type-parameter Type nodes.
// The fix extends both the walker arm and the callable-context
// resolution path (`compute_callable_qname`) so that interface
// methods produce `<Interface>::<method>::<TypeParam>` qnames.

#[test]
fn interface_method_signature_emits_type_parameter_with_class_method_qname() {
    let src = r"
interface IBox {
    wrap<T>(x: T): T;
}
";
    let staging = build_staging(src, "iface_method_sig.ts");
    assert!(
        has_type_node(&staging, "IBox::wrap::T"),
        "Expected Type node `IBox::wrap::T` for interface method_signature, got: {:?}",
        type_node_qnames(&staging)
    );
}

#[test]
fn interface_method_signature_inside_namespace_includes_namespace_in_qname() {
    let src = r"
namespace N {
    interface IBox {
        wrap<T>(x: T): T;
    }
}
";
    let staging = build_staging(src, "ns_iface_method_sig.ts");
    assert!(
        has_type_node(&staging, "N::IBox::wrap::T"),
        "Expected Type node `N::IBox::wrap::T`, got: {:?}",
        type_node_qnames(&staging)
    );
}

// ───────── AC-6: conditional types ─────────

#[test]
fn conditional_type_emits_param_node_only() {
    let src = r"
type R<T> = T extends X ? Y : Z;
";
    let staging = build_staging(src, "conditional.ts");
    // AC-6: T still emitted as Type node.
    assert!(
        has_type_node(&staging, "R::T"),
        "Expected Type node R::T for conditional type-alias, got: {:?}",
        type_node_qnames(&staging)
    );
    // AC-6: the `T extends X` inside the *body* of a conditional
    // is NOT a generic-declaration-list constraint; it must not
    // emit a Constraint edge from R::T.
    let edges = collect_constraint_edges(&staging);
    assert!(
        !edges.iter().any(|(s, _)| s == "R::T"),
        "Expected no Constraint edge from R::T (conditional's `T extends X` is not a generic-declaration constraint), got: {edges:?}",
    );
}
