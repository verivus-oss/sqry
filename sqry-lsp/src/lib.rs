mod cancel;
mod cli;
pub mod config;
mod conversion;
pub mod daemon_host;
pub mod documents;
pub mod file_types;
pub mod handlers;
pub mod protocol;
mod security;
mod server;
pub mod session;
pub mod utils;

pub use cancel::spawn_blocking;
pub use cli::{LspCli, LspOptions};
pub use server::SqryLanguageServer;

use anyhow::{Context, Result};
use log::{error, info};
use session::SessionManager;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tower_lsp::{ClientSocket, LspService};

/// Environment variable that suppresses daemon auto-start in `run_daemon_client_mode`.
///
/// When set to `"1"`, the LSP shim will NOT attempt to start sqryd automatically
/// when the daemon socket is unreachable. The original connect error is returned
/// directly. This is intended for CI environments and users who want explicit
/// control over the daemon lifecycle.
const ENV_NO_AUTO_START: &str = "SQRY_DAEMON_NO_AUTO_START";

/// Exit code returned by `sqryd start --detach` when the daemon is already running.
/// Treated as success — the daemon is available, regardless of who started it.
const SQRYD_ALREADY_RUNNING_EXIT_CODE: i32 = 75;

/// Run the sqry LSP server with the provided options.
///
/// Dispatch logic:
///
/// - `options.daemon == true` → [`run_daemon_client_mode`]: this
///   process is a SHIM CLIENT; connect to the sqryd daemon at
///   `options.daemon_socket` (or the resolved default) and pump
///   stdio bytes between the editor and the daemon's hosted
///   tower_lsp server. No in-process `SessionManager` is created.
/// - Otherwise → the legacy standalone path: spin up a
///   [`SessionManager`] and serve either stdio or TCP in the same
///   process.
///
/// The daemon-client branch is checked first and returns directly,
/// so the two transports never race for stdio.
///
/// # Errors
///
/// Returns an error when the Tokio runtime fails to start or when any
/// transport encounters unrecoverable IO failures. In daemon-client
/// mode, additionally returns errors wrapping
/// [`sqry_daemon_client::ClientError`] for connect / handshake /
/// pump failures.
pub fn run(options: LspOptions) -> Result<()> {
    init_logger(&options);

    let rt = Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    rt.block_on(async move {
        // Daemon-client mode owns stdio end-to-end, so check it first
        // and return before any legacy SessionManager / TCP listener
        // construction runs. Mutual exclusion with `--stdio` / `--socket`
        // is already enforced by clap's `conflicts_with_all` on LspCli,
        // but the explicit early return keeps the legacy path reachable
        // only when `options.daemon == false`.
        if options.daemon {
            return run_daemon_client_mode(&options).await;
        }

        let session = SessionManager::new(options.clone());

        if let Some(addr) = &options.socket {
            serve_socket(addr, options.allow_public_bind, session.clone()).await?;
        }

        if options.use_stdio() {
            serve_stdio(session).await?;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Run as a shim CLIENT against a running sqryd daemon (Phase 8c U11).
///
/// Opens a UDS / named-pipe connection, drives the
/// [`sqry_daemon_client::ShimRegister`] → [`sqry_daemon_client::ShimRegisterAck`]
/// handshake with `ShimProtocol::Lsp`, then byte-pumps this process's
/// stdin/stdout against the resulting stream. The tower_lsp server
/// hosting the LSP session lives inside the daemon and is wired up
/// by the daemon's router via `daemon_host::host_on_streams`.
///
/// The daemon version advertised in `ShimRegisterAck.daemon_version`
/// is surfaced at `info!` log level so editors that tee `sqry-lsp`'s
/// stderr get a diagnostic banner. The final byte-counts returned by
/// [`sqry_daemon_client::pump_stdio`] are likewise logged at shutdown.
///
/// # Errors
///
/// - Any [`sqry_daemon_client::ClientError`] surfacing from
///   [`sqry_daemon_client::connect_shim`] (connect / handshake /
///   envelope-version / rejection).
/// - Any [`sqry_daemon_client::ClientError::Io`] from
///   [`sqry_daemon_client::pump_stdio`] (stdio broke mid-session).
///
/// Each error is wrapped in an [`anyhow::Error`] with a contextual
/// prefix so editor-side diagnostics cite which stage failed.
async fn run_daemon_client_mode(options: &LspOptions) -> Result<()> {
    let socket_path = resolve_daemon_socket(options.daemon_socket.as_deref());
    info!(
        "sqry-lsp connecting to sqryd daemon at {}",
        socket_path.display()
    );

    let conn = match sqry_daemon_client::connect_shim(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Lsp,
        std::process::id(),
    )
    .await
    {
        Ok(conn) => conn,
        Err(ref e) if is_connect_failure(e) => {
            // Connect-level failure: daemon not running (or not yet started).
            // Check opt-out before attempting auto-start.
            if std::env::var_os(ENV_NO_AUTO_START).as_deref() == Some(std::ffi::OsStr::new("1")) {
                anyhow::bail!("daemon shim connect failed: {e}");
            }
            info!(
                "daemon not reachable at {}; attempting auto-start",
                socket_path.display()
            );
            auto_start_daemon(&socket_path).await?;
            // Retry after auto-start succeeds and socket is reachable.
            sqry_daemon_client::connect_shim(
                &socket_path,
                sqry_daemon_client::ShimProtocol::Lsp,
                std::process::id(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("daemon shim connect failed after auto-start: {e}"))?
        }
        Err(e) => {
            // Non-connect failure (handshake, version mismatch, rejected): do not retry.
            anyhow::bail!("daemon shim connect failed: {e}");
        }
    };

    info!(
        "sqry-lsp shim connected (daemon version {})",
        conn.daemon_version()
    );

    let (bytes_up, bytes_down) = sqry_daemon_client::pump_stdio(conn)
        .await
        .map_err(|e| anyhow::anyhow!("daemon byte-pump failed: {e}"))?;

    info!("sqry-lsp shim pump complete: {bytes_up} bytes up, {bytes_down} bytes down");
    Ok(())
}

/// Returns `true` when the [`sqry_daemon_client::ClientError`] indicates a
/// transport-level connect failure (i.e. the daemon is not running), as opposed
/// to a protocol-level failure (handshake rejection, version mismatch, etc.).
///
/// Only connect-level failures warrant an auto-start retry. If the daemon is
/// running but rejects the connection or sends an incompatible version, retrying
/// is pointless — the same response will recur until the operator takes action.
fn is_connect_failure(err: &sqry_daemon_client::ClientError) -> bool {
    matches!(
        err,
        sqry_daemon_client::ClientError::Connect { .. }
            | sqry_daemon_client::ClientError::ConnectTimeout { .. }
    )
}

/// Resolve the path to the `sqryd` binary.
///
/// Resolution order (first match wins):
/// 1. `$SQRYD_PATH` environment variable.
/// 2. Sibling of `std::env::current_exe()` (canonicalized, symlink-safe).
///    On Windows also tries the `.exe`-suffixed variant.
/// 3. `PATH` lookup via [`which::which`].
///
/// Note: there is no `--sqryd-path` CLI flag on the LSP binary. Editors (VS
/// Code, Neovim, etc.) spawn `sqry-lsp` without a mechanism to pass such a
/// flag ergonomically. The `SQRYD_PATH` environment variable is the correct
/// configuration surface for editors.
///
/// # Errors
///
/// Returns an error if no `sqryd` binary can be located, with an actionable
/// message directing the user to install sqryd or set `SQRYD_PATH`.
fn resolve_sqryd_binary() -> Result<PathBuf> {
    // 1. SQRYD_PATH environment variable.
    if let Some(val) = std::env::var_os("SQRYD_PATH") {
        let path = PathBuf::from(val);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("SQRYD_PATH={} does not exist", path.display());
    }

    // 2. Sibling of the current executable (canonical, symlink-resolved).
    if let Ok(exe) = std::env::current_exe() {
        // Canonicalize to follow symlinks (prevents ../foo path traversal).
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(dir) = canonical.parent() {
            let sibling = dir.join("sqryd");
            if sibling.exists() {
                return Ok(sibling);
            }
            // Windows: try .exe suffix.
            let sibling_exe = dir.join("sqryd.exe");
            if sibling_exe.exists() {
                return Ok(sibling_exe);
            }
        }
    }

    // 3. PATH lookup.
    which::which("sqryd").with_context(|| {
        "sqryd binary not found. \
         Install sqryd alongside sqry-lsp, set SQRYD_PATH to its path, \
         or start sqryd manually before launching the LSP server."
            .to_owned()
    })
}

/// Resolve the `sqryd` binary, exec `sqryd start --detach`, and poll the
/// socket until it becomes reachable (up to a 5-second budget).
///
/// Exit code `75` from `sqryd start --detach` is treated as success — it
/// means the daemon was already running, which is the desired end-state.
///
/// **Socket path propagation.** If the caller resolved a non-default
/// `socket_path` (via `--daemon-socket` or `$SQRYD_SOCKET`), the
/// spawned `sqryd` must bind on that same path. We propagate it via
/// `SQRY_DAEMON_SOCKET`. This env var is the authoritative server-side
/// override (see `sqry-daemon::DaemonConfig::apply_env_overrides`), so the
/// daemon will bind exactly where the client is polling. When both client
/// and server would use the same default we still forward the env var for
/// symmetry; it is a no-op on the server side.
///
/// The socket path is passed as `&OsStr` to `Command::env` so non-UTF-8
/// path components are preserved losslessly. This matches the documented
/// guarantee in `resolve_daemon_socket` and the daemon's own
/// `env::var_os(ENV_SOCKET_PATH)` consumption path.
///
/// **Windows behaviour.** On Windows, `sqryd start --detach` runs the
/// daemon in the foreground (the `--detach` flag is a no-op; see
/// `sqry-daemon::entrypoint::run_start_detach`). Waiting for that process
/// to exit would block indefinitely. We therefore spawn-and-forget on
/// Windows: the child starts the daemon and continues running in the
/// background; we immediately move on to polling the named pipe.
///
/// # Errors
///
/// - Binary resolution error (`SQRYD_PATH` or PATH lookup failure).
/// - Process spawn / start failure.
/// - Non-zero, non-75 exit code from `sqryd start --detach` (Unix only).
/// - Socket not reachable within the 5-second poll budget.
async fn auto_start_daemon(socket_path: &Path) -> Result<()> {
    let binary = resolve_sqryd_binary()?;
    info!("auto-starting sqryd from {}", binary.display());

    // Forward the resolved socket path to the spawned daemon using the OsStr
    // API so non-UTF-8 path components are preserved losslessly. `to_string_lossy()`
    // would mangle such paths, causing the daemon to bind a different socket
    // than the client polls/connects to.
    spawn_sqryd_start_detach(&binary, socket_path).await?;

    poll_socket_reachable(socket_path, Duration::from_secs(5)).await?;
    info!("sqryd auto-started successfully");
    Ok(())
}

/// Spawn `sqryd start --detach` with `SQRY_DAEMON_SOCKET` set to
/// `socket_path` (passed via `OsStr` to preserve non-UTF-8 paths).
///
/// **Unix**: waits for the intermediate parent process to exit.
/// `sqryd start --detach` on Unix performs a double-fork — the
/// intermediate parent exits quickly once the grandchild (the real daemon)
/// is up, making `.status()` wait safe and bounded. Exit code `75`
/// (already running) is treated as success.
///
/// **Windows**: `sqryd start --detach` runs the daemon in the foreground and
/// never exits, so waiting on `.status()` would block forever. Instead we
/// call `.spawn()` and immediately drop the `Child` handle. The process is
/// given `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` creation flags so it
/// has no console window and does not inherit Ctrl-C from the editor session,
/// matching the daemon's own Windows detach path in
/// `sqry-daemon::lifecycle::detach::spawn_daemon_grandchild`. The named-pipe
/// poll in [`poll_socket_reachable`] detects when the daemon is ready.
///
/// On non-Unix/non-Windows platforms the non-Unix (spawn-and-forget) branch
/// is used (best effort; no native detach mechanism is available).
async fn spawn_sqryd_start_detach(binary: &Path, socket_path: &Path) -> Result<()> {
    let binary = binary.to_path_buf();
    let socket_path = socket_path.to_path_buf();

    #[cfg(unix)]
    {
        // spawn_blocking: workspace tokio does not enable the `process`
        // feature, so tokio::process::Command is unavailable. The blocking
        // pool is appropriate here because `sqryd start --detach` completes
        // quickly on Unix (the intermediate parent exits after forking the
        // grandchild daemon).
        //
        // `binary_for_err` retains a display copy for the `with_context`
        // error message after `binary` is moved into the closure.
        let binary_for_err = binary.clone();
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&binary)
                .args(["start", "--detach"])
                // Use as_os_str() to preserve non-UTF-8 socket paths losslessly.
                .env("SQRY_DAEMON_SOCKET", socket_path.as_os_str())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .status()
        })
        .await
        .with_context(|| "auto-start: spawn_blocking task panicked")?
        .with_context(|| {
            format!(
                "auto-start: failed to exec sqryd at {}",
                binary_for_err.display()
            )
        })?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            if code != SQRYD_ALREADY_RUNNING_EXIT_CODE {
                anyhow::bail!("auto-start: sqryd start --detach exited with code {code}");
            }
            // code == 75: already running — treat as success.
            info!("sqryd is already running (exit code 75)");
        }
    }

    #[cfg(not(unix))]
    {
        // On Windows (and any other non-Unix platform), `--detach` is a no-op:
        // sqryd runs in the foreground and never returns. Spawn-and-forget so
        // we can proceed to socket polling. The child is detached from our
        // stdio; stderr is inherited so failures appear in the editor's
        // output panel.
        //
        // `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` are Windows creation
        // flags that move the child into a new process group with no console
        // window and no Ctrl-C forwarding. This matches the daemon's own
        // Windows grandchild-spawn path in
        // `sqry-daemon::lifecycle::detach::spawn_daemon_grandchild`.
        //
        // `binary_for_err` retains a display copy for the `with_context`
        // error message after `binary` is moved into the closure.
        let binary_for_err = binary.clone();
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&binary);
            cmd.args(["start", "--detach"])
                // Use as_os_str() to preserve non-UTF-8 socket paths losslessly.
                .env("SQRY_DAEMON_SOCKET", socket_path.as_os_str())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit());
            // Apply Windows-specific creation flags to detach from editor session.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                // DETACHED_PROCESS (0x0000_0008): no console window.
                // CREATE_NEW_PROCESS_GROUP (0x0000_0200): own Ctrl-C group.
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
                cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            }
            cmd.spawn().map(|_child| ()) // drop Child — daemon runs independently
        })
        .await
        .with_context(|| "auto-start: spawn_blocking task panicked")?
        .with_context(|| {
            format!(
                "auto-start: failed to spawn sqryd at {}",
                binary_for_err.display()
            )
        })?;
    }

    Ok(())
}

/// Poll the socket path at 100ms intervals until it is reachable or the
/// budget is exhausted.
///
/// Uses a lightweight raw connect probe ([`try_connect_async`]) rather than
/// the full `connect_shim` handshake, so the poll does not consume a shim
/// connection slot.
///
/// # Errors
///
/// Returns an error with the socket path and budget seconds when the socket
/// is not reachable within the budget.
async fn poll_socket_reachable(socket_path: &Path, budget: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if try_connect_async(socket_path).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "auto-start: sqryd started but socket {} not reachable within {}s",
                socket_path.display(),
                budget.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Probe whether the daemon socket is accepting connections.
///
/// This is a raw connect (no handshake frames) used only for readiness
/// polling. The connection is immediately dropped on success.
///
/// - **Unix**: `tokio::net::UnixStream::connect` — returns `true` if the
///   connect syscall succeeds.
/// - **Windows**: attempts to open the named pipe via
///   `tokio::net::windows::named_pipe::ClientOptions::new().open()`.
///   A named pipe is "ready" when `CreateFile` succeeds, which requires
///   the server to have called `CreateNamedPipe` AND entered the
///   listening state. A filesystem existence check (`path.exists()`) is
///   NOT sufficient — the pipe file appears in the namespace before the
///   server is actually listening and ready to accept connections.
/// - **Other**: always returns `false` (no platform support).
async fn try_connect_async(socket_path: &Path) -> bool {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(socket_path).await.is_ok()
    }
    #[cfg(windows)]
    {
        // `ClientOptions::new().open()` is a non-blocking pipe open attempt.
        // It succeeds only when the pipe server is listening and a server
        // instance is available. This matches the canonical pattern used by
        // `sqry-daemon::lifecycle::detach::try_connect` and
        // `sqry-daemon-client` for Windows named-pipe readiness detection.
        // Pass `socket_path.as_os_str()` directly — `ClientOptions::open`
        // accepts `impl AsRef<OsStr>`, so no lossy UTF-8 conversion is needed
        // and non-UTF-8 pipe paths (e.g. from a custom `SQRY_DAEMON_SOCKET`)
        // are preserved losslessly.
        use tokio::net::windows::named_pipe::ClientOptions;
        ClientOptions::new().open(socket_path.as_os_str()).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = socket_path;
        false
    }
}

/// Resolve the UDS / named-pipe path the shim client should connect to.
///
/// Precedence matches the design in §G of the Phase 8c design doc:
///
/// 1. Explicit `--daemon-socket <PATH>` override.
/// 2. `$SQRYD_SOCKET` environment variable (the shared client-side
///    env var; distinct from the server's `SQRY_DAEMON_SOCKET` which
///    configures where the daemon BINDS).
/// 3. Platform default, exactly mirroring
///    `sqry-daemon::DaemonConfig::socket_path()` so a default daemon +
///    default client meet without configuration:
///    - unix: `$XDG_RUNTIME_DIR/sqry/sqryd.sock` when set, else
///      `$TMPDIR/sqry-<uid>/sqryd.sock`, else
///      `/tmp/sqry-<uid>/sqryd.sock`. `<uid>` is the real POSIX UID
///      via `libc::getuid()` — this matches the daemon's
///      `user_scoped_dir_name` helper exactly (see Codex Task 5
///      iter-1 MAJOR fix note in that module).
///    - windows: `\\.\pipe\sqry` (matching the daemon's default
///      `pipe_name = "sqry"`).
///
/// `std::env::var_os` returns `None` only when the variable is absent;
/// it yields `Some(OsString)` for any present value including non-UTF-8.
/// Non-UTF-8 values are accepted as-is and converted to `PathBuf` —
/// the platform path APIs are `OsStr`-native so no lossy conversion
/// occurs. This matches the daemon-side `env::var_os(ENV_SOCKET_PATH)`
/// behaviour in `sqry-daemon::DaemonConfig::apply_env_overrides`.
fn resolve_daemon_socket(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    if let Some(env_val) = std::env::var_os("SQRYD_SOCKET") {
        return PathBuf::from(env_val);
    }
    platform_default_daemon_socket()
}

/// Unix platform default — mirrors
/// `sqry-daemon::config::runtime_dir()` + `.join("sqryd.sock")`.
#[cfg(unix)]
fn platform_default_daemon_socket() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("sqry").join("sqryd.sock");
    }
    // UID-scoped `/tmp` fallback, matching `user_scoped_dir_name` in
    // `sqry-daemon/src/config.rs`. The `/tmp` branch is authoritative
    // when `TMPDIR` is unset (the common systemd-unit /
    // Docker-container case on Linux).
    // SAFETY: `libc::getuid` is a POSIX call with no preconditions
    // and cannot fail; calling it from a multi-threaded runtime is
    // safe per POSIX. Mirrors the safety argument in
    // `sqry-daemon::config::user_scoped_dir_name`.
    let uid = unsafe { libc::getuid() };
    let dir_name = format!("sqry-{uid}");
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        return PathBuf::from(tmp).join(&dir_name).join("sqryd.sock");
    }
    PathBuf::from("/tmp").join(&dir_name).join("sqryd.sock")
}

/// Windows platform default — mirrors
/// `sqry-daemon::DaemonConfig::socket_path()` with the default
/// `pipe_name = "sqry"`.
#[cfg(windows)]
fn platform_default_daemon_socket() -> PathBuf {
    PathBuf::from(r"\\.\pipe\sqry")
}

fn init_logger(options: &LspOptions) {
    let env = env_logger::Env::default().default_filter_or(&options.log_level);
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp(None);
    if builder.try_init().is_err() {
        // Logger already initialised elsewhere (e.g., via sqry CLI); ignore.
    }
}

/// Build an LSP service with all sqry custom methods registered.
///
/// This is the single source of truth for custom method registration,
/// shared by stdio, socket, and test transports.
#[allow(clippy::too_many_lines)] // Single registration table for all custom LSP methods; splitting it would hide the authoritative method inventory.
pub(crate) fn build_sqry_service(
    session: SessionManager,
) -> (LspService<SqryLanguageServer>, ClientSocket) {
    LspService::build(|client| server::SqryLanguageServer::new(client, session))
        .custom_method(
            "sqry/search",
            server::SqryLanguageServer::handle_sqry_search,
        )
        .custom_method(
            "sqry/references",
            server::SqryLanguageServer::handle_sqry_relation,
        )
        .custom_method(
            "sqry/indexStatus",
            server::SqryLanguageServer::handle_index_status,
        )
        .custom_method(
            "sqry/listFiles",
            server::SqryLanguageServer::handle_list_files,
        )
        .custom_method(
            "sqry/listSymbols",
            server::SqryLanguageServer::handle_list_symbols,
        )
        .custom_method(
            "sqry/listFilesByLanguage",
            server::SqryLanguageServer::handle_list_files_by_language,
        )
        .custom_method(
            "sqry/listCrossLanguageRelations",
            server::SqryLanguageServer::handle_list_cross_language_relations,
        )
        .custom_method(
            "sqry/listDuplicateGroups",
            server::SqryLanguageServer::handle_list_duplicate_groups,
        )
        .custom_method(
            "sqry/listCircularDependencies",
            server::SqryLanguageServer::handle_list_circular_dependencies,
        )
        .custom_method(
            "sqry/listUnusedSymbols",
            server::SqryLanguageServer::handle_list_unused_symbols,
        )
        .custom_method(
            "sqry/hierarchicalSearch",
            server::SqryLanguageServer::handle_hierarchical_search,
        )
        .custom_method("sqry/ask", server::SqryLanguageServer::handle_ask)
        .custom_method(
            "sqry/directCallers",
            server::SqryLanguageServer::handle_direct_callers,
        )
        .custom_method(
            "sqry/directCallees",
            server::SqryLanguageServer::handle_direct_callees,
        )
        .custom_method(
            "sqry/batchCallerCalleeCount",
            server::SqryLanguageServer::handle_batch_caller_callee_count,
        )
        .custom_method(
            "sqry/graphStats",
            server::SqryLanguageServer::handle_graph_stats,
        )
        .custom_method(
            "sqry/patternSearch",
            server::SqryLanguageServer::handle_pattern_search,
        )
        .custom_method(
            "sqry/dependencyImpact",
            server::SqryLanguageServer::handle_dependency_impact,
        )
        .custom_method(
            "sqry/explainSymbol",
            server::SqryLanguageServer::handle_explain_symbol,
        )
        .custom_method(
            "sqry/tracePath",
            server::SqryLanguageServer::handle_trace_path,
        )
        .custom_method(
            "sqry/graphExport",
            server::SqryLanguageServer::handle_graph_export,
        )
        .custom_method("sqry/subgraph", server::SqryLanguageServer::handle_subgraph)
        .custom_method(
            "sqry/isNodeInCycle",
            server::SqryLanguageServer::handle_is_node_in_cycle,
        )
        .custom_method(
            "sqry/similarSymbols",
            server::SqryLanguageServer::handle_similar_symbols,
        )
        .custom_method(
            "sqry/showDependencies",
            server::SqryLanguageServer::handle_show_dependencies,
        )
        .custom_method(
            "sqry/complexityMetrics",
            server::SqryLanguageServer::handle_complexity_metrics,
        )
        .custom_method(
            "sqry/getInsights",
            server::SqryLanguageServer::handle_get_insights,
        )
        .custom_method(
            "sqry/semanticDiff",
            server::SqryLanguageServer::handle_semantic_diff,
        )
        .finish()
}

/// Serve the LSP protocol over stdio.
///
/// # Errors
///
/// Returns an error when the transport layer fails to initialise or run.
///
/// # Cancellation Safety
///
/// This function is the main LSP server loop. It is cancellation-safe because
/// dropping the future will gracefully shutdown the `tower_lsp::Server`, which
/// closes stdio connections cleanly. No state corruption occurs as the server
/// manages all connection state internally.
async fn serve_stdio(session: SessionManager) -> Result<()> {
    info!("sqry-lsp using stdio transport");

    let (service, messages) = build_sqry_service(session);
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    tower_lsp::Server::new(stdin, stdout, messages)
        .serve(service)
        .await;
    Ok(())
}

/// Build an in-process LSP service for testing purposes.
#[must_use]
pub fn build_test_service(session: &SessionManager) -> LspService<SqryLanguageServer> {
    let (service, _messages) = build_sqry_service(session.clone());
    service
}

/// Serve the LSP protocol over TCP.
///
/// # Errors
///
/// Returns an error when binding the socket or serving the connection fails.
///
/// # Cancellation Safety
///
/// This function is the main TCP socket server loop. It is cancellation-safe
/// because dropping the future will stop accepting new connections and drop the
/// `TcpListener`, which releases the bound socket. In-flight connections spawned
/// via `tokio::spawn` continue running independently and are not affected.
async fn serve_socket(addr: &str, allow_public_bind: bool, session: SessionManager) -> Result<()> {
    let resolved_addr = addr
        .to_socket_addrs()
        .context("invalid socket address")?
        .next()
        .ok_or_else(|| anyhow::anyhow!("unable to resolve socket address"))?;

    let listener = TcpListener::bind(resolved_addr)
        .await
        .context("failed to bind LSP socket")?;

    // Validate bind address for security concerns
    security::validate_bind_address(resolved_addr, allow_public_bind);

    info!("sqry-lsp listening on socket {resolved_addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("accepted LSP client from {peer}");
                let session_clone = session.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_socket_stream(stream, session_clone).await {
                        error!("socket session ended with error: {err:?}");
                    }
                });
            }
            Err(err) => {
                return Err(err).context("failed to accept LSP socket connection");
            }
        }
    }
}

/// Handle an individual TCP stream.
///
/// # Errors
///
/// Returns an error when the LSP server fails to process the stream.
///
/// # Cancellation Safety
///
/// This function handles a single LSP client connection. It is cancellation-safe
/// because dropping the future will gracefully shutdown the `tower_lsp::Server`
/// for this connection, closing the TCP stream cleanly. The session state (shared
/// via `SessionManager`) remains consistent as all mutations are atomic.
async fn handle_socket_stream(stream: TcpStream, session: SessionManager) -> Result<()> {
    let (reader, writer) = tokio::io::split(stream);
    let (service, messages) = build_sqry_service(session);
    tower_lsp::Server::new(reader, writer, messages)
        .serve(service)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::LspOptions;
    use crate::session::SessionManager;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Serialises tests that mutate process-wide environment variables.
    /// Without this, two tests racing on `SQRYD_SOCKET` (or other env
    /// vars read by `resolve_daemon_socket`) can observe each other's
    /// state. Mirrors the identical pattern in
    /// `sqry-daemon/src/config.rs::tests::ENV_LOCK`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_options() -> LspOptions {
        LspOptions {
            stdio: false,
            socket: None,
            index_root: None,
            log_level: "error".to_string(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
        }
    }

    // ── build_test_service ────────────────────────────────────────────────────

    #[test]
    fn build_test_service_returns_service_without_panic() {
        let session = SessionManager::new(make_options());
        // Just verify construction doesn't panic
        let _service = super::build_test_service(&session);
    }

    // ── resolve_daemon_socket precedence ─────────────────────────────────────
    //
    // These tests exercise the three-tier lookup (override > env > platform
    // default) directly. They are deliberately synchronous (no tokio runtime
    // needed) because `resolve_daemon_socket` is a pure path-resolution
    // function with no async I/O.

    /// An explicit `--daemon-socket` override takes precedence over any env
    /// variable or platform default. This is the highest-priority tier in
    /// the three-way lookup.
    #[test]
    fn resolve_daemon_socket_override_wins() {
        let override_path = Path::new("/explicit/override.sock");
        // Even if SQRYD_SOCKET were set (it might be in CI environments),
        // the explicit override must win.
        let result = super::resolve_daemon_socket(Some(override_path));
        assert_eq!(
            result,
            PathBuf::from("/explicit/override.sock"),
            "explicit override must beat env var and platform default"
        );
    }

    /// When no override is supplied but `$SQRYD_SOCKET` is set, that env
    /// var is used as the second-priority tier. The test serialises via
    /// `ENV_LOCK` to avoid racing with other env-mutating tests in this
    /// module, and restores the original value on exit.
    #[test]
    fn resolve_daemon_socket_env_var_wins_over_platform_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Capture original value so we can restore it.
        let original = std::env::var_os("SQRYD_SOCKET");

        let sentinel = "/tmp/sqry-lsp-test-sentinel-env.sock";
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe { std::env::set_var("SQRYD_SOCKET", sentinel) };

        let result = super::resolve_daemon_socket(None);

        // Restore before any assertion (so cleanup runs even on panic).
        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original {
                Some(v) => std::env::set_var("SQRYD_SOCKET", v),
                None => std::env::remove_var("SQRYD_SOCKET"),
            }
        }

        assert_eq!(
            result,
            PathBuf::from(sentinel),
            "$SQRYD_SOCKET must win over platform default when no explicit override"
        );
    }

    /// When neither override nor env var is present, `resolve_daemon_socket`
    /// falls through to the platform default. On Unix the default is a
    /// UID-scoped path (XDG or /tmp); on Windows it is the named-pipe path.
    /// The test serialises via `ENV_LOCK` to avoid racing with other
    /// env-mutating tests in this module.
    #[test]
    fn resolve_daemon_socket_falls_through_to_platform_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Temporarily unset SQRYD_SOCKET so the env tier is skipped.
        let original = std::env::var_os("SQRYD_SOCKET");
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe { std::env::remove_var("SQRYD_SOCKET") };

        let result = super::resolve_daemon_socket(None);

        // Restore before any assertion so a failing assert does not
        // leave SQRYD_SOCKET unset for sibling tests.
        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original {
                Some(v) => std::env::set_var("SQRYD_SOCKET", v),
                None => std::env::remove_var("SQRYD_SOCKET"),
            }
        }

        let result_str = result.to_string_lossy();
        assert!(
            !result_str.is_empty(),
            "platform default must be a non-empty path"
        );
        #[cfg(unix)]
        assert!(
            result_str.ends_with("sqryd.sock"),
            "unix default must end with sqryd.sock, got: {result_str}"
        );
        #[cfg(windows)]
        assert!(
            result_str.starts_with(r"\\.\pipe\"),
            "windows default must be a named pipe, got: {result_str}"
        );
    }

    // ── run_daemon_client_mode error path ─────────────────────────────────────
    //
    // Tests that the runtime path (`run_daemon_client_mode`) surfaces a
    // diagnostic error when the daemon socket does not exist. We use a
    // deterministically absent path so the `connect_shim` call returns
    // `ClientError::Connect` without touching real I/O.

    /// When the daemon socket path does not exist and auto-start is opted out,
    /// `run_daemon_client_mode` must return an `Err` wrapping "daemon shim
    /// connect failed". `SQRY_DAEMON_NO_AUTO_START=1` is set so the test is
    /// deterministic even in environments where `sqryd` is installed.
    ///
    /// The `ENV_LOCK` guard is dropped before the `.await` to avoid the
    /// `clippy::await_holding_lock` lint. Env-mutation serialisation still
    /// works because the guard is re-acquired for the restore step.
    ///
    /// `#[serial]` ensures this test does not race with other async tests
    /// that mutate `SQRY_DAEMON_NO_AUTO_START` during the `.await` window
    /// (the window between dropping ENV_LOCK and re-acquiring it for restore).
    #[serial_test::serial]
    #[tokio::test]
    async fn run_daemon_client_mode_returns_error_when_socket_absent() {
        // Suppress auto-start so the test exercises the direct connect-fail path.
        // Set inside the lock, then drop the guard before awaiting.
        let original_no_auto = {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let original = std::env::var_os(super::ENV_NO_AUTO_START);
            // SAFETY: serialised by ENV_LOCK; guard dropped before await.
            unsafe { std::env::set_var(super::ENV_NO_AUTO_START, "1") };
            original
        }; // guard dropped here — safe to .await below

        // Use a path that cannot possibly exist as a UDS socket.
        let absent_socket =
            PathBuf::from("/tmp/sqry-lsp-test-nonexistent-daemon-socket-u11-runtime.sock");
        let options = LspOptions {
            stdio: false,
            socket: None,
            index_root: None,
            log_level: "error".to_string(),
            config: None,
            allow_public_bind: false,
            daemon: true,
            daemon_socket: Some(absent_socket),
        };

        let result = super::run_daemon_client_mode(&options).await;

        // Restore env under the lock.
        {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: serialised by ENV_LOCK.
            unsafe {
                match original_no_auto {
                    Some(v) => std::env::set_var(super::ENV_NO_AUTO_START, v),
                    None => std::env::remove_var(super::ENV_NO_AUTO_START),
                }
            }
        }

        let err = result.expect_err("must fail when daemon socket is absent");
        let msg = format!("{err}");
        assert!(
            msg.contains("daemon shim connect failed"),
            "error must cite the connect stage; got: {msg}"
        );
    }

    // ── U1: auto-start unit tests ─────────────────────────────────────────────

    /// `resolve_sqryd_binary` uses `SQRYD_PATH` as the first resolution tier.
    /// When it points to a file that exists, that path is returned directly.
    #[test]
    fn resolve_sqryd_binary_finds_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Create a real file so the existence check passes.
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_sqryd = dir.path().join("sqryd");
        std::fs::write(&fake_sqryd, b"#!/bin/sh\n").expect("write fake sqryd");

        let original = std::env::var_os("SQRYD_PATH");
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe { std::env::set_var("SQRYD_PATH", &fake_sqryd) };

        let result = super::resolve_sqryd_binary();

        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original {
                Some(v) => std::env::set_var("SQRYD_PATH", v),
                None => std::env::remove_var("SQRYD_PATH"),
            }
        }

        assert!(
            result.is_ok(),
            "expected Ok when SQRYD_PATH points to an existing file; got: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            fake_sqryd,
            "returned path must match SQRYD_PATH"
        );
    }

    /// When `SQRYD_PATH` points to a non-existent file, `resolve_sqryd_binary`
    /// must return an `Err` with a message that includes `SQRYD_PATH`.
    ///
    /// Using a non-existent-path value for `SQRYD_PATH` gives us a
    /// deterministic error regardless of whether `sqryd` is installed on
    /// `PATH` or present as a sibling of the test binary. This avoids the
    /// flakiness of the "clear all env vars and hope sqryd is absent" approach.
    ///
    /// This test is serialised via ENV_LOCK to avoid races with other tests
    /// that mutate `SQRYD_PATH`.
    #[test]
    fn resolve_sqryd_binary_returns_error_when_not_found() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let original = std::env::var_os("SQRYD_PATH");

        // Point SQRYD_PATH to a path that is guaranteed not to exist.
        let nonexistent = "/tmp/sqry-lsp-test-nonexistent-sqryd-binary-u11-resolve";
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe { std::env::set_var("SQRYD_PATH", nonexistent) };

        let result = super::resolve_sqryd_binary();

        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original {
                Some(v) => std::env::set_var("SQRYD_PATH", v),
                None => std::env::remove_var("SQRYD_PATH"),
            }
        }

        let err = result.expect_err(
            "resolve_sqryd_binary must return Err when SQRYD_PATH points to a non-existent file",
        );
        let msg = format!("{err}");
        assert!(
            msg.contains("SQRYD_PATH"),
            "error must cite SQRYD_PATH so the user knows which variable is wrong; got: {msg}"
        );
    }

    /// `is_connect_failure` must return `true` for `ClientError::Connect`.
    #[test]
    fn is_connect_failure_true_for_connect_error() {
        let err = sqry_daemon_client::ClientError::Connect {
            path: PathBuf::from("/tmp/test.sock"),
            source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        };
        assert!(
            super::is_connect_failure(&err),
            "Connect variant must be classified as a connect failure"
        );
    }

    /// `is_connect_failure` must return `false` for `ClientError::HelloRejected`.
    /// Daemon is running but rejected the hello — auto-start should NOT be attempted.
    #[test]
    fn is_connect_failure_false_for_handshake_error() {
        let err = sqry_daemon_client::ClientError::HelloRejected;
        assert!(
            !super::is_connect_failure(&err),
            "HelloRejected must NOT be classified as a connect failure"
        );
    }

    /// `is_connect_failure` must return `false` for `ClientError::ShimRejected`.
    /// The daemon is running but has rejected this shim (e.g., capacity limit).
    /// Auto-start would not help.
    #[test]
    fn is_connect_failure_false_for_shim_rejected() {
        let err = sqry_daemon_client::ClientError::ShimRejected("capacity limit".to_owned());
        assert!(
            !super::is_connect_failure(&err),
            "ShimRejected must NOT be classified as a connect failure"
        );
    }

    /// When `SQRY_DAEMON_NO_AUTO_START=1` is set and the socket is absent,
    /// `run_daemon_client_mode` must return the original connect error WITHOUT
    /// attempting auto-start (error message must NOT mention "auto-start").
    ///
    /// The `ENV_LOCK` guard is dropped before the `.await` (same pattern as
    /// `run_daemon_client_mode_returns_error_when_socket_absent`) to satisfy
    /// the `clippy::await_holding_lock` lint.
    ///
    /// `#[serial]` ensures this test does not race with other async tests
    /// that mutate `SQRY_DAEMON_NO_AUTO_START` during the `.await` window.
    #[serial_test::serial]
    #[tokio::test]
    async fn auto_start_opt_out_via_env() {
        // Set env under lock, then drop guard before awaiting.
        let original_no_auto = {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let original = std::env::var_os(super::ENV_NO_AUTO_START);
            // SAFETY: serialised by ENV_LOCK; guard dropped before await.
            unsafe { std::env::set_var(super::ENV_NO_AUTO_START, "1") };
            original
        }; // guard dropped here

        let absent_socket = PathBuf::from("/tmp/sqry-lsp-test-optout-disabled.sock");
        let options = LspOptions {
            stdio: false,
            socket: None,
            index_root: None,
            log_level: "error".to_string(),
            config: None,
            allow_public_bind: false,
            daemon: true,
            daemon_socket: Some(absent_socket),
        };

        let result = super::run_daemon_client_mode(&options).await;

        // Restore under lock.
        {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: serialised by ENV_LOCK.
            unsafe {
                match original_no_auto {
                    Some(v) => std::env::set_var(super::ENV_NO_AUTO_START, v),
                    None => std::env::remove_var(super::ENV_NO_AUTO_START),
                }
            }
        }

        let err = result.expect_err("must fail when socket absent and auto-start opted out");
        let msg = format!("{err}");
        assert!(
            msg.contains("daemon shim connect failed"),
            "error must contain 'daemon shim connect failed'; got: {msg}"
        );
        assert!(
            !msg.contains("auto-start"),
            "error must NOT mention auto-start when SQRY_DAEMON_NO_AUTO_START=1; got: {msg}"
        );
    }

    /// `poll_socket_reachable` with a 0ms budget must return an error
    /// containing "not reachable" when the socket does not exist.
    #[tokio::test]
    async fn poll_socket_reachable_times_out() {
        let absent = Path::new("/tmp/sqry-lsp-test-poll-timeout-nonexistent.sock");
        let result = super::poll_socket_reachable(absent, Duration::from_millis(0)).await;
        let err = result.expect_err("poll must fail on non-existent socket with 0ms budget");
        let msg = format!("{err}");
        assert!(
            msg.contains("not reachable"),
            "error must mention 'not reachable'; got: {msg}"
        );
    }

    // ── Additional coverage tests (Codex iter-0 finding M-1) ─────────────────

    /// `is_connect_failure` must return `true` for `ClientError::ConnectTimeout`.
    /// This is the second connect-level variant (alongside `Connect`) that
    /// warrants an auto-start retry.
    #[test]
    fn is_connect_failure_true_for_connect_timeout() {
        let err = sqry_daemon_client::ClientError::ConnectTimeout {
            path: PathBuf::from("/tmp/test.sock"),
            after: Duration::from_secs(5),
        };
        assert!(
            super::is_connect_failure(&err),
            "ConnectTimeout variant must be classified as a connect failure"
        );
    }

    /// `is_connect_failure` must return `false` for
    /// `ClientError::EnvelopeVersionMismatch`. The daemon is running but
    /// speaks a different protocol version -- auto-start cannot help.
    #[test]
    fn is_connect_failure_false_for_envelope_version_mismatch() {
        let err = sqry_daemon_client::ClientError::EnvelopeVersionMismatch {
            got: 99,
            expected: 1,
        };
        assert!(
            !super::is_connect_failure(&err),
            "EnvelopeVersionMismatch must NOT be classified as a connect failure"
        );
    }

    /// `resolve_sqryd_binary` falls through to the sibling-of-current-exe
    /// tier when `SQRYD_PATH` is unset. We create a fake `sqryd` binary
    /// alongside the test binary to exercise this tier.
    #[test]
    fn resolve_sqryd_binary_finds_sibling_of_current_exe() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let original_sqryd_path = std::env::var_os("SQRYD_PATH");
        // SAFETY: serialised by ENV_LOCK; restored before the guard drops.
        unsafe { std::env::remove_var("SQRYD_PATH") };

        // Place a fake `sqryd` next to the test binary.
        let exe = std::env::current_exe().expect("current_exe");
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        let sibling = canonical.parent().expect("exe parent").join("sqryd");

        // Only create the fake binary if it doesn't already exist (avoid
        // interfering with a real sqryd in the target directory).
        let created = if !sibling.exists() {
            std::fs::write(&sibling, b"#!/bin/sh\n").expect("write fake sqryd sibling");
            true
        } else {
            false
        };

        let result = super::resolve_sqryd_binary();

        // Clean up before assertions.
        if created {
            let _ = std::fs::remove_file(&sibling);
        }
        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original_sqryd_path {
                Some(v) => std::env::set_var("SQRYD_PATH", v),
                None => std::env::remove_var("SQRYD_PATH"),
            }
        }

        let resolved = result.expect("sibling tier should find the fake sqryd");
        assert_eq!(
            resolved, sibling,
            "resolved path must be the sibling of current_exe"
        );
    }

    /// `resolve_sqryd_binary` falls through to `which::which("sqryd")` when
    /// both `SQRYD_PATH` and the sibling tier miss. We create a temp dir
    /// with a fake `sqryd` binary and prepend it to `PATH`.
    #[test]
    fn resolve_sqryd_binary_finds_via_path_lookup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let original_sqryd_path = std::env::var_os("SQRYD_PATH");
        let original_path = std::env::var_os("PATH");

        // SAFETY: serialised by ENV_LOCK.
        unsafe { std::env::remove_var("SQRYD_PATH") };

        // Ensure the sibling tier misses: temporarily rename any existing
        // sibling `sqryd` next to current_exe (extremely unlikely in test
        // envs, but be defensive).
        let exe = std::env::current_exe().expect("current_exe");
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        let sibling = canonical.parent().expect("exe parent").join("sqryd");
        let sibling_backup = canonical
            .parent()
            .expect("exe parent")
            .join("sqryd.bak.u11test");
        let sibling_existed = sibling.exists();
        if sibling_existed {
            std::fs::rename(&sibling, &sibling_backup).expect("rename sibling away");
        }

        // Create a fake sqryd in a temp dir and prepend it to PATH.
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_sqryd = dir.path().join("sqryd");
        std::fs::write(&fake_sqryd, b"#!/bin/sh\n").expect("write fake sqryd");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake_sqryd, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake sqryd");
        }

        let new_path = match &original_path {
            Some(p) => {
                let mut s = dir.path().as_os_str().to_owned();
                s.push(":");
                s.push(p);
                s
            }
            None => dir.path().as_os_str().to_owned(),
        };
        // SAFETY: serialised by ENV_LOCK.
        unsafe { std::env::set_var("PATH", &new_path) };

        let result = super::resolve_sqryd_binary();

        // Restore everything.
        if sibling_existed {
            std::fs::rename(&sibling_backup, &sibling).expect("restore sibling");
        }
        // SAFETY: serialised by ENV_LOCK.
        unsafe {
            match original_sqryd_path {
                Some(v) => std::env::set_var("SQRYD_PATH", v),
                None => std::env::remove_var("SQRYD_PATH"),
            }
            match original_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }

        let resolved = result.expect("PATH tier should find the fake sqryd");
        assert_eq!(
            resolved, fake_sqryd,
            "resolved path must be the fake sqryd in the temp PATH dir"
        );
    }

    /// `poll_socket_reachable` succeeds immediately when a real UDS socket
    /// is already listening. This covers the happy-path of the readiness
    /// poll loop.
    #[cfg(unix)]
    #[tokio::test]
    async fn poll_socket_reachable_succeeds_on_live_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("test-live.sock");

        // Bind a real UDS listener so the connect probe succeeds.
        let _listener = tokio::net::UnixListener::bind(&sock_path).expect("bind test UDS listener");

        let result = super::poll_socket_reachable(&sock_path, Duration::from_secs(1)).await;
        assert!(
            result.is_ok(),
            "poll must succeed when a listener is bound; got: {result:?}"
        );
    }

    /// `auto_start_daemon` returns a clear error when `resolve_sqryd_binary`
    /// fails (no binary found). This covers the `auto_start_daemon` entry
    /// path without requiring a real daemon process.
    #[serial_test::serial]
    #[tokio::test]
    async fn auto_start_daemon_fails_when_binary_not_found() {
        // Force resolve_sqryd_binary to fail by pointing SQRYD_PATH to a
        // non-existent path. The sibling/PATH tiers may or may not have a
        // real sqryd, but SQRYD_PATH takes priority and short-circuits.
        let original = {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let orig = std::env::var_os("SQRYD_PATH");
            // SAFETY: serialised by ENV_LOCK.
            unsafe {
                std::env::set_var(
                    "SQRYD_PATH",
                    "/tmp/sqry-lsp-test-nonexistent-sqryd-u11-autostart",
                );
            }
            orig
        };

        let absent_socket = Path::new("/tmp/sqry-lsp-test-autostart-fail.sock");
        let result = super::auto_start_daemon(absent_socket).await;

        // Restore.
        {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: serialised by ENV_LOCK.
            unsafe {
                match original {
                    Some(v) => std::env::set_var("SQRYD_PATH", v),
                    None => std::env::remove_var("SQRYD_PATH"),
                }
            }
        }

        let err = result.expect_err("auto_start_daemon must fail when sqryd binary not found");
        let msg = format!("{err}");
        assert!(
            msg.contains("SQRYD_PATH") || msg.contains("not found"),
            "error must indicate binary resolution failure; got: {msg}"
        );
    }

    /// `spawn_sqryd_start_detach` returns a descriptive error when the
    /// binary path points to a non-existent file.
    #[tokio::test]
    async fn spawn_sqryd_start_detach_fails_for_missing_binary() {
        let missing = Path::new("/tmp/sqry-lsp-test-missing-sqryd-binary-u11");
        let sock = Path::new("/tmp/sqry-lsp-test-spawn-fail.sock");
        let result = super::spawn_sqryd_start_detach(missing, sock).await;
        let err = result.expect_err("spawn must fail for non-existent binary");
        let msg = format!("{err}");
        assert!(
            msg.contains("auto-start"),
            "error must mention auto-start context; got: {msg}"
        );
    }
}
