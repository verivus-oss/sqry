//! Regression guard: `IpcServer::bind` initializes the sqry-mcp payload caches.
//!
//! Unix-only: this drives the `support::ipc::TestServer` harness, which speaks
//! the daemon's Unix-domain-socket wire format (`support/ipc.rs` imports
//! `tokio::net::UnixStream` unconditionally). The whole sqry-daemon IPC
//! integration suite is Linux-only for the same reason; `#![cfg(unix)]` cleanly
//! excludes this file from non-unix builds rather than failing to compile.
//!
#![cfg(unix)]

//! Daemon-hosted `trace_path` / `subgraph` once panicked ("telemetry not
//! initialized") and crashed `sqryd` because the caches the lib's tool
//! dispatch reads were never initialized in the daemon process. The fix
//! initializes them inside `IpcServer::bind` — the single chokepoint every
//! serving path goes through (production entrypoints, the `sqryd-test-server`
//! fixture, and the `TestServer` harness all reach tool dispatch via a bound
//! `IpcServer`).
//!
//! This MUST be its own test binary with a single test: the telemetry cells
//! are process-global `OnceLock`s, so the precondition assertions (telemetry
//! UNSET before any bind) only hold in a fresh process. If `bind` ever stops
//! initializing the caches this fails — it is not a tautology.

mod support;

use sqry_mcp::cache_init_test_probe::{
    subgraph_telemetry_initialized, trace_path_telemetry_initialized,
};
use support::ipc::TestServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_server_bind_initializes_mcp_payload_caches() {
    // Fresh process: nothing has bound an IpcServer or initialized the global
    // telemetry yet.
    assert!(
        !trace_path_telemetry_initialized(),
        "precondition: trace_path telemetry must be unset before any IpcServer::bind"
    );
    assert!(
        !subgraph_telemetry_initialized(),
        "precondition: subgraph telemetry must be unset before any IpcServer::bind"
    );

    // TestServer::new() calls IpcServer::bind, which must initialize the caches.
    let _server = TestServer::new().await;

    assert!(
        trace_path_telemetry_initialized(),
        "IpcServer::bind must initialize trace_path telemetry (else daemon trace_path panics)"
    );
    assert!(
        subgraph_telemetry_initialized(),
        "IpcServer::bind must initialize subgraph telemetry (else daemon subgraph panics)"
    );
}
