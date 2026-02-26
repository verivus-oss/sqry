//! Identity index for composite key lookup.
//!
//! This module implements the `IdentityIndex` (FR-17) that provides
//! fast lookup of nodes by their identity key: (language, file, `qualified_name`).
//!
//! # Overview
//!
//! The `IdentityIndex` enables efficient deduplication and lookup of symbols
//! across incremental builds. When a file is re-indexed, symbols can be
//! matched to their existing nodes using this index.
//!
//! # Identity Key
//!
//! A symbol's identity is determined by:
//! - **Language**: The language plugin that extracted the symbol
//! - **File**: The file containing the symbol
//! - **Qualified Name**: The fully qualified name (e.g., `module::Class::method`)
//!
//! This tuple uniquely identifies a symbol across the codebase.
//!
//! # Thread Safety
//!
//! The index is designed for single-threaded build operations.
//! For concurrent access, wrap in appropriate synchronization primitives.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::string::StringId;

/// Identity key for symbol lookup.
///
/// A composite key that uniquely identifies a symbol across the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityKey {
    /// Language identifier (interned).
    pub language: StringId,
    /// File containing the symbol.
    pub file: FileId,
    /// Qualified name of the symbol (interned).
    pub qualified_name: StringId,
}

impl IdentityKey {
    /// Create a new identity key.
    #[must_use]
    pub fn new(language: StringId, file: FileId, qualified_name: StringId) -> Self {
        Self {
            language,
            file,
            qualified_name,
        }
    }
}

impl Hash for IdentityKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.language.hash(state);
        self.file.hash(state);
        self.qualified_name.hash(state);
    }
}

/// Index for looking up nodes by identity key.
///
/// Provides O(1) lookup of existing nodes during incremental builds.
#[derive(Debug, Default)]
pub struct IdentityIndex {
    /// Map from identity key to node ID.
    index: HashMap<IdentityKey, NodeId>,
    /// Reverse map: file → list of keys in that file.
    /// Used for efficient file removal.
    by_file: HashMap<FileId, Vec<IdentityKey>>,
    /// Reverse map: `node_id` → identity key.
    /// Used for cleanup when nodes are removed by ID.
    by_node: HashMap<NodeId, IdentityKey>,
}

impl IdentityIndex {
    /// Create a new empty identity index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            index: HashMap::with_capacity(capacity),
            by_file: HashMap::new(),
            by_node: HashMap::new(),
        }
    }

    /// Insert a node into the index.
    ///
    /// Returns the previous node ID if the key already existed.
    pub fn insert(&mut self, key: IdentityKey, node_id: NodeId) -> Option<NodeId> {
        let file = key.file;
        let old = self.index.insert(key.clone(), node_id);

        // Update reverse indexes
        let entry = self.by_file.entry(file).or_default();
        if !entry.contains(&key) {
            entry.push(key.clone());
        }

        if let Some(prev) = old {
            self.by_node.remove(&prev);
        }
        self.by_node.insert(node_id, key);

        old
    }

    /// Look up a node by identity key.
    #[must_use]
    pub fn get(&self, key: &IdentityKey) -> Option<NodeId> {
        self.index.get(key).copied()
    }

    /// Check if a key exists in the index.
    #[must_use]
    pub fn contains(&self, key: &IdentityKey) -> bool {
        self.index.contains_key(key)
    }

    /// Remove a node from the index.
    ///
    /// Returns the removed node ID if it existed.
    pub fn remove(&mut self, key: &IdentityKey) -> Option<NodeId> {
        let result = self.index.remove(key);

        if let Some(node_id) = result {
            self.by_node.remove(&node_id);
            if let Some(keys) = self.by_file.get_mut(&key.file) {
                keys.retain(|existing| existing != key);
                if keys.is_empty() {
                    self.by_file.remove(&key.file);
                }
            }
        }

        result
    }

    /// Remove all entries for a file.
    ///
    /// Returns the list of removed (key, `node_id`) pairs.
    pub fn remove_file(&mut self, file: FileId) -> Vec<(IdentityKey, NodeId)> {
        let keys = self.by_file.remove(&file).unwrap_or_default();
        let mut removed = Vec::with_capacity(keys.len());

        for key in keys {
            if let Some(node_id) = self.index.remove(&key) {
                self.by_node.remove(&node_id);
                removed.push((key, node_id));
            }
        }

        removed
    }

    /// Get the number of entries in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Check if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Get the number of files tracked.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Get all node IDs in a file.
    #[must_use]
    pub fn nodes_in_file(&self, file: FileId) -> Vec<NodeId> {
        self.by_file
            .get(&file)
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| self.index.get(key).copied())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = (&IdentityKey, &NodeId)> {
        self.index.iter()
    }

    /// Get all keys for a file.
    pub fn keys_in_file(&self, file: FileId) -> &[IdentityKey] {
        self.by_file.get(&file).map_or(&[], Vec::as_slice)
    }

    /// Clear the index.
    pub fn clear(&mut self) {
        self.index.clear();
        self.by_file.clear();
        self.by_node.clear();
    }

    /// Remove a node by its ID, cleaning up all reverse indexes.
    ///
    /// Returns the removed (key, `node_id`) pair if it existed.
    pub fn remove_node_id(&mut self, node_id: NodeId) -> Option<(IdentityKey, NodeId)> {
        let key = self.by_node.remove(&node_id)?;
        let removed = self.index.remove(&key);

        if let Some(keys) = self.by_file.get_mut(&key.file) {
            keys.retain(|existing| existing != &key);
            if keys.is_empty() {
                self.by_file.remove(&key.file);
            }
        }

        removed.map(|id| (key, id))
    }
}

/// Builder for creating identity keys with interning.
pub struct IdentityKeyBuilder<'a> {
    /// String interner reference.
    strings: &'a mut super::super::storage::StringInterner,
}

impl<'a> IdentityKeyBuilder<'a> {
    /// Create a new builder.
    pub fn new(strings: &'a mut super::super::storage::StringInterner) -> Self {
        Self { strings }
    }

    /// Build an identity key, interning strings as needed.
    ///
    /// # Errors
    ///
    /// Returns error if string interning fails (e.g., interner capacity exceeded).
    pub fn build(
        &mut self,
        language: &str,
        file: FileId,
        qualified_name: &str,
    ) -> Result<IdentityKey, super::super::storage::InternError> {
        Ok(IdentityKey {
            language: self.strings.intern(language)?,
            file,
            qualified_name: self.strings.intern(qualified_name)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::storage::StringInterner;

    fn make_key(lang: u32, file: u32, name: u32) -> IdentityKey {
        IdentityKey::new(StringId::new(lang), FileId::new(file), StringId::new(name))
    }

    #[test]
    fn test_identity_key_equality() {
        let key1 = make_key(1, 2, 3);
        let key2 = make_key(1, 2, 3);
        let key3 = make_key(1, 2, 4);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_identity_index_insert_and_get() {
        let mut index = IdentityIndex::new();
        let key = make_key(1, 2, 3);
        let node_id = NodeId::new(10, 1);

        assert!(index.insert(key.clone(), node_id).is_none());
        assert_eq!(index.get(&key), Some(node_id));
        assert!(index.contains(&key));
    }

    #[test]
    fn test_identity_index_replace() {
        let mut index = IdentityIndex::new();
        let key = make_key(1, 2, 3);
        let old_id = NodeId::new(10, 1);
        let new_id = NodeId::new(20, 2);

        index.insert(key.clone(), old_id);
        let replaced = index.insert(key.clone(), new_id);

        assert_eq!(replaced, Some(old_id));
        assert_eq!(index.get(&key), Some(new_id));
    }

    #[test]
    fn test_identity_index_remove() {
        let mut index = IdentityIndex::new();
        let key = make_key(1, 2, 3);
        let node_id = NodeId::new(10, 1);

        index.insert(key.clone(), node_id);
        let removed = index.remove(&key);

        assert_eq!(removed, Some(node_id));
        assert!(!index.contains(&key));
        assert!(!index.by_node.contains_key(&node_id));
    }

    #[test]
    fn test_identity_index_remove_file() {
        let mut index = IdentityIndex::new();
        let file_id = FileId::new(5);

        // Add multiple symbols from the same file
        let key1 = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        let key2 = IdentityKey::new(StringId::new(1), file_id, StringId::new(11));
        let key3 = IdentityKey::new(StringId::new(1), FileId::new(6), StringId::new(12));

        index.insert(key1.clone(), NodeId::new(1, 1));
        index.insert(key2.clone(), NodeId::new(2, 1));
        index.insert(key3.clone(), NodeId::new(3, 1));

        assert_eq!(index.len(), 3);

        // Remove file 5
        let removed = index.remove_file(file_id);

        assert_eq!(removed.len(), 2);
        assert_eq!(index.len(), 1);
        assert!(!index.contains(&key1));
        assert!(!index.contains(&key2));
        assert!(index.contains(&key3));
    }

    #[test]
    fn test_identity_index_nodes_in_file() {
        let mut index = IdentityIndex::new();
        let file_id = FileId::new(5);

        let key1 = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        let key2 = IdentityKey::new(StringId::new(1), file_id, StringId::new(11));
        let node1 = NodeId::new(1, 1);
        let node2 = NodeId::new(2, 1);

        index.insert(key1, node1);
        index.insert(key2, node2);

        let file_nodes = index.nodes_in_file(file_id);
        assert_eq!(file_nodes.len(), 2);
        assert!(file_nodes.contains(&node1));
        assert!(file_nodes.contains(&node2));
    }

    #[test]
    fn test_identity_key_builder() {
        let mut strings = StringInterner::new();
        let mut builder = IdentityKeyBuilder::new(&mut strings);

        let key = builder
            .build("rust", FileId::new(0), "my_module::MyClass")
            .expect("build should succeed");

        assert_eq!(key.file, FileId::new(0));
        // Verify strings were interned
        assert_eq!(strings.resolve(key.language).as_deref(), Some("rust"));
        assert_eq!(
            strings.resolve(key.qualified_name).as_deref(),
            Some("my_module::MyClass")
        );
    }

    #[test]
    fn test_identity_index_with_capacity() {
        let index = IdentityIndex::with_capacity(100);
        assert!(index.is_empty());
    }

    #[test]
    fn test_identity_index_clear() {
        let mut index = IdentityIndex::new();
        index.insert(make_key(1, 2, 3), NodeId::new(1, 1));
        index.insert(make_key(1, 2, 4), NodeId::new(2, 1));

        assert_eq!(index.len(), 2);

        index.clear();

        assert!(index.is_empty());
        assert_eq!(index.file_count(), 0);
        assert!(index.by_node.is_empty());
    }

    #[test]
    fn test_identity_index_remove_node_id() {
        let mut index = IdentityIndex::new();
        let key = make_key(1, 2, 3);
        let node_id = NodeId::new(10, 1);

        index.insert(key.clone(), node_id);

        let removed = index.remove_node_id(node_id);

        assert!(removed.is_some());
        assert!(!index.contains(&key));
        assert!(!index.by_node.contains_key(&node_id));
        assert!(!index.by_file.contains_key(&key.file));
    }
}
