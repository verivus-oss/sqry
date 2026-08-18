//! `sqry rules` — execute declarative rule-layer definitions and packs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqry_db::queries::dispatch::make_query_db_cold;
use sqry_rules::derived::requires_unsupported_beside_cache;
use sqry_rules::dsl::{RuleDefinition, load_rule_pack_str};
use sqry_rules::engine::{RuleOutput, RuleRun};
use sqry_rules::ir::{RuleEndpoint, RuleNode};
use sqry_rules::rules::{self, ShippedRule, intake, recipes, security};
use sqry_rules::witness::{RuleSeverity, RuleStep, RuleWitness};
use sqry_rules::{RuleEngine, SqryDbRuleBackend};

use crate::args::{Cli, RulesAction, RulesOutputFormat};
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;

const BESIDE_CACHE_UNSUPPORTED_MESSAGE: &str = "rule requires cross-snapshot coordination (CrossSnapshotDiff); the engine cannot source a prior snapshot yet. SimilarTo runs in-engine since L2a";
const MAX_TEXT_WITNESS_STEPS: usize = 20;

/// Runs the `sqry rules` command family.
///
/// # Errors
///
/// Returns an error if the rule selector cannot be resolved, the workspace
/// index cannot be loaded, rule execution fails to serialize output, or stdout
/// / stderr writing fails.
pub fn run_rules(cli: &Cli, action: &RulesAction) -> Result<()> {
    match action {
        RulesAction::Run {
            rule_or_pack,
            path,
            format,
        } => run_rules_pack(cli, rule_or_pack, path.as_deref(), *format),
    }
}

fn run_rules_pack(
    cli: &Cli,
    rule_or_pack: &str,
    path: Option<&str>,
    format: RulesOutputFormat,
) -> Result<()> {
    let output_format = resolve_rules_output_format(cli, format)?;
    let mut streams = OutputStreams::new();

    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        PathBuf::from,
    );
    let Some(location) = find_nearest_index(&search_path) else {
        bail!("No .sqry-index found. Run 'sqry index' first to build the graph index.");
    };
    let loaded = load_rules(rule_or_pack, &location.index_root)?;

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&location.index_root, &config, cli, no_op_reporter())
        .context("failed to load graph; run 'sqry index' to rebuild")?;
    let snapshot = Arc::new(graph.snapshot());
    let db = make_query_db_cold(Arc::clone(&snapshot), &location.index_root);
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    let results = loaded
        .rules
        .iter()
        .map(|rule| execute_loaded_rule(&engine, &backend, rule))
        .collect();
    let report = RulesRunReport {
        source: loaded.source,
        results,
    };

    match output_format {
        RulesOutputFormat::Json => {
            let payload =
                serde_json::to_string_pretty(&report).context("serializing rules report")?;
            streams.write_result(&payload)?;
        }
        RulesOutputFormat::Text => write_text_report(&mut streams, &report)?,
    }

    Ok(())
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

#[derive(Debug, Serialize)]
struct RulesRunReport {
    source: String,
    results: Vec<RulesRunResult>,
}

#[derive(Debug, Serialize)]
struct RulesRunResult {
    id: String,
    status: RuleRunStatus,
    // Authored security metadata (schema 2). Pack-authored and independent of
    // execution success, so it rides every status row; omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    severity: Option<RuleSeverity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<String>,
    output: Option<RuleOutput>,
    witness: Option<RuleWitness>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuleRunStatus {
    Ok,
    Unsupported,
    Error,
}

fn execute_loaded_rule(
    engine: &RuleEngine,
    backend: &SqryDbRuleBackend<'_>,
    rule: &LoadedRule,
) -> RulesRunResult {
    let rule_id = rule.definition.id.clone();
    if rule.requires_beside_cache || contains_unsupported_beside_cache(rule.definition.plan.root())
    {
        return RulesRunResult {
            id: rule_id,
            status: RuleRunStatus::Unsupported,
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
        Ok(RuleRun { output, witness }) => RulesRunResult {
            id: rule_id,
            status: RuleRunStatus::Ok,
            severity: rule.definition.severity,
            cwe: rule.definition.cwe.clone(),
            description: rule.definition.description.clone(),
            remediation: rule.definition.remediation.clone(),
            output: Some(output),
            witness: Some(witness),
            error: None,
        },
        Err(error) => RulesRunResult {
            id: rule_id,
            status: RuleRunStatus::Error,
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
    let path = resolve_workspace_rule_pack_path(rule_or_pack, workspace_root)?;
    let source = rule_or_pack.to_string();
    let toml = std::fs::read_to_string(&path)
        .with_context(|| format!("reading TOML rule pack {}", path.display()))?;
    let pack = load_rule_pack_str(&toml)
        .with_context(|| format!("parsing TOML rule pack {}", path.display()))?;
    Ok(LoadedRules {
        source,
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

fn resolve_workspace_rule_pack_path(rule_or_pack: &str, workspace_root: &Path) -> Result<PathBuf> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("canonicalizing workspace root {}", workspace_root.display()))?;
    let candidate = Path::new(rule_or_pack);
    let path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving TOML rule pack {rule_or_pack}"))?;
    if !path.starts_with(&workspace_root) {
        bail!(
            "rule selector '{rule_or_pack}' is not a shipped rule/pack and does not resolve to a TOML rule pack file in the workspace"
        );
    }
    if !path.is_file() {
        bail!(
            "rule selector '{rule_or_pack}' is not a shipped rule/pack and does not resolve to a TOML rule pack file in the workspace"
        );
    }
    Ok(path)
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

fn resolve_rules_output_format(cli: &Cli, format: RulesOutputFormat) -> Result<RulesOutputFormat> {
    if cli.json && format == RulesOutputFormat::Text {
        bail!("--json conflicts with rules run --format text; use --format json or omit --json");
    }
    if cli.json {
        Ok(RulesOutputFormat::Json)
    } else {
        Ok(format)
    }
}

fn write_text_report(streams: &mut OutputStreams, report: &RulesRunReport) -> Result<()> {
    streams.write_result(&format!("rules source: {}", report.source))?;
    for result in &report.results {
        streams.write_result(&format!("rule {}", result.id))?;
        streams.write_result(&format!("  status: {}", status_name(&result.status)))?;
        if let Some(severity) = result.severity {
            streams.write_result(&format!("  severity: {}", severity_name(severity)))?;
        }
        if let Some(cwe) = &result.cwe {
            streams.write_result(&format!("  cwe: {cwe}"))?;
        }
        if let Some(description) = &result.description {
            streams.write_result(&format!("  description: {description}"))?;
        }
        if let Some(remediation) = &result.remediation {
            streams.write_result(&format!("  remediation: {remediation}"))?;
        }
        if let Some(error) = &result.error {
            streams.write_result(&format!("  error: {error}"))?;
        }
        if let Some(output) = &result.output {
            streams.write_result(&format!("  output: {}", summarize_output(output)))?;
        }
        if let Some(witness) = &result.witness {
            streams.write_result(&format!(
                "  witness: {} step(s), {} citation(s), truncated={}",
                witness.steps.len(),
                witness.citations.len(),
                witness.truncated
            ))?;
            for line in format_witness_step_lines(witness) {
                streams.write_result(&line)?;
            }
        }
    }
    Ok(())
}

fn format_witness_step_lines(witness: &RuleWitness) -> Vec<String> {
    let mut lines = witness
        .steps
        .iter()
        .take(MAX_TEXT_WITNESS_STEPS)
        .map(|step| format!("    - {}", summarize_step(step)))
        .collect::<Vec<_>>();

    let omitted = witness.steps.len().saturating_sub(MAX_TEXT_WITNESS_STEPS);
    if omitted > 0 {
        lines.push(format!(
            "    - ... {omitted} additional witness step(s) omitted from text output; use --format json for full witness data"
        ));
    }

    lines
}

fn status_name(status: &RuleRunStatus) -> &'static str {
    match status {
        RuleRunStatus::Ok => "ok",
        RuleRunStatus::Unsupported => "unsupported",
        RuleRunStatus::Error => "error",
    }
}

fn summarize_output(output: &RuleOutput) -> String {
    match output {
        RuleOutput::Nodes(nodes) => format!("{} node(s)", nodes.len()),
        RuleOutput::Paths(paths) => format!("{} path(s)", paths.len()),
        RuleOutput::Subgraph { nodes, edge_count } => {
            format!(
                "subgraph with {} node(s), {edge_count} edge(s)",
                nodes.len()
            )
        }
        RuleOutput::Relations(rows) => {
            format!(
                "{:?} relation rows: {} node(s), metadata={}",
                rows.kind,
                rows.nodes.len(),
                rows.with_metadata
            )
        }
        RuleOutput::Cycles(cycles) => format!("{} cycle component(s)", cycles.len()),
        RuleOutput::References(references) => {
            format!("{} reference source node(s)", references.len())
        }
        RuleOutput::Metrics(metrics) => format!("{} metric row(s)", metrics.len()),
        RuleOutput::DiffEntries(entries) => format!("{} diff row(s)", entries.len()),
        RuleOutput::EntryPoints(entry_points) => {
            format!("{} entry-point node(s)", entry_points.len())
        }
        RuleOutput::SimilarityMatches(matches) => {
            format!("{} similarity match(es)", matches.len())
        }
        RuleOutput::Sequence(outputs) => {
            let children = outputs
                .iter()
                .map(summarize_output)
                .collect::<Vec<_>>()
                .join("; ");
            format!("{} sequence output(s): {children}", outputs.len())
        }
    }
}

fn summarize_step(step: &RuleStep) -> String {
    match step {
        RuleStep::NodeScanMatched {
            kind,
            visibility,
            match_count,
            ..
        } => format!(
            "node scan matched {match_count} node(s), kind={kind:?}, visibility={visibility:?}"
        ),
        RuleStep::EdgeTraversed {
            from,
            to,
            direction,
            edge_classification,
            depth,
        } => format!(
            "edge traversed {from:?} -> {to:?}, direction={direction:?}, class={edge_classification:?}, depth={depth}"
        ),
        RuleStep::PredicateApplied {
            predicate_kind,
            inputs,
            outputs,
        } => {
            format!("predicate {predicate_kind:?} reduced {inputs} input(s) to {outputs} output(s)")
        }
        RuleStep::SetOpEvaluated {
            op,
            lhs_card,
            rhs_card,
            result_card,
        } => format!("set op {op:?}: lhs={lhs_card}, rhs={rhs_card}, result={result_card}"),
        RuleStep::PathConstructed {
            from,
            to,
            length,
            nodes,
            ..
        } => format!(
            "path {from:?} -> {to:?}, length={length}, node_count={}",
            nodes.len()
        ),
        RuleStep::PathBudgetExhausted { reason } => {
            format!("path budget exhausted: {reason:?}")
        }
        RuleStep::RelationEdgeEmitted {
            from,
            to,
            kind,
            with_metadata,
        } => format!("relation edge {from:?} -> {to:?}, kind={kind:?}, metadata={with_metadata}"),
        RuleStep::CycleDetected {
            component_id,
            length,
            nodes,
        } => format!(
            "cycle component {component_id}, length={length}, node_count={}",
            nodes.len()
        ),
        RuleStep::ReferenceLocated {
            source,
            target,
            citation_index,
        } => format!("reference {source:?} -> {target:?}, citation={citation_index}"),
        RuleStep::MetricComputed {
            metric,
            value,
            node_count,
        } => format!("metric {metric}: value={value}, node_count={node_count}"),
        RuleStep::DiffEntryEmitted { kind, base, head } => {
            format!("diff entry {kind:?}, base={base:?}, head={head:?}")
        }
        RuleStep::EntryPointClassified { classifier, node } => {
            format!("entry point {node:?} classified by {classifier}")
        }
        RuleStep::SimilarityMatchEmitted {
            seed,
            matched,
            score,
            similarity_kind,
        } => format!(
            "similarity match seed={seed:?}, matched={matched:?}, score={score}, kind={similarity_kind:?}"
        ),
        RuleStep::RuleFired { rule_id, severity } => {
            format!(
                "rule fired {rule_id}, severity={}",
                severity_name(*severity)
            )
        }
        RuleStep::WitnessTruncated { dropped, cap } => {
            format!("witness truncated, dropped={dropped}, cap={cap}")
        }
    }
}

fn severity_name(severity: RuleSeverity) -> &'static str {
    match severity {
        RuleSeverity::Info => "info",
        RuleSeverity::Warning => "warning",
        RuleSeverity::Error => "error",
    }
}

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
