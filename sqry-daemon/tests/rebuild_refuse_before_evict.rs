//! A rebuild refused for its own input evicts nothing, and every refusal
//! returns the workspace to `Loaded` (the rebuild dispatcher).
//!
//! Each test runs under memory pressure: workspace A and a sibling B are both
//! resident, and the budget holds the two of them but not A's rebuild working
//! set on top, so A's rebuild reservation can only commit by evicting B. A
//! request the dispatcher refuses (an empty, missing or unrecordable expand
//! cache, an unreadable manifest, a manifest naming a plugin id this binary did
//! not compile, a roster narrower than the manifest records) must be refused
//! before that reservation, so B stays resident. Each refusal is driven twice:
//! as a `daemon/rebuild` request (`handle_changes_with_macro_options`) and as
//! the watcher sends it (`handle_changes`, an empty request reusing the
//! record). Both must leave A `Loaded` with no recorded failure and no retry
//! count. The controls run a valid request under the same pressure and observe
//! B evicted, which is what makes every "B stays resident" assertion mean
//! something.

#![cfg(feature = "test-hooks")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use sqry_core::graph::unified::build::MacroOptionsRequest;
use sqry_core::graph::unified::memory::GraphMemorySize;
use sqry_core::graph::unified::persistence::{
    BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
};
use sqry_core::project::ProjectRootMode;
use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::workspace::builder::{FunctionGraphBuilder, graph_with_function_nodes};
use sqry_daemon::{
    DaemonConfig, DaemonError, ESTIMATE_STAGING_PER_FILE_BYTES, LoadedWorkspace, RebuildDispatcher,
    RosterRecord, WorkingSetInputs, WorkspaceKey, WorkspaceManager, WorkspaceRosterResolver,
    WorkspaceState, working_set_estimate,
};
use sqry_plugin_registry::RosterSource;
use tempfile::TempDir;

/// A forced (full) rebuild, the change set `daemon/rebuild --force` sends.
fn forced() -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    }
}

/// The ids the fast-path roster registers.
fn fast_ids() -> Vec<String> {
    sqry_plugin_registry::create_plugin_manager()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// The ids the full compiled roster registers.
fn all_ids() -> Vec<String> {
    sqry_plugin_registry::create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// Write a manifest at `root` recording `ids` as the plugin selection.
fn write_manifest(root: &Path, ids: Vec<String>, high_cost_mode: &str) {
    let storage = GraphStorage::new(root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test"),
    )
    .with_plugin_selection(Some(PluginSelectionManifest {
        active_plugin_ids: ids,
        high_cost_mode: Some(high_cost_mode.to_string()),
    }))
    .save(storage.manifest_path())
    .expect("manifest saved");
}

/// The working set the dispatcher estimates for a full rebuild of a
/// workspace whose resident graph is `graph`.
fn full_rebuild_estimate(graph: &sqry_core::graph::CodeGraph) -> u64 {
    working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: graph.heap_bytes() as u64,
        staging_overhead: (graph.files().len() as u64)
            .saturating_mul(ESTIMATE_STAGING_PER_FILE_BYTES),
        interner_snapshot_bytes: graph.strings().heap_bytes() as u64,
    })
}

/// Two resident workspaces under a budget that holds both but not A's
/// rebuild working set on top: A's reservation must evict B.
struct Pressure {
    dispatcher: Arc<RebuildDispatcher>,
    a_root: PathBuf,
    a_key: WorkspaceKey,
    ws_a: Arc<LoadedWorkspace>,
    ws_b: Arc<LoadedWorkspace>,
    b_bytes: usize,
    _a_dir: TempDir,
    _b_dir: TempDir,
}

impl Pressure {
    fn new(resolver: Arc<WorkspaceRosterResolver>) -> Self {
        let config = Arc::new(DaemonConfig {
            memory_limit_mb: 2,
            ..DaemonConfig::default()
        });
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let dispatcher =
            RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), resolver);
        let limit = config.memory_limit_bytes();

        // Both generations carry `nodes` function nodes, and A's estimate is
        // derived from its own resident graph. Pick `nodes` with
        // 2 * size <= limit < 2 * size + estimate.
        let mut nodes = 500_u32;
        loop {
            let graph = graph_with_function_nodes(nodes);
            let size = graph.heap_bytes() as u64;
            assert!(2 * size <= limit, "the fixture overshot at {nodes} nodes");
            if 2 * size + full_rebuild_estimate(&graph) > limit {
                break;
            }
            nodes += 500;
        }

        let a_dir = TempDir::new().expect("tempdir");
        let a_root = a_dir.path().canonicalize().expect("canonical root");
        std::fs::write(a_root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
        let a_key = WorkspaceKey::new(a_root.clone(), ProjectRootMode::GitRoot, 0);
        let b_dir = TempDir::new().expect("tempdir");
        let b_key = WorkspaceKey::new(
            b_dir.path().canonicalize().expect("canonical root"),
            ProjectRootMode::GitRoot,
            0,
        );
        let builder = FunctionGraphBuilder::with_fast_path_record(nodes);
        manager.get_or_load(&a_key, &builder, 1).expect("A loads");
        manager.get_or_load(&b_key, &builder, 1).expect("B loads");
        let ws_a = manager.lookup(&a_key).expect("A resident");
        let ws_b = manager.lookup(&b_key).expect("B resident");
        // A is the most recently used, so B is the LRU victim.
        ws_a.touch();
        let b_bytes = ws_b.memory_bytes.load(Ordering::Acquire);
        assert!(b_bytes > 0, "B holds bytes to reclaim");
        Self {
            dispatcher,
            a_root,
            a_key,
            ws_a,
            ws_b,
            b_bytes,
            _a_dir: a_dir,
            _b_dir: b_dir,
        }
    }

    fn production() -> Self {
        Self::new(Arc::new(WorkspaceRosterResolver::new()))
    }

    /// A `daemon/rebuild`-shaped request: forced, with `request`.
    async fn request(&self, request: MacroOptionsRequest) -> Result<(), DaemonError> {
        self.dispatcher
            .handle_changes_with_macro_options(&self.a_key, forced(), request)
            .await
            .wait()
            .await
            .map(|_| ())
    }

    /// A watcher-shaped dispatch: an empty request reusing the record.
    async fn watcher(&self) -> Result<(), DaemonError> {
        self.dispatcher.handle_changes(&self.a_key, forced()).await
    }

    /// A was refused with nothing written: `Loaded`, the same graph, no
    /// recorded failure, no retry; and B was not evicted. The request's
    /// outcome is delivered when its iteration ends, before the drain loop
    /// (on its own task) releases the runner role, so the release is
    /// awaited.
    async fn assert_refused_without_eviction(
        &self,
        label: &str,
        outcome: &Result<(), DaemonError>,
        graph_before: &Arc<sqry_core::graph::CodeGraph>,
    ) {
        println!(
            "{label}: outcome={outcome:?} A={} B={} B_bytes={}",
            self.ws_a.load_state(),
            self.ws_b.load_state(),
            self.ws_b.memory_bytes.load(Ordering::Acquire)
        );
        assert!(outcome.is_err(), "{label}: must be refused");
        assert_eq!(
            self.ws_b.load_state(),
            WorkspaceState::Loaded,
            "{label}: a request refused for its own input must not evict a sibling"
        );
        assert_eq!(
            self.ws_b.memory_bytes.load(Ordering::Acquire),
            self.b_bytes,
            "{label}: the sibling keeps its graph"
        );
        assert_eq!(
            self.ws_a.load_state(),
            WorkspaceState::Loaded,
            "{label}: a refusal returns the workspace to Loaded"
        );
        assert!(
            Arc::ptr_eq(graph_before, &self.ws_a.graph()),
            "{label}: a refusal publishes nothing"
        );
        assert!(
            self.ws_a.last_error.read().is_none(),
            "{label}: a refusal records no failure"
        );
        assert_eq!(
            self.ws_a.retry_count.load(Ordering::Acquire),
            0,
            "{label}: a refusal counts no failed attempt"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.ws_a.rebuild_in_flight.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "{label}: the runner role is released"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

/// The controls: a valid request, then a valid watcher-shaped dispatch,
/// under the same pressure both commit only by evicting B. Without these the
/// refusal tests' "B stays resident" would hold for a budget that never
/// needed B's bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_rebuild_under_the_same_pressure_evicts_the_sibling_control() {
    let fixture = Pressure::production();
    let outcome = fixture.request(MacroOptionsRequest::empty()).await;
    println!(
        "valid request: outcome={outcome:?} A={} B={}",
        fixture.ws_a.load_state(),
        fixture.ws_b.load_state()
    );
    outcome.expect("a valid request rebuilds");
    assert_eq!(fixture.ws_a.load_state(), WorkspaceState::Loaded);
    assert_eq!(
        fixture.ws_b.load_state(),
        WorkspaceState::Evicted,
        "the pressure is real: the valid rebuild evicts the sibling"
    );

    let fixture = Pressure::production();
    let outcome = fixture.watcher().await;
    outcome.expect("a valid watcher dispatch rebuilds");
    assert_eq!(
        fixture.ws_b.load_state(),
        WorkspaceState::Evicted,
        "the pressure is real on the watcher path too"
    );

    // An explicit request that resolves (a cfg flag) is not a refusal either.
    let fixture = Pressure::production();
    let outcome = fixture
        .request(MacroOptionsRequest {
            cfg_flags: Some(vec!["test".to_string()]),
            ..MacroOptionsRequest::empty()
        })
        .await;
    outcome.expect("a valid explicit request rebuilds");
    assert_eq!(fixture.ws_b.load_state(), WorkspaceState::Evicted);
}

/// A requested expand cache directory that does not exist, and a recorded
/// one that was removed (the watcher path), are refused with
/// `RebuildMacroOptionsUnavailable` before the reservation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_expand_cache_is_refused_before_the_reservation() {
    let fixture = Pressure::production();
    let before = fixture.ws_a.graph();
    let missing = fixture.a_root.join("no-such-expand-cache");
    let outcome = fixture
        .request(MacroOptionsRequest {
            expand_cache_dir: Some(missing),
            ..MacroOptionsRequest::empty()
        })
        .await;
    assert!(
        matches!(
            outcome,
            Err(DaemonError::RebuildMacroOptionsUnavailable { .. })
        ),
        "{outcome:?}"
    );
    fixture
        .assert_refused_without_eviction("requested missing cache", &outcome, &before)
        .await;

    // The watcher path: the manifest records a cache that no longer exists.
    let fixture = Pressure::production();
    let before = fixture.ws_a.graph();
    let cache = fixture.a_root.join("expand-cache");
    std::fs::create_dir_all(&cache).expect("cache dir");
    let storage = GraphStorage::new(&fixture.a_root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    let mut manifest = Manifest::new(
        fixture.a_root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test"),
    );
    manifest.macro_options = Some(
        sqry_core::graph::unified::persistence::MacroOptionsManifest {
            cfg_flags: Vec::new(),
            expand_cache_dir: Some(cache.to_string_lossy().to_string()),
        },
    );
    manifest.save(storage.manifest_path()).expect("manifest");
    std::fs::remove_dir_all(&cache).expect("remove the recorded cache");
    let outcome = fixture.watcher().await;
    assert!(
        matches!(
            outcome,
            Err(DaemonError::RebuildMacroOptionsUnavailable { .. })
        ),
        "{outcome:?}"
    );
    fixture
        .assert_refused_without_eviction("recorded missing cache", &outcome, &before)
        .await;
}

/// An empty expand cache directory is refused with `InvalidArgument`
/// (`-32602`) before the reservation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_expand_cache_is_refused_before_the_reservation() {
    let fixture = Pressure::production();
    let before = fixture.ws_a.graph();
    let outcome = fixture
        .request(MacroOptionsRequest {
            expand_cache_dir: Some(PathBuf::new()),
            ..MacroOptionsRequest::empty()
        })
        .await;
    match &outcome {
        Err(err @ DaemonError::InvalidArgument { reason }) => {
            assert_eq!(err.jsonrpc_code(), Some(-32602));
            assert!(reason.contains("empty"), "{reason}");
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    fixture
        .assert_refused_without_eviction("empty cache", &outcome, &before)
        .await;
}

/// An expand cache directory whose canonical path is not valid UTF-8, which
/// the manifest could not record, is refused with `InvalidArgument` before
/// the reservation.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrecordable_expand_cache_is_refused_before_the_reservation() {
    use std::os::unix::ffi::OsStringExt;

    let fixture = Pressure::production();
    let before = fixture.ws_a.graph();
    let mut name = b"expand-".to_vec();
    name.push(0xff);
    let dir = fixture.a_root.join(std::ffi::OsString::from_vec(name));
    std::fs::create_dir_all(&dir).expect("a non-UTF-8 directory");
    let outcome = fixture
        .request(MacroOptionsRequest {
            expand_cache_dir: Some(dir),
            ..MacroOptionsRequest::empty()
        })
        .await;
    match &outcome {
        Err(DaemonError::InvalidArgument { reason }) => {
            assert!(reason.contains("not valid UTF-8"), "{reason}");
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    fixture
        .assert_refused_without_eviction("unrecordable cache", &outcome, &before)
        .await;
}

/// An unreadable manifest is refused with `WorkspaceManifestUnreadable`
/// before the reservation, on both paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_manifest_is_refused_before_the_reservation() {
    for watcher in [false, true] {
        let fixture = Pressure::production();
        let before = fixture.ws_a.graph();
        let storage = GraphStorage::new(&fixture.a_root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"{ not json").expect("corrupt manifest");
        let outcome = if watcher {
            fixture.watcher().await
        } else {
            fixture.request(MacroOptionsRequest::empty()).await
        };
        assert!(
            matches!(
                outcome,
                Err(DaemonError::WorkspaceManifestUnreadable { .. })
            ),
            "{outcome:?}"
        );
        fixture
            .assert_refused_without_eviction(
                if watcher {
                    "unreadable manifest (watcher)"
                } else {
                    "unreadable manifest (request)"
                },
                &outcome,
                &before,
            )
            .await;
    }
}

/// A readable manifest naming a plugin id this binary did not compile is
/// refused with `WorkspaceIncompatibleGraph` (`-32005`) before the
/// reservation, and the workspace returns to `Loaded`: before this repair
/// `daemon/rebuild` recorded a failure and left it `Failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncompiled_plugin_id_is_refused_before_the_reservation() {
    for watcher in [false, true] {
        let fixture = Pressure::production();
        let before = fixture.ws_a.graph();
        let mut ids = fast_ids();
        ids.push("rebuild-refusal-planted-plugin".to_string());
        write_manifest(&fixture.a_root, ids, "fast_path_default");
        let outcome = if watcher {
            fixture.watcher().await
        } else {
            fixture.request(MacroOptionsRequest::empty()).await
        };
        match &outcome {
            Err(err @ DaemonError::WorkspaceIncompatibleGraph { reason, .. }) => {
                assert_eq!(err.jsonrpc_code(), Some(-32005));
                assert!(
                    reason.contains("rebuild-refusal-planted-plugin"),
                    "{reason}"
                );
            }
            other => panic!("expected WorkspaceIncompatibleGraph, got {other:?}"),
        }
        fixture
            .assert_refused_without_eviction(
                if watcher {
                    "uncompiled id (watcher)"
                } else {
                    "uncompiled id (request)"
                },
                &outcome,
                &before,
            )
            .await;
    }
}

/// A roster narrower than the manifest records (a resolver pinned to the
/// fast path over an `include_all` manifest) is refused with
/// `RebuildWouldNarrowSelection` before the reservation, not after a build
/// that evicted a sibling to make room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_roster_is_refused_before_the_reservation() {
    for watcher in [false, true] {
        let pinned_fast = Arc::new(WorkspaceRosterResolver::pinned(
            Arc::new(sqry_plugin_registry::create_plugin_manager()),
            RosterRecord::from_manager(
                &sqry_plugin_registry::create_plugin_manager(),
                RosterSource::Fallback,
            ),
        ));
        let fixture = Pressure::new(pinned_fast);
        let before = fixture.ws_a.graph();
        write_manifest(&fixture.a_root, all_ids(), "include_all");
        let outcome = if watcher {
            fixture.watcher().await
        } else {
            fixture.request(MacroOptionsRequest::empty()).await
        };
        assert!(
            matches!(
                outcome,
                Err(DaemonError::RebuildWouldNarrowSelection { .. })
            ),
            "{outcome:?}"
        );
        fixture
            .assert_refused_without_eviction(
                if watcher {
                    "narrowing (watcher)"
                } else {
                    "narrowing (request)"
                },
                &outcome,
                &before,
            )
            .await;
    }
}

/// A budget that cannot admit the working set even after eviction refuses
/// the rebuild with `MemoryBudgetExceeded` (`-32003`). Nothing has been
/// written when the reservation fails, so the workspace returns to `Loaded`
/// with no recorded failure: before this repair it was left `Failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_budget_that_cannot_admit_the_rebuild_is_a_refusal() {
    for watcher in [false, true] {
        let config = Arc::new(DaemonConfig {
            memory_limit_mb: 1,
            ..DaemonConfig::default()
        });
        let limit = config.memory_limit_bytes();
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let dispatcher = RebuildDispatcher::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::new(WorkspaceRosterResolver::new()),
        );
        // The one resident workspace fits; its rebuild working set on top
        // does not, and there is nothing else to evict.
        let mut nodes = 200_u32;
        loop {
            let graph = graph_with_function_nodes(nodes);
            let size = graph.heap_bytes() as u64;
            assert!(size <= limit, "the fixture overshot at {nodes} nodes");
            if size + full_rebuild_estimate(&graph) > limit {
                break;
            }
            nodes += 200;
        }
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
        let key = WorkspaceKey::new(root, ProjectRootMode::GitRoot, 0);
        manager
            .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(nodes), 1)
            .expect("the workspace loads");
        let ws = manager.lookup(&key).expect("resident");
        let before = ws.graph();
        let outcome = if watcher {
            dispatcher.handle_changes(&key, forced()).await
        } else {
            dispatcher
                .handle_changes_with_macro_options(&key, forced(), MacroOptionsRequest::empty())
                .await
                .wait()
                .await
                .map(|_| ())
        };
        println!(
            "memory budget (watcher={watcher}): outcome={outcome:?} state={}",
            ws.load_state()
        );
        match &outcome {
            Err(err @ DaemonError::MemoryBudgetExceeded { .. }) => {
                assert_eq!(err.jsonrpc_code(), Some(-32003));
            }
            other => panic!("expected MemoryBudgetExceeded, got {other:?}"),
        }
        assert_eq!(ws.load_state(), WorkspaceState::Loaded);
        assert!(Arc::ptr_eq(&before, &ws.graph()));
        assert!(ws.last_error.read().is_none(), "no recorded failure");
        assert_eq!(ws.retry_count.load(Ordering::Acquire), 0, "no backoff");
    }
}

// ---------------------------------------------------------------------------
// The load sites: `WorkspaceManager::get_or_load` (behind `daemon/load`, the
// pinned preload and the daemon-hosted `rebuild_index` load route) prepares
// the build before it reserves, so a load refused for its own input evicts no
// sibling either. The acquirer's read-only reload is covered by the
// manager's own unit tests (`reload_from_disk_read_only` is crate-private).
// ---------------------------------------------------------------------------

/// A resident sibling B sized so that B plus the `daemon/load` estimate does
/// not fit, while the estimate alone does: loading another workspace C must
/// evict B to commit. Returns the manager, B's workspace and the estimate.
fn load_pressure() -> (Arc<WorkspaceManager>, Arc<LoadedWorkspace>, u64, TempDir) {
    let config = Arc::new(DaemonConfig {
        memory_limit_mb: 4,
        ..DaemonConfig::default()
    });
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let limit = config.memory_limit_bytes();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let load_estimate = ((2 * 1024 * 1024) as f64 * sqry_daemon::WORKING_SET_MULTIPLIER) as u64;
    assert!(load_estimate <= limit, "the estimate alone fits");
    let mut nodes = 500_u32;
    loop {
        let size = graph_with_function_nodes(nodes).heap_bytes() as u64;
        if size + load_estimate > limit {
            break;
        }
        nodes += 500;
    }
    let b_dir = TempDir::new().expect("tempdir");
    let b_key = WorkspaceKey::new(
        b_dir.path().canonicalize().expect("canonical"),
        ProjectRootMode::GitRoot,
        0,
    );
    manager
        .get_or_load(
            &b_key,
            &FunctionGraphBuilder::with_fast_path_record(nodes),
            1,
        )
        .expect("B loads");
    let ws_b = manager.lookup(&b_key).expect("B resident");
    (manager, ws_b, load_estimate, b_dir)
}

/// The production builder's durable preparation, as the daemon-hosted
/// `rebuild_index` load route uses it (`prepare_durable_build`): the loader
/// calls `prepare_build`, which this wrapper routes to it.
#[derive(Debug)]
struct DurableLoad(sqry_daemon::RealWorkspaceBuilder);

impl sqry_daemon::WorkspaceBuilder for DurableLoad {
    fn build(&self, root: &Path) -> Result<sqry_daemon::workspace::BuiltGraph, DaemonError> {
        self.prepare_build(root)?()
    }

    fn prepare_build<'a>(
        &'a self,
        root: &Path,
    ) -> Result<sqry_daemon::workspace::builder::PreparedBuild<'a>, DaemonError> {
        self.0.prepare_durable_build(
            root,
            &sqry_core::graph::unified::build::MacroOptionsRequest::empty(),
        )
    }
}

/// The auditor's experiment for the load sites: a `daemon/load`-shaped load
/// of C (the production builder, the handler's estimate) refused for C's own
/// manifest (unreadable, naming an uncompiled id, recording an expand cache
/// that no longer exists) leaves B resident; so does the same load through
/// the durable preparation the `rebuild_index` load route uses, for the
/// inputs it refuses (round 7, plant P25: resolving its inputs inside the
/// returned build would refuse after the reservation had evicted B). The
/// on-disk refusals are recorded on C; the request refusal (the recorded
/// expand cache) leaves no slot for C (B2, round 7 audit). The control
/// loads a valid C under the same pressure and evicts B.
#[test]
fn a_load_refused_for_its_own_input_evicts_no_sibling() {
    type Plant = fn(&Path);
    let unreadable: Plant = |root| {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"{ not json").expect("corrupt manifest");
    };
    let uncompiled: Plant = |root| {
        let mut ids = fast_ids();
        ids.push("load-refusal-planted-plugin".to_string());
        write_manifest(root, ids, "fast_path_default");
    };
    let missing_cache: Plant = |root| {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        let mut manifest = Manifest::new(
            root.to_string_lossy().to_string(),
            1,
            1,
            "fixture-sha256",
            BuildProvenance::new("test", "test"),
        )
        .with_plugin_selection(Some(PluginSelectionManifest {
            active_plugin_ids: fast_ids(),
            high_cost_mode: Some("fast_path_default".to_string()),
        }));
        manifest.macro_options = Some(
            sqry_core::graph::unified::persistence::MacroOptionsManifest {
                cfg_flags: Vec::new(),
                expand_cache_dir: Some(root.join("removed-cache").to_string_lossy().to_string()),
            },
        );
        manifest.save(storage.manifest_path()).expect("manifest");
    };
    let cases: [(&str, Plant); 3] = [
        ("unreadable manifest", unreadable),
        ("uncompiled plugin id", uncompiled),
        ("recorded expand cache removed", missing_cache),
    ];
    // The durable preparation falls back over an unreadable manifest
    // (decision D-i7-8), so that case is not a refusal on it.
    for ((label, plant), durable) in cases
        .into_iter()
        .flat_map(|case| [(case, false), (case, true)])
        .filter(|((label, _), durable)| !(*durable && *label == "unreadable manifest"))
    {
        let (manager, ws_b, load_estimate, _b_dir) = load_pressure();
        let c_dir = TempDir::new().expect("tempdir");
        let c_root = c_dir.path().canonicalize().expect("canonical");
        std::fs::write(c_root.join("lib.rs"), b"pub fn c() {}\n").expect("source");
        plant(&c_root);
        let c_key = WorkspaceKey::new(c_root, ProjectRootMode::default(), 0);
        let real = sqry_daemon::RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
        let refused = if durable {
            manager.get_or_load(&c_key, &DurableLoad(real), load_estimate)
        } else {
            manager.get_or_load(&c_key, &real, load_estimate)
        };
        let label = format!("{label} (durable={durable})");
        println!("{label}: refused={refused:?} B={}", ws_b.load_state());
        assert!(refused.is_err(), "{label}: C must be refused");
        assert_eq!(
            ws_b.load_state(),
            WorkspaceState::Loaded,
            "{label}: a load refused for its own input must not evict a sibling"
        );
        // A refusal of the on-disk index is recorded on C. A refusal of the
        // request's inputs (the recorded expand cache that is gone) leaves
        // no slot: the index is readable, and a slot carrying the refusal
        // would break every later query of it (B2, round 7 audit).
        let request_refusal = label.starts_with("recorded expand cache removed");
        match manager.lookup(&c_key) {
            Some(ws_c) if !request_refusal => {
                assert_eq!(
                    ws_c.load_state(),
                    WorkspaceState::Failed,
                    "{label}: the refused load is recorded on C"
                );
                assert!(ws_c.last_error.read().is_some(), "{label}");
            }
            None if request_refusal => {}
            other => panic!(
                "{label}: request_refusal={request_refusal}, C's slot={:?}",
                other.map(|ws| ws.load_state())
            ),
        }
    }

    // Control: a valid C loads under the same pressure and evicts B.
    let (manager, ws_b, load_estimate, _b_dir) = load_pressure();
    let c_dir = TempDir::new().expect("tempdir");
    let c_root = c_dir.path().canonicalize().expect("canonical");
    std::fs::write(c_root.join("lib.rs"), b"pub fn c() {}\n").expect("source");
    let c_key = WorkspaceKey::new(c_root, ProjectRootMode::default(), 0);
    let builder = sqry_daemon::RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    manager
        .get_or_load(&c_key, &builder, load_estimate)
        .expect("a valid load succeeds");
    assert_eq!(
        ws_b.load_state(),
        WorkspaceState::Evicted,
        "the pressure is real: the valid load evicts the sibling"
    );
}
