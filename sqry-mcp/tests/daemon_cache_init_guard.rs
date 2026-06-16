//! Isolated regression guard for the daemon-hosted trace_path/subgraph crash.
//!
//! Before the fix, the `sqryd` daemon linked the `sqry-mcp` lib target but
//! never initialized the process-global payload caches, so daemon-hosted
//! `trace_path` / `subgraph` panicked ("telemetry not initialized") and
//! crashed the whole daemon. `sqry_mcp::init_mcp_caches` is the lib-side
//! initializer the daemon now calls.
//!
//! This MUST be an integration test (its own test binary) rather than a lib
//! unit test. The telemetry cells are process-global `OnceLock`s: in the
//! shared lib-test binary a sibling test could initialize them first, so a
//! unit test asserting "the accessor doesn't panic after init" would pass
//! even if `init_mcp_caches` stopped wiring telemetry (a tautology). Here the
//! test owns its process: nothing runs `main`, so the cells start UNSET, and
//! the assertions below fail if `init_mcp_caches` ever stops initializing
//! them.
//!
//! Keep this the ONLY test in this file so the single-process isolation holds.

use sqry_mcp::cache_init_test_probe::{
    subgraph_telemetry_initialized, trace_path_telemetry_initialized,
};
use sqry_mcp::{McpConfig, init_mcp_caches};

#[test]
fn init_mcp_caches_is_what_wires_trace_and_subgraph_telemetry() {
    // Precondition: fresh process, nothing has initialized the global
    // telemetry cells yet. If this fails, the test's isolation guarantee is
    // broken (another code path initialized them first) and the post-checks
    // below would no longer prove `init_mcp_caches` did the wiring.
    assert!(
        !trace_path_telemetry_initialized(),
        "precondition: trace_path telemetry must be unset before init_mcp_caches"
    );
    assert!(
        !subgraph_telemetry_initialized(),
        "precondition: subgraph telemetry must be unset before init_mcp_caches"
    );

    init_mcp_caches(&McpConfig::default()).expect("init_mcp_caches must succeed");

    // The regression: these must be wired by init_mcp_caches. If the fix is
    // reverted (daemon/init stops initializing the caches), these fail, which
    // is the daemon panic this guards against.
    assert!(
        trace_path_telemetry_initialized(),
        "init_mcp_caches must wire trace_path telemetry (else daemon trace_path panics)"
    );
    assert!(
        subgraph_telemetry_initialized(),
        "init_mcp_caches must wire subgraph telemetry (else daemon subgraph panics)"
    );
}
