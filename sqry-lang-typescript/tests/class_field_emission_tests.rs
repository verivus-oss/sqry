//! U08 — `C2_OTHER_TS` — TS class field + ctor-promotion + 6 corner cases
//!
//! Spec refs: REQ:R0001..R0005, R0007, R0013, R0023, R0024
//! Design ref: cross-language-field-emission/02_DESIGN §4.5 + FR-13 + FR-24
//!
//! Acceptance criteria covered (DAG U08):
//!   AC-1  Qualified name `Class.field` from class-stack tracking
//!   AC-2  `readonly` / `const enum member` → Constant; otherwise Property
//!   AC-3  `static` modifier → `is_static` = true
//!   AC-4  Visibility from `accessibility_modifier`; `#`-prefix → "private"
//!   AC-5  Edge: `Some(TypeOfContext::Field)` + `Some(&field_name)` (bare)
//!   AC-6  Constructor parameter promotion: `constructor(public name: T)` → Property `Class.name`
//!   AC-7  Collision precedence (FR-13): explicit field wins; promoted parameter
//!         dedupes via `helper.get_node` and only fills `None` attributes
//!   AC-8  6 corner cases (FR-24): decorator, intersection, optional, defaulted,
//!         readonly-promoted (Constant), rest (rejected: no node, no panic)
//!   AC-9  Negative: bare `find_nodes_by_name("count")` is ambiguous when two
//!         classes both define `count` (verified through `staging.nodes()`).

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeEntry, NodeId, StagingGraph};
use sqry_lang_typescript::TypeScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

fn parse_typescript(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("Failed to load TypeScript grammar");
    parser.parse(source, None).expect("Failed to parse")
}

fn build_test_graph(source: &str, file_name: &str) -> StagingGraph {
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
        "expected exactly one node {qname} ({kind:?}); found {}",
        nodes.len()
    );
    nodes[0].1
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

fn entry_visibility(staging: &StagingGraph, entry: &NodeEntry) -> Option<String> {
    let lookup = build_string_lookup(staging);
    entry
        .visibility
        .and_then(|id| lookup.get(&id.index()).cloned())
}

/// Collect (`from_canonical`, `to_canonical`, `edge_kind`) triples for `TypeOf` edges.
fn collect_typeof_edges(staging: &StagingGraph) -> Vec<(String, String, Option<TypeOfContext>)> {
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

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::TypeOf { context, .. },
                ..
            } = op
            {
                let from = canonical_by_id.get(source)?.clone();
                let to = canonical_by_id.get(target)?.clone();
                return Some((from, to, *context));
            }
            None
        })
        .collect()
}

// ------------------------------------------------------------------
// AC-1, AC-3, AC-4: qualified name + static + visibility for Property
// ------------------------------------------------------------------

#[test]
fn ac1_field_qualified_name_uses_class_stack() {
    let source = r"
class Widget {
    public name: string;
}
";
    let staging = build_test_graph(source, "ac1.ts");
    // Canonical form for TS uses `::` separator after canonicalization
    let entry = must_find_node(&staging, "Widget::name", NodeKind::Property);
    assert_eq!(entry.kind, NodeKind::Property);
}

#[test]
fn ac3_static_modifier_sets_is_static_true() {
    let source = r"
class Counter {
    static count: number;
    instanceField: number;
}
";
    let staging = build_test_graph(source, "ac3.ts");

    let static_field = must_find_node(&staging, "Counter::count", NodeKind::Property);
    assert!(
        static_field.is_static,
        "expected Counter::count to have is_static = true"
    );

    let instance_field = must_find_node(&staging, "Counter::instanceField", NodeKind::Property);
    assert!(
        !instance_field.is_static,
        "expected Counter::instanceField to have is_static = false"
    );
}

#[test]
fn ac4_visibility_extracted_from_accessibility_modifier() {
    let source = r"
class Widget {
    public publicField: string;
    private privateField: string;
    protected protectedField: string;
    bareField: string;
}
";
    let staging = build_test_graph(source, "ac4.ts");

    let pub_entry = must_find_node(&staging, "Widget::publicField", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, pub_entry).as_deref(),
        Some("public"),
        "expected publicField visibility=public"
    );

    let priv_entry = must_find_node(&staging, "Widget::privateField", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, priv_entry).as_deref(),
        Some("private"),
        "expected privateField visibility=private"
    );

    let prot_entry = must_find_node(&staging, "Widget::protectedField", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, prot_entry).as_deref(),
        Some("protected"),
        "expected protectedField visibility=protected"
    );

    let bare_entry = must_find_node(&staging, "Widget::bareField", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, bare_entry),
        None,
        "expected bareField visibility=None"
    );
}

#[test]
fn ac4_hash_prefix_forces_private_visibility() {
    let source = r"
class Widget {
    #secret: string;
}
";
    let staging = build_test_graph(source, "ac4_hash.ts");

    // `#secret` is part of the identifier — qualified name preserves the `#`
    let entry = must_find_node(&staging, "Widget::#secret", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("private"),
        "expected #secret to be visibility=private"
    );
}

// ------------------------------------------------------------------
// AC-2: readonly / const enum member → Constant
// ------------------------------------------------------------------

#[test]
fn ac2_readonly_field_emits_constant_kind() {
    let source = r"
class Config {
    readonly id: number;
    static readonly VERSION: string;
    name: string;
}
";
    let staging = build_test_graph(source, "ac2_readonly.ts");

    must_find_node(&staging, "Config::id", NodeKind::Constant);
    let version = must_find_node(&staging, "Config::VERSION", NodeKind::Constant);
    assert!(
        version.is_static,
        "expected static readonly VERSION to be Constant + is_static"
    );
    must_find_node(&staging, "Config::name", NodeKind::Property);
}

#[test]
fn ac2_const_enum_members_emit_constant() {
    let source = r"
const enum Colors {
    Red = 1,
    Green = 2,
    Blue = 4,
}
enum Plain {
    A = 1,
    B = 2,
}
";
    let staging = build_test_graph(source, "ac2_constenum.ts");

    must_find_node(&staging, "Colors::Red", NodeKind::Constant);
    must_find_node(&staging, "Colors::Green", NodeKind::Constant);
    must_find_node(&staging, "Colors::Blue", NodeKind::Constant);
    // Non-const enum members emit Property per AC-2 ("otherwise Property")
    must_find_node(&staging, "Plain::A", NodeKind::Property);
    must_find_node(&staging, "Plain::B", NodeKind::Property);
}

// ------------------------------------------------------------------
// AC-5: TypeOf edge with Field context + bare name
// ------------------------------------------------------------------

#[test]
fn ac5_typeof_edge_uses_field_context_with_bare_name() {
    let source = r"
class Widget {
    name: string;
}
";
    let staging = build_test_graph(source, "ac5.ts");

    let typeofs = collect_typeof_edges(&staging);
    let field_edges: Vec<_> = typeofs
        .iter()
        .filter(|(from, to, ctx)| {
            from == "Widget::name" && to == "string" && *ctx == Some(TypeOfContext::Field)
        })
        .collect();
    assert!(
        !field_edges.is_empty(),
        "expected TypeOf(Field) edge from Widget::name to string; got: {typeofs:?}"
    );

    // AC-5 also requires the edge `name` field to be the BARE name, not the
    // qualified one. Walk the underlying edge kind to inspect.
    let mut bare_name_present = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::TypeOf { context, name, .. },
            ..
        } = op
            && context.is_some_and(|c| c == TypeOfContext::Field)
        {
            // Resolve interned StringId to text
            let lookup = build_string_lookup(&staging);
            if let Some(name_id) = name
                && let Some(text) = lookup.get(&name_id.index())
                && text == "name"
            {
                bare_name_present = true;
                break;
            }
        }
    }
    assert!(
        bare_name_present,
        "expected at least one Field TypeOf edge to carry bare name 'name'"
    );
}

// ------------------------------------------------------------------
// AC-6: constructor parameter promotion
// ------------------------------------------------------------------

#[test]
fn ac6_ctor_parameter_with_modifier_emits_property() {
    let source = r"
class Person {
    constructor(public name: string, private age: number, protected role: string) {}
}
";
    let staging = build_test_graph(source, "ac6.ts");

    let name = must_find_node(&staging, "Person::name", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, name).as_deref(), Some("public"));

    let age = must_find_node(&staging, "Person::age", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, age).as_deref(), Some("private"));

    let role = must_find_node(&staging, "Person::role", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, role).as_deref(),
        Some("protected")
    );
}

#[test]
fn ac6_ctor_parameter_without_modifier_does_not_promote() {
    let source = r"
class Person {
    constructor(name: string) {}
}
";
    let staging = build_test_graph(source, "ac6_noprop.ts");

    let nodes = find_nodes_by_qname(&staging, "Person::name");
    assert!(
        nodes.is_empty(),
        "constructor param without modifier must NOT emit Person::name; got: {:?}",
        nodes.iter().map(|(_, e)| e.kind).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------------
// AC-7: explicit field wins on collision
// ------------------------------------------------------------------

#[test]
fn ac7_explicit_field_wins_over_promoted_param_collision() {
    // Explicit field declares visibility=public + readonly (Constant)
    // Constructor parameter for the same name has visibility=private
    // The explicit declaration wins; promoted param must NOT downgrade
    // visibility nor change the kind.
    let source = r"
class Person {
    public readonly name: string;
    constructor(private name: string) {}
}
";
    let staging = build_test_graph(source, "ac7.ts");

    // Should be exactly one Constant node Person::name (explicit wins)
    let constants = find_nodes_by_qname(&staging, "Person::name")
        .into_iter()
        .filter(|(_, e)| e.kind == NodeKind::Constant)
        .count();
    assert_eq!(
        constants, 1,
        "expected exactly one Constant Person::name (explicit field wins)"
    );

    // Should NOT have a Property Person::name (promoted param must not create)
    let properties = find_nodes_by_qname(&staging, "Person::name")
        .into_iter()
        .filter(|(_, e)| e.kind == NodeKind::Property)
        .count();
    assert_eq!(
        properties, 0,
        "promoted ctor param must NOT create Property when explicit Constant field exists"
    );

    // visibility stays "public" (None-only attribute fill prevents downgrade)
    let entry = must_find_node(&staging, "Person::name", NodeKind::Constant);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("public"),
        "explicit visibility=public must not be downgraded by promoted param"
    );
}

// ------------------------------------------------------------------
// AC-8: 6 corner cases
// ------------------------------------------------------------------

#[test]
fn ac8_decorator_field_still_emits_property() {
    let source = r"
class Widget {
    @observable name: string;
}
";
    let staging = build_test_graph(source, "ac8_decorator.ts");
    let entry = must_find_node(&staging, "Widget::name", NodeKind::Property);
    assert_eq!(entry.kind, NodeKind::Property);
}

#[test]
fn ac8_intersection_type_field_emits_property() {
    let source = r"
class Mixed {
    inter: A & B;
}
";
    let staging = build_test_graph(source, "ac8_intersection.ts");
    must_find_node(&staging, "Mixed::inter", NodeKind::Property);

    let typeofs = collect_typeof_edges(&staging);
    let has_intersection = typeofs.iter().any(|(from, to, ctx)| {
        from == "Mixed::inter" && *ctx == Some(TypeOfContext::Field) && to == "A & B"
    });
    assert!(
        has_intersection,
        "expected TypeOf(Field) edge from Mixed::inter to 'A & B'; got: {typeofs:?}"
    );
}

#[test]
fn ac8_optional_field_emits_property() {
    let source = r"
class Maybe {
    optional?: number;
}
";
    let staging = build_test_graph(source, "ac8_optional.ts");
    must_find_node(&staging, "Maybe::optional", NodeKind::Property);
}

#[test]
fn ac8_defaulted_field_emits_property() {
    let source = r"
class Defaulted {
    defaulted: number = 5;
}
";
    let staging = build_test_graph(source, "ac8_defaulted.ts");
    must_find_node(&staging, "Defaulted::defaulted", NodeKind::Property);
}

// AC-8 (FR-24) — constructor-parameter-promotion corner cases. Six
// fixtures matching the spec's enumerated list: decorator, intersection,
// optional, defaulted, readonly-promoted, rest. The ordinary-field
// variants above cover non-ctor field paths; these exercise the
// `promote_one_ctor_parameter` branch directly.

#[test]
fn ac8_ctor_decorator_param_emits_property() {
    let source = r"
class C {
    constructor(@Inject() public service: Service) {}
}
";
    let staging = build_test_graph(source, "ac8_ctor_decorator.ts");
    let entry = must_find_node(&staging, "C::service", NodeKind::Property);
    assert_eq!(
        entry_visibility(&staging, entry).as_deref(),
        Some("public"),
        "decorator-annotated promoted ctor param keeps visibility=public"
    );
}

#[test]
fn ac8_ctor_intersection_type_param_emits_property() {
    let source = r"
class C {
    constructor(public mixed: A & B) {}
}
";
    let staging = build_test_graph(source, "ac8_ctor_intersection.ts");
    let entry = must_find_node(&staging, "C::mixed", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("public"));
}

#[test]
fn ac8_ctor_optional_param_emits_property() {
    let source = r"
class C {
    constructor(public maybe?: T) {}
}
";
    let staging = build_test_graph(source, "ac8_ctor_optional.ts");
    let entry = must_find_node(&staging, "C::maybe", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("public"));
}

#[test]
fn ac8_ctor_defaulted_param_uses_pattern_name_not_default_identifier() {
    // REGRESSION: previously the walker took the LAST identifier child of
    // the parameter, which incorrectly grabbed the default expression's
    // identifier (`fallback`) instead of the parameter pattern (`y`),
    // producing a stray `C.fallback` Property node.
    let source = r"
class C {
    constructor(public y: U = fallback) {}
}
";
    let staging = build_test_graph(source, "ac8_ctor_defaulted.ts");

    // The promoted field MUST be C::y.
    let entry = must_find_node(&staging, "C::y", NodeKind::Property);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("public"));

    // And the default-expression identifier MUST NOT have been promoted
    // into a class field.
    let stray = find_nodes_by_qname(&staging, "C::fallback");
    assert!(
        stray.is_empty(),
        "default-expression identifier 'fallback' must NOT promote to C::fallback; got: {:?}",
        stray.iter().map(|(_, e)| e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn ac8_readonly_promoted_ctor_param_emits_constant() {
    let source = r"
class Frozen {
    constructor(public readonly id: number) {}
}
";
    let staging = build_test_graph(source, "ac8_ropromoted.ts");
    let entry = must_find_node(&staging, "Frozen::id", NodeKind::Constant);
    assert_eq!(entry_visibility(&staging, entry).as_deref(), Some("public"));
}

#[test]
fn ac8_rest_ctor_parameter_is_rejected_no_panic() {
    let source = r"
class Variadic {
    constructor(...rest: string[]) {}
}
";
    // Must not panic
    let staging = build_test_graph(source, "ac8_rest.ts");
    let nodes = find_nodes_by_qname(&staging, "Variadic::rest");
    assert!(
        nodes.is_empty(),
        "rest parameter must NOT promote to a class field"
    );
}

#[test]
fn ac8_rest_with_modifier_is_rejected_no_panic() {
    // Even if a modifier appears, rest is rejected (FR-24).
    let source = r"
class Variadic2 {
    constructor(public ...rest: string[]) {}
}
";
    // Must not panic regardless of grammar acceptance
    let staging = build_test_graph(source, "ac8_rest_pub.ts");
    let nodes = find_nodes_by_qname(&staging, "Variadic2::rest");
    assert!(
        nodes.is_empty(),
        "public rest parameter must NOT promote to a class field"
    );
}

// ------------------------------------------------------------------
// AC-9: bare name ambiguity with two classes carrying same field
// ------------------------------------------------------------------

#[test]
fn ac9_two_classes_with_same_field_are_distinct_nodes_under_class_qualification() {
    let source = r"
class A {
    count: number;
}
class B {
    count: number;
}
";
    let staging = build_test_graph(source, "ac9.ts");

    let a_count = must_find_node(&staging, "A::count", NodeKind::Property);
    let b_count = must_find_node(&staging, "B::count", NodeKind::Property);

    // The two field nodes must be distinct
    assert!(
        std::ptr::from_ref(a_count) != std::ptr::from_ref(b_count),
        "A::count and B::count must be distinct entries"
    );

    // Bare name "count" must NOT exist as a class field; the qualifier is
    // mandatory and ambiguity at the `count` short-name is the correct
    // resolver-level signal (verified via staging.nodes()).
    let bare = find_nodes_by_qname(&staging, "count");
    assert!(
        bare.iter().all(|(_, e)| e.kind != NodeKind::Property),
        "no bare `count` Property must exist; got: {:?}",
        bare.iter().map(|(_, e)| e.kind).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------------
// Interface fields keep emitting Property/Constant per AC-2 baseline
// (interfaces have no accessibility_modifier or static, but `readonly`
// still applies → Constant).
// ------------------------------------------------------------------

#[test]
fn interface_field_property_with_readonly_constant() {
    let source = r"
interface IConfig {
    name: string;
    readonly id: number;
}
";
    let staging = build_test_graph(source, "iface.ts");

    must_find_node(&staging, "IConfig::name", NodeKind::Property);
    must_find_node(&staging, "IConfig::id", NodeKind::Constant);
}
