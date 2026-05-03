//! Test suite for per-type-parameter Type node emission in Kotlin
//! generic functions and classes.
//!
//! Covers REQ:R0027 (`C2_GEN_TP_KOTLIN`, U18 in the
//! cross-language-field-emission DAG).
//!
//! Acceptance criteria:
//! - AC-1: Function-declaration + class-declaration walkers iterate the
//!   `type_parameters` AST field.
//! - AC-2: Each `type_parameter` child emits a Type node via
//!   `helper.add_type(qualified_name, Some(span_from_node(name_node)))`.
//!   Top-level function: `<func>.<ParamName>` (canonicalised to
//!   `<func>::<ParamName>`); member function:
//!   `<Class>.<func>.<ParamName>`; class: `<Class>.<ParamName>`.
//! - AC-3: `where T : A, T : B` clauses emit one `TypeOf{Constraint}`
//!   edge per `type_constraint` declaration.
//! - AC-4: `inline fun <reified T>` still emits a Type node — the
//!   reified attribute itself is deferred per design §4.15.
//! - AC-5: Variance modifiers (`<in T>` / `<out T>`) still emit base
//!   Type node; variance attribute deferred.
//!
//! Note on separators: the Kotlin plugin builds qualified names with `.`
//! (Kotlin's native source separator), but `canonicalize_graph_qualified_name`
//! in `sqry-core` rewrites them to `::` for graph-internal storage. These
//! tests assert the post-canonicalisation `::` form because that is what
//! `StagingOp::AddNode` records and what every downstream query sees.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_kotlin::relations::KotlinGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Tree;

// ───────────────────────── helpers ─────────────────────────

fn parse_kotlin(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_sqry::language())
        .expect("set Kotlin language");
    parser.parse(content, None).expect("parse Kotlin")
}

fn build_staging(content: &str) -> StagingGraph {
    let tree = parse_kotlin(content);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();
    builder
        .build_graph(
            &tree,
            content.as_bytes(),
            Path::new("Test.kt"),
            &mut staging,
        )
        .expect("build_graph");
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

// ───────────────────────── AC-1/AC-2: top-level generic function ─────────────────────────

#[test]
fn top_level_generic_function_emits_type_parameter_node() {
    let src = r"
fun <T> id(x: T): T = x
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "id::T"),
        "Expected Type node `id::T`, got: {:?}",
        type_node_qnames(&staging),
    );
}

#[test]
fn top_level_generic_function_with_multiple_type_parameters() {
    let src = r"
fun <K, V> pair(k: K, v: V): Pair<K, V> = TODO()
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "pair::K"),
        "missing pair.K, got: {qnames:?}"
    );
    assert!(qnames.iter().any(|q| q == "pair::V"));
}

// ───────────────────────── AC-1/AC-2: member generic function ─────────────────────────

#[test]
fn member_generic_function_emits_type_parameter_node() {
    let src = r"
class Box {
    fun <T> wrap(x: T): T = x
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Box::wrap::T"),
        "Expected Type node `Box::wrap::T`, got: {:?}",
        type_node_qnames(&staging),
    );
}

// ───────────────────────── AC-1/AC-2: generic class ─────────────────────────

#[test]
fn generic_class_emits_type_parameter_node() {
    let src = r"
class Container<T>(val value: T)
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Container::T"),
        "Expected Type node `Container::T`, got: {:?}",
        type_node_qnames(&staging),
    );
}

#[test]
fn generic_class_with_multiple_type_parameters() {
    let src = r"
class Pair<K, V>(val key: K, val value: V)
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "Pair::K"),
        "missing Pair.K, got: {qnames:?}"
    );
    assert!(qnames.iter().any(|q| q == "Pair::V"));
}

// ───────────────────────── AC-3: bound + where-clause constraints ─────────────────────────

#[test]
fn type_parameter_with_inline_bound_emits_constraint_edge() {
    let src = r"
fun <T : Number> sum(x: T): T = x
";
    let staging = build_staging(src);
    assert!(has_type_node(&staging, "sum::T"));

    let constraints = collect_constraint_edges(&staging);
    assert!(
        constraints
            .iter()
            .any(|(s, t)| s == "sum::T" && (t == "Number" || t.ends_with("::Number"))),
        "Expected Constraint edge sum.T -> Number, got: {constraints:?}"
    );
}

#[test]
fn where_clause_emits_one_constraint_per_declaration() {
    // `where T : A, T : B` is represented in tree-sitter-kotlin as a
    // `type_constraints` node containing two `type_constraint` children.
    let src = r"
fun <T> foo(x: T): T where T : A, T : B = x
";
    let staging = build_staging(src);
    assert!(has_type_node(&staging, "foo::T"));

    let constraints = collect_constraint_edges(&staging);
    let from_t: Vec<_> = constraints.iter().filter(|(s, _)| s == "foo::T").collect();
    assert!(
        from_t.len() >= 2,
        "Expected ≥2 Constraint edges from foo.T, got {} edges: {from_t:?}; all constraints: {constraints:?}",
        from_t.len(),
    );

    let targets: Vec<&str> = from_t.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        targets.iter().any(|t| *t == "A" || t.ends_with("::A")),
        "Expected A bound, got: {targets:?}"
    );
    assert!(
        targets.iter().any(|t| *t == "B" || t.ends_with("::B")),
        "Expected B bound, got: {targets:?}"
    );
}

// ───────────────────────── AC-4: reified ─────────────────────────

#[test]
fn reified_type_parameter_emits_base_node() {
    // Reified attribute extension is deferred per §4.15; only the base
    // Type node must be emitted.
    let src = r"
inline fun <reified T> typeOf(): String = T::class.java.name
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "typeOf::T"),
        "Expected Type node `typeOf::T` for reified type-parameter, got: {:?}",
        type_node_qnames(&staging),
    );
}

// ───────────────────────── AC-5: variance ─────────────────────────

#[test]
fn variance_in_type_parameter_emits_base_node() {
    // Variance attribute extension is deferred per §4.15; only the base
    // Type node must be emitted.
    let src = r"
class Sink<in T>
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Sink::T"),
        "Expected Type node `Sink::T` for `<in T>`, got: {:?}",
        type_node_qnames(&staging),
    );
}

#[test]
fn variance_out_type_parameter_emits_base_node() {
    let src = r"
class Source<out T>
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Source::T"),
        "Expected Type node `Source::T` for `<out T>`, got: {:?}",
        type_node_qnames(&staging),
    );
}

// ───────────────────────── Mixed: nested generic method inside generic class ─────────────────────────

#[test]
fn nested_generic_function_inside_generic_class_emits_both_levels() {
    let src = r"
class Box<T>(val value: T) {
    fun <R> map(f: (T) -> R): Box<R> = TODO()
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "Box::T"),
        "missing class-level T, got: {qnames:?}",
    );
    assert!(
        qnames.iter().any(|q| q == "Box::map::R"),
        "missing method-level R, got: {qnames:?}",
    );
}

// ───────────────────────── Negative: non-generic shapes ─────────────────────────

#[test]
fn non_generic_function_emits_no_type_parameter_nodes() {
    let src = r"
fun plain(x: Int): Int = x
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        !qnames.iter().any(|q| q.starts_with("plain::")),
        "Non-generic function must not emit type-parameter nodes, got: {qnames:?}",
    );
}

#[test]
fn non_generic_class_emits_no_type_parameter_nodes() {
    let src = r"
class Plain {
    fun run() {}
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    // Plain.run is fine (a method node); but no Plain::<TypeParam>
    // qname should leak.
    assert!(
        !qnames
            .iter()
            .any(|q| q.starts_with("Plain::") && q != "Plain::run"),
        "Non-generic class must not emit type-parameter nodes, got: {qnames:?}",
    );
}
