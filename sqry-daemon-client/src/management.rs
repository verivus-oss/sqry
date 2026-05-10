//! `DaemonClient` — management API for the sqryd daemon.
//!
//! Provides a high-level async client for daemon lifecycle operations
//! using the [`DaemonHello`] / [`DaemonHelloResponse`] handshake +
//! JSON-RPC request/response pattern.
//!
//! # Connection model
//!
//! [`DaemonClient::connect`] (or [`DaemonClient::connect_with_timeouts`]):
//! 1. Opens a platform-appropriate stream via
//!    [`crate::platform_connect`].
//! 2. Writes a [`DaemonHello`] as the first frame.
//! 3. Reads the [`DaemonHelloResponse`] from the daemon.
//! 4. Validates `compatible` and `envelope_version`.
//! 5. Stores the daemon version for later access via
//!    [`DaemonClient::daemon_version`].
//!
//! After construction, [`DaemonClient::send_request`] writes a
//! JSON-RPC 2.0 request frame and reads the corresponding response
//! frame. The convenience methods [`DaemonClient::stop`] and
//! [`DaemonClient::status`] wrap `send_request` for the two management
//! methods exposed by the daemon.
//!
//! # Why separate from `ShimConnection`
//!
//! The daemon router shape-discriminates the very first frame: a frame
//! with `protocol` + `pid` keys (shim-shaped) enters the shim
//! byte-pump path; a frame with `client_version` + `protocol_version`
//! (hello-shaped) enters the JSON-RPC management path. Clients must
//! never mix the two patterns on the same connection. This module
//! exposes *only* the hello → JSON-RPC path; the shim path lives in
//! [`crate::connect_shim`].

use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use sqry_daemon_protocol::{
    DaemonHello, DaemonHelloResponse, ENVELOPE_VERSION, JsonRpcId, JsonRpcPayload, JsonRpcRequest,
    JsonRpcResponse, JsonRpcVersion, LoadResult, ResponseEnvelope, framing,
};

use crate::{AsyncReadWrite, ClientError, DEFAULT_CONNECT_TIMEOUT, platform_connect};

// ---------------------------------------------------------------------------
// Hello-handshake timeout constant.
// ---------------------------------------------------------------------------

/// Default upper bound on the [`DaemonHello`] → [`DaemonHelloResponse`]
/// handshake round-trip after a successful connect.
///
/// Mirrors [`DEFAULT_CONNECT_TIMEOUT`]: the hello handshake is
/// latency-trivial when the daemon is healthy; anything longer is a
/// strong signal of a stuck accept loop or a protocol regression.
pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// DaemonClient.
// ---------------------------------------------------------------------------

/// Client for daemon management operations.
///
/// Uses the [`DaemonHello`] / [`DaemonHelloResponse`] handshake (NOT
/// the shim handshake) to establish a JSON-RPC session with the daemon.
/// Construct via [`DaemonClient::connect`] or
/// [`DaemonClient::connect_with_timeouts`].
///
/// Each [`DaemonClient`] instance owns exactly one connection; operations
/// are serialised through [`DaemonClient::send_request`]. For concurrent
/// access, create separate `DaemonClient` instances.
pub struct DaemonClient {
    stream: Pin<Box<dyn AsyncReadWrite + Send>>,
    daemon_version: String,
    next_id: i64,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("daemon_version", &self.daemon_version)
            .field("next_id", &self.next_id)
            .field("stream", &"<Pin<Box<dyn AsyncReadWrite + Send>>>")
            .finish()
    }
}

impl DaemonClient {
    // -----------------------------------------------------------------------
    // Constructors.
    // -----------------------------------------------------------------------

    /// Connect to the daemon at `socket_path` using default timeouts.
    ///
    /// Performs the [`DaemonHello`] handshake with
    /// [`DEFAULT_CONNECT_TIMEOUT`] and [`DEFAULT_HELLO_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// - [`ClientError::Connect`] if the socket connect fails.
    /// - [`ClientError::ConnectTimeout`] if connect exceeds
    ///   [`DEFAULT_CONNECT_TIMEOUT`].
    /// - [`ClientError::HandshakeTimeout`] if the hello response does
    ///   not arrive within [`DEFAULT_HELLO_TIMEOUT`].
    /// - [`ClientError::EnvelopeVersionMismatch`] if the daemon's
    ///   [`DaemonHelloResponse::envelope_version`] does not match this
    ///   client's compiled-in [`ENVELOPE_VERSION`]. Checked BEFORE
    ///   `compatible` so a wire-format mismatch is never masked by a
    ///   simultaneous application-level rejection.
    /// - [`ClientError::HelloRejected`] if the daemon responds with
    ///   `compatible: false`.
    /// - [`ClientError::HelloEof`] if the daemon closes the connection
    ///   before sending a hello response.
    /// - [`ClientError::Frame`] / [`ClientError::Io`] for framing or IO
    ///   failures during the handshake.
    pub async fn connect(socket_path: &Path) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(socket_path, DEFAULT_CONNECT_TIMEOUT, DEFAULT_HELLO_TIMEOUT)
            .await
    }

    /// Connect to the daemon with explicit timeout overrides.
    ///
    /// Semantically identical to [`Self::connect`] but lets callers
    /// tune the `connect` and hello-handshake budgets.
    ///
    /// # Errors
    ///
    /// Same as [`Self::connect`].
    pub async fn connect_with_timeouts(
        socket_path: &Path,
        connect_timeout: Duration,
        handshake_timeout: Duration,
    ) -> Result<Self, ClientError> {
        use crate::apply_connect_timeout;

        // Step 1 — bounded platform connect.
        let stream =
            apply_connect_timeout(platform_connect(socket_path), socket_path, connect_timeout)
                .await?;

        // Step 2 — bounded hello handshake via the shared inner driver.
        // `do_hello_handshake` is pub(crate) and lives at module level;
        // it is also the test harness entry point that accepts an
        // already-connected stream directly (no platform_connect step).
        do_hello_handshake(stream, socket_path, handshake_timeout).await
    }

    // -----------------------------------------------------------------------
    // Accessors.
    // -----------------------------------------------------------------------

    /// The daemon version string from the [`DaemonHelloResponse`].
    #[must_use]
    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    // -----------------------------------------------------------------------
    // Core request/response.
    // -----------------------------------------------------------------------

    /// Send a JSON-RPC 2.0 request and return the `result` field on
    /// success, or surface a [`ClientError::RpcError`] for an error
    /// response.
    ///
    /// Each call consumes one monotonically-incrementing request id
    /// (`i64` starting from 1, wrapping on overflow). The method and
    /// params are supplied by the caller; `"jsonrpc": "2.0"` is always
    /// injected.
    ///
    /// The response `id` is validated to match the request `id` before
    /// the payload is returned. A mismatch surfaces as
    /// [`ClientError::Io`] with kind `InvalidData` so the caller can
    /// detect a protocol regression or a corrupted frame stream.
    ///
    /// # Errors
    ///
    /// - [`ClientError::RpcError`] if the daemon returns a JSON-RPC
    ///   error payload.
    /// - [`ClientError::Io`] (`InvalidData`) if the response `id` does
    ///   not match the request `id`, or if the daemon closes the
    ///   connection before responding.
    /// - [`ClientError::Frame`] if the response frame cannot be decoded.
    pub async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let expected_id = JsonRpcId::I64(id);
        let request = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: Some(expected_id.clone()),
            method: method.to_owned(),
            params,
        };

        framing::write_frame_json(&mut self.stream, &request).await?;

        let response: JsonRpcResponse = match framing::read_frame_json(&mut self.stream).await? {
            Some(r) => r,
            None => {
                return Err(ClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed connection after JSON-RPC request",
                )));
            }
        };

        // Validate that the response id echoes our request id. The
        // spec allows `id: null` only on parse-error and
        // invalid-request responses (which the daemon should never
        // emit for a well-formed request), so any mismatch here
        // indicates a protocol regression or a corrupted frame stream.
        if response.id.as_ref() != Some(&expected_id) {
            return Err(ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "JSON-RPC response id mismatch: expected {:?}, got {:?}",
                    expected_id, response.id
                ),
            )));
        }

        match response.payload {
            JsonRpcPayload::Success { result } => Ok(result),
            JsonRpcPayload::Error { error } => Err(ClientError::RpcError {
                code: error.code,
                message: error.message,
                data: error.data,
            }),
        }
    }

    // -----------------------------------------------------------------------
    // Management convenience methods.
    // -----------------------------------------------------------------------

    /// Send a `daemon/stop` JSON-RPC request.
    ///
    /// The daemon initiates graceful shutdown upon receiving this
    /// request. The caller is responsible for polling the socket until
    /// it becomes unreachable if it needs to wait for full daemon exit.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::send_request`].
    pub async fn stop(&mut self) -> Result<(), ClientError> {
        self.send_request("daemon/stop", serde_json::json!({}))
            .await?;
        Ok(())
    }

    /// Send a `daemon/status` JSON-RPC request and return the `result`
    /// field.
    ///
    /// The returned [`serde_json::Value`] is the raw daemon status
    /// object. Callers should render it opportunistically — the exact
    /// field set depends on the daemon version.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::send_request`].
    pub async fn status(&mut self) -> Result<serde_json::Value, ClientError> {
        self.send_request("daemon/status", serde_json::json!({}))
            .await
    }

    /// Send a `daemon/reset` JSON-RPC request to drop the in-memory
    /// graph + admission bytes for the workspace at `path`, preserving
    /// the manager-map entry, `pinned` bit, and `last_error`
    /// (cluster-G §3.2). Files on disk are NEVER touched.
    ///
    /// `force = true` is required to reset a `pinned` workspace.
    ///
    /// Returns the raw `daemon/reset` JSON result, which carries
    /// `{ root, reset }`. `reset = true` when the workspace was
    /// present and reset; `false` when the path matched no workspace.
    ///
    /// # Errors
    ///
    /// - [`ClientError::RpcError`] with code `-32004` if the workspace
    ///   is not loaded.
    /// - [`ClientError::RpcError`] with code `-32008` if the workspace
    ///   is currently `Loading`.
    /// - [`ClientError::RpcError`] with code `-32009` if a rebuild is
    ///   in flight (the daemon dispatched a cancellation; retry after
    ///   the `retry_after_ms` field in `error.data`).
    /// - [`ClientError::RpcError`] with code `-32010` if the workspace
    ///   is pinned and `force = false`.
    /// - Propagates other errors from [`Self::send_request`].
    pub async fn reset(
        &mut self,
        path: &Path,
        force: bool,
    ) -> Result<serde_json::Value, ClientError> {
        self.send_request(
            "daemon/reset",
            serde_json::json!({ "path": path, "force": force }),
        )
        .await
    }

    /// Send a `daemon/active-artifacts` JSON-RPC request and return
    /// the list of `.sqry/graph` directories the daemon currently has
    /// loaded (cluster-E §E.4 hand-off).
    ///
    /// The daemon's `WorkspaceManager::active_artifact_dirs` is the
    /// authoritative source. Read-only and concurrent-safe — callers
    /// should bound the wall-clock with `tokio::time::timeout` so a
    /// stalled daemon does not block CLI commands like
    /// `sqry workspace clean`.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::send_request`]. Returns
    /// [`ClientError::SchemaMismatch`] if the daemon response does not
    /// contain an `artifacts: [PathBuf]` field at the canonical key.
    pub async fn active_artifacts(&mut self) -> Result<Vec<std::path::PathBuf>, ClientError> {
        let raw = self
            .send_request("daemon/active-artifacts", serde_json::json!({}))
            .await?;
        // Cluster-E iter-2: strict parse — a malformed response must
        // produce `SchemaMismatch`, never an empty `Vec`. The codex
        // iter-1 review flagged that an `unwrap_or_default` here let
        // `sqry workspace clean --apply` delete a daemon-locked
        // artifact when the daemon's wire schema drifted.
        //
        // Accepted shapes (`send_request` returns the inner `result`
        // already, but daemon-path callers forward an additional
        // envelope, so we tolerate both nestings):
        //   `{ "artifacts": [...] }`
        //   `{ "result": { "artifacts": [...] } }`
        //
        // `serde_json::from_value` on an explicit field shape gives
        // us a real `serde_json::Error` for `SchemaMismatch` without
        // pulling in `serde` as a direct dep.
        #[derive(serde::Deserialize)]
        struct Body {
            artifacts: Vec<std::path::PathBuf>,
        }
        let body_value = raw
            .get("result")
            .cloned()
            .or_else(|| Some(raw.clone()))
            .unwrap_or(raw);
        let body: Body =
            serde_json::from_value(body_value).map_err(|source| ClientError::SchemaMismatch {
                method: "daemon/active-artifacts",
                source,
            })?;
        Ok(body.artifacts)
    }

    /// Send a `daemon/rebuild` JSON-RPC request to trigger a rebuild
    /// for the workspace at `path`.
    ///
    /// `force = true` forces a full rebuild from scratch; `force = false`
    /// uses the normal incremental/full decision heuristics.
    ///
    /// Returns the raw JSON result on success (containing `duration_ms`,
    /// `nodes`, `edges`, `files_indexed`, `was_full`).
    ///
    /// # Errors
    ///
    /// - [`ClientError::RpcError`] with code `-32004` if the workspace
    ///   is not loaded.
    /// - [`ClientError::RpcError`] with code `-32001` if the rebuild fails.
    /// - Propagates other errors from [`Self::send_request`].
    pub async fn rebuild(
        &mut self,
        path: &Path,
        force: bool,
    ) -> Result<serde_json::Value, ClientError> {
        self.send_request(
            "daemon/rebuild",
            serde_json::json!({ "path": path, "force": force }),
        )
        .await
    }

    /// Send a `daemon/cancel_rebuild` JSON-RPC request to cancel an
    /// in-flight rebuild for the workspace at `path`.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::send_request`].
    pub async fn cancel_rebuild(&mut self, path: &Path) -> Result<serde_json::Value, ClientError> {
        self.send_request("daemon/cancel_rebuild", serde_json::json!({ "path": path }))
            .await
    }

    /// Send a `daemon/load` JSON-RPC request to load a workspace and
    /// return the typed [`ResponseEnvelope<LoadResult>`].
    ///
    /// The daemon's `WorkspaceManager` will index the workspace (if
    /// not already loaded), cache the graph in memory, and start
    /// watching for file changes.
    ///
    /// `index_root` must be an absolute, canonicalized path. The daemon
    /// performs its own canonicalization as a defence-in-depth measure,
    /// but callers should canonicalize eagerly to avoid ambiguous path
    /// errors.
    ///
    /// Unlike [`Self::status`] which returns a raw
    /// [`serde_json::Value`], this method performs a strongly typed
    /// decode so schema drift between the client and daemon surfaces
    /// immediately as [`ClientError::SchemaMismatch`] instead of silent
    /// misreporting on the CLI side.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::send_request`]. Additionally
    /// returns [`ClientError::SchemaMismatch`] if the JSON-RPC
    /// `"result"` field cannot be decoded into
    /// `ResponseEnvelope<LoadResult>`. Notable daemon-side error codes:
    ///
    /// - `-32001` (`WorkspaceBuildFailed`) if the graph builder fails.
    /// - `-32602` (`InvalidArgument`) if `index_root` fails path
    ///   policy validation.
    pub async fn load(
        &mut self,
        index_root: &std::path::Path,
    ) -> Result<ResponseEnvelope<LoadResult>, ClientError> {
        let raw = self
            .send_request(
                "daemon/load",
                serde_json::json!({ "index_root": index_root }),
            )
            .await?;
        serde_json::from_value::<ResponseEnvelope<LoadResult>>(raw).map_err(|source| {
            ClientError::SchemaMismatch {
                method: "daemon/load",
                source,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Internal handshake driver.
// ---------------------------------------------------------------------------

/// Inner helper that performs the [`DaemonHello`] handshake on an
/// already-connected stream. Extracted so that:
///
/// - [`DaemonClient::connect_with_timeouts`] can call it after the
///   bounded platform connect step without code duplication.
/// - Tests can drive it directly via a [`tokio::io::duplex`] pair
///   without needing a real socket.
///
/// `socket_desc` is forwarded verbatim into
/// [`ClientError::HandshakeTimeout`] for diagnostic context (tests
/// pass `Path::new("<in-memory-duplex>")`).
pub(crate) async fn do_hello_handshake<S>(
    mut stream: S,
    socket_desc: &Path,
    handshake_timeout: Duration,
) -> Result<DaemonClient, ClientError>
where
    S: AsyncReadWrite + Send + 'static,
{
    let hello = DaemonHello {
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: 1,
        // STEP_6 (workspace-aware-cross-repo): the management client
        // surface does not bind a logical workspace at hello time —
        // callers that need cross-repo grouping pass the
        // `logical_workspace` payload on the per-method request (e.g.
        // `daemon/load`). The standalone `DaemonClient` keeps the
        // pre-STEP_6 anonymous semantics: every workspace it loads is
        // its own per-source-root entry.
        logical_workspace: None,
    };
    framing::write_frame_json(&mut stream, &hello).await?;

    let read_fut = framing::read_frame_json::<_, DaemonHelloResponse>(&mut stream);
    let response_outcome = match tokio::time::timeout(handshake_timeout, read_fut).await {
        Ok(inner) => inner,
        Err(_elapsed) => {
            return Err(ClientError::HandshakeTimeout {
                path: socket_desc.to_path_buf(),
                after: handshake_timeout,
            });
        }
    };

    let response: DaemonHelloResponse = match response_outcome? {
        Some(r) => r,
        None => return Err(ClientError::HelloEof),
    };

    // Step 3 — envelope_version check runs BEFORE the `compatible`
    // branch. The hello path uses `DaemonHelloResponse::envelope_version`
    // as its wire-format version signal, mirroring the shim path's
    // `ShimRegisterAck::envelope_version` check in `do_shim_handshake`.
    // A mismatched version must not be masked by a simultaneous
    // application-level `compatible = false` rejection.
    if response.envelope_version != ENVELOPE_VERSION {
        return Err(ClientError::EnvelopeVersionMismatch {
            got: response.envelope_version,
            expected: ENVELOPE_VERSION,
        });
    }

    // Step 4 — compatibility check.
    if !response.compatible {
        return Err(ClientError::HelloRejected);
    }

    Ok(DaemonClient {
        stream: Box::pin(stream),
        daemon_version: response.daemon_version,
        next_id: 1,
    })
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::duplex;

    use sqry_daemon_protocol::{
        DaemonHello, DaemonHelloResponse, ENVELOPE_VERSION, JsonRpcError, JsonRpcPayload,
    };

    // -----------------------------------------------------------------------
    // Fake-daemon helpers.
    // -----------------------------------------------------------------------

    /// Spawn a fake-daemon handler on the server side of a duplex pair.
    /// The `handler` closure receives the server side and drives the
    /// protocol exchange. Returns the client side + the handler's
    /// `JoinHandle`.
    async fn spawn_fake_daemon<F, Fut>(
        handler: F,
    ) -> (
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    )
    where
        F: FnOnce(tokio::io::DuplexStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
    {
        let (client_side, server_side) = duplex(65536);
        let handle = tokio::spawn(async move { handler(server_side).await });
        (client_side, handle)
    }

    /// Helper: read and validate the initial [`DaemonHello`] from the
    /// server end of a duplex stream.
    async fn read_hello(server: &mut tokio::io::DuplexStream) -> anyhow::Result<DaemonHello> {
        framing::read_frame_json::<_, DaemonHello>(server)
            .await?
            .ok_or_else(|| anyhow::anyhow!("expected DaemonHello, got EOF"))
    }

    /// Helper: write a compatible [`DaemonHelloResponse`].
    async fn write_hello_response(
        server: &mut tokio::io::DuplexStream,
        version: &str,
    ) -> anyhow::Result<()> {
        let resp = DaemonHelloResponse {
            compatible: true,
            daemon_version: version.to_owned(),
            envelope_version: ENVELOPE_VERSION,
        };
        framing::write_frame_json(server, &resp).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: happy-path hello handshake.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_hello_handshake_happy_path() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let hello = read_hello(&mut server).await?;
            // Validate the hello fields.
            assert_eq!(hello.protocol_version, 1, "protocol_version must be 1");
            assert!(
                !hello.client_version.is_empty(),
                "client_version must be non-empty"
            );
            write_hello_response(&mut server, "8.0.6").await?;
            Ok(())
        })
        .await;

        // Build DaemonClient bypassing platform_connect by using the
        // already-connected stream directly through do_hello_handshake.
        let client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello handshake must succeed");

        assert_eq!(client.daemon_version(), "8.0.6");
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: compatible=false returns HelloRejected.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_hello_rejected_returns_error() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            let resp = DaemonHelloResponse {
                compatible: false,
                daemon_version: "99.0.0".to_owned(),
                envelope_version: ENVELOPE_VERSION,
            };
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let err = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect_err("must fail on compatible=false");

        assert!(
            matches!(err, ClientError::HelloRejected),
            "expected HelloRejected, got {err:?}"
        );
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: daemon closes before hello response → HelloEof.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_hello_eof_returns_error() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            // Close without writing response.
            drop(server);
            Ok(())
        })
        .await;

        let err = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect_err("must fail on EOF");

        assert!(
            matches!(err, ClientError::HelloEof),
            "expected HelloEof, got {err:?}"
        );
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: daemon advertises mismatched envelope_version in hello
    // response → EnvelopeVersionMismatch (checked BEFORE compatible).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_hello_envelope_mismatch_returns_error() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            // Claim a future envelope version. `compatible: true` here is
            // load-bearing: proves the version check fires BEFORE the
            // compatible branch, mirroring the shim path's ordering.
            let resp = DaemonHelloResponse {
                compatible: true,
                daemon_version: "future-daemon".to_owned(),
                envelope_version: 99,
            };
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let err = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect_err("must fail on envelope_version mismatch");

        match err {
            ClientError::EnvelopeVersionMismatch { got, expected } => {
                assert_eq!(got, 99, "daemon advertised 99");
                assert_eq!(expected, ENVELOPE_VERSION, "client expects current");
            }
            other => panic!("expected EnvelopeVersionMismatch, got {other:?}"),
        }
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: send_request returns result on success.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_send_request_returns_result() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            // Read the request.
            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected JsonRpcRequest, got EOF"))?;
            assert_eq!(req.method, "test/method");
            assert_eq!(req.params, serde_json::json!({"key": "value"}));

            // Respond with success.
            let resp = JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: req.id.clone(),
                payload: JsonRpcPayload::Success {
                    result: serde_json::json!({"answer": 42}),
                },
            };
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        let result = client
            .send_request("test/method", serde_json::json!({"key": "value"}))
            .await
            .expect("send_request must succeed");

        assert_eq!(result, serde_json::json!({"answer": 42}));
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: send_request surfaces RpcError on error response.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_send_request_returns_rpc_error() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;

            let resp = JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: req.id.clone(),
                payload: JsonRpcPayload::Error {
                    error: JsonRpcError {
                        code: -32603,
                        message: "Internal error".to_owned(),
                        data: Some(serde_json::json!({"detail": "disk full"})),
                    },
                },
            };
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        let err = client
            .send_request("daemon/anything", serde_json::json!({}))
            .await
            .expect_err("must fail with RpcError");

        match err {
            ClientError::RpcError {
                code,
                message,
                data,
            } => {
                assert_eq!(code, -32603);
                assert_eq!(message, "Internal error");
                assert_eq!(data, Some(serde_json::json!({"detail": "disk full"})));
            }
            other => panic!("expected RpcError, got {other:?}"),
        }
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: send_request id correlation.
    //
    // Covers two aspects of the MINOR finding:
    //   1. Sequential calls use ids 1, 2, 3, ... (monotone increment).
    //   2. A mismatched response id surfaces ClientError::Io(InvalidData).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_send_request_id_correlation() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            // First request → id 1.
            let req1: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected req 1"))?;
            assert_eq!(
                req1.id,
                Some(JsonRpcId::I64(1)),
                "first request id must be 1"
            );
            let resp1 = JsonRpcResponse::success(req1.id.clone(), serde_json::json!(1));
            framing::write_frame_json(&mut server, &resp1).await?;

            // Second request → id 2.
            let req2: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected req 2"))?;
            assert_eq!(
                req2.id,
                Some(JsonRpcId::I64(2)),
                "second request id must be 2"
            );
            let resp2 = JsonRpcResponse::success(req2.id.clone(), serde_json::json!(2));
            framing::write_frame_json(&mut server, &resp2).await?;

            // Third request → id 3. Reply with WRONG id (99).
            // The client must surface ClientError::Io(InvalidData).
            let req3: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected req 3"))?;
            assert_eq!(
                req3.id,
                Some(JsonRpcId::I64(3)),
                "third request id must be 3"
            );
            let bad_resp = JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: Some(JsonRpcId::I64(99)), // wrong!
                payload: JsonRpcPayload::Success {
                    result: serde_json::json!("wrong"),
                },
            };
            framing::write_frame_json(&mut server, &bad_resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        // First two calls succeed with the correct id.
        let r1 = client
            .send_request("a/1", serde_json::json!({}))
            .await
            .expect("call 1 ok");
        assert_eq!(r1, serde_json::json!(1));

        let r2 = client
            .send_request("a/2", serde_json::json!({}))
            .await
            .expect("call 2 ok");
        assert_eq!(r2, serde_json::json!(2));

        // Third call must fail with Io(InvalidData) due to id mismatch.
        let err = client
            .send_request("a/3", serde_json::json!({}))
            .await
            .expect_err("must fail on id mismatch");
        match err {
            ClientError::Io(e) => {
                assert_eq!(
                    e.kind(),
                    std::io::ErrorKind::InvalidData,
                    "kind must be InvalidData"
                );
                assert!(
                    e.to_string().contains("mismatch"),
                    "error message must mention mismatch: {e}"
                );
            }
            other => panic!("expected Io(InvalidData), got {other:?}"),
        }

        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: stop() sends daemon/stop with empty params.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_stop_sends_daemon_stop() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;
            assert_eq!(req.method, "daemon/stop");
            assert_eq!(req.params, serde_json::json!({}));

            let resp = JsonRpcResponse::success(req.id.clone(), serde_json::json!({"ok": true}));
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        client.stop().await.expect("stop must succeed");
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: status() sends daemon/status with empty params.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_status_sends_daemon_status() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;
            assert_eq!(req.method, "daemon/status");
            assert_eq!(req.params, serde_json::json!({}));

            let status_payload = serde_json::json!({
                "version": "8.0.6",
                "uptime_secs": 3600,
                "workspaces": []
            });
            let resp = JsonRpcResponse::success(req.id.clone(), status_payload.clone());
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        let status = client.status().await.expect("status must succeed");
        assert_eq!(status["version"], "8.0.6");
        assert_eq!(status["uptime_secs"], 3600);
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: load() sends daemon/load with index_root.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_load_sends_daemon_load() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;
            assert_eq!(req.method, "daemon/load");
            // Verify the params contain index_root.
            let index_root = req
                .params
                .get("index_root")
                .expect("params must have index_root");
            assert_eq!(index_root, "/repos/my-project");

            let load_result = serde_json::json!({
                "result": {
                    "root": "/repos/my-project",
                    "current_bytes": 2_097_152_u64,
                    "state": "Loaded"
                },
                "meta": { "stale": false, "daemon_version": "8.0.6" }
            });
            let resp = JsonRpcResponse::success(req.id.clone(), load_result.clone());
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        let envelope = client
            .load(Path::new("/repos/my-project"))
            .await
            .expect("load must succeed");

        // Verify the typed result contains the expected fields —
        // schema mismatches would now surface as
        // `ClientError::SchemaMismatch` instead of a silent
        // opportunistic parse.
        assert_eq!(
            envelope.result.root,
            std::path::PathBuf::from("/repos/my-project")
        );
        assert_eq!(
            envelope.result.state,
            sqry_daemon_protocol::WorkspaceState::Loaded
        );
        assert_eq!(envelope.result.current_bytes, 2_097_152_u64);
        assert_eq!(envelope.meta.daemon_version, "8.0.6");
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: DaemonClient::load surfaces schema mismatches.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_load_schema_mismatch_surfaces() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "9.0.0").await?;

            // Read the JSON-RPC request and reply with an intentionally
            // malformed payload: the inner `result` is a plain string
            // (not an object), so deserialisation into
            // `ResponseEnvelope<LoadResult>` must fail at the `result`
            // layer rather than silently returning defaults.
            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;
            let bad_payload = serde_json::json!({
                "result": "not-an-object",
                "meta": { "stale": false, "daemon_version": "9.0.0" }
            });
            let resp = JsonRpcResponse::success(req.id.clone(), bad_payload);
            framing::write_frame_json(&mut server, &resp).await?;
            Ok(())
        })
        .await;

        let mut client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello ok");

        let err = client
            .load(Path::new("/repos/my-project"))
            .await
            .expect_err("schema mismatch must fail");
        match err {
            ClientError::SchemaMismatch { method, .. } => {
                assert_eq!(method, "daemon/load");
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------------
    // Test: connect timeout (via apply_connect_timeout).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_connect_timeout_returns_error() {
        use crate::apply_connect_timeout;

        let socket_path = PathBuf::from("/tmp/sqry-mgmt-client-test-fake.sock");
        let short_timeout = Duration::from_millis(50);

        // A deliberately slow "connect" future that never completes
        // within the budget. The Ok branch is unreachable in practice
        // (the timeout fires first), but must type-check — use a never-
        // reached Error arm so rustc can infer `ClientError` for `E`.
        let slow_fut = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            // This branch is never reached; the timeout fires first.
            Err::<Pin<Box<dyn AsyncReadWrite + Send>>, ClientError>(ClientError::Io(
                std::io::Error::other("unreachable"),
            ))
        };

        let start = std::time::Instant::now();
        // `Pin<Box<dyn AsyncReadWrite + Send>>` does not implement Debug,
        // so `.expect_err()` is unusable here — use explicit match instead.
        let outcome = apply_connect_timeout(slow_fut, &socket_path, short_timeout).await;
        let elapsed = start.elapsed();
        let err = match outcome {
            Err(e) => e,
            Ok(_) => panic!("expected Err(ConnectTimeout), got Ok"),
        };

        match err {
            ClientError::ConnectTimeout { path, after } => {
                assert_eq!(path, socket_path);
                assert_eq!(after, short_timeout);
            }
            other => panic!("expected ConnectTimeout, got {other:?}"),
        }
        assert!(elapsed >= short_timeout);
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout should fire well before 30s sleep"
        );
    }

    // -----------------------------------------------------------------------
    // Test: handshake timeout (daemon accepts but never writes response).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn daemon_client_handshake_timeout_returns_error() {
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            // Consume hello but never respond; sleep past the client's budget.
            let _hello = read_hello(&mut server).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await;

        let short_timeout = Duration::from_millis(100);
        let sentinel = Path::new("<in-memory-duplex>");

        let err = do_hello_handshake(client_stream, sentinel, short_timeout)
            .await
            .expect_err("must time out");

        match err {
            ClientError::HandshakeTimeout { path, after } => {
                assert_eq!(path, sentinel);
                assert_eq!(after, short_timeout);
            }
            other => panic!("expected HandshakeTimeout, got {other:?}"),
        }

        handle.await.expect("join").expect("server ok");
    }
}
