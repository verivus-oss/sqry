//! `A_cancellation.md` §6 rows 2, 3, 4 — integration / stress / replay
//! deferral markers.
//!
//! Cluster-A IMP-A landed the cancellable executor primitive
//! (`tests/query_cancellation.rs` row 1 +
//! `tests/query_cancellation_property.rs` row 5 +
//! `tests/evaluate_join_cancellation_deferral.rs` row 6 +
//! `sqry-daemon/tests/cargo_features_audit.rs` row 7) plus the
//! wrapper plumbing across `sqry-mcp` and `sqry-daemon`. The three
//! integration / stress / replay rows below require harness work
//! that meaningfully extends Layer-2's scope — a SqryServer
//! instance + a 50k-node fixture (row 2), a daemon-level
//! tokio_metrics stress harness (row 3), and a `--release` build
//! against a 200k-node fixture (row 4).
//!
//! These markers are `#[ignore]`d so `cargo test ... -- --ignored`
//! surfaces them as a documented audit trail. The tracker
//! comments name the design row, the harness work needed, and the
//! production code that already covers the cancellation primitive
//! the test would exercise. Each row's `#[ignore]` is removed when
//! the matching harness lands.

#[test]
#[ignore = "deferred — A_cancellation.md §6 row 2: timeout-then-immediate-call regression. \
            Needs sqry-mcp::SqryServer instance + 50k-node fixture. \
            Cancellation primitive itself is pinned by row 1 in \
            sqry-core/tests/query_cancellation.rs::cancellation_observed_within_ci_latency_budget_after_signal."]
fn row2_timeout_then_immediate_call_regression_marker() {
    // Tracker only: see this test's `#[ignore]` reason for the
    // primary cancellation contract that already passes.
}

#[test]
#[ignore = "deferred — A_cancellation.md §6 row 3: pool depth bounded under N concurrent timeouts. \
            Needs sqry-daemon harness + tokio_metrics-reported blocking pool depth. \
            max_blocking_threads(64) cap is wired in sqry-daemon/src/entrypoint.rs and \
            sqry-mcp/src/main.rs; this row would assert behavioural correctness under load."]
fn row3_blocking_pool_depth_stress_marker() {
    // Tracker only.
}

#[test]
#[ignore = "deferred — A_cancellation.md §6 row 4: maintainer's symptom replay. \
            Needs --release + 200k-node synthetic large-graph fixture matching the \
            kind:function AND name~=/.*_set$/ shape. End-to-end flow is covered by \
            row 1's cancellable-executor primitive plus the wrapper plumbing in \
            sqry-mcp/src/server.rs::execute_tool_with_timeout (deadline → cancel \
            → QueryError::Cancelled → RpcError::deadline_exceeded)."]
fn row4_maintainer_replay_marker() {
    // Tracker only.
}
