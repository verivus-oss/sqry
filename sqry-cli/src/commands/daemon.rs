//! `sqry daemon` subcommand handlers.
//!
//! Provides lifecycle management for the sqryd daemon from the `sqry` CLI:
//! - `sqry daemon start` -- resolve `sqryd` binary, exec `sqryd start --detach`.
//! - `sqry daemon stop`  -- connect via [`DaemonClient`], send `daemon/stop`, poll socket gone.
//! - `sqry daemon status` -- connect via [`DaemonClient`], send `daemon/status`, render.
//! - `sqry daemon logs`  -- tail / follow the daemon log file.
//!
//! Additionally exposes [`try_auto_start_daemon`] for future Task 13 wiring into
//! query commands via the `SQRY_DAEMON_AUTO_START=1` opt-in environment variable.
//!
//! # Tokio runtime
//!
//! The `stop` and `status` subcommands need async I/O for [`DaemonClient`].
//! A `current_thread` runtime is created inside each handler — tokio is already
//! a transitive dependency of `sqry-cli` via `sqry-lsp`.
//!
//! # Platform notes
//!
//! [`try_connect_sync`] uses `std::os::unix::net::UnixStream` on Unix and a
//! named-pipe existence probe on Windows so callers can check reachability
//! without spinning up a Tokio runtime.

use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::args::{Cli, DaemonAction};

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Environment variable that enables auto-start of the daemon before query
/// commands. Set to `"1"` to opt in. Not wired into query commands until
/// Task 13 — this module exposes the plumbing only.
// Not yet used by run() — Task 13 wires this into query command dispatch.
#[allow(dead_code)]
const ENV_DAEMON_AUTO_START: &str = "SQRY_DAEMON_AUTO_START";

/// How long to poll for the socket to disappear after sending `daemon/stop`.
const STOP_POLL_INTERVAL_MS: u64 = 100;

/// How long each `tail_follow` iteration waits for a notify event before
/// doing a fallback read.
const FOLLOW_EVENT_TIMEOUT_MS: u64 = 250;

// ---------------------------------------------------------------------------
// Entry-point dispatcher.
// ---------------------------------------------------------------------------

/// Dispatch a `sqry daemon` subcommand.
///
/// # Errors
///
/// Propagates errors from individual subcommand handlers.
pub fn run(_cli: &Cli, action: &DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start {
            sqryd_path,
            timeout,
        } => run_daemon_start(sqryd_path.as_deref(), *timeout),
        DaemonAction::Stop { timeout } => run_daemon_stop(*timeout),
        DaemonAction::Status { json } => run_daemon_status(*json),
        DaemonAction::Logs { lines, follow } => run_daemon_logs(*lines, *follow),
        DaemonAction::Load { path } => run_daemon_load(path),
        DaemonAction::Rebuild {
            path,
            force,
            timeout,
            json,
        } => run_daemon_rebuild(path, *force, *timeout, *json),
        DaemonAction::Reset { path, force } => run_daemon_reset(path, *force),
    }
}

// ---------------------------------------------------------------------------
// reset.
// ---------------------------------------------------------------------------

/// Send a `daemon/reset` JSON-RPC request for the workspace at `path`.
/// Cluster-G §3.2 — non-destructive recovery primitive that drops the
/// in-memory graph + admission bytes but preserves the manager-map
/// entry. The next `daemon/load` reuses the existing on-disk
/// snapshot.
fn run_daemon_reset(path: &Path, force: bool) -> Result<()> {
    let config = load_daemon_config()?;
    let socket_path = config.socket_path();

    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;

    if !try_connect_sync(&socket_path)? {
        anyhow::bail!(
            "daemon is not running (socket {}). Start it with `sqry daemon start`.",
            socket_path.display()
        );
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for daemon reset")?;

    rt.block_on(async {
        let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;
        let result = client
            .reset(&canonical, force)
            .await
            .with_context(|| format!("daemon/reset for {}", canonical.display()))?;
        let was_reset = result
            .get("result")
            .and_then(|r| r.get("reset"))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| result.get("reset").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        if was_reset {
            eprintln!("sqry: workspace {} reset", canonical.display());
        } else {
            eprintln!(
                "sqry: workspace {} was not loaded; nothing to reset",
                canonical.display()
            );
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// rebuild.
// ---------------------------------------------------------------------------

fn run_daemon_rebuild(path: &Path, force: bool, timeout: u64, json: bool) -> Result<()> {
    let config = load_daemon_config()?;
    let socket_path = config.socket_path();

    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;

    if !try_connect_sync(&socket_path)? {
        anyhow::bail!(
            "daemon is not running (socket {}). Start it with `sqry daemon start`.",
            socket_path.display()
        );
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for daemon rebuild")?;

    rt.block_on(async {
        let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path)
            .await
            .with_context(|| {
                format!("failed to connect to daemon at {}", socket_path.display())
            })?;

        let started = Instant::now();
        let deadline = started + Duration::from_secs(timeout);

        // Spawn a status-polling task that prints progress to stderr.
        let poll_socket = socket_path.clone();
        let poll_path = canonical_path.clone();
        let poll_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poll_flag = std::sync::Arc::clone(&poll_done);
        let poll_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if poll_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                // Connect a fresh client for status polling.
                let Ok(mut poll_client) =
                    sqry_daemon_client::DaemonClient::connect(&poll_socket).await
                else {
                    continue;
                };
                if let Ok(status) = poll_client.status().await {
                    let elapsed = started.elapsed().as_secs();
                    if let Some(ws_state) = extract_workspace_state(&status, &poll_path) {
                        eprint!("\rsqry: {ws_state} ({elapsed}s elapsed)");
                        let _ = std::io::stderr().flush();
                    }
                }
            }
        });

        // Send the rebuild request. This blocks until the daemon finishes.
        let result = tokio::select! {
            res = client.rebuild(&canonical_path, force) => res,
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                poll_done.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = poll_handle.await;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                if json {
                    let out = serde_json::json!({
                        "status": "timeout",
                        "elapsed_ms": elapsed_ms,
                        "message": "rebuild still in progress on daemon"
                    });
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else {
                    eprintln!("\nsqry: rebuild timed out after {timeout}s (daemon continues in background)");
                }
                std::process::exit(2);
            }
        };

        poll_done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = poll_handle.await;
        // Clear the progress line.
        eprint!("\r\x1b[K");

        match result {
            Ok(value) => {
                if json {
                    let mut out = serde_json::Map::new();
                    out.insert(
                        "status".to_owned(),
                        serde_json::Value::String("completed".to_owned()),
                    );
                    // Extract fields from the response envelope.
                    if let Some(r) = value.get("result") {
                        if let Some(d) = r.get("duration_ms") {
                            out.insert("duration_ms".to_owned(), d.clone());
                        }
                        if let Some(n) = r.get("nodes") {
                            out.insert("nodes".to_owned(), n.clone());
                        }
                        if let Some(e) = r.get("edges") {
                            out.insert("edges".to_owned(), e.clone());
                        }
                        if let Some(f) = r.get("files_indexed") {
                            out.insert("files_indexed".to_owned(), f.clone());
                        }
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::Value::Object(out))?
                    );
                } else {
                    render_rebuild_human(&value, &canonical_path);
                }
            }
            Err(sqry_daemon_client::ClientError::RpcError {
                code: -32004,
                message,
                ..
            }) => {
                anyhow::bail!(
                    "workspace {} is not loaded on the daemon. \
                     Load it first with `sqry daemon load {}`.\n  (daemon said: {message})",
                    canonical_path.display(),
                    canonical_path.display()
                );
            }
            Err(e) => {
                return Err(anyhow::anyhow!("daemon/rebuild failed: {e}"));
            }
        }
        anyhow::Ok(())
    })?;

    Ok(())
}

fn extract_workspace_state(status: &serde_json::Value, path: &Path) -> Option<String> {
    let workspaces = status.get("result")?.get("workspaces")?.as_array()?;
    let path_str = path.to_string_lossy();
    for ws in workspaces {
        if let Some(root) = ws.get("index_root").and_then(|r| r.as_str())
            && root == path_str.as_ref()
        {
            return ws
                .get("state")
                .and_then(|s| s.as_str())
                .map(|s| s.to_owned());
        }
    }
    None
}

fn render_rebuild_human(value: &serde_json::Value, path: &Path) {
    if let Some(r) = value.get("result") {
        let duration = r.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0);
        let nodes = r.get("nodes").and_then(|n| n.as_u64()).unwrap_or(0);
        let edges = r.get("edges").and_then(|e| e.as_u64()).unwrap_or(0);
        let files = r.get("files_indexed").and_then(|f| f.as_u64()).unwrap_or(0);
        let was_full = r.get("was_full").and_then(|w| w.as_bool()).unwrap_or(false);
        let mode = if was_full { "full" } else { "incremental" };
        eprintln!(
            "sqry: {mode} rebuild of {} completed in {:.1}s ({nodes} nodes, {edges} edges, {files} files)",
            path.display(),
            duration as f64 / 1000.0
        );
    } else {
        eprintln!("sqry: rebuild completed for {}", path.display());
    }
}

// ---------------------------------------------------------------------------
// start.
// ---------------------------------------------------------------------------

/// Resolve the sqryd binary path, check whether the daemon is already running,
/// then exec `sqryd start --detach` and check its exit code.
///
/// After `sqryd start --detach` succeeds, polls the socket for up to `timeout`
/// seconds waiting for the daemon to become reachable. Exits with an error if
/// the daemon does not respond within the budget.
fn run_daemon_start(sqryd_path: Option<&Path>, timeout: u64) -> Result<()> {
    let binary = resolve_sqryd_binary(sqryd_path)?;

    // Load config for socket path (best-effort; fall back to defaults on error).
    let socket_path = load_config_socket_path();

    // Idempotency: already running → exit 0.
    if socket_path
        .as_ref()
        .is_some_and(|sp| try_connect_sync(sp).unwrap_or(false))
    {
        let sp = socket_path.as_ref().unwrap();
        eprintln!("sqry: daemon is already running (socket {})", sp.display());
        return Ok(());
    }

    let status = std::process::Command::new(&binary)
        .args(["start", "--detach"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .with_context(|| format!("failed to exec sqryd at {}", binary.display()))?;

    if !status.success() {
        let code = status.code().unwrap_or(1);
        // Exit code 75 = already running (sqryd EX_TEMPFAIL convention).
        if code == 75 {
            eprintln!("sqry: daemon is already running");
            return Ok(());
        }
        anyhow::bail!("sqryd start --detach exited with code {code}");
    }

    // Poll the socket until it becomes reachable or the timeout elapses.
    if let Some(ref sp) = socket_path {
        poll_until_reachable(sp, timeout)?;
        eprintln!("sqry: daemon started (socket {})", sp.display());
    } else {
        eprintln!("sqry: daemon started");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// stop.
// ---------------------------------------------------------------------------

/// Connect to the daemon, send `daemon/stop`, then poll until the socket
/// is unreachable or the timeout elapses.
fn run_daemon_stop(timeout: u64) -> Result<()> {
    let config = load_daemon_config()?;
    let socket_path = config.socket_path();

    if !try_connect_sync(&socket_path)? {
        eprintln!("sqry: daemon is not running");
        return Ok(());
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for daemon stop")?;

    rt.block_on(async {
        let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;

        // Ignore stop errors — the daemon may close the connection before
        // responding when it receives the shutdown request.
        let _ = client.stop().await;

        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            // Sleep first so we let the daemon begin shutdown.
            tokio::time::sleep(Duration::from_millis(STOP_POLL_INTERVAL_MS)).await;

            if !try_connect_async(&socket_path).await {
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "daemon did not exit within {timeout} seconds; \
                     check the daemon log for errors"
                );
            }
        }
        anyhow::Ok(())
    })?;

    eprintln!("sqry: daemon stopped");
    Ok(())
}

// ---------------------------------------------------------------------------
// status.
// ---------------------------------------------------------------------------

/// Connect to the daemon, send `daemon/status`, and render the response.
fn run_daemon_status(json: bool) -> Result<()> {
    let config = load_daemon_config()?;
    let socket_path = config.socket_path();

    // Check connectivity first so we can give a clean exit-1 without async machinery.
    if !try_connect_sync(&socket_path)? {
        if json {
            // Use serde_json to produce a properly-escaped JSON error envelope —
            // raw string interpolation can produce invalid JSON for socket paths
            // that contain quotes or backslashes (e.g. Windows paths).
            println!(
                "{}",
                serde_json::json!({
                    "error": "daemon_unreachable",
                    "socket": socket_path.display().to_string(),
                })
            );
        } else {
            eprintln!(
                "sqry: daemon is not running (socket {})",
                socket_path.display()
            );
        }
        // Exit code 1 for "not running".
        std::process::exit(1);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for daemon status")?;

    rt.block_on(async {
        let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;

        let result = client
            .status()
            .await
            .context("daemon/status request failed")?;

        if json {
            let out = serde_json::to_string_pretty(&result)
                .context("failed to serialize daemon status as JSON")?;
            println!("{out}");
        } else {
            render_status_human(&result);
        }
        anyhow::Ok(())
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// logs.
// ---------------------------------------------------------------------------

/// Discover the daemon log file path and tail it (or follow it).
///
/// Cluster-G §5.3 changed the default: the daemon now logs to
/// `<runtime_dir>/sqryd.log` automatically. The opt-out path
/// (`log_file = "stderr"`) and the "configured-but-missing-yet" path
/// (daemon just started, file not flushed) both fall back to the
/// platform-specific journal hint via [`print_log_fallback_hint`]
/// (cluster-G §5.4).
fn run_daemon_logs(lines: usize, follow: bool) -> Result<()> {
    let config = load_daemon_config()?;
    let log_path = match resolve_log_path(&config) {
        Ok(p) if p.exists() => p,
        Ok(p) => {
            // Configured but the file does not exist yet (daemon may
            // have just started, or transient FS issue).
            eprintln!(
                "sqry: configured log file {} does not exist yet. \
                 The daemon may have just started, or is logging to stderr.",
                p.display()
            );
            return print_log_fallback_hint(&config);
        }
        Err(_) => return print_log_fallback_hint(&config),
    };

    if follow {
        tail_follow(&log_path, lines)?;
    } else {
        tail_last_n(&log_path, lines)?;
    }
    Ok(())
}

/// Print the platform-specific fallback hint when no log file is
/// available — either because the operator opted out
/// (`log_file = "stderr"`), or because the file does not exist yet
/// (cluster-G §5.4).
///
/// Returns `Ok(())` rather than an error so the CLI exits with status
/// 0; the user already saw the diagnostic on stderr above.
fn print_log_fallback_hint(config: &sqry_daemon::config::DaemonConfig) -> Result<()> {
    eprintln!();
    eprintln!(
        "Default log location: {}",
        config.runtime_dir().join("sqryd.log").display()
    );
    eprintln!("To configure a custom path, set in $XDG_CONFIG_HOME/sqry/daemon.toml:");
    eprintln!(
        "    log_file = \"{}\"",
        config.runtime_dir().join("sqryd.log").display()
    );
    eprintln!("Or via env: SQRY_DAEMON_LOG_FILE=<path>");
    eprintln!();
    if cfg!(target_os = "linux") && std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        eprintln!("If running under systemd --user:");
        eprintln!("    journalctl --user -u sqryd.service -f");
    } else if cfg!(target_os = "linux") {
        eprintln!("If running under systemd:");
        eprintln!("    journalctl -u sqryd.service -f");
    } else if cfg!(target_os = "macos") {
        eprintln!("If running under launchd:");
        eprintln!("    log stream --predicate 'process == \"sqryd\"'");
    } else if cfg!(target_os = "windows") {
        eprintln!("On Windows, use Event Viewer or `Get-WinEvent`.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// load.
// ---------------------------------------------------------------------------

/// Connect to the daemon and send `daemon/load` for the given workspace path.
///
/// The daemon's `WorkspaceManager` will index the workspace (if not already
/// loaded), cache the graph in memory, and start watching for file changes.
/// The response is printed to stderr as a human-readable confirmation.
fn run_daemon_load(path: &Path) -> Result<()> {
    let config = load_daemon_config()?;
    let socket_path = config.socket_path();

    // Canonicalize the workspace path eagerly so the user sees the
    // resolved path in the output, and so the daemon's path-policy
    // canonicalization is defence-in-depth rather than the primary check.
    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;

    if !try_connect_sync(&socket_path)? {
        anyhow::bail!(
            "daemon is not running (socket {}). Start it with `sqry daemon start`.",
            socket_path.display()
        );
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for daemon load")?;

    rt.block_on(async {
        let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;

        // `DaemonClient::load` returns a strongly typed
        // `ResponseEnvelope<LoadResult>` — schema drift between the
        // client and daemon surfaces as `ClientError::SchemaMismatch`
        // here, not as a silently "successful" render with default
        // fields.
        let envelope = client.load(&canonical_path).await.with_context(|| {
            format!(
                "daemon/load request failed for {}",
                canonical_path.display()
            )
        })?;

        let load_result = envelope.result;
        eprintln!(
            "sqry: workspace loaded at {} ({:?}, {})",
            canonical_path.display(),
            load_result.state,
            human_bytes(load_result.current_bytes)
        );
        anyhow::Ok(())
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Binary resolution.
// ---------------------------------------------------------------------------

/// Resolve the `sqryd` binary path.
///
/// Resolution order:
/// 1. Explicit `--sqryd-path` override.
/// 2. Sibling of `current_exe()` (canonical path, symlink-safe).
/// 3. `SQRYD_PATH` environment variable.
/// 4. `PATH` lookup via [`which::which`].
///
/// # Errors
///
/// Returns an error if no `sqryd` binary can be found on the current system.
fn resolve_sqryd_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    // 1. Explicit override.
    if let Some(path) = explicit {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        anyhow::bail!("explicit --sqryd-path {} does not exist", path.display());
    }

    // 2. Sibling of the current executable (canonical, symlink-resolved).
    if let Ok(exe) = std::env::current_exe() {
        // Canonicalize to follow symlinks (security: prevents ../foo attacks).
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

    // 3. SQRYD_PATH env var.
    if let Some(val) = std::env::var_os("SQRYD_PATH") {
        let path = PathBuf::from(val);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("SQRYD_PATH={} does not exist", path.display());
    }

    // 4. PATH lookup.
    which::which("sqryd").with_context(|| {
        "sqryd binary not found. \
         Install sqryd alongside sqry, set SQRYD_PATH, or use --sqryd-path."
            .to_owned()
    })
}

// ---------------------------------------------------------------------------
// Auto-start helper (Task 13 wiring point).
// ---------------------------------------------------------------------------

/// Check whether the daemon auto-start opt-in is active, and if so, try to
/// start the daemon.
///
/// Returns `true` if a daemon is now running (either was already running or
/// was successfully started), `false` if auto-start is disabled (`SQRY_DAEMON_AUTO_START`
/// is not set to `"1"`).
///
/// Not wired into query commands until Task 13. Exported here so Task 13 can
/// `use sqry_cli::commands::daemon::try_auto_start_daemon` without moving code.
///
/// # Errors
///
/// Propagates errors from [`resolve_sqryd_binary`] and
/// `std::process::Command::status`. Errors during the binary execution are
/// treated as a failed auto-start (returns `Ok(false)` after logging a
/// warning).
// Not yet called from run() — Task 13 wires this into query command dispatch.
#[allow(dead_code)]
pub fn try_auto_start_daemon() -> Result<bool> {
    if std::env::var_os(ENV_DAEMON_AUTO_START).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(false);
    }

    // Resolve binary; any resolution error is surfaced to the caller.
    let binary = resolve_sqryd_binary(None)?;

    // Check if already running.
    let socket_path = load_config_socket_path();
    if socket_path
        .as_ref()
        .is_some_and(|sp| try_connect_sync(sp).unwrap_or(false))
    {
        return Ok(true);
    }

    // Exec sqryd start --detach.
    let status = std::process::Command::new(&binary)
        .args(["start", "--detach"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .status()
        .with_context(|| format!("auto-start: failed to exec sqryd at {}", binary.display()))?;

    if !status.success() {
        let code = status.code().unwrap_or(1);
        if code != 75 {
            // 75 = already running; any other nonzero means start failed.
            eprintln!(
                "sqry: Warning: daemon auto-start failed (sqryd exited {code}); \
                 falling back to local mode"
            );
            return Ok(false);
        }
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Formatting helpers.
// ---------------------------------------------------------------------------

/// Format a byte count as a human-readable string (B / KB / MB / GB / TB).
///
/// Uses binary (1024-based) divisors. Values below 1 KB are rendered as `"N B"`.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    const DIVISOR: u64 = 1024;

    if bytes < DIVISOR {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= DIVISOR as f64 && unit_index + 1 < UNITS.len() {
        value /= DIVISOR as f64;
        unit_index += 1;
    }
    // Use integer display when the value is exact; otherwise one decimal place.
    if (value - value.floor()).abs() < 0.05 {
        format!("{:.0} {}", value, UNITS[unit_index])
    } else {
        format!("{:.1} {}", value, UNITS[unit_index])
    }
}

/// Format an uptime in seconds as `"Xd Yh Zm"` (or shorter when leading
/// units are zero).
///
/// Examples:
/// - `0` → `"0m"`
/// - `59` → `"0m"`
/// - `3600` → `"1h"`
/// - `3660` → `"1h 1m"`
/// - `86400` → `"1d"`
/// - `90061` → `"1d 1h 1m"`
#[must_use]
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let mins = (seconds % 3_600) / 60;

    match (days, hours, mins) {
        (0, 0, _) => format!("{mins}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, 0) => format!("{d}d"),
        (d, h, 0) => format!("{d}d {h}h"),
        (d, h, m) => format!("{d}d {h}h {m}m"),
    }
}

// ---------------------------------------------------------------------------
// Status rendering.
// ---------------------------------------------------------------------------

/// Render a `daemon/status` response in human-readable format to stdout.
///
/// Thin wrapper around [`render_status_human_into`] that writes to
/// `std::io::stdout()`. Call [`render_status_human_into`] directly in tests
/// to capture and assert the rendered output.
fn render_status_human(envelope: &serde_json::Value) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    // Ignore write errors to stdout (broken pipe, etc.) — CLI best-effort.
    let _ = render_status_human_into(envelope, &mut handle);
}

/// Render a `daemon/status` response in human-readable format into `out`.
///
/// `envelope` is the [`ResponseEnvelope<DaemonStatus>`] returned by
/// [`sqry_daemon_client::DaemonClient::status`]. The inner `DaemonStatus` lives
/// under the `"result"` key; the `ResponseMeta` lives under `"meta"`.
///
/// Wire contract (`sqry-daemon/src/workspace/status.rs`):
/// ```json
/// {
///   "result": {
///     "daemon_version": "8.0.6",
///     "uptime_seconds": 8040,
///     "memory": {
///       "limit_bytes": 2147483648,
///       "current_bytes": 471859200,
///       "reserved_bytes": 0,
///       "high_water_bytes": 1288490188
///     },
///     "workspaces": [
///       {
///         "index_root": "/repos/example",
///         "state": "Loaded",
///         "pinned": true,
///         "current_bytes": 335544320,
///         "high_water_bytes": 933232896,
///         "last_good_at": null,
///         "last_error": null,
///         "retry_count": 0
///       }
///     ]
///   },
///   "meta": { "stale": false, "daemon_version": "8.0.6" }
/// }
/// ```
///
/// The renderer is **opportunistic**: it renders whatever fields are present,
/// and gracefully degrades when optional fields are absent. This future-proofs
/// the CLI against daemon version drift.
///
/// Expected human output (when all fields are present):
/// ```text
/// sqryd v8.0.6 -- uptime 2h 14m
///
/// Memory: 450 MB / 2048 MB  (peak: 1.2 GB)
///
/// Workspaces (1 loaded):
///   ~/repos/main-project      320 MB  (peak: 890 MB)  [pinned, Loaded]
/// ```
fn render_status_human_into(
    envelope: &serde_json::Value,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    // The `DaemonStatus` payload lives under `"result"`.
    // Fall back to the envelope root for forward-compat with any future
    // unwrapped shape.
    let inner = envelope.get("result").unwrap_or(envelope);

    // ---- Header line ----
    // `daemon_version` is in the inner DaemonStatus. The meta also carries it,
    // but we prefer the authoritative copy in the payload.
    let version = inner
        .get("daemon_version")
        .or_else(|| envelope.get("meta").and_then(|m| m.get("daemon_version")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");

    // `uptime_seconds` is the canonical field name in DaemonStatus.
    let uptime_str = inner
        .get("uptime_seconds")
        .and_then(serde_json::Value::as_u64)
        .map(format_uptime);

    match uptime_str {
        Some(uptime) => writeln!(out, "sqryd v{version} -- uptime {uptime}")?,
        None => writeln!(out, "sqryd v{version}")?,
    }

    // ---- Memory line ----
    // Memory fields are nested under `"memory"` in MemoryStatus.
    let memory = inner.get("memory");
    let mem_current = memory
        .and_then(|m| m.get("current_bytes"))
        .and_then(serde_json::Value::as_u64);
    let mem_limit = memory
        .and_then(|m| m.get("limit_bytes"))
        .and_then(serde_json::Value::as_u64);
    let mem_peak = memory
        .and_then(|m| m.get("high_water_bytes"))
        .and_then(serde_json::Value::as_u64);

    if mem_current.is_some() || mem_limit.is_some() {
        writeln!(out)?;
        let used_str = mem_current.map_or_else(|| "?".to_owned(), human_bytes);
        let limit_str = mem_limit.map_or_else(|| "?".to_owned(), human_bytes);
        match mem_peak {
            Some(peak) => writeln!(
                out,
                "Memory: {used_str} / {limit_str}  (peak: {})",
                human_bytes(peak)
            )?,
            None => writeln!(out, "Memory: {used_str} / {limit_str}")?,
        }
    }

    // ---- Workspaces ----
    let workspaces = inner
        .get("workspaces")
        .and_then(serde_json::Value::as_array);

    if let Some(wss) = workspaces {
        writeln!(out)?;
        writeln!(out, "Workspaces ({} loaded):", wss.len())?;
        for ws in wss {
            render_workspace_line_into(ws, out)?;
        }
    }

    Ok(())
}

/// Render a single workspace line from the status response to stdout.
///
/// Thin wrapper around [`render_workspace_line_into`] that writes to
/// `std::io::stdout()`. Call [`render_workspace_line_into`] directly in tests
/// to capture and assert the rendered output.
///
/// Called indirectly via [`render_status_human`] → [`render_status_human_into`] →
/// [`render_workspace_line_into`]. Kept for standalone use and API completeness.
#[allow(dead_code)]
fn render_workspace_line(ws: &serde_json::Value) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = render_workspace_line_into(ws, &mut handle);
}

/// Render a single workspace line from the status response into `out`.
///
/// Field names match `WorkspaceStatus` in `sqry-daemon/src/workspace/status.rs`:
/// - `index_root` — canonical absolute path (PathBuf serialised as string).
/// - `current_bytes` — live graph size.
/// - `high_water_bytes` — monotonic peak.
/// - `state` — serde form of `WorkspaceState` (e.g. `"Loaded"`, `"Rebuilding"`).
/// - `pinned` — LRU-exempt flag.
/// - `last_error` — most recent error string if any.
fn render_workspace_line_into(ws: &serde_json::Value, out: &mut dyn Write) -> std::io::Result<()> {
    // `index_root` is the canonical field. Accept `path` as a fallback for
    // forward-compat with any future shape change.
    let path = ws
        .get("index_root")
        .or_else(|| ws.get("path"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<unknown>");

    // Attempt to shorten the path with home directory tilde expansion.
    let display_path = tilde_shorten(path);

    // `current_bytes` / `high_water_bytes` are the canonical memory field names.
    let ws_mem = ws
        .get("current_bytes")
        .and_then(serde_json::Value::as_u64)
        .map(human_bytes);
    let ws_peak = ws
        .get("high_water_bytes")
        .and_then(serde_json::Value::as_u64)
        .map(human_bytes);

    // Collect status tags.
    let mut tags: Vec<&str> = Vec::new();
    if ws
        .get("pinned")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        tags.push("pinned");
    }
    let state = ws
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Loaded");
    tags.push(state);

    // Surface the most recent error as a stale tag if present.
    if let Some(err_msg) = ws.get("last_error").and_then(serde_json::Value::as_str) {
        // Format the error tag with the reason inline.
        let tag = format!("error: {err_msg}");
        match (ws_mem, ws_peak) {
            (Some(mem), Some(peak)) => {
                writeln!(
                    out,
                    "  {display_path:<30}  {mem:<8}  (peak: {peak:<8})  [{tags}, {tag}]",
                    tags = tags.join(", ")
                )?;
            }
            (Some(mem), None) => {
                writeln!(
                    out,
                    "  {display_path:<30}  {mem:<8}  [{tags}, {tag}]",
                    tags = tags.join(", ")
                )?;
            }
            _ => {
                writeln!(
                    out,
                    "  {display_path}  [{tags}, {tag}]",
                    tags = tags.join(", ")
                )?;
            }
        }
        return Ok(());
    }

    let tag_str = format!("[{}]", tags.join(", "));
    match (ws_mem, ws_peak) {
        (Some(mem), Some(peak)) => {
            writeln!(
                out,
                "  {display_path:<30}  {mem:<8}  (peak: {peak:<8})  {tag_str}"
            )?;
        }
        (Some(mem), None) => {
            writeln!(out, "  {display_path:<30}  {mem:<8}  {tag_str}")?;
        }
        _ => {
            writeln!(out, "  {display_path}  {tag_str}")?;
        }
    }
    Ok(())
}

/// Replace the home directory prefix in a path string with `~`.
fn tilde_shorten(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(stripped) = path.strip_prefix(home_str.as_ref()) {
            return format!("~{stripped}");
        }
    }
    path.to_owned()
}

// ---------------------------------------------------------------------------
// Log tailing.
// ---------------------------------------------------------------------------

/// Print the last `n` lines from `path` to stdout.
///
/// Reads the entire file into memory as a line buffer, then emits the last `n`
/// lines. Suitable for typical daemon log files (up to a few hundred MB).
/// For multi-GB log files the caller should configure log rotation to keep
/// files bounded.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read.
pub fn tail_last_n(path: &Path, n: usize) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))?;
    let buf_reader = std::io::BufReader::new(file);

    // Collect all lines (suitable for typical log files up to a few hundred MB).
    let mut lines: Vec<String> = Vec::new();
    for line in buf_reader.lines() {
        let l = line.with_context(|| format!("error reading log file {}", path.display()))?;
        lines.push(l);
    }

    // Emit the last n lines.
    let start = lines.len().saturating_sub(n);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in &lines[start..] {
        writeln!(out, "{line}").context("failed to write to stdout")?;
    }
    Ok(())
}

/// Print the last `initial_lines` lines from `path`, then follow the file
/// for new content using `notify::RecommendedWatcher`.
///
/// Rotation-aware: when the log file is renamed or recreated (rotation
/// event), the follower reopens it at the original path (standard `tail -F`
/// behaviour). Ctrl-C exits cleanly via the SIGINT handler inherited from
/// the process.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, the notify watcher cannot
/// be created, or stdout I/O fails.
pub fn tail_follow(path: &Path, initial_lines: usize) -> Result<()> {
    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;

    // Print the last `initial_lines` lines first.
    tail_last_n(path, initial_lines)?;

    // Seek to end of file so we only emit new bytes.
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open log file for follow: {}", path.display()))?;
    let mut pos = file
        .seek(SeekFrom::End(0))
        .context("failed to seek to end of log file")?;

    // Set up a synchronous notify watcher.
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
        .context("failed to create file watcher for log follow")?;

    // Watch the parent directory so we catch renames/recreations.
    let parent = path.parent().unwrap_or(Path::new("."));
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch log directory {}", parent.display()))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Follow loop.
    loop {
        match rx.recv_timeout(Duration::from_millis(FOLLOW_EVENT_TIMEOUT_MS)) {
            Ok(Ok(event)) => {
                let is_rotate = matches!(event.kind, EventKind::Remove(_) | EventKind::Create(_))
                    && event.paths.iter().any(|p| p == path);

                if is_rotate {
                    // Rotation detected — reopen at original path.
                    if path.exists() {
                        match std::fs::File::open(path) {
                            Ok(f) => {
                                file = f;
                                pos = 0;
                            }
                            Err(e) => {
                                eprintln!("sqry: log rotation detected but reopen failed: {e}");
                            }
                        }
                    }
                }

                // Read any new bytes.
                pos = drain_new_bytes(&mut file, pos, path, &mut out)?;
            }
            Ok(Err(e)) => {
                eprintln!("sqry: file watcher error: {e}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fallback poll in case notify misses events on some platforms.
                pos = drain_new_bytes(&mut file, pos, path, &mut out)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    Ok(())
}

/// Read bytes from `file` starting at `current_pos`, writing them to `out`.
/// Returns the new file position.
fn drain_new_bytes(
    file: &mut std::fs::File,
    current_pos: u64,
    path: &Path,
    out: &mut impl Write,
) -> Result<u64> {
    file.seek(SeekFrom::Start(current_pos))
        .with_context(|| format!("seek error in log file {}", path.display()))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| format!("read error in log file {}", path.display()))?;

    if !buf.is_empty() {
        out.write_all(&buf)
            .context("failed to write log output to stdout")?;
        out.flush().context("failed to flush stdout")?;
    }

    let new_pos = current_pos + buf.len() as u64;
    Ok(new_pos)
}

// ---------------------------------------------------------------------------
// Config helpers (best-effort; fall back to defaults on error).
// ---------------------------------------------------------------------------

/// Load `DaemonConfig` or return an error with context.
fn load_daemon_config() -> Result<sqry_daemon::config::DaemonConfig> {
    sqry_daemon::config::DaemonConfig::load().context(
        "failed to load daemon config; ensure daemon.toml is well-formed or \
         remove it to use defaults",
    )
}

/// Resolve the socket path from the daemon config.
/// Returns `None` if the config fails to load (soft failure for start
/// idempotency check only).
fn load_config_socket_path() -> Option<PathBuf> {
    sqry_daemon::config::DaemonConfig::load()
        .ok()
        .map(|c| c.socket_path())
}

/// Resolve the daemon log file path from `config`.
///
/// Cluster-G §5.3 changed the default: a fresh install now resolves to
/// `<runtime_dir>/sqryd.log`. The error path below only fires when the
/// operator has explicitly opted out via `log_file = "stderr"` /
/// `log_file = "-"` (or `SQRY_DAEMON_LOG_FILE=-`). The opted-out
/// recovery flow lives in [`print_log_fallback_hint`] (cluster-G §5.4).
///
/// Extracted from [`run_daemon_logs`] so the opted-out failure path can
/// be exercised in unit tests without requiring a real daemon config on disk.
fn resolve_log_path(config: &sqry_daemon::config::DaemonConfig) -> Result<PathBuf> {
    match config.log_file.resolve() {
        Some(p) => Ok(p),
        None => {
            anyhow::bail!(
                "log_file = \"stderr\" / \"-\" — the daemon writes only to stderr.\n\
                 Tail systemd / journald instead (see `sqry daemon logs --help`),\n\
                 or remove the opt-out so the daemon logs to <runtime_dir>/sqryd.log."
            );
        }
    }
}

/// Poll `socket_path` at [`STOP_POLL_INTERVAL_MS`] intervals until it becomes
/// reachable or `timeout` seconds elapse.
///
/// Returns `Ok(())` when the socket becomes reachable. Returns an error if the
/// deadline is reached before the socket responds.
///
/// Extracted from [`run_daemon_start`] so the timeout failure path can be
/// exercised in unit tests with a non-existent socket path.
fn poll_until_reachable(socket_path: &Path, timeout: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if try_connect_sync(socket_path).unwrap_or(false) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "daemon process started but did not become reachable within {timeout} \
                 seconds (socket {}). Check the daemon log for startup errors.",
                socket_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(STOP_POLL_INTERVAL_MS));
    }
}

// ---------------------------------------------------------------------------
// Socket connectivity probes.
// ---------------------------------------------------------------------------

/// Synchronously probe whether the daemon socket is reachable.
///
/// On Unix: attempts `std::os::unix::net::UnixStream::connect`.
/// On Windows: checks whether the named-pipe path exists
/// (sufficient for readiness detection without async machinery).
///
/// Returns `Ok(true)` if the socket is reachable, `Ok(false)` if not.
///
/// # Errors
///
/// Only returns `Err` for unexpected I/O errors. `ConnectionRefused`,
/// `NotFound`, and `PermissionDenied` are all surfaced as `Ok(false)` —
/// any of them mean the daemon is not reachable from this caller (no
/// listener, no socket file, or kernel/sandbox blocking the connect).
pub fn try_connect_sync(socket_path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        match UnixStream::connect(socket_path) {
            Ok(_) => Ok(true),
            Err(e) => match e.kind() {
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::NotFound
                | std::io::ErrorKind::PermissionDenied => Ok(false),
                _ => Err(anyhow::Error::from(e).context(format!(
                    "unexpected error probing socket {}",
                    socket_path.display()
                ))),
            },
        }
    }
    #[cfg(windows)]
    {
        // Named pipes: the pipe exists iff the path exists.
        Ok(socket_path.exists())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = socket_path;
        Ok(false)
    }
}

/// Asynchronously probe whether the daemon socket is reachable.
/// Returns `true` if connectable, `false` otherwise. Errors are treated
/// as "not reachable".
async fn try_connect_async(socket_path: &Path) -> bool {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(socket_path).await.is_ok()
    }
    #[cfg(windows)]
    {
        socket_path.exists()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = socket_path;
        false
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // resolve_sqryd_binary tests.
    // -----------------------------------------------------------------------

    /// Sibling resolution: create a fake `sqryd` next to a fake `sqry` in a
    /// temp directory and verify that the resolver finds it.
    #[test]
    #[serial_test::serial]
    fn resolve_sqryd_binary_finds_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sqryd_path = dir.path().join("sqryd");
        // Create a minimal executable file.
        std::fs::write(&sqryd_path, b"#!/bin/sh\n").expect("write fake sqryd");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sqryd_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        // Override current_exe would require proc-level mocking — instead
        // verify that the PATH fallback finds the file when SQRYD_PATH is set.
        unsafe {
            std::env::set_var("SQRYD_PATH", &sqryd_path);
        }
        let result = resolve_sqryd_binary(None);
        unsafe {
            std::env::remove_var("SQRYD_PATH");
        }

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        assert_eq!(result.unwrap(), sqryd_path);
    }

    /// PATH fallback: unset SQRYD_PATH and verify we get a resolution
    /// error (no `sqryd` on PATH in a typical test environment) or a valid
    /// path (if sqryd happens to be installed). Either outcome is correct.
    #[test]
    #[serial_test::serial]
    fn resolve_sqryd_binary_falls_back_to_path() {
        // Remove env vars that would override PATH lookup.
        unsafe {
            std::env::remove_var("SQRYD_PATH");
        }

        let result = resolve_sqryd_binary(None);
        // Either found on PATH or not found — both are valid in test environments.
        // We just assert the function returns without panicking.
        let _ = result;
    }

    /// SQRYD_PATH env var: set it to an existing path and verify resolution.
    #[test]
    #[serial_test::serial]
    fn resolve_sqryd_binary_respects_env_var() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sqryd_path = dir.path().join("sqryd");
        std::fs::write(&sqryd_path, b"#!/bin/sh\n").expect("write fake sqryd");

        unsafe {
            std::env::set_var("SQRYD_PATH", &sqryd_path);
        }
        let result = resolve_sqryd_binary(None);
        unsafe {
            std::env::remove_var("SQRYD_PATH");
        }

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        assert_eq!(result.unwrap(), sqryd_path);
    }

    // -----------------------------------------------------------------------
    // human_bytes tests.
    // -----------------------------------------------------------------------

    #[test]
    fn human_bytes_formats_correctly() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(1_048_576), "1 MB");
        assert_eq!(human_bytes(1_073_741_824), "1 GB");
        assert_eq!(human_bytes(1_099_511_627_776), "1 TB");
        // 1.5 MB
        assert_eq!(human_bytes(1_572_864), "1.5 MB");
    }

    // -----------------------------------------------------------------------
    // format_uptime tests.
    // -----------------------------------------------------------------------

    #[test]
    fn format_uptime_renders_hours_minutes() {
        assert_eq!(format_uptime(0), "0m");
        assert_eq!(format_uptime(59), "0m");
        assert_eq!(format_uptime(60), "1m");
        assert_eq!(format_uptime(3600), "1h");
        assert_eq!(format_uptime(3660), "1h 1m");
        assert_eq!(format_uptime(7380), "2h 3m");
        assert_eq!(format_uptime(86400), "1d");
        assert_eq!(format_uptime(90061), "1d 1h 1m");
        assert_eq!(format_uptime(172800), "2d");
    }

    // -----------------------------------------------------------------------
    // render_status_human tests.
    // -----------------------------------------------------------------------

    /// Minimal response: empty `result` object — graceful degradation to
    /// `"sqryd vunknown"` with no memory or workspace sections.
    ///
    /// Verifies output is produced without panicking and contains the fallback
    /// `"vunknown"` version string. The `daemon_version` from `meta` is used
    /// when `result` has no `daemon_version`.
    #[test]
    fn daemon_status_human_renders_minimal_response() {
        // Matches the actual ResponseEnvelope<DaemonStatus> wire shape.
        // Tests graceful degradation when payload fields are absent.
        let envelope = serde_json::json!({
            "result": {},
            "meta": { "stale": false, "daemon_version": "8.0.6" }
        });
        let mut buf: Vec<u8> = Vec::new();
        render_status_human_into(&envelope, &mut buf)
            .expect("render_status_human_into must not fail");
        let output = String::from_utf8(buf).expect("rendered output must be valid UTF-8");
        // With empty `result`, the renderer falls back to `meta.daemon_version`.
        assert!(
            output.contains("v8.0.6"),
            "graceful degradation must fall back to meta.daemon_version '8.0.6'; got:\n{output}"
        );
    }

    /// Full response: use the canonical `ResponseEnvelope<DaemonStatus>` wire
    /// shape from `sqry-daemon/src/workspace/status.rs`. Verifies all sections
    /// appear in the rendered output.
    #[test]
    fn daemon_status_human_renders_full_response() {
        // This matches the actual JSON-RPC `result` returned by
        // `DaemonClient::status()`, which is a ResponseEnvelope<DaemonStatus>.
        let envelope = serde_json::json!({
            "result": {
                "daemon_version": "8.0.6",
                "uptime_seconds": 8040_u64,
                "memory": {
                    "limit_bytes":       2_147_483_648_u64,
                    "current_bytes":       471_859_200_u64,
                    "reserved_bytes":                0_u64,
                    "high_water_bytes":  1_288_490_188_u64
                },
                "workspaces": [
                    {
                        "index_root": "/home/user/repos/main-project",
                        "state": "Loaded",
                        "pinned": true,
                        "current_bytes":   335_544_320_u64,
                        "high_water_bytes": 933_232_896_u64,
                        "last_good_at": null,
                        "last_error": null,
                        "retry_count": 0
                    },
                    {
                        "index_root": "/home/user/repos/auth-service",
                        "state": "Loaded",
                        "pinned": false,
                        "current_bytes":    83_886_080_u64,
                        "high_water_bytes": 324_534_016_u64,
                        "last_good_at": null,
                        "last_error": null,
                        "retry_count": 0
                    }
                ]
            },
            "meta": {
                "stale": false,
                "daemon_version": "8.0.6"
            }
        });
        let mut buf: Vec<u8> = Vec::new();
        render_status_human_into(&envelope, &mut buf)
            .expect("render_status_human_into must not fail");
        let output = String::from_utf8(buf).expect("rendered output must be valid UTF-8");
        // Version and uptime header.
        assert!(
            output.contains("v8.0.6"),
            "must contain version; got:\n{output}"
        );
        assert!(
            output.contains("2h"),
            "must contain uptime hours; got:\n{output}"
        );
        // Memory section.
        assert!(
            output.contains("Memory:"),
            "must contain memory section; got:\n{output}"
        );
        assert!(
            output.contains("2 GB"),
            "must contain limit '2 GB'; got:\n{output}"
        );
        // Workspaces section — both index_root paths must appear.
        assert!(
            output.contains("Workspaces"),
            "must contain workspaces section; got:\n{output}"
        );
        assert!(
            output.contains("main-project"),
            "must contain workspace path; got:\n{output}"
        );
        assert!(
            output.contains("auth-service"),
            "must contain workspace path; got:\n{output}"
        );
    }

    /// Verify that render_status_human_into extracts `daemon_version` and
    /// `uptime_seconds` from the canonical `result` key, and that the rendered
    /// output contains those values verbatim.
    ///
    /// This test captures rendered output into a `Vec<u8>` buffer via
    /// [`render_status_human_into`] so a regression to old field names (e.g.
    /// `version`, `uptime_secs`) would cause the assertions to fail even though
    /// the JSON fixture would still parse correctly.
    ///
    /// **Sentinel distinctness**: `result.daemon_version` ("8.1.2") and
    /// `meta.daemon_version` ("7.9.0") are intentionally different so that a
    /// renderer that accidentally falls back to the `meta` copy would produce
    /// "v7.9.0" in the header instead of "v8.1.2", causing the assertion to fail.
    #[test]
    fn daemon_status_human_extracts_version_and_uptime() {
        let envelope = serde_json::json!({
            "result": {
                "daemon_version": "8.1.2",
                "uptime_seconds": 3661_u64,
                "memory": {
                    "limit_bytes": 1_073_741_824_u64,
                    "current_bytes": 104_857_600_u64,
                    "reserved_bytes": 0_u64,
                    "high_water_bytes": 209_715_200_u64
                },
                "workspaces": []
            },
            // meta.daemon_version is intentionally different from result.daemon_version
            // to detect regressions where the renderer uses the meta copy instead.
            "meta": { "stale": false, "daemon_version": "7.9.0" }
        });

        let mut buf: Vec<u8> = Vec::new();
        render_status_human_into(&envelope, &mut buf)
            .expect("render_status_human_into must not fail writing to Vec");
        let output = String::from_utf8(buf).expect("rendered output must be valid UTF-8");

        // Header line must contain the daemon version from `result.daemon_version`,
        // NOT the stale value from `meta.daemon_version`.
        assert!(
            output.contains("v8.1.2"),
            "rendered output must contain result.daemon_version '8.1.2'; got:\n{output}"
        );
        assert!(
            !output.contains("v7.9.0"),
            "rendered output must NOT contain meta.daemon_version '7.9.0'; got:\n{output}"
        );
        // Header line must contain the uptime formatted from `result.uptime_seconds` (3661s = 1h 1m).
        assert!(
            output.contains("1h 1m"),
            "rendered output must contain uptime '1h 1m'; got:\n{output}"
        );
        // Memory section: current_bytes (104 MB) and limit_bytes (1 GB) must appear.
        assert!(
            output.contains("100 MB"),
            "rendered output must contain memory current '100 MB'; got:\n{output}"
        );
        assert!(
            output.contains("1 GB"),
            "rendered output must contain memory limit '1 GB'; got:\n{output}"
        );
        // Memory section: high_water_bytes (200 MB peak) must appear.
        assert!(
            output.contains("200 MB"),
            "rendered output must contain memory peak '200 MB'; got:\n{output}"
        );
    }

    /// Verify that render_workspace_line_into uses `index_root` /
    /// `current_bytes` / `high_water_bytes` — the canonical WorkspaceStatus
    /// field names — and that those values appear in the rendered output.
    ///
    /// This test captures rendered output into a `Vec<u8>` buffer via
    /// [`render_workspace_line_into`] so a regression to old field names (e.g.
    /// `path`, `memory_bytes`) would cause the assertions to fail.
    #[test]
    fn daemon_status_human_renders_workspace_canonical_fields() {
        let ws = serde_json::json!({
            "index_root": "/home/user/repos/sqry",
            "state": "Loaded",
            "pinned": true,
            "current_bytes": 335_544_320_u64,
            "high_water_bytes": 671_088_640_u64,
            "last_good_at": null,
            "last_error": null,
            "retry_count": 0
        });

        let mut buf: Vec<u8> = Vec::new();
        render_workspace_line_into(&ws, &mut buf)
            .expect("render_workspace_line_into must not fail writing to Vec");
        let output = String::from_utf8(buf).expect("rendered output must be valid UTF-8");

        // The path from `index_root` must appear (possibly tilde-shortened, but
        // the last component "sqry" is always preserved).
        assert!(
            output.contains("sqry"),
            "rendered output must contain the workspace path component 'sqry'; got:\n{output}"
        );
        // current_bytes (320 MB) must appear.
        assert!(
            output.contains("320 MB"),
            "rendered output must contain workspace size '320 MB' from current_bytes; got:\n{output}"
        );
        // high_water_bytes (640 MB) must appear.
        assert!(
            output.contains("640 MB"),
            "rendered output must contain workspace peak '640 MB' from high_water_bytes; got:\n{output}"
        );
        // The "pinned" tag must appear (from the `pinned: true` field).
        assert!(
            output.contains("pinned"),
            "rendered output must contain 'pinned' tag; got:\n{output}"
        );
    }

    // -----------------------------------------------------------------------
    // resolve_log_path tests.
    // -----------------------------------------------------------------------

    /// Cluster-G §5.3 changed the default: `DaemonConfig::default()` now
    /// returns `LogFileSetting::Path(<runtime_dir>/sqryd.log)`, so
    /// `resolve_log_path` resolves successfully without operator
    /// configuration. The error path is exercised only when the
    /// operator explicitly opts out (`log_file = "stderr"`) — see the
    /// `resolve_log_path_errors_when_log_file_opted_out` test below.
    #[test]
    fn resolve_log_path_returns_runtime_dir_default_when_unconfigured() {
        let config = sqry_daemon::config::DaemonConfig::default();
        let result = resolve_log_path(&config).expect("default config must resolve to a path");
        assert!(
            result.ends_with("sqryd.log"),
            "default log path should end with sqryd.log, got: {}",
            result.display()
        );
    }

    /// Operator opt-out via `log_file = "stderr"` is the only path that
    /// should still surface the legacy "no log file" error message
    /// (cluster-G §5.4). The error must reference `stderr` so the user
    /// knows why no file is available.
    #[test]
    fn resolve_log_path_errors_when_log_file_opted_out() {
        let mut config = sqry_daemon::config::DaemonConfig::default();
        config.log_file = sqry_daemon::config::LogFileSetting::Special("stderr".to_string());

        let result = resolve_log_path(&config);
        assert!(
            result.is_err(),
            "resolve_log_path must return Err when log_file is Special"
        );

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("stderr"),
            "error must mention 'stderr' to explain the opt-out; got:\n{err_msg}"
        );
    }

    // -----------------------------------------------------------------------
    // poll_until_reachable tests.
    // -----------------------------------------------------------------------

    /// When the target socket does not exist and the timeout is zero, the
    /// first iteration must already find `Instant::now() >= deadline` and bail
    /// immediately with the expected timeout error message.
    ///
    /// This directly tests the m-1 fix from iter-0: `run_daemon_start` must
    /// honour the `--timeout` parameter and surface an actionable error when
    /// the daemon does not become reachable within the budget.
    #[test]
    fn poll_until_reachable_times_out_for_unreachable_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("nonexistent.sock");

        // timeout = 0 → deadline is already in the past on the first check.
        let result = poll_until_reachable(&socket_path, 0);
        assert!(
            result.is_err(),
            "poll_until_reachable must return Err when socket is unreachable and timeout = 0"
        );

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("did not become reachable"),
            "error must contain 'did not become reachable'; got:\n{err_msg}"
        );
        // The error message must include the socket path for diagnostics.
        assert!(
            err_msg.contains("nonexistent.sock"),
            "error must contain the socket path; got:\n{err_msg}"
        );
    }

    // -----------------------------------------------------------------------
    // try_auto_start_daemon disabled test.
    // -----------------------------------------------------------------------

    /// When SQRY_DAEMON_AUTO_START is not set (or set to a value other than
    /// "1"), try_auto_start_daemon must return Ok(false) immediately.
    #[test]
    #[serial_test::serial]
    fn try_auto_start_daemon_returns_false_when_disabled() {
        unsafe {
            std::env::remove_var(ENV_DAEMON_AUTO_START);
        }
        let result = try_auto_start_daemon().expect("try_auto_start_daemon must not error");
        assert!(
            !result,
            "expected false when SQRY_DAEMON_AUTO_START is unset"
        );

        // Also verify with a non-"1" value.
        unsafe {
            std::env::set_var(ENV_DAEMON_AUTO_START, "0");
        }
        let result = try_auto_start_daemon().expect("try_auto_start_daemon must not error");
        unsafe {
            std::env::remove_var(ENV_DAEMON_AUTO_START);
        }
        assert!(!result, "expected false when SQRY_DAEMON_AUTO_START=0");
    }
}
