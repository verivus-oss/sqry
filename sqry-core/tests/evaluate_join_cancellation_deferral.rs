//! `A_cancellation.md` §6 row 6 — `evaluate_join` cancellation deferral.
//!
//! `evaluate_join` (in `sqry-core/src/query/executor/graph_eval.rs`,
//! reached through `QueryExecutor::execute_join` at `core.rs:667`)
//! has its own per-pair scan loop that does NOT call `evaluate_all`
//! and therefore is **NOT covered** by IMP-A's per-batch
//! cancellation poll. The design (§Open question 2 + §3 deferral
//! note) explicitly tracks this as a follow-up and recommends a
//! parallel per-N polling pass be added when DPA's follow-up
//! cluster lands.
//!
//! This test is `#[ignore]`d by default — it does not assert
//! anything yet — but its existence keeps the deferral auditable:
//! `cargo test ... -- --ignored` surfaces it so reviewers can see
//! the deferral is documented.

#[test]
#[ignore = "deferred — see A_cancellation.md §3 + §Open Q 2 + Hand-offs note. \
            evaluate_join's per-pair scan does not currently observe the \
            CancellationToken; a per-N poll would be a future cluster-A \
            extension."]
fn evaluate_join_cancellation_deferral_marker() {
    // No assertion: the test's purpose is to be discoverable as an
    // ignored row in `cargo test ... -- --ignored` output so the
    // deferral surfaces in audit reports.
    //
    // When the deferral is closed (a per-N poll is added inside
    // `evaluate_join`), this test becomes a real cancel-then-assert
    // contract pin and the `#[ignore]` is removed.
    //
    // Follow-up tracker: `A_cancellation.md` §3 "evaluate_join
    // deferral confirmation" + §Open question 2 + §Hand-offs note.
}
