//! `textDocument/publishDiagnostics`-style helper.
//!
//! sqry does not synthesise diagnostics today (it is a search /
//! relation server, not a typechecker). The handler returns an empty
//! diagnostic list for any path it is asked about.
//!
//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — even though
//! the steady-state response is empty, the gate **must** consult
//! [`crate::session::SessionManager::evaluate_handler_gate`] before
//! the body runs, so member-folder and excluded-path requests
//! short-circuit through the same code path the
//! `sqry/indexStatus` handler already uses (STEP_4).

use crate::session::{HandlerGate, SessionManager};
use anyhow::Result;
use tower_lsp::lsp_types::{Diagnostic, Url};

/// STEP_11_4 — outcome of a `textDocument/publishDiagnostics`-style
/// request, including the gate verdict so the LSP server can
/// surface "member" / "excluded" hints to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsOutcome {
    /// The diagnostics to publish. Always empty today.
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

/// STEP_11_4 — gated diagnostics handler. Never probes the filesystem
/// per folder; consults [`SessionManager::evaluate_handler_gate`] only.
///
/// # Errors
///
/// This handler does not synthesise diagnostics today and therefore
/// never returns `Err` — the `Result` return type is preserved for
/// parity with other handlers.
pub fn handle(session: &SessionManager, uri: &Url) -> Result<DiagnosticsOutcome> {
    match session.evaluate_handler_gate(uri) {
        HandlerGate::Member(_) => Ok(DiagnosticsOutcome {
            diagnostics: Vec::new(),
            partial: true,
            excluded: false,
        }),
        HandlerGate::Excluded => Ok(DiagnosticsOutcome {
            diagnostics: Vec::new(),
            partial: false,
            excluded: true,
        }),
        HandlerGate::Continue => Ok(DiagnosticsOutcome::empty()),
    }
}
