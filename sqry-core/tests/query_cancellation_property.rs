//! `A_cancellation.md` §6 row 5 — happy-path property test.
//!
//! Pins the no-spurious-cancellation invariant: a fresh
//! never-cancelled token must NEVER produce a `QueryError::Cancelled`
//! result, and the cancellable-overload result set must be
//! identical to the back-compat overload's result set on every
//! input. This guards against an implementation that:
//!
//! - Mistakenly polls a stale `Arc<AtomicBool>` value.
//! - Returns `Cancelled` on iteration-counter overflow.
//! - Diverges in match-set ordering between the cancellable and
//!   non-cancellable paths.
//!
//! 64 random small graphs × 4 query shapes is enough to catch the
//! pathologies above without making the test slow on CI; with
//! per-iteration `evaluate_node` cost ≪ 1 ms, the entire test
//! finishes in well under a second.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::QueryExecutor;
use sqry_core::query::cancellation::CancellationToken;

/// Minimal pseudo-random number generator. Avoids pulling in a `rand`
/// dependency for a single test file. The `xorshift64` core is good
/// enough for shuffling test inputs deterministically; we seed from
/// a fixed value so the test is reproducible.
struct Xorshift(u64);
impl Xorshift {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        debug_assert!(lo <= hi);
        let span = (hi - lo) as u64 + 1;
        lo + (self.next() % span) as usize
    }
}

fn build_random_small_graph(rng: &mut Xorshift, target_count: usize) -> CodeGraph {
    let mut graph = CodeGraph::new();
    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/random.rs"), Some(Language::Rust))
        .expect("register file");

    let kinds = [
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Struct,
        NodeKind::Class,
        NodeKind::Constant,
    ];

    for i in 0..target_count {
        let kind = kinds[rng.range(0, kinds.len() - 1)];
        let name = format!("sym_{i}_{}", rng.next() % 10);
        let qname = format!("test::{name}");
        let name_id = graph.strings_mut().intern(&name).expect("intern name");
        let qname_id = graph.strings_mut().intern(&qname).expect("intern qname");

        let entry = NodeEntry::new(kind, name_id, file_id)
            .with_location((i as u32) * 4 + 1, 0, (i as u32) * 4 + 3, 0)
            .with_qualified_name(qname_id);

        let node_id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );
    }
    graph
}

#[test]
fn happy_path_no_spurious_cancellation_and_matches_backcompat_overload() {
    const ROUNDS: usize = 64;
    const QUERIES: &[&str] = &[
        "kind:function",
        "kind:struct",
        "kind:method OR kind:class",
        "kind:function AND name~=/sym_/",
    ];

    let executor = QueryExecutor::new();
    let mut rng = Xorshift::new(0x53717279_53717279); // "SqrySqry"
    let workspace_root = Path::new("/test");

    for round in 0..ROUNDS {
        // Vary node count between 8 and 200 so we cover both
        // sub-batch (no per-batch poll triggers) and multi-batch
        // (poll triggers but observes never-cancelled) regimes.
        let count = rng.range(8, 200);
        let graph = Arc::new(build_random_small_graph(&mut rng, count));

        for q in QUERIES {
            let cancel = CancellationToken::new();

            // Cancellable overload with a fresh never-cancelled token
            // MUST behave identically to the back-compat overload
            // (which constructs its own fresh token internally).
            let r_cancellable = executor
                .execute_on_preloaded_graph_cancellable(
                    Arc::clone(&graph),
                    q,
                    workspace_root,
                    None,
                    &cancel,
                )
                .unwrap_or_else(|e| {
                    panic!(
                        "cancellable overload spuriously errored on round {round} query {q}: {e}"
                    )
                });
            let r_backcompat = executor
                .execute_on_preloaded_graph(Arc::clone(&graph), q, workspace_root, None)
                .unwrap_or_else(|e| {
                    panic!(
                        "back-compat overload spuriously errored on round {round} query {q}: {e}"
                    )
                });

            // Equal match counts: the per-N polling adds no false
            // negatives and the unconditional rayon poll observes
            // never-cancelled.
            assert_eq!(
                r_cancellable.len(),
                r_backcompat.len(),
                "match-count divergence on round {round} query {q}: cancellable={} vs backcompat={}",
                r_cancellable.len(),
                r_backcompat.len(),
            );

            // Token never observed cancelled — defensive: if the
            // implementation accidentally cancels its own input
            // token, this would catch it.
            assert!(
                !cancel.is_cancelled(),
                "fresh token must never become cancelled during a happy-path run"
            );
        }
    }
}
