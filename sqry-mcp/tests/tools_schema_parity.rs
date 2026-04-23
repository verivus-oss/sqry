//! Phase 8c U16 — integration-level tool schema parity test.
//!
//! Verifies that `DAEMON_SUPPORTED_TOOL_NAMES` is a strict subset of the
//! standalone `sqry-mcp` tool inventory. This is an **integration test**
//! (runs in `cargo test --workspace` via the separate test binary) as opposed
//! to the unit tests in `sqry-mcp/src/tools_schema.rs` which run in-crate.
//!
//! The integration variant exercises the public `daemon_supported_tools()` API
//! surface from outside the crate, matching the actual consumer perspective:
//! sqry-daemon's `mcp_host::DaemonMcpHandler::list_tools` calls
//! `sqry_mcp::tools_schema::daemon_supported_tools()` (the public re-export
//! path), and the tool names must match `DAEMON_SUPPORTED_TOOL_NAMES` exactly.
//!
//! # Why a separate integration test?
//!
//! The U7 unit tests in `tools_schema.rs` guard the constant itself (exactly
//! 15, sorted, unique) and the subset relationship against the private
//! `SqryServer::get_filtered_tools()` inventory. This integration test guards
//! the **public-API round-trip**: `daemon_supported_tools()` must return a
//! list whose names match `DAEMON_SUPPORTED_TOOL_NAMES` exactly by set
//! equality — no more, no fewer — and must contain exactly 15 tools with no
//! duplicates.

use std::collections::HashSet;

use sqry_mcp::tools_schema::{DAEMON_SUPPORTED_TOOL_NAMES, daemon_supported_tools};

/// `daemon_supported_tool_names_matches_standalone_subset`
///
/// `daemon_supported_tools()` must return exactly the 15 tools whose names
/// are in `DAEMON_SUPPORTED_TOOL_NAMES`. No extra tools, no missing tools, no
/// duplicates. The parity between the constant and the runtime-filtered list
/// is the integration-level guard that sqry-daemon's `DaemonMcpHandler` will
/// advertise the correct tool set to MCP clients.
///
/// This is the integration-level counterpart to the U7 unit tests
/// `daemon_supported_tools_returns_exact_15_under_default_flags` and
/// `daemon_supported_tool_names_is_strict_subset_of_standalone` in
/// `sqry-mcp/src/tools_schema.rs`. Those unit tests verify internal invariants;
/// this test verifies the public-API contract from the caller's perspective.
#[test]
fn daemon_supported_tool_names_matches_standalone_subset() {
    let tools = daemon_supported_tools();

    let returned_names: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    let expected_names: HashSet<&str> = DAEMON_SUPPORTED_TOOL_NAMES.iter().copied().collect();

    // No duplicates in returned list.
    assert_eq!(
        tools.len(),
        returned_names.len(),
        "daemon_supported_tools() returned duplicate tool names (vec len {} != set len {})",
        tools.len(),
        returned_names.len()
    );

    // Returned names == expected names (set equality).
    let unexpected: Vec<&str> = returned_names
        .difference(&expected_names)
        .copied()
        .collect();
    let missing: Vec<&str> = expected_names
        .difference(&returned_names)
        .copied()
        .collect();

    assert!(
        unexpected.is_empty(),
        "daemon_supported_tools() returned tools NOT in DAEMON_SUPPORTED_TOOL_NAMES: \
         {unexpected:?}. The filter in daemon_supported_tools() must match the constant."
    );
    assert!(
        missing.is_empty(),
        "daemon_supported_tools() is missing tools from DAEMON_SUPPORTED_TOOL_NAMES: \
         {missing:?}. Each daemon-supported tool must appear in the standalone \
         get_filtered_tools() inventory (the source of the filter)."
    );

    // Exactly 15 tools — belt-and-suspenders for the set-equality proof above.
    assert_eq!(
        tools.len(),
        15,
        "daemon_supported_tools() must return exactly 15 tools under default feature flags, \
         got {} tools: {:?}",
        tools.len(),
        returned_names
    );
}
