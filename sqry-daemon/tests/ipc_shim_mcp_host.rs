//! Phase 8c U15 — MCP host integration tests (§K, design iter-2 §I, §H).
//!
//! Exercises the full IPC path: `TestServer` → ShimRegister → MCP host
//! via rmcp `serve_client`. The 8 tests cover:
//!
//! 1. `mcp_host_serves_initialize` — rmcp initialize round-trip.
//! 2. `mcp_host_tools_list_returns_17_subset` — tools/list returns 17 names
//!    (the natural-language sqry_ask tool was removed).
//! 3. `mcp_host_tools_call_semantic_search_fresh_verdict` — fresh workspace,
//!    success response.
//! 4. `mcp_host_raii_deregisters_on_client_disconnect` — ShimRegistry
//!    drains on client drop.
//! 5. `mcp_host_tools_call_stale_splices_warning` — stale workspace,
//!    `_stale_warning` present in both `content[0].text` and
//!    `structured_content`.
//! 6. `mcp_host_tools_call_notready_returns_mcp_error` — notready
//!    workspace → MCP error with `kind="workspace_not_ready"`.
//! 7. `mcp_shutdown_during_tool_call` — call_tool submitted, then shutdown
//!    fired after the per-test `thread_start_hook` notifier confirms the
//!    OS thread inside `execute_with_timeout` has actually started; both
//!    the call and `IpcServer::run` complete within the drain window;
//!    result is either Ok (fast path) or Err (transport close).
//! 8. `mcp_stale_parity_content_and_structured` — stale verdict: BOTH
//!    `content[0].text` and `structured_content` carry `_stale_warning`
//!    (regression test for the wire-parity bug fixed in U8 commit
//!    `c64bfbb0d`).

#![allow(clippy::too_many_lines)]
// The IPC test server and client run over a Unix domain socket
// (`support::ipc`), so this binary is Unix-only.
#![cfg(unix)]

mod support;

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqry_daemon::ipc::tool_core::thread_start_hook;

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{
    DaemonConfig, WorkspaceKey, WorkspaceState,
    ipc::framing::{read_frame_json, write_frame_json},
};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_mcp::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn call_tool_request(
    name: &'static str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> rmcp::model::CallToolRequestParams {
    rmcp::model::CallToolRequestParams::new(name).with_arguments(arguments)
}

/// Connect to the server, perform the MCP shim handshake, and return
/// the rmcp transport halves. Verifies the ack is accepted.
async fn connect_mcp_shim(
    server: &TestServer,
) -> (
    tokio::io::ReadHalf<UnixStream>,
    tokio::io::WriteHalf<UnixStream>,
) {
    let stream = UnixStream::connect(&server.path).await.expect("connect");
    let (mut rh, mut wh) = tokio::io::split(stream);

    let shim_reg = ShimRegister {
        protocol: ShimProtocol::Mcp,
        pid: std::process::id(),
    };
    write_frame_json(&mut wh, &shim_reg)
        .await
        .expect("write ShimRegister");

    let ack = read_frame_json::<_, ShimRegisterAck>(&mut rh)
        .await
        .expect("read ack")
        .expect("ack frame");
    assert!(
        ack.accepted,
        "ack must be accepted; reason={:?}",
        ack.reason
    );

    (rh, wh)
}

/// Insert a tempdir's canonicalized path as a `Loaded` workspace entry.
fn setup_loaded_workspace(server: &TestServer, dir: &TempDir) -> std::path::PathBuf {
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key, WorkspaceState::Loaded);
    canon
}

/// Insert a tempdir's canonicalized path as a `Failed` workspace with
/// `last_good_at` set to `age_secs` seconds in the past. Returns the root.
fn setup_stale_workspace(server: &TestServer, dir: &TempDir, age_secs: u64) -> std::path::PathBuf {
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Failed);
    let ws = server.manager.lookup(&key).expect("ws present");
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(age_secs)));
    canon
}

/// Insert a tempdir's canonicalized path as a `Loading` workspace
/// (NotReady verdict).
fn setup_notready_workspace(server: &TestServer, dir: &TempDir) -> std::path::PathBuf {
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key, WorkspaceState::Loading);
    canon
}

// ---------------------------------------------------------------------------
// Test 1: mcp_host_serves_initialize
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_serves_initialize() {
    let server = TestServer::new().await;

    let (rh, wh) = connect_mcp_shim(&server).await;

    // `serve_client` performs the MCP `initialize` handshake and returns
    // a `RunningService`. If this succeeds the MCP host advertised its
    // capabilities correctly.
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize must succeed");

    // Verify server_info comes back with the daemon identity.
    // The `peer_info()` method returns `Option<&ServerInfo>` which is
    // set after initialize.
    // (If rmcp client exposes peer_info we check it; otherwise the
    // successful `serve_client` call is the assertion.)
    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: mcp_host_tools_list_returns_17_subset
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_tools_list_returns_17_subset() {
    let server = TestServer::new().await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let list_result = running
        .peer()
        .list_tools(None)
        .await
        .expect("list_tools must succeed");

    let got_names: HashSet<&str> = list_result.tools.iter().map(|t| t.name.as_ref()).collect();
    let expected: HashSet<&str> = DAEMON_SUPPORTED_TOOL_NAMES.iter().copied().collect();

    assert_eq!(
        got_names, expected,
        "tools/list must return exactly DAEMON_SUPPORTED_TOOL_NAMES"
    );
    assert_eq!(
        list_result.tools.len(),
        17,
        "tools/list count must be 17 under default feature flags \
         (15 + body-shape structural_similar U07 + generate_overview)"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: mcp_host_tools_call_semantic_search_fresh_verdict
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_tools_call_semantic_search_fresh_verdict() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = setup_loaded_workspace(&server, &dir);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "semantic_search",
            serde_json::Map::from_iter([
                ("query".to_string(), json!("kind:function")),
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("max_results".to_string(), json!(5)),
                ("context_lines".to_string(), json!(0)),
                ("include_classpath".to_string(), json!(false)),
            ]),
        ))
        .await
        .expect("call_tool must succeed for fresh workspace");

    // Must not be an error response.
    assert!(
        result.is_error != Some(true),
        "fresh workspace call must not set is_error=true"
    );

    // content must have at least one text item.
    assert!(
        !result.content.is_empty(),
        "result content must be non-empty"
    );

    // No `_stale_warning` in the first content text.
    let first_text = result.content[0]
        .as_text()
        .map(|t| t.text.as_str())
        .unwrap_or("");
    assert!(
        !first_text.contains("_stale_warning"),
        "fresh verdict must NOT splice _stale_warning; got: {first_text:.200}"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 4: mcp_host_raii_deregisters_on_client_disconnect
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_raii_deregisters_on_client_disconnect() {
    let server = TestServer::new().await;
    let registry = server.shim_registry();

    assert!(registry.is_empty(), "registry must start empty");

    let (rh, wh) = connect_mcp_shim(&server).await;
    // Give the server task time to register the entry.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        registry.len(),
        1,
        "registry must have 1 entry after connect"
    );

    // Drop halves to close the connection.
    drop(rh);
    drop(wh);

    // Wait for RAII deregistration.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if registry.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "registry must deregister within 3s; len={}",
            registry.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        registry.is_empty(),
        "registry must be empty after client disconnect"
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 5: mcp_host_tools_call_stale_splices_warning
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_tools_call_stale_splices_warning() {
    // 12h stale, well within default 24h cap.
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = setup_stale_workspace(&server, &dir, 12 * 3600);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "semantic_search",
            serde_json::Map::from_iter([
                ("query".to_string(), json!("kind:function")),
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("max_results".to_string(), json!(5)),
                ("context_lines".to_string(), json!(0)),
                ("include_classpath".to_string(), json!(false)),
            ]),
        ))
        .await
        .expect("stale workspace call must succeed (not an error result)");

    // `_stale_warning` must be present in `content[0].text`.
    let first_text = result.content[0]
        .as_text()
        .map(|t| t.text.as_str())
        .unwrap_or("");
    assert!(
        first_text.contains("_stale_warning"),
        "stale verdict must splice _stale_warning into content[0].text; got: {first_text:.300}"
    );
    assert!(
        first_text.contains("stale"),
        "_stale_warning value must mention 'stale'; got: {first_text:.300}"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 6: mcp_host_tools_call_notready_returns_mcp_error
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_tools_call_notready_returns_mcp_error() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = setup_notready_workspace(&server, &dir);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let call_result = running
        .peer()
        .call_tool(call_tool_request(
            "semantic_search",
            serde_json::Map::from_iter([
                ("query".to_string(), json!("kind:function")),
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("max_results".to_string(), json!(5)),
                ("context_lines".to_string(), json!(0)),
                ("include_classpath".to_string(), json!(false)),
            ]),
        ))
        .await;

    match call_result {
        Err(rmcp::ServiceError::McpError(mcp_err)) => {
            let data = mcp_err.data.as_ref().expect("error must carry data");
            assert_eq!(
                data["kind"], "workspace_not_ready",
                "NotReady workspace must emit kind='workspace_not_ready'"
            );
            assert_eq!(
                data["retryable"], true,
                "workspace_not_ready must be retryable"
            );
        }
        Err(other) => panic!("expected McpError variant, got: {other:?}"),
        Ok(r) => panic!("expected MCP error for notready workspace, got success: {r:?}"),
    }

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 7: mcp_shutdown_during_tool_call
//
// Validates §I detached-thread shutdown semantics through the REAL daemon
// MCP dispatch path:
//   client call_tool
//     → rmcp DaemonMcpHandler::call_tool
//     → tool_core::classify_and_execute
//     → execute_with_timeout
//     → tokio::task::spawn_blocking(dispatch_by_name)   ← real OS thread
//
// Three §I properties verified:
//   a. The call_tool completes within the drain window — either with a
//      successful result (if the response was delivered before shutdown
//      cancelled the rmcp layer) or with a transport/service error (if
//      the connection was closed before the response arrived). BOTH are
//      valid §I outcomes; what is NOT valid is the call hanging.
//   b. IpcServer::run completes within ipc_shutdown_drain_secs + margin.
//   c. The real spawn_blocking OS thread (inside execute_with_timeout)
//      actually started before shutdown fired — proved by the per-test
//      `thread_start_hook` notifier, set as the FIRST action inside the
//      `spawn_blocking` closure after the OS thread scheduler dispatches
//      it, before any graph work runs.
//
// Strategy (iter-8: path-keyed registration eliminates cross-test races):
//   1. Create a per-test Arc<AtomicBool> notifier and register it keyed by
//      our unique tempdir's canonical path via
//      `thread_start_hook::register(canon, flag)`. Because every test
//      creates its own tempdir, the path is unique to this test — no
//      other concurrent test in this binary can fire our flag.
//   2. Submit call_tool via tokio::spawn against our `canon` path.
//   3. Busy-wait until the notifier is set — this is a SERVER-SIDE barrier
//      tied to the OS thread actually starting our test's tool dispatch.
//      Only THEN fire shutdown.
//   4. Call `thread_start_hook::clear(&canon)` to remove our entry.
//   5. tokio::join! both call_task and server.handle concurrently.
//   6. Assert both complete within the drain window.
//
// Short drain (1 s) keeps the test fast. Long tool_timeout (30 s) avoids
// the ToolTimeout path; we want the shutdown/transport-close path.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_shutdown_during_tool_call() {
    // Short drain to keep the test fast. Long tool_timeout so the
    // ToolTimeout path is not taken — we want the shutdown-drain path.
    let config = DaemonConfig {
        tool_timeout_secs: 30,
        ipc_shutdown_drain_secs: 1,
        ..DaemonConfig::default()
    };
    let server = TestServer::with_config(config).await;
    let dir = tempfile::tempdir().unwrap();
    let canon = setup_loaded_workspace(&server, &dir);
    let canon_str = canon.to_string_lossy().to_string();

    // Connect the MCP shim and initialize.
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    // Register a per-test notifier keyed by this test's unique canonical
    // workspace path (our tempdir's canonicalised path). Cross-test
    // isolation is structural: the daemon's `notify(&path)` only fires
    // the entry whose path equals the dispatched workspace's
    // `canonical_root`. Other concurrent tests use their own tempdirs, so
    // their tool dispatches cannot fire our flag. This is the iter-8
    // redesign that replaces the iter-3..iter-7 token/serializer approach.
    let thread_started = Arc::new(AtomicBool::new(false));
    thread_start_hook::register(canon.clone(), Arc::clone(&thread_started));

    // Clone the Peer so we can submit call_tool from a spawned task while
    // the main task polls for the server-side barrier.
    let peer = running.peer().clone();
    let call_task = tokio::spawn(async move {
        peer.call_tool(call_tool_request(
            "semantic_search",
            serde_json::Map::from_iter([
                ("query".to_string(), json!("kind:function")),
                ("path".to_string(), json!(canon_str)),
                ("max_results".to_string(), json!(5)),
                ("context_lines".to_string(), json!(0)),
                ("include_classpath".to_string(), json!(false)),
            ]),
        ))
        .await
    });

    // Server-side barrier: wait until execute_with_timeout's spawned OS thread
    // sets thread_started=true, or until the call completes on its own.
    // We give it up to 2s; the flag is set as the first action inside the
    // spawn_blocking closure after the request traverses the full daemon path:
    //   rmcp accept → DaemonMcpHandler::call_tool → classify_and_execute
    //   → execute_with_timeout → spawn_blocking(|_| { notify(); run })
    //
    // If the flag is never set within 2s something is wrong with the routing;
    // in that case we fire shutdown anyway and the test will surface the failure
    // via the call_result assertions below.
    let barrier_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !thread_started.load(Ordering::Acquire)
        && tokio::time::Instant::now() < barrier_deadline
        && !call_task.is_finished()
    {
        tokio::task::yield_now().await;
    }

    // Record whether the real path was reached (for assertion below).
    let spawn_blocking_was_called = thread_started.load(Ordering::Acquire);

    // Path-keyed cleanup: removes our entry from the registry. No-op if
    // already absent. The path key is unique to this test's tempdir, so
    // we cannot accidentally clear another test's registration.
    thread_start_hook::clear(&canon);

    // Fire daemon shutdown now that the real spawn_blocking path is confirmed
    // in-flight (or the 2s barrier elapsed). The forwarder in
    // host_mcp_on_streams fires service_ct.cancel(), causing the rmcp inner
    // loop to drain cooperatively.
    server.shutdown.cancel();

    // Assertion (b): IpcServer::run and the call_task both complete within
    // the drain window. Join! them concurrently.
    let drain_margin = Duration::from_secs(5); // drain=1s + 4s margin
    let (call_done, server_done) = tokio::join!(
        tokio::time::timeout(drain_margin, call_task),
        tokio::time::timeout(drain_margin, server.handle),
    );

    // Server must have completed cleanly.
    let server_joined = server_done.expect("IpcServer::run must complete within drain margin");
    assert!(server_joined.is_ok(), "IpcServer::run join must not panic");

    // Assertion (c): the real spawn_blocking OS thread started before shutdown.
    assert!(
        spawn_blocking_was_called,
        "thread_start_hook notifier must be set before shutdown fires — this proves \
         the real OS thread inside execute_with_timeout actually started: \
         DaemonMcpHandler::call_tool → classify_and_execute → execute_with_timeout \
         → spawn_blocking(dispatch_by_name) → [OS thread] → notify()"
    );

    // Assertion (a): the call_tool must have completed (not hung).
    // Two valid outcomes depending on the shutdown/call-completion race:
    //   Ok(_)  — call completed before rmcp cancellation; spawn_blocking
    //            closure ran fully and the result was delivered.
    //   Err(_) — rmcp transport cancelled before the response was sent;
    //            the OS thread may still be running (§I: not killed).
    let call_result = call_done
        .expect("call_tool task must complete within drain margin — must not hang")
        .expect("call_tool spawned task must not panic");

    match &call_result {
        Ok(result) => {
            // §I fast path: call completed before shutdown cancelled rmcp.
            // spawn_blocking closure (dispatch_by_name) ran to completion.
            assert!(
                result.is_error != Some(true),
                "fast-path call to a loaded workspace must not set is_error=true; \
                 got: {result:?}"
            );
        }
        Err(_service_err) => {
            // §I shutdown path: rmcp transport was cancelled before the
            // response arrived. The OS thread is still alive (not killed)
            // per the §I detached-thread contract.
        }
    }

    drop(running);
}

// ---------------------------------------------------------------------------
// Test 8: mcp_stale_parity_content_and_structured
//
// Regression test for the wire-parity bug fixed in U8 commit `c64bfbb0d`:
// BOTH `content[0].text` AND `structured_content` must carry the
// `_stale_warning` key when the workspace is stale. Prior to the fix,
// only `content[0].text` had the warning; `structured_content` was the
// pre-splice `inner` value without it.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_stale_parity_content_and_structured() {
    // 6h stale, within default 24h cap.
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = setup_stale_workspace(&server, &dir, 6 * 3600);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "semantic_search",
            serde_json::Map::from_iter([
                ("query".to_string(), json!("kind:function")),
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("max_results".to_string(), json!(5)),
                ("context_lines".to_string(), json!(0)),
                ("include_classpath".to_string(), json!(false)),
            ]),
        ))
        .await
        .expect("stale workspace call must succeed (not a protocol error)");

    // ---- Assertion A: content[0].text carries _stale_warning ----
    // `content[0]` is the text payload rendered via
    // `serde_json::to_string_pretty(&payload)` where `payload` has the
    // `_stale_warning` key spliced in. The text is a JSON string, so we
    // look for the key name in the serialized form.
    let content_text = result.content[0]
        .as_text()
        .map(|t| t.text.as_str())
        .expect("content[0] must be a text item");

    assert!(
        content_text.contains("_stale_warning"),
        "content[0].text MUST contain '_stale_warning' for stale verdict. \
         This tests against the U8 bug where content[0].text was built from \
         the pre-splice inner value. Got: {content_text:.400}"
    );

    // ---- Assertion B: structured_content carries _stale_warning ----
    // `structured_content` is set to `Some(payload)` where `payload` is
    // the post-splice `Value::Object` with `_stale_warning` inserted.
    // This is the CRITICAL assertion from design iter-1 §K fix: both
    // fields must carry the same spliced payload.
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured_content must be Some for stale verdict");

    assert!(
        structured.get("_stale_warning").is_some(),
        "structured_content MUST contain '_stale_warning' key for stale verdict. \
         Prior to U8 fix (c64bfbb0d), this was missing — structured_content was \
         the pre-splice inner value. Got: {structured}"
    );

    let stale_warning_value = &structured["_stale_warning"];
    assert!(
        stale_warning_value.is_string(),
        "_stale_warning in structured_content must be a string value; got: {stale_warning_value}"
    );

    let warning_str = stale_warning_value.as_str().unwrap();
    assert!(
        warning_str.contains("stale"),
        "_stale_warning value must describe the stale condition; got: {warning_str}"
    );

    // ---- Assertion C: both payloads are consistent ----
    // The `content[0].text` is `serde_json::to_string_pretty(structured)`,
    // so the JSON in the text must be parseable and must also contain
    // `_stale_warning` when parsed.
    let parsed_text: serde_json::Value = serde_json::from_str(content_text)
        .expect("content[0].text must be valid JSON (serde_json::to_string_pretty output)");
    assert_eq!(
        parsed_text.get("_stale_warning"),
        structured.get("_stale_warning"),
        "content[0].text and structured_content must carry identical _stale_warning payloads \
         (regression: prior to c64bfbb0d they differed)"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 9: mcp_host_rebuild_index_loads_workspace
//
// `rebuild_index` in daemon MCP mode calls `get_or_load` to build the
// graph in the daemon's WorkspaceManager. With `force: true` (the
// default matching standalone MCP) the workspace is unloaded first.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_loads_workspace() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    // Call rebuild_index with force=true (explicit).
    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await
        .expect("rebuild_index must succeed for valid directory");

    assert!(
        result.is_error != Some(true),
        "rebuild_index must not set is_error=true; got: {result:?}"
    );

    // The daemon-hosted `rebuild_index` response shape is identical to
    // standalone sqry-mcp (envelope: `data`, `execution_ms`,
    // `used_graph`, `total`, `truncated`, `workspace_path`). Anything
    // daemon-specific would break response-shape parity for both
    // `structured_content` and `content[0].text` consumers.
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured_content must be present");

    let data = structured.get("data").expect("envelope must carry `data`");
    assert_eq!(
        data.get("success"),
        Some(&json!(true)),
        "rebuild_index must return data.success=true"
    );
    assert!(
        data.get("rootPath").and_then(|v| v.as_str()).is_some(),
        "rebuild_index must include data.rootPath; got: {data:?}"
    );
    assert!(
        data.get("nodeCount")
            .and_then(serde_json::Value::as_u64)
            .is_some(),
        "rebuild_index must include data.nodeCount; got: {data:?}"
    );
    assert!(
        data.get("builtAt").and_then(|v| v.as_str()).is_some(),
        "rebuild_index must include data.builtAt; got: {data:?}"
    );
    assert_eq!(
        structured.get("used_graph"),
        Some(&json!(true)),
        "envelope must set used_graph=true"
    );
    assert!(
        structured
            .get("workspace_path")
            .and_then(|v| v.as_str())
            .is_some(),
        "envelope must include workspace_path; got: {structured:?}"
    );
    // Daemon-specific keys that existed in the pre-parity response
    // shape must NOT leak through — they were never part of the
    // standalone contract.
    assert!(
        structured.get("daemonMode").is_none(),
        "envelope must not leak daemon-specific keys; got: {structured:?}"
    );
    assert!(
        structured.get("currentBytes").is_none(),
        "envelope must not leak daemon-specific keys; got: {structured:?}"
    );

    // Verify the workspace is now loaded in the manager.
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::default(), 0);
    let ws = server
        .manager
        .lookup(&key)
        .expect("workspace must be present after rebuild_index");
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Loaded,
        "workspace must be in Loaded state after rebuild_index"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 10: mcp_host_rebuild_index_force_default_is_true
//
// When `force` is omitted from the arguments, the daemon MCP handler
// must default to `true` — matching the standalone MCP
// `RebuildIndexParams` schema where `force` defaults to `default_true`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_force_default_is_true() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();

    // Pre-load the workspace so we can verify that omitting `force`
    // still triggers a reload (i.e., force defaults to true).
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::default(), 0);
    server
        .manager
        .insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    // Call rebuild_index WITHOUT the force argument — must default to true.
    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([(
                "path".to_string(),
                json!(canon.to_string_lossy().as_ref()),
            )]),
        ))
        .await
        .expect("rebuild_index must succeed when force is omitted");

    assert!(
        result.is_error != Some(true),
        "rebuild_index with default force must not error"
    );

    let structured = result
        .structured_content
        .as_ref()
        .expect("structured_content must be present");
    // Response must use the standalone envelope shape (data-under-`data`).
    let data = structured
        .get("data")
        .expect("envelope must carry `data` (parity with standalone sqry-mcp)");
    assert_eq!(data.get("success"), Some(&json!(true)));

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 11: mcp_host_rebuild_index_invalid_path_returns_error
//
// A non-existent path must produce an MCP error, not a panic or
// success response.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_invalid_path_returns_error() {
    let server = TestServer::new().await;

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    // Call rebuild_index with a path that does not exist.
    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([(
                "path".to_string(),
                json!("/nonexistent/path/that/does/not/exist"),
            )]),
        ))
        .await;

    // The call must fail with an MCP error (not a success).
    assert!(
        result.is_err(),
        "rebuild_index with nonexistent path must return MCP error"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 12: mcp_host_rebuild_index_accepts_file_path
//
// Parity with standalone sqry-mcp: `rebuild_index` accepts a file path
// and uses the file's parent directory as the effective workspace root.
// Regression test for Codex gpt-5.4 iter-0 finding 3 (MAJOR).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_accepts_file_path() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("lib.rs");
    std::fs::write(&file_path, "fn main() {}").expect("write fixture");
    let canonical_file = canonicalize_path(&file_path).unwrap();
    let canonical_dir = canonicalize_path(dir.path()).unwrap();

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                (
                    "path".to_string(),
                    json!(canonical_file.to_string_lossy().as_ref()),
                ),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await
        .expect("rebuild_index must accept a file path (standalone parity)");

    assert!(result.is_error != Some(true));

    let structured = result
        .structured_content
        .as_ref()
        .expect("structured_content must be present");
    let data = structured.get("data").expect("envelope must carry data");
    let reported_root = data
        .get("rootPath")
        .and_then(|v| v.as_str())
        .expect("data.rootPath must be a string");
    // rootPath must resolve to the parent directory of the file arg.
    let expected = canonical_dir.to_string_lossy().replace('\\', "/");
    assert_eq!(
        reported_root, expected,
        "data.rootPath must equal the parent directory of the file argument"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 13: mcp_host_rebuild_index_rejects_non_absolute_path
//
// #566: unlike standalone sqry-mcp (which resolves an omitted/`.` path
// against the client launch directory via the MCP `roots/list` callback),
// the daemon has no client working directory. A relative or omitted
// `rebuild_index` path would otherwise canonicalize against the daemon's
// own process CWD ($HOME under the systemd user unit), silently targeting
// the wrong workspace. The daemon must reject both with a clear
// `InvalidArgument`. (Supersedes the former
// `mcp_host_rebuild_index_defaults_path_to_dot` parity test, which encoded
// the pre-#566 silent-CWD behavior.)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_rejects_non_absolute_path() {
    let server = TestServer::new().await;

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    // (a) Omitted `path` is rejected (no client CWD to default to).
    let omitted = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([("force".to_string(), json!(true))]),
        ))
        .await;
    match omitted {
        Err(rmcp::ServiceError::McpError(mcp_err)) => {
            let reason = mcp_err
                .data
                .as_ref()
                .and_then(|d| d["details"]["reason"].as_str())
                .expect("details.reason must be a string")
                .to_string();
            assert!(
                reason.contains("required") && reason.contains("daemon mode"),
                "omitted `path` must be rejected as required in daemon mode; got: {reason}"
            );
        }
        other => panic!("expected InvalidArgument McpError for omitted path, got: {other:?}"),
    }

    // (b) A present-but-relative `path` is rejected (it would otherwise
    //     canonicalize against the daemon's CWD).
    let relative = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!("some/relative/dir")),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await;
    match relative {
        Err(rmcp::ServiceError::McpError(mcp_err)) => {
            let reason = mcp_err
                .data
                .as_ref()
                .and_then(|d| d["details"]["reason"].as_str())
                .expect("details.reason must be a string")
                .to_string();
            assert!(
                reason.contains("absolute"),
                "relative `path` must be rejected as non-absolute; got: {reason}"
            );
        }
        other => panic!("expected InvalidArgument McpError for relative path, got: {other:?}"),
    }

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 14: mcp_host_rebuild_index_cache_hit_preserves_built_at
//
// When `force=false` and an on-disk index already exists at the
// resolved root, the daemon must return the existing manifest's
// `built_at` timestamp and an "already exists" message — NOT claim a
// fresh `builtAt = now()`. Regression test for Codex gpt-5.4 iter-0
// finding 4 (MINOR).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_cache_hit_preserves_built_at() {
    use sqry_core::graph::unified::persistence::manifest::{BuildProvenance, Manifest};

    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();

    // Seed a deterministic `.sqry/graph/manifest.json` directly — the
    // TestServer uses `EmptyGraphBuilder` which does not persist, so
    // going through `rebuild_index(force=true)` would leave no
    // manifest behind. Writing the manifest by hand gives the
    // cache-hit path something concrete to read back.
    let ground_truth_built_at = "2026-01-01T00:00:00Z".to_string();
    let graph_dir = canon.join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).expect("create .sqry/graph");
    let provenance =
        BuildProvenance::new("9.0.0-test-fixture", "test/ipc_shim_mcp_host::cache_hit");
    let manifest = Manifest {
        schema_version: 1,
        snapshot_format_version: 2,
        built_at: ground_truth_built_at.clone(),
        root_path: canon.to_string_lossy().into_owned(),
        node_count: 42,
        edge_count: 17,
        raw_edge_count: None,
        snapshot_sha256: "0".repeat(64),
        build_provenance: provenance,
        file_count: std::collections::HashMap::new(),
        languages: Vec::new(),
        config: std::collections::HashMap::new(),
        confidence: std::collections::HashMap::new(),
        last_indexed_commit: None,
        plugin_selection: None,
        macro_options: None,
    };
    manifest
        .save(graph_dir.join("manifest.json"))
        .expect("write seed manifest");
    // An index is its manifest and its snapshot (round 7: a manifest alone
    // is built, not reported). The cache-hit leg reads no snapshot bytes,
    // so a placeholder file is enough.
    std::fs::write(graph_dir.join("snapshot.sqry"), b"placeholder").expect("write snapshot");

    // Issue rebuild_index with `force=false`. Must return the
    // existing manifest's built_at (no fresh `now()`-stamp).
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let cache_hit = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!(false)),
            ]),
        ))
        .await
        .expect("rebuild_index with force=false must succeed on existing index");
    drop(running);

    let data = cache_hit
        .structured_content
        .as_ref()
        .and_then(|s| s.get("data").cloned())
        .expect("cache-hit envelope must carry data");
    let reported_built_at = data
        .get("builtAt")
        .and_then(|v| v.as_str())
        .expect("data.builtAt must be a string");
    assert_eq!(
        reported_built_at, ground_truth_built_at,
        "cache-hit must preserve the manifest's built_at, not synthesize now()"
    );
    assert_eq!(
        data.get("nodeCount"),
        Some(&json!(42)),
        "cache-hit nodeCount must come from the manifest"
    );
    assert_eq!(
        data.get("edgeCount"),
        Some(&json!(17)),
        "cache-hit edgeCount must come from the manifest"
    );
    let reported_msg = data
        .get("message")
        .and_then(|v| v.as_str())
        .expect("data.message must be a string");
    assert!(
        reported_msg.contains("already exists"),
        "cache-hit message must reflect the no-op path, got: {reported_msg:?}"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 15: mcp_host_rebuild_index_non_boolean_force_rejected
//
// Standalone `RebuildIndexParams::force` is a typed `bool`, so serde
// rejects `"force": "yes"` with an invalid-type error. The daemon
// handler mirrors that strictness rather than silently defaulting to
// `true` on an ill-typed value. Coverage note from Codex gpt-5.4
// iter-1.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_non_boolean_force_rejected() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!("yes")),
            ]),
        ))
        .await;
    assert!(
        result.is_err(),
        "rebuild_index must reject non-boolean `force` with an MCP error"
    );

    // Non-string `path` must also be rejected (typed strictness parity).
    let result_bad_path = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([("path".to_string(), json!(42))]),
        ))
        .await;
    assert!(
        result_bad_path.is_err(),
        "rebuild_index must reject non-string `path` with an MCP error"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 16: mcp_host_rebuild_index_cache_hit_unreadable_manifest_errors
//
// If `.sqry/graph/` exists but the manifest is malformed (e.g., empty
// or non-JSON), the daemon must surface a structured MCP error rather
// than panic or silently rebuild. Mirrors standalone's
// `.context("Index exists but manifest is unreadable")`. Coverage note
// from Codex gpt-5.4 iter-1.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_cache_hit_unreadable_manifest_errors() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();

    // Create `.sqry/graph/manifest.json` with invalid JSON so
    // `storage.exists()` is true but `load_manifest()` fails.
    let graph_dir = canon.join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).expect("create .sqry/graph");
    std::fs::write(graph_dir.join("manifest.json"), b"not-valid-json")
        .expect("write malformed manifest");

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!(false)),
            ]),
        ))
        .await;
    assert!(
        result.is_err(),
        "rebuild_index must surface a structured error when the manifest is unreadable"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 17 (Unix only): symlink-resolved file paths rebuild the symlink
// target's parent directory.
//
// `std::fs::canonicalize` follows symlinks, so a symlinked file must
// canonicalize to the real file's parent (not the symlink's parent).
// Parity with standalone, where `canonicalize_in_workspace` drives the
// same semantics. Coverage note from Codex gpt-5.4 iter-1.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_symlinked_file_resolves_to_target_parent() {
    use std::os::unix::fs::symlink;

    let server = TestServer::new().await;
    let real_dir = tempfile::tempdir().unwrap();
    let real_canon = canonicalize_path(real_dir.path()).unwrap();
    let real_file = real_dir.path().join("target.rs");
    std::fs::write(&real_file, "fn main() {}").expect("write real target");

    let link_dir = tempfile::tempdir().unwrap();
    let symlink_path = link_dir.path().join("alias.rs");
    symlink(&real_file, &symlink_path).expect("create symlink");

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                (
                    "path".to_string(),
                    json!(symlink_path.to_string_lossy().as_ref()),
                ),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await
        .expect("rebuild_index must accept a symlinked file (standalone parity)");

    let data = result
        .structured_content
        .as_ref()
        .and_then(|s| s.get("data").cloned())
        .expect("envelope must carry data");
    let reported = data
        .get("rootPath")
        .and_then(|v| v.as_str())
        .expect("data.rootPath must be a string");
    let expected = real_canon.to_string_lossy().replace('\\', "/");
    assert_eq!(
        reported, expected,
        "symlinked file must resolve to the real file's parent directory"
    );

    drop(running);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Surface parity W4 (design W4-D8) and decision D-i7-2: the daemon-hosted
// `rebuild_index` accepts `cfg_flags`, `expand_cache` and
// `reset_macro_options`, builds with them, records them in the index it
// persists (as the standalone `rebuild_index` and `daemon/rebuild` do),
// advertises them, and refuses a recorded expand cache directory that no
// longer exists through `daemon_err_to_mcp`. The oracles are the resident
// graph's cfg activation, the manifest's record, and the snapshot as the CLI
// loads it.
// ---------------------------------------------------------------------------

const W4_CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";

/// Index `root` in-process with the fast-path roster and `macro_options`,
/// so the manifest records them (W4-D6) for the daemon host to reuse.
fn w4_index_with_macro_options(
    root: &std::path::Path,
    macro_options: sqry_core::graph::unified::build::MacroBuildOptions,
) {
    w4_write_source(root);
    let plugins = sqry_plugin_registry::create_plugin_manager();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    let config = sqry_core::graph::unified::build::BuildConfig {
        macro_options,
        ..sqry_core::graph::unified::build::BuildConfig::default()
    };
    sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
        root,
        &plugins,
        &config,
        "test:w4_mcp_host",
        Some(
            sqry_core::graph::unified::persistence::PluginSelectionManifest {
                active_plugin_ids: ids,
                high_cost_mode: Some("fast_path_default".to_string()),
            },
        ),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("index persists");
}

/// Write the fixture's source (a `cfg(test)` item beside an ungated one)
/// under `root`, without indexing it.
fn w4_write_source(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(root.join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
}

fn w4_recorded_macro_options(
    root: &std::path::Path,
) -> Option<sqry_core::graph::unified::persistence::MacroOptionsManifest> {
    sqry_core::graph::unified::persistence::GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
}

/// The cfg flags the manifest at `root` records, `None` with no record.
fn w4_recorded_cfg_flags(root: &std::path::Path) -> Option<Vec<String>> {
    w4_recorded_macro_options(root).map(|record| record.cfg_flags)
}

/// The activation `graph` records for the fixture's `cfg(test)` item.
fn w4_cfg_test_activation(graph: &sqry_core::graph::CodeGraph) -> Option<bool> {
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

/// The activation the resident graph of `root` records for the fixture's
/// `cfg(test)` item.
fn w4_resident_cfg_test_activation(server: &TestServer, root: &std::path::Path) -> Option<bool> {
    let key = WorkspaceKey::new(root.to_path_buf(), ProjectRootMode::default(), 0);
    let ws = server
        .manager
        .lookup(&key)
        .expect("workspace resident after rebuild_index");
    w4_cfg_test_activation(&ws.graph())
}

/// The activation the snapshot on disk records for the fixture's
/// `cfg(test)` item, loaded the way the CLI loads it
/// (`load_from_path` with the plugin manager).
fn w4_snapshot_cfg_test_activation(root: &std::path::Path) -> Option<bool> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let plugins = sqry_plugin_registry::create_plugin_manager();
    let graph = sqry_core::graph::unified::persistence::load_from_path(
        storage.snapshot_path(),
        Some(&plugins),
    )
    .expect("the snapshot loads");
    w4_cfg_test_activation(&graph)
}

async fn w4_real_builder_server() -> TestServer {
    w4_real_builder_server_with(DaemonConfig::default()).await
}

async fn w4_real_builder_server_with(config: DaemonConfig) -> TestServer {
    let resolver = Arc::new(sqry_daemon::WorkspaceRosterResolver::new());
    let builder: Arc<dyn sqry_daemon::WorkspaceBuilder> = Arc::new(
        sqry_daemon::RealWorkspaceBuilder::new(Arc::clone(&resolver)),
    );
    TestServer::with_builder_config_and_roster(builder, config, resolver).await
}

fn w4_index_bytes(root: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    (
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        std::fs::read(storage.snapshot_path()).expect("snapshot bytes"),
    )
}

/// The command the manifest at `root` says built the index.
fn w4_build_command(root: &std::path::Path) -> String {
    sqry_core::graph::unified::persistence::GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .build_provenance
        .build_command
}

/// An IPC client past the hello handshake.
async fn w4_ipc_client(server: &TestServer) -> TestIpcClient {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client
}

/// `daemon/rebuild` of `root` with `force: true` and `extra` parameters.
async fn w4_ipc_rebuild(
    client: &mut TestIpcClient,
    root: &std::path::Path,
    extra: &[(&str, serde_json::Value)],
) -> sqry_daemon::ipc::protocol::JsonRpcResponse {
    let mut params = serde_json::Map::from_iter([
        ("path".to_string(), json!(root.to_string_lossy().as_ref())),
        ("force".to_string(), json!(true)),
    ]);
    for (name, value) in extra {
        params.insert((*name).to_string(), value.clone());
    }
    client
        .request("daemon/rebuild", serde_json::Value::Object(params))
        .await
}

/// The `daemon/status` row of `root`.
async fn w4_status_row(client: &mut TestIpcClient, root: &std::path::Path) -> serde_json::Value {
    let resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&resp);
    let wanted = root.to_string_lossy().to_string();
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(wanted.as_str()))
        })
        .cloned()
        .unwrap_or_else(|| panic!("a status row for {wanted}: {status}"))
}

/// `rebuild_index` with `path`, `force` and `extra` arguments.
async fn w4_rebuild_index(
    peer: &rmcp::Peer<rmcp::RoleClient>,
    root: &std::path::Path,
    force: bool,
    extra: &[(&str, serde_json::Value)],
) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
    let mut arguments = serde_json::Map::from_iter([
        ("path".to_string(), json!(root.to_string_lossy().as_ref())),
        ("force".to_string(), json!(force)),
    ]);
    for (name, value) in extra {
        arguments.insert((*name).to_string(), value.clone());
    }
    peer.call_tool(call_tool_request("rebuild_index", arguments))
        .await
}

/// The MCP error of a refused tool call.
fn w4_mcp_error(
    label: &str,
    outcome: Result<rmcp::model::CallToolResult, rmcp::ServiceError>,
) -> rmcp::ErrorData {
    match outcome {
        Ok(result) => panic!("{label}: the call must be refused, got {result:?}"),
        Err(rmcp::ServiceError::McpError(err)) => err,
        Err(other) => panic!("{label}: expected an MCP error envelope, got {other:?}"),
    }
}

/// The message both legs of `rebuild_index` give for macro options beside
/// `force=false` when a graph exists (on disk or resident): F3.
fn w4_need_force_message(root: &std::path::Path) -> String {
    format!(
        "invalid argument: rebuild_index: cfg_flags, expand_cache and reset_macro_options need \
         force=true when a graph already exists at {} (an index on disk or a workspace loaded \
         in the daemon); nothing was built",
        root.display()
    )
}

/// T13 (daemon-hosted leg) and T15 (daemon-hosted `rebuild_index`), as
/// decision D-i7-2 settles them: the daemon-hosted `rebuild_index` accepts
/// the macro arguments, builds with them, and records them in the index it
/// persists. With nothing resident (the load leg) a forced rebuild without
/// arguments reuses the record and persists the index; resident (the
/// in-place leg) `cfg_flags` replaces the record; a plain `daemon/rebuild`
/// afterwards, and a load after an unload, keep what was recorded;
/// `reset_macro_options` drops the record, and a plain `daemon/rebuild`
/// after it records nothing. A wrong type and a macro argument beside
/// `force=false` over an existing index are refused and write nothing.
/// Before D-i7-2 the daemon-hosted surface built with the options but never
/// recorded them, so the next plain rebuild dropped them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_accepts_records_and_reuses_the_macro_options() {
    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    w4_index_with_macro_options(
        &canon,
        sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: vec!["test".to_string()],
            expand_cache_dir: None,
        },
    );
    assert_eq!(
        w4_recorded_cfg_flags(&canon),
        Some(vec!["test".to_string()]),
        "fixture precondition: the manifest records the flag"
    );
    assert_eq!(w4_build_command(&canon), "test:w4_mcp_host");
    let mut client = w4_ipc_client(&server).await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let peer = running.peer();
    let assert_options = |leg: &str, activation: Option<bool>, flags: Option<Vec<String>>| {
        assert_eq!(
            w4_resident_cfg_test_activation(&server, &canon),
            activation,
            "{leg}: the resident graph"
        );
        assert_eq!(
            w4_snapshot_cfg_test_activation(&canon),
            activation,
            "{leg}: the snapshot the CLI loads"
        );
        assert_eq!(
            w4_recorded_cfg_flags(&canon),
            flags,
            "{leg}: the manifest's record"
        );
    };

    // Load leg, no arguments: the record is reused and the index persisted.
    let result = w4_rebuild_index(peer, &canon, true, &[])
        .await
        .expect("rebuild_index without macro arguments must succeed");
    assert!(result.is_error != Some(true), "{result:?}");
    assert_eq!(
        w4_build_command(&canon),
        "daemon:rebuild_index",
        "the daemon-hosted rebuild_index persisted the index"
    );
    assert_options("load leg", Some(true), Some(vec!["test".to_string()]));

    // In-place leg: explicit flags replace the record.
    let result = w4_rebuild_index(peer, &canon, true, &[("cfg_flags", json!(["feature=w4"]))])
        .await
        .expect("rebuild_index with cfg_flags must succeed");
    assert!(result.is_error != Some(true), "{result:?}");
    assert_options(
        "in-place leg",
        Some(false),
        Some(vec!["feature=w4".to_string()]),
    );

    // A plain `daemon/rebuild` reuses what rebuild_index recorded.
    let resp = w4_ipc_rebuild(&mut client, &canon, &[]).await;
    expect_success(&resp);
    assert_options(
        "plain daemon/rebuild",
        Some(false),
        Some(vec!["feature=w4".to_string()]),
    );

    // So does a load after an unload (the load leg, no arguments).
    let resp = client
        .request(
            "daemon/unload",
            json!({ "index_root": canon.to_string_lossy().as_ref() }),
        )
        .await;
    expect_success(&resp);
    let result = w4_rebuild_index(peer, &canon, true, &[])
        .await
        .expect("rebuild_index after an unload must succeed");
    assert!(result.is_error != Some(true), "{result:?}");
    assert_options(
        "load leg after an unload",
        Some(false),
        Some(vec!["feature=w4".to_string()]),
    );

    // A wrong type is refused, never coerced or dropped; a macro argument
    // beside force=false over an existing index reaches no build and is
    // refused instead of being dropped. Neither writes anything.
    let index_before = w4_index_bytes(&canon);
    let refused = w4_rebuild_index(peer, &canon, true, &[("cfg_flags", json!("test"))]).await;
    assert!(
        refused.is_err(),
        "a string cfg_flags must be refused: {refused:?}"
    );
    let err = w4_mcp_error(
        "reset without force",
        w4_rebuild_index(peer, &canon, false, &[("reset_macro_options", json!(true))]).await,
    );
    assert_eq!(err.message, w4_need_force_message(&canon));
    assert_eq!(
        w4_index_bytes(&canon),
        index_before,
        "the refusals write nothing"
    );

    // Reset builds without any options and drops the record.
    let result = w4_rebuild_index(peer, &canon, true, &[("reset_macro_options", json!(true))])
        .await
        .expect("rebuild_index with reset must succeed");
    assert!(result.is_error != Some(true), "{result:?}");
    assert_options("reset", None, None);

    // A plain `daemon/rebuild` after the reset records nothing again.
    let resp = w4_ipc_rebuild(&mut client, &canon, &[]).await;
    expect_success(&resp);
    assert_options("plain daemon/rebuild after the reset", None, None);

    drop(running);
    drop(client);
    server.stop().await;
}

/// T13 (schema, daemon host): `tools/list` on the daemon host advertises
/// the three arguments on `rebuild_index`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_tools_list_advertises_the_rebuild_index_macro_arguments() {
    let server = TestServer::new().await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let list_result = running
        .peer()
        .list_tools(None)
        .await
        .expect("list_tools must succeed");
    let tool = list_result
        .tools
        .iter()
        .find(|t| t.name.as_ref() == "rebuild_index")
        .expect("rebuild_index advertised");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("rebuild_index schema has properties");
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();
    println!("rebuild_index schema properties (daemon host): {names:?}");
    for name in [
        "cfg_flags",
        "expand_cache",
        "reset_macro_options",
        "path",
        "force",
    ] {
        assert!(
            properties.contains_key(name),
            "rebuild_index schema must advertise {name}: {names:?}"
        );
    }
    assert_eq!(properties.len(), 5, "exactly the five arguments: {names:?}");

    drop(running);
    server.stop().await;
}

/// T14 (daemon-hosted `rebuild_index` leg): a recorded expand cache
/// directory that no longer exists is refused with the standalone server's
/// envelope (S10, round 7: the `-32022` kind, the directory, its origin and
/// the `rebuild_index` arguments that drop the record); nothing on disk
/// changes; `reset_macro_options` is the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_refuses_a_missing_recorded_expand_cache() {
    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    let cache = canon.join("expand-cache");
    std::fs::create_dir_all(&cache).unwrap();
    w4_index_with_macro_options(
        &canon,
        sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: vec!["test".to_string()],
            expand_cache_dir: Some(cache.clone()),
        },
    );
    let recorded_dir = w4_recorded_macro_options(&canon)
        .and_then(|record| record.expand_cache_dir)
        .expect("the manifest names the expand cache directory");
    assert_eq!(std::path::Path::new(&recorded_dir), cache.as_path());
    std::fs::remove_dir_all(&cache).unwrap();
    let index_before = w4_index_bytes(&canon);
    let path_arg = json!(canon.to_string_lossy().as_ref());

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");

    let refused = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), path_arg.clone()),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await;
    let err = match refused {
        Err(err) => err,
        Ok(result) => panic!("a missing recorded expand cache must be refused: {result:?}"),
    };
    let rendered = format!("{err:?}");
    println!("refusal: {rendered}");
    let rmcp::ServiceError::McpError(err) = err else {
        panic!("expected a JSON-RPC error: {rendered}");
    };
    // The standalone server's envelope, its remedy echoing the request's
    // own `path` so it can be sent back as it stands.
    let shared = sqry_mcp::error::rpc_error_to_mcp(
        sqry_mcp::error::RpcError::rebuild_macro_options_unavailable(
            &canon,
            &cache,
            sqry_mcp::error::ExpandCacheOrigin::Recorded,
        )
        .with_reset_path(path_arg.as_str().expect("a string path")),
    );
    assert_eq!(
        (err.code, err.message.as_ref(), &err.data),
        (shared.code, shared.message.as_ref(), &shared.data),
        "the standalone server's envelope"
    );
    let reset = &err.data.as_ref().expect("data")["details"]["reset_arguments"];
    assert_eq!(
        reset,
        &json!({ "path": path_arg, "force": true, "reset_macro_options": true }),
        "the remedy's arguments are the request's own path, force and the reset"
    );
    assert!(
        err.message.contains(&recorded_dir) && err.message.contains("reset_macro_options"),
        "the message names the directory and the way out: {}",
        err.message
    );
    assert_eq!(
        w4_index_bytes(&canon),
        index_before,
        "the index bytes are untouched"
    );

    // The way out on the same surface.
    let result = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), path_arg),
                ("force".to_string(), json!(true)),
                ("reset_macro_options".to_string(), json!(true)),
            ]),
        ))
        .await
        .expect("rebuild_index with reset must succeed");
    assert!(result.is_error != Some(true), "{result:?}");
    assert_eq!(w4_resident_cfg_test_activation(&server, &canon), None);

    // The in-place route answers the same refusal: the workspace is
    // resident now, so a forced rebuild runs through the dispatcher, whose
    // refusal of a recorded directory that is gone echoes the request's own
    // `path` as it was sent (here not the canonical root).
    let second = canon.join("expand-cache-2");
    std::fs::create_dir_all(&second).unwrap();
    let recorded = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!(true)),
                (
                    "expand_cache".to_string(),
                    json!(second.to_string_lossy().as_ref()),
                ),
            ]),
        ))
        .await
        .expect("an in-place rebuild recording a second expand cache");
    assert!(recorded.is_error != Some(true), "{recorded:?}");
    assert_eq!(
        w4_recorded_macro_options(&canon)
            .and_then(|record| record.expand_cache_dir)
            .map(std::path::PathBuf::from),
        Some(second.clone()),
        "the in-place rebuild recorded the second directory"
    );
    std::fs::remove_dir_all(&second).unwrap();
    let as_sent = format!("{}/.", canon.to_string_lossy());
    let refused = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(as_sent)),
                ("force".to_string(), json!(true)),
            ]),
        ))
        .await;
    let Err(rmcp::ServiceError::McpError(err)) = refused else {
        panic!("the in-place route must refuse a recorded directory that is gone: {refused:?}");
    };
    let details = &err.data.as_ref().expect("data")["details"];
    assert_eq!(details["origin"], json!("recorded"), "{details}");
    assert_eq!(
        details["reset_arguments"],
        json!({ "path": as_sent, "force": true, "reset_macro_options": true }),
        "the in-place route's remedy echoes the path as sent"
    );
    assert!(
        server
            .manager
            .lookup(&WorkspaceKey::new(
                canon.clone(),
                ProjectRootMode::default(),
                0
            ))
            .is_some(),
        "the refused rebuild leaves the workspace resident"
    );

    drop(running);
    server.stop().await;
}

/// Integration of W1 and W4: daemon-hosted `rebuild_index` with an empty
/// `expand_cache` is refused and writes nothing; an empty directory joined to
/// the workspace root named the root itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_refuses_an_empty_expand_cache() {
    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    w4_index_with_macro_options(
        &canon,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    let index_before = w4_index_bytes(&canon);

    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let refused = running
        .peer()
        .call_tool(call_tool_request(
            "rebuild_index",
            serde_json::Map::from_iter([
                ("path".to_string(), json!(canon.to_string_lossy().as_ref())),
                ("force".to_string(), json!(true)),
                ("expand_cache".to_string(), json!("")),
            ]),
        ))
        .await;
    let err = match refused {
        Err(err) => err,
        Ok(result) => panic!("an empty expand cache must be refused: {result:?}"),
    };
    let rendered = format!("{err:?}");
    println!("refusal: {rendered}");
    assert!(
        rendered.contains("expand cache directory is empty"),
        "the MCP error says why: {rendered}"
    );
    assert_eq!(
        w4_index_bytes(&canon),
        index_before,
        "the index bytes are untouched"
    );
    assert!(
        w4_recorded_macro_options(&canon).is_none(),
        "no expand cache was recorded"
    );

    drop(running);
    server.stop().await;
}

/// The semantic_search arguments the refusal tests query the resident
/// workspace with.
fn w4_query_args(root: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("query".to_string(), json!("always_present")),
        ("path".to_string(), json!(root.to_string_lossy().as_ref())),
        ("max_results".to_string(), json!(5)),
        ("context_lines".to_string(), json!(0)),
        ("include_classpath".to_string(), json!(false)),
    ])
}

/// Assert a refused rebuild left the workspace at `key` as it was: `Loaded`,
/// the same graph, still watched, no recorded failure (no backoff), and,
/// when `query` is set, a query still served. A query classifies the
/// manifest on its own, so after a roster refusal (the manifest itself is
/// what is refused) the caller passes `false`.
async fn w4_assert_kept(
    label: &str,
    server: &TestServer,
    client: &mut TestIpcClient,
    peer: &rmcp::Peer<rmcp::RoleClient>,
    key: &WorkspaceKey,
    graph_before: &Arc<sqry_core::graph::CodeGraph>,
    query: bool,
) {
    let ws = server
        .manager
        .lookup(key)
        .unwrap_or_else(|| panic!("{label}: the workspace must stay resident"));
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Loaded,
        "{label}: the workspace must stay Loaded"
    );
    assert!(
        Arc::ptr_eq(graph_before, &ws.graph()),
        "{label}: the resident graph must be the one published before"
    );
    let row = w4_status_row(client, &key.source_root).await;
    assert_eq!(row["state"], json!("Loaded"), "{label}: {row}");
    assert_eq!(
        row["watching"],
        json!(true),
        "{label}: still watched: {row}"
    );
    assert!(
        row["last_error"].is_null(),
        "{label}: no recorded failure: {row}"
    );
    assert_eq!(row["retry_count"], json!(0), "{label}: no backoff: {row}");
    if !query {
        return;
    }
    let query = peer
        .call_tool(call_tool_request(
            "semantic_search",
            w4_query_args(&key.source_root),
        ))
        .await
        .unwrap_or_else(|err| panic!("{label}: a query after the refusal fails: {err:?}"));
    assert!(query.is_error != Some(true), "{label}: {query:?}");
}

/// Send the same refused rebuild to `daemon/rebuild` and to the
/// daemon-hosted `rebuild_index`, assert both refuse it with the same
/// message and the expected codes, and that each refusal kept the
/// workspace. Returns the shared message.
#[allow(clippy::too_many_arguments)]
async fn w4_refused_on_both_surfaces(
    label: &str,
    server: &TestServer,
    client: &mut TestIpcClient,
    peer: &rmcp::Peer<rmcp::RoleClient>,
    key: &WorkspaceKey,
    graph_before: &Arc<sqry_core::graph::CodeGraph>,
    extra: &[(&str, serde_json::Value)],
    ipc_code: i32,
    mcp_code: i32,
    query: bool,
) -> String {
    let resp = w4_ipc_rebuild(client, &key.source_root, extra).await;
    let ipc_err = expect_error(&resp).clone();
    assert_eq!(
        ipc_err.code, ipc_code,
        "{label}: daemon/rebuild: {ipc_err:?}"
    );
    w4_assert_kept(
        &format!("{label} (daemon/rebuild)"),
        server,
        client,
        peer,
        key,
        graph_before,
        query,
    )
    .await;

    let mcp_err = w4_mcp_error(
        label,
        w4_rebuild_index(peer, &key.source_root, true, extra).await,
    );
    assert_eq!(
        mcp_err.code.0, mcp_code,
        "{label}: rebuild_index: {mcp_err:?}"
    );
    w4_assert_kept(
        &format!("{label} (rebuild_index)"),
        server,
        client,
        peer,
        key,
        graph_before,
        query,
    )
    .await;
    // The same refusal on both surfaces. An unusable expand cache (`-32022`
    // on IPC) is the one whose remedy differs by audience (S10, round 7):
    // IPC names the CLI flags beside the wire fields, the daemon-hosted MCP
    // sends the standalone server's text. Both state the same reason.
    if ipc_code == -32022 {
        let reason = |message: &str| message.split("; ").next().unwrap_or_default().to_string();
        assert_eq!(
            reason(&mcp_err.message),
            reason(&ipc_err.message),
            "{label}: both surfaces give the same reason"
        );
    } else {
        assert_eq!(
            mcp_err.message, ipc_err.message,
            "{label}: both surfaces give the same refusal"
        );
    }
    println!("{label}: ipc: {}", ipc_err.message);
    println!("{label}: mcp: {}", mcp_err.message);
    ipc_err.message
}

/// Integration of W1 and W4, F2 and F6: a refused rebuild of a resident
/// workspace leaves it as it was, and `daemon/rebuild` and the
/// daemon-hosted `rebuild_index` give the same refusal. Both rebuild a
/// resident workspace in place through the rebuild dispatcher, which
/// resolves every input it refuses before it reserves memory, and nothing
/// unloads the workspace. Covers the three macro-option refusals (an
/// empty, a missing and an unrecordable expand cache, the last a UTF-8
/// symlink whose canonical target is not valid UTF-8), a valid control,
/// and the two roster refusals: a readable manifest naming a plugin id
/// this binary did not compile, which only the roster resolution refuses,
/// and an unreadable manifest, which `daemon/rebuild` refuses and the
/// forced `rebuild_index` repairs (D-i7-8). After each
/// refusal on each surface the workspace is `Loaded` with the same graph,
/// still watched, with no recorded failure, and serves a query. The
/// narrowing and memory budget refusals need their own fixtures
/// (`rebuild_index_in_place.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_refusals_keep_the_resident_workspace() {
    use std::os::unix::ffi::OsStringExt;

    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    // A git repository, which the file watcher requires.
    support::init_git_repo(&canon);
    w4_index_with_macro_options(
        &canon,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::default(), 0);
    // The expand cache fixtures live outside the workspace, so creating
    // them does not wake the watcher.
    let outside = tempfile::tempdir().unwrap();
    let outside = canonicalize_path(outside.path()).unwrap();
    let missing = outside.join("no-such-expand-cache");
    let mut name = b"expand-".to_vec();
    name.push(0xff);
    let not_utf8 = outside.join(std::ffi::OsString::from_vec(name));
    std::fs::create_dir_all(&not_utf8).expect("a non-UTF-8 directory");
    let link = outside.join("expand-link");
    std::os::unix::fs::symlink(&not_utf8, &link).expect("a UTF-8 link to it");

    let mut client = w4_ipc_client(&server).await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let peer = running.peer();

    // `daemon/load` makes the workspace resident and watched.
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": canon.to_string_lossy().as_ref() }),
        )
        .await;
    expect_success(&resp);
    let graph_before = server
        .manager
        .lookup(&key)
        .expect("resident after daemon/load")
        .graph();
    w4_assert_kept(
        "loaded",
        &server,
        &mut client,
        peer,
        &key,
        &graph_before,
        true,
    )
    .await;

    let message = w4_refused_on_both_surfaces(
        "empty",
        &server,
        &mut client,
        peer,
        &key,
        &graph_before,
        &[("expand_cache", json!(""))],
        -32602,
        -32602,
        true,
    )
    .await;
    assert!(
        message.contains("expand cache directory is empty"),
        "{message}"
    );
    let message = w4_refused_on_both_surfaces(
        "missing",
        &server,
        &mut client,
        peer,
        &key,
        &graph_before,
        &[("expand_cache", json!(missing.to_string_lossy().as_ref()))],
        -32022,
        -32602,
        true,
    )
    .await;
    // The request named the directory, so the way out is to name one that
    // exists: the wire field beside the CLI flag. Dropping the record would
    // not help, so it is not offered (S10, round 7).
    assert!(
        message.contains(missing.to_string_lossy().as_ref())
            && message.contains("expand_cache (--expand-cache)")
            && !message.contains("--no-macro-options"),
        "{message}"
    );
    let message = w4_refused_on_both_surfaces(
        "unrecordable",
        &server,
        &mut client,
        peer,
        &key,
        &graph_before,
        &[("expand_cache", json!(link.to_string_lossy().as_ref()))],
        -32602,
        -32602,
        true,
    )
    .await;
    assert!(message.contains("not valid UTF-8"), "{message}");

    // The valid control: a rebuild without the refused input rebuilds.
    let rebuilt = w4_rebuild_index(peer, &canon, true, &[])
        .await
        .expect("the valid control succeeds");
    assert!(rebuilt.is_error != Some(true), "{rebuilt:?}");
    let graph_after_control = server
        .manager
        .lookup(&key)
        .expect("resident after the control")
        .graph();
    assert!(
        !Arc::ptr_eq(&graph_before, &graph_after_control),
        "the valid control publishes a new graph"
    );
    w4_assert_kept(
        "control",
        &server,
        &mut client,
        peer,
        &key,
        &graph_after_control,
        true,
    )
    .await;

    // The roster refusals. First a readable manifest naming a plugin id
    // this binary did not compile, which only the roster resolution
    // refuses (the macro options resolution reads the same manifest and
    // accepts it); then an unreadable manifest, which both refuse.
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&canon);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push("integration-w1w4-planted-plugin".to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    let message = w4_refused_on_both_surfaces(
        "uncompiled id",
        &server,
        &mut client,
        peer,
        &key,
        &graph_after_control,
        &[],
        -32005,
        -32603,
        false,
    )
    .await;
    assert!(
        message.contains("integration-w1w4-planted-plugin"),
        "{message}"
    );

    // An unreadable manifest: `daemon/rebuild` refuses it (D9's daemon
    // row), the workspace kept; the daemon-hosted `rebuild_index` with
    // `force` falls back over it and records the fallback, as the
    // standalone `rebuild_index` does (decision D-i7-8).
    std::fs::write(storage.manifest_path(), b"{ not json").expect("corrupt the manifest");
    let resp = w4_ipc_rebuild(&mut client, &canon, &[]).await;
    let ipc_err = expect_error(&resp).clone();
    assert_eq!(ipc_err.code, -32001, "{ipc_err:?}");
    assert!(
        ipc_err
            .message
            .contains(storage.manifest_path().to_string_lossy().as_ref()),
        "{}",
        ipc_err.message
    );
    w4_assert_kept(
        "unreadable manifest (daemon/rebuild)",
        &server,
        &mut client,
        peer,
        &key,
        &graph_after_control,
        false,
    )
    .await;
    let repaired = w4_rebuild_index(peer, &canon, true, &[])
        .await
        .expect("rebuild_index falls back over the unreadable manifest");
    assert!(repaired.is_error != Some(true), "{repaired:?}");
    assert!(
        storage.load_manifest().is_ok(),
        "the manifest is readable again"
    );

    drop(running);
    drop(client);
    server.stop().await;
}

/// F3: a workspace `daemon/load` built in memory has no index on disk, and
/// `rebuild_index` with `force=false` over it builds nothing, so macro
/// options beside it are refused with the message the on-disk cache-hit
/// leg gives, whichever of the three arguments carries them (an existing,
/// an empty and a missing expand cache alike). Without the options it
/// answers "Index already exists" with the resident generation's build
/// time. Before this repair it answered "Index built successfully." with
/// the same graph and the options dropped. The forced rebuild that follows
/// persists the index, after which the on-disk leg refuses the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_without_force_refuses_macro_options_over_a_resident_graph() {
    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    w4_write_source(&canon);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&canon);
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::default(), 0);
    let outside = tempfile::tempdir().unwrap();
    let existing = canonicalize_path(outside.path()).unwrap();
    let missing = existing.join("no-such-expand-cache");

    let mut client = w4_ipc_client(&server).await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let peer = running.peer();

    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": canon.to_string_lossy().as_ref() }),
        )
        .await;
    expect_success(&resp);
    assert!(
        !storage.exists(),
        "fixture precondition: daemon/load writes no index"
    );
    let ws = server.manager.lookup(&key).expect("resident");
    let graph_before = ws.graph();
    assert_eq!(w4_cfg_test_activation(&graph_before), None);

    let need_force = w4_need_force_message(&canon);
    for (label, extra) in [
        ("cfg_flags", ("cfg_flags", json!(["test"]))),
        (
            "existing expand_cache",
            ("expand_cache", json!(existing.to_string_lossy().as_ref())),
        ),
        ("empty expand_cache", ("expand_cache", json!(""))),
        (
            "missing expand_cache",
            ("expand_cache", json!(missing.to_string_lossy().as_ref())),
        ),
        ("reset_macro_options", ("reset_macro_options", json!(true))),
    ] {
        let err = w4_mcp_error(label, w4_rebuild_index(peer, &canon, false, &[extra]).await);
        assert_eq!(err.code.0, -32602, "{label}: {err:?}");
        assert_eq!(err.message, need_force, "{label}");
        assert!(
            Arc::ptr_eq(&graph_before, &ws.graph()),
            "{label}: nothing was built"
        );
        assert!(!storage.exists(), "{label}: nothing was written");
    }

    // Without the options: the existing graph is reported, nothing built.
    let result = w4_rebuild_index(peer, &canon, false, &[])
        .await
        .expect("rebuild_index without force reports the resident graph");
    let structured = result.structured_content.clone().expect("structured");
    assert_eq!(
        structured["data"]["message"],
        json!("Index already exists. Use force=true to rebuild.")
    );
    let last_good_at = (*ws.last_good_at.read()).expect("a load sets last_good_at");
    assert_eq!(
        structured["data"]["builtAt"],
        json!(chrono::DateTime::<chrono::Utc>::from(last_good_at).to_rfc3339()),
        "the build time is the resident generation's"
    );
    assert_eq!(
        structured["data"]["nodeCount"],
        json!(graph_before.node_count())
    );
    assert!(Arc::ptr_eq(&graph_before, &ws.graph()));
    assert!(!storage.exists());

    // The forced rebuild builds with the options and persists the index;
    // the on-disk leg then refuses the same way.
    let result = w4_rebuild_index(peer, &canon, true, &[("cfg_flags", json!(["test"]))])
        .await
        .expect("a forced rebuild with options succeeds");
    assert!(result.is_error != Some(true), "{result:?}");
    assert!(storage.exists(), "the forced rebuild persisted the index");
    assert_eq!(w4_resident_cfg_test_activation(&server, &canon), Some(true));
    assert_eq!(
        w4_recorded_cfg_flags(&canon),
        Some(vec!["test".to_string()])
    );
    let err = w4_mcp_error(
        "on-disk leg",
        w4_rebuild_index(peer, &canon, false, &[("cfg_flags", json!(["test"]))]).await,
    );
    assert_eq!(err.message, need_force, "the on-disk leg");

    drop(running);
    drop(client);
    server.stop().await;
}

/// F3, the other branch: with no graph anywhere (no index on disk, nothing
/// resident) `rebuild_index` with `force=false` builds, so its macro
/// options are honoured and recorded, and validated as on a forced build:
/// an empty and a missing expand cache are refused and write nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_without_force_builds_a_fresh_index_with_the_options() {
    let server = w4_real_builder_server().await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let peer = running.peer();

    let refused_dir = tempfile::tempdir().unwrap();
    let refused_root = canonicalize_path(refused_dir.path()).unwrap();
    w4_write_source(&refused_root);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&refused_root);
    let missing = refused_root.join("no-such-expand-cache");
    let err = w4_mcp_error(
        "empty",
        w4_rebuild_index(peer, &refused_root, false, &[("expand_cache", json!(""))]).await,
    );
    assert!(
        err.message.contains("expand cache directory is empty"),
        "{err:?}"
    );
    let err = w4_mcp_error(
        "missing",
        w4_rebuild_index(
            peer,
            &refused_root,
            false,
            &[("expand_cache", json!(missing.to_string_lossy().as_ref()))],
        )
        .await,
    );
    assert_eq!(
        err.data.as_ref().map(|data| data["kind"].clone()),
        Some(json!("rebuild_macro_options_unavailable")),
        "{err:?}"
    );
    assert!(!storage.exists(), "the refusals wrote nothing");

    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    w4_write_source(&canon);
    let result = w4_rebuild_index(peer, &canon, false, &[("cfg_flags", json!(["test"]))])
        .await
        .expect("a fresh build with options succeeds");
    let structured = result.structured_content.clone().expect("structured");
    assert_eq!(
        structured["data"]["message"],
        json!("Index built successfully.")
    );
    assert_eq!(w4_resident_cfg_test_activation(&server, &canon), Some(true));
    assert_eq!(w4_snapshot_cfg_test_activation(&canon), Some(true));
    assert_eq!(
        w4_recorded_cfg_flags(&canon),
        Some(vec!["test".to_string()])
    );

    drop(running);
    server.stop().await;
}

/// F6: a forced `rebuild_index` of a resident, watched workspace rebuilds
/// it in place, so the file watcher keeps watching: `daemon/status` reports
/// `watching: true` afterwards, and a source edit then reaches a
/// watcher-driven rebuild that publishes the edited source. Before this
/// repair the force path unloaded the workspace, which stopped the watcher,
/// and nothing restarted it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_in_place_keeps_the_workspace_watched() {
    let server = w4_real_builder_server().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    // A git repository, which the file watcher requires.
    support::init_git_repo(&canon);
    w4_index_with_macro_options(
        &canon,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    let key = WorkspaceKey::new(canon.clone(), ProjectRootMode::default(), 0);
    let mut client = w4_ipc_client(&server).await;
    let (rh, wh) = connect_mcp_shim(&server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let peer = running.peer();

    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": canon.to_string_lossy().as_ref() }),
        )
        .await;
    expect_success(&resp);
    let row = w4_status_row(&mut client, &canon).await;
    assert_eq!(row["watching"], json!(true), "after daemon/load: {row}");

    let result = w4_rebuild_index(peer, &canon, true, &[])
        .await
        .expect("a forced rebuild succeeds");
    assert!(result.is_error != Some(true), "{result:?}");
    let row = w4_status_row(&mut client, &canon).await;
    println!("after the forced rebuild: {row}");
    assert_eq!(row["state"], json!("Loaded"), "{row}");
    assert_eq!(row["watching"], json!(true), "after rebuild_index: {row}");

    // The watcher still drives rebuilds: an edit publishes a graph that
    // holds the new function.
    std::fs::write(
        canon.join("src").join("lib.rs"),
        format!("{W4_CFG_LIB_RS}\npub fn added_after_the_rebuild() -> u32 {{ 3 }}\n"),
    )
    .expect("edit the source");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let graph = server.manager.lookup(&key).expect("resident").graph();
        let found = graph
            .strings()
            .iter()
            .any(|(_, name)| name.contains("added_after_the_rebuild"));
        if found {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the watcher did not rebuild the edited source within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    drop(running);
    drop(client);
    server.stop().await;
}
