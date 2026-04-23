//! Task 8 Phase 8a — Unix bind-safety tests for configured-path policy.

#![cfg(unix)]

mod support;

use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::sync::Arc;

use sqry_daemon::{
    DaemonConfig, EmptyGraphBuilder, IpcServer, RebuildDispatcher, SocketConfig, WorkspaceBuilder,
    WorkspaceManager,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

async fn bind_with_config(path: PathBuf) -> sqry_daemon::DaemonResult<IpcServer> {
    let config = Arc::new(DaemonConfig {
        socket: SocketConfig {
            path: Some(path),
            pipe_name: None,
        },
        ..DaemonConfig::default()
    });
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
    let tool_executor = Arc::new(sqry_core::query::executor::QueryExecutor::new());
    IpcServer::bind(
        config,
        manager,
        dispatcher,
        builder,
        tool_executor,
        CancellationToken::new(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_path_accepts_fresh_path() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sqryd.sock");
    let server = bind_with_config(sock.clone()).await.expect("bind ok");
    assert!(sock.exists(), "socket must exist after bind");
    assert!(
        std::fs::symlink_metadata(&sock)
            .unwrap()
            .file_type()
            .is_socket()
    );
    drop(server); // drops listener
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_path_refuses_live_socket() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sqryd.sock");
    // First bind succeeds.
    let server = bind_with_config(sock.clone()).await.expect("first bind");
    // Second bind against the same live path must fail.
    let err = bind_with_config(sock.clone())
        .await
        .expect_err("second bind must fail");
    match err {
        sqry_daemon::DaemonError::Config { path, source } => {
            assert_eq!(path, sock);
            assert!(
                source.to_string().contains("already in use"),
                "message: {source}"
            );
        }
        other => panic!("expected Config error, got {other:?}"),
    }
    drop(server);
}

/// After a daemon stop, a stale socket (no process listening) must be
/// auto-unlinked and rebound — even when the bind mode is `Configured`
/// (i.e. `SQRY_DAEMON_SOCKET` is set or `socket.path` is in the TOML).
///
/// This is the regression test for the P0 auto-start bug:
/// `SQRY_DAEMON_SOCKET` triggers stale-socket refusal, making auto-start
/// 100% broken after any normal daemon stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_path_auto_unlinks_stale_socket() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sqryd.sock");
    // Simulate a stale socket: bind + drop immediately, then the
    // socket file lingers on disk with nobody listening.
    {
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        drop(listener);
    }
    // File remains — ensure it is still a socket.
    let meta = std::fs::symlink_metadata(&sock).unwrap();
    assert!(meta.file_type().is_socket());
    // Configured-branch bind must auto-unlink the stale socket and succeed.
    let server = bind_with_config(sock.clone())
        .await
        .expect("stale configured socket must be auto-unlinked and rebound");
    // The socket file must still exist (replaced by the new listener's socket).
    assert!(sock.exists(), "new socket must exist after bind");
    assert!(
        std::fs::symlink_metadata(&sock)
            .unwrap()
            .file_type()
            .is_socket(),
        "rebound path must be a socket"
    );
    drop(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_path_refuses_non_socket_file() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sqryd.sock");
    std::fs::write(&sock, b"not a socket").unwrap();
    let err = bind_with_config(sock.clone())
        .await
        .expect_err("non-socket must fail");
    match err {
        sqry_daemon::DaemonError::Config { path, source } => {
            assert_eq!(path, sock);
            assert!(
                source.to_string().contains("not a socket"),
                "message: {source}"
            );
        }
        other => panic!("expected Config error, got {other:?}"),
    }
    // Regular file must still exist.
    assert!(sock.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_dir_refuses_live_socket() {
    // Drive the runtime-dir branch by pointing XDG_RUNTIME_DIR at a
    // tempdir, then binding twice against `DaemonConfig::default()` —
    // the second bind must observe the first's live socket and refuse.
    //
    // Env mutation is serialised via `acquire_env_lock()` + released
    // BEFORE the first `.await`, then reacquired at teardown, so we
    // never hold a sync `MutexGuard` across an await point
    // (clippy::await_holding_lock).
    let tmp = TempDir::new().unwrap();
    let prev_xdg = {
        let _guard = acquire_env_lock();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        }
        prev
    };

    let cfg1 = Arc::new(DaemonConfig::default());
    let manager1 = WorkspaceManager::new_without_reaper(Arc::clone(&cfg1));
    let plugins1 = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher1 = RebuildDispatcher::new(
        Arc::clone(&manager1),
        Arc::clone(&cfg1),
        Arc::clone(&plugins1),
    );
    let builder1: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
    let tool_executor1 = Arc::new(sqry_core::query::executor::QueryExecutor::new());
    let server = IpcServer::bind(
        Arc::clone(&cfg1),
        manager1,
        dispatcher1,
        builder1,
        tool_executor1,
        CancellationToken::new(),
    )
    .await
    .expect("runtime bind");

    let cfg2 = Arc::new(DaemonConfig::default());
    let manager2 = WorkspaceManager::new_without_reaper(Arc::clone(&cfg2));
    let plugins2 = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher2 = RebuildDispatcher::new(
        Arc::clone(&manager2),
        Arc::clone(&cfg2),
        Arc::clone(&plugins2),
    );
    let builder2: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
    let tool_executor2 = Arc::new(sqry_core::query::executor::QueryExecutor::new());
    let err = IpcServer::bind(
        Arc::clone(&cfg2),
        manager2,
        dispatcher2,
        builder2,
        tool_executor2,
        CancellationToken::new(),
    )
    .await
    .expect_err("second runtime bind must fail");
    match err {
        sqry_daemon::DaemonError::Config { source, .. } => {
            assert!(
                source.to_string().contains("already in use"),
                "message: {source}"
            );
        }
        other => panic!("expected Config, got {other:?}"),
    }
    drop(server);

    {
        let _guard = acquire_env_lock();
        // SAFETY: serialised via ENV_LOCK.
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}

use std::sync::{Mutex, MutexGuard};
static ENV_LOCK: Mutex<()> = Mutex::new(());
fn acquire_env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}
