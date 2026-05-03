//! Test suite for per-type-parameter Type node emission in Java
//! generic methods, constructors, classes, and interfaces.
//!
//! Covers REQ:R0026 (`C2_GEN_TP_JAVA`, U17 in the
//! cross-language-field-emission DAG).
//!
//! Acceptance criteria:
//! - AC-1: All 4 walkers (method/constructor/class/interface declarations)
//!   iterate the `type_parameters` AST field.
//! - AC-2: Each `type_parameter` child emits a Type node via
//!   `helper.add_type(qualified_name, Some(span_from_node(name_node)))`.
//! - AC-3: Multiple bounds (`<T extends A & B>`) emit one
//!   `TypeOf{Constraint}` edge per bound type.
//! - AC-4: Generic methods, constructors, classes, interfaces all
//!   covered with per-shape fixtures.
//! - AC-5: Recursive bounds (`<T extends Comparable<T>>`) form a
//!   well-formed graph (no panic, edges still emitted).
//! - AC-6: Bounded wildcards (`<? extends T>`) remain reference-only
//!   (out of scope for declaration-site Type-parameter nodes).
//!
//! Note on separators: the Java plugin builds qualified names with `.`
//! (Java's native source separator), but `canonicalize_graph_qualified_name`
//! in `sqry-core` rewrites them to `::` for graph-internal storage. These
//! tests assert the post-canonicalisation `::` form because that is what
//! `StagingOp::AddNode` records and what every downstream query sees.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::collections::HashMap;
use std::path::PathBuf;
use tree_sitter::Tree;

// ───────────────────────── helpers ─────────────────────────

fn parse_java(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("set Java language");
    parser.parse(content, None).expect("parse Java")
}

fn build_staging(content: &str) -> StagingGraph {
    let tree = parse_java(content);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();
    builder
        .build_graph(
            &tree,
            content.as_bytes(),
            &PathBuf::from("Test.java"),
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

/// Build (`node_index` -> (`qualified_name`, kind)) lookup.
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

/// Collect all Constraint-context `TypeOf` edges as
/// (`source_qname`, `target_qname`).
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

/// Find the `AddNode` op for a given qualified name and assert its span is
/// non-zero (anchored on the parameter identifier).
///
/// The Java plugin uses `Span::from_bytes(start, end)`, which `add_node_internal`
/// stores into `start_column` / `end_column` (with `start_line` / `end_line` = 1).
/// Byte-range fields (`start_byte` / `end_byte`) are not populated by this path;
/// the column fields carry the byte offsets. Anchored = (`end_column` - `start_column`)
/// is small (≤64 bytes), matching the parameter identifier (e.g. `T`, `Foo`),
/// rather than the full declaration span.
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
                    entry.end_column > entry.start_column,
                    "Type-parameter node {qname} must have non-empty span (start_column={}, end_column={})",
                    entry.start_column,
                    entry.end_column,
                );
                let len = entry.end_column - entry.start_column;
                assert!(
                    len <= 64,
                    "Type-parameter node {qname} span should anchor on name identifier (≤64 bytes), got {len} bytes",
                );
            }
        }
    }
    assert!(
        found_any,
        "Expected at least one Type node with qname {qname}",
    );
}

// ───────────────────────── AC-1/AC-2/AC-4: classes ─────────────────────────

#[test]
fn class_with_single_type_parameter_emits_type_node() {
    let src = r"
package com.example;
public class Box<T> {
    private T value;
}
";
    let staging = build_staging(src);

    // Type node `com::example::Box::T` must exist.
    assert!(
        has_type_node(&staging, "com::example::Box::T"),
        "Expected Type node `com::example::Box::T`, got: {:?}",
        type_node_qnames(&staging),
    );
    assert_type_node_has_real_span(&staging, "com::example::Box::T");
}

#[test]
fn class_with_multiple_type_parameters_emits_each() {
    let src = r"
package com.example;
public class Pair<K, V> {
    private K key;
    private V value;
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "com::example::Pair::K"),
        "missing Pair.K, got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(has_type_node(&staging, "com::example::Pair::V"));
}

// ───────────────────────── AC-1/AC-2: interfaces ─────────────────────────

#[test]
fn interface_with_bounded_type_parameter_emits_type_and_constraint() {
    let src = r"
package com.example;
public interface Foo<T extends Bar> {
    T get();
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "com::example::Foo::T"),
        "missing Foo.T, got: {:?}",
        type_node_qnames(&staging)
    );
    let constraints = collect_constraint_edges(&staging);
    assert!(
        constraints
            .iter()
            .any(|(s, t)| s == "com::example::Foo::T" && (t == "Bar" || t.ends_with(".Bar"))),
        "Expected Constraint edge Foo.T -> Bar, got: {constraints:?}",
    );
}

// ───────────────────────── AC-1/AC-2: generic methods ─────────────────────────

#[test]
fn generic_method_emits_type_parameter_node() {
    let src = r"
package com.example;
public class Util {
    public static <T> java.util.List<T> singletonList(T t) {
        return null;
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "com::example::Util::singletonList::T"),
        "missing Util.singletonList.T, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "com::example::Util::singletonList::T");
}

#[test]
fn generic_method_with_multiple_type_parameters() {
    let src = r"
package com.example;
public class Util {
    public <K, V> java.util.Map<K, V> pair(K k, V v) {
        return null;
    }
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "com::example::Util::pair::K"),
        "missing pair.K, got: {qnames:?}",
    );
    assert!(qnames.iter().any(|q| q == "com::example::Util::pair::V"));
}

// ───────────────────────── AC-1/AC-2: generic constructors ─────────────────────────

#[test]
fn generic_constructor_emits_type_parameter_node() {
    // Constructor qualified name uses "<init>" sentinel
    // (see extract_constructor_context).
    let src = r"
package com.example;
public class Foo {
    public <T> Foo(T x) {}
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "com::example::Foo::<init>::T"),
        "Expected Type node `com::example::Foo::<init>::T`, got: {qnames:?}",
    );
}

// ───────────────────────── AC-3: multiple bounds ─────────────────────────

#[test]
fn type_parameter_with_intersection_bounds_emits_one_constraint_per_bound() {
    let src = r"
package com.example;
public class Box<T extends Number & Comparable<T> & java.io.Serializable> {}
";
    let staging = build_staging(src);
    assert!(has_type_node(&staging, "com::example::Box::T"));

    let constraints = collect_constraint_edges(&staging);
    let from_t: Vec<_> = constraints
        .iter()
        .filter(|(s, _)| s == "com::example::Box::T")
        .collect();
    // AC-3: one Constraint edge per bound type — Number, Comparable, Serializable.
    assert!(
        from_t.len() >= 3,
        "Expected ≥3 Constraint edges for `<T extends Number & Comparable<T> & Serializable>`, got {} edges from Box.T: {from_t:?}; all constraints: {constraints:?}",
        from_t.len(),
    );

    // Verify each bound is represented (base type names; generic args
    // are not part of the bound's identity here).
    let targets: Vec<&str> = from_t.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        targets
            .iter()
            .any(|t| *t == "Number" || t.ends_with("::Number")),
        "Expected Number bound, got: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|t| *t == "Comparable" || t.ends_with("::Comparable")),
        "Expected Comparable bound, got: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|t| *t == "Serializable" || t.ends_with("::Serializable")),
        "Expected Serializable bound, got: {targets:?}"
    );
}

// ───────────────────────── AC-5: recursive bounds ─────────────────────────

#[test]
fn recursive_bound_forms_well_formed_graph() {
    let src = r"
package com.example;
public class Util {
    public static <T extends Comparable<T>> T max(T a, T b) {
        return a;
    }
}
";
    let staging = build_staging(src);
    // Type-parameter node MUST exist
    assert!(
        has_type_node(&staging, "com::example::Util::max::T"),
        "missing Util.max.T (recursive-bound case), got: {:?}",
        type_node_qnames(&staging)
    );
    // Constraint edge MUST exist (and must not panic)
    let constraints = collect_constraint_edges(&staging);
    let from_t: Vec<_> = constraints
        .iter()
        .filter(|(s, _)| s == "com::example::Util::max::T")
        .collect();
    assert!(
        !from_t.is_empty(),
        "Expected at least one Constraint edge from Util.max.T -> Comparable, got: {constraints:?}"
    );
    assert!(
        from_t
            .iter()
            .any(|(_, t)| *t == "Comparable" || t.ends_with(".Comparable")),
        "Expected Comparable target in recursive bound, got: {from_t:?}"
    );
}

// ───────────────────────── AC-6: bounded wildcards stay reference-only ─────────────────────────

#[test]
fn bounded_wildcard_does_not_emit_declaration_node() {
    // `<? extends T>` is a use-site wildcard inside a parameter type,
    // NOT a declaration of a new type parameter on the method.
    // AC-6: we MUST NOT emit a Type node for the wildcard.
    let src = r"
package com.example;
public class Util {
    public static void consume(java.util.List<? extends Number> xs) {}
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    // No Type node for "consume.?" or "consume" type-parameter-anchored
    // qname should leak from the wildcard.
    assert!(
        !qnames
            .iter()
            .any(|q| q.starts_with("com::example::Util::consume::")),
        "Bounded wildcard must NOT emit a declaration-site Type node, got leaked qnames: {qnames:?}",
    );
}

// ───────────────────────── Mixed: nested class + nested generic method ─────────────────────────

#[test]
fn nested_generic_method_inside_generic_class_emits_both_levels() {
    let src = r"
package com.example;
public class Box<T> {
    public <R> Box<R> map(java.util.function.Function<T, R> mapper) {
        return null;
    }
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        qnames.iter().any(|q| q == "com::example::Box::T"),
        "missing class-level T, got: {qnames:?}",
    );
    assert!(
        qnames.iter().any(|q| q == "com::example::Box::map::R"),
        "missing method-level R, got: {qnames:?}",
    );
}

// ───────────────────────── Negative: non-generic shapes ─────────────────────────

#[test]
fn non_generic_class_emits_no_type_parameter_nodes() {
    let src = r"
package com.example;
public class Plain {
    public void run() {}
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    assert!(
        !qnames.iter().any(|q| q.starts_with("com::example::Plain::")
            && !q.starts_with("com::example::Plain::run")
            && q != "com::example::Plain"),
        "Non-generic class must not emit type-parameter nodes, got: {qnames:?}",
    );
}
