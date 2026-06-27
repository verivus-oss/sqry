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
    graph::unified::persistence::{GraphStorage, Manifest},
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

fn make_tiny_rust_workspace() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).expect("create src dir");
    fs::write(
        src.join("lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 7 }\n",
    )
    .expect("write tiny Rust fixture");
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"rebuild-persistence-integrity\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    tmp
}

fn direct_index_workspace(
    root: &Path,
    plugins: &sqry_core::plugin::PluginManager,
) -> sqry_core::graph::unified::build::BuildResult {
    let cfg = BuildConfig::default();
    let graph = sqry_core::graph::unified::build::build_unified_graph(root, plugins, &cfg)
        .expect("direct graph build must succeed");
    let (_graph, result) = sqry_core::graph::unified::build::persist_and_analyze_graph(
        graph,
        root,
        plugins,
        &cfg,
        "test:direct-index",
        None,
        sqry_core::progress::no_op_reporter(),
        1,
    )
    .expect("direct graph persistence must succeed");
    result
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn manifest_snapshot_sha_pair(root: &Path) -> (String, String) {
    let storage = GraphStorage::new(root);
    let manifest = Manifest::load(storage.manifest_path()).expect("load manifest");
    let actual = sqry_db::persistence::compute_file_sha256(storage.snapshot_path())
        .expect("hash snapshot.sqry");
    (manifest.snapshot_sha256, hex_lower(&actual))
}

fn assert_manifest_matches_snapshot(root: &Path) {
    let (manifest_sha, actual_sha) = manifest_snapshot_sha_pair(root);
    assert_eq!(
        manifest_sha, actual_sha,
        "manifest snapshot_sha256 must match actual snapshot.sqry SHA-256",
    );
}

fn assert_filesystem_graph_loads(root: &Path, plugins: &sqry_core::plugin::PluginManager) {
    let storage = GraphStorage::new(root);
    let graph = sqry_core::graph::unified::persistence::load_from_path(
        storage.snapshot_path(),
        Some(plugins),
    )
    .expect("filesystem-backed graph load must succeed");
    assert!(
        graph.node_count() > 0,
        "filesystem-backed graph load must return indexed nodes",
    );
}

async fn wait_for_derived_cache(root: &Path) {
    let derived = root.join(".sqry").join("graph").join("derived.sqry");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if derived.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for QueryDbHook to write {}",
        derived.display()
    );
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
        post_incr_graph.node_count() > initial_node_count,
        "incremental-triggered durable rebuild must include the newly appended function \
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
// Rebuild persistence integrity regressions.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn direct_index_then_daemon_load_preserves_manifest_snapshot_integrity() {
    use sqry_daemon::workspace::{QueryDbHook, SharedHook};
    use std::time::Duration;

    let tmp = make_tiny_rust_workspace();
    let root = tmp.path().to_path_buf();
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());

    direct_index_workspace(&root, &plugins);
    assert_manifest_matches_snapshot(&root);
    assert_filesystem_graph_loads(&root, &plugins);

    let storage = GraphStorage::new(&root);
    let before_snapshot = fs::read(storage.snapshot_path()).expect("read snapshot before load");
    let before_manifest = fs::read(storage.manifest_path()).expect("read manifest before load");

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    manager.set_hook(QueryDbHook::new(Duration::from_secs(5)) as SharedHook);

    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024,
        staging_overhead: 256 * 1024,
        interner_snapshot_bytes: 128 * 1024,
    });

    let graph = manager
        .get_or_load(&key, &builder, estimate)
        .expect("daemon load must succeed");
    assert!(
        graph.node_count() > 0,
        "daemon load graph must be non-empty"
    );

    wait_for_derived_cache(&root).await;

    let after_snapshot = fs::read(storage.snapshot_path()).expect("read snapshot after hook drain");
    let after_manifest = fs::read(storage.manifest_path()).expect("read manifest after hook drain");
    assert_eq!(
        after_manifest, before_manifest,
        "QueryDbHook must not mutate manifest.json during daemon load",
    );
    assert_eq!(
        after_snapshot, before_snapshot,
        "QueryDbHook must not mutate canonical snapshot.sqry during daemon load",
    );
    assert_manifest_matches_snapshot(&root);
    assert_filesystem_graph_loads(&root, &plugins);
}

#[tokio::test]
async fn direct_index_then_daemon_load_then_force_rebuild_keeps_filesystem_index_coherent() {
    use sqry_daemon::workspace::{QueryDbHook, SharedHook};
    use std::time::Duration;

    let tmp = make_tiny_rust_workspace();
    let root = tmp.path().to_path_buf();
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());

    direct_index_workspace(&root, &plugins);
    assert_manifest_matches_snapshot(&root);
    assert_filesystem_graph_loads(&root, &plugins);

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    manager.set_hook(QueryDbHook::new(Duration::from_secs(5)) as SharedHook);
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024,
        staging_overhead: 256 * 1024,
        interner_snapshot_bytes: 128 * 1024,
    });

    manager
        .get_or_load(&key, &builder, estimate)
        .expect("daemon load must succeed");
    wait_for_derived_cache(&root).await;
    assert_manifest_matches_snapshot(&root);
    assert_filesystem_graph_loads(&root, &plugins);

    let changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    };
    dispatcher
        .handle_changes(&key, changes)
        .await
        .expect("force rebuild must succeed only after durable persistence");

    assert_eq!(
        dispatcher.last_mode(),
        Some(RebuildMode::Full),
        "TreeDiverged force signal must select a full rebuild",
    );
    assert_manifest_matches_snapshot(&root);
    assert_filesystem_graph_loads(&root, &plugins);
}

#[tokio::test]
async fn durable_rebuild_persistence_failure_does_not_publish_or_leave_stale_manifest() {
    let tmp = make_tiny_rust_workspace();
    let root = tmp.path().to_path_buf();
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());

    direct_index_workspace(&root, &plugins);
    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024,
        staging_overhead: 256 * 1024,
        interner_snapshot_bytes: 128 * 1024,
    });
    let prior_graph = manager
        .get_or_load(&key, &builder, estimate)
        .expect("initial daemon load must succeed");
    let prior_node_count = prior_graph.node_count();

    fs::write(
        root.join("src").join("new_after_failure.rs"),
        "pub fn new_after_failure() -> u32 { 9 }\n",
    )
    .expect("write new source file");

    let storage = GraphStorage::new(&root);
    fs::remove_file(storage.snapshot_path()).expect("remove snapshot before failure injection");
    fs::create_dir(storage.snapshot_path()).expect("replace snapshot path with directory");

    let changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    };
    let err = dispatcher
        .handle_changes(&key, changes)
        .await
        .expect_err("rebuild must fail when durable snapshot persistence fails");
    assert!(
        matches!(err, sqry_daemon::DaemonError::WorkspaceBuildFailed { .. }),
        "durable persistence failure must surface as WorkspaceBuildFailed, got {err:?}",
    );

    let served_workspace = manager
        .lookup(&key)
        .expect("workspace must remain registered after failed rebuild");
    let served_graph = served_workspace.graph.load_full();
    assert_eq!(
        served_graph.node_count(),
        prior_node_count,
        "failed durable rebuild must not publish the new graph",
    );
    assert!(
        !storage.manifest_path().exists(),
        "failed durable rebuild must remove stale manifest before touching snapshot",
    );
}

// ---------------------------------------------------------------------------
// PF03B integration: production QueryDbHook wired through publish path.
//
// Verifies that when the daemon installs `QueryDbHook` (matching what
// `entrypoint::build_daemon_components` does), a publish leaves the canonical
// snapshot untouched and persists derived.sqry with a SHA bound to the
// verified existing snapshot identity. Issue #436 removed daemon publish-time
// synthetic relation warmup, so this test deliberately expects a valid empty
// derived-cache container rather than precomputed callers/callees entries.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pf08_pf09_query_db_hook_writes_derived_sqry_header_after_publish() {
    use sqry_daemon::workspace::{QueryDbHook, SharedHook};
    use std::time::Duration;

    let tmp = make_tempdir_fixture();
    let workspace_root = tmp.path().to_path_buf();
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    direct_index_workspace(&workspace_root, &plugins);
    assert_manifest_matches_snapshot(&workspace_root);
    let storage = GraphStorage::new(&workspace_root);
    let snapshot_path = storage.snapshot_path().to_path_buf();
    let snapshot_before = fs::read(&snapshot_path).expect("read pre-load snapshot");
    let manifest_sha_before = Manifest::load(storage.manifest_path())
        .expect("load manifest")
        .snapshot_sha256;

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));

    // Mirror entrypoint.rs::build_daemon_components, install the
    // production QueryDbHook with the configured derived-save timeout.
    let hook = QueryDbHook::new(Duration::from_millis(config.derived_save_timeout_ms));
    manager.set_hook(hook as SharedHook);

    let key = WorkspaceKey::new(workspace_root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
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
    // configured derived-save window. The tiny fixture saves in well
    // under a second, so cap the wait so a genuine non-write fails fast
    // instead of stalling the suite for the full 120 s ceiling.
    let derived = storage.graph_dir().join("derived.sqry");
    let poll_budget_ms = config.derived_save_timeout_ms.clamp(2_000, 15_000);
    let deadline = std::time::Instant::now() + Duration::from_millis(poll_budget_ms);
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
    let snapshot_after = fs::read(&snapshot_path).expect("read post-load snapshot");
    assert_eq!(
        snapshot_after, snapshot_before,
        "RPI: production QueryDbHook must not mutate canonical snapshot.sqry"
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
    assert_eq!(
        header.entry_count, 0,
        "Issue #436: daemon publish hook must not synthesize relation-query warmup entries"
    );
    let current_sha =
        sqry_db::persistence::compute_file_sha256(&snapshot_path).expect("hash current snapshot");
    assert_eq!(
        header.snapshot_sha256, current_sha,
        "RPI: derived header must match the verified persisted snapshot SHA"
    );
    assert_eq!(
        hex_lower(&header.snapshot_sha256),
        manifest_sha_before,
        "RPI: derived header SHA must match manifest.snapshot_sha256"
    );
}

// ---------------------------------------------------------------------------
// Regression: the REBUILD path must also dispatch QueryDbHook.
//
// verivus-oss/sqry#358. The load path (`get_or_load`) dispatched the hook,
// but `RebuildDispatcher::execute_one_rebuild` published a new graph without
// firing it. The derived-cache save therefore ran only on load: after every
// `sqry daemon rebuild` / incremental rebuild the snapshot SHA changed while
// `derived.sqry` stayed bound to the pre-rebuild SHA, so it was discarded as
// stale on the next query and never rewritten until the next load. This test
// drives a real rebuild and asserts derived.sqry is refreshed against the
// post-rebuild snapshot SHA. Without the fix the derived header stays at the
// pre-rebuild SHA and the poll below times out.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn rebuild_path_dispatches_query_db_hook_and_refreshes_derived_cache() {
    use sqry_daemon::workspace::{QueryDbHook, SharedHook};
    use std::time::{Duration, Instant};

    let tmp = make_tiny_rust_workspace();
    let root = tmp.path().to_path_buf();
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());

    direct_index_workspace(&root, &plugins);

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    manager.set_hook(
        QueryDbHook::new(Duration::from_millis(config.derived_save_timeout_ms)) as SharedHook,
    );
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);
    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 1_024 * 1024,
        staging_overhead: 256 * 1024,
        interner_snapshot_bytes: 128 * 1024,
    });

    // Load through the existing (already-working) path, which dispatches the
    // hook and writes derived.sqry against the initial snapshot.
    manager
        .get_or_load(&key, &builder, estimate)
        .expect("daemon load must succeed");
    wait_for_derived_cache(&root).await;
    let storage = GraphStorage::new(&root);
    let (_manifest_after_load, sha_after_load) = manifest_snapshot_sha_pair(&root);

    // Add a source file so the rebuild produces a different snapshot SHA.
    fs::write(
        root.join("src").join("added_for_rebuild_regression.rs"),
        "pub fn added_for_rebuild_regression() -> u32 { 11 }\n",
    )
    .expect("write new source file");

    // Force a full rebuild through the dispatcher.
    let changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    };
    dispatcher
        .handle_changes(&key, changes)
        .await
        .expect("force rebuild must succeed");
    assert_eq!(
        dispatcher.last_mode(),
        Some(RebuildMode::Full),
        "TreeDiverged force signal must select a full rebuild",
    );

    let (_manifest_after_rebuild, sha_after_rebuild) = manifest_snapshot_sha_pair(&root);
    assert_ne!(
        sha_after_rebuild, sha_after_load,
        "adding a source file must change the post-rebuild snapshot SHA",
    );

    // The fix under test: the rebuild path re-dispatches QueryDbHook, so
    // derived.sqry is rewritten with the NEW snapshot SHA. The hook is
    // fire-and-forget, so poll until the derived header matches the
    // post-rebuild snapshot. Without the rebuild-path dispatch this never
    // updates and the loop times out at the stale pre-rebuild SHA.
    let derived = storage.graph_dir().join("derived.sqry");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut derived_sha = String::new();
    while Instant::now() < deadline {
        if let Ok(bytes) = fs::read(&derived)
            && let Ok((header, _tail)) = sqry_db::persistence::deserialize_derived_header(&bytes)
        {
            derived_sha = hex_lower(&header.snapshot_sha256);
            if derived_sha == sha_after_rebuild {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        derived_sha, sha_after_rebuild,
        "verivus-oss/sqry#358: rebuild path must re-dispatch QueryDbHook so derived.sqry is \
         rewritten against the post-rebuild snapshot SHA (it was left stale at the pre-rebuild SHA)",
    );
}
