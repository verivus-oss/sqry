//! `sqry-daemon-client` — sqryd daemon client library.
//!
//! Exposes two independent connection surfaces:
//!
//! ## Shim path (Phase 8c)
//!
//! - [`connect_shim`] — open a UDS / named-pipe connection to a running
//!   sqryd daemon, send [`ShimRegister`] as the very first frame, and
//!   await [`ShimRegisterAck`].
//! - [`ShimConnection`] — owned, post-handshake full-duplex stream.
//! - [`pump_stdio`] — run the byte-pump between the current process's
//!   stdin/stdout and the shim connection until either half-stream hits
//!   EOF.
//!
//! ## Management path (Task 10)
//!
//! - [`DaemonClient`] — management client for daemon lifecycle
//!   operations. Uses the [`sqry_daemon_protocol::DaemonHello`] /
//!   [`sqry_daemon_protocol::DaemonHelloResponse`] handshake + JSON-RPC
//!   request/response loop. Constructed via [`DaemonClient::connect`]
//!   or [`DaemonClient::connect_with_timeouts`].
//!
//! # Connection discipline
//!
//! Per the Codex iter-1 B1 fix, clients either do
//! `DaemonHello → JSON-RPC` **or** `ShimRegister → byte-pump`, never
//! both on the same connection. The daemon router shape-discriminates
//! on the first frame, so crossing the paths would route the client to
//! the wrong handler. The two surfaces are intentionally separate at the
//! API level to make this constraint compile-time enforced.
//!
//! # pump_stdio factoring
//!
//! `pump_stdio` is split into a generic [`pump_stdio_impl`] that takes
//! the editor-side reader/writer + shim-side split halves, plus a thin
//! public wrapper that supplies [`tokio::io::stdin`] / [`tokio::io::stdout`].
//! Testing the process-global stdin/stdout is impossible in practice
//! (they are a per-process singleton owned by the runtime), so tests
//! drive `pump_stdio_impl` directly with a pair of
//! [`tokio::io::duplex`] streams.
//!
//! The public wrapper must not use `tokio::io::copy_bidirectional`
//! because `tokio::io::stdin()` + `tokio::io::stdout()` cannot be
//! combined into a single duplex stream — see Codex iter-1 B5 fix
//! discussion in the Phase 8c design.

pub mod management;

pub use management::{DEFAULT_HELLO_TIMEOUT, DaemonClient};
pub use sqry_daemon_protocol::{
    ENVELOPE_VERSION, ShimProtocol, ShimRegister, ShimRegisterAck, framing,
};

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

// ---------------------------------------------------------------------------
// Bounded-handshake defaults.
// ---------------------------------------------------------------------------

/// Default upper bound on the platform `connect` (UDS `connect` / named-pipe
/// `open`) step of [`connect_shim`]. A stuck daemon — socket file present,
/// accept-loop hung — would otherwise hang the editor's LSP / MCP launch
/// indefinitely; bounding it surfaces a fast, diagnostic error instead.
///
/// Callers that need explicit control (longer cold-start allowance for a
/// just-spawned daemon, CI determinism, etc.) should use
/// [`connect_shim_with_timeouts`] and pass a custom value.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default upper bound on the `ShimRegister → ShimRegisterAck` handshake round-trip
/// after a successful connect. Applies to the ack read only — the
/// `ShimRegister` write is guarded by the same underlying stream but never
/// blocks past the kernel buffer on a UDS / named-pipe.
///
/// Five seconds mirrors [`DEFAULT_CONNECT_TIMEOUT`]: the shim handshake is
/// latency-trivial when the daemon is healthy, so anything longer is a strong
/// signal of a stuck accept loop or a protocol regression rather than load.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Error surface.
// ---------------------------------------------------------------------------

/// Client-side error surface. `FrameError` subsumes both IO and serde_json
/// decode failures that originate inside [`sqry_daemon_protocol::framing`];
/// the top-level [`ClientError::Io`] variant is reserved for IO failures
/// outside the codec (e.g. split/copy plumbing in [`pump_stdio_impl`]).
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// UDS `connect` / named-pipe `open` failed. `path` is preserved for
    /// diagnostics; `source` is the underlying [`std::io::Error`].
    #[error("failed to connect to daemon socket at {path}: {source}", path = path.display())]
    Connect {
        /// Socket path supplied to [`connect_shim`].
        path: PathBuf,
        /// Underlying transport failure (`ConnectionRefused`,
        /// `NotFound`, permission denied, etc.).
        #[source]
        source: std::io::Error,
    },

    /// Platform `connect` did not complete within the caller's
    /// (or [`DEFAULT_CONNECT_TIMEOUT`]) budget. `after` is the elapsed
    /// bound, NOT the observed wall-clock, so callers can log the policy
    /// they tripped rather than a per-call measurement.
    #[error(
        "timed out connecting to daemon socket at {path} after {after:?}",
        path = path.display()
    )]
    ConnectTimeout {
        /// Socket path supplied to [`connect_shim`] / [`connect_shim_with_timeouts`].
        path: PathBuf,
        /// Timeout budget that was hit (defaults to
        /// [`DEFAULT_CONNECT_TIMEOUT`]).
        after: Duration,
    },

    /// Daemon accepted the TCP / UDS / named-pipe connection but did not
    /// send a [`ShimRegisterAck`] within the caller's
    /// (or [`DEFAULT_HANDSHAKE_TIMEOUT`]) budget. Distinct from
    /// [`ClientError::ShimAckEof`], which indicates a clean mid-handshake
    /// close — here the socket is still open but silent.
    #[error(
        "timed out waiting for ShimRegisterAck from daemon at {path} after {after:?}",
        path = path.display()
    )]
    HandshakeTimeout {
        /// Socket path the handshake was initiated against. Preserved so
        /// editor-side diagnostics can cite the exact daemon that hung.
        path: PathBuf,
        /// Handshake timeout budget that was hit (defaults to
        /// [`DEFAULT_HANDSHAKE_TIMEOUT`]).
        after: Duration,
    },

    /// Daemon's [`ShimRegisterAck`] carried an `envelope_version` that does
    /// not match this client's compiled-in [`ENVELOPE_VERSION`]. Because the
    /// shim path has **no** `DaemonHello` handshake (per the iter-1 B1 fix,
    /// the router shape-discriminates on the first frame), the envelope
    /// version on `ShimRegisterAck` is the ONLY wire-format version signal
    /// the shim client gets — drifting it silently would route byte-pump
    /// traffic against a mismatched schema. This variant is returned
    /// BEFORE the accepted/rejected branch so a mismatched version cannot
    /// be masked by a simultaneous application-level rejection.
    #[error("daemon envelope_version mismatch: got {got}, expected {expected}")]
    EnvelopeVersionMismatch {
        /// Version the daemon advertised on its [`ShimRegisterAck`].
        got: u32,
        /// Version this client was compiled against
        /// ([`ENVELOPE_VERSION`]).
        expected: u32,
    },

    /// Daemon responded with `ShimRegisterAck { accepted: false, reason, ... }`.
    /// The rejection reason is surfaced verbatim so shim clients can
    /// propagate it to their parent process's launch diagnostics.
    #[error("daemon rejected shim registration: {0}")]
    ShimRejected(String),

    /// Daemon closed the connection cleanly before sending
    /// [`ShimRegisterAck`]. Distinct from
    /// [`ClientError::Io`] because it indicates a mid-handshake
    /// policy rejection (socket-close, not crash).
    #[error("daemon closed connection during shim handshake")]
    ShimAckEof,

    // -----------------------------------------------------------------------
    // Management path (Task 10 / `DaemonClient`).
    // -----------------------------------------------------------------------
    /// Daemon rejected the [`sqry_daemon_protocol::DaemonHello`]
    /// handshake with `compatible: false`. This indicates a
    /// major-version incompatibility between the client and the running
    /// daemon — callers should surface a user-visible error and suggest
    /// upgrading the sqryd binary.
    #[error("daemon rejected hello handshake (version incompatible)")]
    HelloRejected,

    /// Daemon closed the connection cleanly before sending a
    /// [`sqry_daemon_protocol::DaemonHelloResponse`]. Distinct from
    /// [`ClientError::Io`] — indicates a mid-handshake close by the
    /// daemon (e.g. the daemon is in a shutdown state and refusing new
    /// management connections) rather than a transport error.
    #[error("daemon closed connection during hello handshake")]
    HelloEof,

    /// JSON-RPC error response from the daemon. Returned when the
    /// daemon sends an `"error"` payload (rather than a `"result"`)
    /// in response to a [`DaemonClient::send_request`] call.
    ///
    /// `code` is the JSON-RPC error code (e.g. -32603 for Internal),
    /// `message` is the human-readable description, and `data` is the
    /// optional structured error detail (format is method-specific).
    #[error("daemon returned error: {code} {message}")]
    RpcError {
        /// JSON-RPC error code.
        code: i32,
        /// Human-readable error message.
        message: String,
        /// Optional structured error data (method-specific).
        data: Option<serde_json::Value>,
    },

    // -----------------------------------------------------------------------
    // Shared transport errors.
    // -----------------------------------------------------------------------
    /// Transport-level IO failure outside the codec boundary (split,
    /// pump, flush, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Framing or JSON-decode failure. Returned by both the shim
    /// handshake path ([`connect_shim`]) and the management path
    /// ([`DaemonClient`]) when a frame body cannot be decoded as the
    /// expected type, or when a raw frame IO fails at the codec layer.
    #[error("frame decode: {0}")]
    Frame(#[from] sqry_daemon_protocol::framing::FrameError),

    /// A typed management-API call (e.g. [`crate::DaemonClient::load`])
    /// received a structurally valid JSON-RPC `result` payload that did
    /// not deserialise into the expected response type. Fails hard
    /// rather than silently returning partial/default data, so schema
    /// drift between the client and daemon surfaces as a user-visible
    /// error instead of a misleading success.
    ///
    /// `method` identifies which JSON-RPC method produced the payload;
    /// `source` carries the underlying `serde_json` decode failure.
    #[error("daemon response for {method} did not match expected schema: {source}")]
    SchemaMismatch {
        /// JSON-RPC method whose response failed to deserialise.
        method: &'static str,
        /// Underlying decode failure.
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------------
// AsyncReadWrite sealed-by-blanket-impl trait.
// ---------------------------------------------------------------------------

/// Full-duplex stream trait. Every `AsyncRead + AsyncWrite + Unpin` type
/// implements it automatically via the blanket impl below; no concrete
/// types outside this crate need to name it explicitly.
///
/// Having a single trait-object type (`dyn AsyncReadWrite + Send`) lets
/// [`ShimConnection`] store a platform-agnostic stream handle without
/// leaking `UnixStream` vs `NamedPipeClient` through the public API.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Unpin + ?Sized {}

// ---------------------------------------------------------------------------
// ShimConnection.
// ---------------------------------------------------------------------------

/// A connected shim session. Handshake is complete; bytes flow raw
/// between the caller and the daemon from this point on.
///
/// Construct via [`connect_shim`]. The [`ShimConnection::daemon_version`]
/// accessor returns the version string the daemon advertised in
/// [`ShimRegisterAck::daemon_version`].
///
/// [`ShimConnection::into_stream`] consumes `self` and yields the
/// owned, pinned full-duplex stream for callers that want to drive the
/// byte-pump themselves (e.g. with a custom split strategy). Most
/// callers should instead pass `self` to [`pump_stdio`], which wires
/// the stream to process stdin/stdout.
pub struct ShimConnection {
    stream: Pin<Box<dyn AsyncReadWrite + Send>>,
    daemon_version: String,
}

impl ShimConnection {
    /// Server-advertised daemon version from [`ShimRegisterAck`]. Useful
    /// for shim clients that want to surface a banner to their parent
    /// process or gate on a minimum daemon version.
    #[must_use]
    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    /// Consume the connection and return the owned full-duplex stream.
    /// After this call the handshake metadata is gone — callers that
    /// need the daemon version should copy it out via
    /// [`Self::daemon_version`] before calling `into_stream`.
    #[must_use]
    pub fn into_stream(self) -> Pin<Box<dyn AsyncReadWrite + Send>> {
        self.stream
    }
}

impl std::fmt::Debug for ShimConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShimConnection")
            .field("daemon_version", &self.daemon_version)
            .field("stream", &"<Pin<Box<dyn AsyncReadWrite + Send>>>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Platform-specific connect helpers.
// ---------------------------------------------------------------------------

/// Open a platform-appropriate full-duplex stream to the daemon socket.
///
/// On unix this is `tokio::net::UnixStream::connect(path)`. On windows
/// it is `tokio::net::windows::named_pipe::ClientOptions::new().open(path)`.
/// Any IO error is wrapped in [`ClientError::Connect`] with the original
/// path preserved for diagnostics.
#[cfg(unix)]
async fn platform_connect(
    socket_path: &Path,
) -> Result<Pin<Box<dyn AsyncReadWrite + Send>>, ClientError> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|source| ClientError::Connect {
            path: socket_path.to_path_buf(),
            source,
        })?;
    Ok(Box::pin(stream))
}

#[cfg(windows)]
async fn platform_connect(
    socket_path: &Path,
) -> Result<Pin<Box<dyn AsyncReadWrite + Send>>, ClientError> {
    // Windows named pipes open synchronously but the returned handle is
    // async-ready once bound to the tokio runtime.
    let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(socket_path.as_os_str())
        .map_err(|source| ClientError::Connect {
            path: socket_path.to_path_buf(),
            source,
        })?;
    Ok(Box::pin(pipe))
}

// ---------------------------------------------------------------------------
// Shim handshake.
// ---------------------------------------------------------------------------

/// Inner handshake driver, factored out of [`connect_shim`] for
/// testability.
///
/// Takes any [`AsyncReadWrite + Send + 'static`][`AsyncReadWrite`]
/// stream (production: the platform-specific stream from
/// [`platform_connect`]; tests: one end of a [`tokio::io::duplex`]).
/// Writes [`ShimRegister`] as the first frame, reads exactly one
/// [`ShimRegisterAck`], validates its `envelope_version`, and yields a
/// [`ShimConnection`] on success.
///
/// The `socket_desc` argument is threaded purely for diagnostic
/// error-message context (so [`ClientError::HandshakeTimeout`] can cite
/// the exact path the hang is associated with). Tests that exercise
/// this via [`tokio::io::duplex`] may pass a sentinel path like
/// `Path::new("<in-memory-duplex>")`.
async fn do_shim_handshake<S>(
    mut stream: S,
    protocol: ShimProtocol,
    client_pid: u32,
    socket_desc: &Path,
    handshake_timeout: Duration,
) -> Result<ShimConnection, ClientError>
where
    S: AsyncReadWrite + Send + 'static,
{
    // Step 1: send ShimRegister as the LITERAL first frame on the wire.
    // No DaemonHello is ever emitted on this path — the router uses the
    // shape of the first frame to discriminate between CLI / JSON-RPC
    // clients and shim clients, so crossing the streams here would
    // route us to the wrong path (per Codex iter-1 B1 fix).
    let register = ShimRegister {
        protocol,
        pid: client_pid,
    };
    framing::write_frame_json(&mut stream, &register).await?;

    // Step 2: read exactly one response frame, bounded by
    // `handshake_timeout`. A stuck accept-loop that never writes the
    // ack frame would otherwise block the editor's LSP / MCP launch
    // indefinitely. The bound applies to the full frame read (length
    // prefix + body), which is the only step on the ack path that can
    // stall on daemon-side silence.
    //
    // `read_frame_json` returns:
    //   - Ok(Some(ack))  — typed ack decoded
    //   - Ok(None)       — clean EOF at frame boundary (mid-handshake
    //                      close by the daemon — treat as refusal)
    //   - Err(FrameError::Io | FrameError::Json) — pass through
    let read_fut = framing::read_frame_json::<_, ShimRegisterAck>(&mut stream);
    let ack_outcome = match tokio::time::timeout(handshake_timeout, read_fut).await {
        Ok(inner) => inner,
        Err(_elapsed) => {
            return Err(ClientError::HandshakeTimeout {
                path: socket_desc.to_path_buf(),
                after: handshake_timeout,
            });
        }
    };
    let ack: ShimRegisterAck = match ack_outcome? {
        Some(ack) => ack,
        None => return Err(ClientError::ShimAckEof),
    };

    // Step 3: envelope_version check runs BEFORE accepted/rejected so a
    // wire-format drift is never masked by a simultaneous application
    // rejection. The shim path has no DaemonHello, so this is the only
    // version-negotiation signal the client gets — silently accepting a
    // mismatched version would route byte-pump traffic against a schema
    // the client cannot reason about.
    if ack.envelope_version != ENVELOPE_VERSION {
        return Err(ClientError::EnvelopeVersionMismatch {
            got: ack.envelope_version,
            expected: ENVELOPE_VERSION,
        });
    }

    // Step 4: unwrap accepted vs rejected.
    if ack.accepted {
        Ok(ShimConnection {
            stream: Box::pin(stream),
            daemon_version: ack.daemon_version,
        })
    } else {
        let reason = ack.reason.unwrap_or_else(|| "no reason given".to_owned());
        Err(ClientError::ShimRejected(reason))
    }
}

/// Open a shim connection to the daemon with explicit timeout overrides.
///
/// Semantically identical to [`connect_shim`] but lets callers tune the
/// `connect` and handshake budgets. Editors in cold-start scenarios may
/// want a longer connect budget (e.g. a just-spawned daemon still
/// binding its socket); CI harnesses may want tighter budgets for fast
/// failure. Either way, both timeouts MUST be > 0 — zero-valued
/// durations fire immediately inside [`tokio::time::timeout`] and
/// surface as the corresponding timeout variant on the very first poll.
///
/// # Errors
///
/// All of [`connect_shim`]'s errors, plus
/// [`ClientError::ConnectTimeout`] and [`ClientError::HandshakeTimeout`]
/// for the two bounded steps.
pub async fn connect_shim_with_timeouts(
    socket_path: &Path,
    protocol: ShimProtocol,
    client_pid: u32,
    connect_timeout: Duration,
    handshake_timeout: Duration,
) -> Result<ShimConnection, ClientError> {
    // Step 1 — bounded connect. The platform helper itself wraps the
    // connect call in `ClientError::Connect { path, source }` on IO
    // failure; the outer timeout is only responsible for distinguishing
    // "hung accept loop" (ConnectTimeout) from "refused / not found"
    // (Connect { ... }).
    let stream =
        apply_connect_timeout(platform_connect(socket_path), socket_path, connect_timeout).await?;

    // Step 2 — bounded handshake.
    do_shim_handshake(stream, protocol, client_pid, socket_path, handshake_timeout).await
}

/// Apply [`ClientError::ConnectTimeout`] semantics to any
/// `Future<Output = Result<T, ClientError>>`. Factored out so the test
/// suite can drive a deterministic slow future (e.g. `tokio::time::sleep`
/// followed by a unit-value Ok) without having to reproduce a
/// platform-level hung connect. Production uses it exactly once, at
/// `connect_shim_with_timeouts`'s step 1.
async fn apply_connect_timeout<Fut, T>(
    fut: Fut,
    socket_path: &Path,
    connect_timeout: Duration,
) -> Result<T, ClientError>
where
    Fut: std::future::Future<Output = Result<T, ClientError>>,
{
    match tokio::time::timeout(connect_timeout, fut).await {
        Ok(inner) => inner,
        Err(_elapsed) => Err(ClientError::ConnectTimeout {
            path: socket_path.to_path_buf(),
            after: connect_timeout,
        }),
    }
}

/// Open a shim connection to the daemon.
///
/// Sends [`ShimRegister`] as the **very first frame** on the wire and
/// awaits [`ShimRegisterAck`]. On a successful ack, the returned
/// [`ShimConnection`] wraps the same underlying stream with the
/// handshake frames already consumed — bytes read/written through the
/// stream after this point are raw LSP / MCP traffic.
///
/// Uses [`DEFAULT_CONNECT_TIMEOUT`] and [`DEFAULT_HANDSHAKE_TIMEOUT`];
/// callers that need explicit control should use
/// [`connect_shim_with_timeouts`].
///
/// # Errors
///
/// - [`ClientError::Connect`] if the UDS / named-pipe `connect` fails.
/// - [`ClientError::ConnectTimeout`] if the connect step does not
///   complete within [`DEFAULT_CONNECT_TIMEOUT`].
/// - [`ClientError::HandshakeTimeout`] if the daemon does not send a
///   [`ShimRegisterAck`] frame within [`DEFAULT_HANDSHAKE_TIMEOUT`]
///   after a successful connect.
/// - [`ClientError::EnvelopeVersionMismatch`] if the ack's
///   `envelope_version` does not match this client's
///   compiled-in [`ENVELOPE_VERSION`].
/// - [`ClientError::ShimRejected`] if the daemon's
///   [`ShimRegisterAck`] carries `accepted = false`.
/// - [`ClientError::ShimAckEof`] if the daemon closes cleanly before
///   sending an ack (i.e. read_frame returned `Ok(None)` at a frame
///   boundary).
/// - [`ClientError::Io`] / [`ClientError::Frame`] for IO / framing
///   failures at any point during the handshake.
pub async fn connect_shim(
    socket_path: &Path,
    protocol: ShimProtocol,
    client_pid: u32,
) -> Result<ShimConnection, ClientError> {
    connect_shim_with_timeouts(
        socket_path,
        protocol,
        client_pid,
        DEFAULT_CONNECT_TIMEOUT,
        DEFAULT_HANDSHAKE_TIMEOUT,
    )
    .await
}

// ---------------------------------------------------------------------------
// pump_stdio + pump_stdio_impl.
// ---------------------------------------------------------------------------

/// Generic byte-pump. Copies bytes from `editor_in → shim_w` and
/// `shim_r → editor_out` concurrently via `tokio::select!`. Returns as
/// soon as **either** direction hits EOF (or an IO error) and cancels
/// the other via drop.
///
/// The `tokio::select!` form is load-bearing: `tokio::io::stdin()` +
/// `tokio::io::stdout()` cannot be combined into a single duplex
/// stream, which rules out `tokio::io::copy_bidirectional`. See the
/// Phase 8c design §A.2 Codex iter-1 B5 fix.
///
/// Returned tuple is `(bytes_up, bytes_down)`. The direction that hit
/// EOF first reports its final byte count; the cancelled direction
/// reports `0`.
///
/// Factored out of [`pump_stdio`] for testability — production code
/// supplies the process-global stdin/stdout, tests supply a pair of
/// [`tokio::io::duplex`] halves.
pub(crate) async fn pump_stdio_impl<EI, EO, SR, SW>(
    mut editor_in: EI,
    mut editor_out: EO,
    mut shim_r: SR,
    mut shim_w: SW,
) -> Result<(u64, u64), ClientError>
where
    EI: AsyncRead + Unpin,
    EO: AsyncWrite + Unpin,
    SR: AsyncRead + Unpin,
    SW: AsyncWrite + Unpin,
{
    tokio::select! {
        up = tokio::io::copy(&mut editor_in, &mut shim_w) => {
            let bytes_up = up?;
            Ok((bytes_up, 0))
        }
        down = tokio::io::copy(&mut shim_r, &mut editor_out) => {
            let bytes_down = down?;
            Ok((0, bytes_down))
        }
    }
}

/// Run the byte-pump between process stdin/stdout and a shim
/// connection. Returns the total bytes copied in each direction once
/// either half hits EOF.
///
/// # Errors
///
/// Propagates [`ClientError::Io`] from the underlying `tokio::io::copy`
/// calls; [`ClientError::Frame`] cannot occur here because the shim
/// byte-pump is a raw pass-through (framing metadata lives on the
/// wrapped LSP / MCP message stream above, not on the transport).
pub async fn pump_stdio(conn: ShimConnection) -> Result<(u64, u64), ClientError> {
    let stream = conn.into_stream();
    let (shim_r, shim_w) = tokio::io::split(stream);
    let editor_in = tokio::io::stdin();
    let editor_out = tokio::io::stdout();
    pump_stdio_impl(editor_in, editor_out, shim_r, shim_w).await
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    /// Helper: spawn a fake daemon on the server side of a duplex pair
    /// and return the client side + the handler join handle.
    ///
    /// The handler receives the server end and drives the handshake by
    /// reading the first frame (ShimRegister), asserting it, and
    /// writing whatever ack it wants the test to see. Keeping the
    /// handler as a plain async closure (rather than a BoxFuture)
    /// avoids pulling `futures` into this crate's dep surface for the
    /// sake of tests alone.
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
        let (client_side, server_side) = duplex(8192);
        let handle = tokio::spawn(async move { handler(server_side).await });
        (client_side, handle)
    }

    // -----------------------------------------------------------------
    // Test 1 — ShimRegister is the literal first frame; happy-path ack.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn connect_shim_sends_shim_register_as_first_frame() {
        let (client, handle) = spawn_fake_daemon(|mut server| async move {
            // Decode the first frame and assert it is a ShimRegister
            // with the exact fields we sent. Using read_frame_json with
            // ShimRegister as the expected shape implicitly validates
            // that a DaemonHello would NOT decode here (it has different
            // fields + deny_unknown_fields).
            let got: ShimRegister = framing::read_frame_json(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected ShimRegister, got EOF"))?;
            assert_eq!(got.protocol, ShimProtocol::Lsp);
            assert_eq!(got.pid, 42);

            // Respond with an accepted ack.
            let ack = ShimRegisterAck {
                accepted: true,
                daemon_version: "test-1.0".to_owned(),
                reason: None,
                envelope_version: 1,
            };
            framing::write_frame_json(&mut server, &ack).await?;
            Ok(())
        })
        .await;

        let conn = do_shim_handshake(
            client,
            ShimProtocol::Lsp,
            42,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HANDSHAKE_TIMEOUT,
        )
        .await
        .expect("handshake ok");
        assert_eq!(conn.daemon_version(), "test-1.0");

        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------
    // Test 2 — Rejected ack surfaces ClientError::ShimRejected(reason).
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn connect_shim_rejected_returns_shim_rejected_error() {
        let (client, handle) = spawn_fake_daemon(|mut server| async move {
            // Consume the register frame so the server-side duplex
            // doesn't block.
            let _: ShimRegister = framing::read_frame_json(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected ShimRegister, got EOF"))?;
            let ack = ShimRegisterAck {
                accepted: false,
                daemon_version: "test-1.0".to_owned(),
                reason: Some("cap exceeded".to_owned()),
                envelope_version: 1,
            };
            framing::write_frame_json(&mut server, &ack).await?;
            Ok(())
        })
        .await;

        let err = do_shim_handshake(
            client,
            ShimProtocol::Mcp,
            7,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HANDSHAKE_TIMEOUT,
        )
        .await
        .expect_err("must reject");
        match err {
            ClientError::ShimRejected(msg) => assert_eq!(msg, "cap exceeded"),
            other => panic!("expected ShimRejected, got {other:?}"),
        }
        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------
    // Test 3 — Owned stream round-trip after a successful handshake,
    //          with daemon_version carried through from the ack.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn connect_shim_accepted_returns_owned_stream_with_daemon_version() {
        let (client, handle) = spawn_fake_daemon(|mut server| async move {
            let _: ShimRegister = framing::read_frame_json(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected ShimRegister"))?;
            let ack = ShimRegisterAck {
                accepted: true,
                daemon_version: "daemon-9.9.9".to_owned(),
                reason: None,
                envelope_version: 1,
            };
            framing::write_frame_json(&mut server, &ack).await?;

            // Post-handshake raw byte transfer: daemon side writes 13
            // bytes of greeting that the client side must read back
            // unmodified through the pinned owned stream.
            server.write_all(b"hello, world!").await?;
            server.flush().await?;
            Ok(())
        })
        .await;

        let conn = do_shim_handshake(
            client,
            ShimProtocol::Lsp,
            1,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HANDSHAKE_TIMEOUT,
        )
        .await
        .expect("handshake ok");
        assert_eq!(conn.daemon_version(), "daemon-9.9.9");

        let mut stream = conn.into_stream();
        let mut buf = [0u8; 13];
        stream.read_exact(&mut buf).await.expect("read raw bytes");
        assert_eq!(&buf, b"hello, world!");

        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------
    // Test 4 — pump_stdio_impl copies bytes in BOTH directions.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn pump_stdio_bidirectional_copy() {
        // Simulate a full shim byte-pump: editor-side reader/writer
        // connected to a buffer we can inspect post-hoc, shim-side
        // reader/writer connected to a server mock that echoes.
        //
        // Editor in: supplies 5 bytes "ping\n" then EOFs (close), which
        // should cause pump_stdio_impl's up-direction copy to finish
        // with bytes_up = 5 and cancel the down-direction via drop.
        let (mut editor_in_writer, editor_in) = duplex(64);
        let (editor_out, mut editor_out_reader) = duplex(64);
        // KEEP shim_r_producer ALIVE for the duration of the pump so the
        // down-direction blocks on read rather than seeing immediate EOF.
        // If we dropped it here, `tokio::select!` would race — the
        // down-direction copy would win with Ok(0) before the up-direction
        // had a chance to propagate any bytes.
        let (_shim_r_producer, shim_r) = duplex(64);
        let (shim_w, mut shim_w_consumer) = duplex(64);

        // Fire the pump in a task so we can feed + close stdin-side
        // deterministically.
        let pump =
            tokio::spawn(
                async move { pump_stdio_impl(editor_in, editor_out, shim_r, shim_w).await },
            );

        // Feed 5 bytes editor-side and close — this drives the
        // up-direction copy to EOF (bytes_up = 5). The down-direction
        // stays blocked on read (shim_r_producer is still alive above).
        editor_in_writer.write_all(b"ping\n").await.unwrap();
        editor_in_writer.flush().await.unwrap();
        drop(editor_in_writer);

        // The up-direction wins select! when editor_in hits EOF after
        // the 5 bytes drain. pump_stdio_impl returns (5, 0).
        let (bytes_up, bytes_down) = pump.await.expect("join").expect("pump ok");
        assert_eq!(bytes_up, 5, "up direction should report 5 bytes");
        assert_eq!(bytes_down, 0, "down direction was cancelled");

        // Drain what the up-direction wrote to shim_w_consumer. After
        // pump_stdio_impl returned, shim_w was dropped — but the 5 bytes
        // were already buffered into the duplex pair before EOF. We must
        // be able to read_exact them now.
        let mut got_up = [0u8; 5];
        shim_w_consumer.read_exact(&mut got_up).await.unwrap();
        assert_eq!(&got_up, b"ping\n");

        // editor_out_reader should not have anything queued on it since
        // the down-direction was cancelled before any bytes flowed.
        let mut scratch = [0u8; 8];
        // Try a non-blocking-ish read: with the writer dropped by the
        // cancelled pump task, we expect 0 (EOF).
        let n = editor_out_reader.read(&mut scratch).await.unwrap();
        assert_eq!(n, 0, "editor_out should see EOF with 0 bytes copied");

        // _shim_r_producer drops here at end of scope — fine, pump has
        // already returned.
    }

    // -----------------------------------------------------------------
    // Test 5 — EOF on shim side yields Ok(_) with down-direction count.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn pump_stdio_eof_on_shim_side_returns_ok() {
        // The shim-read side delivers 7 bytes then closes; the pump
        // should observe clean EOF on the down-direction, return
        // Ok((0, 7)), and cancel the up-direction.
        let (_editor_in_writer, editor_in) = duplex(64);
        let (editor_out, mut editor_out_reader) = duplex(64);
        let (mut shim_r_producer, shim_r) = duplex(64);
        let (shim_w, _shim_w_consumer) = duplex(64);

        // Feed 7 bytes from the shim side and close — this triggers
        // `tokio::io::copy(&mut shim_r, &mut editor_out)` to finish
        // cleanly with 7 bytes.
        shim_r_producer.write_all(b"payload").await.unwrap();
        shim_r_producer.flush().await.unwrap();
        drop(shim_r_producer);

        let (bytes_up, bytes_down) = pump_stdio_impl(editor_in, editor_out, shim_r, shim_w)
            .await
            .expect("pump Ok on EOF");
        assert_eq!(bytes_up, 0, "up direction was cancelled");
        assert_eq!(bytes_down, 7, "down direction should report 7 bytes");

        // The editor_out side should have received exactly those 7
        // bytes before the task completed.
        let mut buf = [0u8; 7];
        editor_out_reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"payload");
    }

    // -----------------------------------------------------------------
    // Test 6 — Daemon advertises a mismatched envelope_version.
    //
    // The shim path has no DaemonHello, so the envelope_version on the
    // ack is the ONLY wire-version negotiation signal. A bogus value
    // (99, simulating a future incompatible schema) MUST surface as
    // ClientError::EnvelopeVersionMismatch and MUST be raised BEFORE
    // the accepted/rejected branch — i.e. even a successful-looking
    // ack (`accepted: true`) with the wrong version is an error.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn connect_shim_envelope_mismatch_returns_error() {
        let (client, handle) = spawn_fake_daemon(|mut server| async move {
            let _: ShimRegister = framing::read_frame_json(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected ShimRegister"))?;
            // Deliberately claim a future envelope version. `accepted:
            // true` here is load-bearing: the test proves the version
            // check short-circuits *before* the application-level
            // acceptance branch.
            let ack = ShimRegisterAck {
                accepted: true,
                daemon_version: "future-daemon".to_owned(),
                reason: None,
                envelope_version: 99,
            };
            framing::write_frame_json(&mut server, &ack).await?;
            Ok(())
        })
        .await;

        let err = do_shim_handshake(
            client,
            ShimProtocol::Lsp,
            123,
            Path::new("<in-memory-duplex>"),
            DEFAULT_HANDSHAKE_TIMEOUT,
        )
        .await
        .expect_err("envelope mismatch must error");
        match err {
            ClientError::EnvelopeVersionMismatch { got, expected } => {
                assert_eq!(got, 99, "daemon advertised 99");
                assert_eq!(expected, ENVELOPE_VERSION, "client expects current");
            }
            other => panic!("expected EnvelopeVersionMismatch, got {other:?}"),
        }

        handle.await.expect("join").expect("server ok");
    }

    // -----------------------------------------------------------------
    // Test 7 — apply_connect_timeout surfaces ConnectTimeout when the
    //          wrapped connect future exceeds its budget.
    //
    // Forcing a real platform-level hung connect is impractical in a
    // portable test: on linux AF_UNIX connects complete synchronously
    // into the listener's SOMAXCONN-sized backlog (4096 on modern
    // kernels), and on windows `ClientOptions::open` is synchronous
    // and fails fast with `FILE_NOT_FOUND` when the named pipe is
    // missing. To exercise the timeout wiring deterministically we
    // factor the wrap into [`apply_connect_timeout`] and drive it
    // with a `tokio::time::sleep` + Ok(()) future that deliberately
    // blows past the short budget. This verifies:
    //
    //   - the wrap emits `ClientError::ConnectTimeout` (not
    //     `HandshakeTimeout`, not `Io`) on elapse,
    //   - the `path` field round-trips the caller-supplied socket
    //     path for diagnostic parity, and
    //   - the `after` field echoes the policy budget (not a measured
    //     wall-clock), so callers can log what policy they tripped.
    //
    // `connect_shim_with_timeouts` itself is a ~5-line composition
    // over `apply_connect_timeout` + `do_shim_handshake`; both
    // timeout wraps are covered by tests 7 and 8 individually.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn connect_shim_connect_timeout_returns_error() {
        let socket_path = PathBuf::from("/tmp/sqry-daemon-client-test-fake.sock");
        let short_timeout = Duration::from_millis(50);

        // Deliberately slow "connect" future: sleep well past the
        // budget and then claim success. apply_connect_timeout MUST
        // trip first and surface ConnectTimeout.
        let slow_fut = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok::<(), ClientError>(())
        };

        let start = std::time::Instant::now();
        let err = apply_connect_timeout(slow_fut, &socket_path, short_timeout)
            .await
            .expect_err("must surface ConnectTimeout");
        let elapsed = start.elapsed();

        match err {
            ClientError::ConnectTimeout { path, after } => {
                assert_eq!(path, socket_path, "error path matches input");
                assert_eq!(after, short_timeout, "error cites policy budget");
            }
            other => panic!("expected ConnectTimeout, got {other:?}"),
        }
        // Sanity-check: we actually waited roughly the budget rather
        // than short-circuiting on the first poll.
        assert!(
            elapsed >= short_timeout,
            "expected to wait >= {short_timeout:?}, waited {elapsed:?}"
        );
        // And we did NOT wait anywhere near the 30s the future would
        // have blocked for — proving the timeout actually fired.
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout should fire well before the inner future's 30s sleep (waited {elapsed:?})"
        );
    }

    // -----------------------------------------------------------------
    // Test 8 — do_shim_handshake surfaces HandshakeTimeout when the
    //          daemon accepts but never writes the ack frame.
    //
    // The fake-daemon closure holds the server side open but simply
    // sleeps past our budget, proving the bound applies to the ack
    // read step only and surfaces the correct path + budget.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn do_shim_handshake_handshake_timeout_returns_error() {
        let (client, handle) = spawn_fake_daemon(|mut server| async move {
            // Consume the register frame so the server-side duplex
            // isn't blocked on write-back-pressure; then sleep past
            // the client's budget without writing an ack.
            let _: ShimRegister = framing::read_frame_json(&mut server)
                .await?
                .ok_or_else(|| anyhow::anyhow!("expected ShimRegister"))?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await;

        let short_timeout = Duration::from_millis(100);
        let sentinel = Path::new("<in-memory-duplex>");
        let err = do_shim_handshake(client, ShimProtocol::Mcp, 9, sentinel, short_timeout)
            .await
            .expect_err("handshake must time out");
        match err {
            ClientError::HandshakeTimeout { path, after } => {
                assert_eq!(path, sentinel, "error cites caller-supplied path");
                assert_eq!(after, short_timeout, "error cites policy budget");
            }
            other => panic!("expected HandshakeTimeout, got {other:?}"),
        }

        // The spawned handler is still sleeping; let it finish so the
        // test leaves no orphan tasks.
        handle.await.expect("join").expect("server ok");
    }
}
