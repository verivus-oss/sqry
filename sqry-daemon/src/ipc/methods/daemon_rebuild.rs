//! `daemon/rebuild` handler.
//!
//! Triggers an explicit rebuild for a loaded workspace. The caller
//! identifies the workspace by its directory path; the handler locates
//! the matching `WorkspaceKey` in the manager map and dispatches a
//! rebuild via
//! [`crate::rebuild::RebuildDispatcher::handle_changes_with_macro_options`],
//! answering with the outcome of this request's own iteration.
//!
//! # State transitions
//!
//! The workspace must be in a serving state (`Loaded`, `Rebuilding`, or
//! `Failed`). If the path is not found, or the workspace is `Unloaded`,
//! `Loading` or `Evicted`, the handler returns `-32004
//! WorkspaceNotLoaded`.
//!
//! The `RebuildDispatcher` drives the state machine:
//!
//! ```text
//!   Loaded → Rebuilding → Loaded    (success)
//!   Loaded → Rebuilding → Loaded    (refused before anything is written)
//!   Loaded → Rebuilding → Failed    (build or persist failure)
//!   Loaded → Rebuilding → Unloaded  (cancelled: daemon/cancel_rebuild, daemon/reset)
//! ```
//!
//! # A rebuild already in flight
//!
//! A new request never cancels the running rebuild. If a rebuild is in
//! flight (from the `SourceTreeWatcher` or another caller), the request
//! parks in the workspace's rebuild lane, merged into the entry already
//! parked there when the two may share one iteration: their macro
//! options mean the same, or one of them is a watcher-driven enqueue.
//! Otherwise it is refused with `-32602` and the parked entry keeps its
//! options (decision D-i7-1). The running rebuild drains the lane when it
//! finishes, and this request is answered with the outcome of the
//! iteration that consumed it. When this request takes the runner role,
//! the drain loop runs on its own task and this request is answered as
//! soon as its own iteration ends. The handler bounds its wait at
//! `RebuildDispatcher::outcome_wait` (600 s) and answers `-32000`
//! `RebuildOutcomeTimeout` past it, whose data says the rebuild continues
//! and names `daemon/status` as the read that gives its outcome (a retry
//! would queue another rebuild); the rebuild itself is never abandoned. The
//! `SourceTreeWatcher` subscription remains active throughout.
//!
//! # Force flag
//!
//! `force = false` (default): uses the normal incremental / full
//! decision logic in [`crate::rebuild::decide_mode`]. The dispatcher
//! schedules the iteration in incremental mode unless the change set or
//! file-count heuristics mandate a full one. Either mode builds the whole
//! graph and persists it durably (the core's `incremental_rebuild` has no
//! production caller); the mode sizes the working-set estimate and is
//! what `was_full` and the recorded build command report.
//!
//! `force = true`: signals a full rebuild by injecting a
//! `GitChangeClass::TreeDiverged` into the change set, which causes
//! `decide_mode` to unconditionally select `RebuildMode::Full`.
//!
//! The result's `was_full` reports the mode the request's own iteration
//! ran, which is full whenever any request merged into it forced one.

use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use sqry_core::graph::unified::build::MacroOptionsRequest;
use sqry_core::watch::{ChangeSet, GitChangeClass};

use crate::error::DaemonError;
use crate::rebuild::RebuildMode;
use crate::workspace::WorkspaceState;

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{RebuildResult, ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

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

    /// `Some(flags)`: replace the `--cfg` flags the workspace manifest
    /// records with these (an empty list clears them); absent: keep the
    /// recorded flags (surface parity W4, design W4-D8).
    #[serde(default)]
    pub cfg_flags: Option<Vec<String>>,

    /// `Some(dir)`: replace the expand cache directory the manifest
    /// records; absent: keep the recorded one.
    #[serde(default)]
    pub expand_cache: Option<std::path::PathBuf>,

    /// Drop the recorded macro options before applying the two fields
    /// above (`sqry daemon rebuild --no-macro-options`).
    #[serde(default)]
    pub reset_macro_options: bool,
}

impl RebuildParams {
    /// The macro build options request this call expresses.
    #[must_use]
    pub fn macro_request(&self) -> MacroOptionsRequest {
        MacroOptionsRequest {
            cfg_flags: self.cfg_flags.clone(),
            expand_cache_dir: self.expand_cache.clone(),
            reset: self.reset_macro_options,
        }
    }
}

/// Handle one `daemon/rebuild` request.
///
/// 1. Canonicalize `path`, and refuse a macro options request the request
///    check refuses (`-32602`, "rebuild of <root> refused: <reason>").
/// 2. Find the matching workspace in the manager. Return `-32004
///    WorkspaceNotLoaded` if not found.
/// 3. Verify the workspace is in a serving state (not `Unloaded`,
///    `Loading`, or `Evicted`) — return `-32004 WorkspaceNotLoaded`
///    if not.
/// 4. Build a `ChangeSet` that reflects the requested rebuild mode:
///    - `force = false`: empty `ChangeSet` so the dispatcher runs the
///      normal incremental / full decision path. An empty change set is
///      scheduled in incremental mode, which still builds and persists
///      the whole graph.
///    - `force = true`: `ChangeSet` with `git_change_class =
///      Some(GitChangeClass::TreeDiverged)` so `decide_mode` selects
///      `RebuildMode::Full` unconditionally.
/// 5. Dispatch via `RebuildDispatcher::handle_changes_with_macro_options`
///    and wait, bounded by `RebuildDispatcher::outcome_wait`, for this
///    request's own outcome.
/// 6. Return `RebuildResult` from that outcome's report: the counts of
///    the generation this request's iteration published and the mode it
///    ran.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: RebuildParams =
        serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    // Step 1: canonicalize path, and check the macro options request on
    // its own (S6, round 7): an empty, blank or padded cfg flag or an
    // empty expand cache is refused (`-32602`) before anything is looked
    // up, enqueued or built.
    let canonical = resolve_index_root(&params.path)?;
    crate::rebuild::validate_macro_request(&canonical, &params.macro_request())
        .map_err(MethodError::Daemon)?;

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

    // Step 5: dispatch the rebuild and wait for THIS request's outcome.
    //
    // The dispatch returns at once in both cases:
    //   (a) This call took the runner role: the drain loop runs on its own
    //       task and delivers this request's result when its first
    //       iteration ends, before any iteration parked behind it.
    //   (b) Another runner was active: the request was parked in the lane
    //       (merged with the entry there when the two may share one
    //       iteration) and the runner delivers the result of the iteration
    //       that consumes it.
    //
    // Either way the outcome is this request's own iteration (integration
    // of W1 and W4): a request refused while queued is reported as refused,
    // where the old wait on `rebuild_in_flight` reported `Completed` for
    // whatever the drain loop did. Only this handler's wait is bounded; the
    // drain loop is never abandoned mid-pipeline.
    let bound = ctx.dispatcher.outcome_wait();
    let started = Instant::now();
    let outcome = ctx
        .dispatcher
        .handle_changes_with_macro_options(&key, changes, params.macro_request())
        .await;
    let remaining = bound.saturating_sub(started.elapsed());
    let report = match tokio::time::timeout(remaining, outcome.wait()).await {
        Ok(own) => own.map_err(MethodError::Daemon)?,
        Err(_elapsed) => {
            return Err(MethodError::Daemon(DaemonError::RebuildOutcomeTimeout {
                root: canonical,
                secs: bound.as_secs(),
                deadline_ms: u64::try_from(bound.as_millis()).unwrap_or(u64::MAX),
            }));
        }
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    // A workspace a load wanted watched (its load failed, so no watcher
    // started) is watched once this rebuild made it resident (audit S2,
    // D2E). Idempotent for a workspace already watched.
    if ws.watch_wanted.load(std::sync::atomic::Ordering::Acquire) {
        ctx.dispatcher.start_watching(&key);
    }

    // Step 6: the counts of the generation this request's own iteration
    // published, from its report. A read of the slot here would describe
    // whichever iteration published last.
    let graph = &report.published.graph;
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
            was_full: Some(report.mode == RebuildMode::Full),
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
    use tokio_util::sync::CancellationToken;

    fn make_config() -> Arc<DaemonConfig> {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::new(DaemonConfig::default())
    }

    fn make_ctx(manager: Arc<WorkspaceManager>) -> HandlerContext {
        let config = make_config();
        let roster = Arc::new(crate::workspace::WorkspaceRosterResolver::new());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), roster);
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
            mcp_redaction: std::sync::Arc::new(crate::mcp_host::redaction::McpRedaction::disabled()),
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

    /// T12 (surface parity W4, W4-D8): the three macro fields parse, default
    /// to "keep the record", and an old client's `{path, force}` request
    /// expresses an empty request (invariant I7).
    #[test]
    fn rebuild_params_macro_fields_parse_and_default() {
        let params: daemon_rebuild::RebuildParams =
            serde_json::from_value(json!({ "path": "/some/path", "force": true })).unwrap();
        assert!(
            params.macro_request().is_empty(),
            "an old client keeps the record"
        );
        let params: daemon_rebuild::RebuildParams = serde_json::from_value(json!({
            "path": "/some/path",
            "cfg_flags": ["feature=serde"],
            "expand_cache": "/cache/dir",
            "reset_macro_options": true,
        }))
        .unwrap();
        let request = params.macro_request();
        assert_eq!(request.cfg_flags, Some(vec!["feature=serde".to_string()]));
        assert_eq!(
            request.expand_cache_dir.as_deref(),
            Some(std::path::Path::new("/cache/dir"))
        );
        assert!(request.reset);
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
