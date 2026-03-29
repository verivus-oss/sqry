//! Relation extraction helpers for the Swift plugin.
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod bridging;
mod graph_builder;
mod type_extractor;

pub use bridging::{BridgingHeaderLocator, SwiftBridgingIndex};
pub use graph_builder::SwiftGraphBuilder;
pub use type_extractor::extract_type_names_from_swift_type;
