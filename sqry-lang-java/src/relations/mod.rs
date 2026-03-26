//! Relation extraction entry points for the Java plugin.
//!
//! This module exposes the public surface used by `JavaPlugin` to satisfy
//! the `LanguagePlugin` trait. The heavy lifting now lives inside the
//! `relations-shared` crate—our adapter simply wires the shared engine to
//! the Java-specific hooks that were ported from the previous implementation.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

pub mod graph_builder;
pub mod java_common;
mod local_scopes;

pub use graph_builder::JavaGraphBuilder;

pub use java_common::{JavaRelationContext, PackageResolver};

// Re-export helper functions for callers that previously pulled them from the
// per-plugin modules.
pub use java_common::{build_member_symbol, build_symbol, normalize_type_name};
