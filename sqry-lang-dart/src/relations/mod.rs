//! Relation extraction for Dart - implements GraphBuilder for code graph construction.
//!
//! Extracts Dart-specific edges:
//! - Class definitions and method calls
//! - Widget build hierarchies (Flutter)
//! - MethodChannel platform invocations
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
mod queries;
pub mod type_extractor;

pub use graph_builder::DartGraphBuilder;
pub use queries::DartQueries;
