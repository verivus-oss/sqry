//! Relation extraction for SAP ABAP code.
//!
//! Provides `AbapGraphBuilder` for extracting ABAP relationships:
//! - Class method definitions
//! - Function module implementations
//! - SELECT statement table references (`TableRead` edges)
//! - INSERT/MODIFY/UPDATE/DELETE table references (`TableWrite` edges)
//! - `TypeOf` edges for type annotations
//! - References edges for type dependencies
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.
//!
//! ## Implementation Notes
//!
//! Due to tree-sitter-abap grammar limitations:
//! - SELECT statements are parsed via the grammar (`select_statement_obsolete`)
//! - INSERT/MODIFY/UPDATE/DELETE use text-based pattern matching as fallback
//! - DATA/TYPES/FIELD-SYMBOLS type declarations use text-based extraction

mod graph_builder;
pub(crate) mod type_extractor;

pub use graph_builder::AbapGraphBuilder;
