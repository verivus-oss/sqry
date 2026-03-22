//! Shared relation extraction infrastructure for language plugins.
//!
//! This module provides common utilities used by all language plugins:
//! - `identity` - Call identity building utilities
//! - `queries` - Tree-sitter query helpers
//! - `types` - Synthetic name generation

/// Builder utilities for constructing canonical call identities.
pub mod identity;
pub mod queries;
pub mod types;

pub use identity::{CallIdentityBuilder, CallIdentityKind, CallIdentityMetadata};
pub use types::SyntheticNameBuilder;
