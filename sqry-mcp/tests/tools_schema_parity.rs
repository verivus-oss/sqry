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
//! 15 tools, sorted, unique) and the subset relationship against the
//! private `SqryServer::get_filtered_tools()` inventory. This integration test
//! guards the **public-API round-trip**: `daemon_supported_tools()` must
//! return a list whose names match `DAEMON_SUPPORTED_TOOL_NAMES` exactly by
//! set equality (no more, no fewer), and must contain exactly 15 tools
//! (the natural-language sqry_ask tool was removed) with no duplicates.

use std::collections::HashSet;

use sqry_mcp::tools_schema::{DAEMON_SUPPORTED_TOOL_NAMES, daemon_supported_tools};

/// `daemon_supported_tool_names_matches_standalone_subset`
///
/// `daemon_supported_tools()` must return exactly the 15 tools whose names
/// are in `DAEMON_SUPPORTED_TOOL_NAMES` (the natural-language sqry_ask tool
/// was removed). No extra tools, no missing tools, no duplicates. The parity
/// between the constant and the runtime-filtered list is the integration-
/// level guard that sqry-daemon's `DaemonMcpHandler` will advertise the
/// correct tool set to MCP clients.
///
/// This is the integration-level counterpart to the U7 unit tests
/// `daemon_supported_tools_returns_exact_16_under_default_flags` and
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

    // Exactly 17 tools (15 + body-shape `structural_similar` U07 +
    // `generate_overview`): belt-and-suspenders for the set-equality proof
    // above.
    assert_eq!(
        tools.len(),
        17,
        "daemon_supported_tools() must return exactly 17 tools under default feature flags \
         (15 + structural_similar + generate_overview), got {} tools: {:?}",
        tools.len(),
        returned_names
    );
}

/// T13 (schema, standalone host, surface parity W4 design W4-D8): the
/// standalone inventory advertises `cfg_flags`, `expand_cache` and
/// `reset_macro_options` on `rebuild_index`. `daemon_supported_tools()`
/// is the standalone `get_filtered_tools()` inventory filtered by name, so
/// the schema object it returns for `rebuild_index` is the standalone
/// host's; the daemon host's `tools/list` is pinned in
/// `sqry-daemon/tests/ipc_shim_mcp_host.rs`.
#[test]
fn rebuild_index_schema_advertises_the_macro_option_arguments() {
    let tools = daemon_supported_tools();
    let tool = tools
        .iter()
        .find(|t| t.name.as_ref() == "rebuild_index")
        .expect("rebuild_index advertised");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("rebuild_index schema has properties");
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();
    println!("rebuild_index schema properties (standalone host): {names:?}");
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
}
