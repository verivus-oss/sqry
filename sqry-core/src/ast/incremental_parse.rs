//! Incremental parsing support for tree-sitter.
//!
//! This module provides utilities for incremental AST parsing using tree-sitter's
//! `InputEdit` API. When a file changes, instead of re-parsing from scratch,
//! tree-sitter can reuse unchanged portions of the old parse tree.
//!
//! # Architecture
//!
//! - `InputEditCalculator`: Calculates byte/line/column edits between file versions
//! - `TreeCache`: LRU cache storing recently parsed trees for reuse
//! - `IncrementalParser`: High-level wrapper coordinating incremental parsing
//!
//! # Performance
//!
//! Incremental parsing typically provides 5-10x speedup for small edits compared
//! to full re-parsing, as tree-sitter reuses unchanged AST subtrees.

use lru::LruCache;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use tree_sitter::{InputEdit, Point, Tree};

/// Calculates `InputEdit` describing changes between two file versions.
///
/// The calculator finds the common prefix and suffix between old and new content,
/// then computes the byte offsets and line/column positions for the changed region.
///
/// # Algorithm
///
/// 1. Find longest common prefix → start of edit
/// 2. Find longest common suffix → end of edit
/// 3. Convert byte offsets to (line, column) via `byte_to_point()`
/// 4. Build `InputEdit` with old/new positions
///
/// # Example
///
/// ```ignore
/// let old = b"fn foo() {}\n";
/// let new = b"fn foo() { return 42; }\n";
/// let edit = InputEditCalculator::calculate(old, new);
/// // edit describes: inserted " return 42;" at position (0, 10)
/// ```
pub struct InputEditCalculator;

impl InputEditCalculator {
    /// Calculate the `InputEdit` between old and new file content.
    ///
    /// Returns an `InputEdit` that can be passed to `tree.edit()` to inform
    /// tree-sitter about the changes, enabling incremental re-parsing.
    ///
    /// # Edge Cases
    ///
    /// - Empty old → full insert
    /// - Empty new → full delete
    /// - Identical content → zero-width edit at start
    #[must_use]
    pub fn calculate(old_content: &[u8], new_content: &[u8]) -> InputEdit {
        // Handle empty cases
        if old_content.is_empty() && new_content.is_empty() {
            return InputEdit {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Point { row: 0, column: 0 },
                old_end_position: Point { row: 0, column: 0 },
                new_end_position: Point { row: 0, column: 0 },
            };
        }

        if old_content.is_empty() {
            // Full insert
            let new_end_position = Self::byte_to_point(new_content, new_content.len());
            return InputEdit {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: new_content.len(),
                start_position: Point { row: 0, column: 0 },
                old_end_position: Point { row: 0, column: 0 },
                new_end_position,
            };
        }

        if new_content.is_empty() {
            // Full delete
            let old_end_position = Self::byte_to_point(old_content, old_content.len());
            return InputEdit {
                start_byte: 0,
                old_end_byte: old_content.len(),
                new_end_byte: 0,
                start_position: Point { row: 0, column: 0 },
                old_end_position,
                new_end_position: Point { row: 0, column: 0 },
            };
        }

        // Find common prefix
        let prefix_len = Self::find_common_prefix(old_content, new_content);

        // Find common suffix (in the remaining content after prefix)
        let old_remaining = &old_content[prefix_len..];
        let new_remaining = &new_content[prefix_len..];
        let suffix_len = Self::find_common_suffix(old_remaining, new_remaining);

        // Calculate byte positions
        let start_byte = prefix_len;
        let old_end_byte = prefix_len + old_remaining.len() - suffix_len;
        let new_end_byte = prefix_len + new_remaining.len() - suffix_len;

        // Convert to (line, column) positions
        let start_position = Self::byte_to_point(old_content, start_byte);
        let old_end_position = Self::byte_to_point(old_content, old_end_byte);
        let new_end_position = Self::byte_to_point(new_content, new_end_byte);

        InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        }
    }

    /// Find the length of the common prefix between two byte slices.
    fn find_common_prefix(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
    }

    /// Find the length of the common suffix between two byte slices.
    fn find_common_suffix(a: &[u8], b: &[u8]) -> usize {
        a.iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| x == y)
            .count()
    }

    /// Convert a byte offset to a (line, column) position.
    ///
    /// Lines are 0-indexed. Columns count UTF-8 bytes (not characters).
    /// Newlines are '\n' (LF) - '\r' is treated as regular character.
    fn byte_to_point(content: &[u8], offset: usize) -> Point {
        let mut row = 0;
        let mut column = 0;

        for (i, &byte) in content.iter().enumerate() {
            if i >= offset {
                break;
            }
            if byte == b'\n' {
                row += 1;
                column = 0;
            } else {
                column += 1;
            }
        }

        Point { row, column }
    }
}

/// Cached parse tree with metadata.
///
/// Stores a tree-sitter `Tree` along with the content hash it was parsed from.
/// This allows validating cache hits and detecting when re-parsing is needed.
#[derive(Debug)]
struct CachedTree {
    /// The parsed tree
    tree: Tree,
    /// Hash of the content this tree was parsed from
    hash: u64,
}

impl CachedTree {
    fn new(tree: Tree, hash: u64) -> Self {
        Self { tree, hash }
    }
}

/// LRU cache for parsed trees to enable incremental parsing.
///
/// Stores recently parsed trees so that when a file changes, we can:
/// 1. Retrieve the old tree from cache
/// 2. Calculate the `InputEdit` describing the change
/// 3. Call `tree.edit(InputEdit)` to inform tree-sitter
/// 4. Re-parse with the edited tree (tree-sitter reuses unchanged subtrees)
///
/// # Capacity
///
/// Default capacity is 100 trees. Assuming ~10-50KB per tree, this uses ~1-5MB memory.
/// Oldest trees are evicted when capacity is exceeded (LRU policy).
///
/// # Thread Safety
///
/// The cache is wrapped in a `Mutex` for thread-safe access. Lock contention should
/// be minimal as operations are fast (hash lookups, no disk I/O).
///
/// # Example
///
/// ```ignore
/// let cache = TreeCache::with_default_capacity();
/// cache.insert(&path, tree, hash);
/// if let Some((old_tree, old_hash)) = cache.get(&path) {
///     // Use old_tree for incremental parsing
/// }
/// ```
pub struct TreeCache {
    cache: Mutex<LruCache<PathBuf, CachedTree>>,
}

impl TreeCache {
    /// Default cache capacity (number of trees).
    pub const DEFAULT_CAPACITY: usize = 100;

    /// Create a new tree cache with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of trees to cache (must be > 0)
    ///
    /// # Panics
    ///
    /// Panics if capacity is 0.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity).expect("capacity must be > 0");
        Self {
            cache: Mutex::new(LruCache::new(capacity)),
        }
    }

    fn lock_cache(&self) -> MutexGuard<'_, LruCache<PathBuf, CachedTree>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Create a new tree cache with default capacity (100 trees).
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }

    /// Insert a tree into the cache.
    ///
    /// # Arguments
    ///
    /// * `path` - Absolute path to the file
    /// * `tree` - The parsed tree
    /// * `hash` - Hash of the content the tree was parsed from
    ///
    /// If the cache is at capacity, the least recently used tree is evicted.
    pub fn insert(&self, path: &Path, tree: Tree, hash: u64) {
        let mut cache = self.lock_cache();
        cache.put(path.to_path_buf(), CachedTree::new(tree, hash));
    }

    /// Retrieve a tree from the cache.
    ///
    /// # Arguments
    ///
    /// * `path` - Absolute path to the file
    ///
    /// # Returns
    ///
    /// `Some((tree, hash))` if the tree is cached, `None` otherwise.
    /// The returned tree is cloned (tree-sitter trees are cheap to clone).
    ///
    /// # Note
    ///
    /// Accessing a tree marks it as recently used (LRU update).
    pub fn get(&self, path: &Path) -> Option<(Tree, u64)> {
        let mut cache = self.lock_cache();
        cache
            .get(path)
            .map(|cached| (cached.tree.clone(), cached.hash))
    }

    /// Remove a tree from the cache.
    ///
    /// # Arguments
    ///
    /// * `path` - Absolute path to the file
    ///
    /// # Returns
    ///
    /// `true` if the tree was present and removed, `false` if not found.
    pub fn remove(&self, path: &Path) -> bool {
        let mut cache = self.lock_cache();
        cache.pop(path).is_some()
    }

    /// Clear all trees from the cache.
    pub fn clear(&self) {
        let mut cache = self.lock_cache();
        cache.clear();
    }

    /// Get the current number of cached trees.
    pub fn len(&self) -> usize {
        let cache = self.lock_cache();
        cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// High-level incremental parser coordinating tree caching and re-parsing.
///
/// Combines `TreeCache`, `InputEditCalculator`, and tree-sitter's incremental
/// parsing to minimize re-parsing overhead when files change.
///
/// # Workflow
///
/// 1. Check if old tree is cached for the file
/// 2. If cached and content changed:
///    - Calculate `InputEdit` describing changes
///    - Call `tree.edit(InputEdit)` to inform tree-sitter
///    - Re-parse with the edited tree (tree-sitter reuses unchanged subtrees)
/// 3. If not cached or edit fails:
///    - Fall back to full parse
/// 4. Cache the resulting tree for future incremental parses
///
/// # Performance
///
/// - **Cache hit + small edit**: 5-10x faster than full parse
/// - **Cache miss**: Same as full parse (no overhead)
/// - **Incremental parse failure**: Automatic fallback to full parse
///
/// # Thread Safety
///
/// The internal `TreeCache` uses a `Mutex`, so `IncrementalParser` is `Send + Sync`.
/// Multiple threads can share the same parser instance safely.
///
/// # Example
///
/// ```ignore
/// use sqry_core::ast::IncrementalParser;
///
/// let parser = IncrementalParser::with_default_capacity();
///
/// // First parse (cache miss)
/// let tree1 = parser.parse(&plugin, &path, &content1, None)?;
///
/// // Second parse after edit (cache hit, incremental)
/// let tree2 = parser.parse(&plugin, &path, &content2, Some(&content1))?;
/// // ↑ 5-10x faster than full re-parse
/// ```
pub struct IncrementalParser {
    cache: TreeCache,
}

impl IncrementalParser {
    /// Create a new incremental parser with the specified cache capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of trees to cache (must be > 0)
    ///
    /// # Panics
    ///
    /// Panics if capacity is 0.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: TreeCache::new(capacity),
        }
    }

    /// Create a new incremental parser with default cache capacity (100 trees).
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self {
            cache: TreeCache::with_default_capacity(),
        }
    }

    /// Parse content using incremental parsing when possible.
    ///
    /// # Arguments
    ///
    /// * `plugin` - Language plugin providing `parse_ast()` method
    /// * `path` - Absolute path to the file (used as cache key)
    /// * `new_content` - Current file content
    /// * `old_content` - Previous file content (optional, for `InputEdit` calculation)
    ///
    /// # Returns
    ///
    /// Parsed tree-sitter `Tree`. The tree is also cached for future incremental parses.
    ///
    /// # Incremental Parsing
    ///
    /// Incremental parsing is used when:
    /// 1. Old tree is cached for this path
    /// 2. `old_content` is provided
    /// 3. Content actually changed (different hash)
    ///
    /// Otherwise falls back to full parse (no performance penalty).
    ///
    /// # Errors
    ///
    /// Returns error if parsing fails (both incremental and fallback).
    pub fn parse<P>(
        &self,
        plugin: &P,
        path: &Path,
        new_content: &[u8],
        old_content: Option<&[u8]>,
    ) -> Result<Tree, crate::plugin::error::ParseError>
    where
        P: crate::plugin::LanguagePlugin + ?Sized,
    {
        // Calculate hash of new content
        let new_hash = Self::hash_content(new_content);

        // Try incremental parse if we have old tree and old content
        if let (Some(old_content), Some((old_tree, old_hash))) = (old_content, self.cache.get(path))
        {
            // Only do incremental parse if content actually changed
            if new_hash == old_hash {
                // Content unchanged - return cached tree
                return Ok(old_tree);
            }
            match Self::parse_incremental(plugin, &old_tree, old_content, new_content) {
                Ok(tree) => {
                    // Incremental parse succeeded - cache and return
                    self.cache.insert(path, tree.clone(), new_hash);
                    return Ok(tree);
                }
                Err(_) => {
                    // Incremental parse failed - fall through to full parse
                    log::debug!(
                        "Incremental parse failed for {}, falling back to full parse",
                        path.display()
                    );
                }
            }
        }

        // Fall back to full parse (cache miss or incremental failed)
        let tree = plugin.parse_ast(new_content)?;
        self.cache.insert(path, tree.clone(), new_hash);
        Ok(tree)
    }

    /// Perform incremental parse using tree-sitter's `InputEdit` API.
    ///
    /// # Arguments
    ///
    /// * `plugin` - Language plugin providing `parse_ast()` method
    /// * `old_tree` - Previously parsed tree
    /// * `old_content` - Previous file content
    /// * `new_content` - Current file content
    ///
    /// # Returns
    ///
    /// New tree if incremental parse succeeds, error otherwise.
    ///
    /// # Algorithm
    ///
    /// 1. Calculate `InputEdit` describing changes between old/new content
    /// 2. Clone old tree and call `tree.edit(InputEdit)`
    /// 3. Re-parse with edited tree as hint to tree-sitter
    /// 4. Tree-sitter reuses unchanged AST subtrees internally
    fn parse_incremental<P>(
        plugin: &P,
        old_tree: &Tree,
        old_content: &[u8],
        new_content: &[u8],
    ) -> Result<Tree, crate::plugin::error::ParseError>
    where
        P: crate::plugin::LanguagePlugin + ?Sized,
    {
        // Calculate edit describing changes
        let edit = InputEditCalculator::calculate(old_content, new_content);

        // Clone tree and apply edit
        let mut edited_tree = old_tree.clone();
        edited_tree.edit(&edit);

        // Re-parse with edited tree as hint
        // Note: We need to use tree-sitter Parser directly here
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&plugin.language())
            .map_err(|e| crate::plugin::error::ParseError::LanguageSetFailed(e.to_string()))?;

        parser
            .parse(new_content, Some(&edited_tree))
            .ok_or(crate::plugin::error::ParseError::TreeSitterFailed)
    }

    /// Calculate `XXHash3` hash of content.
    fn hash_content(content: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    /// Clear the tree cache.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    /// Get the number of cached trees.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_single_line_edit() {
        let old = b"fn foo() {}";
        let new = b"fn bar() {}";
        let edit = InputEditCalculator::calculate(old, new);

        assert_eq!(edit.start_byte, 3); // After "fn "
        assert_eq!(edit.old_end_byte, 6); // After "foo"
        assert_eq!(edit.new_end_byte, 6); // After "bar"
        assert_eq!(edit.start_position, Point { row: 0, column: 3 });
        assert_eq!(edit.old_end_position, Point { row: 0, column: 6 });
        assert_eq!(edit.new_end_position, Point { row: 0, column: 6 });
    }

    #[test]
    fn test_calculate_multiline_insert() {
        let old = b"line1\nline3\n";
        let new = b"line1\nline2\nline3\n";
        let edit = InputEditCalculator::calculate(old, new);

        // Common prefix: "line1\nline" (10 bytes) - stops at '3' vs '2'
        // Remaining: old="3\n" (2), new="2\nline3\n" (8), suffix="3\n" reversed (2)
        assert_eq!(edit.start_byte, 10); // After "line1\nline"
        assert_eq!(edit.old_end_byte, 10); // Zero-width in old (entire "3\n" is suffix)
        assert_eq!(edit.new_end_byte, 16); // After "line1\nline2\nline"
        assert_eq!(edit.start_position, Point { row: 1, column: 4 });
        assert_eq!(edit.old_end_position, Point { row: 1, column: 4 });
        assert_eq!(edit.new_end_position, Point { row: 2, column: 4 });
    }

    #[test]
    fn test_calculate_multiline_delete() {
        let old = b"line1\nline2\nline3\n";
        let new = b"line1\nline3\n";
        let edit = InputEditCalculator::calculate(old, new);

        // Common prefix: "line1\nline" (10 bytes) - stops at '2' vs '3'
        // Remaining: old="2\nline3\n" (8), new="3\n" (2), suffix="3\n" reversed (2)
        assert_eq!(edit.start_byte, 10); // After "line1\nline"
        assert_eq!(edit.old_end_byte, 16); // After "line1\nline2\nline"
        assert_eq!(edit.new_end_byte, 10); // Zero-width in new (entire "3\n" is suffix)
        assert_eq!(edit.start_position, Point { row: 1, column: 4 });
        assert_eq!(edit.old_end_position, Point { row: 2, column: 4 });
        assert_eq!(edit.new_end_position, Point { row: 1, column: 4 });
    }

    #[test]
    fn test_calculate_empty_to_content() {
        let old = b"";
        let new = b"hello\nworld\n";
        let edit = InputEditCalculator::calculate(old, new);

        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, 0);
        assert_eq!(edit.new_end_byte, 12);
        assert_eq!(edit.start_position, Point { row: 0, column: 0 });
        assert_eq!(edit.old_end_position, Point { row: 0, column: 0 });
        assert_eq!(edit.new_end_position, Point { row: 2, column: 0 });
    }

    #[test]
    fn test_calculate_content_to_empty() {
        let old = b"hello\nworld\n";
        let new = b"";
        let edit = InputEditCalculator::calculate(old, new);

        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, 12);
        assert_eq!(edit.new_end_byte, 0);
        assert_eq!(edit.start_position, Point { row: 0, column: 0 });
        assert_eq!(edit.old_end_position, Point { row: 2, column: 0 });
        assert_eq!(edit.new_end_position, Point { row: 0, column: 0 });
    }

    #[test]
    fn test_calculate_no_change() {
        let old = b"unchanged";
        let new = b"unchanged";
        let edit = InputEditCalculator::calculate(old, new);

        assert_eq!(edit.start_byte, 9); // End of content
        assert_eq!(edit.old_end_byte, 9);
        assert_eq!(edit.new_end_byte, 9);
        assert_eq!(edit.start_position, Point { row: 0, column: 9 });
        assert_eq!(edit.old_end_position, Point { row: 0, column: 9 });
        assert_eq!(edit.new_end_position, Point { row: 0, column: 9 });
    }

    #[test]
    fn test_byte_to_point_multiline() {
        let content = b"line1\nline2\nline3\n";

        assert_eq!(
            InputEditCalculator::byte_to_point(content, 0),
            Point { row: 0, column: 0 }
        );
        assert_eq!(
            InputEditCalculator::byte_to_point(content, 5),
            Point { row: 0, column: 5 }
        );
        assert_eq!(
            InputEditCalculator::byte_to_point(content, 6),
            Point { row: 1, column: 0 }
        );
        assert_eq!(
            InputEditCalculator::byte_to_point(content, 12),
            Point { row: 2, column: 0 }
        );
    }

    #[test]
    fn test_find_common_prefix() {
        assert_eq!(InputEditCalculator::find_common_prefix(b"abc", b"abx"), 2);
        assert_eq!(InputEditCalculator::find_common_prefix(b"abc", b"xyz"), 0);
        assert_eq!(InputEditCalculator::find_common_prefix(b"abc", b"abc"), 3);
        assert_eq!(InputEditCalculator::find_common_prefix(b"", b"abc"), 0);
    }

    #[test]
    fn test_find_common_suffix() {
        assert_eq!(InputEditCalculator::find_common_suffix(b"abc", b"xbc"), 2);
        assert_eq!(InputEditCalculator::find_common_suffix(b"abc", b"xyz"), 0);
        assert_eq!(InputEditCalculator::find_common_suffix(b"abc", b"abc"), 3);
        assert_eq!(InputEditCalculator::find_common_suffix(b"", b"abc"), 0);
    }

    // TreeCache tests

    fn create_dummy_tree() -> Tree {
        use tree_sitter::Parser;
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("set language");
        parser.parse("fn main() {}", None).expect("parse")
    }

    #[test]
    fn test_tree_cache_insert_and_get() {
        let cache = TreeCache::with_default_capacity();
        let path = PathBuf::from("/test/file.rs");
        let tree = create_dummy_tree();
        let hash = 0x1234_5678_90ab_cdef;

        // Insert tree
        cache.insert(&path, tree, hash);
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());

        // Retrieve tree
        let result = cache.get(&path);
        assert!(result.is_some());
        let (cached_tree, cached_hash) = result.unwrap();
        assert_eq!(cached_hash, hash);
        // Tree should be valid (has a root node)
        assert!(!cached_tree.root_node().kind().is_empty());
    }

    #[test]
    fn test_tree_cache_miss() {
        let cache = TreeCache::with_default_capacity();
        let path = PathBuf::from("/test/file.rs");

        // Cache miss
        assert!(cache.get(&path).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn test_tree_cache_remove() {
        let cache = TreeCache::with_default_capacity();
        let path = PathBuf::from("/test/file.rs");
        let tree = create_dummy_tree();

        cache.insert(&path, tree, 0x123);
        assert_eq!(cache.len(), 1);

        // Remove
        assert!(cache.remove(&path));
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&path).is_none());

        // Remove non-existent
        assert!(!cache.remove(&path));
    }

    #[test]
    fn test_tree_cache_clear() {
        let cache = TreeCache::with_default_capacity();
        let path1 = PathBuf::from("/test/file1.rs");
        let path2 = PathBuf::from("/test/file2.rs");

        cache.insert(&path1, create_dummy_tree(), 0x123);
        cache.insert(&path2, create_dummy_tree(), 0x456);
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        assert!(cache.get(&path1).is_none());
        assert!(cache.get(&path2).is_none());
    }

    #[test]
    fn test_tree_cache_lru_eviction() {
        let cache = TreeCache::new(2); // Small cache
        let path1 = PathBuf::from("/test/file1.rs");
        let path2 = PathBuf::from("/test/file2.rs");
        let path3 = PathBuf::from("/test/file3.rs");

        // Fill cache
        cache.insert(&path1, create_dummy_tree(), 0x111);
        cache.insert(&path2, create_dummy_tree(), 0x222);
        assert_eq!(cache.len(), 2);

        // Insert 3rd tree - should evict path1 (LRU)
        cache.insert(&path3, create_dummy_tree(), 0x333);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&path1).is_none()); // Evicted
        assert!(cache.get(&path2).is_some()); // Still present
        assert!(cache.get(&path3).is_some()); // Newly added
    }

    #[test]
    fn test_tree_cache_lru_access_updates() {
        let cache = TreeCache::new(2); // Small cache
        let path1 = PathBuf::from("/test/file1.rs");
        let path2 = PathBuf::from("/test/file2.rs");
        let path3 = PathBuf::from("/test/file3.rs");

        // Fill cache
        cache.insert(&path1, create_dummy_tree(), 0x111);
        cache.insert(&path2, create_dummy_tree(), 0x222);

        // Access path1 to mark it as recently used
        assert!(cache.get(&path1).is_some());

        // Insert path3 - should evict path2 (now LRU), not path1
        cache.insert(&path3, create_dummy_tree(), 0x333);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&path1).is_some()); // Still present (was accessed)
        assert!(cache.get(&path2).is_none()); // Evicted (LRU)
        assert!(cache.get(&path3).is_some()); // Newly added
    }

    #[test]
    fn test_tree_cache_thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(TreeCache::with_default_capacity());
        let mut handles = vec![];

        // Spawn multiple threads inserting trees
        for i in 0_u64..10 {
            let cache_clone = Arc::clone(&cache);
            let handle = thread::spawn(move || {
                let path = PathBuf::from(format!("/test/file{i}.rs"));
                cache_clone.insert(&path, create_dummy_tree(), i);
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all trees inserted
        assert_eq!(cache.len(), 10);
    }

    // IncrementalParser tests

    // Simple mock plugin for testing
    struct MockPlugin;

    impl crate::plugin::LanguagePlugin for MockPlugin {
        fn metadata(&self) -> crate::plugin::LanguageMetadata {
            crate::plugin::LanguageMetadata {
                id: "mock",
                name: "Mock",
                version: "1.0.0",
                author: "test",
                description: "Mock plugin for testing",
                tree_sitter_version: "0.24",
            }
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["mock"]
        }

        fn language(&self) -> tree_sitter::Language {
            tree_sitter_rust::LANGUAGE.into()
        }

        fn parse_ast(&self, content: &[u8]) -> Result<Tree, crate::plugin::error::ParseError> {
            use tree_sitter::Parser;
            let mut parser = Parser::new();
            parser
                .set_language(&self.language())
                .map_err(|e| crate::plugin::error::ParseError::LanguageSetFailed(e.to_string()))?;
            parser
                .parse(content, None)
                .ok_or(crate::plugin::error::ParseError::TreeSitterFailed)
        }

        fn extract_scopes(
            &self,
            _tree: &Tree,
            _content: &[u8],
            _file_path: &Path,
        ) -> Result<Vec<crate::ast::Scope>, crate::plugin::error::ScopeError> {
            Ok(vec![])
        }
    }

    #[test]
    fn test_incremental_parser_cache_miss() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");
        let content = b"fn foo() {}";

        // First parse (cache miss)
        let tree = parser.parse(&plugin, &path, content, None).unwrap();
        assert!(!tree.root_node().kind().is_empty());
        assert_eq!(parser.cache_len(), 1);
    }

    #[test]
    fn test_incremental_parser_cache_hit_unchanged() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");
        let content = b"fn foo() {}";

        // First parse
        let tree1 = parser.parse(&plugin, &path, content, None).unwrap();

        // Second parse with same content (cache hit, no change)
        let tree2 = parser
            .parse(&plugin, &path, content, Some(content))
            .unwrap();

        // Should return same tree (content unchanged)
        assert_eq!(tree1.root_node().kind(), tree2.root_node().kind());
        assert_eq!(parser.cache_len(), 1);
    }

    #[test]
    fn test_incremental_parser_incremental_parse() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");
        let old_content = b"fn foo() {}";
        let new_content = b"fn bar() {}";

        // First parse
        let tree1 = parser.parse(&plugin, &path, old_content, None).unwrap();
        assert_eq!(parser.cache_len(), 1);

        // Second parse after edit (incremental parse)
        let tree2 = parser
            .parse(&plugin, &path, new_content, Some(old_content))
            .unwrap();

        // Both trees should be valid
        assert!(!tree1.root_node().kind().is_empty());
        assert!(!tree2.root_node().kind().is_empty());
        // Cache should still have 1 entry (same path)
        assert_eq!(parser.cache_len(), 1);
    }

    #[test]
    fn test_incremental_parser_multiline_edit() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");

        let old_content = b"fn foo() {\n    println!(\"hello\");\n}";
        let new_content = b"fn foo() {\n    println!(\"world\");\n    println!(\"test\");\n}";

        // First parse
        parser.parse(&plugin, &path, old_content, None).unwrap();

        // Second parse with multiline edit (incremental)
        let tree = parser
            .parse(&plugin, &path, new_content, Some(old_content))
            .unwrap();

        // Tree should be valid
        assert!(!tree.root_node().kind().is_empty());
        assert_eq!(parser.cache_len(), 1);
    }

    #[test]
    fn test_incremental_parser_clear_cache() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path1 = PathBuf::from("/test/file1.rs");
        let path2 = PathBuf::from("/test/file2.rs");

        parser.parse(&plugin, &path1, b"fn foo() {}", None).unwrap();
        parser.parse(&plugin, &path2, b"fn bar() {}", None).unwrap();
        assert_eq!(parser.cache_len(), 2);

        parser.clear_cache();
        assert_eq!(parser.cache_len(), 0);
    }

    #[test]
    fn test_incremental_parser_different_files() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path1 = PathBuf::from("/test/file1.rs");
        let path2 = PathBuf::from("/test/file2.rs");

        // Parse two different files
        parser.parse(&plugin, &path1, b"fn foo() {}", None).unwrap();
        parser.parse(&plugin, &path2, b"fn bar() {}", None).unwrap();

        // Both should be cached
        assert_eq!(parser.cache_len(), 2);

        // Edit first file (should use incremental parse)
        parser
            .parse(&plugin, &path1, b"fn baz() {}", Some(b"fn foo() {}"))
            .unwrap();

        // Still 2 entries
        assert_eq!(parser.cache_len(), 2);
    }

    /// ACCEPTANCE TEST: Verify incremental parsing produces identical results to full parsing
    /// This is the core correctness guarantee of incremental parsing.
    #[test]
    fn test_acceptance_incremental_equals_full_parse() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");

        let original_content = b"fn foo() { let x = 1; }";
        let modified_content = b"fn foo() { let x = 2; let y = 3; }";

        // Full parse of original
        let tree1 = parser
            .parse(&plugin, &path, original_content, None)
            .unwrap();

        // Incremental parse (with old content)
        let tree2_incremental = parser
            .parse(&plugin, &path, modified_content, Some(original_content))
            .unwrap();

        // Full parse of modified (for comparison)
        parser.clear_cache(); // Clear cache to force full parse
        let tree2_full = parser
            .parse(&plugin, &path, modified_content, None)
            .unwrap();

        // CRITICAL: Incremental and full parse MUST produce identical trees
        assert_eq!(
            tree2_incremental.root_node().to_sexp(),
            tree2_full.root_node().to_sexp(),
            "Incremental parsing MUST produce identical AST to full parsing"
        );

        // Both should have valid trees
        assert!(!tree1.root_node().kind().is_empty());
        assert!(!tree2_incremental.root_node().kind().is_empty());
        assert!(!tree2_full.root_node().kind().is_empty());
    }

    /// ACCEPTANCE TEST: Verify fallback behavior when incremental parsing fails
    /// Tests that the system gracefully degrades to full parsing on errors.
    #[test]
    fn test_acceptance_fallback_on_incremental_failure() {
        let parser = IncrementalParser::with_default_capacity();
        let plugin = MockPlugin;
        let path = PathBuf::from("/test/file.rs");

        let original_content = b"fn foo() {}";
        let modified_content = b"fn bar() {}";

        // Parse original
        let tree1 = parser
            .parse(&plugin, &path, original_content, None)
            .unwrap();
        assert!(!tree1.root_node().kind().is_empty());

        // Provide corrupted "old content" that doesn't match cached tree
        // This should trigger fallback to full parse (instead of crashing)
        let corrupted_old_content = b"this is not the old content";

        let result = parser.parse(
            &plugin,
            &path,
            modified_content,
            Some(corrupted_old_content),
        );

        // Should succeed (via fallback to full parse)
        assert!(result.is_ok(), "Should fall back to full parse on mismatch");
        let tree2 = result.unwrap();
        assert!(!tree2.root_node().kind().is_empty());

        // Should still cache the result
        assert_eq!(parser.cache_len(), 1);
    }
}
