//! Connection router.
//!
//! Phase 8a handled exactly one first-frame shape: [`DaemonHello`].
//! Phase 8c (U10) extends [`run_connection`] to peek the first frame as
//! a `serde_json::Value` and shape-discriminate:
//!
//! - `{protocol, pid, ...}` → [`ShimRegister`] → [`run_shim_connection`]
//!   wires the stream halves into the LSP or MCP byte-pump host.
//! - otherwise → [`run_hello_connection`] (Phase 8a JSON-RPC loop,
//!   preserved bit-for-bit).
//!
//! The split keeps the shim and management paths isolated: a shim
//! connection never enters the JSON-RPC request loop (and vice versa),
//! so a buggy or malicious client cannot smuggle one frame shape into
//! the other's handler by tampering with later frames.
//!
//! After a successful `daemon/hello` handshake [`run_hello_connection`]
//! enters a JSON-RPC 2.0 request loop that handles both single
//! requests and batches. See [`dispatch_batch`] for the batch semantics.
//!
//! # Shim lifecycle
//!
//! [`run_shim_connection`] performs the atomic admission step via
//! [`ShimRegistry::try_register_bounded`] (iter-1 B2 fix: single
//! mutex-guard admission), writes the
//! [`ShimRegisterAck`] reply frame, and then hands the raw stream
//! halves to either
//! [`sqry_lsp::daemon_host::host_on_streams`] or
//! [`crate::mcp_host::host_mcp_on_streams`]. The [`ShimHandle`]
//! returned by `try_register_bounded` is held in a local binding for
//! the host-call duration and its RAII `Drop` removes the registry
//! entry when the host returns — whether that's a peer disconnect,
//! shutdown cancellation, or a host-level error.

use std::io;
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::ENVELOPE_VERSION;

use super::framing::{FrameError, read_frame, write_frame_json};
use super::methods::{
    ConnectionState, HandlerContext, dispatch as dispatch_request, internal_error_response,
};
use super::protocol::{
    DaemonHello, DaemonHelloResponse, JsonRpcResponse, ShimProtocol, ShimRegister, ShimRegisterAck,
};
use super::shim_registry::ShimHandle;
use super::validation::{ValidationError, parse_error_response, validate_request_value};

/// Connection-level error. Only covers cases that justify dropping the
/// connection — JSON-RPC-layer failures return responses instead.
#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("connection io: {0}")]
    Io(#[from] io::Error),
}

impl From<FrameError> for ConnectionError {
    fn from(err: FrameError) -> Self {
        match err {
            FrameError::Io(e) => Self::Io(e),
            FrameError::Json(e) => Self::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame json: {e}"),
            )),
        }
    }
}

/// Result of processing one incoming frame.
#[derive(Debug)]
enum FrameResponse {
    /// No response (batch of only notifications OR single notification).
    None,
    /// Single response frame.
    Single(JsonRpcResponse),
    /// Batch response — one per non-notification element.
    Batch(Vec<JsonRpcResponse>),
    /// Parse error — caller writes the response then closes the
    /// connection because framing state is indeterminate.
    ParseError(JsonRpcResponse),
}

/// Entry-point called by [`super::server::IpcServer::run`] for every
/// accepted connection. Consumes the stream.
///
/// Phase 8c U10 first-frame dispatcher:
///
/// - The very first frame is read once as raw bytes, parsed as
///   `serde_json::Value`, and shape-discriminated on the presence of
///   both `protocol` and `pid` object keys. This is the minimum set of
///   keys [`ShimRegister`] requires under `deny_unknown_fields`, and
///   neither key overlaps with [`DaemonHello`] (`client_version` +
///   `protocol_version`), so the two shapes are mutually exclusive.
/// - Shim-shaped frames flow to [`run_shim_connection`], which is
///   responsible for admission, ack, and dispatching to the byte-pump
///   host. Management-shaped frames flow to [`run_hello_connection`].
/// - A first frame that is neither parseable as JSON nor matches
///   either shape yields a `-32600 Invalid Request` response with
///   `id: null`, followed by an immediate close.
///
/// The `Send + 'static` bounds on `S` are required because the shim
/// byte-pump hosts spawn internal tasks that move the split halves
/// across `.await` points; the existing `IpcServer::run` already
/// spawns a per-connection task so both branches keep compiling.
pub async fn run_connection<S>(stream: S, ctx: HandlerContext) -> Result<(), ConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);

    // First-frame peek: read raw bytes, parse as `serde_json::Value`,
    // shape-discriminate. Transport-level frame-read failures (IO, size
    // cap) propagate as [`ConnectionError`]; JSON parse failures yield
    // a `-32600` response and a clean close so a dumb scanner cannot
    // wedge the daemon.
    //
    // `read_frame` returns `Result<Option<Vec<u8>>, io::Error>` (raw
    // bytes flavour); JSON parse errors surface from the
    // `serde_json::from_slice` step below, not here.
    let first_bytes = match read_frame(&mut reader).await {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let first_value: serde_json::Value = match serde_json::from_slice(&first_bytes) {
        Ok(v) => v,
        Err(e) => {
            let resp = ValidationError::InvalidRequest {
                reason: "first frame must be DaemonHello or ShimRegister",
                context: Some(e.to_string()),
            }
            .into_jsonrpc_response();
            let _ = write_frame_json(&mut writer, &resp).await;
            let _ = writer.shutdown().await;
            return Ok(());
        }
    };

    // Shape discrimination: [`ShimRegister`] requires BOTH `protocol`
    // and `pid` keys (with `deny_unknown_fields`). [`DaemonHello`]
    // requires `client_version` + `protocol_version`. Neither pair
    // overlaps, so matching on the presence of `protocol` + `pid` is
    // sufficient — and deliberately the MORE SPECIFIC check, so any
    // ambiguous future shape is rejected by the fall-through hello
    // deserialiser rather than silently routed into the shim path.
    if is_shim_register_shape(&first_value) {
        // Strict deserialisation catches any fields that passed the
        // quick shape check but violate `deny_unknown_fields` (e.g., a
        // malicious frame carrying `protocol`, `pid`, AND an extra
        // `backdoor` field). On failure the reject envelope is
        // formatted as a [`ShimRegisterAck`] — the shim client already
        // expects its own ack type as the FIRST frame after sending
        // `ShimRegister`, so the wire-form stays coherent.
        let shim_req: ShimRegister = match serde_json::from_value(first_value) {
            Ok(r) => r,
            Err(e) => {
                let ack = ShimRegisterAck {
                    accepted: false,
                    daemon_version: ctx.daemon_version.to_owned(),
                    reason: Some(format!("invalid ShimRegister: {e}")),
                    envelope_version: ENVELOPE_VERSION,
                };
                let _ = write_frame_json(&mut writer, &ack).await;
                let _ = writer.shutdown().await;
                return Ok(());
            }
        };
        return run_shim_connection(reader, writer, shim_req, ctx).await;
    }

    // Fall through: treat as [`DaemonHello`] — preserves Phase 8a's
    // JSON-RPC request loop exactly.
    run_hello_connection(reader, writer, first_value, ctx).await
}

/// Returns `true` when the peeked first frame is JSON-object-shaped
/// with both `protocol` and `pid` keys — the minimum conjunction
/// [`ShimRegister`] requires.
///
/// Factored out so the shape rule is explicit and testable; the full
/// `#[serde(deny_unknown_fields)]` validation happens in
/// [`serde_json::from_value::<ShimRegister>`] at the actual dispatch
/// site, not here.
#[must_use]
fn is_shim_register_shape(v: &serde_json::Value) -> bool {
    v.as_object()
        .is_some_and(|m| m.get("protocol").is_some() && m.get("pid").is_some())
}

/// Phase 8a `daemon/hello` handshake + JSON-RPC request loop.
///
/// Extracted verbatim from the pre-U10 `run_connection` body; the only
/// behavioural change is that the first frame was already consumed by
/// [`run_connection`] and is re-passed here as a `serde_json::Value`
/// for typed deserialisation.
async fn run_hello_connection<R, W>(
    mut reader: R,
    mut writer: W,
    first_value: serde_json::Value,
    ctx: HandlerContext,
) -> Result<(), ConnectionError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // The first frame was parsed to `Value` by `run_connection`.
    // Re-materialise as a strict [`DaemonHello`] here; any mismatch
    // yields a `-32600 Invalid Request` and a close, mirroring Phase
    // 8a's pre-U10 behaviour exactly.
    let hello: DaemonHello = match serde_json::from_value(first_value) {
        Ok(h) => h,
        Err(e) => {
            let resp = ValidationError::InvalidRequest {
                reason: "first frame must be a DaemonHello handshake",
                context: Some(e.to_string()),
            }
            .into_jsonrpc_response();
            let _ = write_frame_json(&mut writer, &resp).await;
            let _ = writer.shutdown().await;
            return Ok(());
        }
    };

    // Version negotiation.
    let compatible = hello.protocol_version == 1;
    let hello_resp = DaemonHelloResponse {
        compatible,
        daemon_version: ctx.daemon_version.to_owned(),
        envelope_version: ENVELOPE_VERSION,
    };
    write_frame_json(&mut writer, &hello_resp).await?;
    if !compatible {
        let _ = writer.shutdown().await;
        return Ok(());
    }

    // STEP_6 iter-2 BLOCK fix: capture the connection-level
    // `logical_workspace` binding from the parsed `DaemonHello`.
    // Every subsequent `daemon/load` on this connection that omits
    // its own `logical_workspace` param inherits this binding.
    let conn_state = ConnectionState::from_hello(hello.logical_workspace);

    // JSON-RPC request loop.
    loop {
        tokio::select! {
            biased;
            () = ctx.shutdown.cancelled() => break,
            frame = read_frame(&mut reader) => match frame {
                Ok(None) => break,
                Ok(Some(bytes)) => {
                    let outcome = handle_frame(&ctx, &conn_state, &bytes).await;
                    match outcome {
                        FrameResponse::None => {}
                        FrameResponse::Single(resp) => {
                            write_frame_json(&mut writer, &resp).await?;
                        }
                        FrameResponse::Batch(responses) => {
                            write_frame_json(&mut writer, &responses).await?;
                        }
                        FrameResponse::ParseError(resp) => {
                            let _ = write_frame_json(&mut writer, &resp).await;
                            break;
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let _ = writer.shutdown().await;
    Ok(())
}

/// Phase 8c U10 shim-register path.
///
/// 1. Atomic admission via
///    [`ShimRegistry::try_register_bounded`] under a single
///    `parking_lot::Mutex` guard (Codex iter-1 B2 fix — the racy
///    `len() >= cap` + `register()` two-step is gone).
/// 2. Write [`ShimRegisterAck`] — acceptance carries `reason: None`
///    (omitted from the wire form); rejection carries
///    [`RejectReason`]'s `Display` output and closes the connection.
/// 3. Dispatch the raw stream halves to the protocol-appropriate
///    byte-pump host.
/// 4. The [`ShimHandle`] returned by `try_register_bounded` is held in
///    the local `_handle` binding for the duration of the host call;
///    its `Drop` removes the registry entry on function return, whether
///    the host returned cleanly, via `shutdown.cancelled()`, or with an
///    error.
///
/// Shutdown cancellation is forwarded into each host through the
/// `ctx.shutdown` token clone, per Codex iter-2 §I: flipping the token
/// causes the LSP/MCP host to drain and return, the `_handle` drops,
/// and `run_shim_connection` exits.
///
/// [`RejectReason`]: super::shim_registry::RejectReason
async fn run_shim_connection<R, W>(
    reader: R,
    mut writer: W,
    req: ShimRegister,
    ctx: HandlerContext,
) -> Result<(), ConnectionError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Step 1: atomic bounded admission. Single-mutex-guard check →
    // insert, so concurrent `ShimRegister` frames racing into the same
    // registry cannot oversubscribe `max_shim_connections`.
    let handle_result = ctx.shim_registry.try_register_bounded(
        req.protocol,
        req.pid,
        ctx.config.max_shim_connections,
    );

    // Step 2: ack. On reject we format the rejection reason (currently
    // `"shim registry full (N / cap)"` — see
    // [`super::shim_registry::RejectReason`] Display impl) and
    // immediately close.
    let _handle: ShimHandle = match handle_result {
        Ok(h) => {
            let ack = ShimRegisterAck {
                accepted: true,
                daemon_version: ctx.daemon_version.to_owned(),
                reason: None,
                envelope_version: ENVELOPE_VERSION,
            };
            write_frame_json(&mut writer, &ack).await?;
            h
        }
        Err(reject) => {
            let ack = ShimRegisterAck {
                accepted: false,
                daemon_version: ctx.daemon_version.to_owned(),
                reason: Some(reject.to_string()),
                envelope_version: ENVELOPE_VERSION,
            };
            let _ = write_frame_json(&mut writer, &ack).await;
            let _ = writer.shutdown().await;
            return Ok(());
        }
    };

    // Step 3: hand the stream halves to the byte-pump host. From this
    // point on the wire carries the protocol's native framing (LSP
    // Content-Length headers, or rmcp's newline-delimited JSON) — the
    // sqryd 4-byte-LE length-prefix codec is no longer used on this
    // connection.
    match req.protocol {
        ShimProtocol::Lsp => {
            // Per Codex iter-1/iter-2 §E: one fresh `SessionManager`
            // per daemon-hosted LSP shim. Shared session state across
            // connections is a deferred performance optimisation, not
            // a correctness requirement for Phase 8c.
            let manager = Arc::clone(&ctx.manager);
            let session =
                sqry_lsp::session::SessionManager::new(sqry_lsp::LspOptions::default_daemon())
                    .with_revision_status_provider(move || {
                        manager
                            .resident_revision_statuses(None, false)
                            .into_iter()
                            .filter_map(|status| serde_json::to_value(status).ok())
                            .collect()
                    });
            if let Err(e) = sqry_lsp::daemon_host::host_on_streams(
                reader,
                writer,
                session,
                ctx.shutdown.clone(),
            )
            .await
            {
                tracing::warn!(error = %e, "lsp shim host errored");
            }
        }
        ShimProtocol::Mcp => {
            let tool_timeout = std::time::Duration::from_secs(ctx.config.tool_timeout_secs);
            if let Err(e) = crate::mcp_host::host_mcp_on_streams(
                reader,
                writer,
                Arc::clone(&ctx.manager),
                Arc::clone(&ctx.workspace_builder),
                Arc::clone(&ctx.tool_executor),
                tool_timeout,
                ctx.daemon_version,
                ctx.shutdown.clone(),
            )
            .await
            {
                tracing::warn!(error = %e, "mcp shim host errored");
            }
        }
    }

    // Step 4: `_handle` drops here, removing the entry from the
    // registry.
    Ok(())
}

/// Top-level per-frame dispatch: parse → array vs object → batch or
/// single dispatch.
///
/// `conn` carries per-connection state captured at the
/// `DaemonHello` handshake (today: the optional
/// `logical_workspace` binding inherited by `daemon/load`).
async fn handle_frame(ctx: &HandlerContext, conn: &ConnectionState, bytes: &[u8]) -> FrameResponse {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return FrameResponse::ParseError(parse_error_response(e)),
    };
    match value {
        serde_json::Value::Array(items) => dispatch_batch(ctx, conn, items).await,
        serde_json::Value::Object(_) => dispatch_single(ctx, conn, value).await,
        _ => FrameResponse::Single(
            ValidationError::InvalidRequest {
                reason: "request must be an object or array",
                context: None,
            }
            .into_jsonrpc_response(),
        ),
    }
}

/// Process a JSON-RPC 2.0 batch. Empty batches return a single
/// `-32600` response; notifications are filtered out of the response
/// array; nested-array elements become per-slot `-32600` errors.
async fn dispatch_batch(
    ctx: &HandlerContext,
    conn: &ConnectionState,
    items: Vec<serde_json::Value>,
) -> FrameResponse {
    if items.is_empty() {
        return FrameResponse::Single(
            ValidationError::InvalidRequest {
                reason: "batch must contain at least one request",
                context: None,
            }
            .into_jsonrpc_response(),
        );
    }
    let mut responses = Vec::with_capacity(items.len());
    for item in items {
        if item.is_array() {
            responses.push(
                ValidationError::InvalidRequest {
                    reason: "batch element must be a request object",
                    context: None,
                }
                .into_jsonrpc_response(),
            );
            continue;
        }
        match dispatch_single(ctx, conn, item).await {
            FrameResponse::Single(resp) => responses.push(resp),
            FrameResponse::None => {}
            other => responses.push(internal_error_response(
                None,
                &format!("batch dispatch returned unexpected shape: {other:?}"),
            )),
        }
    }
    if responses.is_empty() {
        FrameResponse::None
    } else {
        FrameResponse::Batch(responses)
    }
}

/// Validate + dispatch a single request value.
async fn dispatch_single(
    ctx: &HandlerContext,
    conn: &ConnectionState,
    value: serde_json::Value,
) -> FrameResponse {
    match validate_request_value(value) {
        Ok(req) => match dispatch_request(ctx, conn, req).await {
            Some(resp) => FrameResponse::Single(resp),
            None => FrameResponse::None, // notification
        },
        Err(e) => FrameResponse::Single(e.into_jsonrpc_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_shim_register_shape_accepts_full_object() {
        let v = serde_json::json!({"protocol": "lsp", "pid": 42});
        assert!(is_shim_register_shape(&v));
    }

    #[test]
    fn is_shim_register_shape_accepts_object_with_extra_keys() {
        // The quick shape check allows extras — strict
        // `deny_unknown_fields` deserialisation catches them at the
        // actual dispatch site.
        let v = serde_json::json!({"protocol": "lsp", "pid": 42, "other": 1});
        assert!(is_shim_register_shape(&v));
    }

    #[test]
    fn is_shim_register_shape_rejects_missing_protocol() {
        let v = serde_json::json!({"pid": 42});
        assert!(!is_shim_register_shape(&v));
    }

    #[test]
    fn is_shim_register_shape_rejects_missing_pid() {
        let v = serde_json::json!({"protocol": "lsp"});
        assert!(!is_shim_register_shape(&v));
    }

    #[test]
    fn is_shim_register_shape_rejects_daemon_hello_shape() {
        // `DaemonHello { client_version, protocol_version }` must NOT
        // collide with the shim-shape check — even though it carries a
        // `protocol_version` key, neither `protocol` (bare) nor `pid`
        // is present.
        let v = serde_json::json!({"client_version": "8.0.6", "protocol_version": 1});
        assert!(!is_shim_register_shape(&v));
    }

    #[test]
    fn is_shim_register_shape_rejects_non_object() {
        assert!(!is_shim_register_shape(&serde_json::Value::Null));
        assert!(!is_shim_register_shape(&serde_json::json!([1, 2, 3])));
        assert!(!is_shim_register_shape(&serde_json::json!("hello")));
        assert!(!is_shim_register_shape(&serde_json::json!(42)));
    }
}
