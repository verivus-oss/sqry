//! Relation extraction for `ServiceNow` code.
//!
//! Provides `ServiceNowGraphBuilder` for extracting `ServiceNow` relationships:
//! - `ES6` `import` and `CommonJS` `require()` (`Import` edges)
//! - `GlideRecord` table references
//! - `gs.*` API calls
//! - Script Include `Class.create()` patterns
//! - Function calls
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder`
//! and the language-specific `*GraphBuilder` types exported from this module to build
//! `CodeGraph`.

mod graph_builder;

pub use graph_builder::ServiceNowGraphBuilder;
