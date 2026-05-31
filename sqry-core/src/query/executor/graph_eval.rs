//! Graph-native predicate evaluation.
//!
//! This module provides graph-native predicate evaluation for queries against
//! `CodeGraph`, bypassing the legacy index path.
//!
//! # Design (v6 - CodeGraph-Native Query Executor)
//!
//! Key semantics preserved from the legacy index path:
//! - `name:` uses `segments_match` for qualified name suffix matching
//! - `kind:`, `lang:`, `visibility:`, `scope.*` are **case-sensitive**
//! - `imports:` per-node — matches a node iff its own outgoing `Imports`
//!   edge target/alias/wildcard matches, or it is an `Import` node whose
//!   text matches. Aligned with `sqry-db::queries::ImportsQuery` per the
//!   Phase N "Unified Surface Contract" (planner-IR is canonical, all
//!   transports mirror it). The previous file-scoped semantic was retired
//!   in DB15 to remove the cross-engine divergence flagged by Codex's
//!   DB14 review.
//! - `references:` includes `References` + `Calls` + `Imports` + `FfiCall` edges
//! - `callers:` checks OUTGOING edges (find nodes that call X)
//! - `callees:` checks INCOMING edges (find nodes called by X)
//! - Relation predicates use `segments_match` for qualified names
//!
//! # `returns:` evaluation
//!
//! `returns:<TypeName>` is evaluated edge-based, mirroring the planner's
//! `node_returns_type` (see `sqry_db::planner::execute::node_returns_type`):
//! the candidate node's outgoing edges are scanned for
//! `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }`, and the
//! target node's interned primary name is byte-exact-compared (case-sensitive)
//! to the predicate value. Substring/regex semantics are out of scope and may
//! land later as a distinct `returns~` operator. The legacy
//! `NodeEntry.signature.contains(...)` substring path was retired in favour of
//! this contract because it produced false positives whenever the requested
//! type name occurred anywhere in the function signature text.
//!
//! # Limitations (v1)
//!
//! The following predicates are **NOT SUPPORTED** in graph backend v1:
//! - Plugin fields - requires metadata `HashMap` not in `NodeEntry`
//! - Numeric operators - requires metadata values
//!
//! **Supported boolean predicates**: `async:true`, `static:true`

use crate::graph::unified::FileId;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::kind::TypeOfContext;
use crate::graph::unified::edge::{EdgeKind, StoreEdgeRef};
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::resolution::{
    canonicalize_graph_qualified_name, display_graph_qualified_name,
};
use crate::graph::unified::storage::arena::NodeEntry;
use crate::plugin::PluginManager;
use crate::query::name_matching::segments_match;
use crate::query::regex_cache::{CompiledRegex, get_or_compile_regex};
use crate::query::types::{Condition, Expr, JoinEdgeKind, JoinExpr, Operator, Value};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Try `re.is_match(text)`, logging a warning and returning `false` if the
/// regex engine hits its backtrack limit.  This prevents silent failures
/// while keeping the predicate-evaluation chain infallible.
fn regex_is_match(re: &CompiledRegex, text: &str) -> bool {
    match re.is_match(text) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("regex match aborted (backtrack limit?): {e}");
            false
        }
    }
}
use std::sync::Arc;

/// Cache of precomputed subquery result sets, keyed by `(span.start, span.end)`.
///
/// Subquery expressions from the AST are evaluated once against all graph nodes
/// and their results cached here. During per-node evaluation, matchers look up
/// the cache instead of re-evaluating the full graph. This reduces subquery cost
/// from O(N^2) to O(N) where N is the number of graph nodes.
type SubqueryCache = HashMap<(usize, usize), Arc<HashSet<NodeId>>>;

/// Context for graph-native predicate evaluation.
///
/// Encapsulates all dependencies needed for evaluating predicates against
/// a `CodeGraph`. The context includes precomputed caches for performance.
pub struct GraphEvalContext<'a> {
    /// Reference to the code graph
    pub graph: &'a CodeGraph,
    /// Plugin manager for detecting plugin fields
    pub plugin_manager: &'a PluginManager,
    /// Workspace root for relative path resolution in `path:` predicates
    pub workspace_root: Option<&'a Path>,
    /// If true, disable parallel execution
    pub disable_parallel: bool,
    /// Precomputed subquery result sets, keyed by `(span.start, span.end)`.
    /// Populated by `precompute_subqueries()` before the per-node evaluation loop.
    pub subquery_cache: SubqueryCache,
    /// Cooperative cancellation token polled by [`evaluate_all`]
    /// at every [`CANCELLATION_POLL_BATCH`]-node boundary in the
    /// sequential path and on every iteration in the rayon path
    /// (per `A_cancellation.md` §3 + `00_contracts.md` §3.CC-1).
    ///
    /// Never-cancelled by default — non-cancellable callers
    /// (`QueryExecutor::execute_on_*`) construct a fresh token
    /// trampoline. The MCP / IPC dispatch wrappers
    /// (`A_cancellation.md` §2) install a per-request token whose
    /// `cancel()` is called when the per-tool deadline elapses, so
    /// the in-flight `spawn_blocking` thread observes the signal on
    /// its next pass-boundary poll and short-circuits with
    /// [`crate::query::error::QueryError::Cancelled`].
    pub cancellation: crate::query::cancellation::CancellationToken,
    /// Per-tool runtime row budget (per `C_budget.md` §§1–3 +
    /// `00_contracts.md` §3.CC-2). [`evaluate_all`] calls
    /// `budget.tick()` on every per-node iteration; on overflow
    /// `tick()` writes a [`crate::query::budget::CancellationSource::Budget`]
    /// tag to the budget's shared atomic state and flips the
    /// shared cancellation token. The same token is the one in
    /// the `cancellation` field above — they are the same
    /// `Arc<AtomicBool>`, so the cooperative-cancellation poll
    /// path and the budget-overflow path observe the same flag
    /// regardless of which side cancelled first.
    ///
    /// Default budget is unbounded (`u64::MAX`), preserving
    /// call-site back-compat for LSP / CLI evaluators that have
    /// no per-tool budget concept yet.
    pub budget: crate::query::budget::QueryBudget,
}

impl<'a> GraphEvalContext<'a> {
    /// Creates a new evaluation context.
    #[must_use]
    pub fn new(graph: &'a CodeGraph, plugin_manager: &'a PluginManager) -> Self {
        // Single never-cancelled token shared between the
        // cancellation field and the budget. The budget defaults
        // to unbounded so LSP / CLI evaluators never trip it.
        let cancellation = crate::query::cancellation::CancellationToken::new();
        let budget = crate::query::budget::QueryBudget::unbounded(cancellation.clone());
        Self {
            graph,
            plugin_manager,
            workspace_root: None,
            disable_parallel: false,
            subquery_cache: HashMap::new(),
            cancellation,
            budget,
        }
    }

    /// Sets the workspace root for relative path resolution.
    #[must_use]
    pub fn with_workspace_root(mut self, root: &'a Path) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Disables parallel execution.
    #[must_use]
    pub fn with_parallel_disabled(mut self, disabled: bool) -> Self {
        self.disable_parallel = disabled;
        self
    }

    /// Installs the cooperative cancellation token polled by
    /// [`evaluate_all`].
    ///
    /// `A_cancellation.md` §3 + `00_contracts.md` §3.CC-1: the token
    /// is `Arc<AtomicBool>`-backed (re-exported from the build
    /// pipeline), so cloning into the context is one Arc bump and
    /// every clone observes the same flag.
    #[must_use]
    pub fn with_cancellation(
        mut self,
        token: crate::query::cancellation::CancellationToken,
    ) -> Self {
        // Cancellation token AND the budget's internal token must
        // be the same `Arc<AtomicBool>` so a budget overflow
        // observed by `evaluate_all`'s poll path classifies
        // through the budget's source tag (and vice versa). Per
        // `C_budget.md` §3 + `00_contracts.md` §3.CC-1.
        self.cancellation = token.clone();
        // Replace the budget with one that wraps the new token,
        // preserving the previous `max_rows` configuration.
        let max_rows = self.budget.max_rows;
        self.budget = crate::query::budget::QueryBudget::new(max_rows, token);
        self
    }

    /// Installs the per-tool row budget polled by [`evaluate_all`]
    /// (per `C_budget.md` §3 + `00_contracts.md` §3.CC-2). The
    /// supplied `QueryBudget` MUST share the same
    /// [`crate::query::cancellation::CancellationToken`] as
    /// [`Self::with_cancellation`] — `with_budget` overwrites the
    /// `cancellation` field with the budget's token to enforce
    /// this invariant.
    #[must_use]
    pub fn with_budget(mut self, budget: crate::query::budget::QueryBudget) -> Self {
        self.cancellation = budget.cancel.clone();
        self.budget = budget;
        self
    }

    /// Precomputes all subquery result sets from the expression tree.
    ///
    /// This must be called before `evaluate_all` to avoid O(N^2) behavior
    /// where each subquery is re-evaluated for every candidate node.
    ///
    /// # Errors
    ///
    /// Returns an error if subquery evaluation fails.
    pub fn precompute_subqueries(&mut self, expr: &Expr) -> Result<()> {
        let mut subquery_exprs = Vec::new();
        collect_subquery_exprs(expr, &mut subquery_exprs);

        for (span_key, inner_expr) in subquery_exprs {
            if !self.subquery_cache.contains_key(&span_key) {
                let result_set = evaluate_subquery(self, inner_expr)?;
                self.subquery_cache.insert(span_key, Arc::new(result_set));
            }
        }
        Ok(())
    }
}

/// Collects all `Value::Subquery(inner)` expressions from the AST for precomputation.
///
/// Returns `(span_key, &Expr)` pairs where `span_key` is `(span.start, span.end)`.
///
/// Uses **post-order** traversal: nested (inner) subqueries appear before their
/// enclosing (outer) subqueries. This ensures `precompute_subqueries()` evaluates
/// dependencies first, so outer subquery evaluation can find inner results in the cache.
fn collect_subquery_exprs<'a>(expr: &'a Expr, out: &mut Vec<((usize, usize), &'a Expr)>) {
    match expr {
        Expr::Condition(cond) => {
            if let Value::Subquery(inner) = &cond.value {
                // Post-order: recurse into nested subqueries FIRST
                collect_subquery_exprs(inner, out);
                // Then record this (outer) subquery
                out.push(((cond.span.start, cond.span.end), inner));
            }
        }
        Expr::And(operands) | Expr::Or(operands) => {
            for op in operands {
                collect_subquery_exprs(op, out);
            }
        }
        Expr::Not(inner) => collect_subquery_exprs(inner, out),
        Expr::Join(join) => {
            collect_subquery_exprs(&join.left, out);
            collect_subquery_exprs(&join.right, out);
        }
    }
}

/// Cooperative-cancellation polling cadence for [`evaluate_all`]'s
/// sequential path: one [`CancellationToken::is_cancelled`] atomic
/// load per `CANCELLATION_POLL_BATCH` node evaluations.
///
/// `A_cancellation.md` §3 — quantitative motivation:
/// - `evaluate_node` for the canonical broad-regex case
///   (`name~=/.*_set$/`) costs 50–200 ns per node on x86 (cached
///   regex + a single short-string match). One `Acquire` atomic load
///   is 1–3 ns uncontended.
/// - With a batch of 1024, the per-iteration amortised poll cost is
///   ~3 ns / 1024 ≈ 0.003 ns ≪ 0.01% of predicate cost.
/// - Wall-clock per-batch deadline observation latency: 100 ns × 1024
///   ≈ 100 µs — comfortably below the 60 s tool deadline budget.
/// - Smaller (e.g. 1) → 3% per-iteration overhead. Larger (e.g.
///   100 000) → 10 ms cancel latency, fine for deadlines but bad for
///   future admin-cancel IPCs.
///
/// `1024` is the conservative choice. The unit test
/// `query_cancellation` in `sqry-core/tests/` measures actual
/// cancel-to-return latency; the value can be tightened to 32–256 or
/// loosened to 4096 post-implementation if the benchmark shows
/// headroom.
///
/// [`CancellationToken::is_cancelled`]: crate::query::cancellation::CancellationToken::is_cancelled
pub const CANCELLATION_POLL_BATCH: usize = 1024;

/// Evaluates query against all nodes, returning matching `NodeIds`.
///
/// # Errors
///
/// Returns an error if predicate evaluation fails (e.g., unsupported
/// predicates) or if the context's cancellation token is observed
/// cancelled (returns
/// [`crate::query::error::QueryError::Cancelled`] wrapped in
/// `anyhow::Error`).
pub fn evaluate_all(ctx: &mut GraphEvalContext, expr: &Expr) -> Result<Vec<NodeId>> {
    // Cluster-A iter-2 BLOCKER 2: poll the cancellation token BEFORE
    // `precompute_subqueries`. Subquery precomputation can recurse
    // through `evaluate_all` for arbitrary relation chains and the
    // existing per-batch poll fires only inside the scan loop — an
    // already-cancelled or deadline-cancelled request would otherwise
    // run an unbounded subquery scan before observing cancellation
    // (codex iter-1 review).
    //
    // Cluster-C iter-2 BLOCKER 1: also CAS-tag External here so the
    // wire envelope reflects the deadline cancel (rather than getting
    // reclassified as `runtime_budget` if a tick() ran later).
    if ctx.budget.cancel.is_cancelled() {
        ctx.budget.mark_external_cancel();
        return Err(crate::query::QueryError::Cancelled.into());
    }
    // Precompute all subquery result sets before the per-node evaluation loop.
    // Subquery scans recurse through `evaluate_all`, so the budget counter
    // covers them automatically (per `C_budget.md` §3).
    ctx.precompute_subqueries(expr)?;

    let arena = ctx.graph.nodes();

    // Create recursion guard
    let recursion_limits = crate::config::RecursionLimits::load_or_default()?;
    let expr_depth = recursion_limits.effective_expr_depth()?;
    let mut guard = crate::query::security::RecursionGuard::new(expr_depth)?;

    // Per-scan tracing span (per `C_budget.md` §C7). Fields are
    // populated on close so observability sees the final count on
    // every exit path (success, budget-exceeded, recursion error,
    // panic).
    let _span = tracing::info_span!(
        "graph_eval.evaluate_all",
        budget_rows = ctx.budget.max_rows,
        examined = tracing::field::Empty,
        matched = tracing::field::Empty,
        budget_exceeded = tracing::field::Empty,
    )
    .entered();

    // Pre-loop cancellation check (`A_cancellation.md` §3 DESIGNED
    // block). Handles the case where the wrapper deadline has
    // already fired before this evaluator was even reached — e.g.
    // when a parse-cache miss took the entire deadline budget.
    //
    // Cluster-C iter-2 BLOCKER 1: when an external observer flipped
    // the cancel token but never CAS-tagged the budget source, mark
    // External NOW so a subsequent `tick()` overflow cannot CAS
    // `None → Budget` and reclassify a deadline-cancellation as a
    // runtime-budget rejection on the wire.
    if ctx.cancellation.is_cancelled() {
        ctx.budget.mark_external_cancel();
        return finalize_span_and_return(ctx, Err(classify_cancel(&ctx.budget)), expr);
    }

    let result: Result<Vec<NodeId>> = if ctx.disable_parallel {
        // Sequential evaluation with per-CANCELLATION_POLL_BATCH
        // cooperative-cancellation poll. Budget tick fires on every
        // node iteration. On an externally-cancelled token the
        // typed error is chosen by the budget's source-tag (per
        // `C_budget.md` §3 Finding 3 fix).
        let mut matches = Vec::new();
        let mut since_check: usize = 0;
        let mut bail: Option<anyhow::Error> = None;
        for (id, entry) in arena.iter() {
            // Skip Phase 4c-prime unified-away losers — they remain in
            // the arena as inert duplicates so CSR row_ptr sizing stays
            // stable, but publish-visible query evaluation must not
            // surface them (Gate 0d iter-1 blocker). See
            // `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            since_check += 1;
            if since_check >= CANCELLATION_POLL_BATCH {
                since_check = 0;
                if ctx.cancellation.is_cancelled() {
                    // Cluster-C iter-2 BLOCKER 1: ensure External
                    // wins the source-tag CAS race when the wrapper
                    // flipped the token but didn't mark the budget.
                    // No-op when Budget was already CAS'd by a
                    // concurrent worker's tick().
                    ctx.budget.mark_external_cancel();
                    bail = Some(classify_cancel(&ctx.budget));
                    break;
                }
            }
            // Budget tick — increments shared counter, trips
            // cancel on overflow. On overflow `tick()` has already
            // stamped source = Budget before returning, so a
            // concurrent observer sees the matching tag.
            if ctx.budget.tick().is_err() {
                bail = Some(classify_cancel(&ctx.budget));
                break;
            }
            match evaluate_node(ctx, id, expr, &mut guard) {
                Ok(true) => matches.push(id),
                Ok(false) => {}
                Err(e) => {
                    bail = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = bail {
            Err(e)
        } else {
            Ok(matches)
        }
    } else {
        // Parallel evaluation - each thread needs its own guard
        use rayon::prelude::*;

        let node_ids: Vec<_> = arena
            .iter()
            .filter(|(_id, entry)| !entry.is_unified_loser())
            .map(|(id, _)| id)
            .collect();

        // Clone the budget for the rayon workers (cheap — every
        // field is `Arc`-shared). All workers observe the same
        // `examined` counter and the same source tag.
        let budget = ctx.budget.clone();
        let results: Vec<Result<Option<NodeId>>> = node_ids
            .into_par_iter()
            .map(|id| {
                // Once cancellation is observed, every subsequent
                // worker funnels through `classify_cancel` for a
                // deterministic typed error matching the source
                // tag. Per `C_budget.md` §3 Finding 3 fix.
                if budget.cancel.is_cancelled() {
                    // Cluster-C iter-2 BLOCKER 1: same as the
                    // sequential path — mark External BEFORE
                    // classify_cancel so a subsequent tick() can't
                    // reclassify a deadline-cancellation as
                    // runtime_budget on the wire.
                    budget.mark_external_cancel();
                    return Err(classify_cancel(&budget));
                }
                if budget.tick().is_err() {
                    return Err(classify_cancel(&budget));
                }
                let mut thread_guard = crate::query::security::RecursionGuard::new(expr_depth)?;
                evaluate_node(ctx, id, expr, &mut thread_guard)
                    .map(|m| if m { Some(id) } else { None })
            })
            .collect();

        // First-error semantics: collect propagates the first Err
        // in result-vector order. Because every worker that
        // observed cancellation funneled through `classify_cancel`,
        // the emitted variant is consistent with the source tag.
        let mut matches = Vec::new();
        let mut first_err: Option<anyhow::Error> = None;
        for result in results {
            match result {
                Ok(Some(id)) => matches.push(id),
                Ok(None) => {}
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if let Some(e) = first_err {
            Err(e)
        } else {
            Ok(matches)
        }
    };

    finalize_span_and_return(ctx, result, expr)
}

/// Convert an observed cancellation into the typed error matching
/// the budget's source tag. Single source of truth for the
/// sequential and parallel paths so the wrapper-side downcast
/// always sees the same variant for the same cause.
///
/// Source-tag classification rules (per `C_budget.md` §3):
///
/// - `Budget` → `BudgetExceeded { examined, limit }` — wrapper
///   emits `query_too_broad` with `details.source = "runtime_budget"`.
/// - `External` → `QueryError::Cancelled` — wrapper emits
///   `deadline_exceeded` (per A §4 / CC-1).
/// - `None` → can only occur if a worker observes
///   `is_cancelled()` before the cancelling thread has finished
///   its source-tag write. Treated as `External` to preserve the
///   deadline-cancel envelope.
fn classify_cancel(budget: &crate::query::budget::QueryBudget) -> anyhow::Error {
    match budget.source() {
        crate::query::budget::CancellationSource::Budget => {
            anyhow::Error::from(crate::query::budget::BudgetExceeded {
                examined: budget.examined.load(std::sync::atomic::Ordering::Relaxed),
                limit: budget.max_rows,
                predicate_shape: None,
            })
        }
        crate::query::budget::CancellationSource::External
        | crate::query::budget::CancellationSource::None => {
            anyhow::Error::from(crate::query::error::QueryError::Cancelled)
        }
    }
}

/// Populate the per-scan tracing span fields, emit the
/// budget-exceeded WARN log when relevant, and return the result.
///
/// Per `C_budget.md` §4 the WARN log on budget exceedance carries:
/// the redacted predicate shape (`expr.shape_summary()`), the
/// `examined` count, and the configured `budget_rows`. The
/// predicate shape is bounded ≤256 bytes and never contains
/// values, paths, or regex patterns — operators can grep for
/// canonical shapes without exposing PII.
fn finalize_span_and_return(
    ctx: &GraphEvalContext,
    result: Result<Vec<NodeId>>,
    expr: &Expr,
) -> Result<Vec<NodeId>> {
    let examined = ctx
        .budget
        .examined
        .load(std::sync::atomic::Ordering::Relaxed);
    let span = tracing::Span::current();
    span.record("examined", examined);
    match result {
        Ok(m) => {
            span.record("matched", m.len() as u64);
            span.record("budget_exceeded", false);
            Ok(m)
        }
        Err(e)
            if e.downcast_ref::<crate::query::budget::BudgetExceeded>()
                .is_some() =>
        {
            span.record("matched", 0u64);
            span.record("budget_exceeded", true);
            let shape = expr.shape_summary();
            tracing::warn!(
                examined,
                budget = ctx.budget.max_rows,
                predicate = %shape,
                "query exceeded row budget — cancellation triggered"
            );
            // Cluster-C iter-2: attach the sanitised predicate_shape
            // to the BudgetExceeded payload so the wrapper-side
            // runtime_budget envelope can surface it (codex iter-1
            // review). `unwrap` is safe — the matches arm above
            // confirmed the downcast is `Some`.
            let mut budget_err = e
                .downcast::<crate::query::budget::BudgetExceeded>()
                .expect("matched arm guarantees downcast");
            budget_err.predicate_shape = Some(shape);
            Err(anyhow::Error::from(budget_err))
        }
        Err(other) => {
            span.record("matched", 0u64);
            span.record("budget_exceeded", false);
            Err(other)
        }
    }
}

/// Evaluates a single node against an expression.
///
/// # Errors
///
/// Returns an error for unsupported predicates or if recursion depth exceeds the guard's limit.
pub fn evaluate_node(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    expr: &Expr,
    guard: &mut crate::query::security::RecursionGuard,
) -> Result<bool> {
    guard.enter()?;

    let result = match expr {
        Expr::Condition(cond) => evaluate_condition(ctx, node_id, cond),
        Expr::And(operands) => {
            for operand in operands {
                if !evaluate_node(ctx, node_id, operand, guard)? {
                    guard.exit();
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Expr::Or(operands) => {
            for operand in operands {
                if evaluate_node(ctx, node_id, operand, guard)? {
                    guard.exit();
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Expr::Not(inner) => Ok(!evaluate_node(ctx, node_id, inner, guard)?),
        Expr::Join(_) => {
            // Join expressions are evaluated at a higher level (execute_join),
            // not per-node. If we reach here, it's a programming error.
            Err(anyhow::anyhow!(
                "Join expressions cannot be evaluated per-node; use execute_join instead"
            ))
        }
    };

    guard.exit();
    result
}

fn evaluate_condition(ctx: &GraphEvalContext, node_id: NodeId, cond: &Condition) -> Result<bool> {
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return Ok(false);
    };

    match cond.field.as_str() {
        "kind" => Ok(match_kind(ctx, entry, &cond.operator, &cond.value)),
        "name" => Ok(match_name(ctx, entry, &cond.operator, &cond.value)),
        "path" => Ok(match_path(ctx, entry, &cond.operator, &cond.value)),
        "lang" | "language" => Ok(match_lang(ctx, entry, &cond.operator, &cond.value)),
        "visibility" => Ok(match_visibility(ctx, entry, &cond.operator, &cond.value)),
        "async" => Ok(match_async(entry, &cond.operator, &cond.value)),
        "static" => Ok(match_static(entry, &cond.operator, &cond.value)),
        "callers" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_callers_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_callers(ctx, node_id, &cond.value))
            }
        }
        "callees" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_callees_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_callees(ctx, node_id, &cond.value))
            }
        }
        "imports" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_imports_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_imports(ctx, node_id, &cond.value))
            }
        }
        "exports" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_exports_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_exports(ctx, node_id, &cond.value))
            }
        }
        "references" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_references_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_references(ctx, node_id, &cond.operator, &cond.value))
            }
        }
        "impl" | "implements" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_implements_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_implements(ctx, node_id, &cond.value))
            }
        }
        field if field.starts_with("scope.") => Ok(match_scope(
            ctx,
            node_id,
            field,
            &cond.operator,
            &cond.value,
        )),
        "returns" => Ok(match_returns(
            ctx,
            node_id,
            entry,
            &cond.operator,
            &cond.value,
        )),
        // Phase A C indirect-call precision (U18.1): predicate parity with the
        // planner surface (`sqry_query`). NodeEntry has no `id` field in the
        // public arena API — anchor `NodeId` is the `node_id` parameter to this
        // function. `address_taken` / `callsite_promiscuous` are O(1) flag
        // lookups against `ctx.graph.macro_metadata()`; `resolved_via` walks
        // outgoing Calls edges anchored at `node_id`. See Phase A 02_DESIGN §11
        // and 03_IMPLEMENTATION_PLAN §"U18.1".
        "address_taken" => Ok(match_address_taken(
            ctx,
            node_id,
            &cond.operator,
            &cond.value,
        )),
        "callsite_promiscuous" => Ok(match_callsite_promiscuous(
            ctx,
            node_id,
            &cond.operator,
            &cond.value,
        )),
        "resolved_via" => Ok(match_resolved_via(ctx, node_id, &cond.value)),
        field if is_plugin_field(ctx, field) => Err(anyhow!(
            "Plugin field '{field}' requires metadata not available in graph backend"
        )),
        _ => Ok(false), // Unknown field
    }
}

/// Checks if a field is a plugin-specific field.
fn is_plugin_field(ctx: &GraphEvalContext, field: &str) -> bool {
    // Check plugin registry for field descriptors
    let is_registered_field = ctx
        .plugin_manager
        .plugins()
        .iter()
        .flat_map(|plugin| plugin.fields().iter())
        .any(|descriptor| descriptor.name == field);

    if is_registered_field {
        return true;
    }

    // Fallback static list (for when registry not fully wired)
    // Note: "async" and "static" are now handled natively via `NodeEntry` flags
    matches!(
        field,
        "abstract" | "final" | "generic" | "parameters" | "arity"
    )
}

// ============================================================================
// Kind predicate (with regex + synonyms)
// ============================================================================

/// Maps synonyms to canonical kind names (case-sensitive).
///
/// Per v6 spec, kind: comparisons are case-sensitive.
/// Only exact matches for known synonyms are normalized.
fn normalize_kind(kind: &str) -> &str {
    match kind {
        // Rust/Go synonyms (case-sensitive)
        "trait" => "interface", // Rust trait = interface
        "impl" => "implementation",
        // Property synonyms
        "field" => "property",
        // Module synonyms
        "namespace" => "module",
        // Component synonyms
        "element" => "component",
        // CSS/Style synonyms
        "style" => "style_rule",
        "at_rule" => "style_at_rule",
        "css_var" | "custom_property" => "style_variable",
        // No lowercasing - return as-is for case-sensitive comparison
        _ => kind,
    }
}

fn match_kind(
    _ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let actual = entry.kind.as_str();

    match (operator, value) {
        (Operator::Equal, Value::String(expected)) => {
            let normalized_expected = normalize_kind(expected);
            let normalized_actual = normalize_kind(actual);
            normalized_actual == normalized_expected
        }
        (Operator::Regex, Value::Regex(regex_val)) => get_or_compile_regex(
            &regex_val.pattern,
            regex_val.flags.case_insensitive,
            regex_val.flags.multiline,
            regex_val.flags.dot_all,
        )
        .map(|re| regex_is_match(&re, actual))
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Name predicate (EXACT match for equality, regex supported)
// ============================================================================

fn match_name(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    match (operator, value) {
        // Use segments_match for qualified name matching:
        // - "connect" matches "database::connect" (suffix match)
        // - "foo::bar" matches "baz::foo::bar" (suffix match)
        // - "database::connect" matches "database::connect" (exact match)
        (Operator::Equal, Value::String(expected)) => {
            entry_query_texts(ctx.graph, entry).iter().any(|candidate| {
                language_aware_segments_match(ctx.graph, entry.file, candidate, expected)
            })
        }
        (Operator::Regex, Value::Regex(regex_val)) => get_or_compile_regex(
            &regex_val.pattern,
            regex_val.flags.case_insensitive,
            regex_val.flags.multiline,
            regex_val.flags.dot_all,
        )
        .map(|re| {
            entry_query_texts(ctx.graph, entry)
                .iter()
                .any(|candidate| regex_is_match(&re, candidate))
        })
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Path predicate (glob + regex)
// ============================================================================

/// Check if a pattern is relative (doesn't start with `/`).
fn is_relative_pattern(pattern: &str) -> bool {
    !pattern.starts_with('/')
}

fn match_path(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(file_path) = ctx.graph.files().resolve(entry.file) else {
        return false;
    };

    match (operator, value) {
        (Operator::Equal, Value::String(pattern)) => {
            // Only strip workspace_root for RELATIVE patterns (parity with legacy index)
            let match_path = if is_relative_pattern(pattern) {
                if let Some(root) = ctx.workspace_root {
                    file_path
                        .strip_prefix(root)
                        .map_or_else(|_| file_path.to_path_buf(), std::path::Path::to_path_buf)
                } else {
                    file_path.to_path_buf()
                }
            } else {
                // Absolute pattern: match against full path
                file_path.to_path_buf()
            };
            globset::Glob::new(pattern)
                .map(|g| g.compile_matcher().is_match(&match_path))
                .unwrap_or(false)
        }
        (Operator::Regex, Value::Regex(regex_val)) => {
            // For regex, always use full path (matches legacy index behavior)
            get_or_compile_regex(
                &regex_val.pattern,
                regex_val.flags.case_insensitive,
                regex_val.flags.multiline,
                regex_val.flags.dot_all,
            )
            .map(|re| regex_is_match(&re, file_path.to_string_lossy().as_ref()))
            .unwrap_or(false)
        }
        _ => false,
    }
}

// ============================================================================
// Language predicate (from FILE REGISTRY, not NodeEntry)
// ============================================================================

/// Convert Language enum to canonical string for parity with legacy index.
///
/// Legacy index uses canonical names like "javascript", "typescript", etc.
/// `Language::Display` uses short forms like "js", "ts".
/// This function provides the canonical mapping for query parity.
fn language_to_canonical(lang: crate::graph::node::Language) -> &'static str {
    use crate::graph::node::Language;
    match lang {
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::CSharp => "csharp",
        Language::Css => "css",
        Language::JavaScript => "javascript",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::Go => "go",
        Language::Java => "java",
        Language::Ruby => "ruby",
        Language::Php => "php",
        Language::Swift => "swift",
        Language::Kotlin => "kotlin",
        Language::Scala => "scala",
        Language::Sql => "sql",
        Language::Dart => "dart",
        Language::Lua => "lua",
        Language::Perl => "perl",
        Language::Shell => "shell",
        Language::Groovy => "groovy",
        Language::Elixir => "elixir",
        Language::R => "r",
        Language::Haskell => "haskell",
        Language::Html => "html",
        Language::Svelte => "svelte",
        Language::Vue => "vue",
        Language::Zig => "zig",
        Language::Terraform => "terraform",
        Language::Puppet => "puppet",
        Language::Pulumi => "pulumi",
        Language::Http => "http",
        Language::Plsql => "plsql",
        Language::Apex => "apex",
        Language::Abap => "abap",
        Language::ServiceNow => "servicenow",
        Language::Json => "json",
    }
}

fn match_lang(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    // Get language from file registry - NodeEntry has no language field
    let Some(lang) = ctx.graph.files().language_for_file(entry.file) else {
        return false;
    };

    // Use canonical language names for parity with legacy index
    let actual = language_to_canonical(lang);

    // Support both equality and regex operators
    match (operator, value) {
        (Operator::Equal, Value::String(expected)) => actual == expected,
        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
            &rv.pattern,
            rv.flags.case_insensitive,
            rv.flags.multiline,
            rv.flags.dot_all,
        )
        .map(|re| regex_is_match(&re, actual))
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Visibility predicate
// ============================================================================

fn match_visibility(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value.as_string() else {
        return false;
    };

    let normalized_expected = if expected == "pub" {
        "public"
    } else {
        expected
    };

    let Some(vis_id) = entry.visibility else {
        // No visibility = private by default
        return match operator {
            Operator::Equal => normalized_expected == "private",
            _ => false,
        };
    };

    let Some(actual) = ctx.graph.strings().resolve(vis_id) else {
        return false;
    };
    let normalized_actual = if actual.as_ref().starts_with("pub") {
        "public"
    } else {
        actual.as_ref()
    };

    // Case-sensitive comparison
    match operator {
        Operator::Equal => normalized_actual == normalized_expected,
        _ => false,
    }
}

// ============================================================================
// Returns predicate
// ============================================================================

/// Match `returns:<TypeName>` predicate via edge-based, byte-exact evaluation.
///
/// Walks every outgoing edge from `node_id` and checks for the first
/// `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }` whose
/// target node's interned primary name equals the predicate value
/// byte-exactly (case-sensitive). Returns `false` if the candidate has no
/// `Return`-context type edges, or if every such edge targets a different
/// name.
///
/// This mirrors `sqry_db::planner::execute::node_returns_type` exactly so
/// that the legacy graph-query backend and the planner produce identical
/// results for `returns:` predicates. The previous substring path against
/// `NodeEntry.signature` was retired because it produced false positives
/// whenever the requested type name occurred anywhere in the function
/// signature text (e.g. `returns:error` matched any signature mentioning
/// `error` in a parameter or doc).
///
/// Substring/regex semantics are out of scope here and may land later as a
/// distinct `returns~` operator; only `Operator::Equal` is honoured.
///
/// The `entry.kind` guard for `Function`/`Method` is retained as a cheap
/// fast-path early-out: only callable nodes can plausibly have a
/// `TypeOf{Return}` outgoing edge, so non-callable candidates can be
/// rejected without touching the edge store. The planner does not need
/// this guard because its dispatch surface keys on a different shape.
fn match_returns(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value.as_string() else {
        return false;
    };

    // Only functions and methods can have a return type.  This is a fast
    // early-out — see the doc comment above for the rationale.
    if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }

    if !matches!(operator, Operator::Equal) {
        return false;
    }

    let nodes = ctx.graph.nodes();
    let strings = ctx.graph.strings();
    for edge in ctx.graph.edges().edges_from(node_id) {
        if !matches!(
            edge.kind,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Return),
                ..
            }
        ) {
            continue;
        }
        let Some(target_entry) = nodes.get(edge.target) else {
            continue;
        };
        if let Some(name) = strings.resolve(target_entry.name)
            && name.as_ref() == expected
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Boolean predicates (async, static)
// ============================================================================

/// Match `async:true` or `async:false` predicate.
///
/// Checks the `is_async` flag on `NodeEntry`.
/// Handles both Boolean values (from parser) and String values ("true"/"false").
fn match_async(entry: &NodeEntry, operator: &Operator, value: &Value) -> bool {
    let expected = value_to_bool(value);
    let Some(expected) = expected else {
        return false;
    };

    match operator {
        Operator::Equal => entry.is_async == expected,
        _ => false,
    }
}

/// Match `static:true` or `static:false` predicate.
///
/// Checks the `is_static` flag on `NodeEntry`.
/// Handles both Boolean values (from parser) and String values ("true"/"false").
fn match_static(entry: &NodeEntry, operator: &Operator, value: &Value) -> bool {
    let expected = value_to_bool(value);
    let Some(expected) = expected else {
        return false;
    };

    match operator {
        Operator::Equal => entry.is_static == expected,
        _ => false,
    }
}

/// Convert a Value to a boolean.
///
/// Handles:
/// - `Value::Boolean(b)` → `Some(b)`
/// - `Value::String("true"|"yes"|"1")` → `Some(true)`
/// - `Value::String("false"|"no"|"0")` → `Some(false)`
/// - Other values → `None`
fn value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(b) => Some(*b),
        Value::String(s) => match s.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

// ============================================================================
// Phase A C indirect-call precision (U18.1)
//
// Predicate parity with the planner surface (`sqry_query`). The planner-side
// queries live in `sqry-db/src/queries/{address_taken,callsite_promiscuous}.rs`
// and the planner-IR `resolved_via` site filter lives in
// `sqry-db/src/planner/ir.rs`. The helpers below give the **core-query**
// executor the same semantic answer. SPEC §3.1.3 + DESIGN §11 / §12.
// ============================================================================

/// `address_taken:true` / `address_taken:false` — match nodes flagged by the
/// C plugin's address-taken classifier (DESIGN §6).
///
/// Backed by `NodeFlags::ADDRESS_TAKEN` via
/// [`crate::graph::unified::NodeMetadataStore::is_address_taken`]. O(1) hash
/// lookup against the per-snapshot metadata store. On non-C nodes the flag is
/// never set, so this predicate naturally short-circuits to `false`.
fn match_address_taken(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value_to_bool(value) else {
        return false;
    };
    match operator {
        Operator::Equal => ctx.graph.macro_metadata().is_address_taken(node_id) == expected,
        _ => false,
    }
}

/// `callsite_promiscuous:true` / `callsite_promiscuous:false` — match nodes
/// flagged by the C indirect-call resolver as containing at least one
/// callsite that exceeded the per-callsite cardinality cap (DESIGN §5.2).
///
/// Backed by `NodeFlags::CALLSITE_PROMISCUOUS` via
/// [`crate::graph::unified::NodeMetadataStore::is_callsite_promiscuous`]. O(1)
/// hash lookup against the per-snapshot metadata store.
fn match_callsite_promiscuous(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value_to_bool(value) else {
        return false;
    };
    match operator {
        Operator::Equal => ctx.graph.macro_metadata().is_callsite_promiscuous(node_id) == expected,
        _ => false,
    }
}

/// `resolved_via:direct | type_match | binding_plane` — match nodes that have
/// at least one **outgoing** Calls edge with the requested resolution
/// provenance (DESIGN §6).
///
/// Direction note: we walk OUTGOING edges (`edges_from(node_id)`) — i.e.
/// "this function makes at least one call resolved via X". This mirrors
/// [`match_callers`]'s direction convention (callers walks OUTGOING Calls to
/// answer "does this node call the target?"), so a query like
/// `resolved_via:type_match AND callers:my_read` matches a caller whose
/// outgoing Calls edge to `my_read` was resolved by flat type matching.
///
/// Edge-level predicate — the field is `indexed: false` in the registry
/// because there is no flag-or-bitset shortcut: we must walk
/// `edges_from(node_id)` and inspect each `EdgeKind::Calls { resolved_via, ... }`.
fn match_resolved_via(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(want_str) = value.as_string() else {
        return false;
    };
    let want = match want_str {
        "direct" => crate::graph::unified::edge::ResolvedVia::Direct,
        "type_match" => crate::graph::unified::edge::ResolvedVia::TypeMatch,
        "binding_plane" => crate::graph::unified::edge::ResolvedVia::BindingPlane,
        "virtual_dispatch" => crate::graph::unified::edge::ResolvedVia::VirtualDispatch,
        "interface_dispatch" => crate::graph::unified::edge::ResolvedVia::InterfaceDispatch,
        "duck_typed" => crate::graph::unified::edge::ResolvedVia::DuckTyped,
        "structural" => crate::graph::unified::edge::ResolvedVia::Structural,
        "promiscuous_elided" => crate::graph::unified::edge::ResolvedVia::PromiscuousElided,
        _ => return false, // unknown enum value
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Calls { resolved_via, .. } = &edge.kind
            && *resolved_via == want
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Relation predicates (CORRECTED DIRECTION)
// ============================================================================

/// `callers:X` - find symbols that CALL X
///
/// When evaluating node Y: does Y call X? → Check Y's OUTGOING edges
///
/// For dynamically-typed languages (Lua, Python, Ruby, etc.), method calls like
/// `target:method()` create edges to `receiver::method` where `receiver` is the
/// variable name, not the class name. To handle this, when querying for a qualified
/// name like `Class::method`, we also match call edges to `X::method` where X is
/// any receiver, as long as a symbol `Class::method` exists in the graph.
fn match_callers(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_name) = value.as_string() else {
        return false;
    };

    // Extract the method name part for fallback matching in dynamic languages
    // e.g., "Player::takeDamage" -> "takeDamage"
    let method_part = extract_method_name(target_name);

    // Check OUTGOING Calls edges from this node
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && let Some(target_entry) = ctx.graph.nodes().get(edge.target)
        {
            let callee_names = entry_query_texts(ctx.graph, target_entry);

            if callee_names.iter().any(|callee_name| {
                language_aware_segments_match(
                    ctx.graph,
                    target_entry.file,
                    callee_name,
                    target_name,
                )
            }) {
                return true;
            }

            // For dynamic languages: if target_name is qualified (e.g., "Player::takeDamage")
            // and callee is also qualified (e.g., "target::takeDamage"), match on method part
            // This handles cases where the receiver type isn't known at call time
            if let Some(method) = &method_part
                && callee_names
                    .iter()
                    .filter_map(|callee_name| extract_method_name(callee_name))
                    .any(|callee_method| method == &callee_method)
            {
                return true;
            }
        }
    }
    false
}

/// Extract the method name from a qualified name.
/// e.g., "`Player::takeDamage`" -> Some("takeDamage")
/// e.g., "takeDamage" -> None (no separator, not qualified)
#[must_use]
pub fn extract_method_name(qualified: &str) -> Option<String> {
    // Look for separator from the end
    for sep in ["::", ".", "#", ":", "/"] {
        if let Some(pos) = qualified.rfind(sep) {
            let method = &qualified[pos + sep.len()..];
            if !method.is_empty() {
                return Some(method.to_string());
            }
        }
    }
    None
}

/// `callees:X` - find symbols that ARE CALLED BY X
///
/// When evaluating node Y: does X call Y? → Check Y's INCOMING edges for source=X
fn match_callees(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(caller_name) = value.as_string() else {
        return false;
    };

    // Check INCOMING Calls edges to this node
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && let Some(source_entry) = ctx.graph.nodes().get(edge.source)
            && entry_query_texts(ctx.graph, source_entry)
                .iter()
                .any(|source_name| {
                    language_aware_segments_match(
                        ctx.graph,
                        source_entry.file,
                        source_name,
                        caller_name,
                    )
                })
        {
            return true;
        }
    }
    false
}

/// `imports:X` — per-node match.
///
/// A node matches iff:
/// 1. It is itself an `Import` node whose name (or alias text) matches `X`, or
/// 2. It has at least one outgoing `Imports` edge whose target name, alias,
///    or wildcard flag matches `X` (see [`import_edge_matches`]).
///
/// This is the planner-canonical semantic per
/// `docs/development/phase-n-structural-semantics/02_DESIGN.md` §6 — every
/// transport (planner, MCP, CLI, LSP) shares it. The previous file-scoped
/// behavior ("every node in a file that imports X matches") was retired in
/// DB15 to remove the cross-engine divergence flagged by Codex's DB14
/// review. Matches the set returned by
/// [`sqry_db::queries::ImportsQuery`](https://docs.rs/sqry-db) for the same
/// key.
fn match_imports(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_module) = value.as_string() else {
        return false;
    };

    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };

    if entry.kind == NodeKind::Import && import_entry_matches(ctx.graph, entry, target_module) {
        return true;
    }

    for edge in ctx.graph.edges().edges_from(node_id) {
        if import_edge_matches(ctx.graph, &edge, target_module) {
            return true;
        }
    }
    false
}

/// Returns `true` when the given `Imports` edge imports something matching
/// `target_module`, checking the target node text, the edge's alias, and the
/// wildcard flag. Shared with [`crate::graph::unified::concurrent::GraphSnapshot`]
/// consumers (including `sqry-db::queries::relation`) that need the same
/// semantics as the graph-native `imports:` predicate.
#[must_use]
pub fn import_edge_matches<G: crate::graph::unified::concurrent::GraphAccess>(
    graph: &G,
    edge: &StoreEdgeRef,
    target_module: &str,
) -> bool {
    let EdgeKind::Imports { alias, is_wildcard } = &edge.kind else {
        return false;
    };

    // Check target node text across simple, canonical, and native-display forms.
    let target_match = graph
        .nodes()
        .get(edge.target)
        .is_some_and(|entry| import_entry_matches(graph, entry, target_module));

    // Check alias
    let alias_match = alias
        .and_then(|sid| graph.strings().resolve(sid))
        .is_some_and(|alias_str| {
            graph.nodes().get(edge.source).is_some_and(|entry| {
                import_text_matches(graph, entry.file, alias_str.as_ref(), target_module)
            })
        });

    // Check wildcard
    let wildcard_match = *is_wildcard && target_module == "*";

    target_match || alias_match || wildcard_match
}

/// Substring-based text match for `imports:` semantics, with language-aware
/// canonicalization fallback when the file's language maps the input module
/// path into graph-internal `::` form.
#[must_use]
pub fn import_text_matches<G: crate::graph::unified::concurrent::GraphAccess>(
    graph: &G,
    file_id: FileId,
    candidate: &str,
    target_module: &str,
) -> bool {
    if candidate.contains(target_module) {
        return true;
    }

    graph
        .files()
        .language_for_file(file_id)
        .is_some_and(|language| {
            let canonical_target = canonicalize_graph_qualified_name(language, target_module);
            canonical_target != target_module && candidate.contains(&canonical_target)
        })
}

/// Matches an Import/candidate node entry against a target module using
/// the shared `imports:` substring + canonicalization semantics.
#[must_use]
pub fn import_entry_matches<G: crate::graph::unified::concurrent::GraphAccess>(
    graph: &G,
    entry: &NodeEntry,
    target_module: &str,
) -> bool {
    entry_query_texts(graph, entry)
        .iter()
        .any(|candidate| import_text_matches(graph, entry.file, candidate, target_module))
}

/// Segment-aware name equality with a language-specific canonicalization
/// fallback. First tries a direct [`segments_match`]; if that fails, the
/// file's language (if any) is consulted to canonicalize `expected` into
/// graph-internal `::` form before retrying.
#[must_use]
pub fn language_aware_segments_match<G: crate::graph::unified::concurrent::GraphAccess>(
    graph: &G,
    file_id: FileId,
    candidate: &str,
    expected: &str,
) -> bool {
    if segments_match(candidate, expected) {
        return true;
    }

    graph
        .files()
        .language_for_file(file_id)
        .is_some_and(|language| {
            let canonical_expected = canonicalize_graph_qualified_name(language, expected);
            canonical_expected != expected && segments_match(candidate, &canonical_expected)
        })
}

fn push_unique_query_text(texts: &mut Vec<String>, candidate: impl Into<String>) {
    let candidate = candidate.into();
    if !texts.iter().any(|existing| existing == &candidate) {
        texts.push(candidate);
    }
}

/// Collects every string form that can satisfy a name query for the given
/// node entry: the interned name, the qualified name, and (when a language
/// is recorded) the display-qualified name produced by
/// [`display_graph_qualified_name`]. Duplicates are dropped so that relation
/// matchers do not re-check equivalent forms.
#[must_use]
pub fn entry_query_texts<G: crate::graph::unified::concurrent::GraphAccess>(
    graph: &G,
    entry: &NodeEntry,
) -> Vec<String> {
    let mut texts = Vec::with_capacity(3);

    if let Some(name) = graph.strings().resolve(entry.name) {
        push_unique_query_text(&mut texts, name.to_string());
    }

    if let Some(qualified) = entry
        .qualified_name
        .and_then(|qualified_name_id| graph.strings().resolve(qualified_name_id))
    {
        push_unique_query_text(&mut texts, qualified.to_string());

        if let Some(language) = graph.files().language_for_file(entry.file) {
            push_unique_query_text(
                &mut texts,
                display_graph_qualified_name(
                    language,
                    qualified.as_ref(),
                    entry.kind,
                    entry.is_static,
                ),
            );
        }
    }

    texts
}

/// `exports:X` - find symbols that export X
///
/// File scope filtering: Only considers export edges where both source and
/// target are in the same file as the node being evaluated. This prevents
/// re-export edges from causing symbols in other files to match.
fn match_exports(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_name) = value.as_string() else {
        return false;
    };

    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };
    let node_file = entry.file;

    if !entry_query_texts(ctx.graph, entry).iter().any(|candidate| {
        language_aware_segments_match(ctx.graph, entry.file, candidate, target_name)
    }) {
        return false;
    }

    let edges = ctx.graph.edges();

    // Check OUTGOING export edges: this node exports something
    for edge in edges.edges_from(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind {
            // File scope filtering: export edge must be in same file
            if let Some(target_entry) = ctx.graph.nodes().get(edge.target)
                && target_entry.file == node_file
            {
                return true;
            }
        }
    }

    // Check INCOMING export edges: something exports this node
    for edge in edges.edges_to(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind {
            // File scope filtering: export edge source must be in same file
            if let Some(source_entry) = ctx.graph.nodes().get(edge.source)
                && source_entry.file == node_file
            {
                return true;
            }
        }
    }

    false
}

/// `references:X` - find symbols named X that have references TO them.
///
/// Current behavior: references include `Calls`, `Imports`, `FfiCall`, and `References` edges.
fn match_references(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    // First check if the symbol name matches the target
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };

    let name_matches = match (operator, value) {
        (Operator::Equal, Value::String(target)) => entry_query_texts(ctx.graph, entry)
            .iter()
            .any(|candidate| candidate == target || candidate.ends_with(&format!("::{target}"))),
        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
            &rv.pattern,
            rv.flags.case_insensitive,
            rv.flags.multiline,
            rv.flags.dot_all,
        )
        .map(|re| {
            entry_query_texts(ctx.graph, entry)
                .iter()
                .any(|candidate| regex_is_match(&re, candidate))
        })
        .unwrap_or(false),
        _ => false,
    };

    if !name_matches {
        return false;
    }

    // Check if the symbol has references TO it (incoming edges)
    // Current behavior: References, Calls, Imports, AND FfiCall all count as references
    for edge in ctx.graph.edges().edges_to(node_id) {
        let is_reference = matches!(
            &edge.kind,
            EdgeKind::References
                | EdgeKind::Calls { .. }
                | EdgeKind::Imports { .. }
                | EdgeKind::FfiCall { .. }
        );
        if is_reference {
            return true;
        }
    }

    false
}

/// `implements:X` - find symbols that implement interface/trait X
fn match_implements(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(trait_name) = value.as_string() else {
        return false;
    };

    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Implements = &edge.kind
            && let Some(target_entry) = ctx.graph.nodes().get(edge.target)
            && entry_query_texts(ctx.graph, target_entry)
                .iter()
                .any(|name| {
                    language_aware_segments_match(ctx.graph, target_entry.file, name, trait_name)
                })
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Scope predicates (with regex support)
// ============================================================================

/// Convert `NodeKind` to scope type string for predicate matching.
///
/// This mapping provides parity with legacy index scope predicates:
/// - Trait → interface
/// - Test → function
/// - Service → class
/// - etc.
fn node_kind_to_scope_type(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function | NodeKind::Test => "function",
        NodeKind::Method => "method",
        NodeKind::Class | NodeKind::Service => "class",
        NodeKind::Interface | NodeKind::Trait => "interface",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Module => "module",
        NodeKind::Macro => "macro",
        NodeKind::Component => "component",
        NodeKind::Resource | NodeKind::Endpoint => "resource",
        // Non-container types
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::EnumVariant => "enumvariant",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::CallSite => "callsite",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Other => "other",
    }
}

fn match_scope(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    field: &str,
    operator: &Operator,
    value: &Value,
) -> bool {
    let scope_part = field.strip_prefix("scope.").unwrap_or("");
    match scope_part {
        "type" => match_scope_type(ctx, node_id, operator, value),
        "name" => match_scope_name(ctx, node_id, operator, value),
        "parent" => match_scope_parent_name(ctx, node_id, operator, value),
        "ancestor" => match_scope_ancestor_name(ctx, node_id, operator, value),
        _ => false,
    }
}

fn match_scope_type(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
        {
            // Use mapped scope type for parity with legacy index
            let scope_type = node_kind_to_scope_type(parent.kind);
            return match (operator, value) {
                // Case-sensitive comparison
                (Operator::Equal, Value::String(exp)) => scope_type == exp,
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| regex_is_match(&re, scope_type))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

fn match_scope_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
            && let Some(name) = ctx.graph.strings().resolve(parent.name)
        {
            return match (operator, value) {
                // Use segments_match for qualified name suffix matching
                (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| regex_is_match(&re, &name))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

/// `scope.parent:X` - find nodes with immediate parent NAMED X.
///
/// Uses `segments_match` for qualified name suffix matching (same as name: predicate).
fn match_scope_parent_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
            && let Some(name) = ctx.graph.strings().resolve(parent.name)
        {
            return match (operator, value) {
                // Use segments_match for qualified name suffix matching
                (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| regex_is_match(&re, &name))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

/// `scope.ancestor:X` - find nodes with any ancestor NAMED X.
///
/// CYCLE PROTECTION: Uses visited set to prevent infinite loops.
/// Uses `segments_match` for qualified name suffix matching.
fn match_scope_ancestor_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    let mut current = node_id;
    let mut visited = HashSet::new();
    visited.insert(node_id);

    loop {
        let mut found_parent = false;
        for edge in ctx.graph.edges().edges_to(current) {
            if let EdgeKind::Contains = &edge.kind {
                // CYCLE PROTECTION: Skip if we've already visited this node
                if visited.contains(&edge.source) {
                    continue;
                }
                visited.insert(edge.source);

                found_parent = true;
                current = edge.source;
                if let Some(parent) = ctx.graph.nodes().get(current)
                    && let Some(name) = ctx.graph.strings().resolve(parent.name)
                {
                    let matches = match (operator, value) {
                        // Use segments_match for qualified name suffix matching
                        (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                            &rv.pattern,
                            rv.flags.case_insensitive,
                            rv.flags.multiline,
                            rv.flags.dot_all,
                        )
                        .map(|re| regex_is_match(&re, &name))
                        .unwrap_or(false),
                        _ => false,
                    };
                    if matches {
                        return true;
                    }
                }
                break;
            }
        }
        if !found_parent {
            break;
        }
    }
    false
}

// ============================================================================
// Subquery evaluation
// ============================================================================

/// Evaluate an expression against all nodes and return the set of matching node IDs.
///
/// Used for subquery evaluation: `callers:(kind:function AND async:true)`.
///
/// # Errors
///
/// Returns an error if predicate evaluation fails.
pub fn evaluate_subquery(ctx: &GraphEvalContext, expr: &Expr) -> Result<HashSet<NodeId>> {
    let recursion_limits = crate::config::RecursionLimits::load_or_default()?;
    let expr_depth = recursion_limits.effective_expr_depth()?;
    let mut guard = crate::query::security::RecursionGuard::new(expr_depth)?;

    let arena = ctx.graph.nodes();
    let mut matches = HashSet::new();
    for (id, _) in arena.iter() {
        if evaluate_node(ctx, id, expr, &mut guard)? {
            matches.insert(id);
        }
    }
    Ok(matches)
}

/// Match callers using a precomputed subquery result set.
///
/// Checks if any of this node's outgoing Calls edges target a node in the
/// precomputed set. Returns an error if the subquery was not precomputed
/// (indicates a bug in `precompute_subqueries`).
fn match_callers_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match callees using a precomputed subquery result set.
fn match_callees_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && matches.contains(&edge.source)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match imports using a precomputed subquery result set.
///
/// Per-node semantic (DB15): returns true iff this specific node has an
/// outgoing `Imports` edge whose target is in the precomputed subquery set.
/// Aligned with [`match_imports`] and `sqry_db::queries::ImportsQuery`.
fn match_imports_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Imports { .. } = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match exports using a precomputed subquery result set.
fn match_exports_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match implements using a precomputed subquery result set.
fn match_implements_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Implements = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match references using a precomputed subquery result set.
fn match_references_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_to(node_id) {
        let is_reference = matches!(
            &edge.kind,
            EdgeKind::References
                | EdgeKind::Calls { .. }
                | EdgeKind::Imports { .. }
                | EdgeKind::FfiCall { .. }
        );
        if is_reference && matches.contains(&edge.source) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ============================================================================
// Join evaluation
// ============================================================================

/// Evaluate a join expression, returning matched (left, right) node pairs.
///
/// For `(kind:function AND lang:rust) CALLS (kind:function AND lang:python)`:
/// 1. Evaluate LHS query → set of matching nodes
/// 2. Evaluate RHS query → set of matching nodes
/// 3. For each LHS node, check edges of the specified kind
/// 4. If edge target is in RHS set → add (lhs, rhs) pair
///
/// # Errors
///
/// Returns an error if subquery evaluation or edge traversal fails.
pub fn evaluate_join(
    ctx: &GraphEvalContext,
    join: &JoinExpr,
    max_results: Option<usize>,
) -> Result<JoinEvalResult> {
    let lhs_matches = evaluate_subquery(ctx, &join.left)?;
    let rhs_matches = evaluate_subquery(ctx, &join.right)?;
    let cap = max_results.unwrap_or(DEFAULT_JOIN_RESULT_CAP);

    let mut pairs = Vec::new();
    let mut truncated = false;
    'outer: for &lhs_id in &lhs_matches {
        for edge in ctx.graph.edges().edges_from(lhs_id) {
            if edge_matches_join_kind(&edge.kind, &join.edge) && rhs_matches.contains(&edge.target)
            {
                pairs.push((lhs_id, edge.target));
                if pairs.len() >= cap {
                    truncated = true;
                    break 'outer;
                }
            }
        }
    }
    Ok(JoinEvalResult { pairs, truncated })
}

/// Result of a join evaluation including truncation metadata.
pub struct JoinEvalResult {
    /// The matched (source, target) node ID pairs.
    pub pairs: Vec<(NodeId, NodeId)>,
    /// Whether the result set was truncated by the result cap.
    pub truncated: bool,
}

/// Default result cap for join queries.
///
/// Prevents unbounded memory growth when a join produces a large number of matching pairs.
const DEFAULT_JOIN_RESULT_CAP: usize = 10_000;

/// Check if an edge kind matches a join edge kind.
fn edge_matches_join_kind(edge_kind: &EdgeKind, join_kind: &JoinEdgeKind) -> bool {
    match join_kind {
        JoinEdgeKind::Calls => matches!(edge_kind, EdgeKind::Calls { .. }),
        JoinEdgeKind::Imports => matches!(edge_kind, EdgeKind::Imports { .. }),
        JoinEdgeKind::Inherits => matches!(edge_kind, EdgeKind::Inherits),
        JoinEdgeKind::Implements => matches!(edge_kind, EdgeKind::Implements),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::query::types::{Condition, Field, Span};
    use std::path::Path;

    #[test]
    fn test_import_text_matches_canonicalized_qualified_imports() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/FileProcessor.cs"))
            .unwrap();
        assert!(graph.files_mut().set_language(file_id, Language::CSharp));

        assert!(import_text_matches(
            &graph,
            file_id,
            "System::IO",
            "System.IO"
        ));
        assert!(import_text_matches(
            &graph,
            file_id,
            "System::Collections::Generic",
            "System.Collections.Generic"
        ));
        assert!(!import_text_matches(
            &graph,
            file_id,
            "System::Text",
            "System.IO"
        ));
    }

    #[test]
    fn test_language_aware_segments_match_supports_ruby_method_separators() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("app/models/user.rb"))
            .unwrap();
        assert!(graph.files_mut().set_language(file_id, Language::Ruby));

        assert!(language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::show",
            "Admin::Users::Controller#show"
        ));
        assert!(language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::show",
            "show"
        ));
        assert!(!language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::index",
            "Admin::Users::Controller#show"
        ));
    }

    #[test]
    fn test_normalize_kind() {
        // Case-sensitive synonyms
        assert_eq!(normalize_kind("trait"), "interface");
        assert_eq!(normalize_kind("TRAIT"), "TRAIT"); // Case-sensitive: not a synonym
        assert_eq!(normalize_kind("field"), "property");
        assert_eq!(normalize_kind("namespace"), "module");
        assert_eq!(normalize_kind("function"), "function"); // unchanged
    }

    #[test]
    fn test_graph_eval_context_builder() {
        let graph = CodeGraph::new();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm)
            .with_workspace_root(Path::new("/test"))
            .with_parallel_disabled(true);

        assert!(ctx.disable_parallel);
        assert_eq!(ctx.workspace_root, Some(Path::new("/test")));
    }

    // ================================================================
    // collect_subquery_exprs tests
    // ================================================================

    /// Helper: build `field:(inner_expr)` as a Condition with `Value::Subquery`.
    fn subquery_condition(field: &str, inner: Expr, start: usize, end: usize) -> Expr {
        Expr::Condition(Condition {
            field: Field(field.to_string()),
            operator: Operator::Equal,
            value: Value::Subquery(Box::new(inner)),
            span: Span::with_position(start, end, 1, start + 1),
        })
    }

    /// Helper: build a simple `kind:function` condition.
    fn kind_condition(kind: &str) -> Expr {
        Expr::Condition(Condition {
            field: Field("kind".to_string()),
            operator: Operator::Equal,
            value: Value::String(kind.to_string()),
            span: Span::default(),
        })
    }

    #[test]
    fn test_collect_subquery_exprs_post_order_depth_2() {
        // Build: callers:(callees:(kind:function))
        // Inner subquery: callees:(kind:function) at span (20, 40)
        // Outer subquery: callers:(...) at span (0, 50)
        let inner_subquery = subquery_condition("callees", kind_condition("function"), 20, 40);
        let outer_subquery = subquery_condition("callers", inner_subquery, 0, 50);

        let mut out = Vec::new();
        collect_subquery_exprs(&outer_subquery, &mut out);

        // Post-order: inner appears before outer
        assert_eq!(
            out.len(),
            2,
            "should collect both inner and outer subqueries"
        );
        assert_eq!(out[0].0, (20, 40), "inner subquery span should come first");
        assert_eq!(out[1].0, (0, 50), "outer subquery span should come second");
    }

    #[test]
    fn test_collect_subquery_exprs_post_order_depth_3() {
        // Build: callers:(callees:(imports:(kind:function)))
        let innermost = subquery_condition("imports", kind_condition("function"), 30, 50);
        let middle = subquery_condition("callees", innermost, 15, 55);
        let outer = subquery_condition("callers", middle, 0, 60);

        let mut out = Vec::new();
        collect_subquery_exprs(&outer, &mut out);

        assert_eq!(out.len(), 3, "should collect all three nested subqueries");
        assert_eq!(out[0].0, (30, 50), "innermost should come first");
        assert_eq!(out[1].0, (15, 55), "middle should come second");
        assert_eq!(out[2].0, (0, 60), "outer should come last");
    }

    #[test]
    fn test_collect_subquery_exprs_and_or_branches() {
        // Build: callers:(kind:function) AND callees:(kind:method)
        let left = subquery_condition("callers", kind_condition("function"), 0, 25);
        let right = subquery_condition("callees", kind_condition("method"), 30, 55);
        let expr = Expr::And(vec![left, right]);

        let mut out = Vec::new();
        collect_subquery_exprs(&expr, &mut out);

        assert_eq!(out.len(), 2, "should collect subqueries from both branches");
        assert_eq!(out[0].0, (0, 25), "left branch subquery");
        assert_eq!(out[1].0, (30, 55), "right branch subquery");
    }

    #[test]
    fn test_collect_subquery_exprs_no_subqueries() {
        // Simple condition with no subqueries: kind:function
        let expr = kind_condition("function");

        let mut out = Vec::new();
        collect_subquery_exprs(&expr, &mut out);

        assert!(
            out.is_empty(),
            "should collect nothing for plain conditions"
        );
    }

    // ================================================================
    // FfiCall edge in references/referenced_by predicates
    // ================================================================

    use crate::graph::unified::edge::{BidirectionalEdgeStore, FfiConvention};
    use crate::graph::unified::storage::{
        AuxiliaryIndices, FileRegistry, NodeArena, StringInterner,
    };

    /// Build a `CodeGraph` with: `caller --FfiCall(C)--> target`
    fn build_ffi_graph() -> (CodeGraph, NodeId, NodeId) {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let caller_name = strings.intern("caller_fn").unwrap();
        let target_name = strings.intern("ffi_target").unwrap();
        let file_id = files.register(Path::new("test.r")).unwrap();

        let caller_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: caller_name,
                file: file_id,
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 0,
                end_line: 5,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        let target_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: target_name,
                file: file_id,
                start_byte: 200,
                end_byte: 300,
                start_line: 10,
                start_column: 0,
                end_line: 15,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        indices.add(caller_id, NodeKind::Function, caller_name, None, file_id);
        indices.add(target_id, NodeKind::Function, target_name, None, file_id);

        edges.add_edge(
            caller_id,
            target_id,
            EdgeKind::FfiCall {
                convention: FfiConvention::C,
            },
            file_id,
        );

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        (graph, caller_id, target_id)
    }

    #[test]
    fn test_ffi_call_edge_in_references_predicate() {
        let (graph, _caller_id, target_id) = build_ffi_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        // `references:ffi_target` should match because there is an FfiCall edge to it
        let result = match_references(
            &ctx,
            target_id,
            &Operator::Equal,
            &Value::String("ffi_target".to_string()),
        );
        assert!(result, "references: predicate should match FfiCall edges");
    }

    #[test]
    fn test_ffi_call_edge_in_references_subquery() {
        let (graph, caller_id, target_id) = build_ffi_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        // Simulate a subquery that matched the caller node
        let mut subquery_results = HashSet::new();
        subquery_results.insert(caller_id);

        // match_references_subquery checks incoming edges to target_id
        // and verifies the source is in the subquery result set
        let result = match_references_subquery(&ctx, target_id, Some(&subquery_results)).unwrap();
        assert!(
            result,
            "references subquery should match FfiCall edge sources"
        );
    }

    // ================================================================
    // returns: predicate (edge-based, byte-exact)
    // ================================================================

    /// Build a `CodeGraph` with two functions and an `error` type node:
    /// - `returner_fn` --TypeOf{Return}--> `error`
    /// - `plain_fn` (no outgoing TypeOf edges)
    ///
    /// Returns `(graph, returner_id, plain_id, error_type_id)`.
    fn build_returns_graph() -> (CodeGraph, NodeId, NodeId, NodeId) {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let returner_name = strings.intern("returner_fn").unwrap();
        let plain_name = strings.intern("plain_fn").unwrap();
        let error_name = strings.intern("error").unwrap();
        let file_id = files.register(Path::new("test.go")).unwrap();

        let returner_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: returner_name,
                file: file_id,
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 0,
                end_line: 5,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        let plain_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: plain_name,
                file: file_id,
                start_byte: 200,
                end_byte: 300,
                start_line: 10,
                start_column: 0,
                end_line: 15,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        let error_type_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Type,
                name: error_name,
                file: file_id,
                start_byte: 400,
                end_byte: 410,
                start_line: 20,
                start_column: 0,
                end_line: 20,
                end_column: 10,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        indices.add(
            returner_id,
            NodeKind::Function,
            returner_name,
            None,
            file_id,
        );
        indices.add(plain_id, NodeKind::Function, plain_name, None, file_id);
        indices.add(error_type_id, NodeKind::Type, error_name, None, file_id);

        edges.add_edge(
            returner_id,
            error_type_id,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Return),
                index: None,
                name: None,
            },
            file_id,
        );

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        (graph, returner_id, plain_id, error_type_id)
    }

    #[test]
    fn test_match_returns_byte_exact_hit() {
        let (graph, returner_id, _plain_id, _error_id) = build_returns_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);
        let entry = graph.nodes().get(returner_id).expect("returner exists");

        // returns:error against the function with a TypeOf{Return} edge to
        // the `error` type node should match (byte-exact).
        assert!(match_returns(
            &ctx,
            returner_id,
            entry,
            &Operator::Equal,
            &Value::String("error".to_string()),
        ));
    }

    #[test]
    fn test_match_returns_no_edges_misses() {
        let (graph, _returner_id, plain_id, _error_id) = build_returns_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);
        let entry = graph.nodes().get(plain_id).expect("plain_fn exists");

        // returns:error against a function with no TypeOf{Return} edges
        // must NOT match (the legacy substring path would have to be
        // entirely off this code path for this to hold).
        assert!(!match_returns(
            &ctx,
            plain_id,
            entry,
            &Operator::Equal,
            &Value::String("error".to_string()),
        ));
    }

    #[test]
    fn test_match_returns_byte_exact_miss_on_different_target_name() {
        let (graph, returner_id, _plain_id, _error_id) = build_returns_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);
        let entry = graph.nodes().get(returner_id).expect("returner exists");

        // returns:Error (capitalised) must NOT match `error` — byte-exact
        // is case-sensitive.  This is the property the previous
        // signature.contains substring path failed to enforce.
        assert!(!match_returns(
            &ctx,
            returner_id,
            entry,
            &Operator::Equal,
            &Value::String("Error".to_string()),
        ));
    }

    #[test]
    fn test_match_returns_rejects_non_callable_kinds() {
        let (graph, _returner_id, _plain_id, error_id) = build_returns_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);
        // The `error` node is a Type, not a Function/Method, so the
        // fast-path early-out should reject it without consulting edges.
        let entry = graph.nodes().get(error_id).expect("error type exists");

        assert!(!match_returns(
            &ctx,
            error_id,
            entry,
            &Operator::Equal,
            &Value::String("error".to_string()),
        ));
    }

    // ================================================================
    // Phase A C indirect-call precision (U18.1)
    //
    // Predicate parity with the planner surface (`sqry_query`). The three
    // new graph-eval helpers — `match_address_taken`,
    // `match_callsite_promiscuous`, `match_resolved_via` — surface the
    // same three predicates the planner already exposes via U14. SPEC
    // §3.1.3 + DESIGN §11 / §12.
    // ================================================================

    use crate::graph::unified::NodeMetadataStore;
    use crate::graph::unified::edge::ResolvedVia;

    /// Build a tiny graph with two functions `flagged` and `plain`. Caller is
    /// responsible for setting flags on the returned `NodeId`s post-hoc.
    /// Returns `(graph, flagged_id, plain_id)`.
    fn build_two_function_graph() -> (CodeGraph, NodeId, NodeId) {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let flagged_name = strings.intern("flagged").unwrap();
        let plain_name = strings.intern("plain").unwrap();
        let file_id = files.register(Path::new("u18_1.c")).unwrap();

        let mk_fn = |arena: &mut NodeArena, name, start: u32, end: u32, line: u32| -> NodeId {
            arena
                .alloc(NodeEntry {
                    kind: NodeKind::Function,
                    name,
                    file: file_id,
                    start_byte: start,
                    end_byte: end,
                    start_line: line,
                    start_column: 0,
                    end_line: line + 2,
                    end_column: 0,
                    signature: None,
                    doc: None,
                    qualified_name: None,
                    visibility: None,
                    is_async: false,
                    is_static: false,
                    is_unsafe: false,
                    body_hash: None,
                })
                .unwrap()
        };

        let flagged_id = mk_fn(&mut arena, flagged_name, 0, 50, 1);
        let plain_id = mk_fn(&mut arena, plain_name, 100, 150, 10);

        indices.add(flagged_id, NodeKind::Function, flagged_name, None, file_id);
        indices.add(plain_id, NodeKind::Function, plain_name, None, file_id);

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            NodeMetadataStore::new(),
        );
        (graph, flagged_id, plain_id)
    }

    /// `match_address_taken(true)` must return `true` for a node with the
    /// `NodeFlags::ADDRESS_TAKEN` bit set, and `false` for a sibling without
    /// it. Inverse predicate (`address_taken:false`) must hold the complement.
    #[test]
    fn match_address_taken_returns_true_when_flag_set() {
        let (mut graph, flagged_id, plain_id) = build_two_function_graph();
        graph.macro_metadata_mut().mark_address_taken(flagged_id);
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        assert!(match_address_taken(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));
        assert!(!match_address_taken(
            &ctx,
            plain_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));

        // Inverse: address_taken:false should match `plain` and NOT `flagged`.
        assert!(match_address_taken(
            &ctx,
            plain_id,
            &Operator::Equal,
            &Value::Boolean(false)
        ));
        assert!(!match_address_taken(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::Boolean(false)
        ));

        // String form `"true"` / `"false"` (covers the path through
        // `value_to_bool`'s string-coercion fallback).
        assert!(match_address_taken(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::String("true".to_string())
        ));
    }

    /// `match_callsite_promiscuous(true)` returns `true` for nodes with the
    /// `NodeFlags::CALLSITE_PROMISCUOUS` bit set.
    #[test]
    fn match_callsite_promiscuous_returns_true_when_flag_set() {
        let (mut graph, flagged_id, plain_id) = build_two_function_graph();
        graph
            .macro_metadata_mut()
            .mark_callsite_promiscuous(flagged_id);
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        assert!(match_callsite_promiscuous(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));
        assert!(!match_callsite_promiscuous(
            &ctx,
            plain_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));

        // ADDRESS_TAKEN and CALLSITE_PROMISCUOUS are independent bits — setting
        // one must not flip the other. Co-occurrence regression check mirrors
        // metadata.rs `co_occurrence_macro_and_address_taken`.
        graph.macro_metadata_mut().mark_address_taken(flagged_id);
        let ctx = GraphEvalContext::new(&graph, &pm);
        assert!(match_callsite_promiscuous(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));
        assert!(match_address_taken(
            &ctx,
            flagged_id,
            &Operator::Equal,
            &Value::Boolean(true)
        ));
    }

    /// `match_resolved_via` must filter outgoing Calls edges by their
    /// `ResolvedVia` discriminator. Build a fixture where `caller` has two
    /// outgoing Calls edges — one `Direct`, one `BindingPlane` — and confirm
    /// the helper distinguishes both, rejects `TypeMatch`, and rejects
    /// unknown enum strings.
    #[test]
    fn match_resolved_via_filters_calls_edges_by_resolution() {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let caller_name = strings.intern("caller").unwrap();
        let target_a = strings.intern("target_direct").unwrap();
        let target_b = strings.intern("target_binding").unwrap();
        let file_id = files.register(Path::new("u18_1_resolved_via.c")).unwrap();

        let mk_fn = |arena: &mut NodeArena, name, start: u32, end: u32, line: u32| {
            arena
                .alloc(NodeEntry {
                    kind: NodeKind::Function,
                    name,
                    file: file_id,
                    start_byte: start,
                    end_byte: end,
                    start_line: line,
                    start_column: 0,
                    end_line: line + 2,
                    end_column: 0,
                    signature: None,
                    doc: None,
                    qualified_name: None,
                    visibility: None,
                    is_async: false,
                    is_static: false,
                    is_unsafe: false,
                    body_hash: None,
                })
                .unwrap()
        };
        let caller_id = mk_fn(&mut arena, caller_name, 0, 50, 1);
        let target_a_id = mk_fn(&mut arena, target_a, 100, 150, 10);
        let target_b_id = mk_fn(&mut arena, target_b, 200, 250, 20);

        indices.add(caller_id, NodeKind::Function, caller_name, None, file_id);
        indices.add(target_a_id, NodeKind::Function, target_a, None, file_id);
        indices.add(target_b_id, NodeKind::Function, target_b, None, file_id);

        // Two outgoing Calls from caller: one Direct, one BindingPlane.
        edges.add_edge(
            caller_id,
            target_a_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        edges.add_edge(
            caller_id,
            target_b_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::BindingPlane,
            },
            file_id,
        );

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            NodeMetadataStore::new(),
        );
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        // Direct hits.
        assert!(match_resolved_via(
            &ctx,
            caller_id,
            &Value::String("direct".to_string())
        ));
        // BindingPlane hits.
        assert!(match_resolved_via(
            &ctx,
            caller_id,
            &Value::String("binding_plane".to_string())
        ));
        // TypeMatch must miss — no such edge in the fixture.
        assert!(!match_resolved_via(
            &ctx,
            caller_id,
            &Value::String("type_match".to_string())
        ));
        // Target nodes have no outgoing Calls — every variant must miss.
        assert!(!match_resolved_via(
            &ctx,
            target_a_id,
            &Value::String("direct".to_string())
        ));
        // Unknown enum strings are graceful misses (no panic, no false match).
        assert!(!match_resolved_via(
            &ctx,
            caller_id,
            &Value::String("not_a_real_variant".to_string())
        ));
        // Non-string values are graceful misses (e.g. someone passes a bool).
        assert!(!match_resolved_via(&ctx, caller_id, &Value::Boolean(true)));
    }
}
