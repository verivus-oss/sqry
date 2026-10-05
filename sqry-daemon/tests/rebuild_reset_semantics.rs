//! `daemon/reset` and the rebuild runner (integration round 7, decision
//! D-i7-3; audit B1's reset legs, S1's E9 and S6).
//!
//! A workspace whose rebuild is in flight is not reset: "in flight" is a
//! runner holding the runner role, read under the rebuild lane, not the
//! `Rebuilding` state. The runner is cancelled and the call answers
//! `-32009` for the caller to retry. Before the repair `reset` keyed on
//! the state and set the cancellation flag without the lane, so:
//!
//! - a workspace whose runner was between iterations (the first published,
//!   a second request parked) was reset, and the parked request then
//!   rebuilt it, `Loaded` with its watcher stopped (E9, 50 of 50 runs);
//! - a `Rebuilding` workspace with no runner kept the flag with nothing to
//!   consume it, and the reset never completed (E7).
//!
//! A stopped watcher's own change set is also refused under the lane, so a
//! watcher that received changes just before a reset cannot rebuild the
//! reset workspace back to `Loaded`.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_core::watch::{ChangeSet, GitChangeClass, LastIndexedGitState};
use sqry_daemon::workspace::builder::FunctionGraphBuilder;
use sqry_daemon::{DaemonConfig, DaemonError, TestCapture, WorkspaceKey, WorkspaceState};
use support::ipc::{TestIpcClient, TestServer, expect_success};
use support::rebuild_fixtures::{
    err_of, index_fast_path, ipc_client, ipc_rebuild, mixed_workspace, real_server, status_row,
    wait_until_blocking,
};

fn forced() -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    }
}

fn git_state() -> LastIndexedGitState {
    LastIndexedGitState {
        head_ref: Some("refs/heads/main".to_string()),
        head_commit_oid: None,
        head_tree_oid: None,
    }
}

/// E9: a reset that lands while the runner is between iterations (its
/// first iteration published, a second request parked) dispatches a
/// cancellation and resets nothing; the parked request is answered
/// `-32004` and never runs; the workspace stays `Loaded` and watched. The
/// retried reset then resets it, and it stays `Unloaded` and unwatched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_between_two_iterations_cancels_the_parked_request_and_resets_on_retry() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    capture.arm_post_publish_hold();

    let spawn_rebuild = |server: &TestServer| {
        let sock = server.path.clone();
        let root = root.clone();
        tokio::spawn(async move {
            let mut c = TestIpcClient::connect(&sock).await;
            c.hello(1).await;
            ipc_rebuild(&mut c, &root, true, &[]).await
        })
    };
    let first = spawn_rebuild(&server);
    tokio::time::timeout(Duration::from_secs(60), capture.wait_until_post_publish())
        .await
        .expect("the first iteration publishes");
    let ws = server.manager.lookup(&key).expect("resident");
    let second = spawn_rebuild(&server);
    let parked = {
        let ws = Arc::clone(&ws);
        tokio::task::spawn_blocking(move || {
            wait_until_blocking(Duration::from_secs(30), || {
                ws.rebuild_lane.try_lock().is_ok_and(|lane| lane.is_some())
            })
        })
        .await
        .expect("join")
    };
    assert!(parked, "the second request parks behind the runner");

    let state_at_reset = ws.load_state();
    let reset = client
        .request("daemon/reset", json!({ "path": root.to_string_lossy() }))
        .await;
    let reset_code = err_of(&reset).map(|e| e.code);
    capture.release_post_publish();
    let first = first.await.expect("join");
    let second = second.await.expect("join");
    let released = {
        let ws = Arc::clone(&ws);
        tokio::task::spawn_blocking(move || {
            wait_until_blocking(Duration::from_secs(30), || {
                !ws.rebuild_in_flight.load(Ordering::Acquire)
            })
        })
        .await
        .expect("join")
    };
    let row = status_row(&mut client, &root).await;
    let iterations = capture.iterations.lock().len();
    println!(
        "E9 reset between iterations: state at reset={state_at_reset:?} reset={reset_code:?} \
         first={:?} second={:?} released={released} after: state={} watching={} iterations={iterations}",
        err_of(&first).map(|e| e.code),
        err_of(&second).map(|e| e.code),
        row["state"],
        row["watching"],
    );
    assert_eq!(
        reset_code,
        Some(-32009),
        "a runner holds the role, so the reset is a cancellation: {:?}",
        err_of(&reset)
    );
    assert!(
        err_of(&first).is_none(),
        "the published iteration keeps its result"
    );
    assert_eq!(
        err_of(&second).map(|e| e.code),
        Some(-32004),
        "the parked request is cancelled, not run"
    );
    assert_eq!(iterations, 1, "the parked request never ran an iteration");
    assert!(released);
    assert_eq!(row["state"], json!("Loaded"), "nothing was reset: {row}");
    assert_eq!(
        row["watching"],
        json!(true),
        "nothing stopped the watcher: {row}"
    );
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire));

    let retried = client
        .request("daemon/reset", json!({ "path": root.to_string_lossy() }))
        .await;
    let row = status_row(&mut client, &root).await;
    println!(
        "E9 retried reset: error={:?} state={} watching={}",
        err_of(&retried).map(|e| e.code),
        row["state"],
        row["watching"]
    );
    assert_eq!(expect_success(&retried)["result"]["reset"], json!(true));
    assert_eq!(row["state"], json!("Unloaded"));
    assert_eq!(row["watching"], json!(false));
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire));
    drop(client);
    server.stop().await;
}

/// E7: a `Rebuilding` workspace with no runner (the state the B1 defect
/// left behind) is reset, not cancelled: no flag is left with nothing to
/// consume it, and the workspace loads and rebuilds afterwards. Before the
/// repair the reset answered `-32009` forever and left the flag set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuilding_workspace_with_no_runner_is_reset_and_leaves_no_cancellation() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Rebuilding);
    let ws = server.manager.lookup(&key).expect("resident");
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire), "no runner");

    let reset = server.manager.reset(&key, false);
    println!(
        "E7 reset of a Rebuilding workspace with no runner: {reset:?} state={:?} flag={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire)
    );
    assert!(matches!(reset, Ok(true)), "{reset:?}");
    assert_eq!(ws.load_state(), WorkspaceState::Unloaded);
    assert!(
        !ws.rebuild_cancelled.load(Ordering::Acquire),
        "no cancellation is left with no runner to consume it"
    );
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(3), 1)
        .expect("the reset workspace loads");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    server.stop().await;
}

/// A change set from a watcher whose stop signal is set starts no rebuild:
/// the enqueue is refused under the lane, nothing runs and nothing parks.
/// The control: the same enqueue with the signal clear runs one iteration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_watchers_change_set_starts_no_rebuild() {
    let server = TestServer::new().await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(3), 1)
        .expect("loads");
    let ws = server.manager.lookup(&key).expect("resident");

    let stopped = AtomicBool::new(true);
    let refused = server
        .dispatcher
        .handle_changes_with_git_state(&key, forced(), git_state(), &stopped)
        .await;
    let ran_after_stop = capture.iterations.lock().len();
    let parked_after_stop = ws.rebuild_lane.try_lock().is_ok_and(|lane| lane.is_some());
    let clear = AtomicBool::new(false);
    let control = server
        .dispatcher
        .handle_changes_with_git_state(&key, forced(), git_state(), &clear)
        .await;
    let ran_after_control = capture.iterations.lock().len();
    println!(
        "stopped watcher enqueue: refused={refused:?} iterations={ran_after_stop} \
         parked={parked_after_stop}; control={control:?} iterations={ran_after_control}"
    );
    assert!(
        matches!(refused, Err(DaemonError::WorkspaceEvicted { .. })),
        "{refused:?}"
    );
    assert_eq!(ran_after_stop, 0, "nothing ran");
    assert!(!parked_after_stop, "nothing parked");
    assert!(
        control.is_ok() || matches!(control, Err(DaemonError::WorkspaceBuildFailed { .. })),
        "the control reaches the pipeline: {control:?}"
    );
    assert_eq!(ran_after_control, 1, "the control runs one iteration");
    server.stop().await;
}

/// Audit S2 (D2F): a `daemon/rebuild` whose handler passed its serving
/// check just before a `daemon/reset` landed reaches the dispatcher with
/// a reset (`Unloaded`) workspace. The enqueue repeats the serving check
/// under the lane and refuses it (`-32004`), so the reset is not undone.
/// The plain enqueue (no serving check, as a direct caller makes it)
/// reaches an iteration, which refuses to enter from `Unloaded` over the
/// placeholder. Before the repair both rebuilt the workspace to `Loaded`
/// with its watcher stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_reaching_a_reset_workspace_leaves_it_reset() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    support::init_git_repo(&root);
    std::fs::write(root.join("lib.rs"), b"pub fn a() {}\n").expect("write");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
        .expect("loads");
    server.dispatcher.start_watching(&key);
    assert!(server.manager.reset(&key, false).expect("resets"));
    let ws = server.manager.lookup(&key).expect("entry");

    let outcome = server
        .dispatcher
        .handle_changes_with_macro_options(
            &key,
            forced(),
            sqry_core::graph::unified::build::MacroOptionsRequest::empty(),
        )
        .await;
    let explicit = tokio::time::timeout(Duration::from_secs(60), outcome.wait())
        .await
        .expect("the outcome arrives");
    let after_explicit = ws.load_state();
    let plain = server.dispatcher.handle_changes(&key, forced()).await;
    let after_plain = ws.load_state();
    println!(
        "D2F explicit={:?} state={after_explicit:?}; plain={:?} state={after_plain:?} watched={}",
        explicit.as_ref().map(|_| ()),
        plain,
        server.dispatcher.live_watcher_keys().contains(&key)
    );
    assert!(
        matches!(explicit, Err(DaemonError::WorkspaceNotLoaded { .. })),
        "the explicit rebuild of a reset workspace is refused: {:?}",
        explicit.map(|_| ())
    );
    assert_eq!(after_explicit, WorkspaceState::Unloaded);
    assert!(
        matches!(plain, Err(DaemonError::WorkspaceNotLoaded { .. })),
        "an iteration does not enter from a reset workspace: {plain:?}"
    );
    assert_eq!(after_plain, WorkspaceState::Unloaded);
    server.stop().await;
}

/// Audit S2 (plant L11): the watcher's stop signal is read under the
/// rebuild lane, not before it. The test holds the lane, starts a
/// watcher's enqueue with a clear signal (it waits for the lane), sets the
/// signal as a reset would while it holds the lane, and releases it: the
/// enqueue is refused and nothing runs. Read before the lane, the clear
/// signal let the change set rebuild the workspace the reset meant to
/// stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_signal_set_while_the_enqueue_waits_for_the_lane_refuses_it() {
    let server = TestServer::new().await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(3), 1)
        .expect("loads");
    let ws = server.manager.lookup(&key).expect("resident");

    let lane = ws.rebuild_lane.lock().await;
    let stop = Arc::new(AtomicBool::new(false));
    let enqueue = {
        let dispatcher = Arc::clone(&server.dispatcher);
        let key = key.clone();
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            dispatcher
                .handle_changes_with_git_state(&key, forced(), git_state(), &stop)
                .await
        })
    };
    // Let the enqueue reach the lane and wait on it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !enqueue.is_finished(),
        "the enqueue waits for the held lane"
    );
    stop.store(true, Ordering::Release);
    drop(lane);
    let refused = enqueue.await.expect("join");
    let ran = capture.iterations.lock().len();
    println!("L11 enqueue={refused:?} iterations={ran}");
    assert!(
        matches!(refused, Err(DaemonError::WorkspaceEvicted { .. })),
        "a signal set before the lane is granted refuses the enqueue: {refused:?}"
    );
    assert_eq!(ran, 0, "nothing ran");
    server.stop().await;
}

/// Audit S2 (D2E): a `daemon/load` that fails leaves the slot `Failed`;
/// the `daemon/rebuild` that then makes the workspace resident must leave
/// it watched, because the load asked for it to be watched. Before the
/// repair the failed load recorded no intent and the rebuilt workspace
/// was left unwatched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_of_a_failed_load_is_watched() {
    #[derive(Debug)]
    struct Failing;
    impl sqry_daemon::WorkspaceBuilder for Failing {
        fn build(
            &self,
            root: &std::path::Path,
        ) -> Result<sqry_daemon::workspace::BuiltGraph, DaemonError> {
            Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: "injected load failure".into(),
            })
        }
    }
    let server = TestServer::with_builder(Arc::new(Failing)).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    support::init_git_repo(&root);
    std::fs::write(root.join("lib.rs"), b"pub fn a() {}\n").expect("write");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let load = client
        .request(
            "daemon/load",
            json!({ "index_root": root.to_string_lossy() }),
        )
        .await;
    assert!(err_of(&load).is_some(), "the load fails");
    let ws = server.manager.lookup(&key).expect("the failed load's slot");
    assert_eq!(ws.load_state(), WorkspaceState::Failed);
    let rebuild = client
        .request(
            "daemon/rebuild",
            json!({ "path": root.to_string_lossy(), "force": true }),
        )
        .await;
    expect_success(&rebuild);
    let watched = server.dispatcher.live_watcher_keys().contains(&key);
    println!(
        "D2E state={:?} watched={watched} watch_wanted={}",
        ws.load_state(),
        ws.watch_wanted.load(Ordering::Acquire)
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(watched, "the workspace the load wanted watched is watched");
    drop(client);
    server.stop().await;
}

/// Audit S2: the explicit callers' serving check is repeated under the
/// rebuild lane, atomic with the enqueue. A workspace that is not serving
/// when the enqueue takes the lane (here `Unloaded` with a graph, as a
/// cancelled rebuild leaves it, so the iteration's own entry check would
/// let it in) is refused: `daemon/rebuild` with `-32004 WorkspaceNotLoaded`
/// (its handler's own answer for that state), `rebuild_index` with
/// `WorkspaceEvicted`. Nothing runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_rebuild_of_a_workspace_not_serving_is_refused_under_the_lane() {
    let server = TestServer::new().await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
    let empty = sqry_core::graph::unified::build::MacroOptionsRequest::empty;
    let rebuild = server
        .dispatcher
        .handle_changes_with_macro_options(&key, forced(), empty())
        .await
        .wait()
        .await;
    let rebuild_index = server
        .dispatcher
        .handle_changes_for_rebuild_index(&key, forced(), empty())
        .await
        .wait()
        .await;
    let ran = capture.iterations.lock().len();
    println!(
        "not serving: daemon/rebuild={:?} rebuild_index={:?} iterations={ran}",
        rebuild.as_ref().map(|_| ()),
        rebuild_index.as_ref().map(|_| ())
    );
    assert!(
        matches!(rebuild, Err(DaemonError::WorkspaceNotLoaded { .. })),
        "{:?}",
        rebuild.map(|_| ())
    );
    assert!(
        matches!(rebuild_index, Err(DaemonError::WorkspaceEvicted { .. })),
        "{:?}",
        rebuild_index.map(|_| ())
    );
    assert_eq!(ran, 0, "nothing ran");
    server.stop().await;
}

/// Audit S7 (plant R03): `reset` retries a lane it cannot take, for up to
/// `RESET_LANE_WAIT`, before it answers `-32009` with nothing dispatched.
/// A lane held briefly (as a lane holder's few instructions hold it) is
/// waited out and the reset completes; a lane held past the bound is
/// answered `ResetCancellationDispatched`, and no cancellation is left
/// behind. Giving up at once answered `-32009` for the brief hold too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_waits_out_a_briefly_held_lane_and_gives_up_after_its_bound() {
    use sqry_daemon::workspace::manager::RESET_LANE_WAIT;
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let builder = FunctionGraphBuilder::with_fast_path_record(3);

    // A brief hold: the reset waits and completes.
    server
        .manager
        .get_or_load(&key, &builder, 1)
        .expect("loads");
    let ws = server.manager.lookup(&key).expect("resident");
    let lane = ws.rebuild_lane.lock().await;
    let reset = {
        let manager = Arc::clone(&server.manager);
        let key = key.clone();
        tokio::task::spawn_blocking(move || manager.reset(&key, false))
    };
    tokio::time::sleep(RESET_LANE_WAIT / 5).await;
    assert!(!reset.is_finished(), "the reset waits for the held lane");
    drop(lane);
    let brief = reset.await.expect("join");
    println!("brief hold: reset={brief:?} state={:?}", ws.load_state());
    assert!(matches!(brief, Ok(true)), "{brief:?}");
    assert_eq!(ws.load_state(), WorkspaceState::Unloaded);

    // A hold past the bound: -32009, nothing dispatched.
    server
        .manager
        .get_or_load(&key, &builder, 1)
        .expect("reloads");
    let lane = ws.rebuild_lane.lock().await;
    let reset = {
        let manager = Arc::clone(&server.manager);
        let key = key.clone();
        tokio::task::spawn_blocking(move || manager.reset(&key, false))
    };
    let long = reset.await.expect("join");
    drop(lane);
    println!(
        "long hold: reset={long:?} state={:?} cancelled={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire)
    );
    assert!(
        matches!(long, Err(DaemonError::ResetCancellationDispatched { .. })),
        "{long:?}"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded, "nothing was reset");
    assert!(
        !ws.rebuild_cancelled.load(Ordering::Acquire),
        "no cancellation is left behind"
    );
    server.stop().await;
}
