//! Rust code graph building using unified `CodeGraph` architecture
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;
pub(crate) mod local_scopes;

pub use graph_builder::{RustGraphBuilder, RustGraphConfig};
