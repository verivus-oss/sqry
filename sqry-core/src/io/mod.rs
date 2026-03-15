//! I/O utilities for efficient file operations
//!
//! This module provides abstractions for file reading with support for:
//! - Memory-mapped files for large reads
//! - Buffered reading for small files or when mmap is unavailable
//! - Automatic fallback based on file size and platform support

pub mod binary;
pub mod file_reader;

pub use binary::{is_binary_bytes, is_binary_file};
pub use file_reader::{FileReader, ReaderPolicy};
