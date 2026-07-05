//! `daemon/rebuild` handler.
//!
//! Triggers an explicit rebuild for a loaded workspace. The caller
//! identifies the workspace by its directory path; the handler locates
//! the matching `WorkspaceKey` in the manager map and dispatches a
//! rebuild via [`crate::rebuild::RebuildDispatcher::handle_changes`].
//!
//! # State transitions
//!
//! The workspace must be in a state that the manager recognises as
//! loaded (`Loaded`, `Rebuilding`, or `Failed`). If the path is not
//! found at all the handler returns `-32004 WorkspaceNotLoaded`.
//!
//! The `RebuildDispatcher` drives the usual state-machine transitions:
//!
//! ```text
//!   Loaded → Rebuilding → Loaded   (success)
//!   Loaded → Rebuilding → Failed   (rebuild error)
//!   Rebuilding → (cancel prior) → Rebuilding → Loaded / Failed
//! ```
//!
//! # In-flight cancellation
//!
//! If a rebuild is already in flight (e.g., from the `SourceTreeWatcher`),
//! `handle_changes` coalesces the new request into the pending lane per
//! the §J.2 runner-role semantics — the running iteration drains the
//! lane and starts another rebuild incorporating the new request. The
//! `SourceTreeWatcher` subscription remains active throughout.
//!
//! # Force flag
//!
//! `force = false` (default): uses the normal incremental / full
//! decision logic in [`crate::rebuild::decide_mode`]. The dispatcher
//! runs an incremental rebuild unless the change set or file-count
//! heuristics mandate a full one.
//!
//! `force = true`: signals a full rebuild by injecting a
//! `GitChangeClass::TreeDiverged` into the change set, which causes
//! `decide_mode` to unconditionally select `RebuildMode::Full`.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use sqry_core::watch::{ChangeSet, GitChangeClass};

use crate::error::DaemonError;
use crate::workspace::WorkspaceState;

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{RebuildResult, ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const POLL_TIMEOUT: Duration = Duration::from_secs(600);

/// `daemon/rebuild` request parameters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebuildParams {
    /// Directory path of the workspace to rebuild. Must match a
    /// currently-loaded workspace (resolved and canonicalized before
    /// comparison against registered [`crate::workspace::WorkspaceKey`]s).
    pub path: std::path::PathBuf,

    /// When `true`, force a full rebuild from scratch regardless of the
    /// incremental threshold or reverse-dep closure size. When `false`
    /// (default), the existing [`crate::rebuild::decide_mode`] heuristics
    /// decide between full and incremental.
    #[serde(default)]
    pub force: bool,
}

/// Handle one `daemon/rebuild` request.
///
/// 1. Canonicalize `path`.
/// 2. Find the matching workspace in the manager. Return `-32004
///    WorkspaceNotLoaded` if not found.
/// 3. Verify the workspace is in a serving state (not `Unloaded`,
///    `Loading`, or `Evicted`) — return `-32004 WorkspaceNotLoaded`
///    if not.
/// 4. Build a `ChangeSet` that reflects the requested rebuild mode:
///    - `force = false`: empty `ChangeSet` so the dispatcher runs the
///      normal incremental / full decision path. The existing graph has
///      all files registered, so an empty change set will trigger an
///      incremental rebuild or a full rebuild if the closure heuristics
///      demand it.
///    - `force = true`: `ChangeSet` with `git_change_class =
///      Some(GitChangeClass::TreeDiverged)` so `decide_mode` selects
///      `RebuildMode::Full` unconditionally.
/// 5. Dispatch via `RebuildDispatcher::handle_changes`.
/// 6. Read back graph stats and return `RebuildResult`.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: RebuildParams =
        serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    // Step 1: canonicalize path.
    let canonical = resolve_index_root(&params.path)?;

    // Step 2: find the workspace by its root path.
    let (key, ws) = ctx
        .manager
        .find_key_and_workspace_by_path(&canonical)
        .ok_or_else(|| {
            MethodError::Daemon(DaemonError::WorkspaceNotLoaded {
                root: canonical.clone(),
            })
        })?;

    // Step 3: verify the workspace is in a serving state.
    let current_state = ws.load_state();
    if !current_state.is_serving() {
        return Err(MethodError::Daemon(DaemonError::WorkspaceNotLoaded {
            root: canonical.clone(),
        }));
    }

    // Step 4: build the change set.
    //
    // `force = false`: empty ChangeSet → dispatcher runs the normal
    // incremental / full decision path.
    //
    // `force = true`: inject a TreeDiverged git_change_class so
    // `decide_mode` unconditionally selects `RebuildMode::Full`.
    let changes = if params.force {
        ChangeSet {
            changed_files: vec![],
            git_state_changed: true,
            git_change_class: Some(GitChangeClass::TreeDiverged),
        }
    } else {
        ChangeSet {
            changed_files: vec![],
            git_state_changed: false,
            git_change_class: None,
        }
    };

    // Step 5: dispatch rebuild and measure wall-clock duration.
    //
    // `handle_changes` has two exit paths:
    //   (a) This call became the runner — it ran the full rebuild pipeline
    //       and returned after the drain loop completed. Stats are fresh.
    //   (b) Another runner was active — the request was coalesced into
    //       the pending lane and `handle_changes` returned `Ok(())`
    //       immediately. The active runner's drain loop will pick up our
    //       request at its next iteration. Stats would be stale if read
    //       right now.
    //
    // For case (b), poll `rebuild_in_flight` until the drain loop
    // finishes. Once it clears `rebuild_in_flight`, the graph snapshot
    // reflects the rebuild that incorporated our coalesced request.
    let started = Instant::now();
    ctx.dispatcher
        .handle_changes(&key, changes)
        .await
        .map_err(MethodError::Daemon)?;

    // Wait for any in-flight rebuild to complete (handles coalesced case).
    while ws.rebuild_in_flight.load(Ordering::Acquire) {
        if started.elapsed() > POLL_TIMEOUT {
            return Err(MethodError::Daemon(DaemonError::ToolTimeout {
                root: canonical,
                secs: POLL_TIMEOUT.as_secs(),
                deadline_ms: u64::try_from(POLL_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            }));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Step 6: read back graph stats from the freshly published snapshot.
    // `ws.graph` is an ArcSwap; the `load()` gives us the current Arc.
    let graph = ws.graph.load();
    let nodes = graph.node_count() as u64;
    let edges = graph.edge_count() as u64;
    let files_indexed = graph.files().len() as u64;

    let envelope = ResponseEnvelope {
        result: RebuildResult {
            root: canonical,
            // Cluster-G §2.4: existing daemon-rebuild path always blocks
            // until publish, so the only outcome we surface here is
            // `Completed`. The `Started` / `Coalesced` / `Rejected`
            // shapes will land alongside the §2.3 `DispatchOutcome`
            // refactor (deferred — see G follow-up index).
            status: sqry_daemon_protocol::RebuildStatus::Completed,
            duration_ms: Some(duration_ms),
            nodes: Some(nodes),
            edges: Some(edges),
            files_indexed: Some(files_indexed),
            was_full: Some(params.force),
        },
        meta: ResponseMeta::fresh_from(WorkspaceState::Loaded, ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/rebuild: {e}")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use crate::config::DaemonConfig;
    use crate::error::DaemonError;
    use crate::ipc::methods::{HandlerContext, MethodError, daemon_rebuild};
    use crate::ipc::shim_registry::ShimRegistry;
    use crate::workspace::{EmptyGraphBuilder, WorkspaceManager};
    use crate::{JSONRPC_WORKSPACE_EVICTED, RebuildDispatcher};
    use sqry_core::plugin::PluginManager;
    use tokio_util::sync::CancellationToken;

    fn make_config() -> Arc<DaemonConfig> {
        Arc::new(DaemonConfig::default())
    }

    fn make_ctx(manager: Arc<WorkspaceManager>) -> HandlerContext {
        let config = make_config();
        let plugins = Arc::new(PluginManager::default());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), plugins);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        HandlerContext {
            manager,
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
        }
    }

    #[tokio::test]
    async fn unloaded_workspace_returns_workspace_not_loaded() {
        let manager = WorkspaceManager::new_without_reaper(make_config());
        let ctx = make_ctx(manager);

        // Use a path that was never registered.
        let params = json!({ "path": "/nonexistent/workspace" });
        let result = daemon_rebuild::handle(&ctx, params).await;

        match result {
            Err(MethodError::Daemon(DaemonError::WorkspaceNotLoaded { root })) => {
                // The path must not exist on disk so resolve_index_root
                // returns InvalidParams (not WorkspaceNotLoaded).
                // That is the correct pre-flight rejection — the test
                // validates the error type rather than pinning the exact
                // variant because path existence varies by host.
                let _ = root;
            }
            Err(MethodError::InvalidParams(_)) => {
                // Path does not exist on disk → resolve_index_root returns
                // InvalidParams. This is also correct for a non-existent
                // workspace path. Both error kinds are acceptable.
            }
            other => panic!("expected WorkspaceNotLoaded or InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn rebuild_params_force_defaults_to_false() {
        let params: daemon_rebuild::RebuildParams =
            serde_json::from_value(json!({ "path": "/some/path" })).unwrap();
        assert!(!params.force, "force must default to false");
        assert_eq!(params.path, PathBuf::from("/some/path"));
    }

    #[test]
    fn rebuild_params_force_true_parses() {
        let params: daemon_rebuild::RebuildParams =
            serde_json::from_value(json!({ "path": "/some/path", "force": true })).unwrap();
        assert!(params.force);
    }

    #[test]
    fn rebuild_params_rejects_unknown_fields() {
        let err = serde_json::from_value::<daemon_rebuild::RebuildParams>(
            json!({ "path": "/p", "extra": true }),
        )
        .expect_err("unknown fields must be rejected");
        assert!(
            err.to_string().contains("unknown field"),
            "expected 'unknown field' in error: {err}"
        );
    }

    #[test]
    fn workspace_not_loaded_has_code_minus_32004() {
        let err = DaemonError::WorkspaceNotLoaded {
            root: PathBuf::from("/repo"),
        };
        assert_eq!(
            err.jsonrpc_code(),
            Some(JSONRPC_WORKSPACE_EVICTED),
            "WorkspaceNotLoaded must map to -32004"
        );
        let data = err.error_data().expect("must emit structured data");
        assert!(
            data.get("root").is_some(),
            "error_data must include root: {data}"
        );
        assert!(
            data.get("hint").is_some(),
            "error_data must include hint: {data}"
        );
    }
}
