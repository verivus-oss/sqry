//! AC-4, AC-5, AC-6, AC-9 integration tests for the Go T1.2 method-set
//! promotion pass.
//!
//! As of Cluster G1, `ac4_ambiguity` and `ac6_pointer_required` run
//! live (05_TEST_PLAN.md §7.5 RESOLVED). Two remain `#[ignore]`d:
//!   - `ac5_promoted_queryable` — Cluster G1 follow-up; local var
//!     TypeOf-edge target plumbing routes to an unqualified Type stub
//!     instead of the canonical Struct. See `05_TEST_PLAN.md §7.6`.
//!   - `ac9_alias_embedding` — Phase 2 (alias-of-unnamed-struct
//!     embedding per `golang/go#66540`, out of T1.1 scope per
//!     `01_SPEC.md §8`). See `05_TEST_PLAN.md §7.7`.

#[path = "common/mod.rs"]
mod common;

use sqry_core::graph::Language;
use sqry_core::graph::unified::CodeGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::resolution::canonicalize_graph_qualified_name;
use std::path::Path;

fn has_implements_edge(graph: &CodeGraph, source_qn: &str, target_qn: &str) -> bool {
    let src_canonical = canonicalize_graph_qualified_name(Language::Go, source_qn);
    let tgt_canonical = canonicalize_graph_qualified_name(Language::Go, target_qn);
    let src_id_opt = graph.strings().get(&src_canonical);
    let tgt_id_opt = graph.strings().get(&tgt_canonical);
    // Hardening guard (Cluster G1 codex-iter): a negative-assertion AC
    // test must not silently "pass" because the fixture failed to
    // materialise any nodes.
    assert!(
        src_id_opt.is_some() || tgt_id_opt.is_some(),
        "has_implements_edge: neither source qn {source_qn:?} (canonical {src_canonical:?}) nor target qn {target_qn:?} (canonical {tgt_canonical:?}) is interned — fixture likely failed to materialise either node",
    );
    let (Some(src_id), Some(tgt_id)) = (src_id_opt, tgt_id_opt) else {
        return false;
    };
    let src_candidates = graph.indices().by_qualified_name(src_id);
    let tgt_candidates = graph.indices().by_qualified_name(tgt_id);
    for &src in src_candidates {
        for edge in graph.edges().edges_from(src) {
            if matches!(edge.kind, EdgeKind::Implements) && tgt_candidates.contains(&edge.target) {
                return true;
            }
        }
    }
    false
}

/// AC-4 (verbatim `golang/go#57352`): same-depth method-name ambiguity
/// blocks both promotion AND `Implements`. `Foo` embeds `A` and `AB`;
/// both interfaces declare `a()`, so `a` is ambiguous at depth 1 and
/// is NOT in `Foo`'s method set. Neither `Implements(Foo → AB)` nor
/// `Implements(Foo → A)` may exist; no promoted `fx.Foo.a` node may be
/// materialised.
#[test]
fn ac4_ambiguity() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac4"));

    assert!(
        !has_implements_edge(&graph, "fx.Foo", "fx.AB"),
        "Implements(fx.Foo → fx.AB) MUST NOT be emitted (same-depth ambiguity)",
    );
    assert!(
        !has_implements_edge(&graph, "fx.Foo", "fx.A"),
        "Implements(fx.Foo → fx.A) MUST NOT be emitted (same-depth ambiguity)",
    );

    // No promoted `fx.Foo.a` node may be materialised. The interned
    // qualified name might not exist at all (StringId not minted), or
    // — if minted — the by_qualified_name bucket must be empty of any
    // synthetic Method node.
    if let Some(qn_id) = graph
        .strings()
        .get(&canonicalize_graph_qualified_name(Language::Go, "fx.Foo.a"))
    {
        let candidates = graph.indices().by_qualified_name(qn_id);
        assert!(
            candidates.is_empty(),
            "promoted method node fx.Foo.a MUST NOT be materialised (golang/go#57352)",
        );
    }
}

/// AC-5: promoted method is queryable from the outer type. `Outer`
/// embeds `Inner`; `Inner.Greeting()` is reachable as
/// `Outer.Greeting()` via promotion. The pass mints the synthetic
/// `fx.Outer.Greeting` method node and back-references its call site
/// from `fx.use`.
#[test]
#[ignore = "Cluster G follow-up — local var TypeOf-edge target plumbing routes to unqualified Type stub instead of canonical Struct; see 05_TEST_PLAN §7.6"]
fn ac5_promoted_queryable() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/promote_ac5"));

    // Promoted `fx.Outer.Greeting` node must exist.
    let qn_id = graph
        .strings()
        .get(&canonicalize_graph_qualified_name(
            Language::Go,
            "fx.Outer.Greeting",
        ))
        .expect("fx.Outer.Greeting qn must be interned by the pass");
    let candidates = graph.indices().by_qualified_name(qn_id);
    assert!(
        !candidates.is_empty(),
        "promoted method fx.Outer.Greeting must be materialised",
    );

    // Shadow `Calls` or `References` edge from `fx.use` to the
    // promoted method node must exist (AC-5 wording: direct_callers
    // of fx.Outer.Greeting returns fx.use).
    let use_qn = graph
        .strings()
        .get(&canonicalize_graph_qualified_name(Language::Go, "fx.use"))
        .expect("fx.use qn interned");
    let use_candidates = graph.indices().by_qualified_name(use_qn).to_vec();
    assert_eq!(use_candidates.len(), 1, "exactly one fx.use node");
    let use_node = use_candidates[0];

    let outgoing = graph.edges().edges_from(use_node);
    let promoted_set: std::collections::BTreeSet<_> = candidates.iter().copied().collect();
    assert!(
        outgoing.iter().any(
            |e| matches!(e.kind, EdgeKind::Calls { .. } | EdgeKind::References)
                && promoted_set.contains(&e.target)
        ),
        "fx.use must reach fx.Outer.Greeting via the shadow Calls/References edge",
    );
}

/// AC-6: pointer-required promotion does not over-promote. `Inner`
/// has a pointer-receiver method `Mutate`. `OuterV` embeds `Inner`
/// (value embedding) — only the pointer form `*OuterV` satisfies
/// `Mutator`. `OuterP` embeds `*Inner` (pointer embedding) — both
/// `OuterP` and `*OuterP` satisfy `Mutator`.
#[test]
fn ac6_pointer_required() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/promote_ac6"));

    // OuterV (value) does NOT satisfy Mutator.
    assert!(
        !has_implements_edge(&graph, "fx.OuterV", "fx.Mutator"),
        "Implements(fx.OuterV → fx.Mutator) MUST NOT be emitted (value embedding of pointer-receiver method)",
    );
    // *OuterV DOES satisfy Mutator.
    assert!(
        has_implements_edge(&graph, "fx.*OuterV", "fx.Mutator"),
        "Implements(fx.*OuterV → fx.Mutator) must be emitted",
    );
    // OuterP (pointer-embed of Inner) DOES satisfy Mutator.
    assert!(
        has_implements_edge(&graph, "fx.OuterP", "fx.Mutator"),
        "Implements(fx.OuterP → fx.Mutator) must be emitted (pointer embedding lifts to value form)",
    );
    // *OuterP DOES satisfy Mutator (Go spec §"Method sets":
    // pointer-form of an outer struct sees both value- and
    // pointer-receiver methods promoted through the pointer embed).
    // Per 01_SPEC §7 AC-6 line 793: "Promoted method `Mutate` is
    // reachable from both `fx.OuterP` and `*fx.OuterP`."
    assert!(
        has_implements_edge(&graph, "fx.*OuterP", "fx.Mutator"),
        "Implements(fx.*OuterP → fx.Mutator) must be emitted",
    );
}

/// AC-9 (verbatim `golang/go#66540`): type-alias embedding promotes
/// through the alias. `A = struct { io.Reader }` is an alias for an
/// unnamed struct embedding `io.Reader`. `S struct { A }` then
/// promotes `io.Reader`'s `Read` method onto `S`, so `S` (or `*S`
/// depending on receiver pointerness) satisfies `io.Reader`.
#[test]
#[ignore = "Phase 2 — golang/go#66540 alias-of-unnamed-struct embedding promotion; out of T1.1 scope per 01_SPEC §8; see 05_TEST_PLAN §7.7"]
fn ac9_alias_embedding() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/promote_ac9"));

    // Either S or *S must implement io.Reader. The exact bucket
    // depends on the resolved pointerness of io.Reader.Read; AC-9's
    // wording asserts S satisfies io.Reader without prescribing
    // bucket — accept either.
    let satisfied = has_implements_edge(&graph, "fx.S", "io.Reader")
        || has_implements_edge(&graph, "fx.*S", "io.Reader");
    assert!(
        satisfied,
        "Implements(fx.S → io.Reader) or Implements(fx.*S → io.Reader) must be emitted (type-alias embedding promotion)",
    );
}
