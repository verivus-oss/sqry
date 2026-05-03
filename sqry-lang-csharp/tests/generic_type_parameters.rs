//! Test suite for per-type-parameter Type node emission in C# generic
//! methods, classes, and interfaces.
//!
//! Covers REQ:R0028 (`C2_GEN_TP_CSHARP`, U19 in the
//! cross-language-field-emission DAG).
//!
//! Acceptance criteria (DAG U19):
//! - AC-1: All 3 walkers (method/class/interface declarations) iterate
//!   the `type_parameter_list` AST child.
//! - AC-2: Each `type_parameter` child emits a Type node with qualified
//!   name `<namespace>.<ClassName>.<MethodName>.<ParamName>` (or class
//!   / interface form). Span anchored on the parameter identifier.
//! - AC-3: `where T : <constraint>` clauses emit one
//!   `TypeOf{Constraint}` edge per constraint entry (intersection bounds
//!   `where T : A, B, C` produce three edges).
//! - AC-4: Synthetic constraints `new()`, `class`, `struct`, `unmanaged`,
//!   `notnull` emit Constraint edges to interned synthetic Type nodes
//!   named exactly `new()`, `class`, `struct`, `unmanaged`, `notnull`.
//! - AC-5: Variance modifiers (`in T`, `out U`) still emit the base
//!   parameter Type node; the variance attribute itself is deferred.
//!
//! Note on separators: the C# plugin builds qualified names with `.`
//! (C#'s native source separator), but `canonicalize_graph_qualified_name`
//! in `sqry-core` rewrites them to `::` for graph-internal storage. These
//! tests assert the post-canonicalisation `::` form because that is what
//! `StagingOp::AddNode` records.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_csharp::relations::CSharpGraphBuilder;
use std::collections::HashMap;
use std::path::PathBuf;
use tree_sitter::Tree;

// ───────────────────────── helpers ─────────────────────────

fn parse_csharp(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .expect("set C# language");
    parser.parse(content, None).expect("parse C#")
}

fn build_staging(content: &str) -> StagingGraph {
    let tree = parse_csharp(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    builder
        .build_graph(
            &tree,
            content.as_bytes(),
            &PathBuf::from("Test.cs"),
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

/// Assert that the Type node for `qname` carries a span anchored on the
/// parameter identifier (small byte length, not the full declaration).
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
        "Expected at least one Type node with qname {qname}"
    );
}

// ───────────────────────── AC-1/AC-2/AC-4: classes ─────────────────────────

#[test]
fn class_with_single_type_parameter_emits_type_node() {
    let src = r"
namespace Acme {
    public class Box<T> {
        public T Value;
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::Box::T"),
        "Expected Type node `Acme::Box::T`, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Acme::Box::T");
}

#[test]
fn class_with_multiple_type_parameters_emits_each() {
    let src = r"
namespace Acme {
    public class Pair<K, V> {
        public K Key;
        public V Value;
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::Pair::K"),
        "missing Pair.K, got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(has_type_node(&staging, "Acme::Pair::V"));
}

// ───────────────────────── AC-1/AC-2: interfaces ─────────────────────────

#[test]
fn interface_with_single_type_parameter_emits_type_node() {
    let src = r"
namespace Acme {
    public interface IFoo<T> {
        T Get();
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::IFoo::T"),
        "missing IFoo.T, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Acme::IFoo::T");
}

// ───────────────────────── AC-1/AC-3/AC-4: generic interface methods ─────────────────────────

/// Regression: the `interface_declaration` arm of `walk_tree_for_edges`
/// previously returned early after `process_interface_member_exports`,
/// so `method_declaration` children inside an interface body never
/// reached the method-walker (and never had their type-parameter Type
/// nodes / Constraint edges emitted). This test pins the fix: a
/// generic interface method must produce the param Type node and the
/// `new()` synthetic Constraint edge.
#[test]
fn generic_interface_method_emits_type_parameter_and_constraint() {
    let src = r"
namespace Acme {
    public interface IFoo {
        T M<T>() where T : new();
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::IFoo::M::T"),
        "missing IFoo.M.T, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Acme::IFoo::M::T");

    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::IFoo::M::T" && t == "new()"),
        "Expected Constraint edge Acme::IFoo::M::T -> new(), got: {edges:?}",
    );
}

// ───────────────────────── AC-1/AC-2: generic methods ─────────────────────────

#[test]
fn generic_method_emits_type_parameter_node() {
    let src = r"
namespace Acme {
    public class Util {
        public static T Identity<T>(T x) { return x; }
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::Util::Identity::T"),
        "missing Util.Identity.T, got: {:?}",
        type_node_qnames(&staging)
    );
    assert_type_node_has_real_span(&staging, "Acme::Util::Identity::T");
}

#[test]
fn generic_method_with_multiple_type_parameters() {
    let src = r"
namespace Acme {
    public class Util {
        public static V Convert<K, V>(K k) { return default(V); }
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::Util::Convert::K"),
        "missing Util.Convert.K, got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(has_type_node(&staging, "Acme::Util::Convert::V"));
}

// ───────────────────────── AC-3: where-clause type bound ─────────────────────────

#[test]
fn where_clause_emits_constraint_edge_to_named_type() {
    let src = r"
namespace Acme {
    public class Box<T> where T : IComparable {
        public T Value;
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T"
                && (t == "IComparable" || t.ends_with("::IComparable"))),
        "Expected Constraint edge Acme::Box::T -> IComparable, got: {edges:?}",
    );
}

#[test]
fn where_clause_intersection_emits_one_edge_per_bound() {
    let src = r"
namespace Acme {
    public class Box<T> where T : IComparable, IEnumerable {
        public T Value;
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    let from_t: Vec<&(String, String)> =
        edges.iter().filter(|(s, _)| s == "Acme::Box::T").collect();
    assert!(
        from_t
            .iter()
            .any(|(_, t)| t == "IComparable" || t.ends_with("::IComparable")),
        "Expected Constraint edge Acme::Box::T -> IComparable, got: {edges:?}",
    );
    assert!(
        from_t
            .iter()
            .any(|(_, t)| t == "IEnumerable" || t.ends_with("::IEnumerable")),
        "Expected Constraint edge Acme::Box::T -> IEnumerable, got: {edges:?}",
    );
}

// ───────────────────────── AC-4: synthetic constraints ─────────────────────────

#[test]
fn where_clause_new_constraint_emits_synthetic_edge() {
    let src = r"
namespace Acme {
    public class Box<T> where T : new() {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T" && t == "new()"),
        "Expected Constraint edge Acme::Box::T -> new(), got: {edges:?}",
    );
}

#[test]
fn where_clause_class_constraint_emits_synthetic_edge() {
    let src = r"
namespace Acme {
    public class Box<T> where T : class {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T" && t == "class"),
        "Expected Constraint edge Acme::Box::T -> class, got: {edges:?}",
    );
}

#[test]
fn where_clause_struct_constraint_emits_synthetic_edge() {
    let src = r"
namespace Acme {
    public class Box<T> where T : struct {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T" && t == "struct"),
        "Expected Constraint edge Acme::Box::T -> struct, got: {edges:?}",
    );
}

#[test]
fn where_clause_unmanaged_constraint_emits_synthetic_edge() {
    let src = r"
namespace Acme {
    public class Box<T> where T : unmanaged {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T" && t == "unmanaged"),
        "Expected Constraint edge Acme::Box::T -> unmanaged, got: {edges:?}",
    );
}

#[test]
fn where_clause_notnull_constraint_emits_synthetic_edge() {
    let src = r"
namespace Acme {
    public class Box<T> where T : notnull {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Box::T" && t == "notnull"),
        "Expected Constraint edge Acme::Box::T -> notnull, got: {edges:?}",
    );
}

#[test]
fn where_clause_mixed_named_and_synthetic_constraints() {
    let src = r"
namespace Acme {
    public class Box<T> where T : IComparable, new() {
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    let from_t: Vec<&(String, String)> =
        edges.iter().filter(|(s, _)| s == "Acme::Box::T").collect();
    assert!(
        from_t
            .iter()
            .any(|(_, t)| t == "IComparable" || t.ends_with("::IComparable")),
        "Expected Constraint edge Box.T -> IComparable, got: {edges:?}",
    );
    assert!(
        from_t.iter().any(|(_, t)| t == "new()"),
        "Expected Constraint edge Box.T -> new(), got: {edges:?}",
    );
}

#[test]
fn generic_method_with_constraint_emits_edges() {
    let src = r"
namespace Acme {
    public class Util {
        public static T Identity<T>(T x) where T : new() { return x; }
    }
}
";
    let staging = build_staging(src);
    let edges = collect_constraint_edges(&staging);
    assert!(
        edges
            .iter()
            .any(|(s, t)| s == "Acme::Util::Identity::T" && t == "new()"),
        "Expected Constraint edge Util.Identity.T -> new(), got: {edges:?}",
    );
}

// ───────────────────────── AC-5: variance modifiers ─────────────────────────

#[test]
fn interface_with_variance_modifiers_still_emits_base_type_nodes() {
    let src = r"
namespace Acme {
    public interface IFoo<in T, out U> {
    }
}
";
    let staging = build_staging(src);
    assert!(
        has_type_node(&staging, "Acme::IFoo::T"),
        "missing IFoo.T, got: {:?}",
        type_node_qnames(&staging)
    );
    assert!(
        has_type_node(&staging, "Acme::IFoo::U"),
        "missing IFoo.U, got: {:?}",
        type_node_qnames(&staging)
    );
}

// ───────────────────────── No-op: non-generic declarations ─────────────────────────

#[test]
fn non_generic_class_emits_no_type_parameter_nodes() {
    let src = r"
namespace Acme {
    public class Plain {
        public int Value;
    }
}
";
    let staging = build_staging(src);
    let qnames = type_node_qnames(&staging);
    // Bare existing types from field/method TypeOf edges may exist (e.g.
    // "int") — but no `Acme::Plain::*` Type nodes from type-parameter
    // emission should appear.
    assert!(
        !qnames.iter().any(|q| q.starts_with("Acme::Plain::")),
        "Expected zero type-parameter Type nodes for non-generic Plain, got: {qnames:?}",
    );
}
