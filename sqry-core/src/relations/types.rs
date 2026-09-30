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

/// True when `segment` is a synthetic name minted for an unnamed construct.
///
/// Every form recognised here is derived from the construct's position in the
/// file that holds it (a line, a row/column pair, a byte offset, or a hash of
/// the text plus the position), or is a bare placeholder such as
/// `<anonymous>`. None of them identifies the construct outside its own file:
/// two unrelated files routinely mint the same one.
///
/// The forms in the tree are `<anon:{context}@{line}>` and
/// `anon:{label}:{hash}` from [`SyntheticNameBuilder`], plus the
/// `<anonymous>` / `<anonymous:{row}:{col}>` / `<anonymous@{byte}>`
/// placeholders used by the Elixir, Lua, R and Java plugins. `<` and `:` are
/// not legal in an identifier in any language sqry parses, so no real symbol
/// name can reach this predicate.
#[must_use]
pub fn is_synthetic_anonymous_segment(segment: &str) -> bool {
    segment.starts_with("<anon") || segment.starts_with("anon:")
}

/// True when any `::`-separated segment of `qualified_name` is a synthetic
/// anonymous name, per [`is_synthetic_anonymous_segment`].
///
/// A qualified name that carries such a segment is not a cross-file identity:
/// the segment was synthesised from a position, so the same string can name
/// unrelated constructs in different files. Callers that treat a qualified
/// name as a symbol's identity across the workspace must exclude these.
#[must_use]
pub fn has_synthetic_anonymous_segment(qualified_name: &str) -> bool {
    qualified_name
        .split("::")
        .any(is_synthetic_anonymous_segment)
}

#[cfg(test)]
mod tests {
    use super::{has_synthetic_anonymous_segment, is_synthetic_anonymous_segment};

    #[test]
    fn recognises_every_synthetic_form_the_tree_mints() {
        // `SyntheticNameBuilder::from_node`, used by the Rust and TypeScript
        // plugins.
        assert!(is_synthetic_anonymous_segment("<anon:closure@925>"));
        assert!(is_synthetic_anonymous_segment("<anon:arrow@42>"));
        assert!(is_synthetic_anonymous_segment("<anon:class@7>"));
        // `SyntheticNameBuilder::from_node_with_hash`, used by the JavaScript
        // plugin.
        assert!(is_synthetic_anonymous_segment("anon:arrow:a3f2b1c0"));
        // Placeholders minted by the Elixir, R, Lua and Java plugins.
        assert!(is_synthetic_anonymous_segment("<anonymous>"));
        assert!(is_synthetic_anonymous_segment("<anonymous:31:8>"));
        assert!(is_synthetic_anonymous_segment("<anonymous@2048>"));
    }

    #[test]
    fn leaves_real_symbol_names_alone() {
        for name in [
            "",
            "tests",
            "anonymize",
            "anonymous_user",
            "Anon",
            "write_hello_response",
            "WorkspaceManager",
            "make_anon:thing",
        ] {
            assert!(
                !is_synthetic_anonymous_segment(name),
                "{name} is a real name and must not read as synthetic"
            );
        }
    }

    #[test]
    fn finds_a_synthetic_segment_anywhere_in_a_qualified_name() {
        assert!(has_synthetic_anonymous_segment("tests::<anon:closure@925>"));
        assert!(has_synthetic_anonymous_segment(
            "tests::<anon:closure@925>::helper"
        ));
        assert!(has_synthetic_anonymous_segment("<anon:class@7>::method"));
        assert!(has_synthetic_anonymous_segment(
            "mod::anon:arrow:a3f2b1c0::inner"
        ));
    }

    #[test]
    fn a_qualified_name_of_real_segments_is_not_synthetic() {
        for qualified in [
            "tests::write_hello_response",
            "workspace::manager::WorkspaceManager::try_evict_for_test",
            "anonymize::run",
            "handlers::anonymous_user",
        ] {
            assert!(
                !has_synthetic_anonymous_segment(qualified),
                "{qualified} has no synthetic segment"
            );
        }
    }
}
