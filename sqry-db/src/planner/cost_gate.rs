//! Pre-flight cost gate for the `sqry-db` planner grammar
//! (`B_cost_gate.md` §B4 + `00_contracts.md` §3.CC-2).
//!
//! Parallel to [`sqry_core::query::cost_gate`] for the executor's
//! `Expr` AST: walks the planner [`PlanNode`] IR and rejects shapes
//! whose evaluator cost is structurally unbounded over the current
//! snapshot's arena. The wire-stable rejection envelope is the same
//! `query_too_broad` shape — the MCP boundary at
//! `sqry-mcp/src/execution/tools/planner_query.rs` (and the daemon's
//! `tool_dispatch` for the planner path) downcasts
//! [`PlannerCostGateError`] and reshapes it into the canonical 4-key
//! envelope, byte-identical to the executor-side
//! [`sqry_core::query::cost_gate::CostGateError`] mapping.
//!
//! The two gates do NOT share an error type because the IR shapes
//! differ: the executor's `Expr` carries `Field+Operator+Value`
//! triples while the planner walks `PlanNode::Filter(Predicate)`
//! whose name patterns are pre-classified into [`MatchMode`]
//! variants. Sharing the wire envelope keeps clients happy with one
//! parser for both transports.

use crate::planner::ir::{
    MatchMode, PlanNode, Predicate, PredicateValue, QueryPlan, StringPattern,
};
use thiserror::Error;

/// Re-export of the executor-side default thresholds so the
/// planner gate uses the same numeric defaults as the executor gate.
/// Per `00_contracts.md` §3.CC-3 the standalone-MCP and daemon
/// defaults are byte-identical across both paths.
pub use sqry_core::query::cost_gate::CostGateConfig as PlannerCostGateConfig;
pub use sqry_core::query::cost_gate::QUERY_TOO_BROAD_DOC_URL;
pub use sqry_core::query::cost_gate::SCOPE_FILTER_FIELDS;

/// Verdict the planner-side gate returns to the caller. Mirrors the
/// executor-side variant byte-for-byte; the MCP boundary maps
/// either error to the same `query_too_broad` envelope.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PlannerCostGateError {
    /// A planner-IR predicate's evaluator cost is structurally
    /// unbounded over the current snapshot's arena and the plan
    /// lacks the scope coupling that would narrow it.
    #[error(
        "query rejected: planner predicate `{predicate_shape}` is unbounded over {node_count} \
         nodes; add a scope filter (one of: {scope_hint}) or anchor the regex with `^` / a \
         literal prefix \u{2265} {min_prefix_len} chars. See {doc_url}"
    )]
    QueryTooBroad {
        /// Sanitised shape summary for the offending predicate
        /// (`B_cost_gate.md` §3 `predicate_shape` field). Cluster-B
        /// iter-2: every constructor in this file emits a field+op-
        /// only / mode-only shape with no raw user bytes.
        predicate_shape: String,
        /// Snapshot arena size at gate time.
        node_count: usize,
        /// Configured static node-limit threshold the gate compared
        /// against (cluster-B iter-2 BLOCKER 2 fix).
        node_limit: usize,
        /// Comma-joined scope-filter hint (always derived from
        /// [`SCOPE_FILTER_FIELDS`]).
        scope_hint: String,
        /// Active prefix-length threshold from the config.
        min_prefix_len: usize,
        /// Doc URL ([`QUERY_TOO_BROAD_DOC_URL`]).
        doc_url: &'static str,
    },
}

impl PlannerCostGateError {
    /// Build the canonical CC-2 7-key `details` payload, identical
    /// shape to [`sqry_core::query::cost_gate::CostGateError::to_query_too_broad_details`].
    /// Source discriminator is hard-wired to `"static_estimate"`.
    #[must_use]
    pub fn to_query_too_broad_details(&self) -> serde_json::Value {
        let Self::QueryTooBroad {
            predicate_shape,
            node_count,
            node_limit,
            scope_hint: _,
            min_prefix_len: _,
            doc_url,
        } = self;
        let suggested: Vec<&str> = SCOPE_FILTER_FIELDS.to_vec();
        // Cap the shape at 256 bytes (cluster-B iter-2 fix) to match
        // the executor-side `Expr::shape_summary` cap.
        let mut shape = predicate_shape.clone();
        if shape.len() > 256 {
            shape.truncate(253);
            shape.push('\u{2026}');
        }
        serde_json::json!({
            "source": sqry_core::query::cost_gate::SOURCE_STATIC_ESTIMATE,
            "kind": sqry_core::query::cost_gate::KIND_QUERY_TOO_BROAD,
            "estimated_visited_nodes": node_count,
            // Cluster-B iter-2: `limit` is the configured threshold,
            // not the snapshot's node count.
            "limit": node_limit,
            "predicate_shape": shape,
            "suggested_predicates": suggested,
            "doc_url": doc_url,
        })
    }
}

/// Top-level planner gate entrypoint.
///
/// Walks the [`QueryPlan`] tree and rejects unbounded shapes. The
/// shape rules are equivalent to the executor-side gate's:
///
/// 1. `PlanNode::NodeScan` with no kind/visibility filter and a
///    prohibitive `name_pattern` (regex without anchor / sufficient
///    literal) → reject above the arena-size threshold.
/// 2. `PlanNode::Filter(Predicate::MatchesName(prohibitive regex))`
///    → reject unless an enclosing `Chain` step couples via
///    `NodeScan { kind: Some(_) | visibility: Some(_) }` or
///    `InFile`.
///
/// # Errors
///
/// Returns [`PlannerCostGateError::QueryTooBroad`] when the plan
/// shape is structurally unbounded over an arena of the given size.
pub fn check_plan(
    plan: &QueryPlan,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> Result<(), PlannerCostGateError> {
    walk_node(&plan.root, /*scope_in_scope=*/ false, node_count, cfg)
}

// ─────────────────── internals ─────────────────────

fn cap_engaged(node_count: usize, cfg: &PlannerCostGateConfig) -> bool {
    match cfg.node_count_threshold {
        Some(0) | None => false,
        Some(threshold) => node_count > threshold,
    }
}

fn walk_node(
    node: &PlanNode,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> Result<(), PlannerCostGateError> {
    match node {
        PlanNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        } => {
            // A NodeScan with kind: Some(_) or visibility: Some(_)
            // is itself a scope filter — `kind_index` /
            // `visibility_index` narrow the arena before the
            // name_pattern is evaluated. A bare name_pattern with
            // no kind/visibility filter is the prohibitive case
            // (full-arena scan).
            let scoped = scope_in_scope || kind.is_some() || visibility.is_some();
            if let Some(name_pattern) = name_pattern {
                check_name_pattern(name_pattern, scoped, node_count, cfg)?;
            }
            Ok(())
        }
        PlanNode::EdgeTraversal { .. } => {
            // Edge traversal cost is bounded by the input set
            // size — the input set itself was gated upstream.
            Ok(())
        }
        PlanNode::Filter { predicate } => {
            check_predicate(predicate, scope_in_scope, node_count, cfg)
        }
        PlanNode::SetOp { left, right, .. } => {
            // Each side of a SetOp evaluates independently, so each
            // side must satisfy the rule on its own.
            walk_node(left, scope_in_scope, node_count, cfg)?;
            walk_node(right, scope_in_scope, node_count, cfg)
        }
        PlanNode::Chain { steps } => {
            // Chain inherits coupling: an upstream `NodeScan` with
            // kind/visibility filters narrows the running set, so
            // downstream filters can rely on that scope.
            let mut chain_scope = scope_in_scope;
            for step in steps {
                walk_node(step, chain_scope, node_count, cfg)?;
                chain_scope = chain_scope || node_introduces_scope(step);
            }
            Ok(())
        }
    }
}

fn node_introduces_scope(node: &PlanNode) -> bool {
    matches!(
        node,
        PlanNode::NodeScan { kind: Some(_), .. }
            | PlanNode::NodeScan {
                visibility: Some(_),
                ..
            }
            | PlanNode::Filter {
                predicate: Predicate::InFile(_) | Predicate::InScope(_),
            }
    )
}

fn check_predicate(
    predicate: &Predicate,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> Result<(), PlannerCostGateError> {
    match predicate {
        Predicate::And(list) => {
            // Coupling: any sibling that narrows scope (InFile,
            // InScope) couples the rest of the AND chain.
            let coupled = scope_in_scope
                || list
                    .iter()
                    .any(|p| matches!(p, Predicate::InFile(_) | Predicate::InScope(_)));
            for p in list {
                check_predicate(p, coupled, node_count, cfg)?;
            }
            Ok(())
        }
        Predicate::Or(list) => {
            for p in list {
                check_predicate(p, scope_in_scope, node_count, cfg)?;
            }
            Ok(())
        }
        Predicate::Not(inner) => check_predicate(inner, scope_in_scope, node_count, cfg),
        Predicate::MatchesName(pattern) => {
            check_name_pattern(pattern, scope_in_scope, node_count, cfg)
        }
        // Recursive subquery: walk the inner plan under the same
        // scope; its prohibitive shapes propagate to the outer
        // verdict.
        Predicate::Callers(v)
        | Predicate::Callees(v)
        | Predicate::Imports(v)
        | Predicate::Exports(v)
        | Predicate::References(v)
        | Predicate::Implements(v) => check_predicate_value(v, scope_in_scope, node_count, cfg),
        // Existence + attribute predicates are cheap or
        // index-bounded — never prohibitive. CfgCondition is a single
        // HashMap lookup + (at worst) one short stored-string parse;
        // Wraps is one outbound-edge scan from the node. Phase A (U14)
        // `address_taken` / `callsite_promiscuous` are O(1) flag lookups
        // against the metadata store; `resolved_via` is bounded by a
        // node's outgoing `Calls` degree (typically small). None of these
        // approach the cost-gate cap.
        Predicate::HasCaller
        | Predicate::HasCallee
        | Predicate::IsUnused
        | Predicate::IsDefinition(_)
        | Predicate::IsUnsafe(_)
        | Predicate::IsAddressTaken(_)
        | Predicate::ResolvedVia(_)
        | Predicate::HasCallsitePromiscuous(_)
        // Phase β joint-stubs (Plan A + Plan B): both predicates evaluate
        // against the empty side-table surface in this PR, so they are
        // strictly cheaper than `ResolvedVia` (which walks outgoing Calls).
        // They remain in the cheap class once the downstream PRs populate
        // the tables — framework lookup is a single BTreeMap probe; set
        // membership over outgoing Calls is bounded by node degree.
        | Predicate::FrameworkEq(_)
        | Predicate::ResolvedViaEq(_)
        // Structural similarity (U09): the probe's neighbour set is computed
        // once (cached LSH index probe) and memoised, then membership is an
        // O(1) set lookup per candidate — a bounded linear filter, well under
        // the cost-gate cap.
        | Predicate::ShapeSimilar(_)
        | Predicate::InFile(_)
        | Predicate::InScope(_)
        | Predicate::Returns(_)
        | Predicate::CfgCondition(_)
        | Predicate::Wraps(_) => Ok(()),
    }
}

fn check_predicate_value(
    value: &PredicateValue,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> Result<(), PlannerCostGateError> {
    match value {
        PredicateValue::Pattern(_) => Ok(()),
        PredicateValue::Regex(rx) => {
            if !cap_engaged(node_count, cfg) || scope_in_scope {
                return Ok(());
            }
            if regex_shape_is_acceptable(&rx.pattern, cfg) {
                return Ok(());
            }
            // Cluster-B iter-2: emit field+op-only shape; never echo
            // raw user pattern bytes into the wire envelope.
            Err(reject("references~=<elided>".to_string(), node_count, cfg))
        }
        PredicateValue::Subquery(plan) => walk_node(plan, scope_in_scope, node_count, cfg),
    }
}

fn check_name_pattern(
    pattern: &StringPattern,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> Result<(), PlannerCostGateError> {
    if !cap_engaged(node_count, cfg) || scope_in_scope {
        return Ok(());
    }
    // Cluster-B iter-2: emit field+op+match-mode-only shape; never
    // echo raw user pattern bytes into the wire envelope.
    let shape = match pattern.mode {
        // Exact matches use `name_index` directly — always cheap.
        MatchMode::Exact => return Ok(()),
        // Glob, prefix, suffix, and contains all need a literal
        // long enough to be a meaningful filter. Reuse the
        // configured `min_literal_len` threshold.
        MatchMode::Prefix => {
            if pattern.raw.len() > cfg.min_prefix_len {
                return Ok(());
            }
            "name:<prefix-elided>*".to_string()
        }
        MatchMode::Suffix => {
            if pattern.raw.len() > cfg.min_literal_len {
                return Ok(());
            }
            "name:*<suffix-elided>".to_string()
        }
        MatchMode::Contains => {
            if pattern.raw.len() > cfg.min_literal_len {
                return Ok(());
            }
            "name:*<contains-elided>*".to_string()
        }
        MatchMode::Glob => {
            // Approximate: extract the longest non-glob run of
            // chars and treat it as the literal lower bound.
            let max_literal = pattern
                .raw
                .split(['*', '?', '['])
                .map(str::len)
                .max()
                .unwrap_or(0);
            if max_literal > cfg.min_literal_len {
                return Ok(());
            }
            "name:<glob-elided>".to_string()
        }
    };
    Err(reject(shape, node_count, cfg))
}

fn regex_shape_is_acceptable(pattern: &str, cfg: &PlannerCostGateConfig) -> bool {
    let Ok(hir) = regex_syntax::parse(pattern) else {
        // Defer to the validator; never fire a false positive on a
        // syntactically valid-but-unusual pattern the planner
        // accepted.
        return true;
    };
    let mut extractor = regex_syntax::hir::literal::Extractor::new();
    extractor.kind(regex_syntax::hir::literal::ExtractKind::Prefix);
    let prefixes = extractor.extract(&hir);
    let longest_prefix = prefixes.literals().map_or(0, |lits| {
        lits.iter()
            .map(|lit| lit.as_bytes().len())
            .max()
            .unwrap_or(0)
    });
    if longest_prefix > cfg.min_prefix_len {
        return true;
    }
    if let Some(min_len) = hir.properties().minimum_len()
        && min_len > cfg.min_literal_len
    {
        return true;
    }
    false
}

fn reject(
    predicate_shape: String,
    node_count: usize,
    cfg: &PlannerCostGateConfig,
) -> PlannerCostGateError {
    PlannerCostGateError::QueryTooBroad {
        predicate_shape,
        node_count,
        node_limit: cfg.node_count_threshold.unwrap_or(0),
        scope_hint: SCOPE_FILTER_FIELDS.join(", "),
        min_prefix_len: cfg.min_prefix_len,
        doc_url: QUERY_TOO_BROAD_DOC_URL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::ir::RegexPattern;
    use sqry_core::graph::unified::node::kind::NodeKind;

    fn cfg() -> PlannerCostGateConfig {
        PlannerCostGateConfig::default()
    }

    fn plan(root: PlanNode) -> QueryPlan {
        QueryPlan::new(root)
    }

    #[test]
    fn planner_gate_rejects_bare_substring_below_threshold_arena_passes() {
        let p = plan(PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern {
                raw: "foo".to_string(),
                mode: MatchMode::Contains,
                case_insensitive: false,
            }),
        });
        // Below threshold — gate doesn't fire.
        check_plan(&p, 1_000, &cfg()).expect("below cap must pass");
    }

    #[test]
    fn planner_gate_rejects_bare_substring_above_threshold() {
        let p = plan(PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern {
                raw: "foo".to_string(),
                mode: MatchMode::Contains,
                case_insensitive: false,
            }),
        });
        let err = check_plan(&p, 1_000_000, &cfg()).expect_err("must reject");
        assert!(matches!(err, PlannerCostGateError::QueryTooBroad { .. }));
    }

    #[test]
    fn planner_gate_allows_kind_coupled_substring() {
        let p = plan(PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: Some(StringPattern {
                raw: "foo".to_string(),
                mode: MatchMode::Contains,
                case_insensitive: false,
            }),
        });
        check_plan(&p, 1_000_000, &cfg()).expect("kind coupling must pass");
    }

    #[test]
    fn planner_gate_allows_long_substring_without_coupling() {
        let p = plan(PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern {
                raw: "deserialize".to_string(),
                mode: MatchMode::Contains,
                case_insensitive: false,
            }),
        });
        check_plan(&p, 1_000_000, &cfg()).expect("long literal must pass");
    }

    #[test]
    fn planner_gate_rejects_prohibitive_regex_in_filter_without_coupling() {
        let p = plan(PlanNode::Chain {
            steps: vec![
                PlanNode::NodeScan {
                    kind: None,
                    visibility: None,
                    name_pattern: None,
                },
                PlanNode::Filter {
                    predicate: Predicate::References(PredicateValue::Regex(RegexPattern::new(
                        ".*foo.*",
                    ))),
                },
            ],
        });
        let err = check_plan(&p, 1_000_000, &cfg()).expect_err("must reject");
        assert!(matches!(err, PlannerCostGateError::QueryTooBroad { .. }));
    }

    #[test]
    fn planner_gate_passes_canonical_kind_only_query() {
        let p = plan(PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: None,
        });
        check_plan(&p, 1_000_000_000, &cfg()).expect("canonical kind:function must pass");
    }
}
