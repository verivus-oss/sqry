//! IPC accept loop.
//!
//! Binds a UDS (Unix) or named pipe (Windows), accepts incoming
//! connections, and spawns a per-connection handler task. Graceful
//! shutdown is driven by a [`tokio_util::sync::CancellationToken`];
//! after cancellation, the loop drains active connections bounded by
//! [`crate::config::DaemonConfig::ipc_shutdown_drain_secs`].
//!
//! The two Unix bind branches (`RuntimeDir` vs `Configured`) implement
//! the Phase 8a iter-1 B2 fix: runtime-dir paths are auto-managed
//! (parent created 0700, stale socket removed after a liveness probe).
//! Configured paths also auto-unlink stale sockets after a liveness
//! probe confirms no process is listening — this is required for
//! auto-start to work after a daemon stop. Live sockets are never
//! touched: the daemon refuses to bind if a live daemon is already
//! listening. Non-socket files at the configured path are always
//! rejected.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use anyhow::anyhow;
use sqry_core::query::executor::QueryExecutor;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use crate::config::DaemonConfig;
#[cfg(unix)]
use crate::config::ENV_SOCKET_PATH;
#[cfg(unix)]
use crate::error::DaemonError;
use crate::error::DaemonResult;
use crate::rebuild::RebuildDispatcher;
use crate::workspace::{WorkspaceBuilder, WorkspaceManager};

use super::methods::HandlerContext;
use super::router::run_connection;
use super::shim_registry::ShimRegistry;

/// Top-level IPC server handle. Construct with [`Self::bind`] then
/// drive with [`Self::run`].
pub struct IpcServer {
    listener: Listener,
    socket_path: PathBuf,
    manager: Arc<WorkspaceManager>,
    dispatcher: Arc<RebuildDispatcher>,
    workspace_builder: Arc<dyn WorkspaceBuilder>,
    tool_executor: Arc<QueryExecutor>,
    shim_registry: Arc<ShimRegistry>,
    shutdown: CancellationToken,
    active_connections: Arc<AtomicU64>,
    config: Arc<DaemonConfig>,
    daemon_version: &'static str,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("socket_path", &self.socket_path)
            .field("daemon_version", &self.daemon_version)
            .finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Bind the server. Unix: `UnixListener` with the two-branch policy;
    /// Windows: `NamedPipeServer` with explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured socket or pipe cannot be bound, or
    /// when Unix socket-parent validation fails.
    pub async fn bind(
        config: Arc<DaemonConfig>,
        manager: Arc<WorkspaceManager>,
        dispatcher: Arc<RebuildDispatcher>,
        workspace_builder: Arc<dyn WorkspaceBuilder>,
        tool_executor: Arc<QueryExecutor>,
        shutdown: CancellationToken,
    ) -> DaemonResult<Self> {
        let socket_path = config.socket_path();
        // Cluster-G §5.2 — pre-flight the socket parent directory so a
        // missing or unwritable parent surfaces as a typed
        // `DaemonError::SocketSetup` (-32007) with copy-paste recovery
        // text rather than a generic `EACCES` from the bind syscall.
        #[cfg(unix)]
        ensure_socket_parent_writable(&socket_path)?;
        let listener = Listener::bind(&config, &socket_path).await?;
        Ok(Self {
            listener,
            socket_path,
            manager,
            dispatcher,
            workspace_builder,
            tool_executor,
            shim_registry: ShimRegistry::new(),
            shutdown,
            active_connections: Arc::new(AtomicU64::new(0)),
            config,
            daemon_version: env!("CARGO_PKG_VERSION"),
        })
    }

    /// Returns the bound socket path (Unix) or named-pipe name
    /// (Windows).
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Return a shared handle to the shim-connection registry.
    ///
    /// Task 9's bootstrap path surfaces the count via `daemon/status`,
    /// and the Phase 8c router / MCP host register shim connections
    /// through this `Arc`. The registry's internal state is guarded by
    /// a `parking_lot::Mutex`, so callers must not hold the returned
    /// `Arc` "actively" (i.e., inside a `.lock()` scope) across
    /// long-running awaits — see [`ShimRegistry::len`] and
    /// [`ShimRegistry::is_empty`] for the snapshot-under-lock
    /// accessors.
    #[must_use]
    pub fn shim_registry(&self) -> Arc<ShimRegistry> {
        Arc::clone(&self.shim_registry)
    }

    /// Accept loop. Returns when the shutdown token fires.
    ///
    /// # Errors
    ///
    /// Returns an error if accepting or serving an IPC connection fails outside
    /// the normal shutdown path.
    pub async fn run(self) -> DaemonResult<()> {
        let Self {
            mut listener,
            manager,
            dispatcher,
            workspace_builder,
            tool_executor,
            shim_registry,
            shutdown,
            active_connections,
            config,
            daemon_version,
            ..
        } = self;

        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    tracing::info!(
                        "ipc_server: shutdown requested; draining active connections"
                    );
                    break;
                }
                res = listener.accept() => match res {
                    Ok(stream) => {
                        let ctx = HandlerContext {
                            manager: Arc::clone(&manager),
                            dispatcher: Arc::clone(&dispatcher),
                            workspace_builder: Arc::clone(&workspace_builder),
                            tool_executor: Arc::clone(&tool_executor),
                            shim_registry: Arc::clone(&shim_registry),
                            shutdown: shutdown.clone(),
                            config: Arc::clone(&config),
                            daemon_version,
                        };
                        active_connections.fetch_add(1, Ordering::AcqRel);
                        let tracker = Arc::clone(&active_connections);
                        tokio::spawn(async move {
                            let conn_result = match stream {
                                #[cfg(unix)]
                                AcceptedStream::Unix(s) => run_connection(s, ctx).await,
                                #[cfg(windows)]
                                AcceptedStream::Pipe(s) => run_connection(s, ctx).await,
                            };
                            if let Err(e) = conn_result {
                                tracing::debug!(error = %e,
                                    "ipc_server: connection terminated with error");
                            }
                            tracker.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e,
                            "ipc_server: accept failed; continuing");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }

        // Drain phase.
        let deadline = Instant::now() + Duration::from_secs(config.ipc_shutdown_drain_secs);
        while Instant::now() < deadline && active_connections.load(Ordering::Acquire) > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let lingering = active_connections.load(Ordering::Acquire);
        if lingering > 0 {
            tracing::warn!(
                lingering,
                "ipc_server: {} connections still active at drain deadline",
                lingering
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Socket parent directory pre-flight (cluster-G §5.2).
// ---------------------------------------------------------------------------

/// Ensure the socket path's parent directory exists and is writable
/// before the daemon attempts to bind. Surfaces a typed
/// [`DaemonError::SocketSetup`] (`-32007`) with copy-paste recovery
/// text instead of letting the bind syscall return a generic `EACCES`.
///
/// Called from [`IpcServer::bind`] on Unix only. Windows named-pipe
/// paths (`\\.\pipe\<name>`) have no filesystem parent to validate;
/// they go through the existing pipe-creation error path.
#[cfg(unix)]
fn ensure_socket_parent_writable(socket_path: &Path) -> DaemonResult<()> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| DaemonError::SocketSetup {
            path: socket_path.to_path_buf(),
            reason: "socket path has no parent directory".to_string(),
        })?;
    if let Err(e) = std::fs::create_dir_all(parent) {
        return Err(DaemonError::SocketSetup {
            path: socket_path.to_path_buf(),
            reason: format!(
                "cannot create socket parent {}: {e}. \
                 Hint: set SQRY_DAEMON_SOCKET to a user-writable path \
                 (e.g. $XDG_RUNTIME_DIR/sqry/sqryd.sock or $TMPDIR/sqryd.sock).",
                parent.display(),
            ),
        });
    }
    // Probe writability with a `create_new(true)` open so the call
    // refuses to follow a pre-existing symlink under the probe path
    // (cluster-G iter-2 — codex iter-1 review flagged that
    // `fs::write` follows symlinks and truncates an existing file,
    // which lets a writable socket parent leak the probe write to an
    // unrelated location). The probe filename includes pid + nanos
    // so two daemon-start attempts can't race on the same name.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let probe = parent.join(format!(".sqryd-probe-{}-{nanos:09}", std::process::id()));
    let probe_outcome = OpenOptions::new().write(true).create_new(true).open(&probe);
    match probe_outcome {
        Ok(_file) => {
            // RAII: drop closes; we then delete. Best-effort delete —
            // an EACCES here would be unusual after a successful
            // create, but if it happens, we leave the empty probe
            // file rather than abort startup.
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => {
            // SAFETY: getuid() is always safe to call on Unix.
            let uid: u32 = unsafe { libc::getuid() };
            Err(DaemonError::SocketSetup {
                path: socket_path.to_path_buf(),
                reason: format!(
                    "socket parent {} is not writable by uid {}: {e}. \
                     Either change ownership, or set SQRY_DAEMON_SOCKET \
                     to a directory you own.",
                    parent.display(),
                    uid,
                ),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Accepted-stream enum + Listener.
// ---------------------------------------------------------------------------

enum AcceptedStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    Pipe(tokio::net::windows::named_pipe::NamedPipeServer),
}

#[cfg(unix)]
enum Listener {
    Unix(tokio::net::UnixListener),
}

#[cfg(windows)]
enum Listener {
    Pipe(WindowsPipeAcceptor),
}

impl Listener {
    async fn bind(cfg: &DaemonConfig, path: &Path) -> DaemonResult<Self> {
        #[cfg(unix)]
        {
            let l = bind_unix(cfg, path).await?;
            Ok(Listener::Unix(l))
        }
        #[cfg(windows)]
        {
            let _ = cfg; // consumed here once for the Windows branch
            let name = path.to_string_lossy().into_owned();
            let acceptor = WindowsPipeAcceptor::new(name)?;
            Ok(Listener::Pipe(acceptor))
        }
    }

    async fn accept(&mut self) -> io::Result<AcceptedStream> {
        match self {
            #[cfg(unix)]
            Self::Unix(l) => {
                let (s, _addr) = l.accept().await?;
                Ok(AcceptedStream::Unix(s))
            }
            #[cfg(windows)]
            Self::Pipe(a) => {
                let s = a.accept().await?;
                Ok(AcceptedStream::Pipe(s))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unix bind (two-branch policy).
// ---------------------------------------------------------------------------

#[cfg(unix)]
enum UnixBindMode {
    RuntimeDir,
    Configured,
}

#[cfg(unix)]
fn classify_bind_mode(cfg: &DaemonConfig) -> UnixBindMode {
    if cfg.socket.path.is_some() || std::env::var_os(ENV_SOCKET_PATH).is_some() {
        UnixBindMode::Configured
    } else {
        UnixBindMode::RuntimeDir
    }
}

#[cfg(unix)]
async fn bind_unix(cfg: &DaemonConfig, path: &Path) -> DaemonResult<tokio::net::UnixListener> {
    match classify_bind_mode(cfg) {
        UnixBindMode::RuntimeDir => bind_unix_runtime(path).await,
        UnixBindMode::Configured => bind_unix_configured(path).await,
    }
}

#[cfg(unix)]
async fn bind_unix_runtime(path: &Path) -> DaemonResult<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    remove_stale_socket_if_dead(path).await?;
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(unix)]
async fn bind_unix_configured(path: &Path) -> DaemonResult<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if probe_socket_alive(path).await {
                return Err(DaemonError::Config {
                    path: path.to_path_buf(),
                    source: anyhow!("socket path already in use by a live daemon"),
                });
            }
            // Stale socket: liveness probe confirmed no process is listening.
            // Safe to unlink and rebind regardless of how the path was
            // configured — the prior daemon is gone.
            tracing::warn!(
                path = %path.display(),
                "stale socket detected at configured path; unlinking and rebinding"
            );
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(DaemonError::Config {
                path: path.to_path_buf(),
                source: anyhow!("configured socket path exists and is not a socket"),
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(DaemonError::Io(e)),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(unix)]
async fn remove_stale_socket_if_dead(path: &Path) -> DaemonResult<()> {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if probe_socket_alive(path).await {
                return Err(DaemonError::Config {
                    path: path.to_path_buf(),
                    source: anyhow!("socket path already in use by a live daemon"),
                });
            }
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(DaemonError::Config {
                path: path.to_path_buf(),
                source: anyhow!("runtime path exists and is not a socket"),
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(DaemonError::Io(e)),
    }
    Ok(())
}

/// Hard deadline for the async UDS liveness probe. Loopback UDS
/// handshakes complete in sub-millisecond-to-~1 ms under normal load;
/// 100 ms is comfortably above that budget while still short enough
/// that a wedged kernel path (ptrace target, frozen filesystem,
/// signal-paused peer) does not stall daemon startup. Kernel-level
/// unresponsiveness classifies the path as "not a live daemon" and
/// yields to the refuse/unlink fallback.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// Async liveness probe for a UDS path.
///
/// Returns `true` if a process accepts a UDS connection at `path`
/// within [`PROBE_TIMEOUT`]; `false` otherwise (stale-socket,
/// `ENOENT`, or kernel stall past the deadline). Uses tokio's async
/// UDS connect so the probe never blocks the Tokio reactor — the
/// future yields to the runtime while the kernel drives the connect
/// handshake.
///
/// On a successful probe the returned `UnixStream` is dropped
/// immediately: closing the connection is the correct signal to the
/// peer that this was a liveness ping, not a real client. Remote-peer
/// RST logs on a healthy daemon are a benign consequence.
#[cfg(unix)]
async fn probe_socket_alive(path: &Path) -> bool {
    match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(path)).await {
        Ok(Ok(stream)) => {
            // Explicit drop: the close is the probe's "hang up"
            // signal to the peer. Keep the drop inline for clarity —
            // relying on end-of-arm drop works, but an explicit drop
            // documents the intent.
            drop(stream);
            true
        }
        Ok(Err(_)) => false,    // ECONNREFUSED / ENOENT / other
        Err(_elapsed) => false, // kernel stall past deadline
    }
}

// ---------------------------------------------------------------------------
// Windows named-pipe acceptor.
// ---------------------------------------------------------------------------

#[cfg(windows)]
struct WindowsPipeAcceptor {
    name: String,
    next: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
impl WindowsPipeAcceptor {
    fn new(name: String) -> io::Result<Self> {
        let full = pipe_fullname(&name);
        let next = Some(create_pipe_instance(&full, true)?);
        Ok(Self { name: full, next })
    }

    async fn accept(&mut self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let server = self.next.take().ok_or_else(|| {
            io::Error::other("pipe acceptor in invalid state: no pending instance")
        })?;
        server.connect().await?;
        self.next = Some(create_pipe_instance(&self.name, false)?);
        Ok(server)
    }
}

#[cfg(windows)]
fn pipe_fullname(name: &str) -> String {
    if name.starts_with(r"\\.\pipe\") {
        name.to_owned()
    } else {
        format!(r"\\.\pipe\{name}")
    }
}

#[cfg(windows)]
fn create_pipe_instance(
    full_name: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .max_instances(255)
        .access_inbound(true)
        .access_outbound(true)
        .create(full_name)
}
