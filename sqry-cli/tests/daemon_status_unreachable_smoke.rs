//! Audit C083b (cli-help-impl-alignment-2026-05-04) — smoke test for the
//! JSON envelope emitted by `sqry daemon status --json` when no daemon is
//! running.
//!
//! Companion to `daemon_subcommands::daemon_status_when_not_running_exits_nonzero`,
//! which only covered the human-readable stderr path. This test exercises
//! the JSON-output path (`--json`) and asserts the structured error envelope
//! contract callers / scripts depend on:
//!
//! ```json
//! {
//!     "error": "daemon_unreachable",
//!     "socket": "<path that was probed>"
//! }
//! ```
//!
//! Pre-fix, the unreachable-daemon JSON path printed a literal `{}` which
//! gave callers no machine-readable diagnostic to branch on. Post-fix
//! (audit C083a in `sqry-cli/src/commands/daemon.rs`), the envelope is
//! produced via `serde_json::json!` so paths containing quotes / backslashes
//! are correctly escaped.
//!
//! # Hermetic isolation
//!
//! - Uses an isolated `tempfile::TempDir` for the socket path so the test
//!   never finds the user's real running daemon.
//! - Writes a minimal `daemon.toml` pointing the socket at a path inside
//!   that tempdir which will never exist.
//! - Sets `SQRY_DAEMON_CONFIG` so the CLI loads the test config rather
//!   than the user's `~/.config/sqry/daemon.toml`.
//! - Sets `XDG_RUNTIME_DIR` to the tempdir as a belt-and-suspenders against
//!   the platform-default runtime-dir resolver.
//!
//! # Skipping
//!
//! Unix-only: the unreachable-socket probe `try_connect_sync` uses
//! `UnixStream::connect`. Windows uses a named-pipe existence check, which
//! is covered by parallel infrastructure but not this smoke test.

#![cfg(unix)]

mod common;
use common::sqry_bin;

use std::path::Path;
use std::process::{Command, Stdio};

/// Write a minimal daemon config TOML to `config_path`, pointing the socket
/// to `socket_path`. Mirrors the helper in `daemon_subcommands.rs`; the
/// duplication is intentional — each integration-test file is its own
/// compilation unit and we keep this file standalone (no `mod common`
/// helper for daemon configs to keep the smoke test surface minimal).
fn write_daemon_config(config_path: &Path, socket_path: &Path, runtime_dir: &Path) {
    let contents = format!(
        "[socket]\npath = {:?}\n",
        socket_path.to_string_lossy().as_ref()
    );
    std::fs::write(config_path, &contents)
        .unwrap_or_else(|e| panic!("write daemon config TOML to {}: {e}", config_path.display()));
    let sqry_runtime = runtime_dir.join("sqry");
    std::fs::create_dir_all(&sqry_runtime)
        .unwrap_or_else(|e| panic!("create runtime dir {}: {e}", sqry_runtime.display()));
}

/// Run `sqry daemon status --json` against a non-existent socket and verify
/// the documented JSON error envelope:
///   - exit code: 1
///   - stdout: valid JSON
///   - JSON `error` field: `"daemon_unreachable"`
///   - JSON `socket` field: a non-empty string echoing the probed path
#[test]
fn daemon_status_json_unreachable_emits_error_envelope() {
    let tmp = tempfile::TempDir::new().expect("create tempdir for daemon status smoke");
    let socket_path = tmp.path().join("sqryd-unreachable-smoke.sock");
    let config_path = tmp.path().join("daemon.toml");

    // Sanity: the socket must NOT exist — that's the whole point of the test.
    assert!(
        !socket_path.exists(),
        "tempdir socket {} unexpectedly exists at test start",
        socket_path.display()
    );

    write_daemon_config(&config_path, &socket_path, tmp.path());

    let sqry = sqry_bin();
    let output = Command::new(&sqry)
        .args(["daemon", "status", "--json"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `sqry daemon status --json`: {e}"));

    // 1. Exit code: 1 (the documented "not running" code).
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit code 1 (daemon not running); got {:?}\n\
         stdout: {}\n\
         stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // 2. Stdout must parse as JSON.
    let stdout = String::from_utf8(output.stdout.clone())
        .expect("`sqry daemon status --json` produced non-UTF-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "`sqry daemon status --json` stdout did not parse as JSON: {e}\n\
             stdout was: {stdout:?}"
        )
    });

    // 3. The `error` field must equal the documented constant.
    let error_field = parsed
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("missing or non-string `error` field in JSON: {parsed}"));
    assert_eq!(
        error_field, "daemon_unreachable",
        "unexpected `error` value in JSON envelope: {parsed}"
    );

    // 4. The `socket` field must be a non-empty string echoing the probed path.
    let socket_field = parsed
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("missing or non-string `socket` field in JSON: {parsed}"));
    assert!(
        !socket_field.is_empty(),
        "`socket` field must be non-empty; full envelope: {parsed}"
    );
    // The path in the envelope must reference the tempdir socket we configured.
    // Use Path::ends_with on file_name to be robust against any path normalization
    // the daemon-config loader may apply (canonical vs literal).
    let socket_basename = Path::new(socket_field)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    assert_eq!(
        socket_basename, "sqryd-unreachable-smoke.sock",
        "`socket` field {socket_field:?} did not echo the configured tempdir socket path"
    );
}
