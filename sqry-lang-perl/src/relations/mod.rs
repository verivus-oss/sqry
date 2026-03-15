//! Relation extraction for Perl delegated to relations-shared hooks.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;

pub use graph_builder::PerlGraphBuilder;
