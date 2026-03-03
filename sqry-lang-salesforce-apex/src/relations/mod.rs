//! Relation extraction for Salesforce Apex code.
//!
//! Provides `ApexGraphBuilder` for extracting Apex relationships:
//! - Method calls within classes
//! - Class inheritance (extends)
//! - Interface implementations
//! - DML operations (insert/update/delete/upsert)
//! - SOQL queries referencing sObjects
//! - TypeOf edges for type annotations
//! - References edges for type dependencies
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;
pub(crate) mod type_extractor;

pub use graph_builder::ApexGraphBuilder;
