//! Node types for the unified graph architecture.
//!
//! This module provides the core node-related types:
//! - [`NodeId`]: Generational index handle for nodes
//! - [`NodeKind`]: Enumeration of code entity types
//! - [`CascadeCleanup`]: Coordinated node removal across graph structures
//!
//! # Design Principles
//!
//! - **Opaque handles**: `NodeId` is an opaque index, not a composite key
//! - **Generational safety**: Stale references are detected via generation mismatch
//! - **Type safety**: `NodeKind` provides exhaustive categorization of code entities
//! - **Cascade cleanup**: FR-33 ensures no dangling references after node removal

pub mod cascade;
pub mod id;
pub mod kind;

pub use cascade::{
    CascadeCleanup, CascadeRemovalResult, CascadeStats, FileCascadeResult, cascade_remove_file,
    cascade_remove_node,
};
pub use id::{GenerationOverflowError, NodeId};
pub use kind::NodeKind;
