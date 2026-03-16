//! Relation extraction for Puppet manifests.
//!
//! Provides `PuppetGraphBuilder` for extracting Puppet class relationships:
//! - `include` statements (`include myclass`)
//! - `require` statements (`require myclass`)
//! - `contain` statements (`contain myclass`)
//! - Class inheritance (`inherits parent`)
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;

pub use graph_builder::PuppetGraphBuilder;
