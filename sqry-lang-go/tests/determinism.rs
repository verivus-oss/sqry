//! AC-12 build-determinism test. Reuses the AC-1 (`implements_ac1`),
//! AC-4 (`implements_ac4`), AC-6 (`promote_ac6`), and AC-11 (`sig_ac11`)
//! fixtures and asserts the canonicalised `Implements` edge multiset is
//! bit-identical across three independent `build_workspace` runs over
//! each fixture.
//!
//! Runs live as of Cluster G1 (05_TEST_PLAN.md §7.5 RESOLVED).

#[path = "common/mod.rs"]
mod common;

use sqry_core::graph::unified::CodeGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use std::path::Path;

/// Canonical `(source_qn, target_qn, kind_tag)` multiset of every
/// `Implements` edge in the graph.
fn implements_multiset(graph: &CodeGraph) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (nid, entry) in graph.nodes().iter() {
        let src = entry
            .qualified_name
            .and_then(|id| graph.strings().resolve(id))
            .map(|s| s.as_ref().to_string());
        let Some(src) = src else { continue };
        for edge in graph.edges().edges_from(nid) {
            if matches!(edge.kind, EdgeKind::Implements)
                && let Some(tgt_entry) = graph.nodes().get(edge.target)
                && let Some(tgt) = tgt_entry
                    .qualified_name
                    .and_then(|id| graph.strings().resolve(id))
                    .map(|s| s.as_ref().to_string())
            {
                out.push((src.clone(), tgt, edge.kind.tag().to_string()));
            }
        }
    }
    out.sort();
    out
}

/// AC-12: building the same fixture three times must yield the same
/// `Implements` edge multiset each time. Repeated across the four
/// fixtures listed in `02_DESIGN.md` §10.1 (AC-12 row): ac1, ac4,
/// ac6, ac11.
#[test]
fn ac12_bit_identical_x_runs() {
    let fixtures = [
        "tests/fixtures/go/implements_ac1",
        "tests/fixtures/go/implements_ac4",
        "tests/fixtures/go/promote_ac6",
        "tests/fixtures/go/sig_ac11",
    ];
    for fixture in &fixtures {
        let p = Path::new(fixture);
        let g1 = common::build_workspace(p);
        let g2 = common::build_workspace(p);
        let g3 = common::build_workspace(p);

        let m1 = implements_multiset(&g1);
        let m2 = implements_multiset(&g2);
        let m3 = implements_multiset(&g3);

        assert_eq!(
            m1, m2,
            "{fixture}: Implements multiset must be bit-identical across rebuilds (1 vs 2)",
        );
        assert_eq!(
            m2, m3,
            "{fixture}: Implements multiset must be bit-identical across rebuilds (2 vs 3)",
        );

        assert!(
            !m1.is_empty(),
            "{fixture}: AC-12 fixtures must emit at least one Implements edge",
        );
    }
}
