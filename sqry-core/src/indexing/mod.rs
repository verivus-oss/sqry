//! Indexing utilities shared across graph builders and caches.
//!
//! Provides file change hashing and index compression helpers that are
//! independent of any legacy symbol-based workflows. No build uses the change
//! hashing to skip files (see [`incremental`]).

pub mod compression;
pub mod incremental;

pub use compression::{
    CompressedIndex, CompressionError, CompressionFormat, DEFAULT_COMPRESSION_LEVEL,
};
pub use incremental::{FileHash, HashIndex};
