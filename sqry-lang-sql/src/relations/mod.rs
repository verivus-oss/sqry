//! Relation extraction for SQL - implements GraphBuilder for code graph construction.
//!
//! Extracts SQL-specific edges:
//! - Procedure/function calls
//! - Trigger activations
//! - Table reads (SELECT)
//! - Table writes (INSERT, UPDATE, DELETE, CREATE TABLE, DROP TABLE, ALTER TABLE)
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;

pub use graph_builder::SqlGraphBuilder;
