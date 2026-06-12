//! `textDocument/codeLens` handler.
//!
//! Emits caller-count code lenses on top of every function- or
//! method-shaped node in the requested document. Each lens carries a
//! [`crate::handlers::code_action::COMMAND_SHOW_CALLERS`]
//! (`"sqry.showCallers"`) command so the LSP client can pivot from the
//! count display into the existing show-callers code action.
//!
//! The caller count is derived from `sqry-db`'s
//! [`sqry_db::queries::dispatch::mcp_callers_query`] inversion wrapper —
//! the transport-facing shim that returns the *callers of X* set under
//! the same `graph_eval` direction convention MCP and CLI expose. This
//! preserves the planner's set-membership cache contract (the bare
//! `CallersQuery` keyed on X returns the *X-calls-Y* direction).
//!
//! `STEP_11_4` (workspace-aware-cross-repo, 2026-04-26) — gates on
//! [`crate::session::SessionManager::evaluate_handler_gate`] before any
//! graph access so member-folder and excluded-path requests
//! short-circuit through the same code path the `sqry/indexStatus`
//! handler already uses.
//!
//! Closes audit findings A100 + A157
//! (cli-help-impl-alignment-2026-05-04, CRUD row C074a). Prior to
//! C074a this handler always returned an empty
//! [`CodeLensOutcome::empty`].

use crate::handlers::code_action::COMMAND_SHOW_CALLERS;
use crate::session::{HandlerGate, SessionManager};
use anyhow::Result;
use serde_json::json;
use sqry_core::graph::unified::node::NodeKind;
use sqry_db::queries::RelationKey;
use sqry_db::queries::dispatch::{make_query_db_cold, mcp_callers_query};
use std::sync::Arc;
use tower_lsp::lsp_types::{CodeLens, CodeLensParams, Command};

/// `STEP_11_4` — outcome of a `textDocument/codeLens` request,
/// including the gate verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeLensOutcome {
    /// The code lenses to publish.
    pub lenses: Vec<CodeLens>,
    /// `true` when the request URI lives inside a member folder.
    pub partial: bool,
    /// `true` when the request URI lives inside an excluded path.
    pub excluded: bool,
}

impl CodeLensOutcome {
    /// The empty / non-gated outcome.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            lenses: Vec::new(),
            partial: false,
            excluded: false,
        }
    }
}

/// `STEP_11_4` — gated code-lens handler. Never probes the filesystem
/// per folder.
///
/// On `HandlerGate::Continue`, walks every function/method node in the
/// requested document, looks up the caller-count via
/// [`mcp_callers_query`] keyed on the node's qualified (or fallback
/// simple) name, and emits one [`CodeLens`] per node with the
/// [`COMMAND_SHOW_CALLERS`] command so the client can pivot into the
/// existing show-callers code action. The `data` field carries
/// `{ "name": <qualified-or-simple>, "count": <usize> }` so tests and
/// clients can inspect the count without rendering the title.
///
/// # Errors
///
/// Returns an error when document loading or graph access fails
/// (propagated through [`SessionManager::nodes_in_document`] /
/// [`crate::handlers::node_range_lsp`]).
pub fn handle(session: &SessionManager, params: &CodeLensParams) -> Result<CodeLensOutcome> {
    let uri = &params.text_document.uri;
    match session.evaluate_handler_gate(uri) {
        HandlerGate::Member(_) => {
            return Ok(CodeLensOutcome {
                lenses: Vec::new(),
                partial: true,
                excluded: false,
            });
        }
        HandlerGate::Excluded => {
            return Ok(CodeLensOutcome {
                lenses: Vec::new(),
                partial: false,
                excluded: true,
            });
        }
        HandlerGate::Continue => {}
    }

    // Pull every function/method node defined in the requested document.
    let nodes = session.nodes_in_document(uri)?;
    if nodes.is_empty() {
        return Ok(CodeLensOutcome::empty());
    }

    // Resolve the workspace path so we can build a `QueryDb` with a
    // graph snapshot — the snapshot is what the inversion wrapper
    // walks to count callers.
    let Ok(path) = uri.to_file_path() else {
        return Ok(CodeLensOutcome::empty());
    };
    let Some(graph) = session.graph_for_path(&path)? else {
        // No graph yet (e.g. cold-start before auto-index completes);
        // surface no lenses rather than fail the request.
        return Ok(CodeLensOutcome::empty());
    };

    let snapshot = Arc::new(graph.snapshot());
    let workspace_root = session.index_root_for_cold_load();
    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);

    let mut lenses = Vec::new();
    for node in &nodes {
        if !is_callable(node.kind) {
            continue;
        }

        let lookup_name = node.qualified_name_or_name().to_string();
        let key = RelationKey::exact(lookup_name.clone());
        let callers = mcp_callers_query(&db, &key);
        let count = callers.len();

        let range = match super::node_range_lsp(session, node) {
            Ok(range) => range,
            Err(err) => {
                log::warn!("code_lens: failed to compute LSP range for {lookup_name}: {err}");
                continue;
            }
        };

        let title = if count == 1 {
            format!("{count} caller")
        } else {
            format!("{count} callers")
        };

        let arguments = json!({
            "uri": uri,
            "position": {
                "line": range.start.line,
                "character": range.start.character,
            }
        });

        let command = Command {
            title: title.clone(),
            command: COMMAND_SHOW_CALLERS.into(),
            arguments: Some(vec![arguments]),
        };

        let data = json!({
            "name": lookup_name,
            "count": count,
        });

        lenses.push(CodeLens {
            range,
            command: Some(command),
            data: Some(data),
        });
    }

    Ok(CodeLensOutcome {
        lenses,
        partial: false,
        excluded: false,
    })
}

/// Whether a node kind participates in the caller-count code-lens
/// surface. Mirrors the dispatch taxonomy: only callable kinds carry
/// meaningful caller relations.
fn is_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Macro | NodeKind::LambdaTarget
    )
}
