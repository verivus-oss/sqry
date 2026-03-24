// Relation extraction entry points for the Zig plugin.
// Hook-based extraction was removed; the graph builder is the supported path.

pub mod graph_builder;
pub mod type_extractor;

pub use graph_builder::ZigGraphBuilder;
