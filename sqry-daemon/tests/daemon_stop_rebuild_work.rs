//! `daemon/stop` stops the daemon's rebuild work (integration round 7, S6,
//! issue #902).
//!
//! Before the repair nothing set a file watcher's stop signal on shutdown.
//! The watcher's blocking loop polls only that signal, so it kept running
//! after the IPC server returned, and the runtime's drop at process exit
//! waited for it: a stopped daemon stayed alive until the next file event
//! in a watched tree woke the loop (measured: still alive 10 s after
//! `sqry daemon stop`, gone 12.2 s after a watched file was touched). A
//! rebuild in flight was not cancelled either, so its caller was left
//! waiting through the drain.
//!
//! The in-process tests drive the IPC server's shutdown through
//! `TestServer::stop`, which returns when `IpcServer::run` does; the last
//! test runs the `sqryd` binary itself, the shape issue #902 reported.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{DaemonConfig, TestCapture, WorkspaceKey, WorkspaceState};
use support::ipc::{TestIpcClient, TestServer, expect_success};
use support::rebuild_fixtures::{
    err_of, index_fast_path, ipc_client, ipc_rebuild, mixed_workspace, real_server, status_row,
};

/// After `daemon/stop` the server returns only once every file watcher has
/// exited and been reaped. Before the repair the watcher was still
/// registered and running when the server returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_stop_stops_every_file_watcher() {
    let server = TestServer::new().await;
    let mut roots = Vec::new();
    let mut dirs = Vec::new();
    for _ in 0..2 {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = canonicalize_path(dir.path()).expect("canonical root");
        support::init_git_repo(&root);
        roots.push(root);
        dirs.push(dir);
    }
    let mut client = ipc_client(&server).await;
    for root in &roots {
        expect_success(
            &client
                .request(
                    "daemon/load",
                    json!({ "index_root": root.to_string_lossy() }),
                )
                .await,
        );
        assert_eq!(
            status_row(&mut client, root).await["watching"],
            json!(true),
            "a loaded git workspace is watched"
        );
    }
    let watchers_before = server.dispatcher.watchers_len();
    expect_success(&client.request("daemon/stop", json!({})).await);
    drop(client);

    let dispatcher = Arc::clone(&server.dispatcher);
    let manager = Arc::clone(&server.manager);
    let started = Instant::now();
    server.stop().await;
    let stop_took = started.elapsed();
    let watchers_after = dispatcher.watchers_len();
    let live_after = dispatcher.live_watcher_keys().len();
    println!(
        "S6 daemon/stop: watchers before={watchers_before} after={watchers_after} \
         live after={live_after} stop took {stop_took:?}"
    );
    // Cleanup before the assertions: on the pre-repair head the watchers
    // are still running here, and the test runtime's drop would wait on
    // their blocking loops without bound.
    for root in &roots {
        if let Some(ws) = manager.lookup(&WorkspaceKey::new(
            root.clone(),
            ProjectRootMode::GitRoot,
            0,
        )) {
            ws.stop_watcher();
        }
    }
    assert_eq!(watchers_before, 2, "two watchers were running");
    assert_eq!(
        watchers_after, 0,
        "every watcher must have exited when the server returns"
    );
    assert_eq!(live_after, 0);
}

/// A rebuild in flight when the daemon is stopped is cancelled: its caller
/// is answered `-32004` during the drain and the workspace is not left
/// `Rebuilding`. The iteration is held after its reservation (a test hook)
/// while `daemon/stop` runs, then released into a cancellation it observes
/// at its first pass boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_stop_cancels_a_rebuild_in_flight() {
    let (server, builder) = real_server(DaemonConfig::default()).await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    capture.arm_post_reservation_hold();
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .get_or_load(&key, builder.as_ref(), 1)
        .expect("the workspace loads");
    let ws = server.manager.lookup(&key).expect("resident");

    let sock = server.path.clone();
    let rebuild_root = root.clone();
    let rebuild = tokio::spawn(async move {
        let mut client = TestIpcClient::connect(&sock).await;
        client.hello(1).await;
        ipc_rebuild(&mut client, &rebuild_root, true, &[]).await
    });
    tokio::time::timeout(
        Duration::from_secs(30),
        capture.wait_until_post_reservation(),
    )
    .await
    .expect("the rebuild reaches its reservation");

    let shutdown = server.shutdown.clone();
    let stopping = tokio::spawn(server.stop());
    shutdown.cancel();
    // Wait until the shutdown has set the cancellation, then let the held
    // iteration run into it.
    let ws_wait = Arc::clone(&ws);
    let cancelled = tokio::task::spawn_blocking(move || {
        support::rebuild_fixtures::wait_until_blocking(Duration::from_secs(10), || {
            ws_wait.rebuild_cancelled.load(Ordering::Acquire)
        })
    })
    .await
    .expect("join");
    capture.release_post_reservation();
    let answered = tokio::time::timeout(Duration::from_secs(60), rebuild)
        .await
        .expect("the rebuild caller is answered")
        .expect("join");
    tokio::time::timeout(Duration::from_secs(60), stopping)
        .await
        .expect("the server stops")
        .expect("join");
    let code = err_of(&answered).map(|e| e.code);
    println!(
        "S6 daemon/stop during a rebuild: cancellation set={cancelled} answer={code:?} state={:?}",
        ws.load_state()
    );
    assert!(cancelled, "shutdown must cancel the rebuild in flight");
    assert_eq!(
        code,
        Some(-32004),
        "the cancelled rebuild's caller is told so"
    );
    assert_ne!(
        ws.load_state(),
        WorkspaceState::Rebuilding,
        "no Rebuilding is left behind"
    );
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "the runner role is released"
    );
}

/// Issue #902 itself: `sqryd` with a watched workspace exits promptly
/// after `daemon/stop`, with nothing touching the watched tree. Before the
/// repair the process stayed alive until a file event woke the watcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqryd_exits_promptly_after_daemon_stop_with_a_watched_workspace() {
    let Some(binary) = sqryd_binary() else {
        panic!("the sqryd binary must be built beside this test (CARGO_BIN_EXE_sqryd)");
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sqryd-stop.sock");
    let config = tmp.path().join("daemon.toml");
    std::fs::write(
        &config,
        format!("[socket]\npath = {:?}\n", socket.to_string_lossy().as_ref()),
    )
    .expect("write config");
    let workspace = tempfile::tempdir().expect("workspace");
    let root = canonicalize_path(workspace.path()).expect("canonical root");
    support::init_git_repo(&root);
    let log = std::fs::File::create(tmp.path().join("sqryd.log")).expect("log file");

    let child = Command::new(&binary)
        .arg("foreground")
        .env("SQRY_DAEMON_CONFIG", &config)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn sqryd");
    let mut child = KillOnDrop(child);
    assert!(
        wait_for_socket(&socket, Duration::from_secs(30)),
        "sqryd never bound {}",
        socket.display()
    );

    let mut client = TestIpcClient::connect(&socket).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    assert_eq!(
        status_row(&mut client, &root).await["watching"],
        json!(true),
        "the loaded workspace is watched"
    );
    expect_success(&client.request("daemon/stop", json!({})).await);
    drop(client);

    let started = Instant::now();
    let exited = loop {
        if let Some(status) = child.0.try_wait().expect("try_wait") {
            break Some(status);
        }
        if started.elapsed() > Duration::from_secs(8) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let took = started.elapsed();
    println!("S6 sqryd after daemon/stop: exited={exited:?} after {took:?}");
    let status = exited.unwrap_or_else(|| {
        panic!(
            "sqryd is still running {took:?} after daemon/stop with a watched workspace \
             (issue #902); log:\n{}",
            std::fs::read_to_string(tmp.path().join("sqryd.log")).unwrap_or_default()
        )
    });
    assert!(status.success(), "sqryd exits 0: {status}");
}

/// A `sqryd` whose every durable persist pauses `pause_ms` once it has set
/// the old pair aside, serving `root` (indexed with every plugin, so the
/// recorded selection is `include_all`), with a `daemon/rebuild --force`
/// sent on its own connection. Returns once that rebuild's persist is
/// mid-way: its marker is on disk and the manifest is aside.
struct MidPersist {
    tmp: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    root: PathBuf,
    socket: PathBuf,
    child: KillOnDrop,
    old_manifest: Vec<u8>,
    old_snapshot: Vec<u8>,
    rebuild: tokio::task::JoinHandle<()>,
}

async fn sqryd_mid_persist(pause_ms: u64) -> MidPersist {
    let binary = sqryd_binary()
        .expect("the sqryd binary must be built beside this test (CARGO_BIN_EXE_sqryd)");
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sqryd-mid-persist.sock");
    let config = tmp.path().join("daemon.toml");
    std::fs::write(
        &config,
        format!("[socket]\npath = {:?}\n", socket.to_string_lossy().as_ref()),
    )
    .expect("write config");
    let (workspace, root) = mixed_workspace();
    support::rebuild_fixtures::index_include_all(&root);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let old_manifest = std::fs::read(storage.manifest_path()).expect("old manifest");
    let old_snapshot = std::fs::read(storage.snapshot_path()).expect("old snapshot");
    let log = std::fs::File::create(tmp.path().join("sqryd.log")).expect("log file");
    let child = Command::new(&binary)
        .arg("foreground")
        .env("SQRY_DAEMON_CONFIG", &config)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .env("SQRYD_TEST_MID_PERSIST_PAUSE_MS", pause_ms.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn sqryd");
    let child = KillOnDrop(child);
    assert!(
        wait_for_socket(&socket, Duration::from_secs(30)),
        "sqryd never bound {}",
        socket.display()
    );
    let mut client = TestIpcClient::connect(&socket).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    let rebuild = {
        let (socket, root) = (socket.clone(), root.clone());
        tokio::spawn(async move {
            let mut client = TestIpcClient::connect(&socket).await;
            client.hello(1).await;
            // The answer is a success or a closed connection (the helper
            // then panics inside this task, which is not joined as a
            // failure); neither is asserted, the index on disk is.
            let _ = ipc_rebuild(&mut client, &root, true, &[]).await;
        })
    };
    let graph_dir = storage.graph_dir().to_path_buf();
    let mid_way = support::rebuild_fixtures::wait_until_blocking(Duration::from_secs(60), || {
        rollback_names(&graph_dir)
            .iter()
            .any(|name| name.starts_with(".txn-begun"))
            && !storage.manifest_path().exists()
    });
    assert!(
        mid_way,
        "the rebuild's persist never set the old pair aside; log:\n{}",
        std::fs::read_to_string(tmp.path().join("sqryd.log")).unwrap_or_default()
    );
    MidPersist {
        tmp,
        _workspace: workspace,
        root,
        socket,
        child,
        old_manifest,
        old_snapshot,
        rebuild,
    }
}

fn rollback_names(graph_dir: &Path) -> Vec<String> {
    std::fs::read_dir(graph_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".rollback-"))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether some process holds the daemon's pidfile lock at `lock_path`.
fn lock_is_held(lock_path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(lock_path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Round 7 audit B1: `daemon/stop` while a rebuild's durable persist is
/// mid-way. The daemon finishes the persist before it exits (the persist
/// pauses 22 s, past the server's drain bound plus the 10 s bound of the
/// runtime's shutdown, as the audit's 18.9 s persist was), keeps its
/// pidfile lock until it exits, and leaves a whole new pair. Before the
/// repair the bounded shutdown abandoned the persist: the process exited
/// with the manifest set aside, and the stop was reported while it still
/// ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqryd_stopped_mid_persist_finishes_the_persist_before_it_exits() {
    let mut run = sqryd_mid_persist(22_000).await;
    // A custom socket puts the pidfile lock beside it (`lock_path`).
    let lock_path = run.socket.with_extension("lock");
    assert!(
        lock_is_held(&lock_path),
        "the running daemon holds its lock"
    );
    let mut client = TestIpcClient::connect(&run.socket).await;
    client.hello(1).await;
    let _ = client.request("daemon/stop", json!({})).await;
    drop(client);

    let started = Instant::now();
    // The lock is released as the process's last act, so it can be seen
    // free a moment before the exit is reaped; a release that long
    // precedes the exit is the defect.
    let mut free_while_alive: Option<Instant> = None;
    let status = loop {
        if let Some(status) = run.child.0.try_wait().expect("try_wait") {
            break status;
        }
        if free_while_alive.is_none() && !lock_is_held(&lock_path) {
            free_while_alive = Some(Instant::now());
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "sqryd did not exit 90 s after daemon/stop"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    println!(
        "B1 sqryd stopped mid-persist exited {status} after {:?}",
        started.elapsed()
    );
    let _ = run.rebuild.await;
    assert!(status.success(), "sqryd exits 0: {status}");
    if let Some(freed) = free_while_alive {
        let alive_after = freed.elapsed();
        assert!(
            alive_after < Duration::from_secs(2),
            "the pidfile lock was released {alive_after:?} before the daemon exited"
        );
    }
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&run.root);
    let manifest_bytes = std::fs::read(storage.manifest_path()).unwrap_or_else(|e| {
        panic!(
            "the stop abandoned the persist: no manifest ({e}); log:\n{}",
            std::fs::read_to_string(run.tmp.path().join("sqryd.log")).unwrap_or_default()
        )
    });
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let snapshot = std::fs::read(storage.snapshot_path()).expect("snapshot");
    {
        use sha2::Digest;
        assert_eq!(
            manifest["snapshot_sha256"].as_str(),
            Some(
                sha2::Sha256::digest(&snapshot)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    .as_str()
            ),
            "the manifest describes the snapshot beside it"
        );
    }
    assert_ne!(
        manifest_bytes, run.old_manifest,
        "the persist finished: the manifest is the rebuild's"
    );
    assert_eq!(
        manifest["plugin_selection"]["high_cost_mode"],
        json!("include_all"),
        "the recorded selection is kept"
    );
    assert!(
        rollback_names(storage.graph_dir()).is_empty(),
        "the persist removed its rollback set: {:?}",
        rollback_names(storage.graph_dir())
    );
}

/// Round 7 audit B1, the persist seat's SIGKILL case: a daemon killed in
/// the middle of a persist leaves no manifest, and the next read of the
/// index puts the previous pair back, with its recorded selection. Before
/// the repair nothing restored the set-aside manifest and every later
/// writer recorded `fast_path_default`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_killed_mid_persist_is_recovered_on_the_next_load() {
    let mut run = sqryd_mid_persist(30_000).await;
    run.child.0.kill().expect("SIGKILL sqryd");
    let _ = run.child.0.wait();
    let _ = run.rebuild.await;
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&run.root);
    assert!(
        !storage.manifest_path().exists(),
        "the kill left the manifest aside (the raw state before any read)"
    );
    let manifest = storage
        .load_manifest()
        .expect("the next load reads the previous index");
    assert_eq!(
        std::fs::read(storage.manifest_path()).unwrap(),
        run.old_manifest,
        "the previous manifest is back, byte for byte"
    );
    assert_eq!(
        std::fs::read(storage.snapshot_path()).unwrap(),
        run.old_snapshot,
        "the previous snapshot is back, byte for byte"
    );
    assert_eq!(
        manifest
            .plugin_selection
            .as_ref()
            .and_then(|selection| selection.high_cost_mode.as_deref()),
        Some("include_all"),
        "the recorded selection is kept"
    );
    assert!(rollback_names(storage.graph_dir()).is_empty());
}

/// The `sqryd` binary Cargo built for this package's integration tests.
fn sqryd_binary() -> Option<PathBuf> {
    let path = PathBuf::from(option_env!("CARGO_BIN_EXE_sqryd")?);
    path.is_file().then_some(path)
}

fn wait_for_socket(path: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Kills the daemon if a test fails before it exits on its own.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
