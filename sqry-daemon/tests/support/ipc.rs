//! Shared test helpers for the Task 8 Phase 8a IPC integration tests.
//!
//! Every IPC test file `mod support;` ≡ the module defined in
//! `support/mod.rs`, and then imports `support::ipc::{TestIpcClient, …}`
//! from here. Phase 8a's integration tests use a minimal framed JSON-RPC
//! client that speaks the wire format directly — this is NOT the Task
//! 10 `DaemonClient`.

#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqry_daemon::{
    DaemonConfig, EmptyGraphBuilder, IpcServer, RebuildDispatcher, SocketConfig, WorkspaceBuilder,
    WorkspaceManager,
    ipc::framing::{read_frame_json, write_frame_json},
    ipc::protocol::{DaemonHello, DaemonHelloResponse, JsonRpcError, JsonRpcResponse},
    ipc::shim_registry::ShimRegistry,
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Server spawn helper
// ---------------------------------------------------------------------------

/// Spawn an [`IpcServer`] on a tempdir-backed UDS. Uses the
/// `Configured` bind branch so nothing under `$XDG_RUNTIME_DIR` is
/// mutated. Returns the server path + shutdown token + join handle.
pub struct TestServer {
    pub path: PathBuf,
    pub shutdown: CancellationToken,
    pub handle: tokio::task::JoinHandle<sqry_daemon::DaemonResult<()>>,
    pub manager: Arc<WorkspaceManager>,
    /// Clone of the [`RebuildDispatcher`] used by the [`IpcServer`].
    ///
    /// Exposed for Task 13 e2e tests that need to call
    /// `dispatcher.ensure_watching(...)` after `daemon/load` completes
    /// (e.g., `file_change_triggers_rebuild`). Both the server and the
    /// test harness share the same underlying state through this `Arc`.
    pub dispatcher: Arc<RebuildDispatcher>,
    /// Shared shim-connection registry; mirrors the one held by
    /// `IpcServer`. Exposed for Phase 8c U15 RAII-deregistration tests.
    pub shim_registry: Arc<ShimRegistry>,
    pub _tmp: TempDir,
}

impl TestServer {
    pub async fn new() -> Self {
        Self::with_builder(Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>).await
    }

    pub async fn with_builder(builder: Arc<dyn WorkspaceBuilder>) -> Self {
        Self::with_builder_and_config(builder, DaemonConfig::default()).await
    }

    /// Task 8 Phase 8b: spawn a test server with a caller-provided
    /// [`DaemonConfig`] (e.g., `stale_serve_max_age_hours = 24`). The
    /// socket-path override inside the TempDir is still applied on top
    /// of the provided config so the bind never touches
    /// `$XDG_RUNTIME_DIR`.
    pub async fn with_config(config: DaemonConfig) -> Self {
        Self::with_builder_and_config(
            Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>,
            config,
        )
        .await
    }

    pub async fn with_builder_and_config(
        builder: Arc<dyn WorkspaceBuilder>,
        config_in: DaemonConfig,
    ) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let sock_path = tmp.path().join("sqryd.sock");
        let config = Arc::new(DaemonConfig {
            socket: SocketConfig {
                path: Some(sock_path.clone()),
                pipe_name: None,
            },
            ..config_in
        });
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
        let dispatcher = RebuildDispatcher::new(
            Arc::clone(&manager),
            Arc::clone(&config),
            Arc::clone(&plugins),
        );
        // Clone before consuming: the test harness holds one `Arc` handle,
        // `IpcServer::bind` consumes the other. Both share the same
        // underlying `RebuildDispatcher` state.
        let dispatcher_clone = Arc::clone(&dispatcher);
        // `PluginManager` is not `Clone`, and test workspaces don't exercise
        // the query planner deeply enough to need the full plugin set —
        // use `QueryExecutor::new()` which gets an empty `PluginManager`.
        let tool_executor = Arc::new(sqry_core::query::executor::QueryExecutor::new());
        let shutdown = CancellationToken::new();
        let server = IpcServer::bind(
            Arc::clone(&config),
            Arc::clone(&manager),
            dispatcher,
            builder,
            tool_executor,
            shutdown.clone(),
        )
        .await
        .expect("bind");
        let bound_path = server.socket_path().to_path_buf();
        let shim_registry = server.shim_registry();
        let handle = tokio::spawn(server.run());
        // Wait briefly for the listener to be ready.
        wait_for_socket(&bound_path, std::time::Duration::from_secs(2)).await;
        Self {
            path: bound_path,
            shutdown,
            handle,
            manager,
            dispatcher: dispatcher_clone,
            shim_registry,
            _tmp: tmp,
        }
    }

    /// Return the shared shim-connection registry.
    ///
    /// Convenience accessor for Phase 8c U15 RAII-deregistration tests.
    pub fn shim_registry(&self) -> Arc<ShimRegistry> {
        Arc::clone(&self.shim_registry)
    }

    /// Signal shutdown and await the server future. Panics on error.
    pub async fn stop(self) {
        self.shutdown.cancel();
        let res = self.handle.await.expect("join");
        res.expect("server run returns Ok");
    }
}

async fn wait_for_socket(path: &Path, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("socket at {} never appeared", path.display());
}

// ---------------------------------------------------------------------------
// Minimal framed JSON-RPC client
// ---------------------------------------------------------------------------

pub struct TestIpcClient {
    stream: UnixStream,
    next_id: i64,
}

impl TestIpcClient {
    pub async fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).await.expect("connect");
        Self { stream, next_id: 1 }
    }

    /// Send `DaemonHello` and read the response.
    pub async fn hello(&mut self, protocol_version: u32) -> DaemonHelloResponse {
        let hello = DaemonHello {
            client_version: "test/0.0.1".into(),
            protocol_version,
        };
        write_frame_json(&mut self.stream, &hello)
            .await
            .expect("write hello");
        read_frame_json::<_, DaemonHelloResponse>(&mut self.stream)
            .await
            .expect("read hello")
            .expect("some frame")
    }

    /// Send an arbitrary framed JSON value (useful for parse-error /
    /// invalid-request test injections).
    pub async fn send_raw<T: Serialize>(&mut self, value: &T) {
        write_frame_json(&mut self.stream, value)
            .await
            .expect("write raw");
    }

    /// Send raw bytes as a frame body (for parse-error tests).
    pub async fn send_raw_bytes(&mut self, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap();
        self.stream.write_all(&len.to_le_bytes()).await.unwrap();
        self.stream.write_all(bytes).await.unwrap();
        self.stream.flush().await.unwrap();
    }

    /// Read the next framed JSON-RPC response.
    pub async fn read_response(&mut self) -> JsonRpcResponse {
        read_frame_json::<_, JsonRpcResponse>(&mut self.stream)
            .await
            .expect("read response")
            .expect("some response")
    }

    /// Read the next framed value of an arbitrary typed shape (batches
    /// come back as `Vec<JsonRpcResponse>` via this path).
    pub async fn read_typed<T: DeserializeOwned>(&mut self) -> T {
        read_frame_json::<_, T>(&mut self.stream)
            .await
            .expect("read typed")
            .expect("some typed frame")
    }

    /// Send a well-formed JSON-RPC request with an auto-incrementing
    /// numeric id.
    pub async fn request(&mut self, method: &str, params: Value) -> JsonRpcResponse {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_raw(&req).await;
        self.read_response().await
    }

    /// Send a notification (no id). No response expected.
    pub async fn notify(&mut self, method: &str, params: Value) {
        let req = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send_raw(&req).await;
    }
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

pub fn expect_success(resp: &JsonRpcResponse) -> &Value {
    match &resp.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => result,
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
            panic!("expected success, got error: {error:?}")
        }
    }
}

pub fn expect_error(resp: &JsonRpcResponse) -> &JsonRpcError {
    match &resp.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => error,
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => {
            panic!("expected error, got success: {result:?}")
        }
    }
}
