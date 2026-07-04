//! `daemon/load` handler.
//!
//! Canonicalises `index_root`, constructs a
//! [`crate::workspace::WorkspaceKey`], and drives
//! [`crate::workspace::WorkspaceManager::get_or_load`] on the blocking
//! pool via [`tokio::task::spawn_blocking`].
//!
//! Panics inside the blocking closure are translated into
//! [`crate::error::DaemonError::WorkspaceBuildFailed`] (`-32001`) so
//! clients always see structured daemon errors, never a transport-
//! level `-32603`.
//!
//! # `STEP_6` (workspace-aware-cross-repo) augmentation
//!
//! `daemon/load` accepts an optional `logical_workspace` payload of
//! type [`sqry_daemon_protocol::LogicalWorkspaceWire`]. When present,
//! the daemon constructs **one** [`WorkspaceKey`] per declared source
//! root, all sharing the same `workspace_id`, and loads each in turn.
//! When absent, the legacy single-source-root path is preserved
//! (`workspace_id = None`, `source_root = canonical(params.index_root)`),
//! reproducing today's anonymous behaviour bit-for-bit. Concurrent
//! `daemon/load` calls with the same `workspace_id` are idempotent —
//! `get_or_load`'s lifecycle gate (the `Loading` CAS) ensures a single
//! winner per [`WorkspaceKey`] regardless of caller count, and the
//! manager map dedups equal keys.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use sqry_core::graph::unified::GraphMemorySize;
use sqry_core::project::ProjectRootMode;
use sqry_daemon_protocol::{LoadResult, LogicalWorkspaceWire};

use crate::config::{DaemonConfig, WORKING_SET_MULTIPLIER};
use crate::error::DaemonError;
use crate::workspace::{WorkspaceKey, WorkspaceState};

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{ConnectionState, HandlerContext, MethodError, format_panic_payload};

/// `daemon/load` params.
///
/// Pinning is a config concern — see
/// [`crate::config::WorkspaceConfig::pinned`]; clients cannot mark a
/// workspace as pinned at `daemon/load` time. To pin a workspace, add
/// it to `daemon.toml` and restart the daemon (or call a future
/// `daemon/pin` method when Task 9 exposes it).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadParams {
    pub index_root: PathBuf,
    #[serde(default)]
    pub root_mode: Option<ProjectRootMode>,
    #[serde(default)]
    pub config_fingerprint: Option<u64>,
    /// `STEP_6`: optional logical-workspace binding. When present, every
    /// source root in
    /// [`LogicalWorkspaceWire::source_roots`] is loaded under a
    /// [`WorkspaceKey`] sharing the wire's `workspace_id`. The
    /// pre-STEP_6 single-source-root call (no `logical_workspace`
    /// field) keeps the legacy `WorkspaceKey { workspace_id: None,
    /// source_root: canonical(params.index_root), .. }` shape — full
    /// backward compatibility with older clients.
    #[serde(default)]
    pub logical_workspace: Option<LogicalWorkspaceWire>,
}

/// Conservative initial working-set estimate for a cold `get_or_load`.
/// Calibration is deferred to Task 13 / 14 per the plan; this value is
/// the same 2 MiB floor the design document specifies.
const INITIAL_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024;

/// Multiply the initial working-set estimate by the conservative
/// factor used everywhere else in admission accounting (§G.6).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn working_set_estimate_for_initial(_cfg: &DaemonConfig) -> u64 {
    #[allow(clippy::cast_precision_loss)]
    let scaled = (INITIAL_WORKING_SET_BYTES as f64) * WORKING_SET_MULTIPLIER;
    scaled as u64
}

/// Handle one `daemon/load` request.
///
/// `STEP_6` iter-2 BLOCK fix: when `params.logical_workspace` is
/// `None`, fall back to the connection-level binding captured from
/// `DaemonHello.logical_workspace` (carried on `conn`). Per-request
/// params always win — the inheritance is a fallback, not a merge.
pub(crate) async fn handle(
    ctx: &HandlerContext,
    conn: &ConnectionState,
    params: Value,
) -> Result<Value, MethodError> {
    let params: LoadParams = serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    let canonical_root = resolve_index_root(&params.index_root)?;
    let root_mode = params.root_mode.unwrap_or_default();
    let config_fingerprint = params.config_fingerprint.unwrap_or(0);

    // Resolve the effective logical-workspace binding. Precedence:
    //   1. Per-request `params.logical_workspace`.
    //   2. Otherwise the connection-level binding inherited from
    //      `DaemonHello.logical_workspace`.
    //   3. Otherwise `None` — anonymous (pre-STEP_6) semantics.
    let effective_logical_workspace: Option<&LogicalWorkspaceWire> = params
        .logical_workspace
        .as_ref()
        .or(conn.connection_logical_workspace.as_ref());

    // STEP_11_4 — per-source-root effective fingerprint. Precedence:
    //   1. SourceRootBinding.config_fingerprint matched by path.
    //   2. workspace_config_fingerprint.
    //   3. params.config_fingerprint (legacy single-root parameter).
    //   4. 0.
    let effective_fingerprint_for = |path: &PathBuf| -> u64 {
        if let Some(lw) = effective_logical_workspace {
            for binding in &lw.source_root_bindings {
                if binding.path == *path && binding.config_fingerprint != 0 {
                    return binding.config_fingerprint;
                }
            }
            if lw.workspace_config_fingerprint != 0 {
                return lw.workspace_config_fingerprint;
            }
        }
        config_fingerprint
    };

    // STEP_6: compose the primary key. STEP_11_4: use per-root
    // effective fingerprint so two source roots in the same logical
    // workspace can carry distinct fingerprints.
    let primary_fp = effective_fingerprint_for(&canonical_root);
    let primary_key = match effective_logical_workspace {
        Some(lw) => WorkspaceKey::with_workspace_id(
            lw.workspace_id,
            canonical_root.clone(),
            root_mode,
            primary_fp,
        ),
        None => WorkspaceKey::new(canonical_root.clone(), root_mode, primary_fp),
    };

    // Build the full key set: the primary key plus every additional
    // source root the wire payload declared.
    let mut keys: Vec<WorkspaceKey> = vec![primary_key.clone()];
    if let Some(lw) = effective_logical_workspace {
        for sr in &lw.source_roots {
            let canon = resolve_index_root(sr)?;
            if canon == canonical_root {
                continue; // already covered by primary_key
            }
            let fp = effective_fingerprint_for(&canon);
            keys.push(WorkspaceKey::with_workspace_id(
                lw.workspace_id,
                canon,
                root_mode,
                fp,
            ));
        }
    }

    // Drive get_or_load for every key. The first failure short-circuits
    // and surfaces upward. Successful preceding loads stay in the
    // manager — that is the correct semantics for a partial logical
    // workspace, and matches the design's "partial eviction" symmetry
    // (the daemon does not silently roll back already-loaded source
    // roots when a sibling fails to build).
    let mut primary_graph_arc: Option<Arc<sqry_core::graph::CodeGraph>> = None;
    for key in &keys {
        let manager = Arc::clone(&ctx.manager);
        let builder = Arc::clone(&ctx.workspace_builder);
        let estimate = working_set_estimate_for_initial(&ctx.config);
        let key_for_task = key.clone();
        let key_for_diag = key.clone();

        let graph = match tokio::task::spawn_blocking(move || {
            manager.get_or_load(&key_for_task, &*builder, estimate)
        })
        .await
        {
            Ok(Ok(graph)) => graph,
            Ok(Err(daemon_err)) => return Err(MethodError::Daemon(daemon_err)),
            Err(join_err) if join_err.is_panic() => {
                let reason = format_panic_payload(join_err);
                return Err(MethodError::Daemon(DaemonError::WorkspaceBuildFailed {
                    root: key_for_diag.source_root.clone(),
                    reason: format!("workspace builder panicked: {reason}"),
                }));
            }
            Err(join_err) => return Err(MethodError::JoinError(join_err)),
        };

        if *key == primary_key {
            primary_graph_arc = Some(graph);
        }

        // Start the file watcher for this source root so subsequent
        // edits trigger a debounced incremental rebuild. Best-effort:
        // `start_watching` logs (WARN) and never fails the load if the
        // watcher cannot be created (for example a non-git workspace).
        // Idempotent, so a repeated `daemon/load` for an already-watched
        // key is a no-op. This is the production wiring the
        // `ensure_watching` placement comment defers to
        // (verivus-oss/sqry#461).
        ctx.dispatcher.start_watching(key);
    }

    let primary_graph = primary_graph_arc
        .expect("primary_key is always present in `keys` so get_or_load runs at least once for it");
    let current_bytes = primary_graph.heap_bytes() as u64;
    let envelope = ResponseEnvelope {
        result: LoadResult {
            root: primary_key.source_root.clone(),
            current_bytes,
            state: WorkspaceState::Loaded,
        },
        meta: ResponseMeta::loaded(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/load: {e}")))
}
