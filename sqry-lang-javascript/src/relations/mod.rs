//! Relation tracking for JavaScript using the shared relations core.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
mod jsdoc_parser;
pub(crate) mod local_scopes;
mod type_extractor;

pub use graph_builder::JavaScriptGraphBuilder;

pub mod types {
    #[allow(unused_imports)]
    pub use sqry_core::relations::types::*;
}
