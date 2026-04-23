//! Phase 8c U12 / Task 12 U1 — daemon shim-mode CLI helpers for `sqry-mcp`.
//!
//! Provides:
//!
//! - [`DaemonParseResult`] — the parse-result enum returned by
//!   [`parse_daemon_args`], used by integration tests.
//! - [`parse_daemon_args`] — extract the daemon-specific arguments from the
//!   raw CLI arg list. Returns `DaemonParseResult` so integration tests can
//!   inspect parse outcomes without depending on the private `CliAction` type
//!   in `main.rs`.
//! - [`run_daemon_shim`] — connect to a running sqryd daemon as an MCP shim
//!   client and byte-pump stdin/stdout until EOF.  Task 12 U1 extends this
//!   with auto-start: when `connect_shim` fails with a transport-level
//!   connect error, the shim resolves the `sqryd` binary, executes
//!   `sqryd start --detach`, polls the socket for readiness, and retries.
//!   Opt-out via `SQRY_DAEMON_NO_AUTO_START=1`.
//! - [`resolve_daemon_socket`] — resolve the socket path from explicit
//!   override → `$SQRYD_SOCKET` env var → platform default, mirroring
//!   `sqry-daemon::DaemonConfig::socket_path()` exactly.
//! - [`is_connect_failure`] — predicate distinguishing transport-level
//!   connect failures (warrant auto-start retry) from protocol failures
//!   (do not retry).
//!
//! # Why a separate module
//!
//! `sqry-mcp` exposes a `lib` crate target (`sqry_mcp`) alongside the
//! `[[bin]]` entry-point in `main.rs`.  Functions declared only in `main.rs`
//! are not accessible from integration tests (which link against the lib
//! target).  By placing testable helpers here and declaring this module in
//! both `lib.rs` and `main.rs`, integration tests can call
//! `sqry_mcp::daemon_shim::resolve_daemon_socket` and
//! `sqry_mcp::daemon_shim::parse_daemon_args` directly.
//!
//! # Mirror-symmetry with Task 11 U1 (sqry-lsp)
//!
//! The auto-start logic here is structurally identical to the implementation
//! in `sqry-lsp/src/lib.rs` (Task 11 U1). Divergences are limited to:
//! - Protocol variant: `ShimProtocol::Mcp` vs `ShimProtocol::Lsp`.
//! - Log prefix: `sqry-mcp` vs `sqry-lsp` (using `tracing::` macros here,
//!   `log::` in sqry-lsp — both ultimately emit to the same tracing layer).
//! - Module placement: extracted here for lib-crate testability (sqry-lsp
//!   keeps the helpers in `lib.rs` because its public API is that file).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};

/// Environment variable that suppresses daemon auto-start in [`run_daemon_shim`].
///
/// When set to `"1"`, the MCP shim will NOT attempt to start sqryd automatically
/// when the daemon socket is unreachable. The original connect error is returned
/// directly. This is intended for CI environments and users who want explicit
/// control over the daemon lifecycle.
///
/// Same variable name as `sqry-lsp` — a single opt-out controls both binaries.
const ENV_NO_AUTO_START: &str = "SQRY_DAEMON_NO_AUTO_START";

/// Exit code returned by `sqryd start --detach` when the daemon is already
/// running. Treated as success — the daemon is available, regardless of who
/// started it.
const SQRYD_ALREADY_RUNNING_EXIT_CODE: i32 = 75;

// ─── Parse result type ────────────────────────────────────────────────────────

/// The result of parsing daemon-specific CLI arguments.
///
/// Exposed from the `daemon_shim` module (and therefore from the `sqry_mcp`
/// lib target) so integration tests can inspect parser outcomes without
/// importing the private `CliAction` enum from `main.rs`.
#[derive(Debug, PartialEq)]
pub enum DaemonParseResult {
    /// `--daemon` was present; `socket` is the value of `--daemon-socket`
    /// if supplied, or `None` otherwise.
    Daemon { socket: Option<PathBuf> },
    /// `--daemon-socket` was present without `--daemon` → parse error.
    MissingDaemon,
    /// `--daemon-socket` was present with no following `PATH` argument.
    MissingSocketPath,
    /// `--daemon` was present along with one or more tokens that the
    /// parser does not recognise. Carries the first unknown token so the
    /// caller can surface it in the error message. Codex iter-0 MINOR-1
    /// fix — silently ignoring trailing args after `--daemon` lets
    /// operator typos change behaviour without warning.
    UnknownInDaemonMode { token: String },
    /// Neither `--daemon` nor `--daemon-socket` was present in `args`.
    NotDaemonMode,
}

/// Scan `args` for `--daemon` / `--daemon-socket` and return the parsed
/// [`DaemonParseResult`].
///
/// This is the testable extraction of the daemon-flag scanning that also
/// appears inside `main.rs::parse_cli_action`.  Both call sites produce
/// identical outcomes for the daemon subset of the arg list; the main.rs
/// variant additionally handles `--help`, `--version`, `--list-tools`, and
/// the unknown-flag path.
///
/// # Argument ordering
///
/// `--daemon` and `--daemon-socket <PATH>` may appear in any order in
/// `args`. This mirrors the order-independence that clap provides for U11.
pub fn parse_daemon_args(args: &[String]) -> DaemonParseResult {
    let tail: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();

    let mut has_daemon = false;
    let mut socket_path: Option<PathBuf> = None;
    // Track the first unknown token in daemon mode so we can surface it
    // verbatim in the error. Recording the FIRST occurrence (rather than
    // the last) matches the operator's left-to-right reading of their
    // own command line (Codex iter-0 MINOR-1 fix).
    let mut unknown_token: Option<String> = None;

    let mut i = 0usize;
    while i < tail.len() {
        match tail[i] {
            "--daemon" => {
                has_daemon = true;
            }
            "--daemon-socket" => {
                // Require the next token to be a non-flag value (i.e. not
                // starting with `--`). Consuming a flag token as a PATH would
                // produce the wrong error class and silently swallow `--daemon`
                // when the user writes `--daemon-socket --daemon` (MAJOR fix,
                // Codex iter-0 review).
                match tail.get(i + 1) {
                    Some(&path_str) if !path_str.starts_with("--") => {
                        socket_path = Some(PathBuf::from(path_str));
                        i += 1;
                    }
                    _ => return DaemonParseResult::MissingSocketPath,
                }
            }
            other => {
                // Any token that isn't `--daemon` / `--daemon-socket` /
                // its PATH value is unknown. Record the FIRST one
                // verbatim and keep scanning so we still detect
                // `--daemon` / `--daemon-socket` semantics for the
                // error class decision below.
                if unknown_token.is_none() {
                    unknown_token = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    // Error precedence (matching the order that surfaces clearest
    // operator intent):
    //   1. --daemon-socket without --daemon → MissingDaemon
    //      (the operator clearly wanted a socket; missing --daemon is
    //      the headline issue, not any trailing junk).
    //   2. --daemon present + unknown trailing token → UnknownInDaemonMode
    //      (operator typo or stale flag — surface it verbatim).
    //   3. --daemon present, all tokens recognised → Daemon { socket }.
    //   4. No daemon-mode tokens at all → NotDaemonMode (caller's
    //      "unknown flag" path takes over with the first tail token).
    if socket_path.is_some() && !has_daemon {
        return DaemonParseResult::MissingDaemon;
    }

    if has_daemon {
        if let Some(token) = unknown_token {
            return DaemonParseResult::UnknownInDaemonMode { token };
        }
        return DaemonParseResult::Daemon {
            socket: socket_path,
        };
    }

    DaemonParseResult::NotDaemonMode
}

// ─── Socket path resolution ───────────────────────────────────────────────────

/// Resolve the UDS / named-pipe path the shim client should connect to.
///
/// Precedence matches §G of the Phase 8c design doc and mirrors the
/// resolution logic in `sqry-lsp`'s `resolve_daemon_socket` (U11) exactly,
/// so both shim clients reach the same default when no override is supplied:
///
/// 1. Explicit `--daemon-socket <PATH>` override (passed as `override_path`).
/// 2. `$SQRYD_SOCKET` environment variable (the shared client-side env var;
///    distinct from the server's `SQRY_DAEMON_SOCKET` which configures where
///    the daemon BINDS).
/// 3. Platform default, mirroring `sqry-daemon::DaemonConfig::socket_path()`:
///    - Unix: `$XDG_RUNTIME_DIR/sqry/sqryd.sock` when set, else
///      `$TMPDIR/sqry-<uid>/sqryd.sock`, else `/tmp/sqry-<uid>/sqryd.sock`.
///      `<uid>` is the real POSIX UID via `libc::getuid()` — this matches the
///      daemon's `user_scoped_dir_name` helper exactly.
///    - Windows: `\\.\pipe\sqry` (matching the daemon's default
///      `pipe_name = "sqry"`).
///
/// `std::env::var_os` swallows `NotUnicode` (treating it as unset) — a
/// malformed environment variable falls through to the platform default rather
/// than propagating as an error, matching daemon-side behaviour.
pub fn resolve_daemon_socket(override_path: Option<&Path>) -> PathBuf {
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
    // `sqry-daemon/src/config.rs`. The `/tmp` branch is authoritative when
    // `TMPDIR` is unset (the common systemd-unit / Docker-container case on
    // Linux).
    // SAFETY: `libc::getuid` is a POSIX call with no preconditions and cannot
    // fail; calling it from a multi-threaded runtime is safe per POSIX.
    // Mirrors the safety argument in `sqry-daemon::config::user_scoped_dir_name`.
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

// ─── Shim runner ─────────────────────────────────────────────────────────────

/// Run as a shim CLIENT against a running sqryd daemon (Phase 8c U12 /
/// Task 12 U1).
///
/// Opens a UDS / named-pipe connection, drives the
/// [`sqry_daemon_client::ShimRegister`] → [`sqry_daemon_client::ShimRegisterAck`]
/// handshake with `ShimProtocol::Mcp`, then byte-pumps this process's
/// stdin/stdout against the resulting stream. The actual MCP server hosting
/// the session lives inside the daemon and is wired up by the daemon's
/// router via `mcp_host::host_mcp_on_streams`.
///
/// ## Auto-start (Task 12 U1)
///
/// When the first `connect_shim` attempt returns a transport-level connect
/// error ([`is_connect_failure`] returns `true`), the shim will automatically:
///
/// 1. Check `SQRY_DAEMON_NO_AUTO_START`. If `"1"`, return the original error
///    immediately — no auto-start attempt is made.
/// 2. Resolve the `sqryd` binary via [`resolve_sqryd_binary`].
/// 3. Exec `sqryd start --detach` via [`spawn_sqryd_start_detach`].
/// 4. Poll the socket for readiness via [`poll_socket_reachable`] (5s budget).
/// 5. Retry `connect_shim` once.
///
/// Protocol-level failures (handshake rejection, version mismatch, etc.) are
/// returned immediately — retrying is pointless if the daemon is alive but
/// incompatible.
///
/// The daemon version advertised in `ShimRegisterAck.daemon_version` is
/// surfaced at `info!` log level so tooling that captures `sqry-mcp`'s
/// stderr gets a diagnostic banner. The final byte-counts returned by
/// [`sqry_daemon_client::pump_stdio`] are likewise logged at shutdown.
///
/// # Errors
///
/// - Any [`sqry_daemon_client::ClientError`] surfacing from
///   [`sqry_daemon_client::connect_shim`] (connect / handshake /
///   envelope-version / rejection).
/// - Auto-start errors: binary not found, process spawn failure, non-zero exit.
/// - Socket not reachable within 5s poll budget.
/// - Any [`sqry_daemon_client::ClientError::Io`] from
///   [`sqry_daemon_client::pump_stdio`] (stdio broke mid-session).
///
/// Each error is wrapped in [`anyhow::Error`] with a contextual prefix so
/// caller-side diagnostics cite which stage failed.
pub async fn run_daemon_shim(socket: Option<PathBuf>) -> Result<()> {
    let socket_path = resolve_daemon_socket(socket.as_deref());
    tracing::info!(
        "sqry-mcp connecting to sqryd daemon at {}",
        socket_path.display()
    );

    let conn = match sqry_daemon_client::connect_shim(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
    )
    .await
    {
        Ok(conn) => conn,
        Err(ref e) if is_connect_failure(e) => {
            // Transport-level failure: daemon not running (or not yet started).
            // Check opt-out before attempting auto-start.
            if std::env::var_os(ENV_NO_AUTO_START).as_deref() == Some(std::ffi::OsStr::new("1")) {
                anyhow::bail!("daemon shim connect failed: {e}");
            }
            tracing::info!(
                "daemon not reachable at {}; attempting auto-start",
                socket_path.display()
            );
            auto_start_daemon(&socket_path).await?;
            // Retry after auto-start succeeds and socket is reachable.
            sqry_daemon_client::connect_shim(
                &socket_path,
                sqry_daemon_client::ShimProtocol::Mcp,
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

    tracing::info!(
        "sqry-mcp shim connected (daemon version {})",
        conn.daemon_version()
    );

    let (bytes_up, bytes_down) = sqry_daemon_client::pump_stdio(conn)
        .await
        .map_err(|e| anyhow::anyhow!("daemon byte-pump failed: {e}"))?;

    tracing::info!("sqry-mcp shim pump complete: {bytes_up} bytes up, {bytes_down} bytes down");
    Ok(())
}

/// Returns `true` when the [`sqry_daemon_client::ClientError`] indicates a
/// transport-level connect failure (i.e. the daemon is not running), as opposed
/// to a protocol-level failure (handshake rejection, version mismatch, etc.).
///
/// Only connect-level failures warrant an auto-start retry. If the daemon is
/// running but rejects the connection or sends an incompatible version, retrying
/// is pointless — the same response will recur until the operator takes action.
///
/// This function is `pub(crate)` to allow the unit tests below to construct
/// synthetic errors for coverage.
pub(crate) fn is_connect_failure(err: &sqry_daemon_client::ClientError) -> bool {
    matches!(
        err,
        sqry_daemon_client::ClientError::Connect { .. }
            | sqry_daemon_client::ClientError::ConnectTimeout { .. }
    )
}

/// Resolve the path to the `sqryd` binary.
///
/// Resolution order (first match wins):
/// 1. `$SQRYD_PATH` environment variable — if set, the path must exist.
/// 2. Sibling of `std::env::current_exe()` (canonicalized, symlink-safe).
///    On Windows also tries the `.exe`-suffixed variant.
/// 3. `PATH` lookup via [`which::which`].
///
/// Note: there is no `--sqryd-path` CLI flag on the MCP binary. MCP servers
/// are spawned by AI tooling (Claude Code, Cursor, etc.) without a mechanism
/// to pass such a flag ergonomically. The `SQRYD_PATH` environment variable
/// is the correct configuration surface for that context.
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
         Install sqryd alongside sqry-mcp, set SQRYD_PATH to its path, \
         or start sqryd manually before launching the MCP server."
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
/// `socket_path` (via `--daemon-socket` or `$SQRYD_SOCKET`), the spawned
/// `sqryd` must bind on that same path. We propagate it via
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
    tracing::info!("auto-starting sqryd from {}", binary.display());

    // Forward the resolved socket path to the spawned daemon using the OsStr
    // API so non-UTF-8 path components are preserved losslessly.
    spawn_sqryd_start_detach(&binary, socket_path).await?;

    poll_socket_reachable(socket_path, Duration::from_secs(5)).await?;
    tracing::info!("sqryd auto-started successfully");
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
        // spawn_blocking: `sqryd start --detach` performs a double-fork on
        // Unix — the intermediate parent exits quickly once the grandchild is
        // running, so `.status()` is bounded and blocking is safe. Using the
        // blocking pool avoids holding a tokio task thread while waiting for
        // the fork-and-exit sequence to complete.
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
            tracing::info!("sqryd is already running (exit code 75)");
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
        // instance is available. Pass `socket_path.as_os_str()` directly —
        // `ClientOptions::open` accepts `impl AsRef<OsStr>`, so no lossy
        // UTF-8 conversion is needed and non-UTF-8 pipe paths are preserved.
        use tokio::net::windows::named_pipe::ClientOptions;
        ClientOptions::new().open(socket_path.as_os_str()).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = socket_path;
        false
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- parse_daemon_args ---

    /// `--daemon` alone → `Daemon { socket: None }`.
    #[test]
    fn parse_daemon_alone() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon"]));
        assert_eq!(r, DaemonParseResult::Daemon { socket: None });
    }

    /// `--daemon --daemon-socket <PATH>` → `Daemon { socket: Some(..) }`.
    #[test]
    fn parse_daemon_with_socket() {
        let r = parse_daemon_args(&args(&[
            "sqry-mcp",
            "--daemon",
            "--daemon-socket",
            "/custom.sock",
        ]));
        assert_eq!(
            r,
            DaemonParseResult::Daemon {
                socket: Some(PathBuf::from("/custom.sock"))
            }
        );
    }

    /// `--daemon-socket <PATH> --daemon` (socket before daemon) → same result.
    #[test]
    fn parse_daemon_flags_order_independent() {
        let r = parse_daemon_args(&args(&[
            "sqry-mcp",
            "--daemon-socket",
            "/reorder.sock",
            "--daemon",
        ]));
        assert_eq!(
            r,
            DaemonParseResult::Daemon {
                socket: Some(PathBuf::from("/reorder.sock"))
            }
        );
    }

    /// `--daemon-socket <PATH>` without `--daemon` → `MissingDaemon`.
    #[test]
    fn parse_daemon_socket_without_daemon() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon-socket", "/some/path"]));
        assert_eq!(r, DaemonParseResult::MissingDaemon);
    }

    /// `--daemon-socket` with no following PATH → `MissingSocketPath`.
    #[test]
    fn parse_daemon_socket_with_no_path() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon", "--daemon-socket"]));
        assert_eq!(r, DaemonParseResult::MissingSocketPath);
    }

    /// `--daemon-socket --daemon` — next token is a flag, not a PATH.
    ///
    /// Regression test for Codex iter-0 MAJOR finding: the parser must reject
    /// flag tokens as PATH values and return `MissingSocketPath`, not
    /// `MissingDaemon` (old behaviour: consumed `--daemon` as socket path,
    /// then reported `MissingDaemon` because `has_daemon` was still false).
    #[test]
    fn parse_daemon_socket_with_flag_token_as_path_is_missing_path() {
        // next token is a recognised flag → MissingSocketPath
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon-socket", "--daemon"]));
        assert_eq!(
            r,
            DaemonParseResult::MissingSocketPath,
            "--daemon-socket --daemon must be MissingSocketPath (not MissingDaemon)"
        );
        // next token looks like any other flag → MissingSocketPath
        let r2 = parse_daemon_args(&args(&[
            "sqry-mcp",
            "--daemon",
            "--daemon-socket",
            "--help",
        ]));
        assert_eq!(
            r2,
            DaemonParseResult::MissingSocketPath,
            "--daemon --daemon-socket --help must be MissingSocketPath"
        );
    }

    /// No daemon flags → `NotDaemonMode`.
    #[test]
    fn parse_no_daemon_flags() {
        let r = parse_daemon_args(&args(&["sqry-mcp"]));
        assert_eq!(r, DaemonParseResult::NotDaemonMode);
        let r2 = parse_daemon_args(&args(&["sqry-mcp", "--help"]));
        assert_eq!(r2, DaemonParseResult::NotDaemonMode);
    }

    // --- UnknownInDaemonMode (Codex iter-0 MINOR-1 fix) ---

    /// `--daemon --bogus` (operator typo) MUST surface the unknown
    /// token rather than silently entering daemon mode. The previous
    /// parser swallowed any non-recognised tokens once `--daemon` was
    /// present — Codex's iter-0 review verified `--daemon --bogus`
    /// attempted a daemon connection rather than failing parse, which
    /// diverges from sqry-lsp's stricter clap path.
    #[test]
    fn parse_daemon_with_unknown_trailing_flag_is_rejected() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon", "--bogus"]));
        assert_eq!(
            r,
            DaemonParseResult::UnknownInDaemonMode {
                token: "--bogus".to_string()
            }
        );
    }

    /// Unknown token before `--daemon` is also rejected. Order
    /// independence: the parser scans the whole arg list before
    /// reaching its verdict, so `--bogus --daemon` and
    /// `--daemon --bogus` both surface `UnknownInDaemonMode`.
    #[test]
    fn parse_unknown_before_daemon_flag_is_rejected() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--bogus", "--daemon"]));
        assert_eq!(
            r,
            DaemonParseResult::UnknownInDaemonMode {
                token: "--bogus".to_string()
            }
        );
    }

    /// Multiple unknown tokens — the FIRST one is reported (verbatim)
    /// because that matches the operator's left-to-right reading of
    /// their own command line.
    #[test]
    fn parse_multiple_unknowns_reports_first() {
        let r = parse_daemon_args(&args(&[
            "sqry-mcp",
            "--daemon",
            "--first-bad",
            "--second-bad",
        ]));
        assert_eq!(
            r,
            DaemonParseResult::UnknownInDaemonMode {
                token: "--first-bad".to_string()
            }
        );
    }

    /// Bare positional unknowns (not `--`-prefixed) are also rejected.
    /// A positional argument after `--daemon` is meaningless and must
    /// not silently drop into daemon mode.
    #[test]
    fn parse_daemon_with_positional_unknown_is_rejected() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon", "extra-positional"]));
        assert_eq!(
            r,
            DaemonParseResult::UnknownInDaemonMode {
                token: "extra-positional".to_string()
            }
        );
    }

    /// Unknown trailing flag in NON-daemon-mode invocation must NOT
    /// trigger UnknownInDaemonMode. The caller's "first unknown flag"
    /// path takes over with `NotDaemonMode`.
    #[test]
    fn parse_unknown_without_daemon_falls_through_to_not_daemon_mode() {
        let r = parse_daemon_args(&args(&["sqry-mcp", "--bogus"]));
        assert_eq!(r, DaemonParseResult::NotDaemonMode);
    }

    // --- resolve_daemon_socket ---

    /// Explicit override always wins.
    #[test]
    fn resolve_explicit_override_wins() {
        let path = Path::new("/explicit/sqryd.sock");
        let result = resolve_daemon_socket(Some(path));
        assert_eq!(result, PathBuf::from("/explicit/sqryd.sock"));
    }

    /// `$SQRYD_SOCKET` wins over platform default.
    ///
    /// # Safety note
    ///
    /// `std::env::set_var` / `remove_var` are unsafe in Edition 2024 because
    /// env mutation is inherently thread-unsafe in multi-threaded programs.
    /// `#[serial]` ensures this test is not run concurrently with other
    /// env-mutating tests, preventing flaky failures under `cargo test`'s
    /// default parallelism (MINOR fix, Codex iter-0 review).
    #[serial]
    #[test]
    fn resolve_env_var_wins_over_platform_default() {
        struct EnvGuard {
            key: &'static str,
            prior: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: single-threaded test; env mutation is intentional.
                unsafe {
                    match &self.prior {
                        Some(v) => std::env::set_var(self.key, v),
                        None => std::env::remove_var(self.key),
                    }
                }
            }
        }

        let prior = std::env::var_os("SQRYD_SOCKET");
        let _guard = EnvGuard {
            key: "SQRYD_SOCKET",
            prior,
        };
        // SAFETY: single-threaded test; restoring on drop via EnvGuard.
        unsafe { std::env::set_var("SQRYD_SOCKET", "/env/sqryd.sock") };

        let result = resolve_daemon_socket(None);
        assert_eq!(
            result,
            PathBuf::from("/env/sqryd.sock"),
            "SQRYD_SOCKET env var should take precedence over platform default"
        );
    }

    /// Explicit override wins over `$SQRYD_SOCKET`.
    #[serial]
    #[test]
    fn resolve_explicit_wins_over_env_var() {
        struct EnvGuard {
            key: &'static str,
            prior: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: single-threaded test; env mutation is intentional.
                unsafe {
                    match &self.prior {
                        Some(v) => std::env::set_var(self.key, v),
                        None => std::env::remove_var(self.key),
                    }
                }
            }
        }

        let prior = std::env::var_os("SQRYD_SOCKET");
        let _guard = EnvGuard {
            key: "SQRYD_SOCKET",
            prior,
        };
        // SAFETY: single-threaded test; restoring on drop via EnvGuard.
        unsafe { std::env::set_var("SQRYD_SOCKET", "/env/should-be-ignored.sock") };

        let explicit = Path::new("/explicit/wins.sock");
        let result = resolve_daemon_socket(Some(explicit));
        assert_eq!(
            result,
            PathBuf::from("/explicit/wins.sock"),
            "explicit path must win over SQRYD_SOCKET env var"
        );
    }

    /// On Unix, platform default ends with `sqryd.sock` and contains a
    /// `sqry` path component.
    #[cfg(unix)]
    #[serial]
    #[test]
    fn resolve_unix_platform_default_structure() {
        struct EnvGuard {
            key: &'static str,
            prior: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: single-threaded test; env mutation is intentional.
                unsafe {
                    match &self.prior {
                        Some(v) => std::env::set_var(self.key, v),
                        None => std::env::remove_var(self.key),
                    }
                }
            }
        }

        let prior_socket = std::env::var_os("SQRYD_SOCKET");
        let prior_xdg = std::env::var_os("XDG_RUNTIME_DIR");
        let _g1 = EnvGuard {
            key: "SQRYD_SOCKET",
            prior: prior_socket,
        };
        let _g2 = EnvGuard {
            key: "XDG_RUNTIME_DIR",
            prior: prior_xdg,
        };
        // SAFETY: single-threaded test; restoring on drop via EnvGuards.
        unsafe {
            std::env::remove_var("SQRYD_SOCKET");
            std::env::remove_var("XDG_RUNTIME_DIR");
        }

        let result = resolve_daemon_socket(None);
        let s = result.to_string_lossy();
        assert!(
            s.contains("sqry"),
            "unix platform default should contain 'sqry'; got: {s}"
        );
        assert!(
            s.ends_with("sqryd.sock"),
            "unix platform default should end with 'sqryd.sock'; got: {s}"
        );
    }

    // ─── Task 12 U1: auto-start helper tests ─────────────────────────────────

    /// Helper: inline `EnvGuard` restores one env var on drop.
    /// Defined here rather than at module level to keep it test-scoped.
    struct EnvGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: single-threaded (serial) test; intentional env mutation.
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    // --- is_connect_failure ---

    /// `ClientError::Connect` is a connect-level failure → `true`.
    #[test]
    fn is_connect_failure_true_for_connect_error() {
        let err = sqry_daemon_client::ClientError::Connect {
            path: PathBuf::from("/tmp/sqryd.sock"),
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
        };
        assert!(is_connect_failure(&err));
    }

    /// `ClientError::ConnectTimeout` is a connect-level failure → `true`.
    #[test]
    fn is_connect_failure_true_for_connect_timeout() {
        let err = sqry_daemon_client::ClientError::ConnectTimeout {
            path: PathBuf::from("/tmp/sqryd.sock"),
            after: Duration::from_millis(100),
        };
        assert!(is_connect_failure(&err));
    }

    /// `ClientError::HelloRejected` is a protocol-level failure → `false`.
    #[test]
    fn is_connect_failure_false_for_handshake_error() {
        let err = sqry_daemon_client::ClientError::HelloRejected;
        assert!(!is_connect_failure(&err));
    }

    /// `ClientError::ShimRejected` is a protocol-level failure → `false`.
    #[test]
    fn is_connect_failure_false_for_shim_rejected() {
        let err = sqry_daemon_client::ClientError::ShimRejected("cap limit".to_string());
        assert!(!is_connect_failure(&err));
    }

    /// `ClientError::EnvelopeVersionMismatch` is a protocol-level failure → `false`.
    #[test]
    fn is_connect_failure_false_for_envelope_version_mismatch() {
        let err = sqry_daemon_client::ClientError::EnvelopeVersionMismatch {
            got: 0,
            expected: 1,
        };
        assert!(!is_connect_failure(&err));
    }

    // --- resolve_sqryd_binary ---

    /// When `SQRYD_PATH` points to a file that exists, tier-1 wins.
    #[serial]
    #[test]
    fn resolve_sqryd_binary_finds_env_var() {
        // Create a real file the resolution code can validate with `.exists()`.
        let tmp = tempfile::NamedTempFile::new().expect("temp file");

        let prior = std::env::var_os("SQRYD_PATH");
        let _guard = EnvGuard {
            key: "SQRYD_PATH",
            prior,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("SQRYD_PATH", tmp.path()) };

        let result = resolve_sqryd_binary();
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(result.unwrap(), tmp.path());
    }

    /// When `SQRYD_PATH` points to a deterministic non-existent path,
    /// `resolve_sqryd_binary` returns an error (rather than silently
    /// falling through to sibling / PATH tiers — SQRYD_PATH is authoritative
    /// if set).
    #[serial]
    #[test]
    fn resolve_sqryd_binary_returns_error_when_not_found() {
        // Use a path that is guaranteed non-existent and will never be created
        // by the test environment. The double-UUID prevents collisions if tests
        // run in parallel against the same filesystem, though `#[serial]` makes
        // that extremely unlikely.
        let absent = PathBuf::from(
            "/tmp/sqryd_test_absent_aaaaabbbbccccddddeeeef0001/definitely-does-not-exist",
        );
        assert!(!absent.exists(), "test precondition: path must not exist");

        let prior = std::env::var_os("SQRYD_PATH");
        let _guard = EnvGuard {
            key: "SQRYD_PATH",
            prior,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("SQRYD_PATH", &absent) };

        let result = resolve_sqryd_binary();
        assert!(result.is_err(), "expected Err for absent SQRYD_PATH");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("does not exist"),
            "error should cite 'does not exist'; got: {msg}"
        );
    }

    /// When `SQRYD_PATH` is cleared and there is no `sqryd` sibling next to
    /// `current_exe()`, resolution should find a mock `sqryd` that we place
    /// in the same directory as the test runner executable.
    ///
    /// This test verifies tier-2 (sibling-of-current-exe). It creates an
    /// empty file named `sqryd` next to `std::env::current_exe()` so the
    /// existence check passes, then clears `SQRYD_PATH` to suppress tier-1.
    ///
    /// If a real `sqryd` already exists next to `current_exe()` the test
    /// would pass trivially; if it does not exist the test creates it and
    /// cleans up on exit.
    #[cfg(unix)]
    #[serial]
    #[test]
    fn resolve_sqryd_binary_finds_sibling_of_current_exe() {
        // Locate the sibling path.
        let exe = std::env::current_exe().expect("current_exe");
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        let dir = canonical.parent().expect("exe has parent");
        let sibling = dir.join("sqryd");

        // Track whether we need to clean up the file we create.
        let created = !sibling.exists();
        if created {
            std::fs::write(&sibling, b"").expect("create mock sqryd sibling");
        }

        // Suppress SQRYD_PATH (tier-1) so the sibling path is exercised.
        let prior_path = std::env::var_os("SQRYD_PATH");
        let _guard_path = EnvGuard {
            key: "SQRYD_PATH",
            prior: prior_path,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::remove_var("SQRYD_PATH") };

        let result = resolve_sqryd_binary();

        // Clean up before asserting (so cleanup is guaranteed even on panic).
        if created {
            let _ = std::fs::remove_file(&sibling);
        }

        assert!(
            result.is_ok(),
            "expected Ok for sibling resolution; got: {result:?}"
        );
        assert_eq!(result.unwrap(), sibling);
    }

    /// When both `SQRYD_PATH` and the sibling tier are suppressed, tier-3
    /// (PATH lookup via `which`) should find a mock `sqryd` on PATH.
    ///
    /// Strategy: create a temp directory, write an executable `sqryd` file
    /// into it, prepend it to `PATH`, then call `resolve_sqryd_binary`.
    #[cfg(unix)]
    #[serial]
    #[test]
    fn resolve_sqryd_binary_finds_via_path_lookup() {
        use std::os::unix::fs::PermissionsExt as _;

        // Build a temp dir with an executable `sqryd`.
        let tmp_dir = tempfile::TempDir::new().expect("temp dir");
        let mock_sqryd = tmp_dir.path().join("sqryd");
        std::fs::write(&mock_sqryd, b"#!/bin/sh\n").expect("write mock sqryd");
        std::fs::set_permissions(&mock_sqryd, std::fs::Permissions::from_mode(0o755))
            .expect("chmod mock sqryd");

        // Suppress SQRYD_PATH (tier-1) and any sibling next to current_exe
        // (tier-2) by pointing SQRYD_PATH at a non-existent path — but
        // `SQRYD_PATH` is authoritative-if-set and would return an error.
        // Instead, clear SQRYD_PATH entirely so tier-2 is attempted; but
        // tier-2 checks the actual sibling file existence. To prevent a
        // real `sqryd` sibling from short-circuiting the test, we verify
        // that the result is the temp-dir mock (which must be found before
        // any real sibling via PATH prefix ordering).
        //
        // Simpler approach: set SQRYD_PATH to the absent path so tier-1 returns
        // an error before tier-2/3 execute. But the spec says SQRYD_PATH set +
        // absent = early error, not fallthrough. So we rely on the fact that
        // there is no `sqryd` sibling next to the test executable when running
        // in the CI/dev environment. If there IS one, tier-2 wins, and the
        // found binary will differ from `mock_sqryd`. Guard against this:
        let exe = std::env::current_exe().expect("current_exe");
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        let sibling = canonical.parent().map(|d| d.join("sqryd"));
        let sibling_exists = sibling.as_ref().is_some_and(|p| p.exists());

        // Suppress SQRYD_PATH.
        let prior_sqryd_path = std::env::var_os("SQRYD_PATH");
        let _guard_sqryd = EnvGuard {
            key: "SQRYD_PATH",
            prior: prior_sqryd_path,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::remove_var("SQRYD_PATH") };

        // Prepend tmp_dir to PATH.
        let prior_path_env = std::env::var_os("PATH");
        let new_path = if let Some(ref p) = prior_path_env {
            let mut s = std::ffi::OsString::from(tmp_dir.path());
            s.push(":");
            s.push(p);
            s
        } else {
            std::ffi::OsString::from(tmp_dir.path())
        };
        let _guard_path = EnvGuard {
            key: "PATH",
            prior: prior_path_env,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("PATH", &new_path) };

        let result = resolve_sqryd_binary();

        if sibling_exists {
            // Tier-2 wins: just verify resolution succeeded without error.
            assert!(
                result.is_ok(),
                "sibling exists, expected Ok; got: {result:?}"
            );
        } else {
            // Tier-3 (PATH) wins: verify the temp-dir mock was found.
            assert!(
                result.is_ok(),
                "expected Ok from PATH lookup; got: {result:?}"
            );
            assert_eq!(
                result.unwrap(),
                mock_sqryd,
                "PATH lookup should find the temp-dir mock sqryd"
            );
        }
    }

    // --- poll_socket_reachable ---

    /// When the socket path does not exist and the budget is zero/near-zero,
    /// `poll_socket_reachable` returns an error with "not reachable" in the
    /// message.
    #[tokio::test]
    async fn poll_socket_reachable_times_out() {
        let absent = Path::new("/tmp/sqryd_poll_test_no_such_socket_zzz999.sock");
        // Use a tiny budget (10ms) so the test completes quickly.
        let result = poll_socket_reachable(absent, Duration::from_millis(10)).await;
        assert!(result.is_err(), "expected timeout error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("not reachable"),
            "error should contain 'not reachable'; got: {msg}"
        );
    }

    /// When a real UDS socket is listening, `poll_socket_reachable` returns
    /// immediately without waiting out the budget.
    #[cfg(unix)]
    #[tokio::test]
    async fn poll_socket_reachable_succeeds_on_live_socket() {
        use tokio::net::UnixListener;

        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let sock_path = tmp.path().to_path_buf();
        // NamedTempFile creates a plain file; we need a socket — remove it and
        // bind a UnixListener on that path.
        drop(tmp);
        let _listener = UnixListener::bind(&sock_path).expect("bind unix listener");

        let result = poll_socket_reachable(&sock_path, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok on live socket; got: {result:?}"
        );
    }

    // --- auto_start_daemon ---

    /// When `SQRYD_PATH` points to a non-existent file, `auto_start_daemon`
    /// returns an error from binary resolution (before attempting any spawn).
    #[serial]
    #[tokio::test]
    async fn auto_start_daemon_fails_when_binary_not_found() {
        let absent = PathBuf::from("/tmp/sqryd_autostart_test_absent_binary_xyz999/sqryd-no-exist");
        let prior = std::env::var_os("SQRYD_PATH");
        let _guard = EnvGuard {
            key: "SQRYD_PATH",
            prior,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("SQRYD_PATH", &absent) };

        let result = auto_start_daemon(Path::new("/tmp/irrelevant.sock")).await;
        assert!(result.is_err(), "expected Err when binary not found");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("does not exist"),
            "error should cite 'does not exist'; got: {msg}"
        );
    }

    // --- run_daemon_shim opt-out ---

    /// When `SQRY_DAEMON_NO_AUTO_START=1` is set, `run_daemon_shim` returns
    /// the original connect error without attempting auto-start.
    ///
    /// Strategy: use an absent socket path so `connect_shim` will return a
    /// `ClientError::Connect` failure immediately, then verify that the error
    /// message says "daemon shim connect failed" (NOT "auto-start").
    #[serial]
    #[tokio::test]
    async fn auto_start_opt_out_via_env() {
        // Absent socket → connect will fail immediately.
        let absent_socket = PathBuf::from("/tmp/sqryd_optout_test_no_sock_qqq888/sqryd.sock");

        let prior_no_start = std::env::var_os("SQRY_DAEMON_NO_AUTO_START");
        let _guard = EnvGuard {
            key: "SQRY_DAEMON_NO_AUTO_START",
            prior: prior_no_start,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("SQRY_DAEMON_NO_AUTO_START", "1") };

        let result = run_daemon_shim(Some(absent_socket)).await;
        assert!(result.is_err(), "expected connect error");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("daemon shim connect failed"),
            "error should be the original connect error (not auto-start); got: {msg}"
        );
        assert!(
            !msg.contains("auto-start"),
            "opt-out must suppress auto-start; got: {msg}"
        );
    }

    // --- spawn_sqryd_start_detach ---

    /// When given a non-existent binary path, `spawn_sqryd_start_detach`
    /// returns an error with context about the failed exec.
    #[tokio::test]
    async fn spawn_sqryd_start_detach_fails_for_missing_binary() {
        let missing_binary = Path::new("/tmp/sqryd_spawn_test_no_binary_mmm777/sqryd-missing");
        let socket = Path::new("/tmp/irrelevant_sock_for_spawn_test.sock");
        let result = spawn_sqryd_start_detach(missing_binary, socket).await;
        assert!(result.is_err(), "expected Err for missing binary");
        let msg = format!("{:?}", result.unwrap_err());
        // The error chain should include something about exec failure.
        assert!(
            msg.contains("exec") || msg.contains("spawn") || msg.contains("failed"),
            "error should describe exec/spawn failure; got: {msg}"
        );
    }

    // --- run_daemon_shim wiring: auto-start orchestration ----------------------

    /// When `connect_shim` returns a CONNECT-level error and
    /// `SQRY_DAEMON_NO_AUTO_START` is NOT set, `run_daemon_shim` MUST attempt
    /// auto-start. This test verifies the wiring by pointing the shim at an
    /// absent socket without `SQRY_DAEMON_NO_AUTO_START` set — the auto-start
    /// path executes and FAILS at binary resolution (no `SQRYD_PATH`, no
    /// sibling, no `sqryd` on PATH in the test environment) or at the poll
    /// budget. Either way the error chain MUST NOT contain "auto-start
    /// suppressed", and the error must show an auto-start attempt was made
    /// (error comes from auto_start_daemon, not the raw connect error).
    ///
    /// This proves the `Connect -> auto_start_daemon -> (fail)` wiring path is
    /// reachable, i.e. a regression in the `is_connect_failure` match or the
    /// `ENV_NO_AUTO_START` guard would change the error message and break this
    /// test.
    #[cfg(unix)]
    #[serial]
    #[tokio::test]
    async fn run_daemon_shim_attempts_auto_start_on_connect_failure() {
        // Absent socket → connect_shim produces ClientError::Connect.
        let absent_socket =
            PathBuf::from("/tmp/sqryd_shim_autostart_wiring_test_zzz444/sqryd.sock");

        // Ensure SQRY_DAEMON_NO_AUTO_START is absent.
        let prior_no_start = std::env::var_os("SQRY_DAEMON_NO_AUTO_START");
        let _guard_no_start = EnvGuard {
            key: "SQRY_DAEMON_NO_AUTO_START",
            prior: prior_no_start,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::remove_var("SQRY_DAEMON_NO_AUTO_START") };

        // Point SQRYD_PATH at a non-existent binary so auto_start_daemon fails
        // at binary resolution with a clear, deterministic error — this avoids
        // accidentally invoking a real `sqryd` on the PATH.
        let absent_binary =
            PathBuf::from("/tmp/sqryd_shim_autostart_wiring_test_zzz444/sqryd-no-exist");
        let prior_sqryd_path = std::env::var_os("SQRYD_PATH");
        let _guard_sqryd = EnvGuard {
            key: "SQRYD_PATH",
            prior: prior_sqryd_path,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::set_var("SQRYD_PATH", &absent_binary) };

        let result = run_daemon_shim(Some(absent_socket)).await;
        assert!(result.is_err(), "expected error from auto-start failure");
        let msg = format!("{}", result.unwrap_err());
        // The error must come from the auto-start path (binary resolution),
        // NOT from the raw connect error returned before any auto-start attempt.
        // The original connect error says "daemon shim connect failed" and is
        // NOT followed by auto-start text. The binary-not-found error from
        // auto_start_daemon says "does not exist".
        assert!(
            msg.contains("does not exist"),
            "error should be from binary resolution inside auto_start_daemon; got: {msg}"
        );
        assert!(
            !msg.contains("auto-start suppressed"),
            "auto-start must not have been suppressed; got: {msg}"
        );
    }

    /// When `connect_shim` returns a PROTOCOL-level error (not a connect-level
    /// failure), `run_daemon_shim` MUST return immediately WITHOUT attempting
    /// auto-start.
    ///
    /// Strategy: bind a real Unix socket that accepts connections but
    /// immediately closes them without sending any bytes. `connect_shim` will
    /// see an EOF during the `ShimRegisterAck` read, which maps to
    /// `ClientError::ShimAckEof` — a non-connect error that must NOT trigger
    /// auto-start. The error message must be "daemon shim connect failed" and
    /// must NOT contain "auto-start".
    ///
    /// Marked `#[serial]` and guards `SQRY_DAEMON_NO_AUTO_START` explicitly so
    /// the test is not affected by concurrent env mutations from other serial
    /// tests (Codex iter-1 MINOR fix).
    #[cfg(unix)]
    #[serial]
    #[tokio::test]
    async fn run_daemon_shim_does_not_auto_start_on_protocol_failure() {
        use tokio::net::UnixListener;

        // Ensure SQRY_DAEMON_NO_AUTO_START is absent for this test.
        // We deliberately want auto-start NOT suppressed by the opt-out flag —
        // the protocol failure itself must suppress it via is_connect_failure.
        let prior_no_start = std::env::var_os("SQRY_DAEMON_NO_AUTO_START");
        let _guard = EnvGuard {
            key: "SQRY_DAEMON_NO_AUTO_START",
            prior: prior_no_start,
        };
        // SAFETY: serial test; env restored on drop.
        unsafe { std::env::remove_var("SQRY_DAEMON_NO_AUTO_START") };

        // Create a temp socket path.
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let sock_path = tmp.path().to_path_buf();
        drop(tmp); // remove the regular file so UnixListener can bind

        let listener = UnixListener::bind(&sock_path).expect("bind listener");

        // Spawn an acceptor that accepts one connection then immediately drops
        // it (simulates a daemon that closes mid-handshake).
        tokio::spawn(async move {
            if let Ok((_stream, _)) = listener.accept().await {
                // Immediately drop `_stream` — this sends EOF to the client.
            }
        });

        // The protocol failure (ShimAckEof) must cause run_daemon_shim to bail
        // immediately, without entering auto_start_daemon.
        let result = run_daemon_shim(Some(sock_path)).await;
        assert!(result.is_err(), "expected protocol error");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("daemon shim connect failed"),
            "expected raw protocol error; got: {msg}"
        );
        assert!(
            !msg.contains("auto-start"),
            "protocol failure must NOT trigger auto-start; got: {msg}"
        );
    }
}
