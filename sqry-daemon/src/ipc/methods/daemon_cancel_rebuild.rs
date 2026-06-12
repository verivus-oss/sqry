//! `daemon/cancel_rebuild` handler.
//!
//! Cancels any in-flight rebuild for a loaded workspace by setting the
//! per-workspace [`crate::workspace::LoadedWorkspace::rebuild_cancelled`]
//! atomic flag. The flag is polled at every pass boundary inside the
//! sqry-core build pipeline via the `CancellationToken` mechanism wired
//! up in Phase 7c, so the pipeline aborts at the next safe check-point
//! after the signal is dispatched.
//!
//! # Idempotency
//!
//! If no rebuild is currently in flight the flag is NOT set —
//! `rebuild_cancelled` is only stored when `rebuild_in_flight` is
//! true at the moment the handler runs. This prevents a cancel-while-
//! idle from poisoning the next rebuild (the drain loop's top-of-loop
//! eviction gate would interpret a leftover `true` as an eviction
//! signal and abort the subsequent build).
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

use std::sync::atomic::Ordering;

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
/// 3. Set `ws.rebuild_cancelled = true` (the per-workspace
///    cancellation signal polled by the rebuild pipeline).
/// 4. Return `CancelRebuildResult { cancelled: rebuild_was_in_flight }`.
pub(crate) fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
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

    // Step 3: record whether a rebuild is in flight before signalling
    // cancellation. `rebuild_in_flight` is an AtomicBool on
    // `LoadedWorkspace`; an Acquire load captures the current state.
    let rebuild_was_in_flight = ws.rebuild_in_flight.load(Ordering::Acquire);

    // Only set the cancellation flag when a rebuild is actually running.
    // Setting it while idle would poison the NEXT rebuild: the drain
    // loop's top-of-loop eviction gate observes `rebuild_cancelled ==
    // true` and aborts immediately with `WorkspaceEvicted`.
    //
    // When in-flight, the `spawn_cancellation_forwarder` task in
    // `RebuildDispatcher::execute_rebuild` polls `rebuild_cancelled`
    // at ~50ms intervals and calls `token.cancel()` on the next
    // observation, which propagates the cancel signal to the sqry-core
    // build pipeline at its next pass boundary.
    if rebuild_was_in_flight {
        ws.rebuild_cancelled.store(true, Ordering::Release);
    }

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
        use sqry_core::plugin::PluginManager;

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(PluginManager::default());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), plugins);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        let ctx = HandlerContext {
            manager,
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
        };

        // A non-existent path either fails at resolve_index_root (if the dir
        // doesn't exist) or at find_key_and_workspace_by_path. Both are
        // acceptable rejections.
        let params = json!({ "path": "/nonexistent/workspace" });
        let result = daemon_cancel_rebuild::handle(&ctx, params);

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

        use sqry_core::plugin::PluginManager;
        use sqry_core::project::ProjectRootMode;

        use crate::RebuildDispatcher;
        use crate::config::DaemonConfig;
        use crate::ipc::methods::HandlerContext;
        use crate::ipc::shim_registry::ShimRegistry;
        use crate::workspace::state::WorkspaceKey;
        use crate::workspace::{EmptyGraphBuilder, WorkspaceManager, WorkspaceState};

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();

        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));

        // Register a Loaded workspace (no rebuild in flight).
        let key = WorkspaceKey::new(canonical.clone(), ProjectRootMode::GitRoot, 0x1);
        manager.insert_workspace_in_state_for_test(key, WorkspaceState::Loaded);

        let plugins = Arc::new(PluginManager::default());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), plugins);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        let ctx = HandlerContext {
            manager: Arc::clone(&manager),
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
        };

        let params = json!({ "path": canonical.to_string_lossy().as_ref() });
        let result = daemon_cancel_rebuild::handle(&ctx, params).unwrap();

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
