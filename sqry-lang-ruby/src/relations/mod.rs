//! Relation extraction helpers for the Ruby plugin.
//!
//! Relation extraction is handled by `RubyGraphBuilder` which implements
//! `sqry_core::graph::GraphBuilder` to build the unified `CodeGraph`.
//!
//! YARD documentation parsing is used to extract `TypeOf` and Reference edges.

pub mod graph_builder;
pub mod type_extractor;
pub mod yard_parser;

pub use graph_builder::RubyGraphBuilder;
