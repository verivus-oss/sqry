//! `AuxiliaryIndices`: Fast lookup indices for nodes.
//!
//! This module implements `AuxiliaryIndices`, which provides O(1) lookup
//! of nodes by various attributes.
//!
//! # Design (FR-16)
//!
//! The indices provide:
//! - **`by_kind`**: Find all nodes of a specific `NodeKind`
//! - **`by_name`**: Find all nodes with a specific `StringId` name
//! - **`by_file`**: Find all nodes in a specific `FileId`
//!
//! # Performance
//!
//! - **Insertion**: O(1) amortized (`HashMap` + Vec push)
//! - **Lookup**: O(1) (`HashMap` lookup)
//! - **Removal**: O(n) for the specific index entry (Vec linear search)
//!
//! # Thread Safety
//!
//! The indices are not thread-safe. External synchronization (e.g., `RwLock`)
//! is required for concurrent access.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use super::super::file::id::FileId;
use super::super::node::id::NodeId;
use super::super::node::kind::NodeKind;
use super::super::string::id::StringId;

/// Auxiliary indices for fast node lookup.
///
/// `AuxiliaryIndices` maintains secondary indices over the node arena,
/// enabling efficient queries by node kind, name, or file.
///
/// # Invariants
///
/// - Every `NodeId` in an index must correspond to a valid node in the arena
/// - When a node is removed, it must be removed from all indices
/// - Indices are kept in sync by the graph implementation, not automatically
///
/// # Example
///
/// ```rust,ignore
/// let mut indices = AuxiliaryIndices::new();
///
/// // Add a node to indices
/// indices.add(node_id, NodeKind::Function, name_id, qualified_name, file_id);
///
/// // Query by kind
/// let functions = indices.by_kind(NodeKind::Function);
///
/// // Query by name
/// let matching = indices.by_name(name_id);
///
/// // Query by file
/// let in_file = indices.by_file(file_id);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxiliaryIndices {
    /// Index of nodes by their kind.
    kind_index: HashMap<NodeKind, Vec<NodeId>>,
    /// Index of nodes by their name.
    name_index: HashMap<StringId, Vec<NodeId>>,
    /// Index of nodes by their qualified name.
    qualified_name_index: HashMap<StringId, Vec<NodeId>>,
    /// Index of nodes by their file.
    file_index: HashMap<FileId, Vec<NodeId>>,
    /// Total number of indexed nodes.
    node_count: usize,
}

impl AuxiliaryIndices {
    /// Creates new empty auxiliary indices.
    #[must_use]
    pub fn new() -> Self {
        Self {
            kind_index: HashMap::new(),
            name_index: HashMap::new(),
            qualified_name_index: HashMap::new(),
            file_index: HashMap::new(),
            node_count: 0,
        }
    }

    /// Creates new indices with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            kind_index: HashMap::with_capacity(capacity / 10), // Estimate ~10 kinds
            name_index: HashMap::with_capacity(capacity),
            qualified_name_index: HashMap::with_capacity(capacity),
            file_index: HashMap::with_capacity(capacity / 50), // Estimate ~50 nodes per file
            node_count: 0,
        }
    }

    /// Returns the total number of indexed nodes.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.node_count
    }

    /// Returns true if no nodes are indexed.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.node_count == 0
    }

    /// Adds a node to all indices.
    ///
    /// If the node already exists in any index, it will not be added again
    /// (set semantics). This prevents duplicates during incremental rebuilds.
    ///
    /// # Arguments
    ///
    /// * `id` - The node's ID
    /// * `kind` - The node's kind
    /// * `name` - The node's name (`StringId`)
    /// * `qualified_name` - The node's qualified name (`Option<StringId>`)
    /// * `file` - The node's file (`FileId`)
    ///
    /// # Returns
    ///
    /// `true` if the node was added (new entry), `false` if it already existed.
    pub fn add(
        &mut self,
        id: NodeId,
        kind: NodeKind,
        name: StringId,
        qualified_name: Option<StringId>,
        file: FileId,
    ) -> bool {
        // Check if already present in kind index to detect duplicates
        let kind_ids = self.kind_index.entry(kind).or_default();
        if kind_ids.contains(&id) {
            // Already indexed - skip to prevent duplicates
            return false;
        }

        // Add to all indices
        kind_ids.push(id);
        self.name_index.entry(name).or_default().push(id);
        if let Some(qn) = qualified_name {
            self.qualified_name_index.entry(qn).or_default().push(id);
        }
        self.file_index.entry(file).or_default().push(id);
        self.node_count += 1;
        true
    }

    fn remove_id_from_index<K: Eq + Hash + Copy>(
        index: &mut HashMap<K, Vec<NodeId>>,
        key: K,
        id: NodeId,
    ) -> bool {
        let Some(ids) = index.get_mut(&key) else {
            return false;
        };

        let Some(pos) = ids.iter().position(|&x| x == id) else {
            return false;
        };

        ids.swap_remove(pos);
        if ids.is_empty() {
            index.remove(&key);
        }

        true
    }

    fn remove_id_from_all_buckets<K: Eq + Hash>(index: &mut HashMap<K, Vec<NodeId>>, id: NodeId) {
        for ids in index.values_mut() {
            if let Some(pos) = ids.iter().position(|&x| x == id) {
                ids.swap_remove(pos);
                break;
            }
        }
    }

    fn retain_non_empty<K: Eq + Hash>(index: &mut HashMap<K, Vec<NodeId>>) {
        index.retain(|_, v| !v.is_empty());
    }

    /// Removes a node from all indices.
    ///
    /// # Arguments
    ///
    /// * `id` - The node's ID
    /// * `kind` - The node's kind
    /// * `name` - The node's name
    /// * `qualified_name` - The node's qualified name
    /// * `file` - The node's file
    ///
    /// # Returns
    ///
    /// `true` if the node was found and removed, `false` otherwise.
    pub fn remove(
        &mut self,
        id: NodeId,
        kind: NodeKind,
        name: StringId,
        qualified_name: Option<StringId>,
        file: FileId,
    ) -> bool {
        let removed = Self::remove_id_from_index(&mut self.kind_index, kind, id);

        Self::remove_id_from_index(&mut self.name_index, name, id);
        if let Some(qn) = qualified_name {
            Self::remove_id_from_index(&mut self.qualified_name_index, qn, id);
        }
        Self::remove_id_from_index(&mut self.file_index, file, id);

        if removed {
            self.node_count -= 1;
        }

        removed
    }

    /// Returns all nodes of the given kind.
    #[must_use]
    pub fn by_kind(&self, kind: NodeKind) -> &[NodeId] {
        self.kind_index.get(&kind).map_or(&[], |v| v.as_slice())
    }

    /// Returns all nodes with the given name.
    #[must_use]
    pub fn by_name(&self, name: StringId) -> &[NodeId] {
        self.name_index.get(&name).map_or(&[], |v| v.as_slice())
    }

    /// Returns all nodes with the given qualified name.
    #[must_use]
    pub fn by_qualified_name(&self, name: StringId) -> &[NodeId] {
        self.qualified_name_index
            .get(&name)
            .map_or(&[], |v| v.as_slice())
    }

    /// Returns all nodes in the given file.
    #[must_use]
    pub fn by_file(&self, file: FileId) -> &[NodeId] {
        self.file_index.get(&file).map_or(&[], |v| v.as_slice())
    }

    /// Returns the number of distinct node kinds indexed.
    #[must_use]
    pub fn kind_count(&self) -> usize {
        self.kind_index.len()
    }

    /// Returns the number of distinct names indexed.
    #[must_use]
    pub fn name_count(&self) -> usize {
        self.name_index.len()
    }

    /// Returns the number of distinct files indexed.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.file_index.len()
    }

    /// Returns true if any nodes of the given kind exist.
    #[must_use]
    pub fn has_kind(&self, kind: NodeKind) -> bool {
        self.kind_index.contains_key(&kind)
    }

    /// Returns true if any nodes with the given name exist.
    #[must_use]
    pub fn has_name(&self, name: StringId) -> bool {
        self.name_index.contains_key(&name)
    }

    /// Returns true if any nodes in the given file exist.
    #[must_use]
    pub fn has_file(&self, file: FileId) -> bool {
        self.file_index.contains_key(&file)
    }

    /// Iterates over all kinds with their node counts.
    pub fn iter_kinds(&self) -> impl Iterator<Item = (NodeKind, usize)> + '_ {
        self.kind_index.iter().map(|(&k, v)| (k, v.len()))
    }

    /// Iterates over all names with their node counts.
    pub fn iter_names(&self) -> impl Iterator<Item = (StringId, usize)> + '_ {
        self.name_index.iter().map(|(&n, v)| (n, v.len()))
    }

    /// Iterates over all files with their node counts.
    pub fn iter_files(&self) -> impl Iterator<Item = (FileId, usize)> + '_ {
        self.file_index.iter().map(|(&f, v)| (f, v.len()))
    }

    /// Removes all nodes in the given file from all indices using full node metadata.
    ///
    /// This is the preferred method for file removal during incremental updates.
    /// Each node requires O(1) hash lookups + O(bucket) linear search within each index,
    /// making it O(N * B) where N is nodes removed and B is average bucket size.
    ///
    /// # Arguments
    ///
    /// * `file` - The file being removed
    /// * `nodes` - Iterator of (`NodeId`, `NodeKind`, `StringId`, `Option<StringId>`) for nodes in the file
    ///
    /// # Returns
    ///
    /// The number of nodes successfully removed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // During incremental re-indexing, caller has node metadata from arena
    /// let node_info = nodes_in_file.iter()
    ///     .map(|&id| (id, arena.get_kind(id), arena.get_name(id), arena.get_qualified_name(id)));
    /// indices.remove_file_with_info(file_id, node_info);
    /// ```
    pub fn remove_file_with_info(
        &mut self,
        file: FileId,
        nodes: impl IntoIterator<Item = (NodeId, NodeKind, StringId, Option<StringId>)>,
    ) -> usize {
        // Remove file from file_index first
        self.file_index.remove(&file);
        let removed_count = self.remove_nodes_with_info(nodes);

        self.node_count -= removed_count;
        removed_count
    }

    /// Removes all nodes in the given file from all indices.
    ///
    /// **Warning**: This method is O(F * (K + N)) where F is nodes in file,
    /// K is total kind index entries, and N is total name index entries.
    /// For better performance when node metadata is available, use
    /// [`remove_file_with_info`](Self::remove_file_with_info) instead.
    ///
    /// Returns the IDs of the removed nodes.
    #[deprecated(
        since = "0.1.0",
        note = "use remove_file_with_info for O(N) performance when node metadata is available"
    )]
    pub fn remove_file(&mut self, file: FileId) -> Vec<NodeId> {
        let node_ids = self.file_index.remove(&file).unwrap_or_default();
        let removed_count = node_ids.len();

        self.remove_nodes_without_info(&node_ids);

        self.node_count -= removed_count;
        node_ids
    }

    fn remove_nodes_with_info(
        &mut self,
        nodes: impl IntoIterator<Item = (NodeId, NodeKind, StringId, Option<StringId>)>,
    ) -> usize {
        let mut removed_count = 0;

        for (id, kind, name, qualified_name) in nodes {
            if Self::remove_id_from_index(&mut self.kind_index, kind, id) {
                removed_count += 1;
            }
            Self::remove_id_from_index(&mut self.name_index, name, id);
            if let Some(qn) = qualified_name {
                Self::remove_id_from_index(&mut self.qualified_name_index, qn, id);
            }
        }

        removed_count
    }

    fn remove_nodes_without_info(&mut self, node_ids: &[NodeId]) {
        // Remove from kind and name indices
        // Without knowing kind/name, we must search all entries - O(K + N) per node
        for &id in node_ids {
            Self::remove_id_from_all_buckets(&mut self.kind_index, id);
            Self::remove_id_from_all_buckets(&mut self.name_index, id);
            Self::remove_id_from_all_buckets(&mut self.qualified_name_index, id);
        }

        // Clean up empty entries
        Self::retain_non_empty(&mut self.kind_index);
        Self::retain_non_empty(&mut self.name_index);
        Self::retain_non_empty(&mut self.qualified_name_index);
    }

    /// Clears all indices.
    pub fn clear(&mut self) {
        self.kind_index.clear();
        self.name_index.clear();
        self.qualified_name_index.clear();
        self.file_index.clear();
        self.node_count = 0;
    }

    /// Reserves capacity for at least `additional` more nodes.
    pub fn reserve(&mut self, additional: usize) {
        self.name_index.reserve(additional);
        self.qualified_name_index.reserve(additional);
        self.file_index.reserve(additional / 50);
    }

    /// Returns statistics about the indices.
    #[must_use]
    pub fn stats(&self) -> IndicesStats {
        IndicesStats {
            node_count: self.node_count,
            kind_count: self.kind_index.len(),
            name_count: self.name_index.len(),
            qualified_name_count: self.qualified_name_index.len(),
            file_count: self.file_index.len(),
        }
    }
}

impl Default for AuxiliaryIndices {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AuxiliaryIndices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AuxiliaryIndices(nodes={}, kinds={}, names={}, qnames={}, files={})",
            self.node_count,
            self.kind_index.len(),
            self.name_index.len(),
            self.qualified_name_index.len(),
            self.file_index.len()
        )
    }
}

/// Statistics about `AuxiliaryIndices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndicesStats {
    /// Total number of indexed nodes.
    pub node_count: usize,
    /// Number of distinct node kinds.
    pub kind_count: usize,
    /// Number of distinct names.
    pub name_count: usize,
    /// Number of distinct qualified names.
    pub qualified_name_count: usize,
    /// Number of distinct files.
    pub file_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(index: u32) -> NodeId {
        NodeId::new(index, 1)
    }

    fn test_name(index: u32) -> StringId {
        StringId::new(index)
    }

    fn test_file(index: u32) -> FileId {
        FileId::new(index)
    }

    #[test]
    fn test_new() {
        let indices = AuxiliaryIndices::new();
        assert_eq!(indices.len(), 0);
        assert!(indices.is_empty());
    }

    #[test]
    fn test_with_capacity() {
        let indices = AuxiliaryIndices::with_capacity(1000);
        assert_eq!(indices.len(), 0);
    }

    #[test]
    fn test_add_single() {
        let mut indices = AuxiliaryIndices::new();
        let id = test_id(1);
        let name = test_name(1);
        let file = test_file(1);

        assert!(indices.add(id, NodeKind::Function, name, None, file));

        assert_eq!(indices.len(), 1);
        assert_eq!(indices.by_kind(NodeKind::Function), &[id]);
        assert_eq!(indices.by_name(name), &[id]);
        assert_eq!(indices.by_file(file), &[id]);
    }

    #[test]
    fn test_add_duplicate_prevented() {
        let mut indices = AuxiliaryIndices::new();
        let id = test_id(1);
        let name = test_name(1);
        let file = test_file(1);

        // First add should succeed
        assert!(indices.add(id, NodeKind::Function, name, None, file));
        assert_eq!(indices.len(), 1);

        // Second add of same node should be rejected (set semantics)
        assert!(!indices.add(id, NodeKind::Function, name, None, file));
        assert_eq!(indices.len(), 1); // Count unchanged

        // Indices should still have exactly one entry
        assert_eq!(indices.by_kind(NodeKind::Function).len(), 1);
        assert_eq!(indices.by_name(name).len(), 1);
        assert_eq!(indices.by_file(file).len(), 1);
    }

    #[test]
    fn test_add_multiple_same_kind() {
        let mut indices = AuxiliaryIndices::new();
        let id1 = test_id(1);
        let id2 = test_id(2);
        let id3 = test_id(3);

        indices.add(id1, NodeKind::Function, test_name(1), None, test_file(1));
        indices.add(id2, NodeKind::Function, test_name(2), None, test_file(1));
        indices.add(id3, NodeKind::Function, test_name(3), None, test_file(2));

        assert_eq!(indices.len(), 3);
        let by_kind = indices.by_kind(NodeKind::Function);
        assert_eq!(by_kind.len(), 3);
        assert!(by_kind.contains(&id1));
        assert!(by_kind.contains(&id2));
        assert!(by_kind.contains(&id3));
    }

    #[test]
    fn test_add_multiple_same_name() {
        let mut indices = AuxiliaryIndices::new();
        let id1 = test_id(1);
        let id2 = test_id(2);
        let shared_name = test_name(100);

        indices.add(id1, NodeKind::Function, shared_name, None, test_file(1));
        indices.add(id2, NodeKind::Variable, shared_name, None, test_file(2));

        let by_name = indices.by_name(shared_name);
        assert_eq!(by_name.len(), 2);
        assert!(by_name.contains(&id1));
        assert!(by_name.contains(&id2));
    }

    #[test]
    fn test_add_multiple_same_file() {
        let mut indices = AuxiliaryIndices::new();
        let id1 = test_id(1);
        let id2 = test_id(2);
        let id3 = test_id(3);
        let shared_file = test_file(100);

        indices.add(id1, NodeKind::Function, test_name(1), None, shared_file);
        indices.add(id2, NodeKind::Class, test_name(2), None, shared_file);
        indices.add(id3, NodeKind::Method, test_name(3), None, shared_file);

        let by_file = indices.by_file(shared_file);
        assert_eq!(by_file.len(), 3);
        assert!(by_file.contains(&id1));
        assert!(by_file.contains(&id2));
        assert!(by_file.contains(&id3));
    }

    #[test]
    fn test_remove() {
        let mut indices = AuxiliaryIndices::new();
        let id = test_id(1);
        let name = test_name(1);
        let file = test_file(1);

        indices.add(id, NodeKind::Function, name, None, file);
        assert_eq!(indices.len(), 1);

        let removed = indices.remove(id, NodeKind::Function, name, None, file);
        assert!(removed);
        assert_eq!(indices.len(), 0);
        assert!(indices.by_kind(NodeKind::Function).is_empty());
        assert!(indices.by_name(name).is_empty());
        assert!(indices.by_file(file).is_empty());
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut indices = AuxiliaryIndices::new();
        let id = test_id(1);
        let name = test_name(1);
        let file = test_file(1);

        let removed = indices.remove(id, NodeKind::Function, name, None, file);
        assert!(!removed);
    }

    #[test]
    fn test_remove_one_of_many() {
        let mut indices = AuxiliaryIndices::new();
        let id1 = test_id(1);
        let id2 = test_id(2);
        let name = test_name(1);
        let file = test_file(1);

        indices.add(id1, NodeKind::Function, name, None, file);
        indices.add(id2, NodeKind::Function, name, None, file);
        assert_eq!(indices.len(), 2);

        indices.remove(id1, NodeKind::Function, name, None, file);
        assert_eq!(indices.len(), 1);
        assert_eq!(indices.by_kind(NodeKind::Function), &[id2]);
    }

    #[test]
    fn test_by_kind_empty() {
        let indices = AuxiliaryIndices::new();
        assert!(indices.by_kind(NodeKind::Function).is_empty());
    }

    #[test]
    fn test_by_name_empty() {
        let indices = AuxiliaryIndices::new();
        assert!(indices.by_name(test_name(1)).is_empty());
    }

    #[test]
    fn test_by_file_empty() {
        let indices = AuxiliaryIndices::new();
        assert!(indices.by_file(test_file(1)).is_empty());
    }

    #[test]
    fn test_counts() {
        let mut indices = AuxiliaryIndices::new();

        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );
        indices.add(
            test_id(2),
            NodeKind::Class,
            test_name(2),
            None,
            test_file(1),
        );
        indices.add(
            test_id(3),
            NodeKind::Function,
            test_name(3),
            None,
            test_file(2),
        );

        assert_eq!(indices.kind_count(), 2); // Function, Class
        assert_eq!(indices.name_count(), 3); // 3 unique names
        assert_eq!(indices.file_count(), 2); // 2 unique files
    }

    #[test]
    fn test_has_methods() {
        let mut indices = AuxiliaryIndices::new();
        let name = test_name(1);
        let file = test_file(1);

        indices.add(test_id(1), NodeKind::Function, name, None, file);

        assert!(indices.has_kind(NodeKind::Function));
        assert!(!indices.has_kind(NodeKind::Class));
        assert!(indices.has_name(name));
        assert!(!indices.has_name(test_name(999)));
        assert!(indices.has_file(file));
        assert!(!indices.has_file(test_file(999)));
    }

    #[test]
    fn test_iter_kinds() {
        let mut indices = AuxiliaryIndices::new();

        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );
        indices.add(
            test_id(2),
            NodeKind::Function,
            test_name(2),
            None,
            test_file(1),
        );
        indices.add(
            test_id(3),
            NodeKind::Class,
            test_name(3),
            None,
            test_file(1),
        );

        let kinds: Vec<_> = indices.iter_kinds().collect();
        assert_eq!(kinds.len(), 2);

        let functions = kinds.iter().find(|(k, _)| *k == NodeKind::Function);
        assert_eq!(functions, Some(&(NodeKind::Function, 2)));

        let classes = kinds.iter().find(|(k, _)| *k == NodeKind::Class);
        assert_eq!(classes, Some(&(NodeKind::Class, 1)));
    }

    #[test]
    fn test_iter_files() {
        let mut indices = AuxiliaryIndices::new();
        let file1 = test_file(1);
        let file2 = test_file(2);

        indices.add(test_id(1), NodeKind::Function, test_name(1), None, file1);
        indices.add(test_id(2), NodeKind::Function, test_name(2), None, file1);
        indices.add(test_id(3), NodeKind::Class, test_name(3), None, file2);

        let file_entries: Vec<_> = indices.iter_files().collect();
        assert_eq!(file_entries.len(), 2);
    }

    #[test]
    fn test_remove_file_with_info() {
        let mut indices = AuxiliaryIndices::new();
        let file1 = test_file(1);
        let file2 = test_file(2);

        let id1 = test_id(1);
        let id2 = test_id(2);
        let id3 = test_id(3);
        let name1 = test_name(1);
        let name2 = test_name(2);
        let name3 = test_name(3);

        indices.add(id1, NodeKind::Function, name1, None, file1);
        indices.add(id2, NodeKind::Class, name2, None, file1);
        indices.add(id3, NodeKind::Method, name3, None, file2);

        assert_eq!(indices.len(), 3);

        // Remove file1 with full node metadata
        let nodes = vec![
            (id1, NodeKind::Function, name1, None),
            (id2, NodeKind::Class, name2, None),
        ];
        let removed_count = indices.remove_file_with_info(file1, nodes);
        assert_eq!(removed_count, 2);

        assert_eq!(indices.len(), 1);
        assert!(indices.by_file(file1).is_empty());
        assert_eq!(indices.by_file(file2), &[id3]);

        // Kind indices should also be cleaned up
        assert!(indices.by_kind(NodeKind::Function).is_empty());
        assert!(indices.by_kind(NodeKind::Class).is_empty());
        assert_eq!(indices.by_kind(NodeKind::Method), &[id3]);
    }

    #[test]
    fn test_remove_file_with_info_empty() {
        let mut indices = AuxiliaryIndices::new();
        let removed = indices.remove_file_with_info(test_file(1), std::iter::empty());
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_remove_file_with_info_partial_mismatch() {
        // Test behavior when some nodes don't match (e.g., already removed)
        let mut indices = AuxiliaryIndices::new();
        let file1 = test_file(1);
        let id1 = test_id(1);
        let name1 = test_name(1);

        indices.add(id1, NodeKind::Function, name1, None, file1);
        assert_eq!(indices.len(), 1);

        // Try to remove with wrong kind - should still remove from name/file indices
        let nodes = vec![
            (id1, NodeKind::Function, name1, None),
            (test_id(99), NodeKind::Class, test_name(99), None), // Non-existent node
        ];
        let removed = indices.remove_file_with_info(file1, nodes);
        assert_eq!(removed, 1); // Only one node was actually found

        assert_eq!(indices.len(), 0);
    }

    #[test]
    #[allow(deprecated)]
    fn test_remove_file_deprecated() {
        let mut indices = AuxiliaryIndices::new();
        let file1 = test_file(1);
        let file2 = test_file(2);

        let id1 = test_id(1);
        let id2 = test_id(2);
        let id3 = test_id(3);

        indices.add(id1, NodeKind::Function, test_name(1), None, file1);
        indices.add(id2, NodeKind::Class, test_name(2), None, file1);
        indices.add(id3, NodeKind::Method, test_name(3), None, file2);

        assert_eq!(indices.len(), 3);

        let removed = indices.remove_file(file1);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&id1));
        assert!(removed.contains(&id2));

        assert_eq!(indices.len(), 1);
        assert!(indices.by_file(file1).is_empty());
        assert_eq!(indices.by_file(file2), &[id3]);
    }

    #[test]
    #[allow(deprecated)]
    fn test_remove_file_empty_deprecated() {
        let mut indices = AuxiliaryIndices::new();
        let removed = indices.remove_file(test_file(1));
        assert!(removed.is_empty());
    }

    #[test]
    fn test_clear() {
        let mut indices = AuxiliaryIndices::new();

        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );
        indices.add(
            test_id(2),
            NodeKind::Class,
            test_name(2),
            None,
            test_file(2),
        );

        assert_eq!(indices.len(), 2);
        indices.clear();
        assert_eq!(indices.len(), 0);
        assert!(indices.is_empty());
    }

    #[test]
    fn test_reserve() {
        let mut indices = AuxiliaryIndices::new();
        indices.reserve(1000);
        // Should not panic or fail
    }

    #[test]
    fn test_display() {
        let mut indices = AuxiliaryIndices::new();
        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );

        let display = format!("{indices}");
        assert!(display.contains("AuxiliaryIndices"));
        assert!(display.contains("nodes=1"));
    }

    #[test]
    fn test_stats() {
        let mut indices = AuxiliaryIndices::new();

        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );
        indices.add(
            test_id(2),
            NodeKind::Class,
            test_name(2),
            None,
            test_file(1),
        );

        let stats = indices.stats();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.kind_count, 2);
        assert_eq!(stats.name_count, 2);
        assert_eq!(stats.file_count, 1);
    }

    #[test]
    fn test_default() {
        let indices: AuxiliaryIndices = AuxiliaryIndices::default();
        assert_eq!(indices.len(), 0);
    }

    #[test]
    fn test_clone() {
        let mut indices = AuxiliaryIndices::new();
        indices.add(
            test_id(1),
            NodeKind::Function,
            test_name(1),
            None,
            test_file(1),
        );

        let cloned = indices.clone();
        assert_eq!(cloned.len(), 1);
        assert_eq!(cloned.by_kind(NodeKind::Function).len(), 1);
    }
}
