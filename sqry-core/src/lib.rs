//! sqry-core: Core library for semantic code search
//!
//! This library provides the foundational components for sqry, a semantic code search tool
//! that understands code structure through AST analysis.
//!
//! # Architecture
//!
//! The library is organized into several key modules:
//!
//! - **search**: Core search engine and pattern matching
//! - **ast**: AST parsing and querying
//! - **indexing**: Incremental hashing and index compression utilities
//! - **cache**: Caching layer for performance
//! - **session**: Session-level caching for warm multi-query execution
//! - **plugin**: Plugin system for language extensibility
//! - **output**: Output formatters (text, JSON)
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::search::SearchEngine;
//!
//! let engine = SearchEngine::new();
//! let results = engine.search("pattern", "path/to/code")?;
//! ```
//!
//! # Status
//!
//! This library is under active development (Phase 0). APIs are subject to change.

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

/// Indexing utilities (incremental hashing, compression)
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

// NOTE: The original hybrid embeddings path (embeddings/vector/security
// modules and related features) has been removed from the codebase. Natural language
// support is provided exclusively via the NL→SQRY translation interface described in
// docs/development/-nl-translation/01_SPEC.md.

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
