//! Query Builder API for type-safe programmatic query construction
//!
//! This module provides a fluent builder API for constructing queries programmatically
//! without parsing string queries. It offers compile-time safety for field names and
//! operators, with runtime validation against the [`FieldRegistry`](crate::query::FieldRegistry).
//!
//! # Features
//!
//! - **Type-safe construction**: Methods for all core fields prevent typos
//! - **Fluent API**: Method chaining for readable query construction
//! - **Validation**: Field, operator, value type, and enum constraint validation
//! - **Error accumulation**: All errors reported at `build()` time, not per-method
//! - **Zero-copy optimization**: Uses `Cow<'static, str>` for static field names
//!
//! # Example
//!
//! ```ignore
//! use sqry_core::query::builder::QueryBuilder;
//!
//! // Simple condition
//! let query = QueryBuilder::kind("function")
//!     .build()?;
//!
//! // Combined conditions
//! let query = QueryBuilder::kind("function")
//!     .and(QueryBuilder::lang("rust"))
//!     .and_not(QueryBuilder::name_matches("test.*"))
//!     .build()?;
//!
//! // OR conditions using static constructor
//! let query = QueryBuilder::any(vec![
//!     QueryBuilder::kind("function"),
//!     QueryBuilder::kind("method"),
//! ])
//! .and(QueryBuilder::lang("rust"))
//! .build()?;
//!
//! // Regex with custom flags
//! let query = QueryBuilder::name_matches_with("Test.*", |rb| rb.case_insensitive())
//!     .build()?;
//! ```
//!
//! # Architecture
//!
//! The builder is a thin layer over the existing Query AST types. It provides:
//!
//! 1. **Type-safe construction** of [`Expr`](crate::query::Expr) nodes
//! 2. **Validation** against [`FieldRegistry`](crate::query::FieldRegistry)
//! 3. **Conversion** to executable [`QueryAST`](crate::query::QueryAST) structures
//!
//! # Validation
//!
//! Validation is performed lazily at `build()` time:
//!
//! - Field existence (with alias resolution)
//! - Operator compatibility with field type
//! - Value type matching
//! - Enum value validation (for `kind`, `scope`, `scope.type`)
//! - Regex pattern syntax validation

mod condition;
mod error;
mod query_builder;
mod regex;

pub use condition::ConditionBuilder;
pub use error::BuildError;
pub use query_builder::QueryBuilder;
pub use regex::RegexBuilder;

#[cfg(test)]
mod tests;
