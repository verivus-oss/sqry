//! sqry-core: Core library for semantic code search
//!
//! This library provides the foundational components for sqry, a semantic code search tool
//! that understands code structure through AST analysis.
//!
//! # Architecture
//!
//! The unified code graph is the centre of the crate. Language plugins parse into it,
//! queries are planned and evaluated against it, and it is what gets persisted.
//!
//! - **graph**: The unified arena + CSR code graph, its builders and persistence
//! - **query**: Query parsing, planning, and evaluation over the graph
//! - **relations**: Cross-file and cross-language edge derivation
//! - **plugin**: Plugin system for language extensibility
//! - **ast**: Tree-sitter parsing and AST querying
//! - **search**: Text and hybrid search, used when a query is not structural
//! - **indexing**: File change hashing and index compression utilities
//! - **persistence**: Snapshot read and write, including format upconversion
//! - **workspace**: Workspace root discovery and multi-root resolution
//! - **cache** and **session**: Caching for warm multi-query execution
//! - **schema**: On-disk types whose discriminants are pinned across versions
//! - **output**: Output formatters (text, JSON)
//!
//! # Example
//!
//! ```
//! use sqry_core::search::fallback::FallbackSearchEngine;
//!
//! let engine = FallbackSearchEngine::new().expect("default text searcher");
//! # let _ = engine;
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![allow(clippy::mixed_attributes_style)]
#![cfg_attr(
    test,
    allow(
        clippy::large_stack_arrays,
        reason = "libtest harness generates a large test table; no user code allocates large stack arrays"
    )
)]

/// Core search functionality
pub mod search;

/// AST parsing and querying
pub mod ast;

/// Error types for programmatic tool integration
pub mod errors;

/// Configuration module (buffer sizes, tuning parameters)
pub mod config;

/// JSON response types for programmatic tool integration
pub mod json_response;

/// I/O utilities for efficient file operations
pub mod io;

/// BLAKE3 hashing utilities for cache module
pub mod hash;

/// Caching layer
pub mod cache;

/// Indexing utilities (file change hashing, compression)
pub mod indexing;

/// Progress reporting for indexing and graph builds
pub mod progress;

/// Session management for multi-query caching
pub mod session;

/// File system watcher for CLI watch mode
pub mod watch;

/// Plugin system for language extensibility
pub mod plugin;

/// Shared metadata constants for language plugins
pub mod metadata;

/// Metadata normalization for backward compatibility
pub mod normalizer;

/// Query language for AST-aware code search
pub mod query;

/// Workspace registry and discovery utilities
pub mod workspace;

/// Visualization helpers (graph exporters for DOT, D2, Mermaid, JSON)
pub mod visualization;

/// Output formatting (text, JSON, diagrams)
pub mod output;

/// On-disk persistence helpers (atomic-write, snapshot I/O primitives)
pub mod persistence;

/// Unified graph architecture for cross-language code analysis
pub mod graph;

/// Canonical schema types (single source of truth for semantic enums)
pub mod schema;

/// Shared relation extraction infrastructure for language plugins
pub mod relations;

/// Git integration for change tracking
pub mod git;

/// Project root lifecycle management (per PROJECT_ROOT_SPEC.md)
pub mod project;

/// Confidence metadata for analysis results
pub mod confidence;

/// Local uses and insights (privacy-respecting behavioral capture)
///
/// This module provides anonymous usage pattern collection that stays entirely local.
/// All data uses strongly-typed enums - no arbitrary strings can leak.
/// Users can disable via config, environment, or by building without the feature.
#[cfg(feature = "uses")]
pub mod uses;

/// Common types and utilities
pub mod common {
    //! Common types used across the library
}

/// Test support utilities (verbose logging, artifacts)
///
/// This module provides testing infrastructure including environment-variable
/// driven logging and test artifact generation. It's designed to be zero-overhead
/// when not in use and completely opt-in via environment variables.
///
/// See module documentation for usage examples.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Re-exports for convenience
pub use anyhow::{Error, Result};
pub use confidence::{ConfidenceLevel, ConfidenceMetadata};
