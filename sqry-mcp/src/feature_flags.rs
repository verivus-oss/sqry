//! Feature flag management for sqry-mcp
//!
//! Provides runtime configuration for experimental and progressive rollout features.
//! Feature flags can be controlled via environment variables for safe deployment.
use std::env;

/// Feature flags configuration
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Feature flags are intentionally boolean for env toggles.
pub struct FeatureFlags {
    /// Whether graph tools (`trace_path`, subgraph) are enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_GRAPH`
    /// Default: true (enabled by default after testing)
    pub is_graph_tools_enabled: bool,

    /// Whether the `export_graph` tool is enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_EXPORT`
    /// Default: true
    pub is_export_graph_enabled: bool,

    /// Whether the `cross_language_edges` tool is enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_CROSS_LANGUAGE`
    /// Default: true
    pub is_cross_language_enabled: bool,

    /// Whether the `semantic_diff` tool is enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_SEMANTIC_DIFF`
    /// Default: true
    pub is_semantic_diff_enabled: bool,

    /// Whether the `dependency_impact` tool is enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_DEPENDENCY_IMPACT`
    /// Default: true
    pub is_dependency_impact_enabled: bool,

    /// Whether the `sqry_ask` natural language translation tool is enabled.
    /// Environment variable: `SQRY_MCP_ENABLE_SQRY_ASK`
    /// Default: true
    pub is_sqry_ask_enabled: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            // All features enabled by default after successful testing
            // Can be disabled individually for gradual rollout
            is_graph_tools_enabled: true,
            is_export_graph_enabled: true,
            is_cross_language_enabled: true,
            is_semantic_diff_enabled: true,
            is_dependency_impact_enabled: true,
            is_sqry_ask_enabled: true,
        }
    }
}

impl FeatureFlags {
    /// Load feature flags from environment variables
    pub fn from_env() -> Self {
        Self {
            is_graph_tools_enabled: env_flag("SQRY_MCP_ENABLE_GRAPH", true),
            is_export_graph_enabled: env_flag("SQRY_MCP_ENABLE_EXPORT", true),
            is_cross_language_enabled: env_flag("SQRY_MCP_ENABLE_CROSS_LANGUAGE", true),
            is_semantic_diff_enabled: env_flag("SQRY_MCP_ENABLE_SEMANTIC_DIFF", true),
            is_dependency_impact_enabled: env_flag("SQRY_MCP_ENABLE_DEPENDENCY_IMPACT", true),
            is_sqry_ask_enabled: env_flag("SQRY_MCP_ENABLE_SQRY_ASK", true),
        }
    }

    /// Check if a specific tool is enabled
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        match tool_name {
            "trace_path" | "subgraph" => self.is_graph_tools_enabled,
            "export_graph" => self.is_export_graph_enabled,
            "cross_language_edges" => self.is_cross_language_enabled,
            "semantic_diff" => self.is_semantic_diff_enabled,
            "dependency_impact" => self.is_dependency_impact_enabled,
            "sqry_ask" => self.is_sqry_ask_enabled,
            // Core tools always enabled
            "semantic_search"
            | "hierarchical_search"
            | "relation_query"
            | "call_hierarchy"
            | "explain_code"
            | "search_similar"
            | "show_dependencies"
            | "get_index_status"
            | "rebuild_index"
            // Analysis tools (always enabled, require unified graph)
            | "find_cycles"
            | "find_duplicates"
            | "find_unused"
            // New graph-based tools (always enabled, require unified graph)
            | "is_node_in_cycle"
            | "pattern_search"
            | "direct_callers"
            | "direct_callees"
            // Introspection tools (always enabled)
            | "list_files"
            | "list_symbols"
            | "get_graph_stats"
            | "get_insights"
            | "complexity_metrics"
            // Navigation tools (always enabled)
            | "get_definition"
            | "get_references"
            | "get_hover_info"
            | "get_document_symbols"
            | "get_workspace_symbols"
            // Structural planner tool (DB13, always enabled — requires unified graph)
            | "sqry_query" => true,
            _ => false,
        }
    }

    /// Get disabled reason for a tool (for user-friendly error messages)
    pub fn disabled_reason(&self, tool_name: &str) -> Option<String> {
        if self.is_tool_enabled(tool_name) {
            return None;
        }

        match tool_name {
            "trace_path" | "subgraph" => Some(
                "Graph tools are currently disabled. Set SQRY_MCP_ENABLE_GRAPH=true to enable.".to_string()
            ),
            "export_graph" => Some(
                "Graph export is currently disabled. Set SQRY_MCP_ENABLE_EXPORT=true to enable.".to_string()
            ),
            "cross_language_edges" => Some(
                "Cross-language analysis is currently disabled. Set SQRY_MCP_ENABLE_CROSS_LANGUAGE=true to enable.".to_string()
            ),
            "semantic_diff" => Some(
                "Semantic diff is currently disabled. Set SQRY_MCP_ENABLE_SEMANTIC_DIFF=true to enable.".to_string()
            ),
            "dependency_impact" => Some(
                "Dependency impact analysis is currently disabled. Set SQRY_MCP_ENABLE_DEPENDENCY_IMPACT=true to enable.".to_string()
            ),
            "sqry_ask" => Some(
                "Natural language translation is currently disabled. Set SQRY_MCP_ENABLE_SQRY_ASK=true to enable.".to_string()
            ),
            _ => Some(format!("Unknown tool: {tool_name}")),
        }
    }
}

/// Helper function to read boolean from environment variable
fn env_flag(key: &str, default: bool) -> bool {
    match env::var(key).ok().as_deref() {
        Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        Some(other) => {
            eprintln!("Warning: Invalid value '{other}' for {key}, using default: {default}");
            default
        }
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_flags() {
        let flags = FeatureFlags::default();
        assert!(flags.is_graph_tools_enabled);
        assert!(flags.is_export_graph_enabled);
        assert!(flags.is_cross_language_enabled);
        assert!(flags.is_semantic_diff_enabled);
        assert!(flags.is_dependency_impact_enabled);
    }

    #[test]
    fn test_is_tool_enabled_core_tools() {
        let flags = FeatureFlags::default();
        assert!(flags.is_tool_enabled("semantic_search"));
        assert!(flags.is_tool_enabled("relation_query"));
        assert!(flags.is_tool_enabled("explain_code"));
        assert!(flags.is_tool_enabled("search_similar"));
        assert!(flags.is_tool_enabled("show_dependencies"));
        assert!(flags.is_tool_enabled("get_index_status"));
    }

    #[test]
    fn test_is_tool_enabled_graph_tools() {
        let mut flags = FeatureFlags::default();
        assert!(flags.is_tool_enabled("trace_path"));
        assert!(flags.is_tool_enabled("subgraph"));

        flags.is_graph_tools_enabled = false;
        assert!(!flags.is_tool_enabled("trace_path"));
        assert!(!flags.is_tool_enabled("subgraph"));
    }

    #[test]
    fn test_disabled_reason() {
        let mut flags = FeatureFlags::default();
        assert_eq!(flags.disabled_reason("trace_path"), None);

        flags.is_graph_tools_enabled = false;
        let reason = flags.disabled_reason("trace_path");
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("SQRY_MCP_ENABLE_GRAPH=true"));
    }

    #[test]
    fn test_env_flag_parsing() {
        assert!(env_flag("NONEXISTENT_VAR", true));
        assert!(!env_flag("NONEXISTENT_VAR", false));

        // Note: Cannot test actual env vars in unit tests without setting them,
        // but logic is straightforward
    }
}
