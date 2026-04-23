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
pub(crate) mod graph_duplicates;
pub mod graph_eval;
pub(crate) mod pipeline;

#[cfg(test)]
mod tests;

// Re-export public API
pub use core::QueryExecutor;

// Graph-based duplicate detection (not yet migrated to sqry-db — DB20
// scope). Cycle detection and unused detection migrated to sqry-db in
// DB15-DB19; their config types live in `super::cycles_config` /
// `super::unused_config`.
pub use graph_duplicates::{
    DuplicateConfig, DuplicateGroup, DuplicateType, build_duplicate_groups_graph,
};

// Pipeline execution
pub use pipeline::execute_pipeline_stage;
