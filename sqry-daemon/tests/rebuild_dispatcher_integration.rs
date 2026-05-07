//! Task 7 Phase 7a — `RebuildDispatcher` end-to-end integration test.
//!
//! Loads a real `rust_small` fixture (copied to a temp dir so writes
//! don't pollute the source tree), boots a [`WorkspaceManager`] +
//! [`RebuildDispatcher`], and drives two back-to-back
//! [`RebuildDispatcher::handle_changes`] calls through the real
//! `build_unified_graph` / `incremental_rebuild` pipeline.
//!
//! Assertions:
//!
//! - Initial `get_or_load` populates the graph with > 0 nodes.
//! - First dispatch with a single-file edit takes the `Incremental`
//!   path; `dispatched_count` advances to 1; graph epoch advances.
//! - Second dispatch with `git_change_class = TreeDiverged` takes the
//!   `Full` path; `dispatched_count` advances to 2; graph epoch
//!   advances again.
//! - `WorkspaceManager::status().memory.current_bytes > 0` after both
//!   dispatches and stays under the configured limit (indirect check
//!   for reservation leaks).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use sqry_core::{
    graph::unified::build::BuildConfig,
    project::ProjectRootMode,
    watch::{ChangeSet, GitChangeClass},
};
use sqry_daemon::{
    DaemonConfig, RebuildDispatcher, RebuildMode, WorkspaceKey, WorkspaceManager,
    workspace::{WorkingSetInputs, WorkspaceBuilder, working_set_estimate},
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn fixture_source_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .expect("workspace root must exist")
        .join("sqry-core")
        .join("tests")
        .join("fixtures")
        .join("incremental")
        .join("rust_small")
}

/// Recursively copy the rust_small fixture into `dst`.
fn copy_fixture_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path).expect("create dst dir");
            copy_fixture_tree(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy fixture file");
        }
    }
}

fn make_tempdir_fixture() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    copy_fixture_tree(&fixture_source_path(), tmp.path());
    tmp
}

// ---------------------------------------------------------------------------
// Test-local WorkspaceBuilder backed by the real sqry-core pipeline.
// ---------------------------------------------------------------------------

struct RealGraphBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    cfg: BuildConfig,
}

impl std::fmt::Debug for RealGraphBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealGraphBuilder").finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for RealGraphBuilder {
    fn build(
        &self,
        workspace_root: &Path,
    ) -> Result<sqry_core::graph::CodeGraph, sqry_daemon::DaemonError> {
        sqry_core::graph::unified::build::build_unified_graph(
            workspace_root,
            &self.plugins,
            &self.cfg,
        )
        .map_err(|e| sqry_daemon::DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: format!("test build: {e}"),
        })
    }
}

// ---------------------------------------------------------------------------
// The integration test itself.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_end_to_end_on_rust_small_fixture() {
    let tmp = make_tempdir_fixture();
    let root = tmp.path().to_path_buf();

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );

    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    // --- Initial load via the real pipeline -------------------------
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    // Working-set estimate for the initial load: use the admission
    // helper with a conservative fixture-sized input.
    let initial_estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024, // 1 MiB
        staging_overhead: 256 * 1024,           // 256 KiB
        interner_snapshot_bytes: 128 * 1024,    // 128 KiB
    });
    let initial_graph = manager
        .get_or_load(&key, &builder, initial_estimate)
        .expect("initial load must succeed");
    let initial_epoch = initial_graph.epoch();
    let initial_node_count = initial_graph.node_count();
    assert!(
        initial_node_count > 0,
        "rust_small must produce a non-empty graph",
    );

    // --- Dispatch 1: incremental on a single file edit ---------------
    //
    // Append a trivial function to util.rs so the rebuild has a real
    // delta to produce.
    let edited = root.join("util.rs");
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&edited)
            .expect("open util.rs");
        writeln!(f, "\npub fn added_in_phase_7a_test() {{}}\n").expect("write appended function");
    }

    let inc_changes = ChangeSet {
        changed_files: vec![edited.clone()],
        git_state_changed: false,
        git_change_class: None,
    };

    dispatcher
        .handle_changes(&key, inc_changes)
        .await
        .expect("incremental dispatch must succeed");

    assert_eq!(
        dispatcher.last_mode(),
        Some(RebuildMode::Incremental),
        "single-file edit must choose Incremental",
    );
    assert_eq!(dispatcher.dispatched_count(), 1);

    // Re-read the published graph via the cache-hit fast path on
    // `get_or_load`: once the workspace is `Loaded`, `get_or_load`
    // short-circuits (manager.rs:558) and returns the ArcSwap pointee
    // without touching admission or the builder.
    let post_incr_graph = manager
        .get_or_load(&key, &builder, 0)
        .expect("cache-hit reload");
    assert!(
        post_incr_graph.epoch() > initial_epoch,
        "incremental publish must advance the graph epoch (before={initial_epoch}, after={})",
        post_incr_graph.epoch(),
    );
    assert!(
        post_incr_graph.node_count() > initial_node_count,
        "incremental rebuild must include the newly appended function \
         (before={initial_node_count}, after={})",
        post_incr_graph.node_count(),
    );

    // --- Dispatch 2: full rebuild on a git-state signal ----------------
    //
    // Synthesise a TreeDiverged git classification — no file changes
    // required because the decision fork falls on
    // `requires_full_rebuild()`.
    let full_changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    };

    dispatcher
        .handle_changes(&key, full_changes)
        .await
        .expect("full dispatch must succeed");

    assert_eq!(
        dispatcher.last_mode(),
        Some(RebuildMode::Full),
        "TreeDiverged signal must choose Full",
    );
    assert_eq!(dispatcher.dispatched_count(), 2);

    let post_full_graph = manager
        .get_or_load(&key, &builder, 0)
        .expect("cache-hit reload");
    // Full rebuilds produce a fresh `CodeGraph` whose epoch starts
    // independent of the prior graph's epoch — the two graphs are not
    // in an ancestor relationship. Assert publish occurred by
    // verifying the graph is non-empty and has a comparable node
    // count to the incrementally-updated graph (the tempdir fixture
    // is the same workspace, plus the appended function).
    assert!(
        post_full_graph.node_count() > 0,
        "full publish must produce a non-empty graph",
    );
    assert!(
        post_full_graph.node_count() >= initial_node_count,
        "full publish must include at least as many nodes as the \
         initial rust_small build (before={initial_node_count}, after={})",
        post_full_graph.node_count(),
    );

    // --- Admission sanity -----------------------------------------------
    //
    // If the dispatcher leaked reservation bytes, the daemon-status
    // reading would accumulate past the fixture's actual memory cost.
    // We cannot read `reserved_bytes` directly from an external test
    // binary, but any leak would surface as `current_bytes` ballooning
    // past the configured memory limit.
    let status = manager.status();
    assert!(
        status.memory.current_bytes > 0,
        "publish must update admission accounting",
    );
    assert!(
        status.memory.current_bytes < status.memory.limit_bytes,
        "current_bytes must stay under the configured limit (got {} bytes, cap {})",
        status.memory.current_bytes,
        status.memory.limit_bytes,
    );
}

// ---------------------------------------------------------------------------
// PF03B integration: production QueryDbHook wired through publish path.
//
// Verifies that when the daemon installs `QueryDbHook` (matching what
// `entrypoint::build_daemon_components` does), a publish writes the canonical
// snapshot, warms useful derived-cache entries, and persists derived.sqry with
// a SHA bound to that same snapshot identity.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pf08_pf09_query_db_hook_writes_useful_derived_sqry_after_publish() {
    use sqry_core::graph::unified::persistence::load_from_path;
    use sqry_daemon::workspace::{QueryDbHook, SharedHook};
    use sqry_db::queries::{CalleesQuery, RelationKey};
    use std::time::Duration;

    let tmp = make_tempdir_fixture();
    let workspace_root = tmp.path().to_path_buf();

    // Seed a stale snapshot first. PF09 requires the hook to bind the derived
    // cache to the graph it publishes, not to whatever snapshot happened to be
    // on disk before publish.
    let snap_dir = workspace_root.join(".sqry").join("graph");
    fs::create_dir_all(&snap_dir).expect("create .sqry/graph");
    let snapshot_path = snap_dir.join("snapshot.sqry");
    sqry_core::graph::unified::persistence::save_to_path(
        &sqry_core::graph::CodeGraph::new(),
        &snapshot_path,
    )
    .expect("save stale snapshot.sqry");
    let stale_sha =
        sqry_db::persistence::compute_file_sha256(&snapshot_path).expect("hash stale snapshot");

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));

    // Mirror entrypoint.rs::build_daemon_components — install the
    // production QueryDbHook with the configured drain timeout.
    let hook = QueryDbHook::new(Duration::from_millis(config.rebuild_drain_timeout_ms));
    manager.set_hook(hook as SharedHook);

    let key = WorkspaceKey::new(workspace_root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::new(sqry_plugin_registry::create_plugin_manager()),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024,
        staging_overhead: 256 * 1024,
        interner_snapshot_bytes: 128 * 1024,
    });
    let published_graph = manager
        .get_or_load(&key, &builder, estimate)
        .expect("publish via real fixture builder must succeed");
    assert!(
        published_graph.node_count() > 0,
        "PF08 fixture graph must be non-empty"
    );

    // The hook is fire-and-forget; poll for derived.sqry within the
    // configured timeout window with a small safety margin.
    let derived = snap_dir.join("derived.sqry");
    let deadline = std::time::Instant::now()
        + Duration::from_millis(config.rebuild_drain_timeout_ms.saturating_mul(2).max(2_000));
    while std::time::Instant::now() < deadline {
        if derived.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        derived.exists(),
        "PF03B: production QueryDbHook must write {} after publish",
        derived.display()
    );

    let bytes = fs::read(&derived).expect("read derived.sqry");
    assert!(bytes.len() >= sqry_db::DERIVED_MAGIC.len());
    assert_eq!(
        &bytes[..sqry_db::DERIVED_MAGIC.len()],
        sqry_db::DERIVED_MAGIC,
        "derived.sqry must start with SQRY_DERIVED_V02 magic bytes",
    );

    let (header, _tail) =
        sqry_db::persistence::deserialize_derived_header(&bytes).expect("decode header");
    assert!(
        header.entry_count > 0,
        "PF08: daemon-written derived.sqry must contain useful persisted entries"
    );
    let current_sha =
        sqry_db::persistence::compute_file_sha256(&snapshot_path).expect("hash current snapshot");
    assert_ne!(
        header.snapshot_sha256, stale_sha,
        "PF09: derived header must not retain the stale pre-publish snapshot SHA"
    );
    assert_eq!(
        header.snapshot_sha256, current_sha,
        "PF09: derived header must match the freshly-published snapshot SHA"
    );

    let persisted_graph = load_from_path(&snapshot_path, None).expect("load persisted snapshot");
    let persisted_snapshot = persisted_graph.snapshot();
    let first_symbol = persisted_snapshot
        .iter_nodes()
        .find_map(|(_node_id, node)| persisted_snapshot.strings().resolve(node.name))
        .expect("fixture has at least one resolvable symbol")
        .to_string();

    let cold_db = sqry_db::make_query_db_cold(Arc::new(persisted_snapshot), &workspace_root);
    let before = cold_db.metrics();
    let _ = cold_db.get::<CalleesQuery>(&RelationKey::exact(first_symbol));
    let after = cold_db.metrics();
    assert_eq!(
        after.cache_misses, before.cache_misses,
        "PF08: first matching cold query must not recompute"
    );
    assert_eq!(
        after.cache_hits,
        before.cache_hits + 1,
        "PF08: first matching cold query must be served from daemon-written derived.sqry"
    );
}
