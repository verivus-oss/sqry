//! Relation extraction for Terraform/HCL documents.
//!
//! Provides `TerraformGraphBuilder` for extracting Terraform module relationships:
//! - Module source references (`module { source = "..." }`)
//! - Provider dependencies (`provider "aws" { ... }`)
//! - Resource references within modules
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;

pub use graph_builder::TerraformGraphBuilder;
