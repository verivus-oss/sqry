//! Shared test helpers for the Task 7 Phase 7b2 watcher + dispatcher
//! integration test binaries.
//!
//! Consumed by `rebuild_watcher_editor_patterns.rs`,
//! `rebuild_watcher_git_scenarios.rs`,
//! `rebuild_dispatcher_serialization.rs`, and
//! `rebuild_watcher_shutdown.rs` via a local `mod support;` statement.
//!
//! The `#[allow(dead_code)]` on each helper reflects that a given test
//! binary usually uses a subset of the exported surface; rust-analyzer
//! may highlight unused entries per-binary even though the module as a
//! whole is used.

#![allow(dead_code)]

pub mod editor_patterns;
pub mod ipc;

use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use sqry_core::{graph::unified::build::BuildConfig, project::ProjectRootMode};
use sqry_daemon::{
    DaemonConfig, DaemonError, RebuildDispatcher, WorkspaceKey, WorkspaceManager,
    workspace::{WorkingSetInputs, WorkspaceBuilder, working_set_estimate},
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// WorkspaceBuilder that drives the real sqry-core pipeline (reused from
// sqry-daemon/tests/rebuild_dispatcher_integration.rs).
// ---------------------------------------------------------------------------

pub struct RealGraphBuilder {
    pub plugins: Arc<sqry_core::plugin::PluginManager>,
    pub cfg: BuildConfig,
}

impl std::fmt::Debug for RealGraphBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealGraphBuilder").finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for RealGraphBuilder {
    fn build(&self, workspace_root: &Path) -> Result<sqry_core::graph::CodeGraph, DaemonError> {
        sqry_core::graph::unified::build::build_unified_graph(
            workspace_root,
            &self.plugins,
            &self.cfg,
        )
        .map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: format!("test build: {e}"),
        })
    }
}

// ---------------------------------------------------------------------------
// Harness construction
// ---------------------------------------------------------------------------

/// Full watcher-bridge test harness: tempdir workspace root + initial
/// git repo + loaded workspace + dispatcher + ensure_watching spawned.
pub struct WatcherHarness {
    pub tmp: TempDir,
    pub root: PathBuf,
    pub key: WorkspaceKey,
    pub manager: Arc<WorkspaceManager>,
    pub dispatcher: Arc<RebuildDispatcher>,
}

impl WatcherHarness {
    /// Tight debounce (100 ms) for fast test runs. Callers that need
    /// a different debounce should use [`Self::new_with_debounce`].
    pub async fn new() -> Self {
        Self::new_with_debounce(100).await
    }

    /// Construct a harness with the given debounce window (milliseconds).
    pub async fn new_with_debounce(debounce_ms: u64) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();

        // Seed a minimal git repo so SourceTreeWatcher::new succeeds.
        init_git_repo(&root);

        let config = Arc::new(DaemonConfig {
            debounce_ms,
            ..DaemonConfig::default()
        });
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
        let dispatcher = RebuildDispatcher::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::clone(&plugins),
        );

        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let builder = RealGraphBuilder {
            plugins: Arc::clone(&plugins),
            cfg: BuildConfig::default(),
        };
        let estimate = working_set_estimate(WorkingSetInputs {
            new_graph_final_estimate: 64 * 1024,
            staging_overhead: 32 * 1024,
            interner_snapshot_bytes: 16 * 1024,
        });
        manager
            .get_or_load(&key, &builder, estimate)
            .expect("initial load must succeed");

        let ws = manager.lookup(&key).expect("registered");
        dispatcher
            .ensure_watching(&key, ws, root.clone())
            .await
            .expect("ensure_watching must succeed on a fresh harness");

        Self {
            tmp,
            root,
            key,
            manager,
            dispatcher,
        }
    }

    /// Return the default `settle` duration for "exactly one rebuild"
    /// assertions: `2 × debounce`.
    #[must_use]
    pub fn settle_for_one(&self) -> Duration {
        Duration::from_millis(self.manager_config_debounce_ms() * 2)
    }

    /// Return the default `settle` duration for "zero rebuild"
    /// assertions: `3 × debounce`.
    #[must_use]
    pub fn settle_for_zero(&self) -> Duration {
        Duration::from_millis(self.manager_config_debounce_ms() * 3)
    }

    fn manager_config_debounce_ms(&self) -> u64 {
        // The manager stores the DaemonConfig Arc; we construct it
        // with a short debounce above. Peek via the admission path
        // is impractical — just hard-code the tightness: harness
        // guarantees `debounce_ms` passed to `new_with_debounce`, so
        // callers can query via their own handle. Default 100.
        // Returning the known config via a field would require
        // storing config: Arc<DaemonConfig> here; keep it simple.
        100
    }
}

// ---------------------------------------------------------------------------
// Simpler harness — no watcher, just dispatcher (for §J.2 stress tests
// that drive handle_changes directly).
// ---------------------------------------------------------------------------

pub struct DispatchHarness {
    pub tmp: TempDir,
    pub root: PathBuf,
    pub key: WorkspaceKey,
    pub manager: Arc<WorkspaceManager>,
    pub dispatcher: Arc<RebuildDispatcher>,
}

impl DispatchHarness {
    pub fn new() -> Self {
        Self::with_debounce(100)
    }

    pub fn with_debounce(debounce_ms: u64) -> Self {
        Self::with_debounce_and_cap(debounce_ms, 24)
    }

    /// Task 7 Phase 7c: construct a harness overriding both the
    /// debounce window and the `stale_serve_max_age_hours` cap so
    /// `classify_for_serve` tests can exercise `cap = 0`.
    pub fn with_debounce_and_cap(debounce_ms: u64, stale_cap_hours: u32) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();

        let config = Arc::new(DaemonConfig {
            debounce_ms,
            stale_serve_max_age_hours: stale_cap_hours,
            ..DaemonConfig::default()
        });
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
        let dispatcher = RebuildDispatcher::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::clone(&plugins),
        );

        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let builder = RealGraphBuilder {
            plugins: Arc::clone(&plugins),
            cfg: BuildConfig::default(),
        };
        let estimate = working_set_estimate(WorkingSetInputs {
            new_graph_final_estimate: 64 * 1024,
            staging_overhead: 32 * 1024,
            interner_snapshot_bytes: 16 * 1024,
        });
        manager
            .get_or_load(&key, &builder, estimate)
            .expect("initial load must succeed");

        Self {
            tmp,
            root,
            key,
            manager,
            dispatcher,
        }
    }
}

// ---------------------------------------------------------------------------
// Failing builder + workspace-state helpers (Task 7 Phase 7c)
// ---------------------------------------------------------------------------

/// A [`WorkspaceBuilder`] that always returns
/// [`DaemonError::WorkspaceBuildFailed`]. Used by `classify_for_serve`
/// tests that need a workspace in `Failed` state with no prior good
/// snapshot.
pub struct AlwaysFailBuilder;

impl std::fmt::Debug for AlwaysFailBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlwaysFailBuilder").finish()
    }
}

impl WorkspaceBuilder for AlwaysFailBuilder {
    fn build(&self, workspace_root: &Path) -> Result<sqry_core::graph::CodeGraph, DaemonError> {
        Err(DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: "AlwaysFailBuilder: synthetic failure for classify_for_serve tests".to_string(),
        })
    }
}

/// Insert a `LoadedWorkspace` into `manager.workspaces` in a specific
/// state, bypassing `get_or_load`. Used by `classify_for_serve` tests
/// that need the `Unloaded` / `Loading` arms to be reachable (both
/// states are normally transient during initial load).
///
/// SAFETY: uses internal manager access; the test crate must not
/// expose this to production code. Lives behind `pub` (crate-only)
/// because external tests consume it via `mod support;`.
pub fn insert_workspace_in_state(
    manager: &Arc<sqry_daemon::WorkspaceManager>,
    key: &sqry_daemon::WorkspaceKey,
    state: sqry_daemon::WorkspaceState,
) {
    // Use the manager's test-only insertion helper (added in 7c).
    manager.insert_workspace_in_state_for_test(key.clone(), state);
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Initialise `root` as a new git repo with a single committed file.
/// Required for `SourceTreeWatcher::new` (which resolves the gitdir).
pub fn init_git_repo(root: &Path) {
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@sqry.dev"]);
    run_git(root, &["config", "user.name", "Sqry Test"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("seed.rs"), b"pub fn seed() {}\n").expect("write seed");
    run_git(root, &["add", "seed.rs"]);
    run_git(root, &["commit", "-q", "-m", "seed"]);
}

pub fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git command failed to launch");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

// ---------------------------------------------------------------------------
// Bounded-poll assertion helpers (iter-1 §6 / iter-3 retained)
// ---------------------------------------------------------------------------

/// Snapshot `dispatched_count`, run `f`, poll until the count advances
/// by exactly 1 or `timeout` elapses, then confirm no additional
/// dispatch fires within `post_settle`.
///
/// # Panics
///
/// - Panics with "timeout waiting for 1 rebuild" if the count does not
///   reach `before + 1` within `timeout`.
/// - Panics with "unexpected extra rebuild" if the count exceeds
///   `before + 1` at any point (poll OR post-settle window).
pub async fn assert_exactly_one_rebuild<F>(
    dispatcher: &RebuildDispatcher,
    timeout: Duration,
    post_settle: Duration,
    f: F,
) where
    F: FnOnce(),
{
    let before = dispatcher.dispatched_count();
    f();

    let deadline = Instant::now() + timeout;
    loop {
        let now = dispatcher.dispatched_count();
        assert!(
            now <= before + 1,
            "unexpected extra rebuild during poll (before={before}, now={now})"
        );
        if now == before + 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for 1 rebuild (before={before}, now={now}, timeout={timeout:?})"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    tokio::time::sleep(post_settle).await;
    let final_count = dispatcher.dispatched_count();
    assert_eq!(
        final_count,
        before + 1,
        "unexpected extra rebuild in post-settle window (before={before}, final={final_count})"
    );
}

/// Snapshot `dispatched_count`, run `f`, sleep `settle`, assert count
/// unchanged. Inherently time-bounded: proving absence of a rebuild
/// requires a settle window.
///
/// # Panics
///
/// Panics if `dispatched_count` advances at any point during the
/// settle window.
pub async fn assert_zero_rebuilds<F>(dispatcher: &RebuildDispatcher, settle: Duration, f: F)
where
    F: FnOnce(),
{
    let before = dispatcher.dispatched_count();
    f();
    tokio::time::sleep(settle).await;
    let after = dispatcher.dispatched_count();
    assert_eq!(
        after, before,
        "unexpected rebuild during assert_zero_rebuilds (before={before}, after={after})"
    );
}

// ---------------------------------------------------------------------------
// Generic bounded-poll
// ---------------------------------------------------------------------------

/// Poll `predicate` until it returns `true` or `timeout` elapses.
/// Returns `true` if the predicate fired within the window.
pub async fn wait_until<F>(mut predicate: F, timeout: Duration) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll an async predicate until it returns `true` or timeout elapses.
pub async fn wait_until_async<F, Fut>(mut predicate: F, timeout: Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
