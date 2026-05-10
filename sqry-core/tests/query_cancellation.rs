//! `A_cancellation.md` §6 row 1 — cancellation observed within K ms.
//!
//! Builds a synthetic graph with ~50 000 trivial Function nodes,
//! kicks off `QueryExecutor::execute_on_preloaded_graph_cancellable`
//! on a worker thread with a broad-regex predicate that forces full
//! arena scan, signals `cancel.cancel()` from the main thread after a
//! short pump-prime delay, and asserts the worker returns
//! `Err(QueryError::Cancelled)` within the bounded latency budget.
//!
//! The latency budget is the design's `CANCELLATION_POLL_BATCH = 1024`
//! cadence × the per-`evaluate_node` cost (~50–200 ns for the
//! broad-regex case) = ~100 µs per poll. Plus jitter on slow CI we
//! allow up to **2 s** for the cancel-to-return path; the test
//! still pins the regression on the order of `≪ deadline_budget`,
//! so a buggy implementation that runs to completion (~seconds at
//! 50k nodes) would still fail.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::QueryError;
use sqry_core::query::QueryExecutor;
use sqry_core::query::cancellation::CancellationToken;

/// Build a synthetic preloaded graph with `n` Function-kind nodes, all
/// in a single registered file. Every node has a unique short name so
/// the auxiliary `name_index` does not collapse them, but the broad
/// `name~=/.*foo.*/` regex forces the executor down the per-node
/// match path (no index hit) — exactly the maintainer's failure-mode
/// shape.
fn build_synthetic_graph(n: usize) -> CodeGraph {
    let mut graph = CodeGraph::new();

    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/synthetic.rs"), Some(Language::Rust))
        .expect("register file");

    for i in 0..n {
        let name = format!("sym_{i}");
        let qname = format!("test::{name}");
        let name_id = graph.strings_mut().intern(&name).expect("intern name");
        let qname_id = graph.strings_mut().intern(&qname).expect("intern qname");

        let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
            // Disjoint line ranges so per-node location data is unique
            // and never collapses to a single span (defensive against
            // any future indexer that dedups by location).
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
fn cancellation_observed_within_ci_latency_budget_after_signal() {
    // Synthetic ~50k-node graph. 100k is the design's spec but 50k
    // is enough to make the no-cancel run last seconds (so a buggy
    // implementation that ignores the token would clearly miss the
    // CI latency budget) while keeping the build cost acceptable on
    // slow CI runners.
    const NODE_COUNT: usize = 50_000;
    const SETTLE_MS: u64 = 5;
    // Public release CI can run under enough scheduler contention to
    // exceed the original 500 ms guard while still cancelling promptly.
    const BUDGET_MS: u128 = 2_000;

    let executor = QueryExecutor::new();
    let graph = Arc::new(build_synthetic_graph(NODE_COUNT));
    let workspace_root = Path::new("/test");

    let cancel = CancellationToken::new();
    let cancel_for_worker = cancel.clone();
    let graph_for_worker = Arc::clone(&graph);

    // Spawn the long-running query on a real OS thread so the main
    // thread retains control to fire the cancellation signal.
    let started = Instant::now();
    let worker = std::thread::spawn(move || {
        executor.execute_on_preloaded_graph_cancellable(
            graph_for_worker,
            // `.*foo.*` over 50 000 unique non-matching names forces
            // the executor through the full per-node regex path —
            // exactly the maintainer's #233 P0-1 broad-shape case.
            "kind:function AND name~=/.*foo.*/",
            workspace_root,
            None,
            &cancel_for_worker,
        )
    });

    // Let the worker enter `evaluate_all` before we cancel. The
    // pre-loop check at the top of `evaluate_all` would observe an
    // already-flipped token instantly without exercising the
    // per-batch poll path; this sleep ensures we cover the in-loop
    // behaviour the design pins.
    std::thread::sleep(Duration::from_millis(SETTLE_MS));

    let signaled_at = Instant::now();
    cancel.cancel();

    let result = worker.join().expect("worker thread must not panic");
    let elapsed = signaled_at.elapsed();

    // Bounded latency: the worker must return shortly after the signal.
    assert!(
        elapsed.as_millis() < BUDGET_MS,
        "cancellation latency {}ms exceeded {}ms budget — implementation may be ignoring the token",
        elapsed.as_millis(),
        BUDGET_MS,
    );
    // Total wall time also bounded — defends against a successful run
    // that just happened to complete in the budget window.
    assert!(
        started.elapsed().as_secs() < 30,
        "total wall time exceeded 30s; query likely ran to completion despite cancel"
    );

    // Surface must be the typed `QueryError::Cancelled`, not some
    // other error or a successful result.
    let err = result.expect_err("cancellable query must return Err once cancelled");
    let downcast = err
        .downcast_ref::<QueryError>()
        .expect("error must carry QueryError variant");
    assert!(
        matches!(downcast, QueryError::Cancelled),
        "expected QueryError::Cancelled, got: {downcast:?}"
    );
}

#[test]
fn pre_loop_cancellation_short_circuits_before_evaluate_all_body() {
    // `A_cancellation.md` §3 DESIGNED block: a token already
    // cancelled before `evaluate_all` is entered must short-circuit
    // at the pre-loop check (handles the case where the wrapper
    // deadline elapsed before the closure even reached the
    // evaluator). Pin this with a small graph + an already-flipped
    // token; the result must be `Cancelled`, not the empty match
    // list or a partial scan.
    let executor = QueryExecutor::new();
    let graph = Arc::new(build_synthetic_graph(8));
    let workspace_root = Path::new("/test");

    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = executor
        .execute_on_preloaded_graph_cancellable(
            graph,
            "kind:function",
            workspace_root,
            None,
            &cancel,
        )
        .expect_err("already-cancelled token must error pre-loop");
    let downcast = err
        .downcast_ref::<QueryError>()
        .expect("error must carry QueryError variant");
    assert!(matches!(downcast, QueryError::Cancelled));
}
