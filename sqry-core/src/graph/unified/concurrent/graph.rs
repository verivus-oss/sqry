//! `CodeGraph` and `ConcurrentCodeGraph` implementations.
//!
//! This module provides the core graph types with thread-safe access:
//!
//! - [`CodeGraph`]: Arc-wrapped internals for O(1) `CoW` snapshots
//! - [`ConcurrentCodeGraph`]: `RwLock` wrapper with epoch versioning
//! - [`GraphSnapshot`]: Immutable snapshot for long-running queries

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::confidence::ConfidenceMetadata;
use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::storage::metadata::NodeMetadataStore;
use crate::graph::unified::storage::registry::FileRegistry;

/// Core graph with Arc-wrapped internals for O(1) `CoW` snapshots.
///
/// `CodeGraph` uses `Arc` for all internal components, enabling:
/// - O(1) snapshot creation via Arc cloning
/// - Copy-on-write semantics via `Arc::make_mut`
/// - Memory-efficient sharing between snapshots
///
/// # Design
///
/// The Arc wrapping enables the MVCC pattern:
/// - Readers see a consistent snapshot at the time they acquired access
/// - Writers use `Arc::make_mut` to get exclusive copies only when mutating
/// - Multiple snapshots can coexist without copying data
///
/// # Performance
///
/// - Snapshot creation: O(5) Arc clones ≈ <1μs
/// - Read access: Direct Arc dereference, no locking
/// - Write access: `Arc::make_mut` clones only if refcount > 1
#[derive(Clone)]
pub struct CodeGraph {
    /// Node storage with generational indices.
    nodes: Arc<NodeArena>,
    /// Bidirectional edge storage (forward + reverse).
    edges: Arc<BidirectionalEdgeStore>,
    /// String interner for symbol names.
    strings: Arc<StringInterner>,
    /// File registry for path deduplication.
    files: Arc<FileRegistry>,
    /// Auxiliary indices for fast lookup.
    indices: Arc<AuxiliaryIndices>,
    /// Sparse macro boundary metadata (keyed by full NodeId).
    macro_metadata: Arc<NodeMetadataStore>,
    /// Epoch for version tracking.
    epoch: u64,
    /// Per-language confidence metadata collected during build.
    /// Maps language name (e.g., "rust") to aggregated confidence.
    confidence: HashMap<String, ConfidenceMetadata>,
}

impl CodeGraph {
    /// Creates a new empty `CodeGraph`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.epoch(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(NodeArena::new()),
            edges: Arc::new(BidirectionalEdgeStore::new()),
            strings: Arc::new(StringInterner::new()),
            files: Arc::new(FileRegistry::new()),
            indices: Arc::new(AuxiliaryIndices::new()),
            macro_metadata: Arc::new(NodeMetadataStore::new()),
            epoch: 0,
            confidence: HashMap::new(),
        }
    }

    /// Creates a `CodeGraph` from existing components.
    ///
    /// This is useful when building a graph from external data or
    /// reconstructing from serialized state.
    #[must_use]
    pub fn from_components(
        nodes: NodeArena,
        edges: BidirectionalEdgeStore,
        strings: StringInterner,
        files: FileRegistry,
        indices: AuxiliaryIndices,
        macro_metadata: NodeMetadataStore,
    ) -> Self {
        Self {
            nodes: Arc::new(nodes),
            edges: Arc::new(edges),
            strings: Arc::new(strings),
            files: Arc::new(files),
            indices: Arc::new(indices),
            macro_metadata: Arc::new(macro_metadata),
            epoch: 0,
            confidence: HashMap::new(),
        }
    }

    /// Creates a cheap snapshot of the graph.
    ///
    /// This operation is O(5) Arc clones, which completes in <1μs.
    /// The snapshot is isolated from future mutations to the original graph.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// let snapshot = graph.snapshot();
    /// // snapshot is independent of future mutations to graph
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> GraphSnapshot {
        GraphSnapshot {
            nodes: Arc::clone(&self.nodes),
            edges: Arc::clone(&self.edges),
            strings: Arc::clone(&self.strings),
            files: Arc::clone(&self.files),
            indices: Arc::clone(&self.indices),
            macro_metadata: Arc::clone(&self.macro_metadata),
            epoch: self.epoch,
        }
    }

    /// Returns a reference to the node arena.
    #[inline]
    #[must_use]
    pub fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    /// Returns a reference to the bidirectional edge store.
    #[inline]
    #[must_use]
    pub fn edges(&self) -> &BidirectionalEdgeStore {
        &self.edges
    }

    /// Returns a reference to the string interner.
    #[inline]
    #[must_use]
    pub fn strings(&self) -> &StringInterner {
        &self.strings
    }

    /// Returns a reference to the file registry.
    #[inline]
    #[must_use]
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// Returns a reference to the auxiliary indices.
    #[inline]
    #[must_use]
    pub fn indices(&self) -> &AuxiliaryIndices {
        &self.indices
    }

    /// Returns a reference to the macro boundary metadata store.
    #[inline]
    #[must_use]
    pub fn macro_metadata(&self) -> &NodeMetadataStore {
        &self.macro_metadata
    }

    /// Returns the current epoch.
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns a mutable reference to the node arena.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics: if other
    /// references exist (e.g., snapshots), the data is cloned.
    #[inline]
    pub fn nodes_mut(&mut self) -> &mut NodeArena {
        Arc::make_mut(&mut self.nodes)
    }

    /// Returns a mutable reference to the bidirectional edge store.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn edges_mut(&mut self) -> &mut BidirectionalEdgeStore {
        Arc::make_mut(&mut self.edges)
    }

    /// Returns a mutable reference to the string interner.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn strings_mut(&mut self) -> &mut StringInterner {
        Arc::make_mut(&mut self.strings)
    }

    /// Returns a mutable reference to the file registry.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn files_mut(&mut self) -> &mut FileRegistry {
        Arc::make_mut(&mut self.files)
    }

    /// Returns a mutable reference to the auxiliary indices.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn indices_mut(&mut self) -> &mut AuxiliaryIndices {
        Arc::make_mut(&mut self.indices)
    }

    /// Returns a mutable reference to the macro boundary metadata store.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn macro_metadata_mut(&mut self) -> &mut NodeMetadataStore {
        Arc::make_mut(&mut self.macro_metadata)
    }

    /// Returns mutable references to both the node arena and the string interner.
    ///
    /// This avoids the borrow-conflict that arises when calling `nodes_mut()` and
    /// `strings_mut()` separately on `&mut self`.
    #[inline]
    pub fn nodes_and_strings_mut(&mut self) -> (&mut NodeArena, &mut StringInterner) {
        (
            Arc::make_mut(&mut self.nodes),
            Arc::make_mut(&mut self.strings),
        )
    }

    /// Rebuilds auxiliary indices from the current node arena.
    ///
    /// This avoids the borrow conflict that arises when calling `nodes()` and
    /// `indices_mut()` separately on `&mut self`. Uses disjoint field borrowing
    /// to access `nodes` (shared) and `indices` (mutable) simultaneously.
    /// Internally calls `AuxiliaryIndices::build_from_arena` which clears
    /// existing indices and rebuilds in a single pass without per-element
    /// duplicate checking.
    pub fn rebuild_indices(&mut self) {
        let nodes = &self.nodes;
        Arc::make_mut(&mut self.indices).build_from_arena(nodes);
    }

    /// Increments the epoch counter and returns the new value.
    ///
    /// Called automatically by `ConcurrentCodeGraph::write()`.
    #[inline]
    pub fn bump_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// Sets the epoch to a specific value.
    ///
    /// This is primarily for testing or reconstruction from serialized state.
    #[inline]
    pub fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    /// Returns the number of nodes in the graph.
    ///
    /// This is a convenience method that delegates to `nodes().len()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.node_count(), 0);
    /// ```
    #[inline]
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the number of edges in the graph (forward direction).
    ///
    /// This counts edges in the forward store, including both CSR and delta edges.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.edge_count(), 0);
    /// ```
    #[inline]
    #[must_use]
    pub fn edge_count(&self) -> usize {
        let stats = self.edges.stats();
        stats.forward.csr_edge_count + stats.forward.delta_edge_count
    }

    /// Returns true if the graph contains no nodes.
    ///
    /// This is a convenience method that delegates to `nodes().is_empty()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert!(graph.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns an iterator over all indexed file paths.
    ///
    /// This is useful for enumerating all files that have been processed
    /// and added to the graph.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// for (file_id, path) in graph.indexed_files() {
    ///     println!("File {}: {}", file_id.index(), path.display());
    /// }
    /// ```
    #[inline]
    pub fn indexed_files(
        &self,
    ) -> impl Iterator<Item = (crate::graph::unified::file::FileId, &std::path::Path)> {
        self.files
            .iter()
            .map(|(id, arc_path)| (id, arc_path.as_ref()))
    }

    /// Returns the per-language confidence metadata.
    ///
    /// This contains analysis confidence information collected during graph build,
    /// primarily used by language plugins (e.g., Rust) to track analysis quality.
    #[inline]
    #[must_use]
    pub fn confidence(&self) -> &HashMap<String, ConfidenceMetadata> {
        &self.confidence
    }

    /// Merges confidence metadata for a language.
    ///
    /// If confidence already exists for the language, this merges the new
    /// metadata (taking the lower confidence level and combining limitations).
    /// Otherwise, it inserts the new confidence.
    pub fn merge_confidence(&mut self, language: &str, metadata: ConfidenceMetadata) {
        use crate::confidence::ConfidenceLevel;

        self.confidence
            .entry(language.to_string())
            .and_modify(|existing| {
                // Take the lower confidence level (more conservative)
                let new_level = match (&existing.level, &metadata.level) {
                    (ConfidenceLevel::Verified, other) | (other, ConfidenceLevel::Verified) => {
                        *other
                    }
                    (ConfidenceLevel::Partial, ConfidenceLevel::AstOnly)
                    | (ConfidenceLevel::AstOnly, ConfidenceLevel::Partial) => {
                        ConfidenceLevel::AstOnly
                    }
                    (level, _) => *level,
                };
                existing.level = new_level;

                // Merge limitations (deduplicated)
                for limitation in &metadata.limitations {
                    if !existing.limitations.contains(limitation) {
                        existing.limitations.push(limitation.clone());
                    }
                }

                // Merge unavailable features (deduplicated)
                for feature in &metadata.unavailable_features {
                    if !existing.unavailable_features.contains(feature) {
                        existing.unavailable_features.push(feature.clone());
                    }
                }
            })
            .or_insert(metadata);
    }

    /// Sets the confidence metadata map directly.
    ///
    /// This is primarily used when loading a graph from serialized state.
    pub fn set_confidence(&mut self, confidence: HashMap<String, ConfidenceMetadata>) {
        self.confidence = confidence;
    }
}

impl Default for CodeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CodeGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeGraph")
            .field("nodes", &self.nodes.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Thread-safe wrapper for `CodeGraph` with epoch versioning.
///
/// `ConcurrentCodeGraph` provides MVCC-style concurrency:
/// - Multiple readers can access the graph simultaneously
/// - Only one writer can hold the lock at a time
/// - Each write operation increments the epoch for cursor invalidation
///
/// # Design
///
/// The wrapper uses `parking_lot::RwLock` for efficient locking:
/// - Fair scheduling prevents writer starvation
/// - No poisoning (unlike `std::sync::RwLock`)
/// - Faster lock/unlock operations
///
/// # Usage
///
/// ```rust
/// use sqry_core::graph::unified::concurrent::ConcurrentCodeGraph;
///
/// let graph = ConcurrentCodeGraph::new();
///
/// // Read access (multiple readers allowed)
/// {
///     let guard = graph.read();
///     let _nodes = guard.nodes();
/// }
///
/// // Write access (exclusive)
/// {
///     let mut guard = graph.write();
///     let _nodes = guard.nodes_mut();
/// }
///
/// // Snapshot for long queries
/// let snapshot = graph.snapshot();
/// ```
pub struct ConcurrentCodeGraph {
    /// The underlying code graph protected by a read-write lock.
    inner: RwLock<CodeGraph>,
    /// Global epoch counter for cursor validation.
    epoch: AtomicU64,
}

impl ConcurrentCodeGraph {
    /// Creates a new empty `ConcurrentCodeGraph`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CodeGraph::new()),
            epoch: AtomicU64::new(0),
        }
    }

    /// Creates a `ConcurrentCodeGraph` from an existing `CodeGraph`.
    #[must_use]
    pub fn from_graph(graph: CodeGraph) -> Self {
        let epoch = graph.epoch();
        Self {
            inner: RwLock::new(graph),
            epoch: AtomicU64::new(epoch),
        }
    }

    /// Acquires a read lock on the graph.
    ///
    /// Multiple readers can hold the lock simultaneously.
    /// This does not increment the epoch.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, CodeGraph> {
        self.inner.read()
    }

    /// Acquires a write lock on the graph.
    ///
    /// Only one writer can hold the lock at a time.
    /// This increments the global epoch counter.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, CodeGraph> {
        // Increment the global epoch
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.inner.write();
        // Sync the inner graph's epoch with the global epoch
        guard.set_epoch(self.epoch.load(Ordering::SeqCst));
        guard
    }

    /// Returns the current global epoch.
    ///
    /// This can be used to detect if the graph has been modified
    /// since a previous operation (cursor invalidation).
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Creates a cheap snapshot of the graph.
    ///
    /// This acquires a brief read lock to clone the Arc references.
    /// The snapshot is isolated from future mutations.
    #[must_use]
    pub fn snapshot(&self) -> GraphSnapshot {
        self.inner.read().snapshot()
    }

    /// Attempts to acquire a read lock without blocking.
    ///
    /// Returns `None` if the lock is currently held exclusively.
    #[inline]
    #[must_use]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, CodeGraph>> {
        self.inner.try_read()
    }

    /// Attempts to acquire a write lock without blocking.
    ///
    /// Returns `None` if the lock is currently held by another thread.
    /// If successful, increments the epoch.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, CodeGraph>> {
        self.inner.try_write().map(|mut guard| {
            self.epoch.fetch_add(1, Ordering::SeqCst);
            guard.set_epoch(self.epoch.load(Ordering::SeqCst));
            guard
        })
    }
}

impl Default for ConcurrentCodeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConcurrentCodeGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConcurrentCodeGraph")
            .field("epoch", &self.epoch.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

/// Immutable snapshot of a `CodeGraph` for long-running queries.
///
/// `GraphSnapshot` holds Arc references to the graph components,
/// providing a consistent view that is isolated from concurrent mutations.
///
/// # Design
///
/// Snapshots are created via `CodeGraph::snapshot()` or
/// `ConcurrentCodeGraph::snapshot()`. They are:
///
/// - **Immutable**: No mutation methods available
/// - **Isolated**: Independent of future graph mutations
/// - **Cheap**: Only Arc clones, no data copying
/// - **Self-contained**: Can outlive the original graph/lock
///
/// # Usage
///
/// ```rust
/// use sqry_core::graph::unified::concurrent::{ConcurrentCodeGraph, GraphSnapshot};
///
/// let graph = ConcurrentCodeGraph::new();
///
/// // Create snapshot for a long query
/// let snapshot: GraphSnapshot = graph.snapshot();
///
/// // Snapshot can be used independently
/// let _epoch = snapshot.epoch();
/// ```
#[derive(Clone)]
pub struct GraphSnapshot {
    /// Node storage snapshot.
    nodes: Arc<NodeArena>,
    /// Edge storage snapshot.
    edges: Arc<BidirectionalEdgeStore>,
    /// String interner snapshot.
    strings: Arc<StringInterner>,
    /// File registry snapshot.
    files: Arc<FileRegistry>,
    /// Auxiliary indices snapshot.
    indices: Arc<AuxiliaryIndices>,
    /// Sparse macro boundary metadata snapshot.
    macro_metadata: Arc<NodeMetadataStore>,
    /// Epoch at snapshot time (for cursor validation).
    epoch: u64,
}

impl GraphSnapshot {
    /// Returns a reference to the node arena.
    #[inline]
    #[must_use]
    pub fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    /// Returns a reference to the bidirectional edge store.
    #[inline]
    #[must_use]
    pub fn edges(&self) -> &BidirectionalEdgeStore {
        &self.edges
    }

    /// Returns a reference to the string interner.
    #[inline]
    #[must_use]
    pub fn strings(&self) -> &StringInterner {
        &self.strings
    }

    /// Returns a reference to the file registry.
    #[inline]
    #[must_use]
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// Returns a reference to the auxiliary indices.
    #[inline]
    #[must_use]
    pub fn indices(&self) -> &AuxiliaryIndices {
        &self.indices
    }

    /// Returns a reference to the macro boundary metadata store.
    #[inline]
    #[must_use]
    pub fn macro_metadata(&self) -> &NodeMetadataStore {
        &self.macro_metadata
    }

    /// Returns the epoch at which this snapshot was taken.
    ///
    /// This can be compared against the current graph epoch to
    /// detect if the graph has changed since the snapshot.
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns `true` if this snapshot's epoch matches the given epoch.
    ///
    /// Use this to validate cursors before continuing pagination.
    #[inline]
    #[must_use]
    pub fn epoch_matches(&self, other_epoch: u64) -> bool {
        self.epoch == other_epoch
    }

    // ============================================================================
    // Query Methods
    // ============================================================================

    /// Finds nodes matching a pattern.
    ///
    /// Performs a simple substring match on node names and qualified names.
    /// Returns all matching node IDs.
    ///
    /// # Performance
    ///
    /// Optimized to iterate over unique strings in the interner (smaller set)
    /// rather than all nodes in the arena.
    ///
    /// # Arguments
    ///
    /// * `pattern` - The pattern to match (substring search)
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` for all matching nodes.
    #[must_use]
    pub fn find_by_pattern(&self, pattern: &str) -> Vec<crate::graph::unified::node::NodeId> {
        let mut matches = Vec::new();

        // 1. Scan unique strings in interner for matches
        for (str_id, s) in self.strings.iter() {
            if s.contains(pattern) {
                // 2. If string matches, look up all nodes with this name
                // Check qualified name index
                matches.extend_from_slice(self.indices.by_qualified_name(str_id));
                // Check simple name index
                matches.extend_from_slice(self.indices.by_name(str_id));
            }
        }

        // Deduplicate matches (a node might match both qualified and simple name)
        matches.sort_unstable();
        matches.dedup();

        matches
    }

    /// Gets all callees of a node (functions called by this node).
    ///
    /// Queries the forward edge store for all Calls edges from this node.
    ///
    /// # Arguments
    ///
    /// * `node` - The node ID to query
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` representing functions called by this node.
    #[must_use]
    pub fn get_callees(
        &self,
        node: crate::graph::unified::node::NodeId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::edge::EdgeKind;

        self.edges
            .edges_from(node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .map(|edge| edge.target)
            .collect()
    }

    /// Gets all callers of a node (functions that call this node).
    ///
    /// Queries the reverse edge store for all Calls edges to this node.
    ///
    /// # Arguments
    ///
    /// * `node` - The node ID to query
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` representing functions that call this node.
    #[must_use]
    pub fn get_callers(
        &self,
        node: crate::graph::unified::node::NodeId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::edge::EdgeKind;

        self.edges
            .edges_to(node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .map(|edge| edge.source)
            .collect()
    }

    /// Iterates over all nodes in the graph.
    ///
    /// Returns an iterator yielding (`NodeId`, &`NodeEntry`) pairs for all
    /// occupied slots in the arena.
    ///
    /// # Returns
    ///
    /// An iterator over (`NodeId`, &`NodeEntry`) pairs.
    pub fn iter_nodes(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::graph::unified::node::NodeId,
            &crate::graph::unified::storage::arena::NodeEntry,
        ),
    > {
        self.nodes.iter()
    }

    /// Iterates over all edges in the graph.
    ///
    /// Returns an iterator yielding (source, target, `EdgeKind`) tuples for
    /// all edges in the forward edge store.
    ///
    /// # Returns
    ///
    /// An iterator over edge tuples.
    pub fn iter_edges(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::graph::unified::node::NodeId,
            crate::graph::unified::node::NodeId,
            crate::graph::unified::edge::EdgeKind,
        ),
    > + '_ {
        // Iterate over all nodes in the arena and get their outgoing edges
        self.nodes.iter().flat_map(move |(node_id, _entry)| {
            // Get all edges from this node
            self.edges
                .edges_from(node_id)
                .into_iter()
                .map(move |edge| (node_id, edge.target, edge.kind))
        })
    }

    /// Gets a node entry by ID.
    ///
    /// Returns a reference to the `NodeEntry` if the ID is valid, or None
    /// if the ID is invalid or stale.
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to look up
    ///
    /// # Returns
    ///
    /// A reference to the `NodeEntry`, or None if not found.
    #[must_use]
    pub fn get_node(
        &self,
        id: crate::graph::unified::node::NodeId,
    ) -> Option<&crate::graph::unified::storage::arena::NodeEntry> {
        self.nodes.get(id)
    }
}

impl fmt::Debug for GraphSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphSnapshot")
            .field("nodes", &self.nodes.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::{
        FileScope, NodeId, ResolutionMode, SymbolCandidateOutcome, SymbolQuery,
        SymbolResolutionOutcome,
    };

    fn resolve_symbol_strict(snapshot: &GraphSnapshot, symbol: &str) -> Option<NodeId> {
        match snapshot.resolve_symbol(&SymbolQuery {
            symbol,
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        }) {
            SymbolResolutionOutcome::Resolved(node_id) => Some(node_id),
            SymbolResolutionOutcome::NotFound
            | SymbolResolutionOutcome::FileNotIndexed
            | SymbolResolutionOutcome::Ambiguous(_) => None,
        }
    }

    fn candidate_nodes(snapshot: &GraphSnapshot, symbol: &str) -> Vec<NodeId> {
        match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol,
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(candidates) => candidates,
            SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
        }
    }

    #[test]
    fn test_code_graph_new() {
        let graph = CodeGraph::new();
        assert_eq!(graph.epoch(), 0);
        assert_eq!(graph.nodes().len(), 0);
    }

    #[test]
    fn test_code_graph_default() {
        let graph = CodeGraph::default();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_code_graph_snapshot() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        assert_eq!(snapshot.epoch(), 0);
        assert_eq!(snapshot.nodes().len(), 0);
    }

    #[test]
    fn test_code_graph_bump_epoch() {
        let mut graph = CodeGraph::new();
        assert_eq!(graph.epoch(), 0);
        assert_eq!(graph.bump_epoch(), 1);
        assert_eq!(graph.epoch(), 1);
        assert_eq!(graph.bump_epoch(), 2);
        assert_eq!(graph.epoch(), 2);
    }

    #[test]
    fn test_code_graph_set_epoch() {
        let mut graph = CodeGraph::new();
        graph.set_epoch(42);
        assert_eq!(graph.epoch(), 42);
    }

    #[test]
    fn test_code_graph_from_components() {
        let nodes = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let strings = StringInterner::new();
        let files = FileRegistry::new();
        let indices = AuxiliaryIndices::new();
        let macro_metadata = NodeMetadataStore::new();

        let graph =
            CodeGraph::from_components(nodes, edges, strings, files, indices, macro_metadata);
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_code_graph_mut_accessors() {
        let mut graph = CodeGraph::new();

        // Access mutable references - should not panic
        let _nodes = graph.nodes_mut();
        let _edges = graph.edges_mut();
        let _strings = graph.strings_mut();
        let _files = graph.files_mut();
        let _indices = graph.indices_mut();
    }

    #[test]
    fn test_code_graph_snapshot_isolation() {
        let mut graph = CodeGraph::new();
        let snapshot1 = graph.snapshot();

        // Mutate the graph
        graph.bump_epoch();

        let snapshot2 = graph.snapshot();

        // Snapshots should have different epochs
        assert_eq!(snapshot1.epoch(), 0);
        assert_eq!(snapshot2.epoch(), 1);
    }

    #[test]
    fn test_code_graph_debug() {
        let graph = CodeGraph::new();
        let debug_str = format!("{graph:?}");
        assert!(debug_str.contains("CodeGraph"));
        assert!(debug_str.contains("epoch"));
    }

    #[test]
    fn test_concurrent_code_graph_new() {
        let graph = ConcurrentCodeGraph::new();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_default() {
        let graph = ConcurrentCodeGraph::default();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_from_graph() {
        let mut inner = CodeGraph::new();
        inner.set_epoch(10);
        let graph = ConcurrentCodeGraph::from_graph(inner);
        assert_eq!(graph.epoch(), 10);
    }

    #[test]
    fn test_concurrent_code_graph_read() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.read();
        assert_eq!(guard.epoch(), 0);
        assert_eq!(guard.nodes().len(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_write_increments_epoch() {
        let graph = ConcurrentCodeGraph::new();
        assert_eq!(graph.epoch(), 0);

        {
            let guard = graph.write();
            assert_eq!(guard.epoch(), 1);
        }

        assert_eq!(graph.epoch(), 1);

        {
            let _guard = graph.write();
        }

        assert_eq!(graph.epoch(), 2);
    }

    #[test]
    fn test_concurrent_code_graph_snapshot() {
        let graph = ConcurrentCodeGraph::new();

        {
            let _guard = graph.write();
        }

        let snapshot = graph.snapshot();
        assert_eq!(snapshot.epoch(), 1);
    }

    #[test]
    fn test_concurrent_code_graph_try_read() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.try_read();
        assert!(guard.is_some());
    }

    #[test]
    fn test_concurrent_code_graph_try_write() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.try_write();
        assert!(guard.is_some());
        assert_eq!(graph.epoch(), 1);
    }

    #[test]
    fn test_concurrent_code_graph_debug() {
        let graph = ConcurrentCodeGraph::new();
        let debug_str = format!("{graph:?}");
        assert!(debug_str.contains("ConcurrentCodeGraph"));
        assert!(debug_str.contains("epoch"));
    }

    #[test]
    fn test_graph_snapshot_accessors() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        // All accessors should work
        let _nodes = snapshot.nodes();
        let _edges = snapshot.edges();
        let _strings = snapshot.strings();
        let _files = snapshot.files();
        let _indices = snapshot.indices();
        let _epoch = snapshot.epoch();
    }

    #[test]
    fn test_graph_snapshot_epoch_matches() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        assert!(snapshot.epoch_matches(0));
        assert!(!snapshot.epoch_matches(1));
    }

    #[test]
    fn test_graph_snapshot_clone() {
        let graph = CodeGraph::new();
        let snapshot1 = graph.snapshot();
        let snapshot2 = snapshot1.clone();

        assert_eq!(snapshot1.epoch(), snapshot2.epoch());
    }

    #[test]
    fn test_graph_snapshot_debug() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let debug_str = format!("{snapshot:?}");
        assert!(debug_str.contains("GraphSnapshot"));
        assert!(debug_str.contains("epoch"));
    }

    #[test]
    fn test_multiple_readers() {
        let graph = ConcurrentCodeGraph::new();

        // Multiple readers should be able to acquire locks simultaneously
        let guard1 = graph.read();
        let guard2 = graph.read();
        let guard3 = graph.read();

        assert_eq!(guard1.epoch(), 0);
        assert_eq!(guard2.epoch(), 0);
        assert_eq!(guard3.epoch(), 0);
    }

    #[test]
    fn test_code_graph_clone() {
        let mut graph = CodeGraph::new();
        graph.bump_epoch();

        let cloned = graph.clone();
        assert_eq!(cloned.epoch(), 1);
    }

    #[test]
    fn test_epoch_wrapping() {
        let mut graph = CodeGraph::new();
        graph.set_epoch(u64::MAX);
        let new_epoch = graph.bump_epoch();
        assert_eq!(new_epoch, 0); // Should wrap around
    }

    // ============================================================================
    // Query method tests
    // ============================================================================

    #[test]
    fn test_snapshot_resolve_symbol() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add some nodes with qualified names
        let name_id = graph.strings_mut().intern("test_func").unwrap();
        let qual_name_id = graph.strings_mut().intern("module::test_func").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let entry =
            NodeEntry::new(NodeKind::Function, name_id, file_id).with_qualified_name(qual_name_id);

        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph.indices_mut().add(
            node_id,
            NodeKind::Function,
            name_id,
            Some(qual_name_id),
            file_id,
        );

        let snapshot = graph.snapshot();

        // Find by qualified name
        let found = resolve_symbol_strict(&snapshot, "module::test_func");
        assert_eq!(found, Some(node_id));

        // Find by exact simple name
        let found2 = resolve_symbol_strict(&snapshot, "test_func");
        assert_eq!(found2, Some(node_id));

        // Not found
        assert!(resolve_symbol_strict(&snapshot, "nonexistent").is_none());
    }

    #[test]
    fn test_snapshot_find_by_pattern() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add nodes with different names
        let name1 = graph.strings_mut().intern("foo_bar").unwrap();
        let name2 = graph.strings_mut().intern("baz_bar").unwrap();
        let name3 = graph.strings_mut().intern("qux_test").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();
        let node3 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name3, file_id))
            .unwrap();

        graph
            .indices_mut()
            .add(node1, NodeKind::Function, name1, None, file_id);
        graph
            .indices_mut()
            .add(node2, NodeKind::Function, name2, None, file_id);
        graph
            .indices_mut()
            .add(node3, NodeKind::Function, name3, None, file_id);

        let snapshot = graph.snapshot();

        // Find by pattern
        let matches = snapshot.find_by_pattern("bar");
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&node1));
        assert!(matches.contains(&node2));

        // Find single match
        let matches = snapshot.find_by_pattern("qux");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], node3);

        // No matches
        let matches = snapshot.find_by_pattern("nonexistent");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_snapshot_get_callees() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create caller and callee nodes
        let caller_name = graph.strings_mut().intern("caller").unwrap();
        let callee1_name = graph.strings_mut().intern("callee1").unwrap();
        let callee2_name = graph.strings_mut().intern("callee2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let caller_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller_name, file_id))
            .unwrap();
        let callee1_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee1_name, file_id))
            .unwrap();
        let callee2_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee2_name, file_id))
            .unwrap();

        // Add call edges
        graph.edges_mut().add_edge(
            caller_id,
            callee1_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );
        graph.edges_mut().add_edge(
            caller_id,
            callee2_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Query callees
        let callees = snapshot.get_callees(caller_id);
        assert_eq!(callees.len(), 2);
        assert!(callees.contains(&callee1_id));
        assert!(callees.contains(&callee2_id));

        // Node with no callees
        let callees = snapshot.get_callees(callee1_id);
        assert!(callees.is_empty());
    }

    #[test]
    fn test_snapshot_get_callers() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create caller and callee nodes
        let caller1_name = graph.strings_mut().intern("caller1").unwrap();
        let caller2_name = graph.strings_mut().intern("caller2").unwrap();
        let callee_name = graph.strings_mut().intern("callee").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let caller1_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller1_name, file_id))
            .unwrap();
        let caller2_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller2_name, file_id))
            .unwrap();
        let callee_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee_name, file_id))
            .unwrap();

        // Add call edges
        graph.edges_mut().add_edge(
            caller1_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );
        graph.edges_mut().add_edge(
            caller2_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Query callers
        let callers = snapshot.get_callers(callee_id);
        assert_eq!(callers.len(), 2);
        assert!(callers.contains(&caller1_id));
        assert!(callers.contains(&caller2_id));

        // Node with no callers
        let callers = snapshot.get_callers(caller1_id);
        assert!(callers.is_empty());
    }

    #[test]
    fn test_snapshot_find_symbol_candidates() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add nodes with same symbol name but different qualified names
        let symbol_name = graph.strings_mut().intern("test").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, symbol_name, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Method, symbol_name, file_id))
            .unwrap();

        // Add a different symbol
        let other_name = graph.strings_mut().intern("other").unwrap();
        let node3 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, other_name, file_id))
            .unwrap();

        graph
            .indices_mut()
            .add(node1, NodeKind::Function, symbol_name, None, file_id);
        graph
            .indices_mut()
            .add(node2, NodeKind::Method, symbol_name, None, file_id);
        graph
            .indices_mut()
            .add(node3, NodeKind::Function, other_name, None, file_id);

        let snapshot = graph.snapshot();

        // Find by symbol
        let matches = candidate_nodes(&snapshot, "test");
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&node1));
        assert!(matches.contains(&node2));

        // Find other symbol
        let matches = candidate_nodes(&snapshot, "other");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], node3);

        // No matches
        let matches = candidate_nodes(&snapshot, "nonexistent");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_snapshot_iter_nodes() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add some nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        let snapshot = graph.snapshot();

        // Iterate nodes
        let snapshot_nodes: Vec<_> = snapshot.iter_nodes().collect();
        assert_eq!(snapshot_nodes.len(), 2);

        let node_ids: Vec<_> = snapshot_nodes.iter().map(|(id, _)| *id).collect();
        assert!(node_ids.contains(&node1));
        assert!(node_ids.contains(&node2));
    }

    #[test]
    fn test_snapshot_iter_edges() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        // Add edges
        graph.edges_mut().add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Iterate edges
        let edges: Vec<_> = snapshot.iter_edges().collect();
        assert_eq!(edges.len(), 1);

        let (src, tgt, kind) = &edges[0];
        assert_eq!(*src, node1);
        assert_eq!(*tgt, node2);
        assert!(matches!(
            kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
    }

    #[test]
    fn test_snapshot_get_node() {
        use crate::graph::unified::node::NodeId;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add a node
        let name = graph.strings_mut().intern("test_func").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file_id))
            .unwrap();

        let snapshot = graph.snapshot();

        // Get node
        let entry = snapshot.get_node(node_id);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().kind, NodeKind::Function);

        // Invalid node
        let invalid_id = NodeId::INVALID;
        assert!(snapshot.get_node(invalid_id).is_none());
    }

    #[test]
    fn test_snapshot_query_empty_graph() {
        use crate::graph::unified::node::NodeId;

        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        // All queries should return empty on empty graph
        assert!(resolve_symbol_strict(&snapshot, "test").is_none());
        assert!(snapshot.find_by_pattern("test").is_empty());
        assert!(candidate_nodes(&snapshot, "test").is_empty());

        let dummy_id = NodeId::new(0, 1);
        assert!(snapshot.get_callees(dummy_id).is_empty());
        assert!(snapshot.get_callers(dummy_id).is_empty());

        assert_eq!(snapshot.iter_nodes().count(), 0);
        assert_eq!(snapshot.iter_edges().count(), 0);
    }

    #[test]
    fn test_snapshot_edge_filtering_by_kind() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        // Add different kinds of edges
        graph.edges_mut().add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );
        graph
            .edges_mut()
            .add_edge(node1, node2, EdgeKind::References, file_id);

        let snapshot = graph.snapshot();

        // get_callees should only return Calls edges
        let callees = snapshot.get_callees(node1);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0], node2);

        // iter_edges returns all edges regardless of kind
        let edges: Vec<_> = snapshot.iter_edges().collect();
        assert_eq!(edges.len(), 2);
    }
}
