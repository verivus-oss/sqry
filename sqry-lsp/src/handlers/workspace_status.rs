//! Handler for the `sqry/workspaceStatus` JSON-RPC method.
//!
//! Returns a wire-side view of the session's [`LogicalWorkspace`]:
//! - `workspace_id_short` / `workspace_id_full` — BLAKE3 identity
//!   surfaces (acceptance criterion 6).
//! - `aggregate` — the §1.4 aggregate [`WorkspaceIndexStatus`]
//!   (per-source-root state + summary counters), recomputed fresh.
//! - `project_root_mode`, `source_roots`, `member_folders`,
//!   `exclusions` — projection of the workspace structure for tooling.
//!
//! The handler intentionally takes no parameters — it always describes
//! the session's current logical workspace. A future
//! `sqry/workspaceUpdate` (Step 5) will replace the workspace before
//! this handler is called again.
//!
//! # Member-folder gating
//!
//! Per the `STEP_4` scope split: this handler is the *workspace-level*
//! status surface; it is correct for all paths because it describes the
//! workspace itself, not a single path. The path-classification gating
//! that's required for `diagnostics`, `hover`, `codelens`,
//! `document_symbol`, `code_action`, and `workspace_symbol` (acceptance
//! criterion 5) is the responsibility of `STEP_11_4`. `STEP_4` scopes
//! gating to the two surfaces directly in its contract: `index_status`
//! (handler-side) and `workspace_status` (here, by virtue of the
//! handler being workspace-scoped, not path-scoped).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::session::{SessionManager, WorkspaceStatusInfo, build_workspace_status_info};

/// Wire-side parameters for `sqry/workspaceStatus`.
///
/// Currently empty (the handler always describes the current workspace),
/// but kept as a struct so a future client can pass `workspace_id` to
/// sanity-check it observes the workspace it expects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SqryWorkspaceStatusParams {
    /// Optional client-side `workspace_id_full` for sanity-checking
    /// against the server's current workspace. When `Some` and the
    /// values disagree, the handler still returns the server's view
    /// — it's the client's responsibility to act on the mismatch.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Response payload for `sqry/workspaceStatus` — a thin wrapper around
/// [`WorkspaceStatusInfo`] kept so future fields (e.g. quotas) can be
/// added without changing the wire envelope.
#[derive(Debug, Clone, Serialize)]
pub struct SqryWorkspaceStatusResult {
    /// Workspace identity, structure, and aggregate index status.
    #[serde(flatten)]
    pub info: WorkspaceStatusInfo,
}

/// Compute the workspace-status response for the session's current
/// logical workspace.
///
/// # Errors
///
/// Currently infallible — kept as `Result` so future enhancements
/// (e.g. fetching daemon-side counts) can fail without changing the
/// signature.
#[allow(clippy::unnecessary_wraps)] // future-compatible signature.
pub fn workspace_status(
    session: &SessionManager,
    _params: &SqryWorkspaceStatusParams,
) -> Result<SqryWorkspaceStatusResult> {
    let workspace = session.logical_workspace();
    let info = build_workspace_status_info(workspace.as_ref());
    Ok(SqryWorkspaceStatusResult { info })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LspOptions;
    use crate::session::SessionManager;
    use std::path::PathBuf;

    fn make_session() -> SessionManager {
        SessionManager::new(LspOptions {
            stdio: false,
            socket: None,
            index_root: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
            log_level: "warn".into(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
        })
    }

    #[test]
    fn workspace_status_returns_workspace_id_short_and_full() {
        let session = make_session();
        let result = workspace_status(&session, &SqryWorkspaceStatusParams::default())
            .expect("workspace_status should not fail on default session");
        assert_eq!(
            result.info.workspace_id_short.len(),
            16,
            "short ID should be 16 hex chars"
        );
        assert_eq!(
            result.info.workspace_id_full.len(),
            64,
            "full ID should be 64 hex chars"
        );
        assert!(
            result
                .info
                .workspace_id_full
                .starts_with(result.info.workspace_id_short.as_str())
        );
    }

    #[test]
    fn workspace_status_default_session_has_anonymous_multi_root() {
        let session = make_session();
        let result = workspace_status(&session, &SqryWorkspaceStatusParams::default()).unwrap();
        assert!(
            !result.info.source_roots.is_empty(),
            "default AnonymousMultiRoot should expose at least one source root"
        );
        assert_eq!(result.info.member_folders.len(), 0);
        assert_eq!(result.info.exclusions.len(), 0);
    }
}
