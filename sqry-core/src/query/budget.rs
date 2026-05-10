//! Per-tool runtime row budget (per `C_budget.md` §§1–6 +
//! `00_contracts.md` §3.CC-1 + §3.CC-2).
//!
//! Runtime backstop for queries that slipped past the static cost
//! gate ([`crate::query::cost_gate`] / Subagent B). The budget caps
//! how many rows the executor may examine inside `evaluate_all`'s
//! hot loop; on overflow the budget trips Subagent A's
//! [`CancellationToken`] so the existing drain / drop pathway fires
//! uniformly. A shared [`CancellationSource`] tag records *which*
//! signal cancelled the token so the wrapper-side downcast can
//! choose between [`BudgetExceeded`] (→ `query_too_broad` with
//! `details.source = "runtime_budget"`) and
//! [`crate::query::error::QueryError::Cancelled`] (→
//! `deadline_exceeded`) deterministically — even when the
//! sequential / parallel evaluator paths interleave with the
//! wrapper deadline drop-guard.
//!
//! [`CancellationToken`]: crate::query::cancellation::CancellationToken

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use thiserror::Error;

/// Documented default row budget per tool invocation.
///
/// 5_000_000 rows is large enough that healthy queries on
/// realistic workspaces never trip it (a worst-case `kind:function`
/// scan across every node in a multi-million-line monorepo is
/// bounded by node-count, not by query shape). It exists to bound
/// the worst-case runtime on a query that bypassed the static cost
/// gate via a coupling rule that turned out to be inadequate
/// (e.g. a `kind:function` coupled regex over a synthetic-graph
/// monorepo with millions of generated function nodes — the
/// maintainer's reported failure mode).
pub const DEFAULT_BUDGET_ROWS: u64 = 5_000_000;

/// Environment-variable name for the global default override.
/// Per-call `budget_rows` MCP parameters (per
/// `C_budget.md` §C5) take precedence over the env-var default.
pub const ENV_TOOL_BUDGET_ROWS: &str = "SQRY_TOOL_BUDGET_ROWS";

/// Default per-row check stride. Trades early-trip precision for
/// `fetch_add` overhead. 256 keeps the per-row overhead under one
/// extra branch + one `Relaxed` load on the cancel-poll path.
pub const DEFAULT_CHECK_STRIDE: u64 = 256;

/// Discriminator for which subsystem first triggered cancellation
/// against a [`QueryBudget`]. Stored as `AtomicU8` so it can be
/// tagged once and read by every Rayon worker observing
/// `cancel.is_cancelled()`. Per `C_budget.md` §3 + Codex iter-1
/// finding 3 fix.
///
/// First-observer-wins: any worker that notices the token is
/// cancelled and finds the source still tagged `None` performs a
/// CAS to install `External`. The CAS makes
/// "deadline-cancel-arrived-while-budget-was-just-overflowing"
/// deterministic — whichever signal raced to mark the source first
/// wins, and every subsequent observer (including Rayon workers
/// running on other cores) reads the same tag and emits the
/// matching typed error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CancellationSource {
    /// Token not yet cancelled (or no source tag has been written
    /// yet — see `evaluate_all`'s `classify_cancel` for the rule
    /// that treats `None` observed alongside `is_cancelled()` as
    /// `External`).
    None = 0,
    /// Budget [`QueryBudget::tick`] overflowed `max_rows` and
    /// called `cancel.cancel()`.
    Budget = 1,
    /// Some other path — the wrapper deadline drop-guard, an
    /// admin `daemon/cancel-tool` future hook, or a parent build
    /// cancellation — flipped the shared token.
    External = 2,
}

impl CancellationSource {
    /// Reverse of `repr(u8)`. Maps unknown values to `None` so a
    /// future variant added on a writer that the reader hasn't
    /// caught up with does not fault.
    #[inline]
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => CancellationSource::Budget,
            2 => CancellationSource::External,
            _ => CancellationSource::None,
        }
    }
}

/// Per-tool runtime budget. Constructed at the
/// [`crate::query::executor::QueryExecutor`] boundary; threaded
/// into [`crate::query::executor::graph_eval::GraphEvalContext`]
/// so `evaluate_all` can sample-and-check it without re-reading
/// env vars per call.
///
/// `Clone` is cheap: the inner counters are `Arc`-shared, so
/// cloning across rayon workers is a refcount bump per field.
#[derive(Debug, Clone)]
pub struct QueryBudget {
    /// Maximum rows the executor may examine before tripping
    /// cancellation. `0` is rejected at construction sites (MCP
    /// boundary + env-var parse).
    pub max_rows: u64,
    /// How many rows have actually been examined. Shared across
    /// rayon worker threads; reset per `evaluate_all` call.
    pub examined: Arc<AtomicU64>,
    /// Shared cancellation token (canonical type per
    /// `00_contracts.md` §3.CC-1). Tripped when
    /// `examined >= max_rows`.
    pub cancel: crate::query::cancellation::CancellationToken,
    /// First-observer-wins source tag. Written exactly once by
    /// the first signal to cancel the shared token (either
    /// [`Self::tick`] on budget overflow or
    /// [`Self::mark_external_cancel`]).
    pub state: Arc<AtomicU8>,
    /// How often (in rows) to compare `examined` against
    /// `max_rows`. Trades early-trip precision for `fetch_add`
    /// overhead.
    pub check_stride: u64,
}

impl QueryBudget {
    /// Construct a fresh budget with `max_rows` and the documented
    /// [`DEFAULT_CHECK_STRIDE`]. The supplied token is the canonical
    /// per-request `CancellationToken` from Subagent A's wrapper —
    /// `tick()` calls `cancel()` on it when the budget trips, so
    /// every clone of the token observes the cancellation through
    /// the same shared `Arc<AtomicBool>`.
    #[must_use]
    pub fn new(max_rows: u64, cancel: crate::query::cancellation::CancellationToken) -> Self {
        Self {
            max_rows,
            examined: Arc::new(AtomicU64::new(0)),
            cancel,
            state: Arc::new(AtomicU8::new(CancellationSource::None as u8)),
            check_stride: DEFAULT_CHECK_STRIDE,
        }
    }

    /// Construct an effectively-unbounded budget (`u64::MAX` rows)
    /// for back-compat callers that have not opted into per-tool
    /// budgeting yet. The cancellation token is still wired so
    /// external cancel signals propagate normally.
    #[must_use]
    pub fn unbounded(cancel: crate::query::cancellation::CancellationToken) -> Self {
        Self::new(u64::MAX, cancel)
    }

    /// Resolve the effective budget for an MCP tool call.
    ///
    /// Priority (per `C_budget.md` §2):
    ///
    /// 1. Per-call `budget_rows` MCP parameter (the caller can
    ///    opt to a tighter or looser bound for diagnostics).
    /// 2. Environment-variable `SQRY_TOOL_BUDGET_ROWS`.
    /// 3. [`DEFAULT_BUDGET_ROWS`].
    ///
    /// `0` is rejected at this boundary by mapping to the
    /// effectively-unbounded variant — a budget of zero would
    /// trip on the first row, which is never the operator
    /// intent. Negative env values are similarly mapped to the
    /// default.
    #[must_use]
    pub fn from_per_call_or_env(
        per_call_budget: Option<u64>,
        cancel: crate::query::cancellation::CancellationToken,
    ) -> Self {
        if let Some(rows) = per_call_budget
            && rows > 0
        {
            return Self::new(rows, cancel);
        }
        let env_rows = std::env::var(ENV_TOOL_BUDGET_ROWS)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0);
        let max_rows = env_rows.unwrap_or(DEFAULT_BUDGET_ROWS);
        Self::new(max_rows, cancel)
    }

    /// Returns true iff the budget has been exceeded. Cheap: one
    /// `Relaxed` load + comparison.
    #[inline]
    #[must_use]
    pub fn exceeded(&self) -> bool {
        self.examined.load(Ordering::Relaxed) >= self.max_rows
    }

    /// Reads the (possibly still `None`) cancellation source tag.
    #[inline]
    #[must_use]
    pub fn source(&self) -> CancellationSource {
        CancellationSource::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Tag the source as `External` IFF it is still `None`. CAS,
    /// safe to call from any worker on observation. No-op if
    /// `Budget` already won the race. Returned bool indicates
    /// whether THIS call won the CAS (used in tests).
    #[inline]
    pub fn mark_external_cancel(&self) -> bool {
        self.state
            .compare_exchange(
                CancellationSource::None as u8,
                CancellationSource::External as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Tag the source as `Budget` IFF it is still `None`.
    #[inline]
    fn mark_budget_cancel(&self) -> bool {
        self.state
            .compare_exchange(
                CancellationSource::None as u8,
                CancellationSource::Budget as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Increment by 1. Returns `Err(BudgetExceeded)` the first
    /// time the post-increment count crosses `max_rows`.
    /// Idempotent on subsequent crosses (so multiple rayon
    /// workers racing past the threshold all see `Err` but the
    /// cancel-once contract holds via `CancellationToken::cancel`'s
    /// internal `AtomicBool`).
    ///
    /// Source-tag invariant: the tag is set to `Budget` BEFORE
    /// `cancel.cancel()` is called. This ordering means any
    /// observer that subsequently reads
    /// `cancel.is_cancelled() == true` is guaranteed to see
    /// `source() != None` — the consumer side of the invariant
    /// lives in `evaluate_all`'s `classify_cancel` block.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when the post-increment
    /// `examined` count meets or exceeds `max_rows`.
    #[inline]
    pub fn tick(&self) -> Result<(), BudgetExceeded> {
        let prev = self.examined.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= self.max_rows {
            // Order matters: stamp source BEFORE flipping the
            // token, so observers that see is_cancelled() also
            // see Budget.
            self.mark_budget_cancel();
            self.cancel.cancel();
            return Err(BudgetExceeded {
                examined: prev + 1,
                limit: self.max_rows,
                predicate_shape: None,
            });
        }
        Ok(())
    }
}

/// Typed signal that a query exceeded its row budget. Surfaced
/// from `evaluate_all` as `anyhow::Error::from(BudgetExceeded { .. })`;
/// the MCP / daemon outer wrappers downcast on it (the same pattern
/// `sqry-mcp/src/server.rs` uses for `RpcError`) and emit the
/// `query_too_broad` envelope variant declared by Subagent B with
/// `details.source = "runtime_budget"`,
/// `details.examined = examined`, `details.limit = limit`.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("query exceeded row budget: examined {examined} rows, limit {limit}")]
pub struct BudgetExceeded {
    /// Rows examined before the budget tripped. Surfaced as
    /// `details.examined` in the wire envelope.
    pub examined: u64,
    /// Configured row budget at the time of the trip. Surfaced as
    /// `details.limit` in the wire envelope.
    pub limit: u64,
    /// Sanitised AST shape (`Expr::shape_summary`) of the offending
    /// query, ≤256 bytes, no values / paths / regex patterns. Cluster-C
    /// iter-2: surfaced as `details.predicate_shape` on the runtime-
    /// budget envelope so MCP clients see the same comparable shape
    /// the cluster-B static-estimate envelope already exposes
    /// (codex iter-1 review).
    ///
    /// `None` only when the executor surfaces budget exhaustion before
    /// the finalize step has run (e.g. the cancellable wrappers' own
    /// downcast). The envelope serializes `None` as JSON null.
    pub predicate_shape: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::cancellation::CancellationToken;

    #[test]
    fn tick_below_max_returns_ok() {
        let token = CancellationToken::new();
        let budget = QueryBudget::new(10, token.clone());
        for _ in 0..9 {
            budget.tick().expect("first 9 ticks must succeed");
        }
        assert!(!budget.exceeded(), "9 ticks must not exceed budget of 10");
        assert!(!token.is_cancelled(), "token must remain uncancelled");
        assert_eq!(budget.source(), CancellationSource::None);
    }

    #[test]
    fn tick_at_max_trips_cancel_and_stamps_budget_source() {
        let token = CancellationToken::new();
        let budget = QueryBudget::new(3, token.clone());
        budget.tick().expect("tick 1 ok");
        budget.tick().expect("tick 2 ok");
        let err = budget.tick().expect_err("tick 3 must trip");
        assert_eq!(err.examined, 3);
        assert_eq!(err.limit, 3);
        assert!(token.is_cancelled(), "tick must flip the token");
        assert_eq!(
            budget.source(),
            CancellationSource::Budget,
            "budget overflow must stamp source = Budget"
        );
    }

    #[test]
    fn external_cancel_first_blocks_budget_tag() {
        let token = CancellationToken::new();
        let budget = QueryBudget::new(3, token.clone());
        // External arrives first.
        assert!(budget.mark_external_cancel(), "External wins CAS");
        // Subsequent budget overflow MUST NOT overwrite the tag.
        budget.tick().expect("first tick ok");
        budget.tick().expect("second tick ok");
        let _ = budget.tick();
        assert_eq!(
            budget.source(),
            CancellationSource::External,
            "external-first must keep tag = External even after budget overflow"
        );
    }

    #[test]
    fn budget_cancel_first_blocks_external_tag() {
        let token = CancellationToken::new();
        let budget = QueryBudget::new(2, token.clone());
        budget.tick().expect("first tick ok");
        let _ = budget.tick(); // trips
        assert_eq!(budget.source(), CancellationSource::Budget);
        // External arrives after budget — CAS fails, tag stays Budget.
        assert!(
            !budget.mark_external_cancel(),
            "external-second CAS must fail"
        );
        assert_eq!(budget.source(), CancellationSource::Budget);
    }

    #[test]
    fn from_per_call_prefers_per_call_value_over_env() {
        // Use a unique env-var snapshot so concurrent tests
        // don't interfere — but here the per-call value should
        // win regardless.
        // SAFETY: setting an env var is a one-process-wide write;
        // single-threaded test scope.
        unsafe {
            std::env::set_var(ENV_TOOL_BUDGET_ROWS, "999");
        }
        let token = CancellationToken::new();
        let budget = QueryBudget::from_per_call_or_env(Some(42), token);
        assert_eq!(budget.max_rows, 42, "per-call value must override env var");
        unsafe {
            std::env::remove_var(ENV_TOOL_BUDGET_ROWS);
        }
    }

    #[test]
    fn from_per_call_zero_falls_back_to_default() {
        unsafe {
            std::env::remove_var(ENV_TOOL_BUDGET_ROWS);
        }
        let token = CancellationToken::new();
        let budget = QueryBudget::from_per_call_or_env(Some(0), token);
        assert_eq!(
            budget.max_rows, DEFAULT_BUDGET_ROWS,
            "per-call zero must map to the default rather than trip immediately"
        );
    }

    #[test]
    fn from_per_call_none_uses_default_when_env_unset() {
        unsafe {
            std::env::remove_var(ENV_TOOL_BUDGET_ROWS);
        }
        let token = CancellationToken::new();
        let budget = QueryBudget::from_per_call_or_env(None, token);
        assert_eq!(budget.max_rows, DEFAULT_BUDGET_ROWS);
    }

    #[test]
    fn unbounded_budget_never_trips_on_realistic_iteration_count() {
        let token = CancellationToken::new();
        let budget = QueryBudget::unbounded(token.clone());
        // 1k ticks against a u64::MAX cap is nowhere near the
        // overflow boundary — the unbounded variant must not
        // accidentally trip.
        for _ in 0..1_000 {
            budget.tick().expect("unbounded must not trip");
        }
        assert!(!token.is_cancelled());
    }
}
