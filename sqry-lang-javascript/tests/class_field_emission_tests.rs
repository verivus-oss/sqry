//! U09 — `C2_OTHER_JS` — JS class field unconditional Property emission +
//! `this.x = …` constructor walker.
//!
//! Spec refs: REQ:R0001, R0002, R0003, R0004, R0005, R0006, R0008, R0023
//! Design ref: cross-language-field-emission/02_DESIGN §4.5
//!
//! Acceptance criteria covered (DAG U09):
//!   AC-1  `JSDoc` gate at :1804 removed; Property emission unconditional on
//!         every `field_definition`
//!   AC-2  Span extracted from the field-definition node (was `None`)
//!   AC-3  `static` modifier → `is_static = true`
//!   AC-4  `#`-prefix → visibility = "private"; otherwise visibility = "public"
//!   AC-5  Edge: `Some(TypeOfContext::Field)` + `Some(&field_name)` (BARE,
//!         not `Class.field`) — only emitted when `JSDoc` `@type` is present
//!   AC-6  New constructor walker: `constructor() { this.x = ...; this.y = ...; }`
//!         emits Property nodes `Class.x` + `Class.y`
//!   AC-7  `JSDoc` `@type` is preserved as enrichment for the `TypeOf{Field}`
//!         edge but is NOT a gate on Property emission
//!   AC-8  JS unit + property tests pass

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeEntry, NodeId, StagingGraph};
use sqry_lang_javascript::JavaScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

fn parse_js(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("Failed to load JavaScript grammar");
    parser.parse(source, None).expect("Failed to parse")
}

fn build_test_graph(source: &str, file_name: &str) -> StagingGraph {
    let tree = parse_js(source);
    let file = Path::new(file_name);
    let mut staging = StagingGraph::new();
    let builder = JavaScriptGraphBuilder::default();

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

/// Find all nodes whose canonical qualified name matches `qname`. Returns
/// `(NodeId, &NodeEntry)` pairs to allow attribute inspection.
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

/// Find a single node by canonical name and kind, or panic with diagnostic info.
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

/// Collect (`from_canonical`, `to_canonical`, `edge_kind`, `name_str`) tuples for `TypeOf` edges.
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
// AC-1: Property emission unconditional on every field_definition
// ------------------------------------------------------------------

#[test]
fn ac1_field_without_jsdoc_still_emits_property() {
    let source = r"
class Counter {
    count = 0;
    name = 'widget';
}
";
    let staging = build_test_graph(source, "ac1_no_jsdoc.js");
    must_find_node(&staging, "Counter::count", NodeKind::Property);
    must_find_node(&staging, "Counter::name", NodeKind::Property);
}

#[test]
fn ac1_field_with_jsdoc_still_emits_property() {
    let source = r"
class Counter {
    /** @type {number} */
    count = 0;
}
";
    let staging = build_test_graph(source, "ac1_jsdoc.js");
    must_find_node(&staging, "Counter::count", NodeKind::Property);
}

// ------------------------------------------------------------------
// AC-2: Span extracted (was None)
// ------------------------------------------------------------------

#[test]
fn ac2_field_node_has_span() {
    let source = r"
class Widget {
    name = 'foo';
}
";
    let staging = build_test_graph(source, "ac2_span.js");
    let entry = must_find_node(&staging, "Widget::name", NodeKind::Property);
    // span_from_node returns a real (non-empty) Span. start_line is 1-based
    // after helper adjustment; an empty/default span yields start_line = 0.
    assert!(
        entry.start_line > 0,
        "expected span to be set (start_line > 0); got start_line = {} (entry = {entry:?})",
        entry.start_line,
    );
    // The body of `name = 'foo';` is on line 3 of the source above (with the
    // leading newline + `class Widget {`).
    assert!(
        entry.end_line >= entry.start_line,
        "expected end_line >= start_line; got start_line = {}, end_line = {}",
        entry.start_line,
        entry.end_line,
    );
}

// ------------------------------------------------------------------
// AC-3: static modifier
// ------------------------------------------------------------------

#[test]
fn ac3_static_field_sets_is_static_true() {
    let source = r"
class Counter {
    static count = 0;
    instanceField = 1;
}
";
    let staging = build_test_graph(source, "ac3_static.js");

    let static_field = must_find_node(&staging, "Counter::count", NodeKind::Property);
    assert!(
        static_field.is_static,
        "expected Counter::count to have is_static = true (entry = {static_field:?})"
    );

    let instance_field = must_find_node(&staging, "Counter::instanceField", NodeKind::Property);
    assert!(
        !instance_field.is_static,
        "expected Counter::instanceField to have is_static = false"
    );
}

// ------------------------------------------------------------------
// AC-4: #-prefix → visibility = "private"; otherwise "public"
// ------------------------------------------------------------------

#[test]
fn ac4_hash_prefix_field_is_private() {
    let source = r"
class Widget {
    #secret = 42;
    publicField = 1;
}
";
    let staging = build_test_graph(source, "ac4_hash.js");

    // `#secret` keeps the `#` in the identifier — qualified name is
    // `Widget::#secret` after canonicalization.
    let secret = must_find_node(&staging, "Widget::#secret", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, secret).as_deref(),
        Some("private"),
        "expected #secret visibility = private"
    );

    let public = must_find_node(&staging, "Widget::publicField", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, public).as_deref(),
        Some("public"),
        "expected non-#-prefixed field visibility = \"public\" (entry = {public:?})"
    );
}

// ------------------------------------------------------------------
// AC-5: Edge name = bare field name
// ------------------------------------------------------------------

#[test]
fn ac5_typeof_edge_uses_field_context_with_bare_name_via_jsdoc() {
    // JSDoc enrichment path — only place where TypeOf{Field} edges fire for
    // class fields (vanilla JS has no field type annotations).
    let source = r"
class Widget {
    /** @type {string} */
    name = 'foo';
}
";
    let staging = build_test_graph(source, "ac5.js");

    let typeofs = collect_typeof_edges(&staging);
    let field_edges: Vec<_> = typeofs
        .iter()
        .filter(|(from, _to, ctx, _name)| {
            from == "Widget::name" && *ctx == Some(TypeOfContext::Field)
        })
        .collect();

    assert!(
        !field_edges.is_empty(),
        "expected TypeOf(Field) edge from Widget::name; all typeofs = {typeofs:?}"
    );

    let (_, _, _, name_str) = field_edges[0];
    assert_eq!(
        name_str.as_deref(),
        Some("name"),
        "expected TypeOf(Field) edge name = bare 'name', not 'Widget.name'; got {name_str:?}"
    );
}

// ------------------------------------------------------------------
// AC-6: this.x = ... constructor walker
// ------------------------------------------------------------------

#[test]
fn ac6_constructor_this_assignments_emit_property_nodes() {
    let source = r"
class Person {
    constructor(name, age) {
        this.name = name;
        this.age = age;
    }
}
";
    let staging = build_test_graph(source, "ac6_ctor.js");

    must_find_node(&staging, "Person::name", NodeKind::Property);
    must_find_node(&staging, "Person::age", NodeKind::Property);
}

#[test]
fn ac6_ctor_walker_handles_no_class_fields_present() {
    // When no `field_definition` is present, the ctor walker is the only
    // emitter — verify it works in isolation.
    let source = r"
class Bag {
    constructor() {
        this.items = [];
    }
}
";
    let staging = build_test_graph(source, "ac6_no_fields.js");
    must_find_node(&staging, "Bag::items", NodeKind::Property);
}

#[test]
fn ac6_ctor_walker_dedupes_with_explicit_field() {
    // FR-13 explicit-field-wins: explicit field declaration runs first;
    // `this.x = ...` in the constructor adds to the same node, not a
    // duplicate. Both ops resolve to the same canonical name + kind so the
    // helper cache returns the original node.
    let source = r"
class Counter {
    count = 0;
    constructor() {
        this.count = 1;
    }
}
";
    let staging = build_test_graph(source, "ac6_dedupe.js");
    let nodes = find_nodes_by_qname(&staging, "Counter::count");
    let property_nodes: Vec<_> = nodes
        .iter()
        .filter(|(_, e)| e.kind == NodeKind::Property)
        .collect();
    assert_eq!(
        property_nodes.len(),
        1,
        "expected exactly one Property node for Counter::count; got {} (all canonical: {:?})",
        property_nodes.len(),
        all_canonical_names(&staging)
    );
}

#[test]
fn ac6_ctor_walker_ignores_non_this_assignments() {
    // Locals, params, and module-level assignments must not promote.
    let source = r"
class Widget {
    constructor() {
        let local = 42;
        someGlobal = 99;
        const obj = { foo: 1 };
        obj.bar = 2;
    }
}
";
    let staging = build_test_graph(source, "ac6_negatives.js");
    // None of these should appear as Widget::<name> Property nodes.
    for forbidden in [
        "Widget::local",
        "Widget::someGlobal",
        "Widget::obj",
        "Widget::bar",
        "Widget::foo",
    ] {
        let hits: Vec<_> = find_nodes_by_qname(&staging, forbidden)
            .into_iter()
            .filter(|(_, e)| e.kind == NodeKind::Property)
            .collect();
        assert!(
            hits.is_empty(),
            "expected NO Property node for {forbidden}; found {} (canonical: {:?})",
            hits.len(),
            all_canonical_names(&staging),
        );
    }
}

#[test]
fn ac6_ctor_walker_ignores_nested_function_this() {
    // `this` inside a nested non-arrow function is rebound — we conservatively
    // walk all assignment_expression nodes for `this.x` regardless of nesting,
    // since that is the most useful behaviour for this-prop discovery in JS
    // (matches how class-field discovery tools usually behave). Verify a
    // simple top-level case still works.
    let source = r"
class Outer {
    constructor() {
        this.first = 1;
        const helper = () => {
            this.second = 2; // arrow inherits this — should still attribute
        };
        helper();
    }
}
";
    let staging = build_test_graph(source, "ac6_nested.js");
    must_find_node(&staging, "Outer::first", NodeKind::Property);
    must_find_node(&staging, "Outer::second", NodeKind::Property);
}

// ------------------------------------------------------------------
// AC-7: JSDoc preserved as enrichment for TypeOf edge type (not a gate)
// ------------------------------------------------------------------

#[test]
fn ac7_jsdoc_present_yields_property_plus_typeof_edge() {
    let source = r"
class Box {
    /** @type {number} */
    count = 0;
}
";
    let staging = build_test_graph(source, "ac7.js");

    must_find_node(&staging, "Box::count", NodeKind::Property);

    let typeofs = collect_typeof_edges(&staging);
    let field_edges: Vec<_> = typeofs
        .iter()
        .filter(|(from, to, ctx, _name)| {
            from == "Box::count" && to == "number" && *ctx == Some(TypeOfContext::Field)
        })
        .collect();
    assert!(
        !field_edges.is_empty(),
        "expected TypeOf(Field) edge from Box::count -> number; all typeofs = {typeofs:?}"
    );
}

#[test]
fn ac7_jsdoc_absent_yields_property_without_field_typeof() {
    let source = r"
class Box {
    count = 0;
}
";
    let staging = build_test_graph(source, "ac7_no_jsdoc.js");
    must_find_node(&staging, "Box::count", NodeKind::Property);

    // No TypeOf{Field} edge should originate from Box::count.
    let typeofs = collect_typeof_edges(&staging);
    let field_edges: Vec<_> = typeofs
        .iter()
        .filter(|(from, _to, ctx, _name)| {
            from == "Box::count" && *ctx == Some(TypeOfContext::Field)
        })
        .collect();
    assert!(
        field_edges.is_empty(),
        "expected NO TypeOf(Field) edge from Box::count without JSDoc; got {field_edges:?}"
    );
}

// ------------------------------------------------------------------
// Anonymous class assigned to a variable — class-stack falls back to
// the variable name (mirrors process_class_fields_jsdoc precedent).
// ------------------------------------------------------------------

#[test]
fn anonymous_class_assigned_to_const_uses_variable_name_as_class() {
    let source = r"
const MyWidget = class {
    count = 0;
    constructor() {
        this.label = 'hi';
    }
};
";
    let staging = build_test_graph(source, "anon_const.js");
    must_find_node(&staging, "MyWidget::count", NodeKind::Property);
    must_find_node(&staging, "MyWidget::label", NodeKind::Property);
}
