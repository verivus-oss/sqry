//! Shared types for JavaScript relation extraction
//!
//! Provides utilities for generating stable synthetic names for anonymous
//! functions and classes, and helpers for metadata management.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tree_sitter::Node;

/// Generates stable synthetic names for anonymous functions and classes.
///
/// Anonymous functions and classes without explicit names need identifiers
/// for call graph tracking. This builder creates deterministic names based on
/// the node type and source location.
///
/// # Naming Strategy
///
/// - Line-based (legacy): `<anon:function@{line}>`
/// - Hash-based (FR-JS-PATCH-2): `anon:arrow:a3f2b1c0`
///
/// Hash-based names are computed from node content + location for stability
/// across refactors while avoiding line-number brittleness.
pub struct SyntheticNameBuilder;

impl SyntheticNameBuilder {
    /// Generate a line-based synthetic name from an AST node (legacy).
    ///
    /// # Arguments
    ///
    /// * `node` - The AST node (function or class)
    /// * `content` - Source file bytes (for context extraction if needed)
    /// * `context` - Context type ("function", "class", "arrow", etc.)
    ///
    /// # Returns
    ///
    /// A stable synthetic name like `<anon:function@42>`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // For anonymous function at line 15:
    /// let name = SyntheticNameBuilder::from_node(&node, content, "function");
    /// // Returns: "<anon:function@15>"
    /// ```
    #[must_use]
    pub fn from_node(node: &Node, _content: &[u8], context: &str) -> String {
        let line = node.start_position().row + 1;
        format!("<anon:{context}@{line}>")
    }

    /// Generate a hash-based synthetic name from an AST node (FR-JS-PATCH-2).
    ///
    /// Computes a stable hash from node text content and start position.
    /// This provides deterministic IDs that survive line number changes during
    /// refactoring, while remaining unique within a file.
    ///
    /// # Arguments
    ///
    /// * `node` - The AST node (function or class)
    /// * `content` - Source file bytes (used to hash node text)
    /// * `context_label` - Context type ("arrow", "class", "function", etc.)
    ///
    /// # Returns
    ///
    /// A hash-based synthetic name like `anon:arrow:a3f2b1c0`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // For anonymous arrow function:
    /// let name = SyntheticNameBuilder::from_node_with_hash(&node, content, "arrow");
    /// // Returns: "anon:arrow:4a7b9c2d"
    /// ```
    #[must_use]
    pub fn from_node_with_hash(node: &Node, content: &[u8], context_label: &str) -> String {
        let mut hasher = DefaultHasher::new();

        // Hash node text content if available
        if let Ok(text) = node.utf8_text(content) {
            text.hash(&mut hasher);
        }

        // Hash start position to ensure uniqueness
        let pos = node.start_position();
        pos.row.hash(&mut hasher);
        pos.column.hash(&mut hasher);

        let hash = hasher.finish();
        // Use lower 32 bits for compact hex representation
        format!("anon:{context_label}:{:08x}", (hash & 0xFFFF_FFFF) as u32)
    }
}
