//! Relation tracking for TypeScript built on top of the shared relations core.
//! This module wires TypeScript-specific hooks into the shared traversal so we
//! can capture type-only metadata and handle TypeScript-specific syntax without
//! duplicating JavaScript logic.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
pub(crate) mod local_scopes;
pub mod type_extractor;

pub use graph_builder::TypeScriptGraphBuilder;

pub use sqry_core::relations::types::*;
