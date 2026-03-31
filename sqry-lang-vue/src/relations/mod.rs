//! Relation extraction for Vue files.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;

// Re-export the GraphBuilder for easier access
pub use graph_builder::VueGraphBuilder;
