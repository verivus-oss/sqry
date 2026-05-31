//! `SqrydHook` — post-publish persistence hook.
//!
//! Phase 6c of the sqryd plan, hardened by PF03A (corrective program
//! 2026-05-07). Every successful publish triggers a best-effort write
//! of the derived-analysis cache (`.sqry/graph/derived.sqry`) via
//! `sqry_db::persistence::save_derived`, on a background tokio task
//! with a configurable timeout. Errors and timeouts are logged at
//! WARN and absorbed — they never fail the query/publish path.
//!
//! This module defines the [`SqrydHook`] trait, the [`NoOpHook`] default
//! used for tests and embedded callers, and the production
//! [`QueryDbHook`] that the `sqryd` binary installs at startup
//! (PF03B).
//!
//! ## PF03A architectural decisions
//!
//! ### Crate / feature boundary
//!
//! `sqry-daemon` depends on `sqry-db` unconditionally (added in PF03A).
//! Earlier comments suggested gating the dependency behind a `sqry-db-hook`
//! Cargo feature so embedders could opt out of the writer; PF03A discards
//! that gating because the corrective program (see
//! `docs/development/generational-analysis-platform/priority-followups/03_IMPLEMENTATION_PLAN.md`
//! §A2) demands the production hook be present in every production
//! sqryd build, not opt-in. Embedders that want the no-persistence
//! behaviour pass [`noop_hook`] or any custom impl explicitly into
//! [`super::WorkspaceManager::set_hook`].
//!
//! ### Snapshot SHA timing (PF03A decision A, corrected by RPI)
//!
//! [`QueryDbHook::on_publish`] never writes the canonical
//! `<workspace_root>/.sqry/graph/snapshot.sqry` file. It verifies the
//! already-persisted manifest/snapshot pair, warms a bounded query inventory
//! over the published in-memory graph, and saves `derived.sqry` with the
//! verified persisted snapshot SHA. If the manifest or snapshot is absent,
//! corrupt, or mismatched, the hook skips derived-cache persistence and logs
//! a diagnostic. Canonical snapshot, manifest, and analysis artifacts are
//! owned exclusively by the manifest-as-commit-point graph persistence
//! transaction.
//!
//! ### Async lifetime / ownership (PF03A decision B)
//!
//! The hook is invoked with an owned `Arc<CodeGraph>` (cloned from the
//! published workspace inside `WorkspaceManager`). The spawned task
//! moves that `Arc` into its closure, takes a [`GraphSnapshot`] from it,
//! and wraps the snapshot in its own `Arc` for the temporary
//! [`sqry_db::QueryDb`]. The strong reference held by the spawned task
//! keeps the underlying graph data alive until `save_derived` returns
//! (or the timeout fires), independent of any concurrent workspace
//! `unload`, eviction, or replacement publish. Once the task drops, the
//! `Arc` is released; if the manager has already evicted the workspace
//! the data is freed at that point. There is no path that can leave
//! the spawned task reading freed memory.
//!
//! ### Failure isolation
//!
//! All work runs under [`spawn_hook`], which wraps the future in
//! [`tokio::time::timeout`] and logs both error and timeout outcomes
//! at WARN. The publish/query path never observes the result — the
//! caller does not await the spawned task.
//!
//! The hook runs on the current tokio runtime via `tokio::spawn`; the
//! publish call site does not await it. The default timeout is taken
//! from `DaemonConfig::rebuild_drain_timeout_ms` (5 s by default),
//! clampable by the call site if a tighter ceiling is desired.

use std::{path::Path, sync::Arc, time::Duration};

use sqry_core::graph::CodeGraph;
use sqry_db::queries::{CalleesQuery, CallersQuery, RelationKey};
use tracing::warn;

const MAX_DERIVED_WARMUP_SYMBOLS: usize = 64;

/// Signature for a post-publish persistence hook.
///
/// Called from [`super::WorkspaceManager::publish_and_retain`]
/// *after* the admission commit has succeeded, with the published
/// `Arc<CodeGraph>` and the workspace root path. The hook is
/// expected to return immediately — any actual IO should be
/// spawned on a tokio task via [`spawn_hook`] or equivalent —
/// because `publish_and_retain` is a sync critical section.
pub trait SqrydHook: Send + Sync + std::fmt::Debug {
    /// Notify the hook that a fresh graph has been published for
    /// `workspace_root`. Implementations should NOT block; they
    /// should spawn a background task and return.
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>);
}

impl<T: SqrydHook + ?Sized> SqrydHook for Arc<T> {
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>) {
        (**self).on_publish(workspace_root, graph);
    }
}

/// Null implementation — used by unit tests + the Phase 6c
/// default when no production hook is wired. Logs nothing, does
/// nothing, adds no runtime overhead.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpHook;

impl SqrydHook for NoOpHook {
    fn on_publish(&self, _workspace_root: &Path, _graph: Arc<CodeGraph>) {
        // deliberately empty
    }
}

/// Shared handle to the active hook. The manager stores an
/// `ArcSwap<Arc<dyn SqrydHook>>` so Task 9 can install the
/// production hook after the daemon boots (once the sqry-db
/// `QueryDb` is built), and unit tests can install a recording
/// hook at construction time.
pub type SharedHook = Arc<dyn SqrydHook>;

/// Convenience constructor for [`NoOpHook`] as a [`SharedHook`].
#[must_use]
pub fn noop_hook() -> SharedHook {
    Arc::new(NoOpHook)
}

/// Spawn an async persistence task with the configured timeout.
///
/// The task's result is never awaited by the caller. Errors and
/// timeouts are logged at WARN; the query path is unaffected.
///
/// This helper is public so the production
/// [`super::manager::WorkspaceManager`] and custom `SqrydHook`
/// impls can share the same timeout-and-absorb pattern.
pub fn spawn_hook<F, Fut, E>(
    timeout: Duration,
    workspace_root: std::path::PathBuf,
    task_label: &'static str,
    fut_factory: F,
) where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    tokio::spawn(async move {
        let fut = fut_factory();
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(
                    task = task_label,
                    workspace = %workspace_root.display(),
                    error = %err,
                    "sqryd hook {task_label} failed (absorbed; query path continues)",
                );
            }
            Err(_elapsed) => {
                let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
                warn!(
                    task = task_label,
                    workspace = %workspace_root.display(),
                    timeout_ms,
                    "sqryd hook {task_label} timed out (absorbed; query path continues)",
                );
            }
        }
    });
}

// ---------------------------------------------------------------------------
// QueryDbHook — production post-publish hook.
//
// PF03A introduces this type. PF03B (`sqryd` startup) calls
// `WorkspaceManager::set_hook(QueryDbHook::new(timeout))` so every published
// graph triggers a `sqry_db::persistence::save_derived` call against the
// canonical `<workspace_root>/.sqry/graph/derived.sqry` path. Failures are
// logged at WARN and absorbed; the publish path is never blocked.
// ---------------------------------------------------------------------------

/// Production [`SqrydHook`] backed by `sqry_db::persistence::save_derived`.
///
/// See the module-level docs for the snapshot-SHA-timing and lifetime
/// decisions (PF03A A and B). The hook is parameterised by:
///
/// - `timeout` — wall-clock cap applied via [`spawn_hook`] /
///   [`tokio::time::timeout`]. Defaults to `DaemonConfig::rebuild_drain_timeout_ms`
///   (5 s) when constructed via [`Self::new`].
/// - `query_db_config` — passed straight to [`sqry_db::QueryDb::new`] and
///   used by [`sqry_db::derived_path`] to compute the target file. The
///   default value points at `derived.sqry` and uses the standard
///   per-entry size cap; production sqryd should generally accept the
///   default.
#[derive(Debug, Clone)]
pub struct QueryDbHook {
    timeout: Duration,
    query_db_config: sqry_db::QueryDbConfig,
}

impl QueryDbHook {
    /// Construct a hook with the supplied timeout and the default
    /// [`sqry_db::QueryDbConfig`].
    ///
    /// PF03B's startup path uses this constructor with
    /// `DaemonConfig::rebuild_drain_timeout_ms`.
    #[must_use]
    pub fn new(timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            timeout,
            query_db_config: sqry_db::QueryDbConfig::default(),
        })
    }

    /// Construct a hook with a caller-provided [`sqry_db::QueryDbConfig`].
    ///
    /// Reserved for callers that need to override the default
    /// `derived_persistence_filename` or per-entry size cap. Production
    /// sqryd should normally use [`Self::new`].
    #[must_use]
    pub fn with_query_db_config(
        timeout: Duration,
        query_db_config: sqry_db::QueryDbConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            timeout,
            query_db_config,
        })
    }

    /// Returns the configured wall-clock timeout. Exposed for tests and
    /// observability surfaces.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl SqrydHook for QueryDbHook {
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>) {
        let timeout = self.timeout;
        let query_db_config = self.query_db_config.clone();
        let workspace_root_owned = workspace_root.to_path_buf();

        // Move owned values into the spawned task. spawn_hook wraps the
        // future in `tokio::time::timeout` and absorbs errors / timeouts.
        spawn_hook::<_, _, anyhow::Error>(
            timeout,
            workspace_root_owned.clone(),
            "query-db-save-derived",
            move || {
                let workspace_root = workspace_root_owned;
                let graph = graph;
                let query_db_config = query_db_config;
                async move { run_save_derived(workspace_root, graph, query_db_config).await }
            },
        );
    }
}

/// Body of the production hook: verify the existing persisted
/// manifest/snapshot identity, warm a bounded inventory of persistent
/// relation queries from the published in-memory graph, and persist only the
/// derived cache via `save_derived`.
///
/// All filesystem-touching operations run under [`tokio::task::spawn_blocking`]
/// so the runtime never blocks on synchronous IO.
async fn run_save_derived(
    workspace_root: std::path::PathBuf,
    graph: Arc<CodeGraph>,
    query_db_config: sqry_db::QueryDbConfig,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        run_save_derived_blocking(&workspace_root, &graph, query_db_config)
    })
    .await
    .map_err(|join_err| anyhow::anyhow!("spawn_blocking(query-db-save-derived) join: {join_err}"))?
}

fn run_save_derived_blocking(
    workspace_root: &Path,
    graph: &CodeGraph,
    query_db_config: sqry_db::QueryDbConfig,
) -> anyhow::Result<()> {
    let Some(sha) = verified_persisted_snapshot_sha(workspace_root)? else {
        return Ok(());
    };

    let snapshot_arc = Arc::new(graph.snapshot());
    let db = sqry_db::QueryDb::new(snapshot_arc, query_db_config);
    let warmed_entries = warm_persistent_queries(&db);
    tracing::debug!(
        workspace = %workspace_root.display(),
        warmed_entries,
        "QueryDbHook: warmed persistent query entries before derived-cache save"
    );

    let derived_path = sqry_db::derived_path(workspace_root, db.config());
    sqry_db::persistence::save_derived(&db, sha, &derived_path, workspace_root)?;

    Ok(())
}

fn verified_persisted_snapshot_sha(workspace_root: &Path) -> anyhow::Result<Option<[u8; 32]>> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
    if !storage.manifest_path().exists() {
        tracing::debug!(
            workspace = %workspace_root.display(),
            manifest = %storage.manifest_path().display(),
            "QueryDbHook: skipping derived-cache save because graph manifest is absent"
        );
        return Ok(None);
    }
    if !storage.snapshot_path().exists() {
        tracing::warn!(
            workspace = %workspace_root.display(),
            snapshot = %storage.snapshot_path().display(),
            "QueryDbHook: skipping derived-cache save because graph snapshot is absent"
        );
        return Ok(None);
    }

    let manifest = storage.load_manifest().map_err(|err| {
        anyhow::anyhow!("load manifest {}: {err}", storage.manifest_path().display())
    })?;
    let actual_sha =
        sqry_db::persistence::compute_file_sha256(storage.snapshot_path()).map_err(|err| {
            anyhow::anyhow!(
                "compute_file_sha256({}): {err}",
                storage.snapshot_path().display()
            )
        })?;
    let actual_hex = actual_sha
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if manifest.snapshot_sha256 != actual_hex {
        tracing::warn!(
            workspace = %workspace_root.display(),
            manifest_sha = %manifest.snapshot_sha256,
            actual_sha = %actual_hex,
            "QueryDbHook: skipping derived-cache save because manifest/snapshot SHA-256 mismatch"
        );
        return Ok(None);
    }

    Ok(Some(actual_sha))
}

fn warm_persistent_queries(db: &sqry_db::QueryDb) -> usize {
    let mut symbol_names = std::collections::BTreeSet::new();
    for (_node_id, node) in db.snapshot().iter_nodes() {
        if node.is_unified_loser() {
            continue;
        }
        if let Some(name) = db.snapshot().strings().resolve(node.name) {
            symbol_names.insert(name.to_string());
        }
        if let Some(qualified_name_id) = node.qualified_name
            && let Some(qualified_name) = db.snapshot().strings().resolve(qualified_name_id)
        {
            symbol_names.insert(qualified_name.to_string());
        }
        if symbol_names.len() >= MAX_DERIVED_WARMUP_SYMBOLS {
            break;
        }
    }

    let mut warmed_entries = 0usize;
    for symbol_name in symbol_names {
        let key = RelationKey::exact(symbol_name);
        let _ = db.get::<CallersQuery>(&key);
        let _ = db.get::<CalleesQuery>(&key);
        warmed_entries += 2;
    }
    warmed_entries
}

/// Recording hook used by unit tests to observe hook invocations
/// without exercising the real persistence path.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct RecordingHook {
    pub invocations: parking_lot::Mutex<Vec<std::path::PathBuf>>,
}

impl RecordingHook {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn invocation_count(&self) -> usize {
        self.invocations.lock().len()
    }

    #[must_use]
    pub fn invocation_roots(&self) -> Vec<std::path::PathBuf> {
        self.invocations.lock().clone()
    }
}

impl SqrydHook for RecordingHook {
    fn on_publish(&self, workspace_root: &Path, _graph: Arc<CodeGraph>) {
        self.invocations.lock().push(workspace_root.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_hook_compiles_through_shared_dispatch() {
        let hook: SharedHook = noop_hook();
        let graph = Arc::new(CodeGraph::new());
        hook.on_publish(Path::new("/repos/example"), graph);
    }

    #[test]
    fn recording_hook_captures_invocations_in_order() {
        let hook = RecordingHook::new();
        let graph = Arc::new(CodeGraph::new());
        hook.on_publish(Path::new("/repos/a"), Arc::clone(&graph));
        hook.on_publish(Path::new("/repos/b"), Arc::clone(&graph));
        assert_eq!(hook.invocation_count(), 2);
        let roots = hook.invocation_roots();
        assert_eq!(roots[0], Path::new("/repos/a"));
        assert_eq!(roots[1], Path::new("/repos/b"));
    }

    #[tokio::test]
    async fn spawn_hook_absorbs_error() {
        // Hook returns Err; timeout wrapper logs at WARN and
        // absorbs. Success criterion: the spawned task completes
        // without panic.
        spawn_hook::<_, _, &'static str>(
            Duration::from_millis(100),
            std::path::PathBuf::from("/repos/example"),
            "test-hook",
            || async { Err("simulated failure") },
        );
        // Give the spawned task time to run.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn spawn_hook_absorbs_timeout() {
        spawn_hook::<_, _, &'static str>(
            Duration::from_millis(10),
            std::path::PathBuf::from("/repos/example"),
            "test-hook",
            || async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ---------------------------------------------------------------------
    // PF03A regression coverage for the production QueryDbHook.
    // ---------------------------------------------------------------------

    /// Construction smoke test: the production hook builds with the default
    /// config and reports the requested timeout.
    #[test]
    fn pf03a_query_db_hook_constructs_with_requested_timeout() {
        let hook = QueryDbHook::new(Duration::from_millis(1234));
        assert_eq!(hook.timeout(), Duration::from_millis(1234));
    }

    /// `with_query_db_config` accepts a custom [`sqry_db::QueryDbConfig`]
    /// and threads it through unchanged.
    #[test]
    fn pf03a_query_db_hook_accepts_custom_config() {
        let cfg = sqry_db::QueryDbConfig::default();
        let hook = QueryDbHook::with_query_db_config(Duration::from_millis(50), cfg);
        // The hook holds an owned clone; the timeout accessor proves the
        // construction path completed.
        assert_eq!(hook.timeout(), Duration::from_millis(50));
    }

    /// RPI correction: when the canonical snapshot file is absent, the
    /// hook skips derived-cache persistence. It must not create
    /// `snapshot.sqry` as a side effect of publish/load.
    #[tokio::test]
    async fn rpi_query_db_hook_no_snapshot_file_skips_derived_without_snapshot_write() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let hook = QueryDbHook::new(Duration::from_secs(2));
        let graph = Arc::new(CodeGraph::new());

        // Create the .sqry/graph/ directory but no snapshot.sqry file —
        // mirrors a publish that hasn't yet flushed to disk.
        std::fs::create_dir_all(workspace.path().join(".sqry").join("graph")).unwrap();

        hook.on_publish(workspace.path(), graph);

        // Give the spawned task a generous window to complete.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let snapshot = workspace
            .path()
            .join(".sqry")
            .join("graph")
            .join("snapshot.sqry");
        let derived = workspace
            .path()
            .join(".sqry")
            .join("graph")
            .join("derived.sqry");
        assert!(
            !snapshot.exists(),
            "RPI: hook must not write canonical snapshot.sqry when it is absent (got {})",
            snapshot.display()
        );
        assert!(
            !derived.exists(),
            "RPI: hook must skip derived.sqry when no coherent persisted graph exists (got {})",
            derived.display()
        );
    }

    /// PF03A failure isolation: when the snapshot file is malformed (cannot
    /// be hashed because the parent directory was deleted mid-flight, or
    /// the path has been replaced by a directory), the hook absorbs the
    /// error and never panics. The publish path must keep running.
    #[tokio::test]
    async fn pf03a_query_db_hook_absorbs_save_failure() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let hook = QueryDbHook::new(Duration::from_secs(1));
        let graph = Arc::new(CodeGraph::new());

        // Create a `snapshot.sqry` directory entry that's actually a
        // directory; `compute_file_sha256` will fail with a non-NotFound
        // error and `save_derived` is never called. Either way, the hook
        // returns without panicking.
        let snap_dir = workspace.path().join(".sqry").join("graph");
        std::fs::create_dir_all(&snap_dir).unwrap();
        std::fs::create_dir_all(snap_dir.join("snapshot.sqry")).unwrap();

        hook.on_publish(workspace.path(), graph);
        tokio::time::sleep(Duration::from_millis(100)).await;

        // No panic = success. derived.sqry must not exist (the hook never
        // got far enough to write it).
        let derived = snap_dir.join("derived.sqry");
        assert!(
            !derived.exists(),
            "PF03A: a hashing failure must not leave a partially-written derived.sqry"
        );
    }

    /// RPI end-to-end happy path: with a coherent persisted graph, the hook
    /// writes only derived.sqry before its timeout fires and leaves canonical
    /// snapshot bytes untouched.
    #[tokio::test]
    async fn rpi_query_db_hook_writes_derived_sqry_when_manifest_snapshot_coherent() {
        let workspace = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("src").join("lib.rs"),
            "pub fn helper() -> u32 { 42 }\n",
        )
        .unwrap();

        let plugins = sqry_plugin_registry::create_plugin_manager();
        let cfg = sqry_core::graph::unified::build::BuildConfig::default();
        let graph_owned =
            sqry_core::graph::unified::build::build_unified_graph(workspace.path(), &plugins, &cfg)
                .unwrap();
        sqry_core::graph::unified::build::persist_and_analyze_graph(
            graph_owned,
            workspace.path(),
            &plugins,
            &cfg,
            "test:query-db-hook",
            None,
            sqry_core::progress::no_op_reporter(),
            1,
        )
        .unwrap();

        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace.path());
        let before_snapshot = std::fs::read(storage.snapshot_path()).unwrap();

        let graph_owned =
            sqry_core::graph::unified::build::build_unified_graph(workspace.path(), &plugins, &cfg)
                .unwrap();
        let hook = QueryDbHook::new(Duration::from_secs(5));
        let graph = Arc::new(graph_owned);
        hook.on_publish(workspace.path(), graph);

        // Poll for the derived file with a generous deadline so slow CI
        // runners don't false-negative.
        let derived = storage.graph_dir().join("derived.sqry");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if derived.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            derived.exists(),
            "RPI: hook must write derived.sqry to {} within 5s",
            derived.display()
        );
        let after_snapshot = std::fs::read(storage.snapshot_path()).unwrap();
        assert_eq!(
            after_snapshot, before_snapshot,
            "RPI: hook must not mutate canonical snapshot.sqry while saving derived.sqry"
        );

        // Sanity-check the file is non-empty and starts with the derived
        // magic bytes — proves it actually went through `save_derived`.
        let bytes = std::fs::read(&derived).unwrap();
        assert!(bytes.len() >= sqry_db::DERIVED_MAGIC.len());
        assert_eq!(
            &bytes[..sqry_db::DERIVED_MAGIC.len()],
            sqry_db::DERIVED_MAGIC,
            "derived.sqry must start with SQRY_DERIVED_V02 magic"
        );
    }
}
