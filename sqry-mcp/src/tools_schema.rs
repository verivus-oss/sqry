//! Daemon-hosted MCP tool surface.
//!
//! The sqryd daemon hosts a 16-tool MCP subset via the shim byte-pump
//! transport. Standalone `sqry-mcp` (no daemon) exposes 34 tools via
//! [`crate::server::SqryServer::get_filtered_tools`]. These two are
//! intentionally NOT identical — 18 standalone-only tools are
//! unavailable when connecting through the daemon (per Codex iter-0 B3:
//! "the daemon-hosted MCP surface is finally honest").
//!
//! Users wanting the full 34-tool inventory continue to invoke
//! `sqry-mcp` without `--daemon`.

/// Names of the 16 tools that the daemon MCP host exposes via
/// `tools/list` and that [`crate::daemon_adapter::dispatch::dispatch_by_name`]
/// can route. Alphabetical order for stable assertions.
///
/// **Source of truth:** the Phase 8b `tool_dispatch/tools/*.rs`
/// dispatcher in sqry-daemon plus the NL07 `sqry_ask` arm. Any
/// addition/removal here must be mirrored in sqry-daemon's
/// `ipc::methods::tool_dispatch::dispatch_tool` and in the daemon
/// MCP host's `dispatch_sqry_ask` route.
pub const DAEMON_SUPPORTED_TOOL_NAMES: &[&str] = &[
    "complexity_metrics",
    "dependency_impact",
    "direct_callees",
    "direct_callers",
    "export_graph",
    "find_cycles",
    "find_unused",
    "is_node_in_cycle",
    "rebuild_index",
    "relation_query",
    "semantic_diff",
    "semantic_search",
    "show_dependencies",
    "sqry_ask",
    "subgraph",
    "trace_path",
];

/// Return the rmcp Tool schema list filtered to only the 16
/// daemon-supported tools. The daemon MCP host
/// (`sqry-daemon::mcp_host`, Phase 8c U8) calls this in its
/// `ServerHandler::list_tools`.
///
/// Delegates to [`crate::server::SqryServer::get_filtered_tools`] then
/// filters by [`DAEMON_SUPPORTED_TOOL_NAMES`]. The feature-flag filter
/// still applies — daemon-host tools are additionally subject to
/// [`crate::feature_flags::FeatureFlags::from_env`]. When a feature
/// flag disables one of the 16 names, the returned vec will be
/// shorter than 16.
#[must_use]
pub fn daemon_supported_tools() -> Vec<rmcp::model::Tool> {
    use crate::feature_flags::FeatureFlags;
    use crate::server::SqryServer;
    let server = SqryServer::new(FeatureFlags::from_env());
    server
        .get_filtered_tools()
        .into_iter()
        .filter(|t| DAEMON_SUPPORTED_TOOL_NAMES.contains(&t.name.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// `DAEMON_SUPPORTED_TOOL_NAMES` must contain exactly 16 names and
    /// they must be sorted / deduplicated. Assertion guards against
    /// accidental addition or duplication that would desync from
    /// sqry-daemon's `dispatch_tool` match plus the NL07 `sqry_ask`
    /// route.
    #[test]
    fn daemon_supported_tool_names_is_exactly_16_sorted_unique() {
        assert_eq!(
            DAEMON_SUPPORTED_TOOL_NAMES.len(),
            16,
            "DAEMON_SUPPORTED_TOOL_NAMES must contain exactly 16 tools"
        );

        // Sorted.
        let mut sorted = DAEMON_SUPPORTED_TOOL_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted.as_slice(),
            DAEMON_SUPPORTED_TOOL_NAMES,
            "DAEMON_SUPPORTED_TOOL_NAMES must be alphabetically sorted"
        );

        // Unique.
        let set: HashSet<&str> = DAEMON_SUPPORTED_TOOL_NAMES.iter().copied().collect();
        assert_eq!(
            set.len(),
            DAEMON_SUPPORTED_TOOL_NAMES.len(),
            "DAEMON_SUPPORTED_TOOL_NAMES must contain no duplicates"
        );
    }

    /// With default feature flags (no env flags set), all 16 names must
    /// appear in `daemon_supported_tools()`. If the test harness sets
    /// `SQRY_MCP_FLAGS` env vars that disable one of the 16, this
    /// assertion may need narrowing — but the intent of the default
    /// daemon surface is exactly 16.
    #[test]
    fn daemon_supported_tools_returns_exact_16_under_default_flags() {
        let tools = daemon_supported_tools();
        let names: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        let expected: HashSet<&str> = DAEMON_SUPPORTED_TOOL_NAMES.iter().copied().collect();
        assert_eq!(
            names,
            expected,
            "daemon_supported_tools must return exactly the 16 DAEMON_SUPPORTED_TOOL_NAMES \
             (default feature flags). Got {} tools, expected 16.",
            tools.len()
        );
    }

    /// Every `DAEMON_SUPPORTED_TOOL_NAMES` entry must appear in the
    /// standalone `SqryServer::get_filtered_tools()` inventory — the
    /// daemon subset is a STRICT subset of the standalone 34-tool
    /// surface. Also verifies the standalone inventory is strictly
    /// larger (the 19 standalone-only tools).
    #[test]
    fn daemon_supported_tool_names_is_strict_subset_of_standalone() {
        use crate::feature_flags::FeatureFlags;
        use crate::server::SqryServer;
        let server = SqryServer::new(FeatureFlags::from_env());
        let standalone_names: HashSet<String> = server
            .get_filtered_tools()
            .into_iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        for &daemon_name in DAEMON_SUPPORTED_TOOL_NAMES {
            assert!(
                standalone_names.contains(daemon_name),
                "daemon-supported tool {daemon_name:?} not found in standalone inventory \
                 (DAEMON_SUPPORTED_TOOL_NAMES must be a strict subset of \
                 SqryServer::get_filtered_tools)"
            );
        }
        // Standalone has strictly more tools (the 19 standalone-only set).
        assert!(
            standalone_names.len() > DAEMON_SUPPORTED_TOOL_NAMES.len(),
            "standalone inventory ({} tools) must be strictly larger than daemon subset ({} tools) — \
             otherwise the daemon-subset rationale is broken",
            standalone_names.len(),
            DAEMON_SUPPORTED_TOOL_NAMES.len()
        );
    }
}
