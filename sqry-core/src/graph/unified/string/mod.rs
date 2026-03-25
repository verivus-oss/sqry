//! String interning types for the unified graph architecture.
//!
//! This module provides:
//! - [`StringId`]: Opaque handle for interned strings
//!
//! The `StringInterner` implementation is in the storage module (Step 6).

pub mod id;

pub use id::StringId;
