//! Relations module for Haskell - Graph-based code analysis
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;
pub mod type_extractor;

pub use graph_builder::HaskellGraphBuilder;
