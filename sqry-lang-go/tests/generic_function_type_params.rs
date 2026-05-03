//! GFTP-1..GFTP-5: Generic function and method type-parameter emission.
//!
//! Coverage for U16 / REQ:R0025 / cross-language-field-emission §4.13.
//!
//! Per the gap analysis at
//! `docs/development/public-issue-triage/go_generic_function_type_param_gap_analysis.md`
//! §3 + §4 + §5.2, the Go plugin's `handle_function_declaration` handler
//! never called `process_type_parameters`, so generic top-level functions
//! such as `func Map[T any, U comparable](...)` produced *zero* declaration
//! nodes for `T` and `U`. Likewise `handle_method_declaration` discarded the
//! receiver's existing type-parameter scope, so `func (l *List[E]) Push(v E)`
//! resolved `v`'s type to a bare stub `E` rather than the existing
//! `main.List.E` declaration node.
//!
//! These tests are the failing AC for U16 — they assert the *post-fix*
//! contract, mirroring the GFTP-1..GFTP-5 spec from §5.2 of the gap
//! analysis.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::StagingGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_lang_go::relations::GoGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_go_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("Failed to set Go language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse Go code")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_go_file(source);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new(filename);

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed");

    staging
}

fn build_node_display_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let sqry_core::graph::unified::build::StagingOp::AddNode { entry, expected_id } = op
            {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = staging.resolve_node_display_name(Language::Go, entry)?;
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

fn collect_edges_by_kind<F>(staging: &StagingGraph, predicate: F) -> Vec<(String, String)>
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_display_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let sqry_core::graph::unified::build::StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && predicate(kind)
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));
            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));
            edges.push((from_name, to_name));
        }
    }
    edges
}

fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    expected_context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| {
        matches!(
            kind,
            EdgeKind::TypeOf {
                context: Some(ctx),
                ..
            } if *ctx == expected_context
        )
    })
}

fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| matches!(kind, EdgeKind::References))
}

fn node_exists(staging: &StagingGraph, display_name: &str) -> bool {
    let node_names = build_node_display_name_lookup(staging);
    node_names.values().any(|name| name == display_name)
}

// ============================================================================
// GFTP-1: Single type parameter on a generic function
// ============================================================================

/// Source: `func Map[T any](xs []T) []T { return xs }`
///
/// Asserts the post-fix `process_type_parameters` invocation in
/// `handle_function_declaration` produces:
///
/// 1. A `Type` node with qualified name `main.Map.T` (display name `T`).
/// 2. A `TypeOf{Constraint}` edge `main.Map.T -> any`.
/// 3. The parameter `xs` Reference edges qualify `T` to `main.Map.T`
///    (so `name:T` returns the declared parameter, not the stub).
#[test]
fn gftp_1_single_type_param_on_generic_function() {
    let source = r"package main

func Map[T any](xs []T) []T { return xs }
";

    let staging = build_test_graph(source, "gftp1.go");

    // (1) qualified TypeParameter node exists
    assert!(
        node_exists(&staging, "main.Map.T"),
        "Expected Type node with qualified name 'main.Map.T' (sub-fix 1: \
         handle_function_declaration must call process_type_parameters)"
    );

    // (2) constraint TypeOf edge: main.Map.T -> any
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Map.T" && t == "any"),
        "Expected TypeOf{{Constraint}}: main.Map.T -> any. Got: {constraint_edges:?}"
    );

    // (3) parameter Reference edges qualify the bare T usage
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Map" && t == "main.Map.T"),
        "Expected Reference: main.Map -> main.Map.T (parameter / return \
         qualification through process_function_parameters / \
         process_function_returns). Got: {ref_edges:?}"
    );
}

// ============================================================================
// GFTP-2: Multi-name declaration `func F[T, U any]`
// ============================================================================

/// Source: `func Pair[K, V any](k K, v V) (K, V) { return k, v }`
///
/// Asserts both type parameters get distinct qualified Type nodes and
/// distinct Constraint `TypeOf` edges (one per name in a shared declaration).
#[test]
fn gftp_2_multi_name_type_param_declaration() {
    let source = r"package main

func Pair[K, V any](k K, v V) (K, V) { return k, v }
";

    let staging = build_test_graph(source, "gftp2.go");

    assert!(
        node_exists(&staging, "main.Pair.K"),
        "Expected Type node 'main.Pair.K' for multi-name declaration"
    );
    assert!(
        node_exists(&staging, "main.Pair.V"),
        "Expected Type node 'main.Pair.V' for multi-name declaration"
    );

    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Pair.K" && t == "any"),
        "Expected TypeOf{{Constraint}}: main.Pair.K -> any"
    );
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Pair.V" && t == "any"),
        "Expected TypeOf{{Constraint}}: main.Pair.V -> any"
    );
}

// ============================================================================
// GFTP-3: Union constraint `func Sum[T int | float64]`
// ============================================================================

/// Source: `func Sum[T int | float64](xs []T) T { return xs[0] }`
///
/// Asserts the union-constraint branch of `process_type_constraint` is
/// reached from the function-handler entrypoint:
///
/// - `main.Sum.T` Type node exists.
/// - Reference edges include `int` and `float64` (the union variants).
#[test]
fn gftp_3_union_constraint_on_generic_function() {
    let source = r"package main

func Sum[T int | float64](xs []T) T { return xs[0] }
";

    let staging = build_test_graph(source, "gftp3.go");

    assert!(
        node_exists(&staging, "main.Sum.T"),
        "Expected Type node 'main.Sum.T'"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Sum.T" && t == "int"),
        "Expected Reference: main.Sum.T -> int (union variant). Got: {ref_edges:?}"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Sum.T" && t == "float64"),
        "Expected Reference: main.Sum.T -> float64 (union variant). Got: {ref_edges:?}"
    );
}

// ============================================================================
// GFTP-4: Recursive / dotted constraint + empty type-parameter list
// ============================================================================

/// Source covers two AC-6 corner cases:
///
/// - `func F[T constraints.Ordered](x T)` — dotted constraint
///   (`process_type_constraint` falls through to the bare-identifier
///   branch and walks `extract_all_type_names_from_go_type_with_params`).
/// - `func G[](x int)` — empty type-parameter list. Per
///   `test_empty_type_params` and the gap analysis §4.4, no Type node
///   should be created for an empty list, and the build must not crash.
#[test]
fn gftp_4_recursive_constraint_and_empty_param_list() {
    let source = r"package main

func F[T constraints.Ordered](x T) T { return x }

func G[](x int) int { return x }
";

    let staging = build_test_graph(source, "gftp4.go");

    // Recursive / dotted constraint: F.T exists with constraint TypeOf
    assert!(
        node_exists(&staging, "main.F.T"),
        "Expected Type node 'main.F.T' (dotted constraint constraints.Ordered)"
    );
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.F.T" && t == "constraints.Ordered"),
        "Expected TypeOf{{Constraint}}: main.F.T -> constraints.Ordered. Got: {constraint_edges:?}"
    );

    // Empty type-parameter list MUST NOT produce a `main.G.<anything>` Type
    let display_names: Vec<String> = build_node_display_name_lookup(&staging)
        .into_values()
        .collect();
    assert!(
        !display_names.iter().any(|n| n.starts_with("main.G.")),
        "Empty type-parameter list `func G[]` must not stage any 'main.G.*' \
         Type nodes. Found display names: {display_names:?}"
    );
}

// ============================================================================
// GFTP-5: Receiver-bound type param resolves in method body
// ============================================================================

/// Source (gap analysis §3.3):
///
/// ```go
/// type List[E any] struct{}
/// func (l *List[E]) Push(v E) {}
/// ```
///
/// The declared-type path already creates `main.List.E` from the type-spec
/// handler. `func (l *List[E]) Push(v E)`'s receiver re-uses `E`; it does
/// *not* declare a fresh type parameter. Sub-fix 2 must thread the
/// receiver's `[E]` map into `process_function_parameters` so `v`'s
/// Reference edge target is the existing `main.List.E` qualified node, not
/// the bare stub `E`.
///
/// AC-4 directly: "Method `func (l *List[E]) Push(v E)` resolves `v`'s
/// type to existing `main.List.E` `NodeId`, not stub."
///
/// Note: methods with their OWN type parameters are not legal Go 1.18-1.23
/// per the gap analysis §3.2 / §3.3. We do NOT emit any
/// `main.List.Push.<param>` Type nodes here — that would be a regression.
#[test]
fn gftp_5_receiver_bound_type_param_resolves_in_method() {
    let source = r"package main

type List[E any] struct{}

func (l *List[E]) Push(v E) {}
";

    let staging = build_test_graph(source, "gftp5.go");

    // (1) the declared-type Type node still exists (regression guard for
    //     the type-spec path)
    assert!(
        node_exists(&staging, "main.List.E"),
        "Expected pre-existing Type node 'main.List.E' from type-spec path"
    );

    // (2) v's type qualifies to main.List.E via Reference edges from the
    //     method node
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.List.Push" && t == "main.List.E"),
        "Expected Reference: main.List.Push -> main.List.E (sub-fix 2: \
         method handler must thread receiver type-param map into \
         process_function_parameters). Got: {ref_edges:?}"
    );

    // (3) bare stub 'E' must NOT appear as a Reference target from the
    //     method — that would be the pre-fix behaviour
    assert!(
        !ref_edges
            .iter()
            .any(|(s, t)| s == "main.List.Push" && t == "E"),
        "Reference: main.List.Push -> E (bare stub) must not be staged \
         after sub-fix 2 is applied. Got: {ref_edges:?}"
    );

    // (4) methods do NOT declare their own type parameters in Go 1.18-1.23.
    //     Per the gap analysis §3.2 / §3.3 critical-narrowing rule, we MUST
    //     NOT stage a `main.List.Push.E` Type node — that would model the
    //     receiver's `E` as a fresh declaration on the method scope.
    assert!(
        !node_exists(&staging, "main.List.Push.E"),
        "Method must not stage 'main.List.Push.E' — the receiver's [E] is \
         a *use* of the type-spec's E, not a fresh declaration on the \
         method"
    );

    // (5) GFTP-5 negative TypeOf{Parameter} stub assertion: there must be
    //     NO `TypeOf{Parameter}` edge from `main.List.Push` to a bare `E`
    //     stub. The post-fix resolution targets the `main.List.E` declared
    //     NodeId, so any leaked stub-bound TypeOf{Parameter} would indicate
    //     the receiver type-param map regressed. We accept either of two
    //     post-fix shapes for the parameter type — the canonical
    //     `main.List.E` Reference asserted in (2), and (optionally) a
    //     `TypeOf{Parameter}` edge to the same `main.List.E` qualified
    //     target — but never the bare `E` stub.
    let parameter_typeof_edges =
        collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        !parameter_typeof_edges
            .iter()
            .any(|(s, t)| s == "main.List.Push" && t == "E"),
        "TypeOf{{Parameter}}: main.List.Push -> E (bare stub) must not be \
         staged. The receiver type-param map must resolve `v E` to the \
         existing `main.List.E` NodeId, not a stub. Got: \
         {parameter_typeof_edges:?}"
    );
}

// ============================================================================
// GFTP-6: Anonymous receiver `func (*List[E]) Push(v E)`
// ============================================================================

/// Source:
///
/// ```go
/// type List[E any] struct{}
/// func (*List[E]) Push(v E) {}
/// ```
///
/// Anonymous receivers (no `l` binding name) must still:
///
/// 1. Stage the canonical `main.List.Push` method node (no `[E]` leak in
///    the qualified name — `strip_receiver_modifiers` plus the
///    `qualified_name()` canonicalization both handle the bracketed
///    type-argument suffix).
/// 2. Resolve `v`'s parameter type to the existing `main.List.E` Type node
///    via the receiver type-param map.
/// 3. NOT leak a `TypeOf{Parameter}: main.List.Push -> E` stub edge.
#[test]
fn gftp_6_anonymous_receiver_with_generic_type() {
    let source = r"package main

type List[E any] struct{}

func (*List[E]) Push(v E) {}
";

    let staging = build_test_graph(source, "gftp6.go");

    // (1) Canonical method node exists (no `main.List[E].Push` split).
    assert!(
        node_exists(&staging, "main.List.E"),
        "Expected pre-existing Type node 'main.List.E' from type-spec path"
    );

    let display_names: Vec<String> = build_node_display_name_lookup(&staging)
        .into_values()
        .collect();
    assert!(
        !display_names.iter().any(|n| n.contains("main.List[E]")),
        "Anonymous-receiver method must not stage a 'main.List[E].*' node \
         (qualified_name() must canonicalize via strip_receiver_modifiers). \
         Found: {display_names:?}"
    );

    // (2) Parameter Reference edge resolves to the canonical receiver-bound
    //     Type node.
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.List.Push" && t == "main.List.E"),
        "Expected Reference: main.List.Push -> main.List.E for anonymous \
         receiver `func (*List[E]) Push(v E)`. Got: {ref_edges:?}"
    );

    // (3) No bare-stub TypeOf{Parameter} leak.
    let parameter_typeof_edges =
        collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        !parameter_typeof_edges
            .iter()
            .any(|(s, t)| s == "main.List.Push" && t == "E"),
        "TypeOf{{Parameter}}: main.List.Push -> E stub must not leak from an \
         anonymous receiver. Got: {parameter_typeof_edges:?}"
    );
}

// ============================================================================
// GFTP-7: Multiple receiver type arguments `func (m *Map[K, V]) Put(k K, v V)`
// ============================================================================

/// Source:
///
/// ```go
/// type Map[K comparable, V any] struct{}
/// func (m *Map[K, V]) Put(k K, v V) {}
/// ```
///
/// Multi-arg receivers must thread *all* type-parameter identifiers from
/// the receiver's `type_arguments` list into the method's parameter
/// resolution map. Both `k K` and `v V` must resolve to their canonical
/// receiver-bound Type nodes (`main.Map.K`, `main.Map.V`), and neither may
/// leak a bare-stub `TypeOf{Parameter}` edge.
#[test]
fn gftp_7_multiple_receiver_type_arguments() {
    let source = r"package main

type Map[K comparable, V any] struct{}

func (m *Map[K, V]) Put(k K, v V) {}
";

    let staging = build_test_graph(source, "gftp7.go");

    // Pre-existing receiver-bound Type nodes from the type-spec path.
    assert!(
        node_exists(&staging, "main.Map.K"),
        "Expected pre-existing Type node 'main.Map.K'"
    );
    assert!(
        node_exists(&staging, "main.Map.V"),
        "Expected pre-existing Type node 'main.Map.V'"
    );

    // Parameter References resolve through the receiver type-param map.
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Map.Put" && t == "main.Map.K"),
        "Expected Reference: main.Map.Put -> main.Map.K (k K parameter). \
         Got: {ref_edges:?}"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Map.Put" && t == "main.Map.V"),
        "Expected Reference: main.Map.Put -> main.Map.V (v V parameter). \
         Got: {ref_edges:?}"
    );

    // No bare-stub leaks for either type parameter.
    let parameter_typeof_edges =
        collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        !parameter_typeof_edges
            .iter()
            .any(|(s, t)| s == "main.Map.Put" && (t == "K" || t == "V")),
        "TypeOf{{Parameter}}: main.Map.Put -> K/V stub must not leak for a \
         multi-arg receiver. Got: {parameter_typeof_edges:?}"
    );

    // Canonical method qualified name — no `main.Map[K, V].Put` split.
    let display_names: Vec<String> = build_node_display_name_lookup(&staging)
        .into_values()
        .collect();
    assert!(
        !display_names.iter().any(|n| n.contains("main.Map[")),
        "Multi-arg receiver must not stage a 'main.Map[K, V].*' node \
         (qualified_name() must strip the [...] suffix). Found: \
         {display_names:?}"
    );
}

// ============================================================================
// GFTP-8: Body-call canonicalization on a generic receiver
// ============================================================================

/// Source:
///
/// ```go
/// type List[E any] struct{}
/// func (l *List[E]) Push(v E) { l.Append(v) }
/// func (l *List[E]) Append(v E) {}
/// ```
///
/// `FunctionContext::qualified_name()` is the source of truth for the
/// body-call edge's `from` node. Without canonicalization, `Push`'s body
/// would emit a `Calls` edge from `main.List[E].Push` while
/// `add_method_export_edge_unified` exports `main.List.Push`, splitting
/// the same method across two `NodeIds`.
///
/// This test confirms the `Calls` edge source is the canonical
/// `main.List.Push` form.
#[test]
fn gftp_8_body_call_canonicalization_on_generic_receiver() {
    let source = r"package main

type List[E any] struct{}

func (l *List[E]) Push(v E) { l.Append(v) }

func (l *List[E]) Append(v E) {}
";

    let staging = build_test_graph(source, "gftp8.go");

    let node_names = build_node_display_name_lookup(&staging);
    let mut call_edges: Vec<(String, String)> = Vec::new();
    for op in staging.operations() {
        if let sqry_core::graph::unified::build::StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Calls { .. },
            ..
        } = op
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));
            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));
            call_edges.push((from_name, to_name));
        }
    }

    // The Push -> Append call edge must have a canonical `main.List.Push`
    // source, not a `main.List[E].Push` split.
    assert!(
        call_edges.iter().any(|(s, _)| s == "main.List.Push"),
        "Expected Calls edge sourced from canonical 'main.List.Push' \
         (FunctionContext::qualified_name() must strip receiver modifiers). \
         Got: {call_edges:?}"
    );
    assert!(
        !call_edges.iter().any(|(s, _)| s.contains("main.List[E]")),
        "Calls edge source must not be 'main.List[E].Push' (split-method \
         bug regression). Got: {call_edges:?}"
    );

    // No 'main.List[E].*' display names should be staged anywhere.
    let display_names: Vec<String> = node_names.into_values().collect();
    assert!(
        !display_names.iter().any(|n| n.contains("main.List[E]")),
        "No 'main.List[E].*' nodes may be staged after the qualified_name() \
         canonicalization. Found: {display_names:?}"
    );
}
