//! sqry-mcp library re-exports for fuzzing and testing.
//!
//! This module exposes internal components for fuzz testing. The MCP server
//! itself is the main binary; this library allows external test crates to
//! access validation and pagination functionality.
//!
//! Note: Many internal modules are only used by the binary (main.rs) and appear
//! unused from the library's perspective. The `dead_code` lint is suppressed
//! for these modules.

// These modules are used by the binary (main.rs) but not the library exports.
// Suppress dead_code warnings for modules that are only used by the binary.
// Engine module is public to allow integration tests.
#[allow(dead_code)]
pub mod engine;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod execution;
#[allow(dead_code)]
mod feature_flags;
pub mod mcp_config;
mod pagination;
#[allow(dead_code)]
mod path_resolver;
#[allow(dead_code)]
mod prompts;
#[allow(dead_code)]
mod resources;
#[allow(dead_code)]
mod server;
#[allow(dead_code)]
mod tools;

pub use pagination::{decode_cursor, encode_cursor};

// Re-export mcp_config for testing
pub use mcp_config::McpConfig;

/// Re-export tool validation functions for fuzzing.
#[cfg(any(test, fuzzing))]
pub mod tool_validation {
    pub use crate::tools::{
        validate_cross_language_edges_args, validate_dependency_impact_args,
        validate_explain_code_args, validate_export_graph_args, validate_get_index_status_args,
        validate_relation_query_args, validate_search_similar_args, validate_semantic_diff_args,
        validate_semantic_search_args, validate_show_dependencies_args, validate_subgraph_args,
        validate_trace_path_args,
    };
}
