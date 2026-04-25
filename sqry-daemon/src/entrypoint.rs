//! Production `sqryd` binary entry point — Task 9 U10.
//!
//! # Overview
//!
//! This module owns the complete CLI definition and the ordered startup /
//! shutdown lifecycle for the `sqryd` daemon binary.  Every code path that
//! could surface an error to the operator maps to a POSIX `sysexits.h` exit
//! code via [`DaemonError::exit_code`].
//!
//! # CLI surface (§C.2)
//!
//! ```text
//! sqryd [OPTIONS] [COMMAND]
//!
//! Commands:
//!   start                   Start the daemon (foreground by default; --detach for detached)
//!   foreground              Run in the foreground (alias for `start`)
//!   stop                    Send daemon/stop and wait for socket to become unreachable
//!   status                  Print daemon status (--json for machine-readable output)
//!   install-systemd-user    Emit a systemd user-service unit to stdout   [Linux]
//!   install-systemd-system  Emit a systemd system-service unit to stdout  [Linux]
//!   install-launchd         Emit a launchd user-agent plist to stdout    [macOS]
//!   install-windows         Emit sc.exe + Task Scheduler XML to stdout   [Windows]
//!   print-config            Print the effective daemon configuration as TOML
//! ```
//!
//! Default (no command given): `start` with `detach=false`.
//!
//! # Startup ordering (§C.3.1)
//!
//! The foreground path follows these 17 ordered steps, each protected by
//! RAII so every Drop runs on the success *and* error paths:
//!
//! 1.  Load [`DaemonConfig`] (honour `--config` / `SQRY_DAEMON_CONFIG`).
//! 2.  Install tracing subscriber (gate `RollingSizeAppender` on
//!     `NOTIFY_SOCKET` absence — §G.1 m4).
//! 3.  Create `runtime_dir()` with mode `0700` on Unix.
//! 4.  [`acquire_pidfile_lock`] → [`PidfileLock`].  `WouldBlock` →
//!     [`DaemonError::AlreadyRunning`] → exit 75.
//! 5.  *(Skip — detach path handled in `run_start_detach`.)*
//! 6.  Build plugin manager.
//! 7.  [`WorkspaceManager::new`] (spawns the retention reaper).
//! 8.  [`RebuildDispatcher::new`].
//! 9.  [`RealWorkspaceBuilder::new`].
//! 10. [`QueryExecutor::new`].
//! 11. [`CancellationToken`].
//! 12. Install signal handlers → [`SignalGuard`].
//! 13. Pre-load pinned workspaces (log + continue on failure).
//! 14. [`IpcServer::bind`].
//! 15. Signal ready (§C.3.1 step 15 authoritative matrix):
//!     - `NOTIFY_SOCKET` set → `sd_notify(READY=1)` (authoritative for systemd).
//!     - `--spawned-by-client` → close `SQRYD_READY_PIPE_FD` (authoritative
//!       for the parent auto-spawn path).
//!     - Always: touch `runtime_dir/sqryd.ready` (diagnostic, non-authoritative).
//! 16. `server.run().await`.
//! 17. RAII Drop order: `IpcServer` drops (stops accepting; socket file
//!     remains on disk in configured-path mode), pidfile removed + lock
//!     released by `PidfileLock::Drop`.
//!
//! # Detach path (§C.3.2)
//!
//! On `start --detach` (Unix only):
//!
//! A. Parent acquires `PidfileLock` (`WriteOwner`).
//! B. Parent creates a self-pipe via `pipe2(O_CLOEXEC)`.
//! C. Parent spawns `current_exe()` with `["start", "--detach",
//!    "--spawned-by-client"]` and environment `SQRYD_READY_PIPE_FD`,
//!    `SQRYD_LOCK_FD`, `SQRYD_PIDFILE_PATH`.  A `pre_exec` hook clears
//!    `FD_CLOEXEC` on both FDs and calls `setsid()`.
//! D. Parent closes its write end.
//! E. Parent polls read end up to `auto_start_ready_timeout_secs`:
//!    - EOF -> `hand_off_to_adopter()` + exit 0.
//!    - Timeout -> `child.kill()` (SIGKILL to specific PID via
//!      `std::process::Child::kill`) + exit 69.
//! F. Grandchild reads `SQRYD_LOCK_FD`, wraps via
//!    [`PidfileLock::adopt`], reads `SQRYD_READY_PIPE_FD`, runs steps
//!    2-14, closes pipe at step 15, runs `server.run()`.
//!
//! On Windows, `--detach` is a no-op with a `WARN` log (see §C.5).
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §C, §D, §E, §G, §I, §J.

use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use sqry_core::query::executor::QueryExecutor;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    DaemonConfig, DaemonError, DaemonResult, IpcServer, RealWorkspaceBuilder, RebuildDispatcher,
    WorkspaceManager,
    lifecycle::{
        log_rotate::install_tracing,
        notify::{is_under_systemd, notify_ready},
        pidfile::{PidfileLock, acquire_pidfile_lock},
        signals::install_signal_handlers,
        units::InstallOptions,
    },
};

// ---------------------------------------------------------------------------
// Environment variable names for FD-inheritance protocol (§C.3.2)
// ---------------------------------------------------------------------------

/// Environment variable carrying the write-end FD of the parent->grandchild
/// self-pipe.  The grandchild closes this FD after signalling ready; the
/// parent's read end returns EOF, proving readiness.
const ENV_READY_PIPE_FD: &str = "SQRYD_READY_PIPE_FD";

/// Environment variable carrying the raw FD of the already-locked
/// `sqryd.lock` file.  The grandchild calls
/// [`PidfileLock::adopt`] on this FD instead of calling
/// [`acquire_pidfile_lock`] again.
#[cfg(unix)]
const ENV_LOCK_FD: &str = "SQRYD_LOCK_FD";

/// Environment variable carrying the canonical path of `sqryd.pid` so
/// the grandchild's adopted [`PidfileLock`] can unlink it on Drop.
#[cfg(unix)]
const ENV_PIDFILE_PATH: &str = "SQRYD_PIDFILE_PATH";

/// Environment variable carrying the canonical path of `sqryd.lock` so
/// the grandchild's adopted [`PidfileLock`] knows which lockfile backs
/// the inherited FD.
#[cfg(unix)]
const ENV_LOCKFILE_PATH: &str = "SQRYD_LOCKFILE_PATH";

// ---------------------------------------------------------------------------
// CLI definition (§C.2)
// ---------------------------------------------------------------------------

/// Production `sqryd` daemon binary.
///
/// Run `sqryd help` or `sqryd <subcommand> --help` for usage.
#[derive(Debug, Parser)]
#[command(
    name = "sqryd",
    about = "sqry daemon — persistent semantic code-search graph service",
    version,
    author
)]
pub struct SqrydCli {
    /// Path to the daemon configuration file.
    ///
    /// Defaults to `~/.config/sqry/daemon.toml` (or the platform-specific
    /// equivalent).  Can also be set via the `SQRY_DAEMON_CONFIG` environment
    /// variable; the `--config` flag takes precedence over the env var.
    #[arg(long, value_name = "FILE", env = "SQRY_DAEMON_CONFIG", global = true)]
    pub config: Option<PathBuf>,

    /// Log verbosity (e.g. `info`, `debug`, `sqry_daemon=trace`).
    ///
    /// Accepts the same syntax as `RUST_LOG` / `tracing_subscriber::EnvFilter`.
    /// Overrides both `SQRY_DAEMON_LOG_LEVEL` and the `log_level` field in
    /// the config file.
    #[arg(long, value_name = "LEVEL", global = true)]
    pub log_level: Option<String>,

    /// Subcommand to run.  Defaults to `start` (foreground mode).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the daemon.
    ///
    /// With `--detach` (Unix only): the parent forks a grandchild that binds
    /// the socket and then the parent exits 0.  The grandchild inherits the
    /// pidfile lock FD so the lockfile stays authoritative across the
    /// fork boundary.
    ///
    /// Without `--detach` (the default): the daemon runs in the foreground and
    /// is suitable for direct terminal use, containers, and systemd
    /// `Type=notify` supervision.
    Start(Start),

    /// Run in the foreground (alias for `start` without `--detach`).
    Foreground,

    /// Send `daemon/stop` to the running daemon and wait until its socket
    /// becomes unreachable.
    Stop {
        /// Maximum seconds to wait for the daemon to exit gracefully.
        #[arg(long, default_value_t = 15)]
        timeout_secs: u64,
    },

    /// Query daemon status.
    Status {
        /// Emit machine-readable JSON instead of the default human-readable
        /// summary.
        #[arg(long)]
        json: bool,
    },

    /// Emit a systemd **user** service unit to stdout.
    ///
    /// Pipe the output to
    /// `~/.config/systemd/user/sqryd.service` and then run
    /// `systemctl --user daemon-reload && systemctl --user enable --now sqryd`.
    #[cfg(target_os = "linux")]
    InstallSystemdUser,

    /// Emit a systemd **system** service unit to stdout.
    ///
    /// Use `--user NAME` to specify the POSIX account that the templated
    /// `sqryd@NAME.service` should run as.  Falls back to `$USER` if omitted.
    ///
    /// Install with
    /// `systemctl enable --now sqryd@<username>` after placing the file in
    /// `/etc/systemd/system/`.
    #[cfg(target_os = "linux")]
    InstallSystemdSystem {
        /// POSIX user account name for the `%i` template instance specifier.
        #[arg(long)]
        user: Option<String>,
    },

    /// Emit a launchd user-agent plist to stdout.
    ///
    /// Install with:
    /// ```bash
    /// sqryd install-launchd > ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist
    /// launchctl load -w ~/Library/LaunchAgents/ai.verivus.sqry.sqryd.plist
    /// ```
    #[cfg(target_os = "macos")]
    InstallLaunchd,

    /// Emit `sc.exe create` + Task Scheduler XML to stdout.  [Windows only]
    #[cfg(target_os = "windows")]
    InstallWindows,

    /// Print the effective daemon configuration as canonical TOML and exit.
    ///
    /// Useful to verify which config file was loaded and what the resolved
    /// defaults look like before starting the daemon.
    PrintConfig,
}

/// Arguments for `sqryd start`.
#[derive(Debug, clap::Args, Default)]
pub struct Start {
    /// Fork a grandchild to run the daemon and exit the parent immediately.
    ///
    /// Unix only.  On Windows this flag is accepted but has no effect (a WARN
    /// is logged and the daemon continues in the foreground).
    #[arg(long)]
    pub detach: bool,

    /// *(Internal -- hidden from `--help`)*  Marks the grandchild spawned by
    /// the detach path.  The grandchild adopts the inherited lock FD and
    /// self-pipe FD instead of re-acquiring them.
    #[arg(long, hide = true)]
    pub spawned_by_client: bool,
}

// ---------------------------------------------------------------------------
// Top-level dispatcher
// ---------------------------------------------------------------------------

/// Parse the CLI and dispatch to the appropriate `run_*` function.
///
/// This is the only public entry point called from `main`.  On error it
/// returns the error value; `main` prints it with `{err:#}` and converts
/// `err.exit_code()` to a [`std::process::ExitCode`].
pub fn run() -> DaemonResult<()> {
    let cli = SqrydCli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::Io)?;

    let log_level_owned = cli.log_level.clone();
    let log_level = log_level_owned.as_deref();
    let config_path = cli.config.clone();

    let command = cli.command.unwrap_or(Command::Start(Start::default()));

    match command {
        Command::Start(start) => rt.block_on(run_start(start, config_path, log_level)),
        Command::Foreground => rt.block_on(run_start(Start::default(), config_path, log_level)),
        Command::Stop { timeout_secs } => {
            rt.block_on(run_stop(config_path, log_level, timeout_secs))
        }
        Command::Status { json } => rt.block_on(run_status(config_path, log_level, json)),
        #[cfg(target_os = "linux")]
        Command::InstallSystemdUser => run_install_systemd_user(config_path, log_level),
        #[cfg(target_os = "linux")]
        Command::InstallSystemdSystem { user } => {
            run_install_systemd_system(config_path, log_level, user)
        }
        #[cfg(target_os = "macos")]
        Command::InstallLaunchd => run_install_launchd(config_path, log_level),
        #[cfg(target_os = "windows")]
        Command::InstallWindows => run_install_windows(config_path, log_level),
        Command::PrintConfig => run_print_config(config_path, log_level),
    }
}

// ---------------------------------------------------------------------------
// Start -- foreground and detach paths
// ---------------------------------------------------------------------------

/// Entry point for `sqryd start` and `sqryd foreground`.
///
/// Dispatches to:
/// - [`run_start_spawned_by_client`] when `--spawned-by-client` is set
///   (grandchild entry point in the double-fork detach path).
/// - [`run_start_detach`] when `--detach` is set (parent entry point).
/// - [`run_start_foreground`] otherwise.
async fn run_start(
    args: Start,
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    if args.spawned_by_client {
        return run_start_spawned_by_client(config_path, log_level).await;
    }
    if args.detach {
        return run_start_detach(config_path, log_level).await;
    }
    run_start_foreground(config_path, log_level).await
}

// ---------------------------------------------------------------------------
// Foreground path (§C.3.1) -- 17 ordered steps
// ---------------------------------------------------------------------------

/// Foreground startup path (no detach, no self-pipe).
///
/// Acquires the pidfile lock, wires all components, and runs `server.run()`.
async fn run_start_foreground(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    // Step 1 -- Load config.
    let cfg = load_config(config_path)?;
    let cfg = Arc::new(cfg);

    // Step 2 -- Install tracing.
    let _tracing_guard = match install_tracing(&cfg, log_level) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("sqryd: warning: tracing setup: {e:#}");
            None
        }
    };

    info!(
        version = env!("CARGO_PKG_VERSION"),
        socket = %cfg.socket_path().display(),
        pid_file = %cfg.pid_path().display(),
        "sqryd starting"
    );

    // Step 3 -- Create runtime_dir with 0700 on Unix.
    create_runtime_dir(&cfg)?;

    // Step 4 -- Acquire pidfile lock.
    let pidfile_lock = acquire_pidfile_lock(&cfg)?;
    info!(pid_file = %cfg.pid_path().display(), "pidfile lock acquired");

    // Steps 6-10: build all components.
    let (manager, dispatcher, builder, executor) = build_daemon_components(Arc::clone(&cfg));

    // Step 11 -- CancellationToken.
    let shutdown = CancellationToken::new();

    // Step 12 -- Install signal handlers.
    let _signal_guard = install_signal_handlers(shutdown.clone())?;
    info!("signal handlers installed");

    // Step 13 -- Pre-load pinned workspaces (log + continue on failure).
    preload_pinned_workspaces(&cfg, &manager, &builder).await;

    // Step 14 -- Bind IPC server.
    let server = IpcServer::bind(
        Arc::clone(&cfg),
        Arc::clone(&manager),
        Arc::clone(&dispatcher),
        Arc::clone(&builder),
        Arc::clone(&executor),
        shutdown.clone(),
    )
    .await?;
    info!(socket = %server.socket_path().display(), "IPC server bound");

    // Step 15 -- Signal ready.
    signal_ready(&cfg, server.socket_path());

    // Step 16 -- Run.
    server.run().await?;

    // Step 17 -- RAII Drop: _signal_guard, then pidfile_lock.
    info!("sqryd shutdown complete");
    drop(_signal_guard);
    drop(pidfile_lock);

    Ok(())
}

// ---------------------------------------------------------------------------
// Detach path -- parent side (§C.3.2 A-E)
// ---------------------------------------------------------------------------

/// Parent entry point for `sqryd start --detach`.
///
/// On Unix: creates a self-pipe, acquires the pidfile lock, spawns the
/// grandchild with the lock FD and pipe write-end inherited, then polls the
/// read end until EOF (ready) or timeout.
///
/// On Windows: no-op with WARN log; runs foreground instead.
async fn run_start_detach(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    #[cfg(unix)]
    {
        run_start_detach_unix(config_path, log_level).await
    }
    #[cfg(not(unix))]
    {
        let cfg = load_config(config_path.clone())?;
        setup_stderr_tracing(log_level, &cfg);
        drop(cfg);
        warn!(
            "--detach is a no-op on Windows; running in the foreground instead. \
             Use Task Scheduler or sc.exe to run sqryd as a background service."
        );
        run_start_foreground(config_path, log_level).await
    }
}

#[cfg(unix)]
async fn run_start_detach_unix(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    // Step A -- Load config and set up basic tracing for the parent.
    let cfg = load_config(config_path.clone())?;
    let cfg = Arc::new(cfg);

    let _tracing_guard = match install_tracing(&cfg, log_level) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("sqryd: warning: tracing setup (parent): {e:#}");
            None
        }
    };

    create_runtime_dir(&cfg)?;

    // Step A -- Acquire pidfile lock (WriteOwner).
    let mut pidfile_lock = acquire_pidfile_lock(&cfg)?;
    info!(pid_file = %cfg.pid_path().display(), "parent: pidfile lock acquired (WriteOwner)");

    // Step B -- Create self-pipe with O_CLOEXEC on both ends.
    let (read_fd, write_fd) = create_pipe()?;

    // Retrieve the raw FD from the pidfile lock.
    let lock_fd = pidfile_lock.as_raw_fd();
    let pidfile_path = cfg.pid_path();
    let lockfile_path = cfg.lock_path();

    // Step C -- Spawn the grandchild.
    let exe = std::env::current_exe()
        .map_err(|e| DaemonError::Io(std::io::Error::other(format!("current_exe: {e}"))))?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["start", "--detach", "--spawned-by-client"]);

    if let Some(ref cp) = config_path {
        cmd.arg("--config").arg(cp);
    }
    if let Some(ll) = log_level {
        cmd.arg("--log-level").arg(ll);
    }

    cmd.env(ENV_READY_PIPE_FD, write_fd.to_string());
    cmd.env(ENV_LOCK_FD, lock_fd.to_string());
    cmd.env(ENV_PIDFILE_PATH, pidfile_path.as_os_str());
    cmd.env(ENV_LOCKFILE_PATH, lockfile_path.as_os_str());

    // Redirect stdio to /dev/null for the detached grandchild.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    // pre_exec: setsid() + clear FD_CLOEXEC on write_fd + lock_fd.
    // SAFETY: pre_exec runs after fork in the child; only async-signal-safe
    // syscalls are used (setsid and fcntl are both async-signal-safe per POSIX).
    let write_fd_copy = write_fd;
    let lock_fd_copy = lock_fd;
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(move || {
            // New session: detach from controlling terminal.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Clear FD_CLOEXEC so write_fd and lock_fd survive the exec.
            for fd in [write_fd_copy, lock_fd_copy] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let rc = libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                if rc < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| {
        DaemonError::Io(std::io::Error::other(format!(
            "failed to spawn grandchild sqryd process: {e}"
        )))
    })?;

    let grandchild_pid = child.id();
    info!(pid = grandchild_pid, "spawned grandchild");

    // Step D -- Parent closes its write end; only the grandchild holds it now.
    drop_raw_fd(write_fd);

    // Step E -- Poll the read end until EOF or timeout.
    let timeout_secs = cfg.auto_start_ready_timeout_secs;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);

    let result = poll_ready_pipe(read_fd, deadline);

    // Close read end regardless of outcome.
    drop_raw_fd(read_fd);

    match result {
        Ok(()) => {
            // EOF on the ready pipe — but EOF can also mean the grandchild
            // exited early (before step 15) and the OS closed all its write-end
            // FDs as part of process teardown.  Distinguish the two cases by
            // calling `try_wait`: if the child has already exited it is a
            // startup failure, not a readiness signal (M-1 fix).
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Grandchild already exited — pipe EOF was process death.
                    warn!(
                        pid = grandchild_pid,
                        ?status,
                        "grandchild exited before signalling ready (pipe EOF was process death)"
                    );
                    // Drop with WriteOwner: unlinks pidfile + unlocks.
                    drop(pidfile_lock);
                    return Err(DaemonError::AutoStartTimeout {
                        timeout_secs,
                        socket: cfg.socket_path(),
                    });
                }
                Ok(None) => {
                    // Child is still running — pipe EOF was step 15 close: genuine readiness.
                }
                Err(e) => {
                    // try_wait failed (unusual). Log and assume alive.
                    warn!(
                        pid = grandchild_pid,
                        err = %e,
                        "try_wait after pipe EOF failed -- assuming grandchild is alive"
                    );
                }
            }

            // Grandchild signalled ready: hand off pidfile ownership so our
            // Drop does NOT unlink the pidfile (Handoff state).
            pidfile_lock.hand_off_to_adopter();
            info!(
                pid = grandchild_pid,
                "grandchild signalled ready -- parent exiting 0 (Handoff)"
            );
            // Drop with Handoff: does NOT unlock (M-2 fix applied in pidfile.rs).
            drop(pidfile_lock);
            Ok(())
        }
        Err(()) => {
            warn!(
                pid = grandchild_pid,
                timeout_secs, "grandchild did not signal ready within timeout -- killing"
            );
            // Child::kill sends SIGKILL to the exact PID (n5: targets the
            // specific PID via libc::kill(pid, SIGKILL), bypassing the
            // process group despite the grandchild's setsid).
            if let Err(e) = child.kill() {
                warn!(pid = grandchild_pid, err = %e, "kill(grandchild) failed");
            }
            let _ = child.wait();
            // Drop with WriteOwner: unlinks pidfile + unlocks.
            drop(pidfile_lock);
            Err(DaemonError::AutoStartTimeout {
                timeout_secs,
                socket: cfg.socket_path(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Grandchild path (§C.3.2 F)
// ---------------------------------------------------------------------------

/// Grandchild entry point -- `sqryd start --detach --spawned-by-client`.
///
/// Reads `SQRYD_LOCK_FD` and `SQRYD_READY_PIPE_FD` from the environment,
/// adopts the inherited pidfile lock, and then runs the foreground startup
/// steps 2-16 with the ready-pipe write-end FD so step 15 closes it to
/// signal readiness to the parent.
async fn run_start_spawned_by_client(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    #[cfg(unix)]
    {
        run_start_spawned_by_client_unix(config_path, log_level).await
    }
    #[cfg(not(unix))]
    {
        // Should never happen: --spawned-by-client is only emitted by the
        // parent on Unix.  Run foreground as a safe fallback.
        warn!("--spawned-by-client reached on non-Unix -- running foreground");
        run_start_foreground(config_path, log_level).await
    }
}

#[cfg(unix)]
async fn run_start_spawned_by_client_unix(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    use std::os::unix::io::RawFd;

    // Read inherited FDs from the environment.
    let lock_fd: RawFd = read_env_fd(ENV_LOCK_FD).ok_or_else(|| {
        DaemonError::Io(std::io::Error::other(
            "grandchild: SQRYD_LOCK_FD not set (only valid via --detach parent spawn)",
        ))
    })?;
    let ready_pipe_fd: RawFd = read_env_fd(ENV_READY_PIPE_FD).ok_or_else(|| {
        DaemonError::Io(std::io::Error::other(
            "grandchild: SQRYD_READY_PIPE_FD not set",
        ))
    })?;
    let pidfile_path: PathBuf = std::env::var_os(ENV_PIDFILE_PATH)
        .map(PathBuf::from)
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "grandchild: SQRYD_PIDFILE_PATH not set",
            ))
        })?;
    let lockfile_path: PathBuf = std::env::var_os(ENV_LOCKFILE_PATH)
        .map(PathBuf::from)
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "grandchild: SQRYD_LOCKFILE_PATH not set",
            ))
        })?;

    // Step 1 -- Load config.
    let cfg = load_config(config_path)?;
    let cfg = Arc::new(cfg);

    // Write grandchild's own PID to pidfile (atomic tmp+rename, overwriting
    // the parent's PID written by acquire_pidfile_lock).
    write_pid_file_grandchild(&cfg.pid_path())?;

    // Adopt the inherited lock FD (§C.3.3 m6 canonical signature).
    // SAFETY: lock_fd is a valid inherited FD carrying an active OFD-level
    // flock acquired by the parent.  The grandchild is the sole user of this
    // FD in this process.  adopt() takes ownership; caller must NOT close
    // lock_fd separately.
    let _pidfile_lock = unsafe { PidfileLock::adopt(lock_fd, pidfile_path, lockfile_path) };

    info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "sqryd grandchild: pidfile lock adopted -- beginning foreground startup"
    );

    // Steps 2-16 -- run the foreground startup path passing the ready-pipe FD.
    run_start_foreground_inner(cfg, log_level, ready_pipe_fd).await
}

/// Inner foreground path shared by the grandchild (`--spawned-by-client`)
/// and future callers that already hold a pre-loaded config + lock.
///
/// Steps 2-16 per §C.3.1.  Step 1 (config load) and the pidfile lock
/// are done by the caller.
///
/// `ready_pipe_write_fd`: the self-pipe write-end to close at step 15 to
/// signal the parent.  Pass -1 (or any negative value) on non-unix to skip.
async fn run_start_foreground_inner(
    cfg: Arc<DaemonConfig>,
    log_level: Option<&str>,
    #[cfg(unix)] ready_pipe_write_fd: libc::c_int,
    #[cfg(not(unix))] _ready_pipe_write_fd: i32,
) -> DaemonResult<()> {
    // Step 2 -- Install tracing.
    let _tracing_guard = match install_tracing(&cfg, log_level) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("sqryd: warning: tracing setup: {e:#}");
            None
        }
    };
    info!(
        version = env!("CARGO_PKG_VERSION"),
        socket = %cfg.socket_path().display(),
        "sqryd grandchild: tracing active"
    );

    // Step 3 -- runtime_dir (may already exist; idempotent).
    create_runtime_dir(&cfg)?;

    // Steps 6-10.
    let (manager, dispatcher, builder, executor) = build_daemon_components(Arc::clone(&cfg));

    // Step 11.
    let shutdown = CancellationToken::new();

    // Step 12.
    let _signal_guard = install_signal_handlers(shutdown.clone())?;

    // Step 13.
    preload_pinned_workspaces(&cfg, &manager, &builder).await;

    // Step 14.
    let server = IpcServer::bind(
        Arc::clone(&cfg),
        Arc::clone(&manager),
        Arc::clone(&dispatcher),
        Arc::clone(&builder),
        Arc::clone(&executor),
        shutdown.clone(),
    )
    .await?;
    info!(socket = %server.socket_path().display(), "IPC server bound");

    // Step 15 -- Signal ready.
    signal_ready(&cfg, server.socket_path());

    // Close the self-pipe write end so the parent's read() returns EOF.
    #[cfg(unix)]
    if ready_pipe_write_fd >= 0 {
        close_ready_pipe_fd(ready_pipe_write_fd);
    }

    // Step 16 -- Run.
    server.run().await?;

    info!("sqryd shutdown complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Stop command
// ---------------------------------------------------------------------------

/// Connect to the running daemon, send `daemon/stop`, and wait until the
/// socket becomes unreachable (bounded by `timeout_secs`).
async fn run_stop(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
    timeout_secs: u64,
) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let socket_path = cfg.socket_path();

    info!(socket = %socket_path.display(), "connecting to daemon to send daemon/stop");

    // Send daemon/stop and read the response via the raw framed protocol.
    let stop_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "daemon/stop",
        "params": {}
    });
    send_management_request(&socket_path, &stop_req).await?;

    info!(
        timeout_secs,
        "waiting for daemon socket to become unreachable"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if !crate::lifecycle::detach::try_connect_path(&socket_path).await {
            info!("daemon socket gone -- stop complete");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(DaemonError::AutoStartTimeout {
                timeout_secs,
                socket: socket_path,
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------------------------------------------------------------------------
// Status command (m3 fix: revalidate via socket connect, never pidfile-only)
// ---------------------------------------------------------------------------

/// Query daemon status, revalidating liveness via socket connect (m3 fix).
async fn run_status(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
    json_output: bool,
) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let socket_path = cfg.socket_path();

    // m3 fix: revalidate via socket connect -- never pidfile-only.
    if !crate::lifecycle::detach::try_connect_path(&socket_path).await {
        eprintln!(
            "sqryd: daemon is not running (socket not connectable: {})",
            socket_path.display()
        );
        return Err(DaemonError::Io(std::io::Error::other(format!(
            "daemon socket not reachable: {}",
            socket_path.display()
        ))));
    }

    let status_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "daemon/status",
        "params": {}
    });

    let resp_buf = send_management_request(&socket_path, &status_req).await?;

    if json_output {
        println!("{}", String::from_utf8_lossy(&resp_buf));
    } else {
        // m-4 fix: return Err on malformed JSON so protocol breakage is visible
        // to operators and scripts instead of silently returning Ok.
        let v = serde_json::from_slice::<serde_json::Value>(&resp_buf).map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!(
                "daemon/status response was not valid JSON: {e} (raw: {})",
                String::from_utf8_lossy(&resp_buf)
            )))
        })?;
        if let Some(result) = v.get("result") {
            render_status_human(result);
        } else if let Some(err_val) = v.get("error") {
            eprintln!("sqryd status error: {err_val}");
            return Err(DaemonError::Io(std::io::Error::other(format!(
                "daemon/status error: {err_val}"
            ))));
        } else {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Raw management-request helper (handshake + one JSON-RPC round trip)
// ---------------------------------------------------------------------------

/// Send one JSON-RPC management request to the daemon and return the raw
/// response frame bytes.
///
/// The full protocol is:
/// 1. Connect to the daemon socket.
/// 2. Send `DaemonHello` frame.
/// 3. Read `DaemonHelloResponse` frame (verify `compatible`).
/// 4. Send the JSON-RPC request frame.
/// 5. Read the JSON-RPC response frame.
async fn send_management_request(
    socket_path: &std::path::Path,
    req: &serde_json::Value,
) -> DaemonResult<Vec<u8>> {
    use crate::{DaemonHello, DaemonHelloResponse};
    use sqry_daemon_protocol::framing::{read_frame, write_frame_json};

    #[cfg(unix)]
    let mut stream = {
        tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| {
                DaemonError::Io(std::io::Error::other(format!(
                    "connect to daemon socket {}: {e}",
                    socket_path.display()
                )))
            })?
    };

    #[cfg(windows)]
    let mut stream = {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe_path = socket_path.to_string_lossy();
        ClientOptions::new().open(pipe_path.as_ref()).map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!(
                "connect to daemon pipe {}: {e}",
                pipe_path
            )))
        })?
    };

    // Step 2: send DaemonHello.
    let hello = DaemonHello {
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: 1,
    };
    write_frame_json(&mut stream, &hello)
        .await
        .map_err(|e| DaemonError::Io(std::io::Error::other(format!("send hello: {e}"))))?;

    // Step 3: read DaemonHelloResponse.
    let hello_resp_bytes = read_frame(&mut stream)
        .await
        .map_err(|e| DaemonError::Io(std::io::Error::other(format!("read hello response: {e}"))))?
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "daemon closed connection before hello response",
            ))
        })?;
    let hello_resp: DaemonHelloResponse =
        serde_json::from_slice(&hello_resp_bytes).map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!("parse hello response: {e}")))
        })?;
    if !hello_resp.compatible {
        return Err(DaemonError::Io(std::io::Error::other(
            "daemon is not compatible with this client version",
        )));
    }

    // Step 4: send the JSON-RPC request.
    write_frame_json(&mut stream, req)
        .await
        .map_err(|e| DaemonError::Io(std::io::Error::other(format!("send request: {e}"))))?;

    // Step 5: read the JSON-RPC response.
    let resp_bytes = read_frame(&mut stream)
        .await
        .map_err(|e| DaemonError::Io(std::io::Error::other(format!("read response: {e}"))))?
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "daemon closed connection before sending response",
            ))
        })?;

    Ok(resp_bytes)
}

/// Render a human-readable daemon status summary from the `result` field of a
/// `daemon/status` JSON-RPC response envelope.
fn render_status_human(result: &serde_json::Value) {
    let payload = result.get("data").unwrap_or(result);

    let version = payload
        .get("daemon_version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let uptime = payload
        .get("uptime_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    println!("sqryd  version: {version}");
    println!("       uptime:  {uptime}s");

    if let Some(memory) = payload.get("memory") {
        let limit = memory
            .get("limit_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let current = memory
            .get("current_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!(
            "       memory:  {} MiB used / {} MiB limit",
            current / (1024 * 1024),
            limit / (1024 * 1024)
        );
    }

    if let Some(workspaces) = payload.get("workspaces").and_then(|v| v.as_array()) {
        println!("       workspaces: {}", workspaces.len());
        for ws in workspaces {
            let path = ws.get("index_root").and_then(|v| v.as_str()).unwrap_or("?");
            let state = ws
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            println!("         {state:10} {path}");
        }
    }
}

// ---------------------------------------------------------------------------
// Install subcommands
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn run_install_systemd_user(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let opts = InstallOptions::default();
    let unit = crate::lifecycle::units::systemd::generate_user_unit(&cfg, &opts);
    println!("{unit}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_install_systemd_system(
    config_path: Option<PathBuf>,
    log_level: Option<&str>,
    user: Option<String>,
) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let opts = InstallOptions {
        user: user.clone(),
        ..Default::default()
    };
    // Validate the user account (n3 fix: exits 78 EX_CONFIG on failure).
    let resolved_user =
        crate::lifecycle::units::systemd::resolve_system_unit_user(&opts).map_err(|e| {
            DaemonError::Config {
                path: cfg
                    .pid_path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_owned(),
                source: anyhow::anyhow!("{e}"),
            }
        })?;
    let opts_with_user = InstallOptions {
        user: Some(resolved_user),
        ..Default::default()
    };
    let unit = crate::lifecycle::units::systemd::generate_system_unit(&cfg, &opts_with_user);
    println!("{unit}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_install_launchd(config_path: Option<PathBuf>, log_level: Option<&str>) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let opts = InstallOptions::default();
    let plist = crate::lifecycle::units::launchd::generate_plist(&cfg, &opts);
    println!("{plist}");
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_install_windows(config_path: Option<PathBuf>, log_level: Option<&str>) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let opts = InstallOptions::default();
    let sc = crate::lifecycle::units::windows::generate_sc_create(&cfg, &opts);
    let xml = crate::lifecycle::units::windows::generate_task_xml(&cfg, &opts);
    println!("-- sc.exe create command --");
    println!("{sc}");
    println!();
    println!("-- Task Scheduler XML --");
    println!("{xml}");
    Ok(())
}

fn run_print_config(config_path: Option<PathBuf>, log_level: Option<&str>) -> DaemonResult<()> {
    let cfg = load_config(config_path)?;
    setup_stderr_tracing(log_level, &cfg);
    let toml_str = toml::to_string_pretty(&cfg).map_err(|e| DaemonError::Config {
        path: PathBuf::from("<serialise>"),
        source: anyhow::anyhow!("toml serialisation failed: {e}"),
    })?;
    println!("{toml_str}");
    Ok(())
}

// ---------------------------------------------------------------------------
// main() bridge -- convert DaemonResult<()> to ExitCode
// ---------------------------------------------------------------------------

/// Top-level `main` trampoline.  Calls [`run`], prints any error to stderr,
/// and converts the error to a POSIX exit code via [`DaemonError::exit_code`].
pub fn main_impl() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("sqryd: fatal: {err:#}");
            eprintln!("sqryd: {err:#}");
            ExitCode::from(err.exit_code())
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Load the daemon config, honouring an optional explicit path override.
///
/// When `config_path` is `Some`, the config is loaded from that path and
/// then env overrides are applied.  This avoids mutating the process-global
/// environment after the multi-threaded Tokio runtime has been created
/// (`std::env::set_var` is UB in the presence of concurrent environment reads,
/// and Rust 1.81 made it explicitly unsafe).
///
/// When `config_path` is `None`, `DaemonConfig::load()` is used which
/// respects `SQRY_DAEMON_CONFIG` normally.
fn load_config(config_path: Option<PathBuf>) -> DaemonResult<DaemonConfig> {
    if let Some(ref p) = config_path {
        let mut cfg = DaemonConfig::load_from_path(p)?;
        cfg.apply_env_overrides()?;
        cfg.validate()?;
        Ok(cfg)
    } else {
        DaemonConfig::load()
    }
}

/// Install a minimal stderr tracing subscriber.
///
/// Used by short-lived subcommands (`stop`, `status`, `install-*`,
/// `print-config`).  Silently ignores double-install errors (e.g. in tests).
fn setup_stderr_tracing(log_level: Option<&str>, cfg: &DaemonConfig) {
    let level = log_level
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("SQRY_DAEMON_LOG_LEVEL").ok())
        .unwrap_or_else(|| cfg.log_level.clone());
    let filter = tracing_subscriber::EnvFilter::try_new(&level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .compact()
        .with_env_filter(filter)
        .try_init();
}

/// Create `runtime_dir()` with mode `0700` on Unix.
fn create_runtime_dir(cfg: &DaemonConfig) -> DaemonResult<()> {
    let dir = cfg.runtime_dir();
    std::fs::create_dir_all(&dir).map_err(DaemonError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&dir, perms).map_err(DaemonError::Io)?;
    }

    Ok(())
}

/// Build the plugin manager, workspace manager, rebuild dispatcher,
/// workspace builder, and query executor.
///
/// Steps 6-10 per §C.3.1.  Factored into a helper so both the
/// foreground path and the grandchild inner path share the same code.
fn build_daemon_components(
    cfg: Arc<DaemonConfig>,
) -> (
    Arc<WorkspaceManager>,
    Arc<RebuildDispatcher>,
    Arc<dyn crate::workspace::WorkspaceBuilder>,
    Arc<QueryExecutor>,
) {
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let manager = WorkspaceManager::new(Arc::clone(&cfg));
    let dispatcher =
        RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&cfg), Arc::clone(&plugins));
    let builder: Arc<dyn crate::workspace::WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&plugins)));
    let executor = Arc::new(QueryExecutor::new());
    (manager, dispatcher, builder, executor)
}

/// Emit the authoritative ready signals and touch the diagnostic sentinel.
///
/// - `NOTIFY_SOCKET` set -> `sd_notify(READY=1)` (authoritative for systemd).
/// - Always: touch `runtime_dir/sqryd.ready` (diagnostic, non-authoritative).
///
/// The self-pipe close (grandchild -> parent) is handled inline by the
/// calling function because it requires the raw FD.
fn signal_ready(cfg: &DaemonConfig, socket_path: &std::path::Path) {
    if is_under_systemd() {
        if let Err(e) = notify_ready() {
            warn!(err = %e, "sd_notify(READY=1) failed -- systemctl may time out");
        } else {
            info!("sd_notify: READY=1 sent");
        }
    }

    let ready_path = cfg.runtime_dir().join("sqryd.ready");
    if let Err(e) = std::fs::write(&ready_path, b"") {
        warn!(
            path = %ready_path.display(),
            err = %e,
            "could not touch sqryd.ready sentinel (non-fatal)"
        );
    }

    info!(
        socket = %socket_path.display(),
        "sqryd ready -- accepting connections"
    );
}

/// Pre-load pinned workspaces declared in the daemon config.
///
/// Step 13 per §C.3.1: log + continue on failure.
async fn preload_pinned_workspaces(
    cfg: &DaemonConfig,
    manager: &Arc<WorkspaceManager>,
    builder: &Arc<dyn crate::workspace::WorkspaceBuilder>,
) {
    use sqry_core::project::ProjectRootMode;

    for ws_cfg in &cfg.workspaces {
        if ws_cfg.exclude || !ws_cfg.pinned {
            continue;
        }

        let root = ws_cfg.path.clone();
        let key =
            crate::workspace::WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);

        info!(path = %root.display(), "pre-loading pinned workspace");
        let estimate =
            crate::workspace::working_set_estimate(crate::workspace::WorkingSetInputs::default());

        if let Err(e) = manager.get_or_load(&key, builder.as_ref(), estimate) {
            warn!(
                path = %root.display(),
                err = %e,
                "pinned workspace pre-load failed (log + continue per §C.3.1 step 13)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Unix-specific low-level helpers
// ---------------------------------------------------------------------------

/// Create a pipe with `O_CLOEXEC` on both ends. Returns `(read_fd, write_fd)`.
#[cfg(all(unix, target_os = "linux"))]
fn create_pipe() -> DaemonResult<(libc::c_int, libc::c_int)> {
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 is a Linux syscall; fds is a valid 2-element array.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc < 0 {
        return Err(DaemonError::Io(std::io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

/// Create a pipe with close-on-exec set on both ends. Returns `(read_fd, write_fd)`.
#[cfg(all(unix, not(target_os = "linux")))]
fn create_pipe() -> DaemonResult<(libc::c_int, libc::c_int)> {
    let mut fds = [0i32; 2];
    // SAFETY: pipe is available on POSIX Unix targets; fds is a valid 2-element array.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc < 0 {
        return Err(DaemonError::Io(std::io::Error::last_os_error()));
    }

    if let Err(err) = set_close_on_exec(fds[0]).and_then(|()| set_close_on_exec(fds[1])) {
        drop_raw_fd(fds[0]);
        drop_raw_fd(fds[1]);
        return Err(err);
    }

    Ok((fds[0], fds[1]))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_close_on_exec(fd: libc::c_int) -> DaemonResult<()> {
    // SAFETY: fcntl only observes and updates descriptor flags for a live fd.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(DaemonError::Io(std::io::Error::last_os_error()));
    }

    // SAFETY: F_SETFD sets descriptor flags; FD_CLOEXEC preserves fork/exec hygiene.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(DaemonError::Io(std::io::Error::last_os_error()));
    }

    Ok(())
}

/// Close a raw FD; ignore errors (only call once per FD).
#[cfg(unix)]
fn drop_raw_fd(fd: libc::c_int) {
    // SAFETY: caller ensures exclusive ownership.
    unsafe { libc::close(fd) };
}

/// Poll the read end of the self-pipe until EOF (ready) or deadline.
///
/// Returns `Ok(())` on EOF (grandchild closed its write end).
/// Returns `Err(())` on timeout.
#[cfg(unix)]
fn poll_ready_pipe(read_fd: libc::c_int, deadline: std::time::Instant) -> Result<(), ()> {
    use std::io::Read as _;
    use std::os::unix::io::FromRawFd as _;

    // Wrap in a File for safe read().  We use forget() to prevent double-close
    // because the caller calls drop_raw_fd(read_fd) unconditionally.
    let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };

    // Switch to non-blocking so we can poll without blocking the parent thread.
    // SAFETY: fcntl is async-signal-safe and we hold exclusive ownership of read_fd.
    unsafe {
        let flags = libc::fcntl(read_fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(read_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    loop {
        let mut buf = [0u8; 1];
        match file.read(&mut buf) {
            Ok(0) => {
                // EOF: grandchild closed its write end.
                std::mem::forget(file);
                return Ok(());
            }
            Ok(_) => {
                // Spurious byte -- ignore and poll again.
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc::EAGAIN) =>
            {
                // No data yet.
            }
            Err(_) => {
                std::mem::forget(file);
                return Err(());
            }
        }

        if std::time::Instant::now() >= deadline {
            std::mem::forget(file);
            return Err(());
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Close the self-pipe write end FD after the grandchild has signalled ready.
#[cfg(unix)]
fn close_ready_pipe_fd(fd: libc::c_int) {
    // SAFETY: caller ensures this is the write end and no other code will use it.
    unsafe { libc::close(fd) };
}

/// Read a raw FD integer from an environment variable.
#[cfg(unix)]
fn read_env_fd(var: &str) -> Option<libc::c_int> {
    std::env::var(var).ok()?.parse::<libc::c_int>().ok()
}

/// Write the current process's PID to `pidfile_path` atomically.
///
/// The grandchild calls this to overwrite the parent's PID (written by
/// `acquire_pidfile_lock`) with its own PID.
#[cfg(unix)]
fn write_pid_file_grandchild(pidfile_path: &std::path::Path) -> DaemonResult<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let pid = std::process::id();
    let pid_str = format!("{pid}\n");

    let tmp_path = pidfile_path.with_extension("tmp.gc");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp_path)
            .map_err(DaemonError::Io)?;
        f.write_all(pid_str.as_bytes()).map_err(DaemonError::Io)?;
        f.sync_data().map_err(DaemonError::Io)?;
    }
    std::fs::rename(&tmp_path, pidfile_path).map_err(DaemonError::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- print_config -------------------------------------------------------

    /// `run_print_config` must serialize the effective config as canonical TOML.
    /// The output must round-trip back into a valid `DaemonConfig` with the
    /// same field values.
    #[test]
    fn print_config_emits_canonical_toml() {
        let cfg = DaemonConfig::default();
        let toml_str = toml::to_string_pretty(&cfg)
            .expect("DaemonConfig must serialise to TOML without error");

        assert!(!toml_str.is_empty(), "serialised config must not be empty");

        // Round-trip.
        let reparsed: DaemonConfig =
            toml::from_str(&toml_str).expect("serialised TOML must be parseable back");

        assert_eq!(reparsed.memory_limit_mb, cfg.memory_limit_mb);
        assert_eq!(
            reparsed.auto_start_ready_timeout_secs,
            cfg.auto_start_ready_timeout_secs
        );
        assert_eq!(reparsed.log_keep_rotations, cfg.log_keep_rotations);
    }

    /// `run_print_config` with no config path must succeed (all defaults).
    #[test]
    fn run_print_config_succeeds_with_defaults() {
        // Clear any lingering SQRY_DAEMON_CONFIG.
        unsafe { std::env::remove_var("SQRY_DAEMON_CONFIG") };

        let result = run_print_config(None, None);
        assert!(
            result.is_ok(),
            "run_print_config with no config file must succeed: {result:?}"
        );
    }

    // ---- install-systemd-user (Linux only) ----------------------------------

    /// On Linux, `install_systemd_user` must produce a non-empty string
    /// containing the expected `Type=notify` marker.
    #[cfg(target_os = "linux")]
    #[test]
    fn install_systemd_user_prints_to_stdout() {
        use crate::lifecycle::units::systemd::generate_user_unit;
        let cfg = DaemonConfig::default();
        let opts = InstallOptions::default();
        let unit = generate_user_unit(&cfg, &opts);
        assert!(!unit.is_empty(), "systemd user unit must be non-empty");
        assert!(
            unit.contains("Type=notify"),
            "systemd user unit must contain 'Type=notify'"
        );
        assert!(
            unit.contains("sqryd"),
            "systemd user unit must reference sqryd"
        );
    }

    // ---- clap CLI parsing ---------------------------------------------------

    /// `sqryd` with no args must parse without error (command is None or Start).
    #[test]
    fn default_command_is_start_foreground() {
        let cli = SqrydCli::try_parse_from(["sqryd"]).expect("parse must succeed");
        match cli.command {
            None => {}
            Some(Command::Start(Start {
                detach: false,
                spawned_by_client: false,
            })) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    /// `sqryd start` must parse to `Start { detach: false }`.
    #[test]
    fn start_without_detach_is_foreground() {
        let cli = SqrydCli::try_parse_from(["sqryd", "start"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Start(Start {
                detach: false,
                spawned_by_client: false,
            }))
        ));
    }

    /// `sqryd start --detach` must parse to `Start { detach: true }`.
    #[test]
    fn start_with_detach_flag_is_parsed() {
        let cli = SqrydCli::try_parse_from(["sqryd", "start", "--detach"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Start(Start {
                detach: true,
                spawned_by_client: false,
            }))
        ));
    }

    /// `sqryd start --detach --spawned-by-client` must parse correctly.
    #[test]
    fn start_spawned_by_client_is_hidden_but_parseable() {
        let cli = SqrydCli::try_parse_from(["sqryd", "start", "--detach", "--spawned-by-client"])
            .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Start(Start {
                detach: true,
                spawned_by_client: true,
            }))
        ));
    }

    /// `sqryd foreground` must parse.
    #[test]
    fn foreground_subcommand_parses() {
        let cli = SqrydCli::try_parse_from(["sqryd", "foreground"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Foreground)));
    }

    /// `sqryd stop --timeout-secs 30` must parse with the custom timeout.
    #[test]
    fn stop_with_timeout_parses() {
        let cli =
            SqrydCli::try_parse_from(["sqryd", "stop", "--timeout-secs", "30"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Stop { timeout_secs: 30 })
        ));
    }

    /// `sqryd status --json` must parse with `json = true`.
    #[test]
    fn status_with_json_flag_parses() {
        let cli = SqrydCli::try_parse_from(["sqryd", "status", "--json"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Status { json: true })));
    }

    /// `sqryd print-config` must parse.
    #[test]
    fn print_config_subcommand_parses() {
        let cli = SqrydCli::try_parse_from(["sqryd", "print-config"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::PrintConfig)));
    }

    /// `sqryd --config /tmp/test.toml print-config` must capture the global flag.
    #[test]
    fn global_config_flag_is_parsed() {
        let cli = SqrydCli::try_parse_from(["sqryd", "--config", "/tmp/test.toml", "print-config"])
            .expect("parse");
        assert_eq!(
            cli.config,
            Some(PathBuf::from("/tmp/test.toml")),
            "--config flag must be captured"
        );
        assert!(matches!(cli.command, Some(Command::PrintConfig)));
    }

    /// `sqryd status` (without --json) must parse with `json = false`.
    #[test]
    fn status_without_json_flag_defaults_to_false() {
        let cli = SqrydCli::try_parse_from(["sqryd", "status"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Status { json: false })));
    }

    /// `sqryd stop` with no `--timeout-secs` must default to 15.
    #[test]
    fn stop_defaults_to_15_second_timeout() {
        let cli = SqrydCli::try_parse_from(["sqryd", "stop"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Stop { timeout_secs: 15 })
        ));
    }

    // ---- m-4 fix: status malformed-JSON path --------------------------------

    /// `render_status_human` must handle a minimal valid `daemon/status` result
    /// without panicking — this exercises the non-JSON output path.
    #[test]
    fn render_status_human_handles_minimal_result() {
        let result = serde_json::json!({
            "daemon_version": "8.0.6",
            "uptime_seconds": 42,
        });
        // Must not panic.
        render_status_human(&result);
    }

    /// `load_config` with an explicit path must not mutate the process
    /// environment (M-3 fix regression test).
    ///
    /// Creates a minimal TOML file on disk and calls `load_config(Some(path))`.
    /// Checks that `SQRY_DAEMON_CONFIG` is NOT set in the environment after the
    /// call (implying `set_var` was not called).
    #[test]
    fn load_config_with_explicit_path_does_not_set_env_var() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        // Clear any pre-existing env var so the assertion below is meaningful.
        unsafe { std::env::remove_var("SQRY_DAEMON_CONFIG") };

        // Write a minimal valid daemon TOML to a temp file.
        let mut tmp = NamedTempFile::new().expect("NamedTempFile");
        writeln!(tmp, "# minimal sqryd test config").expect("write");
        let path = tmp.path().to_path_buf();

        let result = load_config(Some(path.clone()));

        assert!(
            result.is_ok(),
            "load_config with valid TOML path must succeed: {result:?}"
        );
        assert!(
            std::env::var_os("SQRY_DAEMON_CONFIG").is_none(),
            "load_config must NOT mutate SQRY_DAEMON_CONFIG (M-3 fix)"
        );
    }
}
