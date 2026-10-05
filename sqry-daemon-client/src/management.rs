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
    JsonRpcResponse, JsonRpcVersion, ListRevisionsRequest, ListRevisionsResult, LoadResult,
    LoadRevisionRequest, LoadRevisionResult, PruneRevisionsRequest, PruneRevisionsResult,
    ResponseEnvelope, RevisionStatus, RevisionStatusRequest, UnloadRevisionRequest,
    UnloadRevisionResult, framing,
};

use crate::{AsyncReadWrite, ClientError, DEFAULT_CONNECT_TIMEOUT, platform_connect};

#[derive(serde::Deserialize)]
struct ActiveArtifactsBody {
    artifacts: Vec<std::path::PathBuf>,
}

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
/// The `daemon/rebuild` request fields beyond `path` (surface parity W4,
/// design W4-D8). `Default` is a plain `force = false` rebuild that reuses
/// the macro options the workspace manifest records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildOptions {
    /// Force a full rebuild from scratch.
    pub force: bool,
    /// `Some(flags)`: replace the recorded `--cfg` flags; `None`: keep them.
    pub cfg_flags: Option<Vec<String>>,
    /// `Some(dir)`: replace the recorded expand cache; `None`: keep it.
    pub expand_cache: Option<std::path::PathBuf>,
    /// Drop the recorded macro options before applying the two above.
    pub reset_macro_options: bool,
}

/// Encode one request parameter as JSON, refusing what JSON text cannot
/// carry (a path that is not valid UTF-8) instead of panicking the way
/// `serde_json::json!` does on that refusal.
fn encode_param<T: serde::Serialize + ?Sized>(
    method: &'static str,
    field: &'static str,
    value: &T,
) -> Result<serde_json::Value, ClientError> {
    serde_json::to_value(value).map_err(|source| ClientError::RequestEncoding {
        method,
        field,
        path: None,
        source,
    })
}

/// [`encode_param`] for a path, naming the path in the refusal.
fn encode_path_param(
    method: &'static str,
    field: &'static str,
    path: &Path,
) -> Result<serde_json::Value, ClientError> {
    serde_json::to_value(path).map_err(|source| ClientError::RequestEncoding {
        method,
        field,
        path: Some(path.to_path_buf()),
        source,
    })
}

/// The `daemon/rebuild` parameters for `path` and `options`: only the
/// fields the caller set, so a default [`RebuildOptions`] is byte-identical
/// to the plain `{path, force}` request.
fn rebuild_params(path: &Path, options: &RebuildOptions) -> Result<serde_json::Value, ClientError> {
    const METHOD: &str = "daemon/rebuild";
    let mut params = serde_json::Map::new();
    params.insert("path".to_string(), encode_path_param(METHOD, "path", path)?);
    params.insert("force".to_string(), serde_json::Value::Bool(options.force));
    if let Some(cfg_flags) = &options.cfg_flags {
        params.insert(
            "cfg_flags".to_string(),
            encode_param(METHOD, "cfg_flags", cfg_flags)?,
        );
    }
    if let Some(expand_cache) = &options.expand_cache {
        params.insert(
            "expand_cache".to_string(),
            encode_path_param(METHOD, "expand_cache", expand_cache)?,
        );
    }
    if options.reset_macro_options {
        params.insert(
            "reset_macro_options".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    Ok(serde_json::Value::Object(params))
}

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
        let expected_id = self.write_request(method, params).await?;

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

    /// Write one JSON-RPC 2.0 request frame (flushed) and return its id,
    /// without reading the response.
    async fn write_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<JsonRpcId, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let request_id = JsonRpcId::I64(id);
        let request = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: Some(request_id.clone()),
            method: method.to_owned(),
            params,
        };

        framing::write_frame_json(&mut self.stream, &request).await?;
        Ok(request_id)
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
    /// - [`ClientError::RequestEncoding`] if `path` is not valid UTF-8;
    ///   nothing is sent.
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
        let path = encode_path_param("daemon/reset", "path", path)?;
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
        let body_value = raw
            .get("result")
            .cloned()
            .or_else(|| Some(raw.clone()))
            .unwrap_or(raw);
        let body: ActiveArtifactsBody =
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
    /// - [`ClientError::RequestEncoding`] if `path` is not valid UTF-8;
    ///   nothing is sent.
    /// - [`ClientError::RpcError`] with code `-32004` if the workspace
    ///   is not loaded.
    /// - [`ClientError::RpcError`] with code `-32001` if the rebuild fails.
    /// - Propagates other errors from [`Self::send_request`].
    pub async fn rebuild(
        &mut self,
        path: &Path,
        force: bool,
    ) -> Result<serde_json::Value, ClientError> {
        let path = encode_path_param("daemon/rebuild", "path", path)?;
        self.send_request(
            "daemon/rebuild",
            serde_json::json!({ "path": path, "force": force }),
        )
        .await
    }

    /// Send a `daemon/rebuild` JSON-RPC request carrying the macro build
    /// option fields as well as `force` (surface parity W4, design W4-D8).
    ///
    /// Only the fields the caller set are sent: an absent `cfg_flags` or
    /// `expand_cache` keeps what the workspace manifest records, and
    /// `reset_macro_options` is sent only when `true`, so a request with
    /// default [`RebuildOptions`] is byte-identical to [`Self::rebuild`]'s.
    /// A relative `expand_cache` is sent as given and the daemon resolves it
    /// against the workspace root; the CLI makes its flag absolute first.
    ///
    /// # Errors
    ///
    /// - [`ClientError::RequestEncoding`] if `path` or `expand_cache` is not
    ///   valid UTF-8; nothing is sent.
    /// - [`ClientError::RpcError`] with code `-32004` if the workspace is
    ///   not loaded, `-32001` if the rebuild fails or the manifest cannot be
    ///   read, `-32005` if the manifest names a plugin id the daemon did not
    ///   compile, and `-32022` if the expand cache directory does not exist
    ///   or is not a directory.
    /// - [`ClientError::RpcError`] with code `-32602` if the request is
    ///   refused as an argument: the expand cache directory is empty, or its
    ///   canonical path is not valid UTF-8, or another request with
    ///   different macro options is already waiting.
    /// - Propagates other errors from [`Self::send_request`].
    pub async fn rebuild_with_options(
        &mut self,
        path: &Path,
        options: &RebuildOptions,
    ) -> Result<serde_json::Value, ClientError> {
        let params = rebuild_params(path, options)?;
        self.send_request("daemon/rebuild", params).await
    }

    /// Send the [`Self::rebuild_with_options`] request and return once it is
    /// written to the daemon socket, without reading the response
    /// (`sqry daemon rebuild --timeout 0`).
    ///
    /// The daemon reads a request frame whole before it acts on it, and it
    /// does not watch the connection while the rebuild runs, so the rebuild
    /// proceeds after this client closes the connection; only the daemon's
    /// reply, which nobody reads, is lost. The connection must not be used
    /// for another request afterwards: its unread reply would answer that
    /// request's read.
    ///
    /// # Errors
    ///
    /// - [`ClientError::RequestEncoding`] if `path` or `expand_cache` is not
    ///   valid UTF-8; nothing is sent.
    /// - [`ClientError::Frame`] if the frame cannot be written.
    pub async fn send_rebuild_with_options(
        &mut self,
        path: &Path,
        options: &RebuildOptions,
    ) -> Result<(), ClientError> {
        let params = rebuild_params(path, options)?;
        self.write_request("daemon/rebuild", params).await?;
        Ok(())
    }

    /// Send a `daemon/cancel_rebuild` JSON-RPC request to cancel an
    /// in-flight rebuild for the workspace at `path`.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `path` is not valid UTF-8
    /// (nothing is sent); otherwise propagates errors from
    /// [`Self::send_request`].
    pub async fn cancel_rebuild(&mut self, path: &Path) -> Result<serde_json::Value, ClientError> {
        let path = encode_path_param("daemon/cancel_rebuild", "path", path)?;
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
    /// [`ClientError::RequestEncoding`] if `index_root` is not valid UTF-8
    /// (nothing is sent). Otherwise propagates errors from
    /// [`Self::send_request`], and returns [`ClientError::SchemaMismatch`]
    /// if the JSON-RPC `"result"` field cannot be decoded into
    /// `ResponseEnvelope<LoadResult>`. Notable daemon-side error codes:
    ///
    /// - `-32001` (`WorkspaceBuildFailed`) if the graph builder fails.
    /// - `-32602` (`InvalidArgument`) if `index_root` fails path
    ///   policy validation.
    pub async fn load(
        &mut self,
        index_root: &std::path::Path,
    ) -> Result<ResponseEnvelope<LoadResult>, ClientError> {
        let index_root = encode_path_param("daemon/load", "index_root", index_root)?;
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

    /// Send a `daemon/loadRevision` request and return the typed result.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `request.root` is not valid
    /// UTF-8 (nothing is sent). Otherwise propagates errors from
    /// [`Self::send_request`], and returns [`ClientError::SchemaMismatch`]
    /// if the daemon response does not decode as
    /// [`ResponseEnvelope<LoadRevisionResult>`].
    pub async fn load_revision(
        &mut self,
        request: LoadRevisionRequest,
    ) -> Result<ResponseEnvelope<LoadRevisionResult>, ClientError> {
        let raw = self
            .send_request(
                "daemon/loadRevision",
                encode_param("daemon/loadRevision", "params", &request)?,
            )
            .await?;
        decode_envelope("daemon/loadRevision", raw)
    }

    /// Send a `daemon/unloadRevision` request.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `request` cannot be encoded as
    /// JSON (nothing is sent); it carries a revision id and a flag, so no
    /// value of today's request type is refused. Otherwise propagates
    /// errors from [`Self::send_request`] and strict response decoding
    /// errors.
    pub async fn unload_revision(
        &mut self,
        request: UnloadRevisionRequest,
    ) -> Result<ResponseEnvelope<UnloadRevisionResult>, ClientError> {
        let raw = self
            .send_request(
                "daemon/unloadRevision",
                encode_param("daemon/unloadRevision", "params", &request)?,
            )
            .await?;
        decode_envelope("daemon/unloadRevision", raw)
    }

    /// Send a `daemon/listRevisions` request.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `request.root` is not valid
    /// UTF-8 (nothing is sent). Otherwise propagates errors from
    /// [`Self::send_request`] and strict response decoding errors.
    pub async fn list_revisions(
        &mut self,
        request: ListRevisionsRequest,
    ) -> Result<ResponseEnvelope<ListRevisionsResult>, ClientError> {
        let raw = self
            .send_request(
                "daemon/listRevisions",
                encode_param("daemon/listRevisions", "params", &request)?,
            )
            .await?;
        decode_envelope("daemon/listRevisions", raw)
    }

    /// Send a `daemon/revisionStatus` request.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `request` cannot be encoded as
    /// JSON (nothing is sent); it carries a revision id, so no value of
    /// today's request type is refused. Otherwise propagates errors from
    /// [`Self::send_request`] and strict response decoding errors.
    pub async fn revision_status(
        &mut self,
        request: RevisionStatusRequest,
    ) -> Result<ResponseEnvelope<RevisionStatus>, ClientError> {
        let raw = self
            .send_request(
                "daemon/revisionStatus",
                encode_param("daemon/revisionStatus", "params", &request)?,
            )
            .await?;
        decode_envelope("daemon/revisionStatus", raw)
    }

    /// Send a `daemon/pruneRevisions` request.
    ///
    /// # Errors
    ///
    /// [`ClientError::RequestEncoding`] if `request.root` is not valid
    /// UTF-8 (nothing is sent). Otherwise propagates errors from
    /// [`Self::send_request`] and strict response decoding errors.
    pub async fn prune_revisions(
        &mut self,
        request: PruneRevisionsRequest,
    ) -> Result<ResponseEnvelope<PruneRevisionsResult>, ClientError> {
        let raw = self
            .send_request(
                "daemon/pruneRevisions",
                encode_param("daemon/pruneRevisions", "params", &request)?,
            )
            .await?;
        decode_envelope("daemon/pruneRevisions", raw)
    }
}

fn decode_envelope<T>(
    method: &'static str,
    value: serde_json::Value,
) -> Result<ResponseEnvelope<T>, ClientError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<ResponseEnvelope<T>>(value)
        .map_err(|source| ClientError::SchemaMismatch { method, source })
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

    #[tokio::test]
    async fn daemon_client_load_revision_sends_typed_request() {
        use sqry_daemon_protocol::{
            ArtifactId, ArtifactInputDigest, LoadRevisionRequest, ObjectFormat, RepositoryIdentity,
            ResidentHandleKind, ResolvedRevision, RevisionId, RevisionLoadState, RevisionSelector,
            RevisionStatus, SourceByteMode,
        };

        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "8.0.6").await?;

            let req: JsonRpcRequest = framing::read_frame_json::<_, JsonRpcRequest>(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected request"))?;
            assert_eq!(req.method, "daemon/loadRevision");
            assert_eq!(req.params["selector"]["kind"], "ref");
            assert_eq!(req.params["selector"]["name"], "main");

            let payload = serde_json::json!({
                "result": {
                    "revision_id": "rev-1",
                    "artifact_id": "artifact-1",
                    "artifact_inputs": {
                        "schema_version": 1,
                        "digest": "digest-1"
                    },
                    "resolved": {
                        "selector": { "kind": "ref", "name": "main" },
                        "repository": {
                            "repo_identity_hash": "repo-hash",
                            "object_format": "sha1"
                        },
                        "commit_oid": "1111111111111111111111111111111111111111",
                        "tree_oid": "2222222222222222222222222222222222222222",
                        "object_format": "sha1",
                        "source_byte_mode": "raw_git_objects",
                        "resolved_at": "2026-06-26T00:00:00Z"
                    },
                    "status": {
                        "revision_id": "rev-1",
                        "handle_kind": "immutable_revision",
                        "resolved": {
                            "selector": { "kind": "ref", "name": "main" },
                            "repository": {
                                "repo_identity_hash": "repo-hash",
                                "object_format": "sha1"
                            },
                            "commit_oid": "1111111111111111111111111111111111111111",
                            "tree_oid": "2222222222222222222222222222222222222222",
                            "object_format": "sha1",
                            "source_byte_mode": "raw_git_objects",
                            "resolved_at": "2026-06-26T00:00:00Z"
                        },
                        "artifact_id": "artifact-1",
                        "artifact_inputs": {
                            "schema_version": 1,
                            "digest": "digest-1"
                        },
                        "state": "loaded",
                        "pinned": false,
                        "active_queries": 0,
                        "memory_bytes": 128
                    }
                },
                "meta": { "stale": false, "daemon_version": "8.0.6" }
            });
            let resp = JsonRpcResponse::success(req.id.clone(), payload);
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

        let request = LoadRevisionRequest {
            root: PathBuf::from("/repos/my-project"),
            selector: RevisionSelector::Ref {
                name: "main".to_owned(),
            },
            source_byte_mode: Some(SourceByteMode::RawGitObjects),
            pin: false,
        };
        let envelope = client
            .load_revision(request)
            .await
            .expect("loadRevision must decode");

        assert_eq!(envelope.result.revision_id, RevisionId("rev-1".to_owned()));
        assert_eq!(
            envelope.result.artifact_id,
            ArtifactId("artifact-1".to_owned())
        );
        assert_eq!(
            envelope.result.artifact_inputs,
            ArtifactInputDigest {
                schema_version: 1,
                digest: "digest-1".to_owned()
            }
        );
        assert_eq!(envelope.result.resolved.object_format, ObjectFormat::Sha1);
        assert_eq!(
            envelope.result.resolved.source_byte_mode,
            SourceByteMode::RawGitObjects
        );
        assert_eq!(
            envelope.result.status.handle_kind,
            ResidentHandleKind::ImmutableRevision
        );
        assert_eq!(envelope.result.status.state, RevisionLoadState::Loaded);
        assert_eq!(
            envelope.result.status.resolved.repository,
            RepositoryIdentity {
                repo_identity_hash: "repo-hash".to_owned(),
                object_format: ObjectFormat::Sha1,
                remote_fingerprint: None,
            }
        );
        let expected_resolved = ResolvedRevision {
            selector: RevisionSelector::Ref {
                name: "main".to_owned(),
            },
            repository: RepositoryIdentity {
                repo_identity_hash: "repo-hash".to_owned(),
                object_format: ObjectFormat::Sha1,
                remote_fingerprint: None,
            },
            commit_oid: Some("1111111111111111111111111111111111111111".to_owned()),
            tree_oid: "2222222222222222222222222222222222222222".to_owned(),
            object_format: ObjectFormat::Sha1,
            source_byte_mode: SourceByteMode::RawGitObjects,
            resolved_at: "2026-06-26T00:00:00Z".to_owned(),
        };
        assert_eq!(envelope.result.resolved, expected_resolved);
        let _status: RevisionStatus = envelope.result.status;
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

    // -----------------------------------------------------------------------
    // Request encoding: `daemon/rebuild` fields and paths JSON cannot carry.
    // -----------------------------------------------------------------------

    /// The `daemon/rebuild` parameters carry only what the caller set: a
    /// default request is `{path, force}`, and each macro field appears only
    /// when given, `reset_macro_options` only when `true`.
    #[test]
    fn rebuild_params_carry_only_the_fields_given() {
        let path = Path::new("/ws");
        assert_eq!(
            rebuild_params(path, &RebuildOptions::default()).expect("encodes"),
            serde_json::json!({ "path": "/ws", "force": false })
        );
        let options = RebuildOptions {
            force: true,
            cfg_flags: Some(vec!["test".to_string(), "feature=x".to_string()]),
            expand_cache: Some(PathBuf::from("/ws/cache")),
            reset_macro_options: true,
        };
        assert_eq!(
            rebuild_params(path, &options).expect("encodes"),
            serde_json::json!({
                "path": "/ws",
                "force": true,
                "cfg_flags": ["test", "feature=x"],
                "expand_cache": "/ws/cache",
                "reset_macro_options": true,
            })
        );
        let cleared = RebuildOptions {
            cfg_flags: Some(vec![]),
            ..RebuildOptions::default()
        };
        assert_eq!(
            rebuild_params(path, &cleared).expect("encodes"),
            serde_json::json!({ "path": "/ws", "force": false, "cfg_flags": [] }),
            "an explicit empty list is sent: it clears the recorded flags"
        );
    }

    /// A connected client whose fake daemon completes the handshake and then
    /// reports every request frame it reads (`None` once the client closes).
    #[allow(clippy::type_complexity)]
    async fn client_recording_requests() -> (
        DaemonClient,
        tokio::task::JoinHandle<anyhow::Result<()>>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = std::sync::Arc::clone(&seen);
        let (client_stream, handle) = spawn_fake_daemon(|mut server| async move {
            let _hello = read_hello(&mut server).await?;
            write_hello_response(&mut server, "test").await?;
            while let Some(request) =
                framing::read_frame_json::<_, serde_json::Value>(&mut server).await?
            {
                record.lock().unwrap().push(request);
            }
            Ok(())
        })
        .await;
        let client = do_hello_handshake(
            client_stream,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HELLO_TIMEOUT,
        )
        .await
        .expect("hello handshake");
        (client, handle, seen)
    }

    /// `send_rebuild_with_options` writes the same request frame
    /// `rebuild_with_options` would and returns without reading a reply
    /// (the fake daemon never answers).
    #[tokio::test]
    async fn send_rebuild_with_options_writes_the_request_and_reads_nothing() {
        let (mut client, handle, seen) = client_recording_requests().await;
        let options = RebuildOptions {
            force: true,
            cfg_flags: Some(vec!["test".to_string()]),
            expand_cache: None,
            reset_macro_options: true,
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            client.send_rebuild_with_options(Path::new("/ws"), &options),
        )
        .await
        .expect("returns without a reply")
        .expect("the frame is written");
        drop(client);
        handle.await.expect("join").expect("fake daemon");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0]["method"], "daemon/rebuild");
        assert_eq!(
            seen[0]["params"],
            rebuild_params(Path::new("/ws"), &options).expect("encodes")
        );
    }

    /// Every management call that carries a path refuses one that is not
    /// valid UTF-8 with `RequestEncoding`, naming the method and the field,
    /// and sends nothing: `serde_json::json!` panicked on these, which took
    /// `sqry daemon rebuild` down with exit 101. Bounded by `grep -n "json!("
    /// sqry-daemon-client/src/management.rs`: the path-carrying sites are
    /// `reset`, `rebuild`, `rebuild_with_options` (two fields),
    /// `send_rebuild_with_options`, `cancel_rebuild`, `load`, and the typed
    /// requests holding a root (`loadRevision`, `listRevisions`,
    /// `pruneRevisions`).
    #[cfg(unix)]
    #[tokio::test]
    async fn path_parameters_that_are_not_utf8_are_refused_and_never_sent() {
        use std::os::unix::ffi::OsStringExt;

        let mut name = b"/ws-".to_vec();
        name.push(0xff);
        let bad = PathBuf::from(std::ffi::OsString::from_vec(name));

        let (mut client, handle, seen) = client_recording_requests().await;
        let mut outcomes: Vec<(&str, Result<(), ClientError>)> = Vec::new();
        outcomes.push(("reset", client.reset(&bad, false).await.map(drop)));
        outcomes.push(("rebuild", client.rebuild(&bad, false).await.map(drop)));
        outcomes.push((
            "rebuild_with_options path",
            client
                .rebuild_with_options(&bad, &RebuildOptions::default())
                .await
                .map(drop),
        ));
        let bad_cache = RebuildOptions {
            expand_cache: Some(bad.clone()),
            ..RebuildOptions::default()
        };
        outcomes.push((
            "rebuild_with_options expand_cache",
            client
                .rebuild_with_options(Path::new("/ws"), &bad_cache)
                .await
                .map(drop),
        ));
        outcomes.push((
            "send_rebuild_with_options",
            client
                .send_rebuild_with_options(Path::new("/ws"), &bad_cache)
                .await,
        ));
        outcomes.push((
            "cancel_rebuild",
            client.cancel_rebuild(&bad).await.map(drop),
        ));
        outcomes.push(("load", client.load(&bad).await.map(drop)));
        outcomes.push((
            "load_revision",
            client
                .load_revision(LoadRevisionRequest {
                    root: bad.clone(),
                    selector: sqry_daemon_protocol::RevisionSelector::Ref {
                        name: "main".to_string(),
                    },
                    source_byte_mode: None,
                    pin: false,
                })
                .await
                .map(drop),
        ));
        outcomes.push((
            "list_revisions",
            client
                .list_revisions(ListRevisionsRequest {
                    root: Some(bad.clone()),
                    include_unloaded: false,
                })
                .await
                .map(drop),
        ));
        outcomes.push((
            "prune_revisions",
            client
                .prune_revisions(PruneRevisionsRequest {
                    root: Some(bad.clone()),
                    apply: false,
                })
                .await
                .map(drop),
        ));
        assert_eq!(outcomes.len(), 10);
        for (site, outcome) in &outcomes {
            match outcome {
                Err(err @ ClientError::RequestEncoding { .. }) => {
                    let rendered = err.to_string();
                    assert!(
                        rendered.contains("invalid UTF-8") && rendered.contains("nothing was sent"),
                        "{site}: {rendered}"
                    );
                }
                other => panic!("{site}: expected RequestEncoding, got {other:?}"),
            }
        }
        drop(client);
        handle.await.expect("join").expect("fake daemon");
        let seen = seen.lock().unwrap().clone();
        assert!(seen.is_empty(), "nothing was sent: {seen:?}");
    }
}
