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
pub mod scope_index;
mod signature_builder;
mod type_extractor;

pub use graph_builder::CGraphBuilder;
// Storage shape + lookup live in `sqry-core`'s `c_indirect` module (U09).
// This crate hosts only the tree-sitter builder; re-export the type so
// callers can keep the `sqry_lang_c::relations::LocalScopeIndex` path
// alongside the new `build_local_scope_index` entry point.
pub use scope_index::build_local_scope_index;
pub use sqry_core::graph::unified::storage::c_indirect::LocalScopeIndex;
