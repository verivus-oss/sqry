//! C094d — verify the global `sqry --workspace <PATH>` flag (forwarded
//! into `LspOptions.workspace`) actually drives the LSP server's
//! source-root resolution end-to-end through the CLI dispatcher.
//!
//! Iter1 reviewer flagged that the prior in-process test bypassed the
//! global `--workspace` parsing on the `sqry` root CLI form
//! (`sqry-cli/src/args/mod.rs:317-330`). This replacement launches the
//! actual `sqry` binary as a subprocess, threads `--workspace <T>`
//! through the global Cli flag (NOT the standalone `sqry-lsp` binary
//! flag), drives an LSP `initialize` + `sqry/workspaceStatus` round
//! trip over stdio, and asserts the response's `source_roots` array
//! contains the supplied workspace path.
//!
//! The Content-Length framing helpers are inlined here because the
//! existing helpers in `sqry-lsp/tests/daemon_mode.rs` are tied to
//! tokio async streams (`AsyncRead`/`AsyncWrite`) — this test drives
//! the child process via blocking `std::process::ChildStdin/Stdout`
//! which is the natural fit for a one-shot LSP smoke. The framing
//! shape (`Content-Length: <N>\r\n\r\n<body>`) is identical.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Locate the `sqry` binary the same way `sqry-cli/tests/common::sqry_bin`
/// does. Duplicated here so this LSP-crate test does not have to reach
/// across the workspace boundary.
fn sqry_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
        let p = std::path::PathBuf::from(path);
        if p.is_file() {
            return p;
        }
    }
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqry") {
        return std::path::PathBuf::from(path);
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_dir = std::path::PathBuf::from(manifest_dir)
        .parent()
        .expect("workspace dir")
        .to_path_buf();
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let make = |base: &str| -> std::path::PathBuf {
        if exe_suffix.is_empty() {
            std::path::PathBuf::from(base)
        } else {
            std::path::PathBuf::from(format!("{base}{exe_suffix}"))
        }
    };
    let debug = workspace_dir.join(make("target/debug/sqry"));
    let release = workspace_dir.join(make("target/release/sqry"));
    if debug.exists() {
        debug
    } else if release.exists() {
        release
    } else {
        panic!(
            "Could not find sqry binary. Tried CARGO_BIN_EXE_sqry, {}, {}. \
             Run `cargo build` first.",
            debug.display(),
            release.display(),
        )
    }
}

/// Send a single JSON-RPC message with LSP Content-Length framing.
fn lsp_send<W: Write>(stdin: &mut W, body: &Value) {
    let payload = serde_json::to_string(body).expect("serialise json-rpc body");
    let frame = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
    stdin.write_all(frame.as_bytes()).expect("write LSP frame");
    stdin.flush().expect("flush LSP frame");
}

/// Read one Content-Length-framed JSON-RPC message from `reader`.
///
/// Content-Length is parsed from the header block, then exactly that
/// many body bytes are pulled. Notifications without an `id`
/// (`window/logMessage`, `$/progress`, etc.) are returned to the caller
/// alongside responses — the caller filters by id.
fn lsp_recv<R: Read>(reader: &mut BufReader<R>) -> Value {
    let mut content_length: Option<usize> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read LSP header line");
        assert!(n > 0, "LSP stream closed before header complete");
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = Some(rest.trim().parse().expect("parse content-length"));
        }
        assert!(
            Instant::now() < deadline,
            "timed out reading LSP headers (15s)"
        );
    }
    let n = content_length.expect("LSP response missing Content-Length");
    let mut body = vec![0u8; n];
    reader
        .read_exact(&mut body)
        .expect("read exact LSP body bytes");
    serde_json::from_slice(&body).expect("LSP body must decode as JSON")
}

/// Read messages from the child until one whose `id` field equals `id`
/// arrives. Notifications and unrelated responses are silently
/// discarded — exactly the same loop a real LSP client uses.
fn lsp_recv_until_id<R: Read>(reader: &mut BufReader<R>, id: i64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let msg = lsp_recv(reader);
        if msg.get("id").and_then(Value::as_i64) == Some(id) {
            return msg;
        }
        assert!(
            Instant::now() < deadline,
            "timed out (30s) waiting for LSP response id={id}; last message: {msg}"
        );
    }
}

/// Drain stderr of the child process for diagnostic context if a step
/// fails. Best-effort, non-blocking.
fn drain_stderr(child: &mut Child) -> String {
    if let Some(mut stderr) = child.stderr.take() {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    } else {
        String::new()
    }
}

#[test]
fn workspace_flag_via_subprocess_drives_source_roots() {
    // Pre-create a temp workspace + a tiny .rs file so the indexer has
    // real content to walk.
    let tmp = TempDir::new().expect("create workspace tempdir");
    let workspace_path = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    std::fs::write(
        workspace_path.join("smoke.rs"),
        "fn marker() -> u32 { 42 }\n",
    )
    .expect("write smoke.rs");

    // Isolated home / xdg / daemon socket so the test can't contact
    // host state. Mirrors the pattern in
    // `sqry-cli/tests/installed_feature_surface_e2e.rs::run`.
    let home = workspace_path.join(".home");
    let xdg_config = workspace_path.join(".xdg/config");
    let xdg_cache = workspace_path.join(".xdg/cache");
    let xdg_data = workspace_path.join(".xdg/data");
    let xdg_runtime = workspace_path.join(".xdg/runtime");
    for dir in [&home, &xdg_config, &xdg_cache, &xdg_data, &xdg_runtime] {
        std::fs::create_dir_all(dir).expect("create isolation dir");
    }
    let isolated_socket = xdg_runtime.join("sqryd.sock");

    // Step 1: build the snapshot so the LSP session can resolve
    // workspace state. A bare `sqry index` is sufficient.
    let index_output = Command::new(sqry_bin())
        .arg("index")
        .arg(&workspace_path)
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_RUNTIME_DIR", &xdg_runtime)
        .env("SQRY_DAEMON_SOCKET", &isolated_socket)
        .output()
        .expect("run `sqry index`");
    assert!(
        index_output.status.success(),
        "`sqry index` setup failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&index_output.stdout),
        String::from_utf8_lossy(&index_output.stderr),
    );

    // Step 2: spawn `sqry --workspace <T> lsp --stdio`. The `--workspace`
    // flag is the GLOBAL Cli option (sqry-cli/src/args/mod.rs:317-330) —
    // it appears BEFORE the `lsp` subcommand on the command line.
    let mut child = Command::new(sqry_bin())
        .arg("--workspace")
        .arg(&workspace_path)
        .arg("lsp")
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_RUNTIME_DIR", &xdg_runtime)
        .env("SQRY_DAEMON_SOCKET", &isolated_socket)
        .spawn()
        .expect("spawn `sqry lsp --stdio` child");

    let stdin = child.stdin.take().expect("child stdin piped");
    let stdout = child.stdout.take().expect("child stdout piped");
    let mut writer = stdin;
    let mut reader = BufReader::new(stdout);

    // Step 3: LSP `initialize` round-trip. processId = null (we don't
    // want the server to track our PID), rootUri = file:// of the
    // workspace path so the server has a fallback if `--workspace`
    // didn't take effect (failing to take effect is what this test
    // catches via the `--workspace` precedence path: workspace > rootUri).
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "rootUri": null,
            "capabilities": {},
        }
    });
    lsp_send(&mut writer, &initialize);
    let init_resp = lsp_recv_until_id(&mut reader, 1);
    assert!(
        init_resp.get("result").is_some(),
        "initialize response missing `result`: {init_resp}\nstderr: {}",
        drain_stderr(&mut child)
    );

    // The `initialized` notification (no id) is required by the LSP
    // spec before tool methods are dispatched.
    lsp_send(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );

    // Step 4: drive the custom `sqry/workspaceStatus` method
    // (registered at sqry-lsp/src/lib.rs:587-590) and assert the
    // returned `source_roots` array contains the `--workspace` path.
    lsp_send(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "sqry/workspaceStatus",
            "params": {}
        }),
    );
    let status_resp = lsp_recv_until_id(&mut reader, 2);

    let result = status_resp.get("result").unwrap_or_else(|| {
        panic!(
            "sqry/workspaceStatus response missing `result`: {status_resp}\nstderr: {}",
            drain_stderr(&mut child)
        )
    });
    // The custom-method response shape unwraps the `WorkspaceStatusInfo`
    // directly into `result` (not nested under `result.info`); the
    // `source_roots` array is at the top level of the result object.
    // Verified against the live wire payload returned by
    // `SqryLanguageServer::handle_workspace_status` — see
    // `sqry-lsp/src/server.rs` for the serialisation site.
    let source_roots = result
        .get("source_roots")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("sqry/workspaceStatus result missing `source_roots` array: {result}")
        });

    assert!(
        !source_roots.is_empty(),
        "source_roots must be non-empty when --workspace is set; got: {source_roots:?}"
    );

    // The exact form is the `Display` of the resolved workspace path.
    // Match by canonicalised string equality OR by prefix containment
    // (multi-root workspaces emit additional roots; `--workspace` may
    // be either the only root or one of several).
    let workspace_str = workspace_path.to_string_lossy().into_owned();
    let contains_workspace = source_roots.iter().any(|root| {
        let s = root.as_str().unwrap_or("");
        s == workspace_str
            || Path::new(s)
                .canonicalize()
                .map(|c| c == workspace_path)
                .unwrap_or(false)
            || s.starts_with(&workspace_str)
    });
    assert!(
        contains_workspace,
        "source_roots must contain the --workspace path {} (got: {source_roots:?})\n\
         stderr: {}",
        workspace_path.display(),
        drain_stderr(&mut child)
    );

    // Step 5: clean LSP shutdown — `shutdown` request + `exit`
    // notification. Spec compliance keeps the child from being orphaned
    // and ensures the test fails loudly if the server panics during
    // teardown (the response would carry an `error` field).
    lsp_send(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown"
        }),
    );
    let shutdown_resp = lsp_recv_until_id(&mut reader, 3);
    assert!(
        shutdown_resp.get("error").is_none(),
        "shutdown returned an error: {shutdown_resp}",
    );

    lsp_send(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit"
        }),
    );

    // Drop writer to close stdin so the child sees EOF if it's still
    // blocked on read after `exit`.
    drop(writer);

    // Wait up to 5s for clean termination, then fall back to kill.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "sqry lsp did not exit within 5s after `exit` notification\nstderr: {}",
                        drain_stderr(&mut child)
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("try_wait on sqry lsp child failed: {e}"),
        }
    }
}
