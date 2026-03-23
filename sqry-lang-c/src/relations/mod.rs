//! Relation tracking for C
//!
//! Provides graph building and relation extraction hooks for C code.
//!
//! ## C-Specific Features
//! - Function declarations vs definitions
//! - Static (file-local) functions
//! - Function pointer calls
//! - No classes, namespaces, or templates (simpler than C++)
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the
//! language-specific `*GraphBuilder` (see this module's export).

mod graph_builder;
mod type_extractor;

pub use graph_builder::CGraphBuilder;
