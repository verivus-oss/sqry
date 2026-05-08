//! Shared tool-dispatch core used by both the JSON-RPC path
//! (`ipc::methods::tool_dispatch::classify_and_build`) and the
//! MCP host path (`mcp_host::DaemonMcpHandler::call_tool`, U8).
//!
//! Introduced in Phase 8c to close Codex iter-1 B4 (CPU-heavy tool
//! work on tokio workers) and iter-1 M2 (avoid two parallel 14-arm
//! dispatchers that could drift). The classify/execute/stale-warning
//! logic lives here; the two transports wrap its `ExecuteVerdict`
//! return into their respective envelope formats.
//!
//! # Concurrency model
//!
//! `classify_and_execute` runs the user-supplied `run` closure inside
//! [`tokio::task::spawn_blocking`] so CPU-heavy graph traversal never
//! ties up a tokio worker. It then wraps the resulting
//! [`tokio::task::JoinHandle`] in [`tokio::time::timeout`] with the
//! caller-supplied per-tool deadline. When the outer timeout fires the
//! `JoinHandle` is dropped — the OS thread continues executing the
//! closure until the closure itself returns, but its result is
//! discarded and [`DaemonError::ToolTimeout`] is returned on the wire.
//! This bounds RESPONSE LATENCY, not the lifetime of a runaway tool
//! closure (see `DaemonConfig::tool_timeout_secs` docs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Test instrumentation for `execute_with_timeout`.
///
/// Provides a workspace-path-keyed notification mechanism so integration
/// tests can prove the real daemon `spawn_blocking(dispatch_by_name)` OS
/// thread actually started before they fire `server.shutdown.cancel()`.
///
/// # Design (iter-8 redesign)
///
/// Earlier iterations (iter-3 through iter-7) used a single global notifier
/// slot keyed by a monotonically increasing `u64` token. Codex iter-3 through
/// iter-6 reviews surfaced a sequence of races (cross-test wipe, snapshot
/// vs register reordering, etc.) that we tried to patch with token-aware
/// `clear`/`notify` and a `tokio::sync::Mutex` serializer (iter-7). None of
/// those patches address the root cause: **the daemon's `notify(token)` call
/// runs for EVERY tool dispatch — including tool calls from OTHER concurrent
/// tests in the same binary**. With a single-slot global notifier, any other
/// test calling `call_tool` while our test holds a registration would fire
/// our flag spuriously (the daemon's `notify` reads our token from the slot
/// and matches it, even though the dispatch belongs to a different test's
/// call).
///
/// The iter-8 fix binds ownership at `register()` time using the **canonical
/// workspace path** as the key:
///
/// 1. The test creates a unique tempdir → unique canonicalised path.
/// 2. The test calls `register(workspace_path, flag)` to add an entry to
///    a `Vec<(PathBuf, Arc<AtomicBool>)>` registry.
/// 3. The daemon's `execute_with_timeout` calls `notify(&canonical_root)`
///    inside the `spawn_blocking` closure with the dispatched workspace's
///    path. `notify` fires the flag of the entry whose path equals
///    `canonical_root` — no other test's registration is touched.
/// 4. The test calls `clear(workspace_path)` at teardown to remove its own
///    entry by path.
///
/// Because every test creates its own tempdir, paths are guaranteed unique.
/// Concurrent tests cannot collide because the registry is a `Vec` (multiple
/// simultaneous registrations are allowed) and the lookup key (path) is
/// per-test-private.
///
/// # Why this is simpler than the iter-7 design
///
/// - No tokens, no `SEQ` counter, no `snapshot_token` step.
/// - No `HOOK_SERIALIZER` — multiple tests can register concurrently.
/// - The registry is a plain `std::sync::Mutex<Vec<...>>` with negligible
///   contention (each tool dispatch takes the lock for one linear scan).
/// - Cannot suffer the "test B's call fires test A's flag" cross-test
///   contamination that iter-3 through iter-7 all retained, because the
///   `notify` key (path) is the dispatched workspace's root, not a global
///   slot value.
///
/// This module is intentionally always compiled (not gated by `#[cfg(test)]`)
/// because the library crate is compiled once — without `cfg(test)` — even
/// when running integration tests, so a `#[cfg(test)]` guard on library code
/// is not visible to integration test binaries. The runtime cost in
/// production is one `Mutex<Vec<_>>::lock()` per tool dispatch over an
/// always-empty vector — a single uncontended atomic CAS.
///
/// # Usage (integration tests)
///
/// ```ignore
/// use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
/// use sqry_daemon::ipc::tool_core::thread_start_hook;
///
/// let canon = canonicalize_path(&dir.path()).unwrap();
/// let started = Arc::new(AtomicBool::new(false));
/// thread_start_hook::register(canon.clone(), Arc::clone(&started));
/// // ... submit call_tool against `canon`, poll started, fire shutdown ...
/// thread_start_hook::clear(&canon);
/// ```
#[doc(hidden)]
pub mod thread_start_hook {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Path-keyed registry of test notifiers. Each entry is a
    /// `(canonical workspace path, notifier flag)` pair. The daemon's
    /// `notify(path)` fires the flag of the entry whose path equals
    /// `path` — `None` of the others is touched. Multiple concurrent
    /// registrations are allowed; tests are isolated by their unique
    /// tempdir paths.
    static REGISTRY: Mutex<Vec<(PathBuf, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

    /// Register a per-test notifier keyed by the canonical workspace
    /// path that the test will dispatch tool calls against.
    ///
    /// The path MUST be canonicalised (via
    /// `sqry_core::project::canonicalize_path`) so it matches the
    /// `canonical_root` value the daemon passes to [`notify`] inside
    /// `execute_with_timeout`. Tests typically use `tempfile::tempdir()`
    /// + `canonicalize_path(dir.path())` which guarantees uniqueness.
    ///
    /// If a previous registration for the same path is still present
    /// (e.g. a test forgot to call [`clear`]), it is replaced. This is
    /// a no-op for the common case because tempdir paths never repeat
    /// within a single test-binary process lifetime.
    pub fn register(workspace_path: PathBuf, flag: Arc<AtomicBool>) {
        let mut guard = REGISTRY
            .lock()
            .expect("thread_start_hook REGISTRY poisoned");
        // Replace any stale entry for this path; otherwise push a new one.
        if let Some(slot) = guard.iter_mut().find(|(p, _)| *p == workspace_path) {
            slot.1 = flag;
        } else {
            guard.push((workspace_path, flag));
        }
    }

    /// Remove the registration for `workspace_path`, if present.
    /// No-op when the path is not registered (e.g. the daemon's
    /// [`notify`] already fired it and we left it in place — note
    /// `notify` does NOT remove entries; teardown is the test's
    /// responsibility).
    pub fn clear(workspace_path: &Path) {
        let mut guard = REGISTRY
            .lock()
            .expect("thread_start_hook REGISTRY poisoned");
        guard.retain(|(p, _)| p != workspace_path);
    }

    /// Called from inside the `spawn_blocking` closure as its first
    /// action. Fires the registered flag for `workspace_path` if one
    /// exists; otherwise a no-op.
    ///
    /// Cross-test isolation is structural: `workspace_path` is the
    /// dispatched workspace's canonical root, so concurrent tests with
    /// different tempdir paths cannot fire each other's flags.
    pub(super) fn notify(workspace_path: &Path) {
        let guard = REGISTRY
            .lock()
            .expect("thread_start_hook REGISTRY poisoned");
        if let Some((_, flag)) = guard.iter().find(|(p, _)| p == workspace_path) {
            flag.store(true, Ordering::Release);
        }
    }
}

use serde_json::Value;
use sqry_core::graph::acquisition::{
    AcquisitionOperation, GraphAcquirer, GraphAcquisition, GraphAcquisitionRequest, GraphFreshness,
    MissingGraphPolicy, PathPolicy, PluginSelectionPolicy, StalePolicy,
};
use sqry_core::project::{ProjectRootMode, absolutize_without_resolution, canonicalize_path};
use sqry_core::query::executor::QueryExecutor;
use sqry_mcp::daemon_adapter::WorkspaceContext;

use crate::error::DaemonError;
use crate::workspace::{
    ServeVerdict, WorkspaceBuilder, WorkspaceKey, WorkspaceManager, acquirer::DaemonGraphProvider,
};

/// Outcome of [`classify_and_execute`]. Callers wrap this in their
/// transport-specific envelope (JSON-RPC `ResponseEnvelope` or MCP
/// `CallToolResult`) and, for stale verdicts, splice the
/// `stale_warning` string into the inner payload.
#[derive(Debug)]
pub(crate) enum ExecuteVerdict {
    /// Tool ran against a Fresh workspace (Loaded or Rebuilding state).
    Fresh {
        inner: Value,
        state: crate::workspace::WorkspaceState,
    },
    /// Tool ran against a Stale workspace. Callers MUST splice
    /// `stale_warning` into the response payload.
    Stale {
        inner: Value,
        stale_warning: String,
        last_good_at: SystemTime,
        last_error: Option<String>,
    },
}

/// Canonicalise a user-supplied `index_root` path, returning
/// [`DaemonError::InvalidArgument`] on any failure. This is the
/// transport-neutral twin of [`crate::ipc::path_policy::resolve_index_root`]
/// — the JSON-RPC path still goes through `path_policy` because its
/// return type is [`crate::ipc::methods::MethodError::InvalidParams`],
/// but the shared `tool_core` pipeline needs a typed
/// [`DaemonError::InvalidArgument`] so the MCP host (U8) can map the
/// same precondition failure into a `-32602`/`validation_error` MCP
/// envelope without going through `MethodError`.
/// SGA04 building block — public-to-the-crate alias for [`resolve_path`]
/// so the daemon graph provider (`workspace::acquirer`) can perform
/// path-policy validation through the SAME canonicaliser the existing
/// dispatch path uses, without duplicating the absolutise / metadata /
/// canonicalize ladder.
///
/// Path canonicalisation must run before any workspace classification
/// — see SGA02's `InvalidPath` precedence contract. Returning a
/// [`DaemonError::InvalidArgument`] keeps the error taxonomy local;
/// the acquirer maps it into [`sqry_core::graph::acquisition::GraphAcquisitionError::InvalidPath`].
///
/// SGA05 will route read-only tool dispatch through the provider; the
/// existing [`classify_and_execute`] entrypoint stays unchanged in this
/// DAG unit.
pub(crate) fn resolve_path_for_acquisition(raw: &Path) -> Result<PathBuf, DaemonError> {
    resolve_path(raw)
}

/// SGA04 building block — construct a daemon-side
/// [`DaemonGraphProvider`] for the supplied manager + builder pair.
///
/// SGA05 routes the JSON-RPC `tool_dispatch::classify_and_build`
/// closure path and the daemon MCP host's `call_tool` graph-backed
/// arms through [`acquire_and_execute`], which builds a provider
/// per-request via this helper.
pub(crate) fn daemon_graph_provider(
    manager: Arc<WorkspaceManager>,
    builder: Arc<dyn WorkspaceBuilder>,
) -> DaemonGraphProvider {
    DaemonGraphProvider::new(manager, builder)
}

fn resolve_path(raw: &Path) -> Result<PathBuf, DaemonError> {
    let absolutised =
        absolutize_without_resolution(raw).map_err(|e| DaemonError::InvalidArgument {
            reason: format!("path_policy: index_root absolutise: {e}"),
        })?;
    match std::fs::metadata(&absolutised) {
        Ok(meta) if meta.is_dir() => {
            canonicalize_path(&absolutised).map_err(|e| DaemonError::InvalidArgument {
                reason: format!("path_policy: index_root canonicalize: {e}"),
            })
        }
        Ok(_) => Err(DaemonError::InvalidArgument {
            reason: "path_policy: index_root exists but is not a directory".to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(DaemonError::InvalidArgument {
            reason: "path_policy: index_root does not exist; daemon/load requires \
                         an existing directory so a canonical WorkspaceKey can be computed"
                .to_string(),
        }),
        Err(e) => Err(DaemonError::InvalidArgument {
            reason: format!("path_policy: index_root stat: {e}"),
        }),
    }
}

/// SGA05 — shared acquire + execute + stale-warning pipeline backed
/// by the [`DaemonGraphProvider`].
///
/// Every daemon-hosted read-only tool (the 14 graph-backed tools in
/// [`sqry_mcp::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES`] minus the
/// mutating `rebuild_index` and the translation-wrapper `sqry_ask`)
/// routes through this entrypoint, which:
///
/// 1. Builds a per-request [`DaemonGraphProvider`] over the supplied
///    `manager` + `builder` pair (`tool_name` is forwarded into the
///    acquisition metadata for diagnostics).
/// 2. Calls
///    [`GraphAcquirer::acquire`](sqry_core::graph::acquisition::GraphAcquirer::acquire)
///    with [`AcquisitionOperation::ReadOnlyQuery`]. The provider
///    canonicalises the path, classifies the workspace, and — on
///    eviction — performs the bounded one-shot read-only persisted
///    rehydrate (per SGA02 §Tool Ownership Boundary and SGA04 contract
///    guarantees).
/// 3. Maps the resulting [`GraphAcquisition`] / [`GraphFreshness`]
///    into the existing [`ExecuteVerdict`]:
///    - `Fresh` and `Reloaded` both surface as
///      [`ExecuteVerdict::Fresh`] so the wire envelope stays
///      byte-compatible (per SGA design §Staleness and Wire
///      Compatibility — reload metadata is internal-only).
///    - `Stale` surfaces as [`ExecuteVerdict::Stale`] with the
///      existing `_stale_warning` rendering preserved.
/// 4. Maps any [`GraphAcquisitionError`] through the
///    [`From<GraphAcquisitionError> for DaemonError`] impl so
///    `WorkspaceEvicted`, `WorkspaceIncompatibleGraph`, `Stale`
///    expiry, and `InvalidArgument` precedence all preserve their
///    existing JSON-RPC / MCP envelope shapes.
/// 5. Runs the user-supplied closure inside
///    [`tokio::task::spawn_blocking`] with the same
///    [`tokio::time::timeout`] outer bound used by
///    [`classify_and_execute`] — CPU-heavy graph traversal does not
///    tie up tokio workers.
///
/// `rebuild_index` MUST NOT call this helper; the mutating rebuild
/// path drives [`WorkspaceManager::get_or_load`] directly so the
/// durable rebuild contract owns those semantics. See
/// `sqry-daemon/src/mcp_host/mod.rs::handle_rebuild_index` and the
/// `MutatingRebuild` short-circuit inside the
/// [`DaemonGraphProvider::acquire`] implementation.
pub(crate) async fn acquire_and_execute<F>(
    manager: Arc<WorkspaceManager>,
    builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    tool_timeout: Duration,
    path: &str,
    tool_name: Option<&'static str>,
    run: F,
) -> Result<ExecuteVerdict, DaemonError>
where
    F: FnOnce(&WorkspaceContext) -> anyhow::Result<Value> + Send + 'static,
{
    // Build a per-request provider (cheap — three Arc clones plus an
    // Option tag) and acquire the graph through the shared boundary.
    let mut provider = DaemonGraphProvider::new(manager, builder);
    if let Some(name) = tool_name {
        provider = provider.with_tool_name(name);
    }
    let request = GraphAcquisitionRequest {
        requested_path: PathBuf::from(path),
        operation: AcquisitionOperation::ReadOnlyQuery,
        // Daemon read-only paths use the in-tree default policies. The
        // provider already canonicalises through the daemon's
        // path-policy ladder (`tool_core::resolve_path`); the
        // `PathPolicy` field on the request is held for symmetry with
        // the filesystem provider.
        path_policy: PathPolicy::default(),
        // The daemon never auto-builds on miss for read-only queries —
        // the daemon's own admission and dispatch flow owns initial
        // graph load via `daemon/load`. `Error` here matches the
        // pre-SGA05 semantics: a workspace that has never been loaded
        // returns a `NotReady` / `WorkspaceBuildFailed` envelope.
        missing_graph_policy: MissingGraphPolicy::Error,
        stale_policy: StalePolicy::default(),
        plugin_selection_policy: PluginSelectionPolicy::default(),
        tool_name,
    };
    let acquisition: GraphAcquisition = provider.acquire(request).map_err(DaemonError::from)?;

    let canonical_root = acquisition.workspace_root.clone();
    let graph = Arc::clone(&acquisition.graph);
    let freshness = acquisition.freshness;

    let wctx = WorkspaceContext {
        workspace_root: canonical_root.clone(),
        graph,
        executor: tool_executor,
    };
    let inner = execute_with_timeout(tool_timeout, &canonical_root, wctx, run).await?;

    match freshness {
        // Fresh and Reloaded both produce the existing fresh response
        // envelope — reload is an internal recovery, not a wire-shape
        // change. The lifecycle label flows from the Fresh path; for
        // Reloaded acquisitions we map to the canonical Loaded state
        // because the bounded reload restores a Loaded workspace.
        GraphFreshness::Fresh { lifecycle_label } => {
            // Decode the daemon provider's lifecycle label back into
            // the wire-visible `WorkspaceState` so the
            // `ResponseMeta::fresh_from(state, ...)` envelope
            // accurately reports `Loaded` vs. `Rebuilding` (the only
            // two states the underlying `classify_for_serve` Fresh
            // arm can produce).
            let state = match lifecycle_label {
                Some("rebuilding") => crate::workspace::WorkspaceState::Rebuilding,
                _ => crate::workspace::WorkspaceState::Loaded,
            };
            Ok(ExecuteVerdict::Fresh { inner, state })
        }
        GraphFreshness::Reloaded { .. } => Ok(ExecuteVerdict::Fresh {
            inner,
            state: crate::workspace::WorkspaceState::Loaded,
        }),
        GraphFreshness::Stale {
            last_good_at,
            last_error,
            age_hours,
        } => {
            // Reconstruct the `last_good_at: SystemTime` and
            // `age_hours: u64` shape the wire envelope expects.
            // `GraphFreshness::Stale` carries the RFC3339 string for
            // transport neutrality; round-trip through chrono so the
            // existing `render_stale_warning` rendering produces the
            // same RFC3339 bytes.
            let parsed_last_good = last_good_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| SystemTime::from(dt.with_timezone(&chrono::Utc)));
            let lg_at = parsed_last_good.unwrap_or_else(SystemTime::now);
            let age_u64 = age_hours.map(|h| h as u64).unwrap_or(0);
            let stale_warning =
                render_stale_warning(&canonical_root, age_u64, lg_at, last_error.as_deref());
            Ok(ExecuteVerdict::Stale {
                inner,
                stale_warning,
                last_good_at: lg_at,
                last_error,
            })
        }
    }
}

/// SGA05 legacy — kept exclusively for the in-crate unit tests in
/// the `tests` module below, which assert direct
/// `classify_for_serve` semantics (NotReady, Loading, ToolTimeout,
/// Internal-from-closure-error) that are easier to express against
/// `WorkspaceManager` directly than against
/// [`acquire_and_execute`] (which also runs path canonicalisation +
/// reload accounting). Production read-only tool dispatch now
/// routes through [`acquire_and_execute`].
///
/// Pipeline:
/// 1. Canonicalises `path` via [`resolve_path`] into a
///    [`DaemonError::InvalidArgument`] on failure.
/// 2. Classifies the workspace via
///    [`WorkspaceManager::classify_for_serve`].
/// 3. On Fresh/Stale: builds a [`WorkspaceContext`] and runs `run`
///    inside [`tokio::task::spawn_blocking`] with a
///    [`tokio::time::timeout(tool_timeout, ...)`] outer bound.
/// 4. On NotReady: returns [`DaemonError::WorkspaceBuildFailed`].
/// 5. On outer timeout: drops the [`tokio::task::JoinHandle`] and
///    returns [`DaemonError::ToolTimeout`] (OS thread continues;
///    result discarded).
#[allow(dead_code)] // Used by in-crate `mod tests`.
///
/// # Errors
///
/// - [`DaemonError::InvalidArgument`] — path canonicalisation failed.
/// - [`DaemonError::WorkspaceBuildFailed`] — NotReady verdict.
/// - [`DaemonError::WorkspaceStaleExpired`] — Stale expired past cap.
/// - [`DaemonError::WorkspaceEvicted`] — workspace evicted between
///   classify and graph capture.
/// - [`DaemonError::ToolTimeout`] — outer timeout fired.
/// - [`DaemonError::Internal`] — `run` returned `anyhow::Error` or
///   [`tokio::task::spawn_blocking`] join failed.
///
/// # Design
///
/// Per Codex iter-3 NIT-1, `daemon_version` is NOT a parameter here
/// — callers pass it to their respective envelope builders (Phase 8b
/// [`crate::ipc::protocol::ResponseMeta::fresh_from`] /
/// [`crate::ipc::protocol::ResponseMeta::stale_from`], MCP `rmcp`
/// envelope).
pub(crate) async fn classify_and_execute<F>(
    manager: Arc<WorkspaceManager>,
    tool_executor: Arc<QueryExecutor>,
    tool_timeout: Duration,
    path: &str,
    run: F,
) -> Result<ExecuteVerdict, DaemonError>
where
    F: FnOnce(&WorkspaceContext) -> anyhow::Result<Value> + Send + 'static,
{
    // Step 1: canonicalise path.
    let canonical_root = resolve_path(Path::new(path))?;
    let key = WorkspaceKey::new(canonical_root.clone(), ProjectRootMode::GitRoot, 0);

    // Step 2: classify.
    let verdict = manager.classify_for_serve(&key, SystemTime::now())?;

    match verdict {
        ServeVerdict::Fresh { graph, state } => {
            let wctx = WorkspaceContext {
                workspace_root: canonical_root.clone(),
                graph,
                executor: tool_executor,
            };
            let inner = execute_with_timeout(tool_timeout, &canonical_root, wctx, run).await?;
            Ok(ExecuteVerdict::Fresh { inner, state })
        }
        ServeVerdict::Stale {
            graph,
            age_hours,
            last_good_at,
            last_error,
        } => {
            let wctx = WorkspaceContext {
                workspace_root: canonical_root.clone(),
                graph,
                executor: tool_executor,
            };
            let inner = execute_with_timeout(tool_timeout, &canonical_root, wctx, run).await?;
            let stale_warning = render_stale_warning(
                &canonical_root,
                age_hours,
                last_good_at,
                last_error.as_deref(),
            );
            Ok(ExecuteVerdict::Stale {
                inner,
                stale_warning,
                last_good_at,
                last_error,
            })
        }
        ServeVerdict::NotReady { state } => Err(DaemonError::WorkspaceBuildFailed {
            root: canonical_root,
            reason: format!("workspace not ready ({state:?}); call daemon/load first"),
        }),
    }
}

/// Run `run` inside `spawn_blocking` with an outer timeout.
///
/// Extracted helper so both the Fresh and Stale arms of
/// [`classify_and_execute`] share identical timeout semantics. On
/// timeout the detached [`tokio::task::JoinHandle`] is dropped (the OS
/// thread continues but the result is discarded); on join failure the
/// error is wrapped in [`DaemonError::Internal`].
async fn execute_with_timeout<F>(
    tool_timeout: Duration,
    canonical_root: &Path,
    wctx: WorkspaceContext,
    run: F,
) -> Result<Value, DaemonError>
where
    F: FnOnce(&WorkspaceContext) -> anyhow::Result<Value> + Send + 'static,
{
    // Derive the canonical wire fields BEFORE spawning so the borrow
    // of `canonical_root` does not cross the `.await`.
    let deadline_ms = u64::try_from(tool_timeout.as_millis()).unwrap_or(u64::MAX);
    let secs = tool_timeout.as_secs();
    let root_owned = canonical_root.to_path_buf();

    // Capture the canonical workspace path so the spawn_blocking closure
    // can call `thread_start_hook::notify(&path)` as its first action.
    // This is the path-keyed test instrumentation hook (iter-8 redesign):
    // a registered test notifier is fired only when its workspace path
    // matches `canonical_root`. Tests use unique tempdirs, so concurrent
    // tests cannot fire each other's flags. The hook is a no-op for the
    // common case of no test registration (one uncontended Mutex<Vec>
    // lock + linear scan over an always-empty vector).
    let hook_path = canonical_root.to_path_buf();

    let join_handle = tokio::task::spawn_blocking(move || {
        // Signal that the real OS thread has started by firing the
        // registered notifier (if any) for the dispatched workspace path.
        // This is the FIRST action inside the closure so the test's
        // server-side barrier resolves before any graph work begins,
        // proving the real daemon dispatch path reached `spawn_blocking`
        // and the OS scheduler dispatched it.
        thread_start_hook::notify(&hook_path);
        run(&wctx)
    });
    match tokio::time::timeout(tool_timeout, join_handle).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(err))) => Err(DaemonError::Internal(err)),
        Ok(Err(join_err)) => Err(DaemonError::Internal(anyhow::anyhow!(
            "spawn_blocking join: {join_err}"
        ))),
        Err(_elapsed) => Err(DaemonError::ToolTimeout {
            root: root_owned,
            secs,
            deadline_ms,
        }),
    }
}

/// Render the `_stale_warning` string spliced into a Stale verdict
/// response by the calling transport layer. Moved to this module so
/// both the JSON-RPC path and the MCP host share the same format.
///
/// With `last_error`:
/// ```text
/// workspace {root} served from last-good build at {rfc3339} ({age_hours}h stale); last error: {reason}
/// ```
/// Without `last_error`:
/// ```text
/// workspace {root} served from last-good build at {rfc3339} ({age_hours}h stale)
/// ```
///
/// The `; last error:` clause is omitted entirely when no diagnostic
/// is available (Phase 8b iter-1 n2 fix: don't emit a trailing
/// `: None`-style marker, keep the wire message self-describing).
pub(crate) fn render_stale_warning(
    root: &Path,
    age_hours: u64,
    last_good_at: SystemTime,
    last_error: Option<&str>,
) -> String {
    use chrono::{DateTime, SecondsFormat, Utc};
    let rfc3339 = DateTime::<Utc>::from(last_good_at).to_rfc3339_opts(SecondsFormat::Secs, true);
    match last_error {
        Some(reason) => format!(
            "workspace {} served from last-good build at {rfc3339} ({age_hours}h stale); last error: {reason}",
            root.display()
        ),
        None => format!(
            "workspace {} served from last-good build at {rfc3339} ({age_hours}h stale)",
            root.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use super::{classify_and_execute, render_stale_warning};
    use crate::config::DaemonConfig;
    use crate::error::DaemonError;
    use crate::workspace::WorkspaceManager;
    use sqry_core::query::executor::QueryExecutor;

    // ----- render_stale_warning (pure) ---------------------------------

    #[test]
    fn render_stale_warning_with_last_error() {
        let root = std::path::Path::new("/tmp/ws");
        // 2025-10-09T09:33:20Z — arbitrary past instant.
        let last_good = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let got = render_stale_warning(root, 48, last_good, Some("parse error"));
        assert!(got.contains("/tmp/ws"));
        assert!(got.contains("48h stale"));
        assert!(got.contains("; last error: parse error"));
        // RFC3339 UTC-Zulu sentinel — `to_rfc3339_opts(Secs, true)` always
        // emits a trailing `Z` rather than `+00:00`.
        assert!(got.contains('Z'), "expected RFC3339 UTC-Zulu form: {got}");
    }

    #[test]
    fn render_stale_warning_without_last_error_omits_clause() {
        let root = std::path::Path::new("/tmp/ws");
        let last_good = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let got = render_stale_warning(root, 48, last_good, None);
        assert!(got.contains("48h stale"));
        assert!(
            !got.contains("last error"),
            "None last_error must omit the clause entirely, got: {got}"
        );
    }

    // ----- classify_and_execute error-path tests -----------------------
    //
    // These tests do not require a live WorkspaceManager / QueryExecutor
    // with real graph state — the assertions exercise:
    //  * `InvalidArgument` when `path` canonicalisation fails (never
    //    reaches the manager)
    //  * `ToolTimeout` when the `run` closure sleeps past the deadline
    //    (requires a Loaded workspace — we use
    //    `insert_workspace_in_state_for_test` so we do not have to drag
    //    in a `WorkspaceBuilder` from the test crate)
    //  * `Internal` when `run` returns `anyhow::Err`
    //  * `WorkspaceBuildFailed` for a NotReady verdict (workspace in
    //    `Loading` state, never actually loaded)

    fn test_manager() -> Arc<WorkspaceManager> {
        let config = Arc::new(DaemonConfig::default());
        WorkspaceManager::new_without_reaper(config)
    }

    fn test_executor() -> Arc<QueryExecutor> {
        // `PluginManager` not required for these error-path assertions;
        // the closure never reaches the planner.
        Arc::new(QueryExecutor::new())
    }

    #[tokio::test]
    async fn classify_and_execute_invalid_path_returns_invalid_argument() {
        let manager = test_manager();
        let executor = test_executor();

        let run = |_wctx: &sqry_mcp::daemon_adapter::WorkspaceContext| -> anyhow::Result<Value> {
            Ok(Value::Null)
        };
        let err = classify_and_execute(
            manager,
            executor,
            Duration::from_secs(10),
            "/this/path/does/not/exist/for/real",
            run,
        )
        .await
        .expect_err("non-existent path must fail");

        match err {
            DaemonError::InvalidArgument { reason } => {
                assert!(
                    reason.contains("path_policy"),
                    "expected 'path_policy' prefix, got: {reason}"
                );
            }
            other => panic!("expected InvalidArgument, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_and_execute_notready_returns_workspace_build_failed() {
        // Insert a workspace in `Loading` state → classify_for_serve
        // returns NotReady → classify_and_execute maps to
        // `DaemonError::WorkspaceBuildFailed`.
        use sqry_core::project::{ProjectRootMode, canonicalize_path};

        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = test_manager();
        let executor = test_executor();

        let key = crate::workspace::WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, crate::workspace::WorkspaceState::Loading);

        let run = |_wctx: &sqry_mcp::daemon_adapter::WorkspaceContext| -> anyhow::Result<Value> {
            Ok(Value::Null)
        };
        let err = classify_and_execute(
            manager,
            executor,
            Duration::from_secs(10),
            root.to_str().unwrap(),
            run,
        )
        .await
        .expect_err("NotReady verdict must fail");

        match err {
            DaemonError::WorkspaceBuildFailed {
                root: got_root,
                reason,
            } => {
                assert_eq!(got_root, root);
                assert!(
                    reason.contains("workspace not ready"),
                    "expected 'workspace not ready' prefix, got: {reason}"
                );
                assert!(
                    reason.contains("Loading"),
                    "expected state Debug in message, got: {reason}"
                );
            }
            other => panic!("expected WorkspaceBuildFailed, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn classify_and_execute_timeout_returns_tool_timeout() {
        // Insert a workspace in `Loaded` state → Fresh verdict →
        // `run` sleeps 500ms with tool_timeout=50ms → ToolTimeout
        // fires. Multi-thread flavor needed because spawn_blocking
        // parks a worker thread.
        use sqry_core::project::{ProjectRootMode, canonicalize_path};

        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = test_manager();
        let executor = test_executor();

        let key = crate::workspace::WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, crate::workspace::WorkspaceState::Loaded);

        let run = |_wctx: &sqry_mcp::daemon_adapter::WorkspaceContext| -> anyhow::Result<Value> {
            std::thread::sleep(Duration::from_millis(500));
            Ok(Value::Null)
        };
        let err = classify_and_execute(
            manager,
            executor,
            Duration::from_millis(50),
            root.to_str().unwrap(),
            run,
        )
        .await
        .expect_err("timeout must fire");

        match err {
            DaemonError::ToolTimeout {
                root: got_root,
                secs,
                deadline_ms,
            } => {
                assert_eq!(got_root, root);
                // 50ms rounds down to 0s for the secs field; the
                // deadline_ms field captures the real wire value.
                assert_eq!(secs, 0, "50ms rounds down to 0 whole seconds");
                assert_eq!(deadline_ms, 50);
            }
            other => panic!("expected ToolTimeout, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn classify_and_execute_internal_error_on_run_failure() {
        use sqry_core::project::{ProjectRootMode, canonicalize_path};

        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = test_manager();
        let executor = test_executor();

        let key = crate::workspace::WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, crate::workspace::WorkspaceState::Loaded);

        let run = |_wctx: &sqry_mcp::daemon_adapter::WorkspaceContext| -> anyhow::Result<Value> {
            Err(anyhow::anyhow!("synthetic closure failure"))
        };
        let err = classify_and_execute(
            manager,
            executor,
            Duration::from_secs(10),
            root.to_str().unwrap(),
            run,
        )
        .await
        .expect_err("closure failure must surface");

        match err {
            DaemonError::Internal(inner) => {
                assert!(
                    inner.to_string().contains("synthetic closure failure"),
                    "expected closure error to survive, got: {inner}"
                );
            }
            other => panic!("expected Internal, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn classify_and_execute_fresh_path_returns_inner_value() {
        // Positive-path smoke: Loaded workspace + happy closure yields
        // `ExecuteVerdict::Fresh { inner, state: Loaded }`.
        use sqry_core::project::{ProjectRootMode, canonicalize_path};

        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = test_manager();
        let executor = test_executor();

        let key = crate::workspace::WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, crate::workspace::WorkspaceState::Loaded);

        let run = |_wctx: &sqry_mcp::daemon_adapter::WorkspaceContext| -> anyhow::Result<Value> {
            Ok(serde_json::json!({"hello": "world"}))
        };
        let verdict = classify_and_execute(
            manager,
            executor,
            Duration::from_secs(10),
            root.to_str().unwrap(),
            run,
        )
        .await
        .expect("fresh path must succeed");

        match verdict {
            super::ExecuteVerdict::Fresh { inner, state } => {
                assert_eq!(inner, serde_json::json!({"hello": "world"}));
                assert_eq!(state, crate::workspace::WorkspaceState::Loaded);
            }
            other => panic!("expected Fresh, got: {other:?}"),
        }
    }

    // Sanity: the documented SystemTime round-trip used by
    // `render_stale_warning` is not platform-dependent.
    #[test]
    fn render_stale_warning_epoch_is_well_formed() {
        let got =
            render_stale_warning(std::path::Path::new("/ws"), 0, SystemTime::UNIX_EPOCH, None);
        assert!(got.contains("1970-01-01T00:00:00Z"), "unexpected: {got}");
    }
}
