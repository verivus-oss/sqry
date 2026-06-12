//! `sqry context-propagation` — Go context-propagation analysis CLI (T3.7).
//!
//! Surfaces `context.Context` plumbing breaks classified by
//! `sqry_db::queries::context_propagation::ContextPropagationQuery`:
//!
//! - `BreakSite` — sync caller has a `context.Context` parameter, callee
//!   accepts `context.Context`, but the call passes zero context args.
//! - `UnthreadedGoroutine` — `go callee(...)` with a ctx-accepting callee.
//! - `HttpHandlerLeak` — caller is `func(http.ResponseWriter, *http.Request)`
//!   and the callee is ctx-accepting, but the call drops `r.Context()`.
//!
//! This CLI mirrors the MCP `context_propagation` tool from Cluster G; both
//! route through `sqry_db::queries::dispatch::make_query_db_cold` so the
//! per-publish cache contract from CLAUDE.md §"Persistence (V10)" is
//! preserved.

use crate::args::{Cli, ContextPropagationMode};
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_db::queries::context_propagation::{
    ContextLeak, ContextLeakSet, ContextMode, ContextModeFilter, ContextPropagationKey,
    ContextPropagationQuery, ContextScope,
};
use sqry_db::queries::dispatch::make_query_db_cold;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Exit code returned when no `.sqry-index` is discoverable. Documented in
/// `CLI_INTEGRATION.md` §1.3 as part of the public surface contract.
const EXIT_NO_INDEX: i32 = 3;

/// Exit code for invalid `--scope` parsing (clap handles invalid `--mode`
/// at the parser layer via `ValueEnum`).
const EXIT_INVALID_ARG: i32 = 2;

/// One JSON row produced by `--output json` (default `cli.json`). Matches
/// the schema enumerated in `CLI_INTEGRATION.md` §1.3.
#[derive(Debug, Clone, Serialize)]
pub struct ContextLeakHit {
    /// `caller_node_id` qualified or short name (matches the `caller`
    /// field expected by JSON consumers).
    pub caller: String,
    /// `callee_node_id` qualified or short name.
    pub callee: String,
    /// `break_site` | `unthreaded_goroutine` | `http_handler_leak`.
    pub mode: String,
    /// File holding the caller function.
    pub caller_file: String,
    /// Call-site span coordinates (1-based lines, byte columns).
    pub call_site: CallSiteSpan,
    /// Caller's `ctx` parameter name when present; `None` for
    /// HTTP-handler + unthreaded-goroutine leak modes where the caller
    /// does not own a `context.Context` parameter directly.
    pub caller_ctx_param: Option<String>,
}

/// Call-site coordinates in 1-based lines + 0-based byte columns.
#[derive(Debug, Clone, Serialize)]
pub struct CallSiteSpan {
    pub file: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Entry point for the `sqry context-propagation` subcommand.
///
/// # Errors
///
/// Returns `Err` if the graph fails to load (corrupt snapshot, unreadable
/// `.sqry-index`, plugin-incompatible). `--scope file:<bad-path>` and the
/// missing-index case do NOT return `Err`; they call `std::process::exit`
/// with the documented exit code so the CLI honours the public exit-code
/// contract in `CLI_INTEGRATION.md` §1.3 before `main.rs`'s anyhow→exit
/// mapping can re-classify the error.
pub fn run_context_propagation(
    cli: &Cli,
    path: Option<&str>,
    scope: &str,
    mode: ContextPropagationMode,
    limit: usize,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        PathBuf::from,
    );

    let Some(location) = find_nearest_index(&search_path) else {
        let _ = streams.write_diagnostic(
            "No .sqry-index found. Run 'sqry index' first to build the graph index.",
        );
        std::process::exit(EXIT_NO_INDEX);
    };

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&location.index_root, &config, cli, no_op_reporter())
        .context("failed to load graph; run 'sqry index' to rebuild")?;

    let snapshot = Arc::new(graph.snapshot());

    let scope_value = match parse_scope(scope, &location.index_root, &snapshot) {
        Ok(parsed) => parsed,
        Err(ScopeError::InvalidSyntax(msg)) => {
            let _ = streams.write_diagnostic(&format!("invalid --scope value: {msg}"));
            std::process::exit(EXIT_INVALID_ARG);
        }
        Err(ScopeError::FileNotInIndex(p)) => {
            // Matches the MCP tool's short-circuit: non-resolvable file
            // returns an empty leak set, exit 0. Print zero leaks in the
            // appropriate output mode and return.
            return emit_results(&mut streams, cli.json, &[], &p, mode);
        }
    };

    let key = ContextPropagationKey {
        scope: scope_value,
        mode: mode.into(),
    };
    let db = make_query_db_cold(Arc::clone(&snapshot), &location.index_root);
    let leak_set: Arc<ContextLeakSet> = db.get::<ContextPropagationQuery>(&key);

    let hits: Vec<ContextLeakHit> = leak_set
        .leaks
        .iter()
        .take(limit)
        .map(|leak| leak_to_hit(&snapshot, leak))
        .collect();

    emit_results(&mut streams, cli.json, &hits, scope, mode)
}

enum ScopeError {
    InvalidSyntax(String),
    FileNotInIndex(String),
}

fn parse_scope(
    raw: &str,
    workspace_root: &Path,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> std::result::Result<ContextScope, ScopeError> {
    if raw == "global" {
        return Ok(ContextScope::Global);
    }
    if let Some(rest) = raw.strip_prefix("file:") {
        if rest.is_empty() {
            return Err(ScopeError::InvalidSyntax(
                "file: prefix requires a path".to_string(),
            ));
        }
        let candidate = PathBuf::from(rest);
        let absolute = if candidate.is_absolute() {
            candidate
        } else {
            workspace_root.join(&candidate)
        };
        // Walk the snapshot's FileRegistry and look for a byte-for-byte
        // match on the registered path. We don't canonicalise — the
        // registry's path is what the index recorded at build time, and
        // re-canonicalising can drift if the workspace was moved.
        let canonical = absolute.canonicalize().unwrap_or(absolute);
        let resolved = snapshot.files().iter().find_map(|(fid, registered)| {
            let r: &std::path::Path = registered.as_ref();
            let r_canon = r.canonicalize().unwrap_or_else(|_| r.to_path_buf());
            if r == canonical.as_path() || r_canon == canonical {
                Some(fid)
            } else {
                None
            }
        });
        return match resolved {
            Some(fid) => Ok(ContextScope::File(fid)),
            None => Err(ScopeError::FileNotInIndex(rest.to_string())),
        };
    }
    Err(ScopeError::InvalidSyntax(format!(
        "expected 'global' or 'file:<path>', got '{raw}'"
    )))
}

fn leak_to_hit(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    leak: &ContextLeak,
) -> ContextLeakHit {
    let source_label = node_label(snapshot, leak.caller);
    let target_label = node_label(snapshot, leak.callee);
    let caller_file = snapshot
        .get_node(leak.caller)
        .and_then(|entry| snapshot.files().resolve(entry.file))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let caller_ctx_param = leak
        .caller_ctx_param
        .map(|nid| node_label(snapshot, nid))
        .filter(|s| !s.is_empty());
    ContextLeakHit {
        caller: source_label,
        callee: target_label,
        mode: concrete_mode_label(leak.mode),
        caller_file: caller_file.clone(),
        call_site: CallSiteSpan {
            file: caller_file,
            // tree-sitter Point::line is 0-based. Surface 1-based for
            // IDE-friendly jump-to.
            start_line: span_line_to_u32(leak.call_span.start.line) + 1,
            start_column: usize_to_u32(leak.call_span.start.column),
            end_line: span_line_to_u32(leak.call_span.end.line) + 1,
            end_column: usize_to_u32(leak.call_span.end.column),
        },
        caller_ctx_param,
    }
}

fn span_line_to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX - 1)
}

fn usize_to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn node_label(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node: sqry_core::graph::unified::node::NodeId,
) -> String {
    let Some(entry) = snapshot.get_node(node) else {
        return String::new();
    };
    if let Some(sid) = entry.qualified_name
        && let Some(qualified) = snapshot.strings().resolve(sid)
    {
        return qualified.to_string();
    }
    snapshot
        .strings()
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn concrete_mode_label(mode: ContextMode) -> String {
    match mode {
        ContextMode::BreakSite => "break_site",
        ContextMode::UnthreadedGoroutine => "unthreaded_goroutine",
        ContextMode::HttpHandlerLeak => "http_handler_leak",
    }
    .to_string()
}

fn mode_label(mode: ContextPropagationMode) -> &'static str {
    match mode {
        ContextPropagationMode::All => "all",
        ContextPropagationMode::BreakSite => "break_site",
        ContextPropagationMode::UnthreadedGoroutine => "unthreaded_goroutine",
        ContextPropagationMode::HttpHandlerLeak => "http_handler_leak",
    }
}

fn emit_results(
    streams: &mut OutputStreams,
    json: bool,
    hits: &[ContextLeakHit],
    scope: &str,
    mode: ContextPropagationMode,
) -> Result<()> {
    if json {
        let payload = serde_json::to_string_pretty(&hits)
            .context("serializing context-propagation hits as JSON")?;
        streams.write_result(&payload)?;
    } else if hits.is_empty() {
        streams.write_result(&format!(
            "no context-propagation leaks (scope={scope}, mode={mode})",
            mode = mode_label(mode),
        ))?;
    } else {
        for hit in hits {
            streams.write_result(&format!(
                "{caller} -> {callee}    [{mode}]",
                caller = hit.caller,
                callee = hit.callee,
                mode = hit.mode,
            ))?;
            streams.write_result(&format!(
                "  call site:    {file}:{line}:{col}",
                file = hit.call_site.file,
                line = hit.call_site.start_line,
                col = hit.call_site.start_column,
            ))?;
            if let Some(param) = &hit.caller_ctx_param {
                streams.write_result(&format!("  caller param: {param}"))?;
            }
        }
    }
    let _ = scope;
    let _ = ContextModeFilter::from(mode); // assertion-by-construction
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_value_enum_round_trips_to_filter() {
        // T3.7 Cluster G-ext smoke: every CLI mode maps to its db filter.
        for (cli_mode, expected) in [
            (ContextPropagationMode::All, ContextModeFilter::All),
            (
                ContextPropagationMode::BreakSite,
                ContextModeFilter::BreakSite,
            ),
            (
                ContextPropagationMode::UnthreadedGoroutine,
                ContextModeFilter::UnthreadedGoroutine,
            ),
            (
                ContextPropagationMode::HttpHandlerLeak,
                ContextModeFilter::HttpHandlerLeak,
            ),
        ] {
            let got: ContextModeFilter = cli_mode.into();
            assert_eq!(got, expected, "{cli_mode:?} maps incorrectly");
        }
    }

    #[test]
    fn span_helpers_saturate_on_overflow() {
        // tree-sitter Point fields are usize; sqry exposes 1-based u32
        // lines for IDE consumers. Asserting saturation behaviour pins
        // the contract that wildly large line numbers (from a corrupt
        // tree, etc.) downgrade to `u32::MAX - 1` rather than overflow.
        assert_eq!(span_line_to_u32(0), 0);
        assert_eq!(span_line_to_u32(42), 42);
        assert_eq!(span_line_to_u32(usize::MAX), u32::MAX - 1);
        assert_eq!(usize_to_u32(0), 0);
        assert_eq!(usize_to_u32(7), 7);
        assert_eq!(usize_to_u32(usize::MAX), u32::MAX);
    }

    #[test]
    fn mode_label_matches_documented_spelling() {
        // Pins the public CLI label spellings used in text-mode output
        // and in the diagnostic emitted by `emit_results` when zero leaks
        // surface. Renaming any of these is a CLI_INTEGRATION.md delta.
        assert_eq!(mode_label(ContextPropagationMode::All), "all");
        assert_eq!(mode_label(ContextPropagationMode::BreakSite), "break_site");
        assert_eq!(
            mode_label(ContextPropagationMode::UnthreadedGoroutine),
            "unthreaded_goroutine"
        );
        assert_eq!(
            mode_label(ContextPropagationMode::HttpHandlerLeak),
            "http_handler_leak"
        );
    }
}
