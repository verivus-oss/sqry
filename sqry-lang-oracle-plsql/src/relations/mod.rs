//! Relation extraction for Oracle PL/SQL code.
//!
//! Provides `OraclePlsqlGraphBuilder` for extracting PL/SQL relationships:
//! - Package procedure/function calls within packages
//! - Cross-package calls (e.g., `other_pkg.proc()`)
//! - Table access relationships (SELECT, INSERT, UPDATE, DELETE)
//! - TypeOf edges for type annotations
//! - References edges for type dependencies
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.
//!
//! ## Current Status
//!
//! The underlying tree-sitter-plsql grammar is designed primarily for PACKAGE and
//! PACKAGE BODY constructs. Relationship extraction is limited to what the grammar
//! supports. Full call graph extraction will be available after grammar improvements.
//!
//! ## What Works Today
//!
//! - Module node creation for PL/SQL files
//! - Basic framework for edge extraction (pending grammar enhancement)

mod graph_builder;
pub(crate) mod type_extractor;

pub use graph_builder::OraclePlsqlGraphBuilder;
