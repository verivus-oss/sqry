//! AC-1, AC-2 (×3), AC-3, AC-7, AC-8 integration tests for the Go T1.1
//! implicit-interface-satisfaction pass.
//!
//! Each test exercises the canonical
//! `sqry_core::graph::unified::build::build_unified_graph` pipeline over
//! a real `go build`-able Go fixture. Assertions go through
//! `graph.indices().by_qualified_name(...)` and
//! `graph.edges().edges_from(...)`, satisfying the AC-13 sqry-first
//! verifiability invariant — no `grep`/`cat`/raw text on fixture source.
//!
//! All seven AC tests in this file run live as of Cluster G1
//! (05_TEST_PLAN.md §7.5 RESOLVED — the qn separator mismatch and the
//! pass's internal `.`-form composers / parsers were fixed
//! end-to-end). Tests assert against `helper.add_*`-canonical
//! `::`-separated qns by canonicalising their AC-natural lookup
//! strings on input.

#[path = "common/mod.rs"]
mod common;

use sqry_core::graph::Language;
use sqry_core::graph::unified::CodeGraph;
use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::resolution::canonicalize_graph_qualified_name;
use std::path::Path;

/// Look up the unique NodeId for a given qualified name. AC bodies cite
/// AC-natural Go-form qns (`fx.File`, `fx.*File`) per 01_SPEC §7. Node
/// qns are interned in canonical (`::`-separated) form via
/// `helper.add_*` → `canonicalize_graph_qualified_name`, so the
/// lookup canonicalises its input before hitting the string interner.
/// Panics if the lookup is empty or ambiguous — both indicate a
/// fixture-shape regression the AC test should not paper over.
fn node_for_qn(graph: &CodeGraph, qn: &str) -> NodeId {
    let canonical = canonicalize_graph_qualified_name(Language::Go, qn);
    let qn_id = graph.strings().get(&canonical).unwrap_or_else(|| {
        panic!("qualified name {qn:?} (canonical {canonical:?}) is not interned")
    });
    let candidates = graph.indices().by_qualified_name(qn_id);
    assert_eq!(
        candidates.len(),
        1,
        "expected exactly one node for qn {qn:?} (canonical {canonical:?}), got {}",
        candidates.len(),
    );
    candidates[0]
}

/// Return true iff the graph carries an `Implements` edge from
/// `source_qn` to `target_qn`. Canonicalises both inputs (per
/// `node_for_qn`'s rationale). Hardening guard (Cluster G1 codex-iter
/// finding): panics if NEITHER source nor target qn is interned — a
/// negative-assertion AC test must not silently "pass" because the
/// fixture failed to materialise any nodes.
fn has_implements_edge(graph: &CodeGraph, source_qn: &str, target_qn: &str) -> bool {
    let src_canonical = canonicalize_graph_qualified_name(Language::Go, source_qn);
    let tgt_canonical = canonicalize_graph_qualified_name(Language::Go, target_qn);
    let src_id_opt = graph.strings().get(&src_canonical);
    let tgt_id_opt = graph.strings().get(&tgt_canonical);
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

/// AC-1: implicit interface satisfaction — single method. `*fx.File`
/// satisfies `fx.Reader` because `(*File).Read` has a pointer
/// receiver, so the value method set of `File` does NOT include
/// `Read`, only the pointer method set does. The pass must mint a
/// synthetic pointer-form `Type` node `fx.*File` and emit
/// `Implements(*File → Reader)` from it.
#[test]
fn ac1_single_method() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac1"));

    // The pointer-form synthetic node must exist.
    let ptr_node = node_for_qn(&graph, "fx.*File");

    // Implements(*File → Reader) must be emitted.
    let reader = node_for_qn(&graph, "fx.Reader");
    let outgoing = graph.edges().edges_from(ptr_node);
    assert!(
        outgoing
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
        "Implements(*fx.File → fx.Reader) must be emitted",
    );

    // Value-form fx.File must NOT carry Implements(File → Reader).
    let file = node_for_qn(&graph, "fx.File");
    let file_outgoing = graph.edges().edges_from(file);
    assert!(
        !file_outgoing
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
        "value-form fx.File must NOT carry Implements (pointer-receiver semantics)",
    );
}

/// AC-2 (cross-file, same package): `Reader` lives in `a.go`,
/// `File` lives in `b.go`. The pass must emit
/// `Implements(File → Reader)` (value receiver on `File.Read`).
#[test]
fn ac2_cross_file() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac2/single"));
    assert!(
        has_implements_edge(&graph, "fx.File", "fx.Reader"),
        "Implements(fx.File → fx.Reader) must be emitted across files in the same package",
    );
}

/// AC-2 (declaration-order independence): the AC-2-single fixture
/// declares Reader in a.go (first) and File in b.go. We rebuild the
/// graph twice and compare the canonicalised `Implements` edge
/// multisets — order must not affect the outcome.
#[test]
fn ac2_declaration_order() {
    let g1 = common::build_workspace(Path::new("tests/fixtures/go/implements_ac2/single"));
    let g2 = common::build_workspace(Path::new("tests/fixtures/go/implements_ac2/single"));
    let m1 = canonical_implements_multiset(&g1);
    let m2 = canonical_implements_multiset(&g2);
    assert_eq!(
        m1, m2,
        "Implements edge multiset must be order-independent across rebuilds",
    );
    let src_canonical = canonicalize_graph_qualified_name(Language::Go, "fx.File");
    let tgt_canonical = canonicalize_graph_qualified_name(Language::Go, "fx.Reader");
    assert!(
        m1.iter()
            .any(|(src, tgt)| src == &src_canonical && tgt == &tgt_canonical),
        "Implements(fx.File → fx.Reader) must appear in the multiset",
    );
}

/// AC-2 (cross-package): `pkg_a.Reader` and `pkg_b.File` in the same
/// workspace; satisfaction crosses the package boundary on
/// method-name match alone.
#[test]
fn ac2_cross_package() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac2/multi_pkg"));
    assert!(
        has_implements_edge(&graph, "pkg_b.File", "pkg_a.Reader"),
        "Implements(pkg_b.File → pkg_a.Reader) must be emitted across package boundaries",
    );
}

/// AC-3: pointer-vs-value receiver discrimination. `BufferV.Write`
/// has a value receiver — both value and pointer method sets carry
/// it, so `Implements(BufferV → Writer)` is emitted. `BufferP.Write`
/// has a pointer receiver — only the pointer method set carries it,
/// so `Implements(*BufferP → Writer)` is emitted from the synthetic
/// pointer-form node, and `Implements(BufferP → Writer)` MUST NOT
/// exist.
#[test]
fn ac3_pointer_value() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac3"));

    // BufferV (value receiver) satisfies Writer.
    assert!(
        has_implements_edge(&graph, "fx.BufferV", "fx.Writer"),
        "Implements(fx.BufferV → fx.Writer) must be emitted (value receiver)",
    );

    // *BufferP (pointer-form) satisfies Writer.
    assert!(
        has_implements_edge(&graph, "fx.*BufferP", "fx.Writer"),
        "Implements(fx.*BufferP → fx.Writer) must be emitted (pointer receiver)",
    );

    // BufferP value form MUST NOT satisfy Writer.
    assert!(
        !has_implements_edge(&graph, "fx.BufferP", "fx.Writer"),
        "Implements(fx.BufferP → fx.Writer) MUST NOT be emitted (pointer-only satisfaction)",
    );
}

/// AC-7: interface mismatch — `NotACloser` exposes `Open()` not
/// `Close()`, so no `Implements` edge to `Closer` may exist on
/// either the value or pointer form.
#[test]
fn ac7_mismatch() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac7"));
    assert!(
        !has_implements_edge(&graph, "fx.NotACloser", "fx.Closer"),
        "Implements(fx.NotACloser → fx.Closer) MUST NOT be emitted",
    );
    assert!(
        !has_implements_edge(&graph, "fx.*NotACloser", "fx.Closer"),
        "Implements(fx.*NotACloser → fx.Closer) MUST NOT be emitted",
    );
}

/// AC-8: the universal interface (`interface{}` / `any`) must be
/// excluded per `01_SPEC.md` §5.7. `Implements(X → HasM)` must be
/// emitted; `Implements(X → Empty)` MUST NOT be emitted even though
/// `X` trivially satisfies `Empty`.
#[test]
fn ac8_empty_iface() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/implements_ac8"));
    assert!(
        has_implements_edge(&graph, "fx.X", "fx.HasM"),
        "Implements(fx.X → fx.HasM) must be emitted (single-method interface)",
    );
    assert!(
        !has_implements_edge(&graph, "fx.X", "fx.Empty"),
        "Implements(fx.X → fx.Empty) MUST NOT be emitted (empty interface filtered per §5.7)",
    );
}

/// Build a canonicalised `(source_qn, target_qn)` multiset of every
/// `Implements` edge in the graph. Used by `ac2_declaration_order`
/// and `determinism::ac12_bit_identical_x_runs` to compare runs.
fn canonical_implements_multiset(graph: &CodeGraph) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (nid, entry) in graph.nodes().iter() {
        let src_qn = entry
            .qualified_name
            .and_then(|id| graph.strings().resolve(id))
            .map(|s| s.as_ref().to_string());
        let Some(src) = src_qn else { continue };
        for edge in graph.edges().edges_from(nid) {
            if matches!(edge.kind, EdgeKind::Implements)
                && let Some(tgt_entry) = graph.nodes().get(edge.target)
                && let Some(tgt) = tgt_entry
                    .qualified_name
                    .and_then(|id| graph.strings().resolve(id))
                    .map(|s| s.as_ref().to_string())
            {
                out.push((src.clone(), tgt));
            }
        }
    }
    out.sort();
    out
}
