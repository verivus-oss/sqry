//! `generate_overview` tool execution: the agent-facing one-call orientation
//! map.
//!
//! Composes the same sections as the `sqry overview` CLI report (summary +
//! health, hubs, subsystems + couplings, hotspots, potential issues, and a set
//! of ready-to-run sqry queries) from the same underlying primitives:
//!
//! - Hubs / subsystems / hotspots come from the deterministic `sqry-core`
//!   analysis primitives ([`rank_hubs`], [`aggregate_subsystems`],
//!   [`rank_hotspots`]) that the CLI report also composes.
//! - Summary stats reuse the exact `get_insights` computations
//!   ([`super::introspection::count_symbol_stats`] /
//!   [`super::introspection::count_edge_stats`]).
//! - Cycles / unused route through the same sqry-db derived queries the
//!   standalone `find_cycles` / `find_unused` tools dispatch; duplicates reuse
//!   the same graph duplicate builder as `find_duplicates`.
//!
//! # Health counts
//!
//! Unlike `get_insights` (which reports cheap structural estimates), the
//! overview's health block reuses the real cycle / unused / duplicate totals it
//! already computes for the `issues` section, so the summary and the issues
//! stay internally consistent (matching the `sqry overview` CLI report).
//!
//! # Redaction
//!
//! Every emitted location is workspace-relative (via
//! [`node_location_for_reporting`]), so no absolute host path is ever emitted
//! regardless of preset. The standalone server's response redactor still walks
//! the payload under its active preset, so the tool is redaction-aware like
//! every other graph-backed MCP tool.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::analysis::{
    Coupling, HubMetric, HubOpts, KindMask, Subsystem, SubsystemOpts, aggregate_subsystems,
    rank_hotspots, rank_hubs,
};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::query::{
    CircularType, DuplicateConfig, DuplicateType, UnusedScope, build_duplicate_groups_graph,
};

use crate::execution::location::node_location_for_reporting;
use crate::execution::types::{
    GenerateOverviewData, HealthIndicatorsData, LanguageStatsData, OverviewCouplingData,
    OverviewDuplicateData, OverviewFanOutData, OverviewHotspotData, OverviewHubData,
    OverviewIssuesData, OverviewNamedLocationData, OverviewSubsystemData, OverviewSubsystemsData,
    OverviewSummaryData, ToolExecution,
};
use crate::execution::utils::duration_to_ms;
use crate::tools::GenerateOverviewArgs;

/// Every report section, in canonical report order.
pub(crate) const ALL_SECTIONS: &[&str] = &[
    "summary",
    "hubs",
    "subsystems",
    "hotspots",
    "issues",
    "questions",
];

/// Parse and validate the `--sections` list into an ordered inclusion set.
///
/// `None` yields an empty vec, which the composer treats as "all sections".
/// A present-but-empty-after-split value or an unknown section name is an
/// error (the caller maps it to a validation error).
pub(crate) fn parse_sections(raw: Option<&str>) -> Result<Vec<String>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut chosen: Vec<String> = Vec::new();
    for name in raw.split(',') {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        if !ALL_SECTIONS.contains(&name.as_str()) {
            return Err(format!(
                "Invalid section '{name}'. Valid sections: {}",
                ALL_SECTIONS.join(", ")
            ));
        }
        if !chosen.contains(&name) {
            chosen.push(name);
        }
    }
    if chosen.is_empty() {
        return Err(
            "sections was empty after parsing; supply at least one valid section".to_string(),
        );
    }
    // Emit in canonical report order regardless of the caller's ordering.
    Ok(ALL_SECTIONS
        .iter()
        .filter(|s| chosen.iter().any(|c| c == *s))
        .map(|s| (*s).to_string())
        .collect())
}

/// Whether `name` is selected. An empty selection means "all sections".
fn section_selected(sections: &[String], name: &str) -> bool {
    sections.is_empty() || sections.iter().any(|s| s == name)
}

/// Execute the `generate_overview` tool (standalone rmcp path).
///
/// Resolves the workspace and acquires the graph, then delegates to the
/// daemon/SqryServer-shared [`inner::execute_generate_overview`].
///
/// # Errors
///
/// Returns an error if workspace resolution or graph acquisition fails.
pub fn execute_generate_overview(
    args: &GenerateOverviewArgs,
) -> Result<ToolExecution<GenerateOverviewData>> {
    // Pre-refactor timing: `start` fires before engine resolution, then threads
    // into the shared `*_for_daemon` core so the analysis body exists once.
    let start = Instant::now();
    let ctx = crate::engine::acquire_workspace_context_scoped(&args.path)?;
    tracing::debug!(
        path = %args.path,
        top = args.top,
        group_depth = args.group_depth,
        "Executing generate_overview tool"
    );
    crate::daemon_adapter::execute_generate_overview_for_daemon(&ctx, args, start)
}

// -------------------------------------------------------------------------
// Node resolution helpers
// -------------------------------------------------------------------------

/// Resolve a node's display label (qualified name preferred, else simple name).
fn resolve_label(snapshot: &GraphSnapshot, node: NodeId) -> Option<String> {
    let entry = snapshot.get_node(node)?;
    entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .or_else(|| snapshot.strings().resolve(entry.name))
        .map(|s| s.to_string())
}

/// Build a workspace-relative `path:line` location string for a node.
fn node_location(graph: &CodeGraph, node: NodeId, workspace_root: &std::path::Path) -> String {
    node_location_for_reporting(graph, node, workspace_root).map_or_else(
        || "unknown".to_string(),
        |loc| {
            if loc.line > 0 {
                format!("{}:{}", loc.file_path, loc.line)
            } else {
                loc.file_path
            }
        },
    )
}

/// Resolve a node into `(name, kind, workspace-relative location)`.
fn node_display(
    graph: &CodeGraph,
    snapshot: &GraphSnapshot,
    node: NodeId,
    workspace_root: &std::path::Path,
) -> (String, String, String) {
    match snapshot.get_node(node) {
        Some(entry) => {
            let name = entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());
            let kind = format!("{:?}", entry.kind);
            let location = node_location(graph, node, workspace_root);
            (name, kind, location)
        }
        None => ("?".to_string(), "?".to_string(), "unknown".to_string()),
    }
}

// -------------------------------------------------------------------------
// Derived-query helpers (shared cache behaviour with find_cycles/find_unused)
// -------------------------------------------------------------------------

/// Materialize cycle SCC node-id rings into qualified-name rings via the same
/// `CyclesQuery` the standalone `find_cycles` tool dispatches.
fn compute_cycle_rings(
    db: &sqry_db::QueryDb,
    snapshot: &GraphSnapshot,
    max_results: usize,
) -> Vec<Vec<String>> {
    let key = sqry_db::queries::CyclesKey {
        circular_type: CircularType::Calls,
        bounds: sqry_db::queries::CycleBounds {
            min_depth: 2,
            max_depth: None,
            max_results,
            should_include_self_loops: false,
        },
    };
    let cycle_node_ids = db.get::<sqry_db::queries::CyclesQuery>(&key);
    cycle_node_ids
        .iter()
        .map(|cycle| {
            cycle
                .iter()
                .filter_map(|&node_id| resolve_label(snapshot, node_id))
                .collect::<Vec<String>>()
        })
        .filter(|ring| !ring.is_empty())
        .collect()
}

/// Fetch unused node ids for a scope via the same `UnusedQuery` + binding-plane
/// post-filter the standalone `find_unused` tool uses.
fn compute_unused(
    db: &sqry_db::QueryDb,
    snapshot: &Arc<GraphSnapshot>,
    scope: UnusedScope,
    max_results: usize,
) -> Vec<NodeId> {
    let key = sqry_db::queries::UnusedKey { scope, max_results };
    let raw = db.get::<sqry_db::queries::UnusedQuery>(&key);
    sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter(&raw, snapshot, db)
}

/// Body-duplicate groups (size > 1) via the same graph duplicate builder as
/// `find_duplicates`.
fn compute_duplicates(graph: &CodeGraph, max_results: usize) -> Vec<OverviewDuplicateData> {
    let config = DuplicateConfig {
        threshold: 1.0,
        max_results,
        is_exact_only: true,
        ..Default::default()
    };
    let groups = build_duplicate_groups_graph(DuplicateType::Body, graph, &config);
    let strings = graph.strings();
    let mut items: Vec<OverviewDuplicateData> = groups
        .into_iter()
        .filter(|g| g.node_ids.len() > 1)
        .map(|group| {
            let members: Vec<String> = group
                .node_ids
                .iter()
                .filter_map(|&node_id| {
                    let entry = graph.nodes().get(node_id)?;
                    entry
                        .qualified_name
                        .and_then(|id| strings.resolve(id))
                        .or_else(|| strings.resolve(entry.name))
                        .map(|s| s.to_string())
                })
                .collect();
            let group_id = group.body_hash_128.map_or_else(
                || format!("{:016x}", group.hash),
                |body_hash| format!("{body_hash}"),
            );
            OverviewDuplicateData {
                count: members.len(),
                group_id,
                members,
            }
        })
        .filter(|g| g.count > 1)
        .collect();
    items.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.group_id.cmp(&b.group_id))
    });
    items
}

// -------------------------------------------------------------------------
// Suggested questions (templated sqry command lines)
// -------------------------------------------------------------------------

/// A symbol name is safe to embed in a suggested command line if it is
/// non-empty and free of quotes/whitespace/control characters.
fn is_safe_arg(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control())
}

/// Template ready-to-run sqry command lines from the computed findings,
/// mirroring the CLI report's suggested-questions section.
fn build_questions(
    snapshot: &GraphSnapshot,
    hubs: &[OverviewHubData],
    subsystems: &[Subsystem],
    couplings: &[Coupling],
    cycles: &[Vec<String>],
    unused_public: &[NodeId],
    duplicates: &[OverviewDuplicateData],
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    // Per top hub (up to 3): impact + direct-callers.
    for hub in hubs.iter().take(3) {
        if is_safe_arg(&hub.name) {
            lines.push(format!("sqry impact \"{}\"", hub.name));
            lines.push(format!("sqry graph direct-callers \"{}\"", hub.name));
        }
    }

    // If cycles exist: list them, and trace a named pair when available.
    if !cycles.is_empty() {
        lines.push("sqry cycles".to_string());
        if let Some(ring) = cycles.first()
            && ring.len() >= 2
            && is_safe_arg(&ring[0])
            && is_safe_arg(&ring[1])
        {
            lines.push(format!(
                "sqry graph trace-path \"{}\" \"{}\"",
                ring[0], ring[1]
            ));
        }
    }

    // Top coupling: trace between the two subsystems' representatives.
    if let Some(coupling) = couplings.first() {
        let rep_by_key: HashMap<&str, String> = subsystems
            .iter()
            .filter_map(|s| {
                resolve_label(snapshot, s.representative).map(|name| (s.key.as_str(), name))
            })
            .collect();
        if let (Some(from), Some(to)) = (
            rep_by_key.get(coupling.from.as_str()),
            rep_by_key.get(coupling.to.as_str()),
        ) && is_safe_arg(from)
            && is_safe_arg(to)
        {
            lines.push(format!("sqry graph trace-path \"{from}\" \"{to}\""));
        }
    }

    // Unused public APIs.
    if !unused_public.is_empty() {
        lines.push("sqry unused --scope public".to_string());
    }

    // Duplicates.
    if !duplicates.is_empty() {
        lines.push("sqry duplicates".to_string());
    }

    // Always: search for the top hub (or top subsystem representative).
    let seed = hubs
        .first()
        .map(|h| h.name.clone())
        .filter(|n| is_safe_arg(n))
        .or_else(|| {
            subsystems
                .first()
                .and_then(|s| resolve_label(snapshot, s.representative))
                .filter(|n| is_safe_arg(n))
        });
    if let Some(seed) = seed {
        lines.push(format!("sqry search \"{seed}\" ."));
    }

    lines
}

pub(crate) mod inner {
    use super::{
        ALL_SECTIONS, Arc, CodeGraph, GenerateOverviewArgs, GenerateOverviewData,
        HealthIndicatorsData, HubMetric, HubOpts, Instant, KindMask, LanguageStatsData,
        OverviewCouplingData, OverviewFanOutData, OverviewHotspotData, OverviewHubData,
        OverviewIssuesData, OverviewNamedLocationData, OverviewSubsystemData,
        OverviewSubsystemsData, OverviewSummaryData, SubsystemOpts, ToolExecution, UnusedScope,
        aggregate_subsystems, build_questions, compute_cycle_rings, compute_duplicates,
        compute_unused, duration_to_ms, node_display, rank_hotspots, rank_hubs, resolve_label,
        section_selected,
    };
    use crate::daemon_adapter::WorkspaceContext;

    /// Daemon/SqryServer-shared body for `generate_overview`.
    #[allow(
        clippy::too_many_lines,
        reason = "composes six report sections in one pass over shared analyses; splitting would obscure the single-computation data-flow"
    )]
    pub(crate) fn execute_generate_overview(
        ctx: &WorkspaceContext,
        args: &GenerateOverviewArgs,
        start: Instant,
    ) -> anyhow::Result<ToolExecution<GenerateOverviewData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph: &CodeGraph = &ctx.graph;
        let snapshot = Arc::new(graph.snapshot());

        let sections = &args.sections;
        let top = args.top;
        let node_count = snapshot.nodes().len();

        // Raw analyses (computed once; cheap integer sweeps + cached derived
        // queries). One cold-loaded derived DB serves both cycle and unused
        // queries, matching find_cycles / find_unused cache behaviour.
        let hub_ranks = rank_hubs(
            &snapshot,
            &HubOpts {
                top,
                by: HubMetric::FanIn,
                kinds: KindMask::default(),
            },
        );
        let fan_out_ranks = rank_hubs(
            &snapshot,
            &HubOpts {
                top,
                by: HubMetric::FanOut,
                kinds: KindMask::default(),
            },
        );
        let (subsystems, couplings) = aggregate_subsystems(
            &snapshot,
            &SubsystemOpts {
                top,
                group_depth: args.group_depth,
            },
        );
        let hotspots_raw = rank_hotspots(&snapshot, top);

        let db =
            sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), workspace_root);
        let cycles_all = compute_cycle_rings(&db, &snapshot, node_count);
        let unused_all = compute_unused(&db, &snapshot, UnusedScope::All, node_count);
        let unused_public = compute_unused(&db, &snapshot, UnusedScope::Public, node_count);
        let duplicate_groups = compute_duplicates(graph, node_count);

        // ---- Summary ----
        let summary = if section_selected(sections, "summary") {
            let stats = super::super::introspection::count_symbol_stats(&snapshot);
            let (total_edges, cross_language_edges) =
                super::super::introspection::count_edge_stats(&snapshot);
            let mut languages: Vec<LanguageStatsData> = stats
                .lang_file_counts
                .iter()
                .map(|(lang, &files)| LanguageStatsData {
                    language: lang.clone(),
                    files,
                    symbols: *stats.lang_symbol_counts.get(lang).unwrap_or(&0),
                })
                .collect();
            languages.sort_by(|a, b| {
                b.files
                    .cmp(&a.files)
                    .then_with(|| a.language.cmp(&b.language))
            });
            Some(OverviewSummaryData {
                total_files: stats.total_files,
                total_symbols: stats.total_symbols,
                total_edges,
                languages,
                health: HealthIndicatorsData {
                    cycles: cycles_all.len(),
                    unused_symbols: unused_all.len(),
                    duplicate_groups: duplicate_groups.len(),
                    cross_language_edges,
                },
            })
        } else {
            None
        };

        // ---- Hubs ----
        let hub_items: Vec<OverviewHubData> = hub_ranks
            .iter()
            .map(|h| {
                let (name, kind, location) = node_display(graph, &snapshot, h.node, workspace_root);
                OverviewHubData {
                    name,
                    kind,
                    location,
                    fan_in: h.fan_in,
                    fan_out: h.fan_out,
                }
            })
            .collect();
        let hubs = section_selected(sections, "hubs").then(|| hub_items.clone());

        // ---- Subsystems ----
        let subsystems_out = section_selected(sections, "subsystems").then(|| {
            let subsystem_items: Vec<OverviewSubsystemData> = subsystems
                .iter()
                .map(|s| OverviewSubsystemData {
                    key: s.key.clone(),
                    size: s.size,
                    internal_edges: s.internal_edges,
                    representative: resolve_label(&snapshot, s.representative)
                        .unwrap_or_else(|| "?".to_string()),
                })
                .collect();
            let coupling_items: Vec<OverviewCouplingData> = couplings
                .iter()
                .map(|c| OverviewCouplingData {
                    from: c.from.clone(),
                    to: c.to.clone(),
                    kind: format!("{:?}", c.kind),
                    count: c.count,
                })
                .collect();
            OverviewSubsystemsData {
                subsystems: subsystem_items,
                couplings: coupling_items,
            }
        });

        // ---- Hotspots ----
        let hotspots = section_selected(sections, "hotspots").then(|| {
            hotspots_raw
                .iter()
                .map(|h| {
                    let (name, kind, location) =
                        node_display(graph, &snapshot, h.node, workspace_root);
                    OverviewHotspotData {
                        name,
                        kind,
                        location,
                        score: h.score,
                    }
                })
                .collect::<Vec<_>>()
        });

        // ---- Issues ----
        let issues = section_selected(sections, "issues").then(|| OverviewIssuesData {
            cycles: cycles_all.iter().take(top).cloned().collect(),
            unused_public: unused_public
                .iter()
                .take(top)
                .map(|&node| {
                    let (name, _kind, location) =
                        node_display(graph, &snapshot, node, workspace_root);
                    OverviewNamedLocationData { name, location }
                })
                .collect(),
            duplicates: duplicate_groups.iter().take(top).cloned().collect(),
            high_fan_out: fan_out_ranks
                .iter()
                .filter(|h| h.fan_out > 0)
                .take(top)
                .map(|h| {
                    let (name, _kind, location) =
                        node_display(graph, &snapshot, h.node, workspace_root);
                    OverviewFanOutData {
                        name,
                        location,
                        fan_out: h.fan_out,
                    }
                })
                .collect(),
        });

        // ---- Suggested questions ----
        let suggested_questions = section_selected(sections, "questions").then(|| {
            build_questions(
                &snapshot,
                &hub_items,
                &subsystems,
                &couplings,
                &cycles_all,
                &unused_public,
                &duplicate_groups,
            )
        });

        let _ = ALL_SECTIONS; // canonical order asserted in tests; keep the import live.

        let data = GenerateOverviewData {
            summary,
            hubs,
            subsystems: subsystems_out,
            hotspots,
            issues,
            suggested_questions,
        };

        tracing::debug!(node_count = node_count, "generate_overview completed");

        Ok(ToolExecution {
            data,
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token: None,
            total: Some(1),
            truncated: Some(false),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sections_none_is_all() {
        assert_eq!(parse_sections(None).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn parse_sections_canonical_order() {
        let out = parse_sections(Some("issues,hubs")).unwrap();
        assert_eq!(out, vec!["hubs".to_string(), "issues".to_string()]);
    }

    #[test]
    fn parse_sections_rejects_unknown() {
        assert!(parse_sections(Some("hubs,bogus")).is_err());
    }

    #[test]
    fn parse_sections_rejects_empty_after_split() {
        assert!(parse_sections(Some(",")).is_err());
    }

    #[test]
    fn section_selected_empty_means_all() {
        assert!(section_selected(&[], "hubs"));
        assert!(section_selected(&["hubs".to_string()], "hubs"));
        assert!(!section_selected(&["hubs".to_string()], "issues"));
    }

    #[test]
    fn is_safe_arg_filters_dangerous_names() {
        assert!(is_safe_arg("do_thing"));
        assert!(is_safe_arg("crate::module::func"));
        assert!(!is_safe_arg(""));
        assert!(!is_safe_arg("has space"));
        assert!(!is_safe_arg("has\"quote"));
    }
}
