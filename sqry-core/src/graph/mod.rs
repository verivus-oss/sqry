//! Unified graph architecture for cross-language code analysis
//!
//! This module implements the core data structures and algorithms for sqry's
//! unified graph architecture, enabling cross-language queries, visualization,
//! and advanced analysis.
//!
//! # Architecture
//!
//! The graph architecture consists of:
//! - **Nodes**: Code entities (functions, classes, modules)
//! - **Edges**: Relationships (calls, imports, HTTP, FFI)
//! - **Indices**: Fast lookup structures for queries
//! - **Builders**: Language-specific graph construction
//!
//! # Memory Optimization
//!
//! Per AGENTS.md:149-151, strings are interned via `Arc<str>` to reduce memory
//! usage in symbol-heavy data structures (saves 10-50 MB for typical repos).
//!
//! # Concurrency
//!
//! The graph uses interior mutability (DashMap) to allow concurrent graph
//! building while maintaining a shared reference (&CodeGraph).
//!
//! # Example
//!
//! ```rust
//! use sqry_core::graph::node::{NodeId, Language};
//!
//! // Create node identifiers with string interning
//! let node_id = NodeId::new(Language::Cpp, "main.cpp", "main");
//! assert_eq!(node_id.to_string(), "cpp:main.cpp:main");
//!
//! // Node name extraction works across languages
//! let cpp_id = NodeId::new(Language::Cpp, "utils.cpp", "std::vector::push_back");
//! assert_eq!(cpp_id.symbol_name(), "push_back");
//!
//! let py_id = NodeId::new(Language::Python, "api.py", "User.authenticate");
//! assert_eq!(py_id.symbol_name(), "authenticate");
//! ```

pub mod body_hash;
pub mod builder;
pub mod call_sites;
pub mod edge;
pub mod error;
pub mod local_scopes;
pub mod node;
pub mod path_resolver;

/// Unified graph architecture with Arena+CSR storage.
///
/// This module provides the new high-performance graph implementation
/// with generational indices, two-tier edge storage, and MVCC concurrency.
///
/// See `docs/development/graph-architecture-reconcile-*/` for design docs.
pub mod unified;

/// Graph comparison and diff functionality.
///
/// This module provides tools for comparing two CodeGraph instances to detect
/// symbol changes between different versions of code (e.g., git refs).
pub mod diff;

// ============================================================================
// Primary Exports (Unified Graph - FR-2025-007)
// ============================================================================

/// The unified code graph with Arena+CSR storage.
///
/// This is the primary graph type for new code. It provides:
/// - Generational indices for safe node/edge references
/// - Two-tier edge storage (inline for common, external for metadata-heavy)
/// - MVCC concurrency via snapshots
///
/// For concurrent building, use [`ConcurrentCodeGraph`] which wraps this type
/// with interior mutability.
pub use unified::concurrent::CodeGraph;

/// Thread-safe wrapper for concurrent graph building.
pub use unified::ConcurrentCodeGraph;

/// Immutable graph snapshot for queries.
pub use unified::GraphSnapshot;

// ============================================================================
// Core Type Exports
// ============================================================================

pub use body_hash::{BodyHash128, HASH_SEED_0, HASH_SEED_1};
pub use builder::GraphBuilder;
pub use call_sites::{CallSite, CallSiteExtras, CallSiteMetadata};
pub use edge::{CodeEdge, DetectionMethod, EdgeId, EdgeKind, EdgeMetadata, FFIType, TableWriteOp};
pub use error::{GraphBuilderError, GraphResult};
pub use node::{CodeNode, Language, NodeId, NodeKind, Position, Span};
pub use path_resolver::{normalize_path, resolve_import_path, resolve_python_import};
