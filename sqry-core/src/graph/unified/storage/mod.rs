//! Storage components for the unified graph architecture.
//!
//! This module provides the core storage types:
//! - [`NodeArena`]: Contiguous node storage with generational indices
//! - [`StringInterner`]: Node name deduplication (Step 6)
//! - [`FileRegistry`]: File path deduplication (Step 7)
//! - [`AuxiliaryIndices`]: Fast lookup indices by kind, name, file (Step 7b)
//! - [`CsrGraph`]: Compressed Sparse Row edge storage (Step 8)
//!
//! # Design Principles
//!
//! - **Cache-friendly**: Contiguous storage for optimal CPU cache usage
//! - **Generational safety**: Stale references detected via generation mismatch
//! - **Memory efficient**: Free list reuse of deleted slots
//! - **O(1) lookup**: Auxiliary indices for fast queries by attribute
//! - **O(1) edge range**: CSR format for fast edge iteration

pub mod arena;
pub mod csr;
pub mod indices;
pub mod interner;
pub mod metadata;
pub mod registry;
pub mod serde_helpers;

pub use arena::{ArenaError, NodeArena, NodeEntry, Slot, SlotState};
pub use csr::{CsrBuilder, CsrError, CsrGraph, CsrStats, EdgeRef};
pub use indices::{AuxiliaryIndices, IndicesStats};
pub use interner::{InternError, InternerStats, ResolveError, StringInterner};
pub use metadata::{
    ClasspathNodeMetadata, MacroNodeMetadata, NodeMetadata, NodeMetadataStore,
    ProcMacroFunctionKind,
};
pub use registry::{FileRegistry, RegistryError, RegistryStats};
