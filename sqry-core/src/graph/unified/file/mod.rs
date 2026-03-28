//! File registry types for the unified graph architecture.
//!
//! This module provides:
//! - [`FileId`]: Opaque handle for registered file paths
//!
//! The `FileRegistry` implementation is in the storage module (Step 7).

pub mod id;

pub use id::FileId;
