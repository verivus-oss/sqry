//! Relation tracking for Python
//!
//! Relation extraction uses the unified `GraphBuilder` implementations.
mod graph_builder;
pub(crate) mod local_scopes;

pub use graph_builder::PythonGraphBuilder;
