//! `sqry_query` MCP tool — structural query execution through the sqry-db
//! planner pipeline (parse → compile → fuse → execute).
//!
//! DB13 scope: this tool is parallel to the legacy `run_query` CLI path. DB14+
//! migrates the traversal handlers onto the planner; once migration completes
//! the legacy path is deleted.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};

use sqry_db::planner::ir::{PlanNode, Predicate};
use sqry_db::planner::{execute_plan, parse_query};
use sqry_db::queries::dispatch::make_query_db_cold;

use crate::engine::{canonicalize_in_workspace_enforced, engine_for_workspace};
use crate::execution::types::{ReindexRequiredData, SqryQueryData, SqryQueryHit, ToolExecution};
use crate::execution::utils::duration_to_ms;
use crate::tools::SqryQueryParams;

/// Default upper bound when the client omits `limit`. Mirrors the CLI default
/// so both frontends behave identically.
const DEFAULT_LIMIT: usize = 1_000;

/// Upper cap on the limit parameter — prevents a single tool call from
/// serialising tens of thousands of results into the MCP channel.
const MAX_LIMIT: usize = 10_000;

const DEFINITION_SIGNAL_REINDEX_REASON: &str =
    "definition signal requires a reindex (snapshot predates definition fidelity marker)";

fn definition_reindex_data() -> ReindexRequiredData {
    ReindexRequiredData {
        reason: DEFINITION_SIGNAL_REINDEX_REASON.to_string(),
    }
}

fn definition_reindex_execution(
    query: String,
    workspace_path: String,
    execution_ms: u64,
) -> ToolExecution<SqryQueryData> {
    let data = SqryQueryData {
        query,
        total_matches: 0,
        truncated: false,
        hits: Vec::new(),
        reindex_required: Some(definition_reindex_data()),
    };

    ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms,
        next_page_token: None,
        total: Some(0),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path,
    }
}

/// Executes the `sqry_query` tool against the current workspace graph.
///
/// # Errors
///
/// - If the workspace has no unified graph (`.sqry/graph/`).
/// - If the text query fails to parse or the resulting plan fails validation.
pub fn execute_sqry_query(params: &SqryQueryParams) -> Result<ToolExecution<SqryQueryData>> {
    let start = Instant::now();

    let workspace_path = if params.path == "." {
        None
    } else {
        Some(std::path::PathBuf::from(&params.path))
    };
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    // Guard against path traversal — same pattern other tools use.
    let _ = canonicalize_in_workspace_enforced(&params.path, &workspace_root)?;

    let graph = engine
        .ensure_graph()
        .context("unified graph snapshot is required for sqry_query")?;

    let mut plan =
        parse_query(&params.query).map_err(|err| anyhow::anyhow!("query parse error: {err}"))?;

    // Phase β joint-stubs: overlay the MCP-level `framework` and
    // `resolved_via` filter params as AND filters on the parsed plan.
    // The planner compiles / fuses / cost-gates / **evaluates** both
    // `Predicate::FrameworkEq` and `Predicate::ResolvedViaEq` end-to-end
    // (`sqry-db/src/planner/execute.rs::check_predicate`); we wrap the
    // root in a `Chain` that pipes the existing scan output through a
    // trailing `Filter`. Predicate-evaluation coverage lives in
    // `sqry-db/tests/phase_beta_predicate_evaluation.rs`; the
    // overlay-translation tests below pin the params→plan-shape contract.
    overlay_phase_beta_filters(&mut plan.root, params);

    let snapshot = Arc::new(graph.snapshot());

    if plan.uses_definition_predicate() && !snapshot.definition_signal_present() {
        return Ok(definition_reindex_execution(
            params.query.clone(),
            workspace_root.display().to_string(),
            duration_to_ms(start.elapsed()),
        ));
    }

    // Pre-flight cost gate (`B_cost_gate.md` §B4 + `00_contracts.md`
    // §3.CC-2). Inspects the planner IR shape against the snapshot's
    // arena size and rejects unbounded shapes before `execute_plan`
    // ever scans a node. The MCP boundary downcast at
    // `sqry-mcp/src/server.rs::execute_tool_with_timeout` (and the
    // daemon-side equivalent) reshapes `PlannerCostGateError` into
    // the canonical `RpcError::query_too_broad` envelope —
    // byte-identical wire shape to the executor-side gate's output.
    sqry_db::planner::cost_gate::check_plan(
        &plan,
        snapshot.nodes().len(),
        &sqry_db::planner::cost_gate::PlannerCostGateConfig::default(),
    )
    .map_err(anyhow::Error::from)?;

    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);

    let node_ids = execute_plan(&plan, &db);
    let total_matches = node_ids.len() as u64;

    let limit = params
        .limit
        .map_or(DEFAULT_LIMIT, |n| usize::try_from(n).unwrap_or(usize::MAX))
        .min(MAX_LIMIT);

    let truncated = node_ids.len() > limit;
    let mut hits: Vec<SqryQueryHit> = Vec::with_capacity(node_ids.len().min(limit));
    for node_id in node_ids.into_iter().take(limit) {
        let Some(entry) = snapshot.nodes().get(node_id) else {
            continue;
        };
        let strings = snapshot.strings();
        let files = snapshot.files();
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| name.clone(), |s| s.to_string());
        let file = files
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let visibility = entry
            .visibility
            .and_then(|sid| strings.resolve(sid))
            .map(|s| s.to_string());

        hits.push(SqryQueryHit {
            name,
            qualified_name,
            kind: entry.kind.as_str().to_string(),
            file,
            line: entry.start_line,
            visibility,
        });
    }

    let data = SqryQueryData {
        query: params.query.clone(),
        total_matches,
        truncated,
        hits,
        reindex_required: None,
    };

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total_matches),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: workspace_root.display().to_string(),
    })
}

/// Overlay the Phase β joint-stubs `framework` / `resolved_via` MCP filter
/// params onto a parsed [`PlanNode`] tree as a trailing
/// [`PlanNode::Filter`] in a [`PlanNode::Chain`].
///
/// This is the converter-side wiring codex `iter_1` §Check 9 mandated: the
/// MCP boundary used to drop both fields; this helper ensures they reach
/// the planner. When the predicate vector is empty (the caller passed
/// `None` for both, the back-compat default), the plan is left
/// untouched.
fn overlay_phase_beta_filters(root: &mut PlanNode, params: &SqryQueryParams) {
    let mut extra: Vec<Predicate> = Vec::new();
    if let Some(framework_param) = params.framework {
        let framework: sqry_core::schema::FrameworkId = framework_param.into();
        extra.push(Predicate::FrameworkEq(framework));
    }
    if let Some(via_params) = &params.resolved_via
        && !via_params.is_empty()
    {
        let set: Vec<sqry_core::schema::ResolvedVia> =
            via_params.iter().copied().map(Into::into).collect();
        extra.push(Predicate::ResolvedViaEq(set));
    }
    if extra.is_empty() {
        return;
    }

    let predicate = if extra.len() == 1 {
        extra.remove(0)
    } else {
        Predicate::And(extra)
    };
    let new_filter = PlanNode::Filter { predicate };

    // Replace the root with a Chain containing the existing root then the
    // new filter. If the root is already a Chain, append to its steps.
    let owned = std::mem::replace(
        root,
        PlanNode::Chain {
            steps: Vec::with_capacity(0),
        },
    );
    let chain = match owned {
        PlanNode::Chain { mut steps } => {
            steps.push(new_filter);
            PlanNode::Chain { steps }
        }
        other => PlanNode::Chain {
            steps: vec![other, new_filter],
        },
    };
    *root = chain;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::params::{FrameworkIdParam, ResolvedViaParam};

    fn make_params(query: &str) -> SqryQueryParams {
        SqryQueryParams {
            query: query.to_string(),
            path: ".".to_string(),
            limit: None,
            budget_rows: None,
            framework: None,
            resolved_via: None,
        }
    }

    #[test]
    fn definition_reindex_data_reason_is_stable() {
        let data = definition_reindex_data();
        assert_eq!(
            data.reason,
            "definition signal requires a reindex (snapshot predates definition fidelity marker)"
        );
    }

    #[test]
    fn definition_reindex_execution_shape_is_stable() {
        let execution = definition_reindex_execution(
            "kind:function items".to_string(),
            "/workspace".to_string(),
            7,
        );

        assert!(!execution.used_index);
        assert!(execution.used_graph);
        assert_eq!(execution.execution_ms, 7);
        assert_eq!(execution.total, Some(0));
        assert_eq!(execution.truncated, Some(false));
        assert_eq!(execution.workspace_path, "/workspace");
        assert_eq!(execution.data.query, "kind:function items");
        assert_eq!(execution.data.total_matches, 0);
        assert!(!execution.data.truncated);
        assert!(execution.data.hits.is_empty());
        assert_eq!(
            execution
                .data
                .reindex_required
                .as_ref()
                .map(|data| data.reason.as_str()),
            Some(
                "definition signal requires a reindex (snapshot predates definition fidelity marker)"
            )
        );

        let json = serde_json::to_value(&execution.data).expect("serialize");
        assert!(json.get("reindexRequired").is_some());
    }

    #[test]
    fn overlay_noop_when_both_params_absent() {
        let mut plan = parse_query("kind:function").expect("parse");
        let original = plan.clone();
        let params = make_params("kind:function");
        overlay_phase_beta_filters(&mut plan.root, &params);
        // Plan unchanged.
        assert_eq!(format!("{:?}", plan), format!("{:?}", original));
    }

    #[test]
    fn overlay_appends_framework_eq_predicate() {
        let mut plan = parse_query("kind:function").expect("parse");
        let mut params = make_params("kind:function");
        params.framework = Some(FrameworkIdParam::Flask);
        overlay_phase_beta_filters(&mut plan.root, &params);
        // The root must contain a FrameworkEq filter — verify via debug
        // string match (cheap, no public Plan walker yet).
        let debug = format!("{:?}", plan.root);
        assert!(
            debug.contains("FrameworkEq(Flask)"),
            "missing FrameworkEq(Flask) in plan: {debug}"
        );
    }

    #[test]
    fn overlay_appends_resolved_via_eq_predicate() {
        let mut plan = parse_query("kind:function").expect("parse");
        let mut params = make_params("kind:function");
        params.resolved_via = Some(vec![
            ResolvedViaParam::Direct,
            ResolvedViaParam::VirtualDispatch,
        ]);
        overlay_phase_beta_filters(&mut plan.root, &params);
        let debug = format!("{:?}", plan.root);
        assert!(
            debug.contains("ResolvedViaEq("),
            "missing ResolvedViaEq in plan: {debug}"
        );
        assert!(
            debug.contains("Direct") && debug.contains("VirtualDispatch"),
            "missing requested variants in plan: {debug}"
        );
    }

    #[test]
    fn overlay_combines_both_with_and() {
        let mut plan = parse_query("kind:function").expect("parse");
        let mut params = make_params("kind:function");
        params.framework = Some(FrameworkIdParam::Spring);
        params.resolved_via = Some(vec![ResolvedViaParam::VirtualDispatch]);
        overlay_phase_beta_filters(&mut plan.root, &params);
        let debug = format!("{:?}", plan.root);
        // Both predicates must appear; And combinator wraps them.
        assert!(
            debug.contains("FrameworkEq(Spring)"),
            "missing FrameworkEq(Spring): {debug}"
        );
        assert!(
            debug.contains("VirtualDispatch"),
            "missing VirtualDispatch: {debug}"
        );
        assert!(debug.contains("And("), "missing And combinator: {debug}");
    }
}
