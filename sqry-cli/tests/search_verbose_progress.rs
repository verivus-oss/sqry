//! Integration tests for the `--verbose` / `SQRY_LOG=info` search-path
//! progress UX shipped by verivus-oss/sqry#238 tier-1.
//!
//! Six contract cases per the DAG `SEARCH_INTEGRATION_TEST` acceptance list
//! at `docs/superpowers/plans/2026-05-11-issue-238-search-progress-ux-dag.toml`:
//!
//!   (a) `sqry search <pat> --verbose` emits `[sqry] load snapshot` +
//!       `complete in ` to stderr.
//!   (b) `sqry --semantic --exact <pat> --verbose` (shorthand) emits
//!       `[sqry] exact name lookup` to stderr.
//!   (c) `SQRY_LOG=info sqry --exact <pat>` (no `--verbose`) emits the
//!       same stages as (a) — env-driven enablement.
//!   (d) `sqry search <pat>` (no verbose, no env) emits NO `[sqry] ...`
//!       lines to stderr — silent-default regression guard.
//!   (e) DAEMON_FALLBACK_DIAGNOSTIC: with a mock UDS listener bound at
//!       `SQRY_DAEMON_SOCKET`, verbose mode emits a `note: sqryd is
//!       running ...` line; without the listener bound, no such line.
//!   (f) `SQRY_OUTPUT_FORMAT=json sqry search <pat> --verbose` emits one
//!       JSON object per line, each a valid stage event (no daemon
//!       diagnostic mixed in when daemon socket is unreachable).
//!
//! Tests share a single indexed tempdir via `OnceLock` so the (~hundreds of
//! ms) `sqry index` cost is amortized across all six cases.

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Shared indexed workspace
// --------------------------------------------------------------------------

/// One-time setup: create a tempdir with a tiny rust source file, run
/// `sqry index` against it, return the dir. Cached via OnceLock so all six
/// tests share the same indexed workspace.
///
/// We hold the TempDir inside the OnceLock so the directory survives for
/// the lifetime of the test binary; dropping it would unlink the snapshot
/// mid-run.
fn shared_workspace() -> &'static Path {
    static WORKSPACE: OnceLock<TempDir> = OnceLock::new();
    // Serialize `sqry index` startup so concurrent first-callers don't
    // race; OnceLock guarantees only one initializer runs, but the cargo
    // test harness can have multiple threads call `shared_workspace` at
    // the same time and we want each to wait cleanly.
    static SETUP_LOCK: Mutex<()> = Mutex::new(());
    let _guard = SETUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    WORKSPACE
        .get_or_init(|| {
            let dir = tempfile::tempdir().expect("create tempdir");
            std::fs::write(
                dir.path().join("hello.rs"),
                "fn hello_search_target() {}\nfn world_search_target() {}\n",
            )
            .expect("write source file");

            let mut cmd = Command::new(sqry_bin());
            cmd.arg("index")
                .arg(dir.path())
                // Point SQRY_DAEMON_SOCKET at a known-unreachable path so
                // the indexer itself doesn't accidentally trigger any
                // daemon-related env detection.
                .env(
                    "SQRY_DAEMON_SOCKET",
                    "/nonexistent/sqry-index-bootstrap.sock",
                );
            cmd.assert().success();

            dir
        })
        .path()
}

/// Returns a `Command` for `sqry` pre-pointed at the shared workspace, with
/// the daemon-socket env scrubbed to a known-unreachable path so per-test
/// daemon-diagnostic behaviour is deterministic (tests that want the
/// diagnostic re-set the env explicitly).
fn sqry_cmd() -> Command {
    let mut cmd = Command::new(sqry_bin());
    cmd.env("SQRY_DAEMON_SOCKET", "/nonexistent/sqry-test-default.sock");
    cmd
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// --------------------------------------------------------------------------
// Case (a) — explicit subcommand --verbose emits load snapshot stage
// --------------------------------------------------------------------------

#[test]
fn verbose_subcommand_emits_load_snapshot_stage() {
    let workspace = shared_workspace();
    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        stderr.contains("[sqry] load snapshot"),
        "missing `[sqry] load snapshot` line in stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("complete in "),
        "missing `complete in` line in stderr; got: {stderr}"
    );
}

// --------------------------------------------------------------------------
// Case (b) — shorthand --exact --verbose emits exact-name-lookup stage
// --------------------------------------------------------------------------

#[test]
fn verbose_shorthand_exact_emits_exact_name_lookup_stage() {
    let workspace = shared_workspace();
    let assert = sqry_cmd()
        .arg("--exact")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        stderr.contains("[sqry] exact name lookup"),
        "missing `[sqry] exact name lookup` line in stderr; got: {stderr}"
    );
}

// --------------------------------------------------------------------------
// Case (c) — SQRY_LOG=info enables verbose without the --verbose flag
// --------------------------------------------------------------------------

#[test]
fn sqry_log_info_enables_verbose_without_flag() {
    let workspace = shared_workspace();
    let assert = sqry_cmd()
        .arg("--exact")
        .arg("hello_search_target")
        .arg(workspace)
        .env("SQRY_LOG", "info")
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        stderr.contains("[sqry] load snapshot"),
        "SQRY_LOG=info should produce `[sqry] load snapshot`; got: {stderr}"
    );
    assert!(
        stderr.contains("[sqry] exact name lookup"),
        "SQRY_LOG=info should produce `[sqry] exact name lookup`; got: {stderr}"
    );
}

// --------------------------------------------------------------------------
// Case (d) — silent default regression guard
// --------------------------------------------------------------------------

#[test]
fn silent_default_emits_no_stage_lines() {
    let workspace = shared_workspace();
    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        // Explicitly scrub the env so an inherited SQRY_LOG / RUST_LOG
        // from the CI shell doesn't enable verbose under our feet.
        .env_remove("SQRY_LOG")
        .env_remove("RUST_LOG")
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        !stderr.contains("[sqry]"),
        "silent default leaked an `[sqry]` line in stderr; got: {stderr}"
    );
    assert!(
        !stderr.contains("\"event\":"),
        "silent default leaked a JSON-line event in stderr; got: {stderr}"
    );
}

// --------------------------------------------------------------------------
// Case (e) — daemon-fallback diagnostic with a mock UDS listener
// --------------------------------------------------------------------------

/// Spawns a tokio-driven mock that mimics only sqryd's `DaemonHello`
/// handshake on a UDS at `socket_path`. The mock accepts exactly one
/// connection, reads the client's `DaemonHello`, replies with a compatible
/// `DaemonHelloResponse`, and closes.
///
/// Returns the join handle so the test can wait for the mock to complete
/// (or drop it implicitly when the test ends).
#[cfg(unix)]
fn spawn_hello_only_mock(socket_path: std::path::PathBuf) -> std::thread::JoinHandle<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock tokio runtime");
        rt.block_on(async move {
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind mock socket");
            ready_tx.send(()).expect("send mock ready");
            // We only need to handle a single hello-handshake — the
            // diagnostic probes once per process. Bound on a generous
            // overall timeout so a hang here surfaces as a test failure
            // rather than indefinitely.
            let accept_fut = listener.accept();
            let (mut stream, _addr) =
                match tokio::time::timeout(std::time::Duration::from_secs(5), accept_fut).await {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => panic!("mock accept failed: {e}"),
                    Err(_) => panic!("mock accept timed out — diagnostic probe never connected"),
                };
            // Read the client's hello, reply, then drop the stream.
            // `read_frame_json` returns `Result<Option<T>>` — `None`
            // signals a clean EOF, which the test should treat as a
            // failed handshake.
            let _hello: sqry_daemon_protocol::DaemonHello =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read DaemonHello (transport)")
                    .expect("DaemonHello frame must be present");
            let response = sqry_daemon_protocol::DaemonHelloResponse {
                compatible: true,
                daemon_version: "mock-0.0.0".to_string(),
                envelope_version: 1,
            };
            sqry_daemon_protocol::framing::write_frame_json(&mut stream, &response)
                .await
                .expect("write DaemonHelloResponse");
        });
    });
    ready_rx.recv().expect("mock socket must bind");
    handle
}

/// Spawns a tokio-driven mock that completes both sqryd's hello handshake
/// and one `daemon/status` JSON-RPC request. The Tier-1 diagnostic uses
/// `DaemonClient::status()` as the reachability proof, so hello alone must
/// not be enough to fire the line.
#[cfg(unix)]
fn spawn_status_mock(socket_path: std::path::PathBuf) -> std::thread::JoinHandle<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock tokio runtime");
        rt.block_on(async move {
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind mock socket");
            ready_tx.send(()).expect("send mock ready");
            let accept_fut = listener.accept();
            let (mut stream, _addr) =
                match tokio::time::timeout(std::time::Duration::from_secs(5), accept_fut).await {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => panic!("mock accept failed: {e}"),
                    Err(_) => panic!("mock accept timed out — diagnostic probe never connected"),
                };

            let _hello: sqry_daemon_protocol::DaemonHello =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read DaemonHello (transport)")
                    .expect("DaemonHello frame must be present");
            let response = sqry_daemon_protocol::DaemonHelloResponse {
                compatible: true,
                daemon_version: "mock-0.0.0".to_string(),
                envelope_version: 1,
            };
            sqry_daemon_protocol::framing::write_frame_json(&mut stream, &response)
                .await
                .expect("write DaemonHelloResponse");

            let request: sqry_daemon_protocol::JsonRpcRequest =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read daemon/status request (transport)")
                    .expect("daemon/status frame must be present");
            assert_eq!(
                request.method, "daemon/status",
                "diagnostic probe must prove daemon reachability via daemon/status"
            );
            let status_response = sqry_daemon_protocol::JsonRpcResponse::success(
                request.id,
                serde_json::json!({
                    "uptime_seconds": 1,
                    "daemon_version": "mock-0.0.0",
                    "memory": {
                        "limit_bytes": 0,
                        "current_bytes": 0,
                        "reserved_bytes": 0,
                        "high_water_bytes": 0
                    },
                    "workspaces": []
                }),
            );
            sqry_daemon_protocol::framing::write_frame_json(&mut stream, &status_response)
                .await
                .expect("write daemon/status response");
        });
    });
    ready_rx.recv().expect("mock socket must bind");
    handle
}

#[cfg(unix)]
enum SearchShimStallPoint {
    Status,
    Search,
}

/// Spawns a daemon-shaped listener for the CLI daemon-search shim. It completes
/// the hello handshake, then either stalls on `daemon/status` or returns a
/// Loaded status and stalls on `daemon/search`.
#[cfg(unix)]
fn spawn_stalling_daemon_search_mock(
    socket_path: std::path::PathBuf,
    workspace_root: PathBuf,
    stall_point: SearchShimStallPoint,
) -> std::thread::JoinHandle<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock tokio runtime");
        rt.block_on(async move {
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind mock socket");
            ready_tx.send(()).expect("send mock ready");
            let (mut stream, _addr) =
                match tokio::time::timeout(Duration::from_secs(5), listener.accept()).await {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => panic!("mock accept failed: {e}"),
                    Err(_) => panic!("mock accept timed out — daemon shim never connected"),
                };

            let _hello: sqry_daemon_protocol::DaemonHello =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read DaemonHello (transport)")
                    .expect("DaemonHello frame must be present");
            let response = sqry_daemon_protocol::DaemonHelloResponse {
                compatible: true,
                daemon_version: "mock-0.0.0".to_string(),
                envelope_version: 1,
            };
            sqry_daemon_protocol::framing::write_frame_json(&mut stream, &response)
                .await
                .expect("write DaemonHelloResponse");

            let status_request: sqry_daemon_protocol::JsonRpcRequest =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read daemon/status request (transport)")
                    .expect("daemon/status frame must be present");
            assert_eq!(
                status_request.method, "daemon/status",
                "daemon-search shim must gate route selection through daemon/status"
            );

            if matches!(stall_point, SearchShimStallPoint::Status) {
                tokio::time::sleep(Duration::from_millis(750)).await;
                return;
            }

            let loaded_status_response = sqry_daemon_protocol::JsonRpcResponse::success(
                status_request.id,
                serde_json::json!({
                    "result": {
                        "uptime_seconds": 1,
                        "daemon_version": "mock-0.0.0",
                        "memory": {
                            "limit_bytes": 0,
                            "current_bytes": 0,
                            "reserved_bytes": 0,
                            "high_water_bytes": 0
                        },
                        "workspaces": [
                            {
                                "index_root": workspace_root.canonicalize()
                                    .expect("canonical workspace")
                                    .to_string_lossy(),
                                "state": "Loaded"
                            }
                        ]
                    },
                    "meta": {
                        "stale": false,
                        "daemon_version": "mock-0.0.0"
                    }
                }),
            );
            sqry_daemon_protocol::framing::write_frame_json(&mut stream, &loaded_status_response)
                .await
                .expect("write daemon/status response");

            let search_request: sqry_daemon_protocol::JsonRpcRequest =
                sqry_daemon_protocol::framing::read_frame_json(&mut stream)
                    .await
                    .expect("read daemon/search request (transport)")
                    .expect("daemon/search frame must be present");
            assert_eq!(
                search_request.method, "daemon/search",
                "loaded workspace should make the shim attempt daemon/search"
            );
            tokio::time::sleep(Duration::from_millis(750)).await;
        });
    });
    ready_rx.recv().expect("mock socket must bind");
    handle
}

#[cfg(unix)]
#[test]
fn daemon_fallback_diagnostic_fires_when_daemon_status_succeeds() {
    let workspace = shared_workspace();
    let sock_dir = tempfile::tempdir().expect("sock tempdir");
    let sock_path = sock_dir.path().join("mock-sqryd.sock");

    // Start the status-capable mock *before* invoking sqry so the probe
    // finds the listener ready. The thread runs to completion when the
    // diagnostic's `DaemonClient::status()` completes; we join below.
    let mock_handle = spawn_status_mock(sock_path.clone());

    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .env("SQRY_DAEMON_SOCKET", &sock_path)
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());

    // Wait for the mock to drain — it must have served the handshake
    // since the diagnostic line fired (sqry exited successfully).
    mock_handle.join().expect("mock thread must not panic");

    assert!(
        stderr.contains("note: sqryd is running"),
        "expected daemon-fallback diagnostic; got: {stderr}"
    );
    assert!(
        stderr.contains(sock_path.to_str().unwrap_or("")),
        "diagnostic must name the actual socket path; got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn daemon_fallback_diagnostic_silent_when_hello_succeeds_but_status_absent() {
    let workspace = shared_workspace();
    let sock_dir = tempfile::tempdir().expect("sock tempdir");
    let sock_path = sock_dir.path().join("hello-only-sqryd.sock");

    let mock_handle = spawn_hello_only_mock(sock_path.clone());

    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .env("SQRY_DAEMON_SOCKET", &sock_path)
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    mock_handle.join().expect("mock thread must not panic");

    assert!(
        !stderr.contains("note: sqryd is running"),
        "diagnostic must not fire when hello succeeds but daemon/status is absent; got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn daemon_search_shim_falls_back_when_status_rpc_stalls() {
    let workspace = shared_workspace();
    let sock_dir = tempfile::tempdir().expect("sock tempdir");
    let sock_path = sock_dir.path().join("status-stall-sqryd.sock");
    let mock_handle = spawn_stalling_daemon_search_mock(
        sock_path.clone(),
        workspace.to_path_buf(),
        SearchShimStallPoint::Status,
    );

    let started = Instant::now();
    let assert = sqry_cmd()
        .arg("--exact")
        .arg("hello_search_target")
        .arg(workspace)
        .env("SQRY_DAEMON_SOCKET", &sock_path)
        .timeout(Duration::from_secs(3))
        .assert()
        .success();
    let elapsed = started.elapsed();
    mock_handle.join().expect("mock thread must not panic");

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("hello_search_target"),
        "fallback search should still return the exact hit; stdout: {stdout}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "stalled daemon/status must be bounded and fall back quickly; elapsed={elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn daemon_search_shim_falls_back_when_search_rpc_stalls() {
    let workspace = shared_workspace();
    let sock_dir = tempfile::tempdir().expect("sock tempdir");
    let sock_path = sock_dir.path().join("search-stall-sqryd.sock");
    let mock_handle = spawn_stalling_daemon_search_mock(
        sock_path.clone(),
        workspace.to_path_buf(),
        SearchShimStallPoint::Search,
    );

    let started = Instant::now();
    let assert = sqry_cmd()
        .arg("--exact")
        .arg("hello_search_target")
        .arg(workspace)
        .env("SQRY_DAEMON_SOCKET", &sock_path)
        .timeout(Duration::from_secs(3))
        .assert()
        .success();
    let elapsed = started.elapsed();
    mock_handle.join().expect("mock thread must not panic");

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("hello_search_target"),
        "fallback search should still return the exact hit after daemon/search stalls; stdout: {stdout}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "stalled daemon/search must be bounded and fall back quickly; elapsed={elapsed:?}"
    );
}

/// Regression guard for the 2026-05-10 tier-1 review finding: a bare
/// `UnixListener::bind` that accepts but does *not* speak the daemon hello
/// protocol must NOT trigger the diagnostic. Before the fix, the probe used
/// `UnixStream::connect` only — any connectable socket was treated as
/// "sqryd is running". After the fix, the probe completes the hello
/// handshake, so a non-daemon listener correctly fails the check.
#[cfg(unix)]
#[test]
fn daemon_fallback_diagnostic_silent_for_non_daemon_listener() {
    use std::os::unix::net::UnixListener;
    let workspace = shared_workspace();
    let sock_dir = tempfile::tempdir().expect("sock tempdir");
    let sock_path = sock_dir.path().join("not-sqryd.sock");
    // Bare UnixListener — accepts connections but never speaks the daemon
    // protocol. The 250ms hello-handshake timeout in the probe must
    // suppress the diagnostic.
    let _listener = UnixListener::bind(&sock_path).expect("bind bare UDS");

    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .env("SQRY_DAEMON_SOCKET", &sock_path)
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        !stderr.contains("note: sqryd is running"),
        "diagnostic must not fire for a listener that doesn't speak daemon hello; got: {stderr}"
    );
}

#[test]
fn daemon_fallback_diagnostic_absent_when_socket_unreachable() {
    let workspace = shared_workspace();
    let unreachable = PathBuf::from("/nonexistent/definitely-not-sqry.sock");

    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .env("SQRY_DAEMON_SOCKET", &unreachable)
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());
    assert!(
        !stderr.contains("note: sqryd is running"),
        "diagnostic must not fire when socket is unreachable; got: {stderr}"
    );
}

// --------------------------------------------------------------------------
// `sqry index` regression guard (CLI_PROGRESS_REPORTER coexistence)
// --------------------------------------------------------------------------
//
// `PlainProgressReporter` was added as a *sibling* to `CliProgressReporter`
// in `sqry-cli/src/progress.rs` — this test asserts the index command's
// stderr characteristics survive the addition. Concretely:
//
//   - `sqry index` must continue to use `CliProgressReporter` (indicatif),
//     not the new plain reporter. Detection: the new reporter's stable
//     prefix `[sqry] ` must NOT appear in index stderr (PlainProgressReporter
//     is only constructed in the search path via `for_search`).
//
//   - Repeated `sqry index` runs over byte-identical workspaces must produce
//     byte-identical stdout/stderr. This is an executable golden-output
//     guard for the DAG `CLI_PROGRESS_REPORTER` acceptance.

#[test]
fn sqry_index_golden_output_is_byte_identical_and_does_not_leak_plain_progress() {
    fn normalized_output(bytes: &[u8], workspace: &Path) -> String {
        let raw = String::from_utf8_lossy(bytes);
        let workspace = workspace.display().to_string();
        let with_stable_path = raw.replace(&workspace, "<WORKSPACE>");
        let duration_re =
            regex::Regex::new(r"\b\d+(?:\.\d+)?(?:ns|µs|us|ms|s)\b").expect("duration regex");
        duration_re
            .replace_all(&with_stable_path, "<DURATION>")
            .into_owned()
    }

    let run_index = |dir: &Path| {
        let _ = std::fs::remove_dir_all(dir.join(".sqry"));
        let _ = std::fs::remove_dir_all(dir.join(".sqry-cache"));
        std::fs::write(
            dir.join("regression_guard.rs"),
            "fn regression_target() {}\n",
        )
        .expect("write source");

        let assert = Command::new(sqry_bin())
            .arg("index")
            .arg(dir)
            // Same env hygiene as the other tests: keep the daemon out of it.
            .env("SQRY_DAEMON_SOCKET", "/nonexistent/index-test.sock")
            .env_remove("SQRY_LOG")
            .env_remove("RUST_LOG")
            .env_remove("SQRY_OUTPUT_FORMAT")
            .assert()
            .success();
        (
            assert.get_output().stdout.clone(),
            assert.get_output().stderr.clone(),
        )
    };

    let dir = tempfile::tempdir().expect("index tempdir");
    let (stdout_before, stderr_before) = run_index(dir.path());
    let (stdout_after, stderr_after) = run_index(dir.path());
    let stdout_before_normalized = normalized_output(&stdout_before, dir.path());
    let stdout_after_normalized = normalized_output(&stdout_after, dir.path());
    let stderr_before_normalized = normalized_output(&stderr_before, dir.path());
    let stderr_after_normalized = normalized_output(&stderr_after, dir.path());
    assert_eq!(
        stdout_before_normalized, stdout_after_normalized,
        "sqry index stdout must be byte-identical across equivalent runs after masking path/timing volatility"
    );
    assert_eq!(
        stderr_before_normalized, stderr_after_normalized,
        "sqry index stderr must be byte-identical across equivalent runs after masking path/timing volatility"
    );

    let stderr = String::from_utf8_lossy(&stderr_after);
    let stdout = String::from_utf8_lossy(&stdout_after);

    // PlainProgressReporter writes `[sqry] <stage> ...` to stderr; that
    // string must never appear in index output (which routes through
    // CliProgressReporter's indicatif spinner instead).
    assert!(
        !stderr.contains("[sqry] load snapshot"),
        "sqry index leaked PlainProgressReporter `load snapshot` event to stderr; \
         the reporter must not be wired into the index path. stderr: {stderr}"
    );
    assert!(
        !stderr.contains("[sqry] regex scan"),
        "sqry index leaked PlainProgressReporter `regex scan` event; got: {stderr}"
    );
    assert!(
        !stderr.contains("[sqry] fuzzy match"),
        "sqry index leaked PlainProgressReporter `fuzzy match` event; got: {stderr}"
    );

    // No JSON-line events leak into stdout (which carries final result
    // data, never per-stage progress). The PlainProgressReporter writes to
    // stderr only, but this guards against an accidental future cross-wire.
    assert!(
        !stdout.contains("\"event\":\"stage_started\""),
        "sqry index stdout contains JSON-line stage events; got: {stdout}"
    );
}

#[test]
fn sqry_index_does_not_leak_plain_progress_reporter_output() {
    let dir = tempfile::tempdir().expect("index tempdir");
    std::fs::write(
        dir.path().join("regression_guard.rs"),
        "fn regression_target() {}\n",
    )
    .expect("write source");

    let assert = Command::new(sqry_bin())
        .arg("index")
        .arg(dir.path())
        // Same env hygiene as the other tests: keep the daemon out of it.
        .env("SQRY_DAEMON_SOCKET", "/nonexistent/index-test.sock")
        .env_remove("SQRY_LOG")
        .env_remove("RUST_LOG")
        .env_remove("SQRY_OUTPUT_FORMAT")
        .assert()
        .success();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // PlainProgressReporter writes `[sqry] <stage> ...` to stderr; that
    // string must never appear in index output (which routes through
    // CliProgressReporter's indicatif spinner instead).
    assert!(
        !stderr.contains("[sqry] load snapshot"),
        "sqry index leaked PlainProgressReporter `load snapshot` event to stderr; \
         the reporter must not be wired into the index path. stderr: {stderr}"
    );
    assert!(
        !stderr.contains("[sqry] regex scan"),
        "sqry index leaked PlainProgressReporter `regex scan` event; got: {stderr}"
    );
    assert!(
        !stderr.contains("[sqry] fuzzy match"),
        "sqry index leaked PlainProgressReporter `fuzzy match` event; got: {stderr}"
    );

    // No JSON-line events leak into stdout (which carries final result
    // data, never per-stage progress). The PlainProgressReporter writes to
    // stderr only, but this guards against an accidental future cross-wire.
    assert!(
        !stdout.contains("\"event\":\"stage_started\""),
        "sqry index stdout contains JSON-line stage events; got: {stdout}"
    );
}

// --------------------------------------------------------------------------
// Case (f) — JSON-line output mode emits parseable stage events
// --------------------------------------------------------------------------

#[test]
fn json_output_mode_emits_parseable_stage_events() {
    let workspace = shared_workspace();
    // Ensure the daemon socket is unreachable so the daemon-fallback
    // diagnostic doesn't mix non-stage events into the stream.
    let assert = sqry_cmd()
        .arg("search")
        .arg("hello_search_target")
        .arg(workspace)
        .arg("--verbose")
        .env("SQRY_OUTPUT_FORMAT", "json")
        .env("SQRY_DAEMON_SOCKET", "/nonexistent/json-mode-test.sock")
        .assert()
        .success();
    let stderr = stderr_of(assert.get_output());

    // Filter to the JSON-line subset (each starts with `{` and parses).
    // Other stderr (e.g., 'Showing N of M matches' from the truncation
    // banner) is not a JSON-line event and is skipped.
    let mut json_line_count = 0;
    let mut saw_stage_started = false;
    let mut saw_stage_completed = false;
    for line in stderr.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("non-parseable JSON line `{trimmed}`: {e}"));
        json_line_count += 1;
        match value["event"].as_str() {
            Some("stage_started") => saw_stage_started = true,
            Some("stage_completed") => saw_stage_completed = true,
            Some("daemon_fallback") => panic!(
                "daemon_fallback event should be absent when SQRY_DAEMON_SOCKET is unreachable; \
                 stderr was: {stderr}"
            ),
            other => panic!("unexpected event variant `{other:?}` in JSON-line stream: {stderr}"),
        }
    }
    assert!(
        json_line_count > 0,
        "expected at least one JSON-line event in stderr; got: {stderr}"
    );
    assert!(
        saw_stage_started && saw_stage_completed,
        "JSON stream missing stage_started/stage_completed pair; got: {stderr}"
    );
}
