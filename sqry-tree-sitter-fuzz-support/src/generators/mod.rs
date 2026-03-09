//! Malformed input generators.
//!
//! This module provides generators for various types of malformed inputs
//! that test tree-sitter's FFI error handling and recovery mechanisms.

pub mod nesting;
pub mod random;
pub mod size;
pub mod utf8;

pub use nesting::generate_deeply_nested;
pub use random::{generate_random_bytes, generate_random_bytes_default, DEFAULT_SEED};
pub use size::{generate_100mb, generate_10mb, generate_1mb, generate_oversized, sizes};
pub use utf8::{
    generate_invalid_continuation, generate_null_bytes, generate_overlong_encoding,
    generate_surrogate_pairs, generate_truncated_utf8,
};
