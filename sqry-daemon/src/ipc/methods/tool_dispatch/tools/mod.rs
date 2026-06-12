//! Phase 8b Task 7 — one wrapper per MCP tool method.
//!
//! Each sub-module deserialises its tool's JSON-RPC `params` via the
//! shared [`sqry_mcp::daemon_params::params_to_<tool>_args`] helper,
//! then forwards the resulting `*Args` through
//! [`super::classify_and_build`] so the workspace-verdict machinery
//! (Fresh / Stale / `NotReady`) is shared.

pub(crate) mod complexity_metrics;
pub(crate) mod dependency_impact;
pub(crate) mod direct_callees;
pub(crate) mod direct_callers;
pub(crate) mod export_graph;
pub(crate) mod find_cycles;
pub(crate) mod find_unused;
pub(crate) mod is_node_in_cycle;
pub(crate) mod relation_query;
pub(crate) mod semantic_diff;
pub(crate) mod semantic_search;
pub(crate) mod show_dependencies;
pub(crate) mod subgraph;
pub(crate) mod trace_path;
