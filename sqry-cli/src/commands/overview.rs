//! `sqry overview` - one-shot repository orientation report.
//!
//! Composes shipped analyses into a single high-signal report for an
//! unfamiliar codebase: a summary (health indicators + graph stats), the
//! load-bearing hubs, the path/package subsystems and their couplings, the
//! complexity hotspots, the potential issues (cycles, unused public APIs,
//! duplicates, high fan-out), and a set of ready-to-run sqry queries. Every
//! section ends by pointing at a concrete query to run next, so the report is
//! a search onramp rather than a terminal artifact.
//!
//! # Reuse, not reinvention
//!
//! The composer is glue over existing computation:
//! - Hubs / subsystems / hotspots come from the deterministic `sqry-core`
//!   analysis primitives ([`rank_hubs`], [`aggregate_subsystems`],
//!   [`rank_hotspots`]). The MCP `generate_overview` tool composes the same
//!   primitives, so the CLI and agent reports match on a shared snapshot.
//! - Cycles / unused / duplicates route through the same sqry-db derived
//!   queries and duplicate builder the standalone `cycles` / `unused` /
//!   `duplicates` commands use, so the report shares one cache behaviour with
//!   them on the same snapshot.
//!
//! # Determinism
//!
//! The whole report path is integer and float-free, so `--format json` on a
//! fixed snapshot is byte-stable across runs. Struct field order is the
//! serialization order (stable JSON contract), and every ranked section is
//! bounded and deterministically ordered by its underlying primitive.
//!
//! # Redaction
//!
//! Every emitted path flows through the shared MCP redaction path primitive
//! ([`sqry_mcp_redaction::rules::path::redact_path`]). Under the default
//! `minimal` preset a path renders workspace-relative (never a raw absolute
//! host path); `none` reveals raw paths for trusted local use; `relative`
//! renders the clean workspace-relative layout. Symbol names carry the
//! standard minimal-preset treatment (code/name-preserving), matching how the
//! MCP `minimal` preset handles code content.

use crate::args::Cli;
use crate::commands::graph::loader;
use anyhow::{Context, Result, bail};
use loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use serde::Serialize;
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::NodeEntry;
use sqry_core::graph::unified::analysis::{
    HubMetric, HubOpts, HubRank, KindMask, SubsystemOpts, aggregate_subsystems, rank_hotspots,
    rank_hubs,
};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::query::{
    CircularType, DuplicateConfig, DuplicateType, UnusedScope, build_duplicate_groups_graph,
};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// Every report section, in canonical report order.
const ALL_SECTIONS: &[&str] = &[
    "summary",
    "hubs",
    "subsystems",
    "hotspots",
    "issues",
    "questions",
];

/// Options for [`run_overview`], threaded from the CLI `Overview` variant.
pub struct OverviewOptions<'a> {
    /// Search path (defaults to the current directory).
    pub path: Option<&'a str>,
    /// Output format: `md`, `json`, or `text`.
    pub format: &'a str,
    /// Maximum rows per ranked section.
    pub top: usize,
    /// Comma-separated subset of sections to include (all when `None`).
    pub sections: Option<&'a str>,
    /// Directory-component depth for subsystem bucket keys.
    pub group_depth: usize,
    /// Write to this file instead of stdout when set.
    pub output: Option<&'a Path>,
    /// Fail if the graph is missing/stale rather than building it.
    pub no_index: bool,
    /// Path redaction preset: `minimal`, `none`, or `relative`.
    pub redaction: &'a str,
}

/// Run the `overview` command.
///
/// # Errors
///
/// Returns an error if a requested section name is invalid, the graph is
/// missing under `--no-index`, the graph cannot be loaded, or the report
/// cannot be written.
pub fn run_overview(cli: &Cli, opts: &OverviewOptions) -> Result<()> {
    let selected = parse_sections(opts.sections)?;

    let root = opts
        .path
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);

    // `--no-index`: refuse to silently build. A missing manifest means "no
    // index"; a manifest without a snapshot means "stale/incomplete index".
    if opts.no_index {
        let storage = GraphStorage::new(&root);
        if !storage.exists() {
            bail!(
                "no graph index found at {} (run `sqry index` first, or drop --no-index to build one)",
                root.display()
            );
        }
        if !storage.snapshot_exists() {
            bail!(
                "graph index at {} is stale or incomplete (rebuild with `sqry index --force`, or drop --no-index)",
                root.display()
            );
        }
    }

    let config = GraphLoadConfig {
        include_hidden: cli.hidden,
        follow_symlinks: cli.follow,
        max_depth: if cli.max_depth == 0 {
            None
        } else {
            Some(cli.max_depth)
        },
        force_build: false,
    };
    let graph = load_unified_graph_for_cli(&root, &config, cli, no_op_reporter())
        .context("Failed to load unified graph for overview")?;

    let redactor = PathRedactor::new(opts.redaction, &root)?;
    let report = compose_report(
        &graph,
        &root,
        opts.top,
        opts.group_depth,
        &selected,
        &redactor,
    );

    let rendered = match opts.format {
        "json" => serde_json::to_string_pretty(&report)
            .context("Failed to serialize overview report to JSON")?,
        "text" => render_text(&report),
        // clap restricts the value set, so anything else is `md`.
        _ => render_markdown(&report),
    };

    if let Some(out_path) = opts.output {
        let mut file = std::fs::File::create(out_path)
            .with_context(|| format!("Failed to create output file {}", out_path.display()))?;
        file.write_all(rendered.as_bytes())
            .with_context(|| format!("Failed to write report to {}", out_path.display()))?;
        // Trailing newline for POSIX-friendly files.
        file.write_all(b"\n").ok();
        // stdout stays silent when writing to a file (FR3).
    } else {
        println!("{rendered}");
    }

    Ok(())
}

/// Parse and validate the `--sections` list into an ordered inclusion set.
fn parse_sections(sections: Option<&str>) -> Result<Vec<String>> {
    let Some(raw) = sections else {
        return Ok(ALL_SECTIONS.iter().map(|s| (*s).to_string()).collect());
    };
    let mut chosen = Vec::new();
    for name in raw.split(',') {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        if !ALL_SECTIONS.contains(&name.as_str()) {
            bail!(
                "Invalid section '{name}'. Valid sections: {}",
                ALL_SECTIONS.join(", ")
            );
        }
        if !chosen.contains(&name) {
            chosen.push(name);
        }
    }
    if chosen.is_empty() {
        bail!("--sections was empty after parsing; supply at least one valid section");
    }
    // Emit in canonical report order regardless of the user's ordering so the
    // rendered report reads consistently.
    Ok(ALL_SECTIONS
        .iter()
        .filter(|s| chosen.iter().any(|c| c == *s))
        .map(|s| (*s).to_string())
        .collect())
}

fn section_selected(selected: &[String], name: &str) -> bool {
    selected.iter().any(|s| s == name)
}

// -------------------------------------------------------------------------
// Redaction
// -------------------------------------------------------------------------

/// The three report redaction presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedactionPreset {
    /// No redaction (raw absolute paths); trusted local use only.
    None,
    /// Workspace-relative paths, code/names preserved (default).
    Minimal,
    /// Clean workspace-relative layout.
    Relative,
}

/// Applies the shared MCP redaction path primitive to every path the report
/// emits, per the selected preset.
struct PathRedactor {
    preset: RedactionPreset,
    /// Canonical workspace root as a string, used to relativize absolute paths.
    workspace_root: Option<String>,
}

impl PathRedactor {
    fn new(preset: &str, root: &Path) -> Result<Self> {
        let preset = match preset {
            "none" => RedactionPreset::None,
            "minimal" => RedactionPreset::Minimal,
            "relative" => RedactionPreset::Relative,
            other => {
                bail!("Invalid --redaction preset '{other}' (expected: minimal, none, relative)")
            }
        };
        // Prefer the canonical root so absolute stored paths strip cleanly; fall
        // back to the given root string if canonicalization fails.
        let workspace_root = std::fs::canonicalize(root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| Some(root.to_string_lossy().into_owned()));
        Ok(Self {
            preset,
            workspace_root,
        })
    }

    /// Redact a single path string per the active preset.
    fn redact(&self, raw: &str) -> String {
        match self.preset {
            RedactionPreset::None => raw.to_string(),
            // In the single-workspace CLI context, `minimal` and `relative`
            // both emit the clean workspace-relative form via the shared
            // primitive (the anonymizing source-root prefix is an MCP
            // multi-root feature that requires a bound LogicalWorkspace). Both
            // strip the absolute host prefix, so neither leaks a raw path.
            RedactionPreset::Minimal | RedactionPreset::Relative => {
                sqry_mcp_redaction::rules::path::redact_path(
                    raw,
                    self.workspace_root.as_deref(),
                    "<workspace>",
                    false,
                    None,
                )
                .unwrap_or_else(|_| basename_fallback(raw))
            }
        }
    }

    /// Redact a `path:line` location string.
    fn location(&self, raw_path: &str, line: u32) -> String {
        let redacted = self.redact(raw_path);
        if line > 0 {
            format!("{redacted}:{line}")
        } else {
            redacted
        }
    }
}

/// Basename-only fallback if the path primitive rejects a malformed path.
fn basename_fallback(raw: &str) -> String {
    raw.rsplit(['/', '\\']).next().unwrap_or(raw).to_string()
}

// -------------------------------------------------------------------------
// Report model
// -------------------------------------------------------------------------

/// The full structured report. `None` sections are omitted from JSON and not
/// rendered. Field order is the serialization order (stable JSON contract).
#[derive(Debug, Serialize)]
struct OverviewReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<SummarySection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hubs: Option<Vec<HubItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subsystems: Option<SubsystemsSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hotspots: Option<Vec<HotspotItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<IssuesSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_questions: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct SummarySection {
    total_files: usize,
    total_symbols: usize,
    total_edges: usize,
    languages: Vec<LanguageCount>,
    health: HealthBlock,
}

#[derive(Debug, Serialize)]
struct LanguageCount {
    language: String,
    count: usize,
}

#[derive(Debug, Serialize)]
struct HealthBlock {
    cycles: usize,
    unused_symbols: usize,
    duplicate_groups: usize,
    cross_language_edges: usize,
}

#[derive(Debug, Serialize)]
struct HubItem {
    name: String,
    kind: String,
    location: String,
    fan_in: u32,
    fan_out: u32,
}

#[derive(Debug, Serialize)]
struct SubsystemsSection {
    subsystems: Vec<SubsystemItem>,
    couplings: Vec<CouplingItem>,
}

#[derive(Debug, Serialize)]
struct SubsystemItem {
    key: String,
    size: u32,
    internal_edges: u64,
    representative: String,
}

#[derive(Debug, Serialize)]
struct CouplingItem {
    from: String,
    to: String,
    kind: String,
    count: u32,
}

#[derive(Debug, Serialize)]
struct HotspotItem {
    name: String,
    kind: String,
    location: String,
    score: usize,
}

#[derive(Debug, Serialize)]
struct IssuesSection {
    cycles: Vec<Vec<String>>,
    unused_public: Vec<NamedLocation>,
    duplicates: Vec<DuplicateItem>,
    high_fan_out: Vec<FanOutItem>,
}

#[derive(Debug, Serialize)]
struct NamedLocation {
    name: String,
    location: String,
}

#[derive(Debug, Clone, Serialize)]
struct DuplicateItem {
    group_id: String,
    count: usize,
    members: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FanOutItem {
    name: String,
    location: String,
    fan_out: u32,
}

// -------------------------------------------------------------------------
// Composition
// -------------------------------------------------------------------------

/// Assemble the report from the loaded graph, including only the selected
/// sections. All underlying data is computed once; `suggested_questions` is
/// templated from that data.
fn compose_report(
    graph: &CodeGraph,
    index_root: &Path,
    top: usize,
    group_depth: usize,
    selected: &[String],
    redactor: &PathRedactor,
) -> OverviewReport {
    let snapshot = Arc::new(graph.snapshot());

    // Raw analyses (computed once; cheap integer sweeps + cached derived queries).
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
    let (subsystems, couplings) =
        aggregate_subsystems(&snapshot, &SubsystemOpts { top, group_depth });
    let hotspots_raw = complexity_top(&snapshot, top);
    let cycles_all = compute_cycle_rings(&snapshot, index_root, snapshot.nodes().len());
    let unused_all = compute_unused(&snapshot, index_root, UnusedScope::All);
    let unused_public = compute_unused(&snapshot, index_root, UnusedScope::Public);
    let duplicate_groups = compute_duplicates(graph, snapshot.nodes().len());

    // Build section payloads.
    let summary = build_summary(
        &snapshot,
        cycles_all.len(),
        unused_all.len(),
        duplicate_groups.len(),
    );

    let hub_items: Vec<HubItem> = hub_ranks
        .iter()
        .map(|h| hub_item(&snapshot, redactor, h))
        .collect();

    let subsystem_items: Vec<SubsystemItem> = subsystems
        .iter()
        .map(|s| SubsystemItem {
            key: s.key.clone(),
            size: s.size,
            internal_edges: s.internal_edges,
            representative: resolve_label(&snapshot, s.representative)
                .unwrap_or_else(|| "?".to_string()),
        })
        .collect();
    let coupling_items: Vec<CouplingItem> = couplings
        .iter()
        .map(|c| CouplingItem {
            from: c.from.clone(),
            to: c.to.clone(),
            kind: format!("{:?}", c.kind),
            count: c.count,
        })
        .collect();

    let hotspot_items: Vec<HotspotItem> = hotspots_raw
        .iter()
        .map(|(node, score)| {
            let (name, kind, location) = node_display(&snapshot, redactor, *node);
            HotspotItem {
                name,
                kind,
                location,
                score: *score,
            }
        })
        .collect();

    let issues = IssuesSection {
        cycles: cycles_all.iter().take(top).cloned().collect(),
        unused_public: unused_public
            .iter()
            .take(top)
            .map(|node| {
                let (name, _kind, location) = node_display(&snapshot, redactor, *node);
                NamedLocation { name, location }
            })
            .collect(),
        duplicates: duplicate_groups.iter().take(top).cloned().collect(),
        high_fan_out: fan_out_ranks
            .iter()
            .filter(|h| h.fan_out > 0)
            .take(top)
            .map(|h| {
                let (name, _kind, location) = node_display(&snapshot, redactor, h.node);
                FanOutItem {
                    name,
                    location,
                    fan_out: h.fan_out,
                }
            })
            .collect(),
    };

    // Suggested questions template off the computed data.
    let questions = build_questions(
        &snapshot,
        &hub_items,
        &subsystems,
        &couplings,
        &cycles_all,
        &unused_public,
        &duplicate_groups,
    );

    OverviewReport {
        summary: section_selected(selected, "summary").then_some(summary),
        hubs: section_selected(selected, "hubs").then_some(hub_items),
        subsystems: section_selected(selected, "subsystems").then_some(SubsystemsSection {
            subsystems: subsystem_items,
            couplings: coupling_items,
        }),
        hotspots: section_selected(selected, "hotspots").then_some(hotspot_items),
        issues: section_selected(selected, "issues").then_some(issues),
        suggested_questions: section_selected(selected, "questions").then_some(questions),
    }
}

/// Build the summary section (stats + health block).
fn build_summary(
    snapshot: &GraphSnapshot,
    cycles: usize,
    unused_symbols: usize,
    duplicate_groups: usize,
) -> SummarySection {
    let total_files = snapshot.files().len();
    let total_symbols = snapshot.nodes().len();

    // O(1) edge count from store stats (matches `graph stats`).
    let edge_stats = snapshot.edges().stats();
    let total_edges = (edge_stats.forward.csr_edge_count + edge_stats.forward.delta_edge_count)
        .saturating_sub(edge_stats.forward.tombstone_count);

    let languages = language_counts(snapshot);
    let cross_language_edges = cross_language_edge_count(snapshot);

    SummarySection {
        total_files,
        total_symbols,
        total_edges,
        languages,
        health: HealthBlock {
            cycles,
            unused_symbols,
            duplicate_groups,
            cross_language_edges,
        },
    }
}

/// Per-language symbol counts, sorted by count descending then name ascending.
fn language_counts(snapshot: &GraphSnapshot) -> Vec<LanguageCount> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_id, entry) in snapshot.iter_nodes() {
        if entry.is_unified_loser() {
            continue;
        }
        let lang = snapshot
            .files()
            .language_for_file(entry.file)
            .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"));
        *counts.entry(lang).or_insert(0) += 1;
    }
    let mut rows: Vec<LanguageCount> = counts
        .into_iter()
        .map(|(language, count)| LanguageCount { language, count })
        .collect();
    rows.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.language.cmp(&b.language))
    });
    rows
}

/// Count edges whose source and target files carry distinct known languages.
fn cross_language_edge_count(snapshot: &GraphSnapshot) -> usize {
    let mut count = 0usize;
    for (src, tgt, _kind) in snapshot.iter_edges() {
        if let (Some(se), Some(te)) = (snapshot.get_node(src), snapshot.get_node(tgt)) {
            let sl = snapshot.files().language_for_file(se.file);
            let tl = snapshot.files().language_for_file(te.file);
            if sl.is_some() && tl.is_some() && sl != tl {
                count += 1;
            }
        }
    }
    count
}

/// Complexity ranking (fan-out complexity), via the shared `sqry-core`
/// [`rank_hotspots`] primitive. Returns the top-N `(node, score)` pairs, score
/// descending, ties broken by node index ascending for determinism. This is
/// the same primitive the MCP `generate_overview` tool composes, so the CLI
/// and agent reports rank hotspots identically on the same snapshot.
fn complexity_top(snapshot: &GraphSnapshot, top: usize) -> Vec<(NodeId, usize)> {
    rank_hotspots(snapshot, top)
        .into_iter()
        .map(|h| (h.node, h.score))
        .collect()
}

/// Materialize cycle SCC node-id rings into qualified-name rings, reusing the
/// same `CyclesQuery` the standalone `cycles` command dispatches.
fn compute_cycle_rings(
    snapshot: &Arc<GraphSnapshot>,
    index_root: &Path,
    max_results: usize,
) -> Vec<Vec<String>> {
    let db = sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(snapshot), index_root);
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

/// Fetch unused node ids for a scope, reusing the `UnusedQuery` + binding-plane
/// post-filter the standalone `unused` command uses.
fn compute_unused(
    snapshot: &Arc<GraphSnapshot>,
    index_root: &Path,
    scope: UnusedScope,
) -> Vec<NodeId> {
    let db = sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(snapshot), index_root);
    let key = sqry_db::queries::UnusedKey {
        scope,
        max_results: snapshot.nodes().len(),
    };
    let raw = db.get::<sqry_db::queries::UnusedQuery>(&key);
    sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter(&raw, snapshot, &db)
}

/// Body-duplicate groups (size > 1), reusing the graph duplicate builder.
fn compute_duplicates(graph: &CodeGraph, max_results: usize) -> Vec<DuplicateItem> {
    let config = DuplicateConfig {
        threshold: 1.0,
        max_results,
        is_exact_only: true,
        ..Default::default()
    };
    let groups = build_duplicate_groups_graph(DuplicateType::Body, graph, &config);
    let strings = graph.strings();
    let mut items: Vec<DuplicateItem> = groups
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
            DuplicateItem {
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

/// Resolve a node into `(name, kind, redacted-location)`.
fn node_display(
    snapshot: &GraphSnapshot,
    redactor: &PathRedactor,
    node: NodeId,
) -> (String, String, String) {
    match snapshot.get_node(node) {
        Some(entry) => {
            let name = entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());
            let kind = format!("{:?}", entry.kind);
            let location = entry_location(snapshot, redactor, entry);
            (name, kind, location)
        }
        None => ("?".to_string(), "?".to_string(), "unknown".to_string()),
    }
}

/// Build a redacted `path:line` location for a node entry.
fn entry_location(snapshot: &GraphSnapshot, redactor: &PathRedactor, entry: &NodeEntry) -> String {
    let raw = snapshot.files().resolve(entry.file).map_or_else(
        || "unknown".to_string(),
        |p| p.to_string_lossy().into_owned(),
    );
    redactor.location(&raw, entry.start_line)
}

/// Build a hub display row from a rank.
fn hub_item(snapshot: &GraphSnapshot, redactor: &PathRedactor, hub: &HubRank) -> HubItem {
    let (name, kind, location) = node_display(snapshot, redactor, hub.node);
    HubItem {
        name,
        kind,
        location,
        fan_in: hub.fan_in,
        fan_out: hub.fan_out,
    }
}

// -------------------------------------------------------------------------
// Suggested questions (templated sqry command lines)
// -------------------------------------------------------------------------

/// A symbol name is safe to embed in a suggested command line if it is
/// non-empty and free of quotes/whitespace/control characters (so the emitted
/// `sqry ... "name"` always parses as a single argument).
fn is_safe_arg(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control())
}

/// Template ready-to-run sqry command lines from the computed findings. Every
/// emitted line parses as a valid sqry command.
fn build_questions(
    snapshot: &GraphSnapshot,
    hubs: &[HubItem],
    subsystems: &[sqry_core::graph::unified::analysis::Subsystem],
    couplings: &[sqry_core::graph::unified::analysis::Coupling],
    cycles: &[Vec<String>],
    unused_public: &[NodeId],
    duplicates: &[DuplicateItem],
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

// -------------------------------------------------------------------------
// Renderers
// -------------------------------------------------------------------------

/// Render the report as Markdown.
fn render_markdown(report: &OverviewReport) -> String {
    let mut out = String::new();
    out.push_str("# Repository overview\n");

    if let Some(summary) = &report.summary {
        out.push_str("\n## Summary\n\n");
        out.push_str(&format!("- Files: {}\n", summary.total_files));
        out.push_str(&format!("- Symbols: {}\n", summary.total_symbols));
        out.push_str(&format!("- Edges: {}\n", summary.total_edges));
        let langs: Vec<String> = summary
            .languages
            .iter()
            .map(|l| format!("{} ({})", l.language, l.count))
            .collect();
        out.push_str(&format!("- Languages: {}\n", langs.join(", ")));
        out.push_str("\n**Health indicators**\n\n");
        out.push_str(&format!("- Cycles: {}\n", summary.health.cycles));
        out.push_str(&format!(
            "- Unused symbols: {}\n",
            summary.health.unused_symbols
        ));
        out.push_str(&format!(
            "- Duplicate groups: {}\n",
            summary.health.duplicate_groups
        ));
        out.push_str(&format!(
            "- Cross-language edges: {}\n",
            summary.health.cross_language_edges
        ));
        out.push_str("\nNext: `sqry graph stats`\n");
    }

    if let Some(hubs) = &report.hubs {
        out.push_str("\n## Hubs (load-bearing symbols)\n\n");
        if hubs.is_empty() {
            out.push_str("_No hubs found._\n");
        } else {
            out.push_str("| # | Symbol | Kind | fan-in | fan-out | Location |\n");
            out.push_str("|---|--------|------|--------|---------|----------|\n");
            for (i, h) in hubs.iter().enumerate() {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} |\n",
                    i + 1,
                    h.name,
                    h.kind,
                    h.fan_in,
                    h.fan_out,
                    h.location
                ));
            }
            if let Some(top) = hubs.first() {
                out.push_str(&format!("\nNext: `sqry impact \"{}\"`\n", top.name));
            }
        }
    }

    if let Some(sub) = &report.subsystems {
        out.push_str("\n## Subsystems (by path/package)\n\n");
        if sub.subsystems.is_empty() {
            out.push_str("_No subsystems found._\n");
        } else {
            out.push_str("| # | Subsystem | Symbols | Internal edges | Representative |\n");
            out.push_str("|---|-----------|---------|----------------|----------------|\n");
            for (i, s) in sub.subsystems.iter().enumerate() {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    i + 1,
                    s.key,
                    s.size,
                    s.internal_edges,
                    s.representative
                ));
            }
        }
        out.push_str("\n**Couplings** (sparse-but-high-fan first)\n\n");
        if sub.couplings.is_empty() {
            out.push_str("_No cross-subsystem couplings found._\n");
        } else {
            for c in &sub.couplings {
                out.push_str(&format!(
                    "- `{}` to `{}` [{}] x{}\n",
                    c.from, c.to, c.kind, c.count
                ));
            }
        }
        out.push_str("\nNext: `sqry graph subsystems`\n");
    }

    if let Some(hotspots) = &report.hotspots {
        out.push_str("\n## Hotspots (fan-out complexity)\n\n");
        if hotspots.is_empty() {
            out.push_str("_No hotspots found._\n");
        } else {
            out.push_str("| # | Symbol | Kind | Score | Location |\n");
            out.push_str("|---|--------|------|-------|----------|\n");
            for (i, h) in hotspots.iter().enumerate() {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    i + 1,
                    h.name,
                    h.kind,
                    h.score,
                    h.location
                ));
            }
            out.push_str("\nNext: `sqry graph complexity --sort-complexity`\n");
        }
    }

    if let Some(issues) = &report.issues {
        out.push_str("\n## Potential issues\n\n");

        out.push_str("**Cycles**\n\n");
        if issues.cycles.is_empty() {
            out.push_str("_None found._\n");
        } else {
            for ring in &issues.cycles {
                let mut chain = ring.join(" to ");
                if let Some(first) = ring.first() {
                    chain.push_str(" to ");
                    chain.push_str(first);
                }
                out.push_str(&format!("- {chain}\n"));
            }
            out.push_str("\nNext: `sqry cycles`\n");
        }

        out.push_str("\n**Unused public APIs**\n\n");
        if issues.unused_public.is_empty() {
            out.push_str("_None found._\n");
        } else {
            for u in &issues.unused_public {
                out.push_str(&format!("- {} ({})\n", u.name, u.location));
            }
            out.push_str("\nNext: `sqry unused --scope public`\n");
        }

        out.push_str("\n**Duplicates**\n\n");
        if issues.duplicates.is_empty() {
            out.push_str("_None found._\n");
        } else {
            for d in &issues.duplicates {
                out.push_str(&format!(
                    "- {} duplicates: {}\n",
                    d.count,
                    d.members.join(", ")
                ));
            }
            out.push_str("\nNext: `sqry duplicates`\n");
        }

        out.push_str("\n**High fan-out**\n\n");
        if issues.high_fan_out.is_empty() {
            out.push_str("_None found._\n");
        } else {
            for h in &issues.high_fan_out {
                out.push_str(&format!(
                    "- {} (fan-out {}, {})\n",
                    h.name, h.fan_out, h.location
                ));
            }
            out.push_str("\nNext: `sqry graph hubs --by fan-out`\n");
        }
    }

    if let Some(questions) = &report.suggested_questions {
        out.push_str("\n## Suggested questions\n\n");
        if questions.is_empty() {
            out.push_str("_No suggestions._\n");
        } else {
            for q in questions {
                out.push_str(&format!("- `{q}`\n"));
            }
        }
    }

    out
}

/// Render the report as a terse console digest.
fn render_text(report: &OverviewReport) -> String {
    let mut out = String::new();
    out.push_str("REPOSITORY OVERVIEW\n");

    if let Some(summary) = &report.summary {
        out.push_str(&format!(
            "summary: {} files, {} symbols, {} edges | cycles={} unused={} dup_groups={} xlang={}\n",
            summary.total_files,
            summary.total_symbols,
            summary.total_edges,
            summary.health.cycles,
            summary.health.unused_symbols,
            summary.health.duplicate_groups,
            summary.health.cross_language_edges,
        ));
    }

    if let Some(hubs) = &report.hubs {
        out.push_str(&format!("hubs: {}\n", hubs.len()));
        for h in hubs {
            out.push_str(&format!(
                "  {} [{}] in={} out={} {}\n",
                h.name, h.kind, h.fan_in, h.fan_out, h.location
            ));
        }
    }

    if let Some(sub) = &report.subsystems {
        out.push_str(&format!(
            "subsystems: {} ({} couplings)\n",
            sub.subsystems.len(),
            sub.couplings.len()
        ));
        for s in &sub.subsystems {
            out.push_str(&format!(
                "  {} ({} symbols, {} internal) rep={}\n",
                s.key, s.size, s.internal_edges, s.representative
            ));
        }
        for c in &sub.couplings {
            out.push_str(&format!(
                "  {} to {} [{}] x{}\n",
                c.from, c.to, c.kind, c.count
            ));
        }
    }

    if let Some(hotspots) = &report.hotspots {
        out.push_str(&format!("hotspots: {}\n", hotspots.len()));
        for h in hotspots {
            out.push_str(&format!(
                "  {} [{}] score={} {}\n",
                h.name, h.kind, h.score, h.location
            ));
        }
    }

    if let Some(issues) = &report.issues {
        out.push_str(&format!(
            "issues: {} cycles, {} unused_public, {} duplicates, {} high_fan_out\n",
            issues.cycles.len(),
            issues.unused_public.len(),
            issues.duplicates.len(),
            issues.high_fan_out.len(),
        ));
    }

    if let Some(questions) = &report.suggested_questions {
        out.push_str("questions:\n");
        for q in questions {
            out.push_str(&format!("  {q}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_sections_defaults_to_all() {
        let sections = parse_sections(None).unwrap();
        assert_eq!(sections, ALL_SECTIONS.to_vec());
    }

    #[test]
    fn parse_sections_subset_in_canonical_order() {
        // User order is reversed; output stays canonical.
        let sections = parse_sections(Some("issues,hubs")).unwrap();
        assert_eq!(sections, vec!["hubs".to_string(), "issues".to_string()]);
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
    fn redaction_none_preserves_raw_path() {
        let r = PathRedactor::new("none", Path::new("/tmp")).unwrap();
        assert_eq!(
            r.redact("/abs/host/path/src/main.rs"),
            "/abs/host/path/src/main.rs"
        );
    }

    #[test]
    fn redaction_minimal_strips_absolute_prefix() {
        let r = PathRedactor::new("minimal", Path::new("/home/user/project")).unwrap();
        let out = r.redact("/home/user/project/src/main.rs");
        assert!(
            !out.starts_with("/home/user/project"),
            "minimal must not emit the raw workspace-root prefix: {out}"
        );
        assert!(out.contains("main.rs"), "basename must survive: {out}");
    }

    #[test]
    fn redaction_external_path_is_collapsed() {
        let r = PathRedactor::new("minimal", Path::new("/home/user/project")).unwrap();
        let out = r.redact("/etc/passwd");
        assert!(
            out.starts_with("<external>/") || out == "passwd",
            "external paths collapse to a basename form: {out}"
        );
    }

    #[test]
    fn is_safe_arg_filters_dangerous_names() {
        assert!(is_safe_arg("do_thing"));
        assert!(is_safe_arg("crate::module::func"));
        assert!(!is_safe_arg(""));
        assert!(!is_safe_arg("has space"));
        assert!(!is_safe_arg("has\"quote"));
    }

    /// Every templated suggested-questions line must parse as a valid sqry
    /// command (the report is a search onramp, so its links must be runnable).
    ///
    /// The full `Cli` parser is large; building it recurses deeper than the
    /// default 2 MiB test-thread stack, so the parse runs on a worker thread
    /// with a generous stack (the production `main` thread has an 8 MiB stack).
    #[test]
    fn templated_questions_parse_as_sqry_commands() {
        let samples = [
            "sqry impact \"do_thing\"",
            "sqry graph direct-callers \"do_thing\"",
            "sqry cycles",
            "sqry graph trace-path \"a_fn\" \"b_fn\"",
            "sqry unused --scope public",
            "sqry duplicates",
            "sqry search \"do_thing\" .",
        ];
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                for line in samples {
                    let argv = shlex::split(line).unwrap_or_else(|| {
                        panic!("suggested line is not shell-splittable: {line}")
                    });
                    Cli::try_parse_from(&argv)
                        .unwrap_or_else(|e| panic!("suggested line failed to parse: {line}\n{e}"));
                }
            })
            .expect("spawn parse worker")
            .join()
            .expect("parse worker panicked");
    }
}
