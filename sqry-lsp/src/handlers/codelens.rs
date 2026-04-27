//! `textDocument/codeLens` handler.
//!
//! sqry does not surface code lenses today. The handler returns an
//! empty list for any path it is asked about.
//!
//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — gates on
//! [`crate::session::SessionManager::evaluate_handler_gate`] before
//! the body runs.

use crate::session::{HandlerGate, SessionManager};
use anyhow::Result;
use tower_lsp::lsp_types::{CodeLens, CodeLensParams};

/// STEP_11_4 — outcome of a `textDocument/codeLens` request,
/// including the gate verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeLensOutcome {
    /// The code lenses to publish. Always empty today.
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

/// STEP_11_4 — gated code-lens handler. Never probes the filesystem
/// per folder.
///
/// # Errors
///
/// This handler does not synthesise code lenses today and therefore
/// never returns `Err` — the `Result` return type is preserved for
/// parity with other handlers.
pub fn handle(session: &SessionManager, params: &CodeLensParams) -> Result<CodeLensOutcome> {
    let uri = &params.text_document.uri;
    match session.evaluate_handler_gate(uri) {
        HandlerGate::Member(_) => Ok(CodeLensOutcome {
            lenses: Vec::new(),
            partial: true,
            excluded: false,
        }),
        HandlerGate::Excluded => Ok(CodeLensOutcome {
            lenses: Vec::new(),
            partial: false,
            excluded: true,
        }),
        HandlerGate::Continue => Ok(CodeLensOutcome::empty()),
    }
}
