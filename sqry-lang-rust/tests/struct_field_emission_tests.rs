//! U11 — `C2_OTHER_RUST` — Rust struct/enum-variant field emission as Property
//! with retained `::` separator + per-field visibility + tuple-struct collision
//! resolution.
//!
//! Spec refs: REQ:R0001, R0002 (`::` retained), R0003, R0004 (always false),
//! R0005, R0021 (enum variants), R0022 (tuple-struct collision), R0023.
//! Design ref: cross-language-field-emission/02_DESIGN §3.1.2 + §3.3 row 6 +
//! §4.6.
//!
//! Acceptance criteria covered (DAG U11):
//!   AC-1  Qualified name `{module_path}::{Struct}::{field_or_index}`;
//!         tuple synthetics use `::` index suffix.
//!   AC-2  All struct fields → Property; `is_static = false`; visibility per
//!         design §3.3 row 6 (`pub` → `"public"`, `pub(crate)` → `"crate"`,
//!         `pub(super)` → `"super"`, `pub(in path)` → `"in:<path>"`,
//!         absent → `"private"`).
//!   AC-3  Edge: `Some(TypeOfContext::Field)` + `Some(&field_name)` (BARE
//!         name; tuple uses "0"/"1" as bare).
//!   AC-4  Enum struct variants `enum Foo { Bar { x: i32 } }` emit Property
//!         `Foo::Bar::x`.
//!   AC-5  Enum tuple variants `enum Foo { Bar(i32) }` emit Property
//!         `Foo::Bar::0`.
//!   AC-6  Tuple-struct collision: `Point(i32, i32)` and `Vec(i32, i32)`
//!         produce distinct `NodeIds` `Point::0`, `Point::1`, `Vec::0`, `Vec::1`.
//!   AC-7  Generic types `struct Foo<T> { value: T }` emit Property
//!         `Foo::value` (symbolic struct, no instantiation).
//!   AC-8  Rust unit + property tests pass.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeEntry, NodeId, StagingGraph};
use sqry_lang_rust::relations::RustGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

fn parse_rust(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("Failed to load Rust grammar");
    parser.parse(source, None).expect("Failed to parse")
}

fn build_test_graph(source: &str, file_name: &str) -> StagingGraph {
    let tree = parse_rust(source);
    let file = Path::new(file_name);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    staging
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::InternString { local_id, value } => Some((local_id.index(), value.clone())),
            _ => None,
        })
        .collect()
}

fn find_nodes_by_qname<'a>(staging: &'a StagingGraph, qname: &str) -> Vec<(NodeId, &'a NodeEntry)> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let canonical = staging.resolve_node_canonical_name(entry)?;
                if canonical == qname {
                    return expected_id.map(|id| (id, entry));
                }
            }
            None
        })
        .collect()
}

fn all_canonical_names(staging: &StagingGraph) -> Vec<(String, NodeKind)> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                let canonical = staging.resolve_node_canonical_name(entry)?.to_string();
                return Some((canonical, entry.kind));
            }
            None
        })
        .collect()
}

fn must_find_node<'a>(staging: &'a StagingGraph, qname: &str, kind: NodeKind) -> &'a NodeEntry {
    let nodes: Vec<_> = find_nodes_by_qname(staging, qname)
        .into_iter()
        .filter(|(_, entry)| entry.kind == kind)
        .collect();
    assert!(
        !nodes.is_empty(),
        "expected node {qname} ({kind:?}) to exist; all nodes: {:?}",
        all_canonical_names(staging)
    );
    assert!(
        nodes.len() == 1,
        "expected exactly one node {qname} ({kind:?}); found {} (all: {:?})",
        nodes.len(),
        all_canonical_names(staging)
    );
    nodes[0].1
}

fn entry_visibility(staging: &StagingGraph, entry: &NodeEntry) -> Option<String> {
    let lookup = build_string_lookup(staging);
    entry
        .visibility
        .and_then(|id| lookup.get(&id.index()).cloned())
}

fn collect_typeof_edges(
    staging: &StagingGraph,
) -> Vec<(String, String, Option<TypeOfContext>, Option<String>)> {
    let canonical_by_id: HashMap<NodeId, String> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let id = (*expected_id)?;
                let canonical = staging.resolve_node_canonical_name(entry)?;
                return Some((id, canonical.to_string()));
            }
            None
        })
        .collect();

    let strings = build_string_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::TypeOf { context, name, .. },
                ..
            } = op
            {
                let from = canonical_by_id.get(source)?.clone();
                let to = canonical_by_id.get(target)?.clone();
                let name_str = name.and_then(|id| strings.get(&id.index()).cloned());
                return Some((from, to, *context, name_str));
            }
            None
        })
        .collect()
}

// ------------------------------------------------------------------
// AC-1 + AC-2: Qualified name + Property kind + is_static = false +
// visibility public for `pub` field
// ------------------------------------------------------------------

#[test]
fn ac1_named_struct_pub_field_emits_property_with_qualified_name() {
    let source = r"
pub struct Foo {
    pub x: i32,
}
";
    let staging = build_test_graph(source, "ac1_named.rs");
    let entry = must_find_node(&staging, "Foo::x", NodeKind::Property);
    assert!(!entry.is_static, "Rust struct fields are never static");
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("public"),
        "pub field should have visibility = public"
    );
}

#[test]
fn ac1_tuple_struct_pub_field_emits_property_with_index_suffix() {
    let source = r"
pub struct Bar(pub i32);
";
    let staging = build_test_graph(source, "ac1_tuple.rs");
    let entry = must_find_node(&staging, "Bar::0", NodeKind::Property);
    assert!(!entry.is_static);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("public"));
}

// ------------------------------------------------------------------
// AC-2: Visibility extraction per design §3.3 row 6
// ------------------------------------------------------------------

#[test]
fn ac2_visibility_pub_crate() {
    let source = r"
struct Wrap {
    pub(crate) x: i32,
}
";
    let staging = build_test_graph(source, "ac2_crate.rs");
    let entry = must_find_node(&staging, "Wrap::x", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("crate"));
}

#[test]
fn ac2_visibility_pub_super() {
    let source = r"
struct Wrap {
    pub(super) x: i32,
}
";
    let staging = build_test_graph(source, "ac2_super.rs");
    let entry = must_find_node(&staging, "Wrap::x", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("super"));
}

#[test]
fn ac2_visibility_pub_in_path() {
    let source = r"
struct Wrap {
    pub(in crate::foo) x: i32,
}
";
    let staging = build_test_graph(source, "ac2_in_path.rs");
    let entry = must_find_node(&staging, "Wrap::x", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("in:crate::foo"),
        "pub(in path) must preserve the path so downstream consumers can disambiguate"
    );
}

#[test]
fn ac2_visibility_private_when_absent() {
    let source = r"
struct Wrap {
    x: i32,
}
";
    let staging = build_test_graph(source, "ac2_priv.rs");
    let entry = must_find_node(&staging, "Wrap::x", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("private")
    );
}

// ------------------------------------------------------------------
// AC-3: TypeOf edge carries TypeOfContext::Field + bare name
// (tuple uses "0"/"1" as bare).
// ------------------------------------------------------------------

#[test]
fn ac3_named_field_typeof_edge_has_field_context_and_bare_name() {
    let source = r"
struct Service {
    repository: UserRepository,
}
";
    let staging = build_test_graph(source, "ac3_named.rs");
    let edges = collect_typeof_edges(&staging);

    let edge = edges
        .iter()
        .find(|(from, _, _, _)| from == "Service::repository")
        .unwrap_or_else(|| panic!("expected TypeOf edge from Service::repository, got: {edges:?}"));
    assert_eq!(
        edge.2,
        Some(TypeOfContext::Field),
        "TypeOf edge must carry Field context"
    );
    assert_eq!(
        edge.3.as_deref(),
        Some("repository"),
        "TypeOf edge name must be the BARE field name (not qualified)"
    );
}

#[test]
fn ac3_tuple_field_typeof_edge_uses_index_as_bare_name() {
    let source = r"
struct Bar(i32);
";
    let staging = build_test_graph(source, "ac3_tuple.rs");
    let edges = collect_typeof_edges(&staging);

    let edge = edges
        .iter()
        .find(|(from, _, _, _)| from == "Bar::0")
        .unwrap_or_else(|| panic!("expected TypeOf edge from Bar::0, got: {edges:?}"));
    assert_eq!(edge.2, Some(TypeOfContext::Field));
    assert_eq!(
        edge.3.as_deref(),
        Some("0"),
        "Tuple field TypeOf edge name must be the BARE index (not qualified)"
    );
}

// ------------------------------------------------------------------
// AC-4: Enum struct variant field emission
// ------------------------------------------------------------------

#[test]
fn ac4_enum_struct_variant_emits_property() {
    let source = r"
enum Foo {
    Bar { x: i32 },
}
";
    let staging = build_test_graph(source, "ac4_enum_struct.rs");
    let entry = must_find_node(&staging, "Foo::Bar::x", NodeKind::Property);
    assert!(!entry.is_static);
}

// ------------------------------------------------------------------
// AC-5: Enum tuple variant field emission
// ------------------------------------------------------------------

#[test]
fn ac5_enum_tuple_variant_emits_property() {
    let source = r"
enum Foo {
    Bar(i32),
}
";
    let staging = build_test_graph(source, "ac5_enum_tuple.rs");
    let entry = must_find_node(&staging, "Foo::Bar::0", NodeKind::Property);
    assert!(!entry.is_static);
}

// ------------------------------------------------------------------
// AC-6: Tuple-struct collision — distinct nodes for identical indices
// across different tuple structs.
// ------------------------------------------------------------------

#[test]
fn ac6_tuple_struct_collision_resolved_by_struct_qualifier() {
    let source = r"
struct Point(i32, i32);
struct Vec(i32, i32);
";
    let staging = build_test_graph(source, "ac6_collision.rs");

    // Each index must produce a distinct, qualified Property node.
    must_find_node(&staging, "Point::0", NodeKind::Property);
    must_find_node(&staging, "Point::1", NodeKind::Property);
    must_find_node(&staging, "Vec::0", NodeKind::Property);
    must_find_node(&staging, "Vec::1", NodeKind::Property);

    // And the four NodeIds must be pairwise distinct.
    let p0 = find_nodes_by_qname(&staging, "Point::0")[0].0;
    let p1 = find_nodes_by_qname(&staging, "Point::1")[0].0;
    let v0 = find_nodes_by_qname(&staging, "Vec::0")[0].0;
    let v1 = find_nodes_by_qname(&staging, "Vec::1")[0].0;
    let mut ids = vec![p0, p1, v0, v1];
    ids.sort_by_key(|id| (id.index(), id.generation()));
    ids.dedup();
    assert_eq!(
        ids.len(),
        4,
        "Point::0, Point::1, Vec::0, Vec::1 must be 4 distinct NodeIds"
    );
}

// ------------------------------------------------------------------
// AC-7: Generic types use the symbolic struct name (no instantiation).
// ------------------------------------------------------------------

#[test]
fn ac7_generic_struct_uses_symbolic_name() {
    let source = r"
struct Foo<T> {
    value: T,
}
";
    let staging = build_test_graph(source, "ac7_generic.rs");
    must_find_node(&staging, "Foo::value", NodeKind::Property);
}

// ------------------------------------------------------------------
// Sanity: unit struct emits no field nodes.
// ------------------------------------------------------------------

#[test]
fn unit_struct_emits_no_field_nodes() {
    let source = r"
struct Marker;
";
    let staging = build_test_graph(source, "unit.rs");
    let names = all_canonical_names(&staging);
    assert!(
        !names.iter().any(|(_, k)| *k == NodeKind::Property),
        "unit struct must not emit any Property nodes; got: {names:?}"
    );
}

// ------------------------------------------------------------------
// U11 follow-up — `union_item` field qualification.
//
// Codex APPROVE_WITH_CHANGES surfaced that
// `qualified_name_for_container` only handled `struct_item` / `enum_item` /
// `enum_variant`. Rust `union` fields fell through the defensive
// bare-name fallback and emitted bare `Property f1` / `f2` — exactly the
// bare-name collision class U11 was meant to remove.
//
// REQ refs: R0001 (Property kind), R0002 (`::` retained), R0003 (TypeOf
// Field context + bare name), R0004 (is_static = false), R0005
// (visibility), R0023 (qualified-name uniqueness).
// ------------------------------------------------------------------

#[test]
fn union_item_fields_emit_qualified_property_nodes_with_typeof_field_edges() {
    let source = r"
union MyUnion {
    f1: u32,
    f2: f32,
}
";
    let staging = build_test_graph(source, "union_basic.rs");

    // Property nodes carry the union-qualified name (no bare-name fallback).
    let f1_entry = must_find_node(&staging, "MyUnion::f1", NodeKind::Property);
    let f2_entry = must_find_node(&staging, "MyUnion::f2", NodeKind::Property);
    assert!(
        !f1_entry.is_static,
        "Rust union fields are never associated"
    );
    assert!(!f2_entry.is_static);

    // Defensive: no bare `f1` / `f2` Property nodes — the previous bug
    // produced these via the bare-name fallback.
    let names = all_canonical_names(&staging);
    for bare in ["f1", "f2"] {
        assert!(
            !names
                .iter()
                .any(|(n, k)| *k == NodeKind::Property && n == bare),
            "union field must not emit a bare-name Property node `{bare}`; got: {names:?}"
        );
    }

    // Edges: TypeOf{Field, name=BARE} from each qualified field Property.
    let edges = collect_typeof_edges(&staging);
    let edge_f1 = edges
        .iter()
        .find(|(from, _, _, _)| from == "MyUnion::f1")
        .unwrap_or_else(|| panic!("expected TypeOf edge from MyUnion::f1, got: {edges:?}"));
    assert_eq!(edge_f1.2, Some(TypeOfContext::Field));
    assert_eq!(
        edge_f1.3.as_deref(),
        Some("f1"),
        "TypeOf edge name must be the BARE field name (not qualified)"
    );

    let edge_f2 = edges
        .iter()
        .find(|(from, _, _, _)| from == "MyUnion::f2")
        .unwrap_or_else(|| panic!("expected TypeOf edge from MyUnion::f2, got: {edges:?}"));
    assert_eq!(edge_f2.2, Some(TypeOfContext::Field));
    assert_eq!(edge_f2.3.as_deref(), Some("f2"));
}

#[test]
fn pub_union_pub_field_emits_qualified_property_with_public_visibility() {
    let source = r"
pub union PubU {
    pub a: i32,
}
";
    let staging = build_test_graph(source, "union_pub.rs");

    let entry = must_find_node(&staging, "PubU::a", NodeKind::Property);
    assert!(!entry.is_static);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("public"),
        "pub union field must carry visibility = public"
    );
}
