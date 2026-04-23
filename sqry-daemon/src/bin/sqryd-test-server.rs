//! `sqryd-test-server` — test-helper binary for Phase 8c U16 integration tests.
//!
//! This binary starts a fully-functional `IpcServer` bound to a UDS socket
//! whose path is taken from the `SQRYD_TEST_SOCKET` environment variable.
//! It writes `READY\n` to stdout once the socket is bound, then runs until
//! stdin is closed (EOF) or the process is killed.
//!
//! # Usage by U16 tests
//!
//! The `daemon_fixture` helpers in `sqry-lsp/tests/common/` and
//! `sqry-mcp/tests/common/` locate this binary via the `current_exe()`
//! parent-directory trick (same approach as `find_sqry_mcp_binary` in the MCP
//! test suite), spawn it with `SQRYD_TEST_SOCKET=/tmp/<uuid>.sock`, wait for
//! `READY`, then supply the socket path to
//! `sqry_daemon_client::connect_shim_with_timeouts`.
//!
//! # Why this binary?
//!
//! The `sqryd` main binary is a lifecycle-scaffold that does not yet bind a
//! socket (Task 9 wires that up). The U16 tests need a live IpcServer without
//! pulling `sqry-daemon` in as a dev-dependency of `sqry-lsp` / `sqry-mcp`
//! (which would introduce a cycle: sqry-daemon → sqry-lsp → sqry-daemon).
//! A test-helper binary in sqry-daemon bridges both constraints cleanly.

use std::env;
use std::io::{self, Write};
use std::sync::Arc;

use sqry_daemon::{
    DaemonConfig, EmptyGraphBuilder, IpcServer, RebuildDispatcher, SocketConfig, WorkspaceBuilder,
    WorkspaceManager,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    // Minimal tracing so test stderr carries a diagnostic breadcrumb.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SQRYD_TEST_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .compact()
        .with_writer(std::io::stderr)
        .init();

    let socket_path = env::var("SQRYD_TEST_SOCKET").expect("SQRYD_TEST_SOCKET must be set");

    let mut config = DaemonConfig {
        socket: SocketConfig {
            path: Some(socket_path.into()),
            pipe_name: None,
        },
        ..DaemonConfig::default()
    };
    // Apply env overrides (e.g. SQRY_DAEMON_MAX_SHIM_CONNECTIONS) so integration
    // tests can configure the server at spawn time without modifying the binary.
    config
        .apply_env_overrides()
        .expect("apply_env_overrides in sqryd-test-server");
    let config = Arc::new(config);

    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );
    let tool_executor = Arc::new(sqry_core::query::executor::QueryExecutor::new());
    let shutdown = CancellationToken::new();

    let server = IpcServer::bind(
        Arc::clone(&config),
        Arc::clone(&manager),
        dispatcher,
        Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>,
        tool_executor,
        shutdown.clone(),
    )
    .await
    .expect("IpcServer::bind");

    // Signal "ready" to the parent process before entering the accept loop.
    // The parent (daemon_fixture) reads this line to know the socket is
    // accepting connections.
    let stdout = io::stdout();
    writeln!(&mut stdout.lock(), "READY").expect("write READY to stdout");
    stdout.lock().flush().expect("flush READY");

    // Run the server in a background task.
    let server_handle = tokio::spawn(server.run());

    // Block until stdin is closed (parent process dropped the pipe) or
    // until we receive an explicit shutdown signal.
    // Using tokio stdin to detect parent close non-blockingly.
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 1];
    // Read until EOF — when the test fixture drops the child's stdin,
    // this returns Ok(0). Errors are treated as "parent gone", same as EOF.
    loop {
        match stdin.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {} // ignore any data
        }
    }

    // Trigger graceful shutdown.
    shutdown.cancel();

    // Wait for the server to drain (best-effort; we do not panic on error
    // so the process exits cleanly regardless).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), server_handle).await;
}
