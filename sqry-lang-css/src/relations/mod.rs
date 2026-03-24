//! Relation extraction for CSS stylesheets.
//!
//! Provides `CssGraphBuilder` for extracting stylesheet relationships:
//! - `@import` edges for stylesheet dependencies
//! - `url()` edges for asset references
//! - CSS custom property (variable) nodes
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
pub(crate) mod queries;

pub use graph_builder::CssGraphBuilder;
