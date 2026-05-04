//! `textDocument/diagnostic` (LSP 3.17 pull-model) handler.
//!
//! Synthesises three classes of diagnostics for symbols defined in the
//! requested document:
//!
//! 1. **Unused symbol warnings** (severity `Warning`) sourced from
//!    [`sqry_db::queries::UnusedQuery`] keyed on
//!    [`sqry_core::query::UnusedScope::All`]. Auto-registered by
//!    `QueryDb::register_builtin_queries` and re-exported from
//!    `sqry_db::queries`.
//! 2. **Cycle-member information** (severity `Information`) sourced
//!    from [`sqry_db::queries::CyclesQuery`] keyed on
//!    [`sqry_core::query::CircularType::Calls`].
//! 3. **Duplicate-group warnings** (severity `Warning`) sourced from
//!    [`sqry_core::query::build_duplicate_groups_graph`] —
//!    sqry-lsp depends on sqry-core, *not* sqry-mcp; the MCP-side
//!    `execute_find_duplicates` wrapper lives in sqry-mcp and must not
//!    be the dependency direction here.
//!
//! Severity levels follow LSP 3.17 §5.18.
//!
//! Closes audit finding A101 (cli-help-impl-alignment-2026-05-04, CRUD
//! row C075a). Prior to C075a this handler always returned an empty
//! [`DiagnosticsOutcome::empty`].
//!
//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — even though
//! the steady-state response can be non-empty, the gate **must** still
//! consult [`crate::session::SessionManager::evaluate_handler_gate`]
//! before any graph access, so member-folder and excluded-path
//! requests short-circuit through the same code path the
//! `sqry/indexStatus` handler already uses (STEP_4).

use crate::session::{HandlerGate, SessionManager};
use anyhow::Result;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::query::{
    CircularType, DuplicateConfig, DuplicateType, UnusedScope, build_duplicate_groups_graph,
};
use sqry_db::queries::dispatch::make_query_db_cold;
use std::collections::HashSet;
use std::sync::Arc;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range, Url};

/// STEP_11_4 — outcome of a `textDocument/diagnostic` request,
/// including the gate verdict so the LSP server can surface
/// "member" / "excluded" hints to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsOutcome {
    /// The diagnostics to publish.
    pub diagnostics: Vec<Diagnostic>,
    /// `true` when the request URI lives inside a member folder.
    pub partial: bool,
    /// `true` when the request URI lives inside an excluded path.
    pub excluded: bool,
}

impl DiagnosticsOutcome {
    /// The empty / non-gated outcome.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            diagnostics: Vec::new(),
            partial: false,
            excluded: false,
        }
    }
}

/// LSP-shaped diagnostic source identifier surfaced to the client.
const DIAGNOSTIC_SOURCE: &str = "sqry";

/// Fetch cap for sqry-db queries — large enough to cover any realistic
/// document, small enough to bound the worst-case fetch under load.
const FETCH_CAP: usize = 4096;

/// STEP_11_4 — gated diagnostics handler. Never probes the filesystem
/// per folder; consults [`SessionManager::evaluate_handler_gate`] only.
///
/// On `HandlerGate::Continue`, walks the requested file's nodes and
/// emits diagnostics for each unused / cycle / duplicate finding that
/// touches a node in this document. Other documents are not affected
/// by this single-document pull request — the LSP spec routes one
/// pull per URI.
///
/// # Errors
///
/// Returns an error when the document URI cannot be converted to a
/// filesystem path or when graph access fails.
pub fn handle(session: &SessionManager, uri: &Url) -> Result<DiagnosticsOutcome> {
    match session.evaluate_handler_gate(uri) {
        HandlerGate::Member(_) => {
            return Ok(DiagnosticsOutcome {
                diagnostics: Vec::new(),
                partial: true,
                excluded: false,
            });
        }
        HandlerGate::Excluded => {
            return Ok(DiagnosticsOutcome {
                diagnostics: Vec::new(),
                partial: false,
                excluded: true,
            });
        }
        HandlerGate::Continue => {}
    }

    let path = match uri.to_file_path() {
        Ok(path) => path,
        Err(()) => return Ok(DiagnosticsOutcome::empty()),
    };

    let Some(graph) = session.graph_for_path(&path)? else {
        return Ok(DiagnosticsOutcome::empty());
    };

    // Map the URI to a FileId so we can filter sqry-db results to
    // symbols defined in the requested document only.
    let Some(file_id) = graph.files().get(&path) else {
        return Ok(DiagnosticsOutcome::empty());
    };
    let document_node_ids: HashSet<NodeId> =
        graph.indices().by_file(file_id).iter().copied().collect();
    if document_node_ids.is_empty() {
        return Ok(DiagnosticsOutcome::empty());
    }

    let snapshot = Arc::new(graph.snapshot());
    let workspace_root = session.index_root_for_cold_load();
    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);

    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // ── Unused-symbol warnings ────────────────────────────────────
    //
    // `UnusedKey { scope: All, max_results }` mirrors the sqry-db
    // dispatch contract used elsewhere in sqry-lsp
    // (`handlers/index.rs:list_unused_symbols`). All-scope is the
    // diagnostic surface — we are not enumerating only public or only
    // private at the document level.
    let unused_node_ids = db.get::<sqry_db::queries::UnusedQuery>(&sqry_db::queries::UnusedKey {
        scope: UnusedScope::All,
        max_results: FETCH_CAP,
    });
    for &node_id in unused_node_ids.iter() {
        if !document_node_ids.contains(&node_id) {
            continue;
        }
        let Some(entry) = graph.nodes().get(node_id) else {
            continue;
        };
        if entry.is_unified_loser() {
            continue;
        }
        let strings = graph.strings();
        let name: String = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .or_else(|| strings.resolve(entry.name))
            .map_or_else(|| "<unknown>".to_string(), |arc| arc.to_string());
        diagnostics.push(make_diagnostic(
            entry_to_range(entry),
            DiagnosticSeverity::WARNING,
            "sqry::unused",
            format!(
                "unused symbol `{name}`: no callers, references, or reachable entry-point path"
            ),
        ));
    }

    // ── Cycle-member information ──────────────────────────────────
    //
    // `CyclesQuery` keyed on `Calls` returns a `Vec<Vec<NodeId>>` of
    // strongly-connected components. Filter to components whose
    // members include at least one node defined in the requested
    // document, and emit one Information-level diagnostic per
    // local cycle member.
    let cycle_components = db.get::<sqry_db::queries::CyclesQuery>(&sqry_db::queries::CyclesKey {
        circular_type: CircularType::Calls,
        bounds: sqry_db::queries::CycleBounds {
            min_depth: 2,
            max_depth: None,
            max_results: FETCH_CAP,
            should_include_self_loops: false,
        },
    });
    for component in cycle_components.iter() {
        if !component.iter().any(|id| document_node_ids.contains(id)) {
            continue;
        }
        for &node_id in component {
            if !document_node_ids.contains(&node_id) {
                continue;
            }
            let Some(entry) = graph.nodes().get(node_id) else {
                continue;
            };
            if entry.is_unified_loser() {
                continue;
            }
            let strings = graph.strings();
            let name: String = entry
                .qualified_name
                .and_then(|sid| strings.resolve(sid))
                .or_else(|| strings.resolve(entry.name))
                .map_or_else(|| "<unknown>".to_string(), |arc| arc.to_string());
            diagnostics.push(make_diagnostic(
                entry_to_range(entry),
                DiagnosticSeverity::INFORMATION,
                "sqry::cycle",
                format!(
                    "cycle member `{name}`: participates in a call cycle of {len} symbols",
                    len = component.len(),
                ),
            ));
        }
    }

    // ── Duplicate-group warnings ──────────────────────────────────
    //
    // `build_duplicate_groups_graph(DuplicateType::Body, ...)` exposes
    // body-hash-equivalence groups. The MCP-side `find_duplicates`
    // wrapper lives in sqry-mcp; we go straight through sqry-core to
    // keep the dependency direction sqry-lsp -> sqry-core only.
    let duplicate_groups =
        build_duplicate_groups_graph(DuplicateType::Body, &graph, &DuplicateConfig::default());
    for group in &duplicate_groups {
        if group.node_ids.len() < 2 {
            continue;
        }
        let local_ids: Vec<NodeId> = group
            .node_ids
            .iter()
            .copied()
            .filter(|id| document_node_ids.contains(id))
            .collect();
        if local_ids.is_empty() {
            continue;
        }
        for node_id in local_ids {
            let Some(entry) = graph.nodes().get(node_id) else {
                continue;
            };
            if entry.is_unified_loser() {
                continue;
            }
            let strings = graph.strings();
            let name: String = entry
                .qualified_name
                .and_then(|sid| strings.resolve(sid))
                .or_else(|| strings.resolve(entry.name))
                .map_or_else(|| "<unknown>".to_string(), |arc| arc.to_string());
            diagnostics.push(make_diagnostic(
                entry_to_range(entry),
                DiagnosticSeverity::WARNING,
                "sqry::duplicate",
                format!(
                    "duplicate group: `{name}` shares its body hash with {others} other symbol(s)",
                    others = group.total_members.saturating_sub(1),
                ),
            ));
        }
    }

    Ok(DiagnosticsOutcome {
        diagnostics,
        partial: false,
        excluded: false,
    })
}

/// Convert a `NodeEntry` (1-based line, byte-column) into a best-effort
/// LSP `Range`. Diagnostics tolerate column drift more than goto/code
/// actions do, so we publish the byte column directly rather than
/// loading the document to compute UTF-16 columns. Clients that render
/// diagnostics will still anchor to the right line.
fn entry_to_range(entry: &sqry_core::graph::unified::storage::arena::NodeEntry) -> Range {
    let start = Position::new(entry.start_line.saturating_sub(1), entry.start_column);
    let end_line = if entry.end_line == 0 {
        entry.start_line.saturating_sub(1)
    } else {
        entry.end_line.saturating_sub(1)
    };
    let end = Position::new(end_line, entry.end_column);
    Range::new(start, end)
}

fn make_diagnostic(
    range: Range,
    severity: DiagnosticSeverity,
    code: &'static str,
    message: String,
) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        code_description: None,
        source: Some(DIAGNOSTIC_SOURCE.to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}
