//! `rules_run` MCP tool (P5U10): execute declarative rule-layer rules and
//! packs through the production `RuleEngine` + `SqryDbRuleBackend`.
//!
//! This surface mirrors the P5U09 `sqry rules run` CLI: it resolves shipped
//! Rust DSL rules by stable ID or pack name, or loads a TOML rule pack from a
//! workspace-scoped path, then runs each rule against the workspace graph and
//! returns the structured output plus witness. Since L2a, `SimilarTo` runs
//! in-engine (structural neighbour query on the current snapshot); only
//! `CrossSnapshotDiff` is still reported `unsupported`, because the engine
//! cannot source a prior snapshot yet.
//!
//! Witness file/line citations ride through the standard
//! `execute_tool_for_request` redaction pipeline: `RuleCitation.file_path` is
//! a `sqry-mcp-redaction` `PATH_FIELDS` entry, so paths are redacted under the
//! `SQRY_REDACTION_PRESET` default exactly like every other MCP tool.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use sqry_db::queries::dispatch::make_query_db_cold;
use sqry_rules::derived::requires_unsupported_beside_cache;
use sqry_rules::dsl::{RuleDefinition, load_rule_pack_str};
use sqry_rules::engine::RuleRun;
use sqry_rules::ir::{RuleEndpoint, RuleNode};
use sqry_rules::rules::{self, ShippedRule, intake, recipes, security};
use sqry_rules::witness::RuleSeverity;
use sqry_rules::{RuleEngine, SqryDbRuleBackend};

use crate::engine::{canonicalize_in_workspace_enforced, engine_for_workspace};
use crate::execution::types::{RulesRunData, RulesRunResultData, RulesRunStatus, ToolExecution};
use crate::execution::utils::duration_to_ms;
use crate::tools::RulesRunParams;

const BESIDE_CACHE_UNSUPPORTED_MESSAGE: &str = "rule requires cross-snapshot coordination (CrossSnapshotDiff); the engine cannot source a prior snapshot yet. SimilarTo runs in-engine since L2a";

/// Executes the `rules_run` tool against the current workspace graph.
///
/// # Errors
///
/// - If the workspace has no unified graph (`.sqry/graph/`).
/// - If the rule-or-pack selector resolves to a TOML path that escapes the
///   workspace, is missing, or fails to parse.
pub fn execute_rules_run(params: &RulesRunParams) -> Result<ToolExecution<RulesRunData>> {
    let start = Instant::now();

    let workspace_path = if params.path == "." {
        None
    } else {
        Some(PathBuf::from(&params.path))
    };
    let workspace_engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = workspace_engine.workspace_root().to_path_buf();
    // Guard against path traversal on the workspace argument, same pattern
    // every other graph-backed tool uses.
    let _ = canonicalize_in_workspace_enforced(&params.path, &workspace_root)?;

    // Resolve the rule pack before touching the graph so a bad selector fails
    // fast (and a TOML path is confined to the workspace).
    let loaded = load_rules(&params.rule_or_pack, &workspace_root)?;

    let graph = workspace_engine
        .ensure_graph()
        .context("unified graph snapshot is required for rules_run")?;
    let snapshot = Arc::new(graph.snapshot());
    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);
    let backend = SqryDbRuleBackend::new(&db);
    let rule_engine = RuleEngine::new();

    let results = loaded
        .rules
        .iter()
        .map(|rule| execute_loaded_rule(&rule_engine, &backend, rule))
        .collect();

    let data = RulesRunData {
        selector: loaded.source,
        results,
    };

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: None,
        truncated: None,
        candidates_scanned: None,
        workspace_path: workspace_root.display().to_string(),
    })
}

#[derive(Debug, Clone)]
struct LoadedRules {
    source: String,
    rules: Vec<LoadedRule>,
}

#[derive(Debug, Clone)]
struct LoadedRule {
    definition: RuleDefinition,
    requires_beside_cache: bool,
}

impl From<ShippedRule> for LoadedRule {
    fn from(value: ShippedRule) -> Self {
        // Derive the gate solely from the plan: a stale hardcoded flag must not
        // be able to gate a rule the engine can actually run (L2a code-gate fix).
        Self {
            requires_beside_cache: contains_unsupported_beside_cache(value.definition.plan.root()),
            definition: value.definition,
        }
    }
}

fn execute_loaded_rule(
    engine: &RuleEngine,
    backend: &SqryDbRuleBackend<'_>,
    rule: &LoadedRule,
) -> RulesRunResultData {
    let rule_id = rule.definition.id.clone();
    if rule.requires_beside_cache || contains_unsupported_beside_cache(rule.definition.plan.root())
    {
        return RulesRunResultData {
            id: rule_id,
            status: RulesRunStatus::Unsupported,
            severity: rule.definition.severity,
            cwe: rule.definition.cwe.clone(),
            description: rule.definition.description.clone(),
            remediation: rule.definition.remediation.clone(),
            output: None,
            witness: None,
            error: Some(BESIDE_CACHE_UNSUPPORTED_MESSAGE.to_string()),
        };
    }

    // Authored severity overrides the caller default; absent falls back to Info.
    let severity = rule.definition.severity.unwrap_or(RuleSeverity::Info);
    match engine.run_named(
        backend,
        &rule.definition.plan,
        &rule.definition.id,
        severity,
    ) {
        Ok(RuleRun { output, witness }) => RulesRunResultData {
            id: rule_id,
            status: RulesRunStatus::Ok,
            severity: rule.definition.severity,
            cwe: rule.definition.cwe.clone(),
            description: rule.definition.description.clone(),
            remediation: rule.definition.remediation.clone(),
            output: Some(output),
            witness: Some(witness),
            error: None,
        },
        Err(error) => RulesRunResultData {
            id: rule_id,
            status: RulesRunStatus::Error,
            severity: rule.definition.severity,
            cwe: rule.definition.cwe.clone(),
            description: rule.definition.description.clone(),
            remediation: rule.definition.remediation.clone(),
            output: None,
            witness: None,
            error: Some(error.to_string()),
        },
    }
}

fn load_rules(rule_or_pack: &str, workspace_root: &Path) -> Result<LoadedRules> {
    match rule_or_pack {
        "bbnty.recipes" => Ok(LoadedRules {
            source: rule_or_pack.to_string(),
            rules: recipes::bbnty_recipe_rules()
                .into_iter()
                .map(LoadedRule::from)
                .collect(),
        }),
        "bbnty.intake" => Ok(LoadedRules {
            source: rule_or_pack.to_string(),
            rules: intake::standard_intake_rules()
                .into_iter()
                .map(LoadedRule::from)
                .collect(),
        }),
        "bbnty.security" => Ok(LoadedRules {
            source: rule_or_pack.to_string(),
            rules: security::security_rules()
                .into_iter()
                .map(LoadedRule::from)
                .collect(),
        }),
        "bbnty.all" => Ok(LoadedRules {
            source: rule_or_pack.to_string(),
            rules: rules::shipped_rules()
                .into_iter()
                .map(LoadedRule::from)
                .collect(),
        }),
        shipped_id => {
            if let Some(rule) = rules::shipped_rules()
                .into_iter()
                .find(|rule| rule.id() == shipped_id)
            {
                return Ok(LoadedRules {
                    source: shipped_id.to_string(),
                    rules: vec![LoadedRule::from(rule)],
                });
            }
            load_toml_rule_pack(rule_or_pack, workspace_root)
        }
    }
}

fn load_toml_rule_pack(rule_or_pack: &str, workspace_root: &Path) -> Result<LoadedRules> {
    // Confine the TOML path to the workspace; reject traversal before any read.
    let resolved = canonicalize_in_workspace_enforced(rule_or_pack, workspace_root)
        .with_context(|| format!("resolving TOML rule pack {rule_or_pack}"))?;
    if !resolved.is_file() {
        bail!(
            "rule selector '{rule_or_pack}' is not a shipped rule/pack and does not resolve to a TOML rule pack file in the workspace"
        );
    }
    let toml = std::fs::read_to_string(&resolved)
        .with_context(|| format!("reading TOML rule pack {}", resolved.display()))?;
    let pack = load_rule_pack_str(&toml)
        .with_context(|| format!("parsing TOML rule pack {}", resolved.display()))?;
    Ok(LoadedRules {
        source: rule_or_pack.to_string(),
        rules: pack
            .rules
            .into_iter()
            .map(|definition| {
                let requires_beside_cache =
                    contains_unsupported_beside_cache(definition.plan.root());
                LoadedRule {
                    definition,
                    requires_beside_cache,
                }
            })
            .collect(),
    })
}

/// Recursively detects whether any node in the rule IR routes through a
/// beside-cache primitive the engine cannot run yet (`CrossSnapshotDiff`).
/// SimilarTo runs in-engine since L2a. Mirrors the CLI helper so both
/// surfaces agree on which rules are `unsupported`.
fn contains_unsupported_beside_cache(node: &RuleNode) -> bool {
    requires_unsupported_beside_cache(node)
        || child_nodes(node)
            .iter()
            .any(|child| contains_unsupported_beside_cache(child))
}

fn child_nodes(node: &RuleNode) -> Vec<&RuleNode> {
    match node {
        RuleNode::SetOp { left, right, .. } => vec![left.as_ref(), right.as_ref()],
        RuleNode::Chain { steps } => steps.iter().collect(),
        RuleNode::PathQuery {
            from, to, avoid, ..
        } => {
            let mut children = endpoint_children([from, to]);
            if let Some(avoid) = avoid {
                children.extend(endpoint_children([avoid]));
            }
            children
        }
        RuleNode::SubgraphExtract { seeds, .. } => endpoint_children([seeds]),
        RuleNode::RelationEdges { from, .. } => endpoint_children([from]),
        RuleNode::ReferencesAt { target } => endpoint_children([target]),
        RuleNode::SimilarTo { seed, scope, .. } => {
            let mut children = endpoint_children([seed]);
            if let Some(scope) = scope {
                children.extend(endpoint_children([scope]));
            }
            children
        }
        RuleNode::NodeScan { .. }
        | RuleNode::EdgeTraversal { .. }
        | RuleNode::Filter { .. }
        | RuleNode::CycleWitness { .. }
        | RuleNode::ComplexityAggregate { .. }
        | RuleNode::CrossSnapshotDiff { .. }
        | RuleNode::EntryPointUnion { .. } => Vec::new(),
    }
}

fn endpoint_children<const N: usize>(endpoints: [&RuleEndpoint; N]) -> Vec<&RuleNode> {
    endpoints
        .into_iter()
        .filter_map(|endpoint| match endpoint {
            RuleEndpoint::Nodes(_) => None,
            RuleEndpoint::Query(node) => Some(node.as_ref()),
        })
        .collect()
}

#[cfg(test)]
#[path = "rules/tests.rs"]
mod tests;
