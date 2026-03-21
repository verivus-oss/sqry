//! Relation extraction for Svelte using `GraphBuilder`.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;
/// Relations extraction for Svelte files using `GraphBuilder` pattern
pub use graph_builder::SvelteGraphBuilder;
