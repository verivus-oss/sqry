//! Relation extraction helpers for the PHP plugin.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
mod phpdoc_parser;
mod type_extractor;

pub use graph_builder::PhpGraphBuilder;
