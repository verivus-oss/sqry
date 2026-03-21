//! Query executor for semantic code search
//!
//! # Architecture
//!
//! The executor is organized into focused modules:
//!
//! - [`core`]: Main `QueryExecutor` struct and orchestration
//! - [`graph_eval`]: CodeGraph-native query evaluation
//!
//! # Module Structure
//!
//! ```text
//! executor/
//! ├── mod.rs              (this file - public API)
//! ├── core.rs             (QueryExecutor facade)
//! ├── graph_eval.rs       (CodeGraph evaluation)
//! └── tests.rs            (unit tests)
//! ```

pub(crate) mod core;
pub(crate) mod graph_cycles;
pub(crate) mod graph_duplicates;
pub(crate) mod graph_eval;
pub(crate) mod graph_unused;
pub(crate) mod pipeline;

#[cfg(test)]
mod tests;

// Re-export public API
pub use core::QueryExecutor;

// Graph-based analysis (preferred)
pub use graph_cycles::{CircularConfig, CircularType, find_all_cycles_graph, is_node_in_cycle};
pub use graph_duplicates::{
    DuplicateConfig, DuplicateGroup, DuplicateType, build_duplicate_groups_graph,
};
pub use graph_unused::{
    UnusedScope, compute_reachable_set_graph, find_unused_nodes, is_node_unused,
};

// Pipeline execution
pub use pipeline::execute_pipeline_stage;
