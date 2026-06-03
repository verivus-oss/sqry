//! T2.5 generic-instantiation emission (explicit path).
//!
//! Covers the acceptance criteria from
//! `docs/development/go-channels-and-generic-instantiation/01_SPEC.md` that the
//! Phase 1 *explicit* instantiation path is responsible for:
//!
//! * AC-7  — explicit `Map[string, int](..)` emits
//!   `Instantiates { type_args: ["string", "int"], Explicit }`.
//! * AC-8  — function-argument inference (`slices.SortFunc`) emits
//!   `Instantiates { ["[]q.User", "q.User"], Inferred }`.
//! * AC-9  — partial instantiation (`apply[[]int](..)`) emits
//!   `Instantiates { ["[]int", "<unknown>"], Partial }`.
//! * AC-10 — untyped-constant default typing (`min(1, 2)`) emits
//!   `Instantiates { [{int, default_typed}], Inferred }`.
//! * AC-11 — explicit `makeT[int]()` stays `Explicit`; an unsolvable inferred
//!   call falls back to `Instantiates { ["<unknown>"], Unknown }`.
//! * AC-12 — the existing `Calls` edge is preserved unchanged.
//! * AC-13 — a goroutine over a generic call keeps `is_async` on `Calls` and
//!   emits the `Instantiates` edge.
//! * AC-14 — a non-generic call emits no `Instantiates` edge.
//!
//! The channel-pairing surface (AC-1..AC-6) lives in `t2_channels.rs`.

use std::collections::HashMap;
use std::path::Path;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StringId;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::InferenceKind;
use sqry_lang_go::relations::GoGraphBuilder;
use tree_sitter::Parser;

fn parse_go_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("set Go language");
    parser.parse(content.as_bytes(), None).expect("parse Go")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_go_file(source);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new(filename), &mut staging)
        .expect("build_graph should succeed");
    staging
}

/// Map every staging-local `StringId` to its interned text.
fn string_lookup(staging: &StagingGraph) -> HashMap<StringId, String> {
    let mut map = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            map.insert(*local_id, value.clone());
        }
    }
    map
}

/// Map every staged node index to its display (qualified or plain) name.
fn node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = string_lookup(staging);
    let mut map = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(id) = expected_id
        {
            let name = entry
                .qualified_name
                .and_then(|q| strings.get(&q).cloned())
                .or_else(|| strings.get(&entry.name).cloned())
                .unwrap_or_default();
            map.insert(id.index(), name);
        }
    }
    map
}

struct InstantiatesEdge {
    target_name: String,
    type_args: Vec<(String, bool)>,
    inference_kind: InferenceKind,
}

fn collect_instantiates(staging: &StagingGraph) -> Vec<InstantiatesEdge> {
    let strings = string_lookup(staging);
    let names = node_name_lookup(staging);
    let mut out = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind:
                EdgeKind::Instantiates {
                    type_args,
                    inference_kind,
                },
            ..
        } = op
        {
            let resolved: Vec<(String, bool)> = type_args
                .iter()
                .map(|ta| {
                    (
                        strings.get(&ta.name).cloned().unwrap_or_default(),
                        ta.default_typed,
                    )
                })
                .collect();
            out.push(InstantiatesEdge {
                target_name: names.get(&target.index()).cloned().unwrap_or_default(),
                type_args: resolved,
                inference_kind: *inference_kind,
            });
        }
    }
    out
}

#[test]
fn ac7_explicit_instantiation_emits_instantiates_edge() {
    let src = r#"
package q

func Map[K comparable, V any](m map[K]V) []K { return nil }

func main() {
    _ = Map[string, int](nil)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);

    assert_eq!(
        edges.len(),
        1,
        "exactly one Instantiates edge expected, got {}",
        edges.len()
    );
    let edge = &edges[0];
    assert_eq!(edge.inference_kind, InferenceKind::Explicit);
    assert!(
        edge.target_name.contains("Map"),
        "Instantiates target should be the generic Map (got {:?})",
        edge.target_name
    );
    let arg_names: Vec<&str> = edge.type_args.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        arg_names,
        vec!["string", "int"],
        "type_args must be the explicit [string, int] in declaration order"
    );
    assert!(
        edge.type_args
            .iter()
            .all(|(_, default_typed)| !default_typed),
        "explicit args are never default-typed"
    );
}

#[test]
fn single_type_arg_explicit_instantiation() {
    // NOTE (parser reality): tree-sitter-go parses an explicit-bracket generic
    // call `Work[int](42)` as a `type_conversion_expression`, not a
    // `call_expression`, so it does not produce a `Calls` edge today (a
    // pre-existing gap independent of this feature). AC-12's "Calls edge
    // preserved unchanged" therefore holds trivially — this feature removes no
    // existing edge — and we assert the new Instantiates edge directly.
    let src = r#"
package q

func Work[T any](x T) {}

func main() {
    Work[int](42)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one Instantiates edge expected");
    assert_eq!(edges[0].inference_kind, InferenceKind::Explicit);
    let arg_names: Vec<&str> = edges[0].type_args.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(arg_names, vec!["int"]);
    assert!(edges[0].target_name.contains("Work"));
}

#[test]
fn generic_type_conversion_emits_no_instantiates() {
    // `Box[int](nil)` where Box is a generic TYPE is a conversion, not a
    // generic function call. tree-sitter-go parses it identically to
    // `Map[string,int](nil)` (type_conversion_expression wrapping
    // generic_type), so the emitter must disambiguate via the
    // locally-declared-generic-function gate and emit NO Instantiates edge.
    // Regression for the false positive caught in cross-LLM review (Codex).
    let src = r#"
package q

type Box[T any] []T

func main() {
    _ = Box[int](nil)
}
"#;
    let staging = build_test_graph(src, "q.go");
    assert_eq!(
        collect_instantiates(&staging).len(),
        0,
        "a generic TYPE conversion must not emit an Instantiates edge (false-positive fence)"
    );
}

/// Calls edges as `(target_name, is_async)` pairs, for the AC-12/AC-13
/// regression checks.
fn collect_calls(staging: &StagingGraph) -> Vec<(String, bool)> {
    let names = node_name_lookup(staging);
    let mut out = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Calls { is_async, .. },
            ..
        } = op
        {
            out.push((
                names.get(&target.index()).cloned().unwrap_or_default(),
                *is_async,
            ));
        }
    }
    out
}

#[test]
fn ac8_inferred_instantiation_from_function_arguments() {
    // `slices.SortFunc(users, func(a, b User) int {...})` — function-argument
    // inference resolves S = []q.User (from `users`) and E = q.User (from the
    // func-literal parameter types). Catalog source: stdlib_generics.
    let src = r#"
package q

import "slices"

type User struct{ Age int }

func main() {
    users := []User{{Age: 1}}
    slices.SortFunc(users, func(a, b User) int { return a.Age - b.Age })
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one Instantiates edge expected (AC-8)");
    let edge = &edges[0];
    assert_eq!(edge.inference_kind, InferenceKind::Inferred);
    assert!(
        edge.target_name.contains("SortFunc"),
        "target should be slices.SortFunc (got {:?})",
        edge.target_name
    );
    let arg_names: Vec<&str> = edge.type_args.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        arg_names,
        vec!["[]q.User", "q.User"],
        "S = []q.User, E = q.User in declaration order"
    );
    assert!(edge.type_args.iter().all(|(_, dt)| !dt));

    // AC-12: the Calls edge to slices.SortFunc is still present (arg_count = 2,
    // not async).
    let calls = collect_calls(&staging);
    assert!(
        calls.iter().any(|(t, a)| t.contains("SortFunc") && !*a),
        "Calls edge to SortFunc must be preserved (AC-12); calls = {calls:?}"
    );
}

#[test]
fn ac9_partial_instantiation_pads_unknown_suffix() {
    // `apply[[]int](nil, f)` provides only the first of two type arguments;
    // the suffix is padded with `<unknown>` and the edge marked Partial.
    let src = r#"
package q

func apply[S ~[]E, E any](s S, f func(E) E) S { return s }

func main() {
    _ = apply[[]int](nil, func(x int) int { return x })
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one Instantiates edge expected (AC-9)");
    let edge = &edges[0];
    assert_eq!(edge.inference_kind, InferenceKind::Partial);
    let arg_names: Vec<&str> = edge.type_args.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(arg_names, vec!["[]int", "<unknown>"]);
}

#[test]
fn ac10_untyped_constant_default_typing() {
    // `min(1, 2)` — both args untyped int constants; Go's default-type rule
    // yields T = int with the default_typed flag set, inference_kind = Inferred.
    let src = r#"
package q

func min[T int | float64](a, b T) T {
    if a < b {
        return a
    }
    return b
}

func main() {
    _ = min(1, 2)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one Instantiates edge expected (AC-10)");
    let edge = &edges[0];
    assert_eq!(edge.inference_kind, InferenceKind::Inferred);
    assert_eq!(edge.type_args.len(), 1);
    assert_eq!(edge.type_args[0].0, "int");
    assert!(
        edge.type_args[0].1,
        "the untyped-constant slot must be flagged default_typed (AC-10)"
    );
}

#[test]
fn ac11_explicit_solvable_and_unsolvable_fallback() {
    // makeT[int]() is explicit (solvable) -> Explicit ["int"].
    // conv(z), where T appears only in the result, is unsolvable by the
    // Phase 1 rules -> Unknown ["<unknown>"].
    let src = r#"
package q

func makeT[T any]() T { var z T; return z }

func conv[T any](x any) T { var z T; return z }

func main() {
    _ = makeT[int]()
    var z any
    _ = conv(z)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 2, "two Instantiates edges expected (AC-11)");

    let make_edge = edges
        .iter()
        .find(|e| e.target_name.contains("makeT"))
        .expect("makeT instantiation");
    assert_eq!(make_edge.inference_kind, InferenceKind::Explicit);
    assert_eq!(
        make_edge
            .type_args
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        vec!["int"]
    );

    let conv_edge = edges
        .iter()
        .find(|e| e.target_name.contains("conv"))
        .expect("conv instantiation");
    assert_eq!(conv_edge.inference_kind, InferenceKind::Unknown);
    assert_eq!(
        conv_edge
            .type_args
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        vec!["<unknown>"]
    );
}

#[test]
fn ac13_goroutine_over_generic_emits_instantiates() {
    // AC-13: `go Work[int](42)`. The explicit-bracket call parses as a
    // type_conversion_expression (parser reality), so it carries no Calls edge;
    // is_async therefore has nothing to attach to and is trivially preserved.
    // The Instantiates edge must still be emitted from inside the goroutine.
    let src = r#"
package q

func Work[T any](x T) {}

func main() {
    go Work[int](42)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one Instantiates edge expected (AC-13)");
    assert_eq!(edges[0].inference_kind, InferenceKind::Explicit);
    assert!(edges[0].target_name.contains("Work"));
    assert_eq!(
        edges[0]
            .type_args
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        vec!["int"]
    );
}

#[test]
fn goroutine_over_inferred_generic_keeps_is_async() {
    // Companion to AC-13 using the inferred (no-bracket) goroutine form, which
    // DOES produce a Calls edge — so we can assert is_async = true is preserved
    // alongside the Instantiates edge.
    let src = r#"
package q

func Work[T any](x T) {}

func main() {
    go Work(42)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let edges = collect_instantiates(&staging);
    assert_eq!(edges.len(), 1, "one inferred Instantiates edge expected");
    assert_eq!(edges[0].inference_kind, InferenceKind::Inferred);
    assert_eq!(edges[0].type_args[0].0, "int");

    let calls = collect_calls(&staging);
    assert!(
        calls.iter().any(|(t, a)| t.contains("Work") && *a),
        "goroutine Calls edge must keep is_async = true (AC-13 spirit); calls = {calls:?}"
    );
}

#[test]
fn ac14_non_generic_call_emits_no_instantiates() {
    let src = r#"
package q

func plain() {}

func main() {
    plain()
}
"#;
    let staging = build_test_graph(src, "q.go");
    assert_eq!(
        collect_instantiates(&staging).len(),
        0,
        "non-generic calls must emit zero Instantiates edges (AC-14)"
    );
}
