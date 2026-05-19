//! AC-10, AC-11 integration tests for the Go T1.3 function-signature
//! implements pass.
//!
//! Both tests run live as of Cluster G1 (05_TEST_PLAN.md §7.5
//! RESOLVED). The T1.3 wiring fix is summarised in that section.

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
    // Hardening guard (Cluster G1 codex-iter): negative-assertion AC
    // tests must not silently "pass" because the fixture failed to
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

/// AC-10: function-signature implements. `Op` is a named function
/// type with NO methods; the conversion `Op(double)` is the
/// address-taken-function witness that triggers T1.3, emitting
/// `Implements(double → Op)`.
#[test]
fn ac10_simple() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/sig_ac10"));
    assert!(
        has_implements_edge(&graph, "fx.double", "fx.Op"),
        "Implements(fx.double → fx.Op) must be emitted (T1.3 function-signature implements)",
    );
}

/// AC-11: HandlerFunc dual edges. `HandlerFunc` is a named function
/// type WITH a method (`ServeHTTP`), so:
///   - T1.3 emits `Implements(myHandler → HandlerFunc)` via the
///     `HandlerFunc(myHandler)` conversion.
///   - T1.1 emits `Implements(HandlerFunc → Handler)` because
///     `HandlerFunc.ServeHTTP` matches `Handler.ServeHTTP`.
///
/// Both edges must coexist.
#[test]
fn ac11_handlerfunc() {
    let graph = common::build_workspace(Path::new("tests/fixtures/go/sig_ac11"));

    assert!(
        has_implements_edge(&graph, "fx.myHandler", "fx.HandlerFunc"),
        "Implements(fx.myHandler → fx.HandlerFunc) must be emitted (T1.3)",
    );
    assert!(
        has_implements_edge(&graph, "fx.HandlerFunc", "fx.Handler"),
        "Implements(fx.HandlerFunc → fx.Handler) must be emitted (T1.1 method-set)",
    );
}
