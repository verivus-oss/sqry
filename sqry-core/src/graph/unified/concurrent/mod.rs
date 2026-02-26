//! Concurrent graph wrappers for thread-safe access.
//!
//! This module implements thread-safe wrappers for the unified graph architecture:
//!
//! - [`CodeGraph`]: Core graph with Arc-wrapped internals for O(1) `CoW` snapshots
//! - [`ConcurrentCodeGraph`]: Thread-safe wrapper with `RwLock` and epoch versioning
//! - [`GraphSnapshot`]: Immutable snapshot for long-running queries (Step 19)
//!
//! # Design (FR-34, FR-47)
//!
//! The concurrency model follows MVCC (Multi-Version Concurrency Control):
//!
//! - **Single writer**: Updates are serialized via `RwLock` write guard
//! - **Multiple readers**: Shared read access via `RwLock` read guard
//! - **Cheap snapshots**: O(1) Arc clones for immutable query views
//! - **Epoch versioning**: Monotonic counter for cursor invalidation
//!
//! # Lock Order
//!
//! To prevent deadlocks, always acquire locks in this order:
//! ```text
//! ConcurrentCodeGraph → NodeArena → EdgeStore → StringInterner
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::concurrent::{ConcurrentCodeGraph, CodeGraph};
//!
//! // Create a concurrent graph
//! let graph = ConcurrentCodeGraph::new();
//!
//! // Read access (shared)
//! {
//!     let read_guard = graph.read();
//!     let nodes = read_guard.nodes();
//!     // ... query nodes ...
//! }
//!
//! // Write access (exclusive)
//! {
//!     let mut write_guard = graph.write();
//!     let nodes_mut = write_guard.nodes_mut();
//!     // ... mutate nodes ...
//! }
//!
//! // Create snapshot for long queries
//! let snapshot = graph.snapshot();
//! // snapshot is independent of future mutations
//! ```

mod channel;
mod graph;

pub use channel::{ChannelError, ChannelStats, GraphUpdate, UpdateChannel, UpdateReceiver};
pub use graph::{CodeGraph, ConcurrentCodeGraph, GraphSnapshot};
