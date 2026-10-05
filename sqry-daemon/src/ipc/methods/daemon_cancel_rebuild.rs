//! `daemon/cancel_rebuild` handler.
//!
//! Cancels any in-flight rebuild for a loaded workspace by setting the
//! per-workspace [`crate::workspace::LoadedWorkspace::rebuild_cancelled`]
//! atomic flag through
//! [`crate::rebuild::RebuildDispatcher::cancel_rebuild`]. The flag is polled
//! at every pass boundary inside the sqry-core build pipeline via the
//! `CancellationToken` mechanism wired up in Phase 7c, so the pipeline
//! aborts at the next safe check-point after the signal is dispatched, and
//! again at the publish recheck. The runner consumes the flag at its
//! cancellation gate: the cancelled iteration leaves the workspace
//! `Unloaded` (the next `daemon/load` brings it back), parked requests are
//! answered `-32004`, and the file watcher keeps watching.
//!
//! # Idempotency
//!
//! If no rebuild is currently in flight the flag is NOT set:
//! `rebuild_cancelled` is only stored when `rebuild_in_flight` is true,
//! read under the rebuild lane, where every runner-role transition happens.
//! This prevents a cancel-while-idle (or one that races the runner's
//! release) from poisoning the next rebuild or the next load.
//!
//! [`CancelRebuildResult::cancelled`] reports `true` when the
//! `rebuild_in_flight` atomic was `true` at the moment the signal was
//! dispatched (i.e., a rebuild was actually running). `false` means the
//! request arrived between rebuilds.
//!
//! # Wire contract
//!
//! - Returns `-32004 WorkspaceNotLoaded` if the path does not match any
//!   registered workspace.
//! - Returns a [`CancelRebuildResult`] on success — the rebuild may
//!   still be in progress when the response is sent; cancellation is
//!   asynchronous.

use serde::Deserialize;
use serde_json::Value;

use crate::error::DaemonError;

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{CancelRebuildResult, ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// `daemon/cancel_rebuild` request parameters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRebuildParams {
    /// Directory path of the workspace whose in-flight rebuild should be
    /// cancelled. Must match a currently-registered workspace.
    pub path: std::path::PathBuf,
}

/// Handle one `daemon/cancel_rebuild` request.
///
/// 1. Canonicalize `path`.
/// 2. Find the matching workspace in the manager.
/// 3. Through the dispatcher, under the rebuild lane: when a rebuild is in
///    flight, set `ws.rebuild_cancelled = true` (the per-workspace
///    cancellation signal polled by the rebuild pipeline).
/// 4. Return `CancelRebuildResult { cancelled: rebuild_was_in_flight }`.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: CancelRebuildParams =
        serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    // Step 1: canonicalize path.
    let canonical = resolve_index_root(&params.path)?;

    // Step 2: find the workspace by its root path.
    let (_key, ws) = ctx
        .manager
        .find_key_and_workspace_by_path(&canonical)
        .ok_or_else(|| {
            MethodError::Daemon(DaemonError::WorkspaceNotLoaded {
                root: canonical.clone(),
            })
        })?;

    // Step 3: cancel under the rebuild lane. Setting the flag while idle
    // would poison the NEXT rebuild (its cancellation gate would abort it)
    // and the next load; reading `rebuild_in_flight` under the lane means the
    // runner either has released the role (nothing is set) or will see the
    // flag at its gate before it does. When in flight, the
    // `spawn_cancellation_forwarder` task in
    // `RebuildDispatcher::execute_rebuild` polls `rebuild_cancelled` at
    // ~50ms intervals and calls `token.cancel()` on the next observation,
    // which propagates the cancel signal to the sqry-core build pipeline at
    // its next pass boundary.
    let rebuild_was_in_flight = ctx.dispatcher.cancel_rebuild(&ws).await;

    let envelope = ResponseEnvelope {
        result: CancelRebuildResult {
            root: canonical,
            cancelled: rebuild_was_in_flight,
        },
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/cancel_rebuild: {e}")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use crate::error::DaemonError;
    use crate::ipc::methods::{MethodError, daemon_cancel_rebuild};

    #[test]
    fn cancel_rebuild_params_parses() {
        let params: daemon_cancel_rebuild::CancelRebuildParams =
            serde_json::from_value(json!({ "path": "/some/path" })).unwrap();
        assert_eq!(params.path, PathBuf::from("/some/path"));
    }

    #[test]
    fn cancel_rebuild_params_rejects_unknown_fields() {
        let err = serde_json::from_value::<daemon_cancel_rebuild::CancelRebuildParams>(
            json!({ "path": "/p", "force": true }),
        )
        .expect_err("unknown fields must be rejected");
        assert!(
            err.to_string().contains("unknown field"),
            "expected 'unknown field' in error: {err}"
        );
    }

    #[tokio::test]
    async fn cancel_nonexistent_workspace_returns_not_loaded() {
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        use crate::RebuildDispatcher;
        use crate::config::DaemonConfig;
        use crate::ipc::methods::HandlerContext;
        use crate::ipc::shim_registry::ShimRegistry;
        use crate::workspace::{EmptyGraphBuilder, WorkspaceManager};

        // The environment is read while the configuration is built; the
        // lock is released before the handler is awaited.
        let config = {
            let _env = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::new(DaemonConfig::default())
        };
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let roster = Arc::new(crate::workspace::WorkspaceRosterResolver::new());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), roster);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        let ctx = HandlerContext {
            manager,
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
            mcp_redaction: std::sync::Arc::new(crate::mcp_host::redaction::McpRedaction::disabled()),
        };

        // A non-existent path either fails at resolve_index_root (if the dir
        // doesn't exist) or at find_key_and_workspace_by_path. Both are
        // acceptable rejections.
        let params = json!({ "path": "/nonexistent/workspace" });
        let result = daemon_cancel_rebuild::handle(&ctx, params).await;

        match result {
            Err(MethodError::Daemon(DaemonError::WorkspaceNotLoaded { .. })) => {}
            Err(MethodError::InvalidParams(_)) => {}
            other => panic!("expected WorkspaceNotLoaded or InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_while_idle_does_not_poison_rebuild_cancelled_flag() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        use tokio_util::sync::CancellationToken;

        use sqry_core::project::ProjectRootMode;

        use crate::RebuildDispatcher;
        use crate::config::DaemonConfig;
        use crate::ipc::methods::HandlerContext;
        use crate::ipc::shim_registry::ShimRegistry;
        use crate::workspace::state::WorkspaceKey;
        use crate::workspace::{EmptyGraphBuilder, WorkspaceManager, WorkspaceState};

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();

        // The environment is read while the configuration is built; the
        // lock is released before the handler is awaited.
        let config = {
            let _env = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::new(DaemonConfig::default())
        };
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));

        // Register a Loaded workspace (no rebuild in flight).
        let key = WorkspaceKey::new(canonical.clone(), ProjectRootMode::GitRoot, 0x1);
        manager.insert_workspace_in_state_for_test(key, WorkspaceState::Loaded);

        let roster = Arc::new(crate::workspace::WorkspaceRosterResolver::new());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), roster);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        let ctx = HandlerContext {
            manager: Arc::clone(&manager),
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
            mcp_redaction: std::sync::Arc::new(crate::mcp_host::redaction::McpRedaction::disabled()),
        };

        let params = json!({ "path": canonical.to_string_lossy().as_ref() });
        let result = daemon_cancel_rebuild::handle(&ctx, params).await.unwrap();

        // `cancelled` must be false — no rebuild was in flight.
        let envelope: serde_json::Value = result;
        assert_eq!(
            envelope["result"]["cancelled"],
            serde_json::Value::Bool(false),
            "cancel while idle must report cancelled=false"
        );

        // Retrieve the workspace and verify the flag was NOT set.
        let (_key, ws) = manager
            .find_key_and_workspace_by_path(&canonical)
            .expect("workspace must still exist");
        assert!(
            !ws.rebuild_cancelled.load(Ordering::Acquire),
            "cancel while idle must not set rebuild_cancelled (would poison next rebuild)"
        );
    }
}
