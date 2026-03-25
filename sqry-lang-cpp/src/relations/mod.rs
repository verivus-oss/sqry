//! Relation extraction helpers for the C++ plugin.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;
pub mod queries;

pub use graph_builder::CppGraphBuilder;
