//! Relation tracking for Go
//!
//! Delegates to the shared relation engine via `relations-shared`.
//! The Go plugin now uses the shared infrastructure with language-specific
//! hooks for customization.
//!
//! ## Go-Specific Features
//! - Method receiver tracking (pointer and value receivers)
//! - Package-level exports (capitalized identifiers)
//! - Built-in function calls (make, new, len, etc.)
//! - Standard library import detection
//! - Interface type assertions
//! - Defer/go keyword usage
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub(crate) mod build_constraints;
mod graph_builder;
pub(crate) mod local_scopes;
mod types;
mod wraps;

pub use graph_builder::GoGraphBuilder;
