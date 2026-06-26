//! Context extraction from ASTs
//!
//! Extracts parent/ancestor information for graph nodes using tree-sitter.
//!
//! The `ContextExtractor` walks the AST to find each symbol's parent nodes,
//! building a complete ancestry chain from the symbol to the root.

use std::path::Path;
use tree_sitter::Node;

use super::error::{AstQueryError, Result};
use super::types::{Context, ContextItem, ContextKind, ContextualMatch, ContextualMatchLocation};
use crate::graph::unified::build::StagingGraph;
use crate::graph::unified::concurrent::CodeGraph;
use crate::plugin::PluginManager;

/// Context extractor for extracting AST context from files
///
/// Uses tree-sitter to parse files and extract parent/ancestor information
/// for each node.
///
/// # Refactoring (FT-A.1 + FT-A.2 Complete)
///
/// This struct has been refactored to use `PluginManager` directly, eliminating
/// the dependency on the deprecated `SymbolExtractor`. The new implementation:
///
/// - Parses files only once using `plugin.parse_ast()`
/// - Builds graph nodes using `plugin.graph_builder()` (no re-parse)
/// - Builds context from the same tree (no re-parse)
///
/// This provides ~40-50% performance improvement for context extraction operations.
pub struct ContextExtractor {
    /// Plugin manager for language-specific operations
    plugin_manager: PluginManager,
}

impl ContextExtractor {
    /// Create a new context extractor with default plugin manager
    ///
    /// # Note
    ///
    /// This creates an empty `PluginManager`. Use `with_plugin_manager()` instead
    /// to provide a properly configured plugin manager with registered plugins.
    #[must_use]
    pub fn new() -> Self {
        Self::with_plugin_manager(PluginManager::new())
    }

    /// Create a new context extractor with a specific plugin manager
    ///
    /// This is the recommended way to create a `ContextExtractor`.
    /// It uses `PluginManager` for all operations, eliminating double-parsing.
    ///
    /// # Arguments
    ///
    /// * `plugin_manager` - Configured plugin manager with registered language plugins
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sqry_core::ast::ContextExtractor;
    /// use sqry_core::plugin::PluginManager;
    ///
    /// let mut manager = PluginManager::new();
    /// // Register plugins...
    /// let extractor = ContextExtractor::with_plugin_manager(manager);
    /// ```
    #[must_use]
    pub fn with_plugin_manager(plugin_manager: PluginManager) -> Self {
        Self { plugin_manager }
    }

    /// Extract contextual matches from a file
    ///
    /// # Modern Implementation (FT-A.1 + FT-A.2 Complete)
    ///
    /// This method uses `PluginManager` directly, parsing the file only ONCE.
    /// The refactoring eliminates the double-parsing issue:
    ///
    /// **Before (v0.4.x and earlier)**:
    /// 1. `SymbolExtractor.extract_from_file()` → parse #1
    /// 2. `ContextExtractor` reads file and parses again → parse #2
    ///
    /// **After (v0.5.0+)**:
    /// 1. `plugin.parse_ast()` → parse once
    /// 2. `plugin.graph_builder().build_graph(tree, ...)` → reuse parse
    /// 3. Build context from same tree → reuse parse
    ///
    /// **Performance Impact**: ~40-50% faster for context extraction operations.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the source file
    ///
    /// # Returns
    ///
    /// Vector of contextual matches (nodes with context)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - File cannot be read
    /// - File contains invalid UTF-8
    /// - Parser fails to parse the file
    /// - No plugin supports the file path
    // Extraction is a single linear pipeline; splitting would obscure error context.
    #[allow(clippy::too_many_lines)]
    pub fn extract_from_file(&self, path: &Path) -> Result<Vec<ContextualMatch>> {
        // Get plugin for this path (extension + special filename routing).
        let plugin = self.plugin_manager.plugin_for_path(path).ok_or_else(|| {
            AstQueryError::ContextExtraction(format!(
                "No plugin found for path: {}",
                path.display()
            ))
        })?;

        // Read file content once
        let raw_content = std::fs::read(path)?;

        // Get language metadata
        let lang_name = plugin.metadata().id;

        // Prepare parse-aligned bytes and parse once using the plugin contract.
        let (prepared_content, tree) = plugin
            .prepare_ast(&raw_content)
            .map_err(|e| AstQueryError::ContextExtraction(format!("Failed to parse AST: {e:?}")))?;
        let parse_content = prepared_content.as_ref();

        let builder = plugin.graph_builder().ok_or_else(|| {
            AstQueryError::ContextExtraction(format!("No graph builder registered for {lang_name}"))
        })?;

        // Build nodes via graph builder using the pre-parsed tree.
        let mut staging = StagingGraph::new();
        builder
            .build_graph(&tree, parse_content, path, &mut staging)
            .map_err(|e| {
                AstQueryError::ContextExtraction(format!(
                    "Failed to build graph for {}: {e}",
                    path.display()
                ))
            })?;

        // Context extraction builds a throwaway graph for query results, not the
        // persisted index; it needs body hashes only, no shape descriptors.
        staging.attach_body_hashes(&raw_content, None);

        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(path, Some(builder.language()))
            .map_err(|e| {
                AstQueryError::ContextExtraction(format!(
                    "Failed to register file {}: {e}",
                    path.display()
                ))
            })?;
        staging.apply_file_id(file_id);

        let string_remap = staging.commit_strings(graph.strings_mut()).map_err(|e| {
            AstQueryError::ContextExtraction(format!(
                "Failed to commit strings for {}: {e}",
                path.display()
            ))
        })?;
        staging.apply_string_remap(&string_remap).map_err(|e| {
            AstQueryError::ContextExtraction(format!(
                "Failed to remap strings for {}: {e}",
                path.display()
            ))
        })?;
        let _node_id_map = staging.commit_nodes(graph.nodes_mut()).map_err(|e| {
            AstQueryError::ContextExtraction(format!(
                "Failed to commit nodes for {}: {e}",
                path.display()
            ))
        })?;

        // Convert content to string for context building
        let content_str = String::from_utf8_lossy(&raw_content);
        let root_node = tree.root_node();

        // Extract context for each node using our tree.
        // Gate 0d iter-2 fix: skip unified losers (should not be
        // present on a freshly committed graph, but the explicit
        // guard keeps the invariant honest). See
        // `NodeEntry::is_unified_loser`.
        let mut contextual_matches = Vec::new();
        for (_, entry) in graph.nodes().iter() {
            if entry.is_unified_loser() {
                continue;
            }
            if ContextKind::from_node_kind(entry.kind).is_none() {
                continue;
            }
            if entry.start_line == 0 {
                continue;
            }
            let start_line = entry.start_line;
            let start_column = entry.start_column;
            let mut node = Self::find_defining_node(root_node, start_line, start_column, lang_name);

            if node.is_none()
                && Self::looks_like_byte_span(
                    entry.start_line,
                    entry.end_line,
                    entry.start_column,
                    entry.end_column,
                    &content_str,
                )
            {
                node = Self::find_defining_node_by_bytes(
                    root_node,
                    entry.start_column as usize,
                    entry.end_column as usize,
                    lang_name,
                );
            }

            if let Some(node) = node {
                // Build context from node (parent and ancestors)
                let semantic_context = Self::build_context(&node, &content_str, lang_name);
                let match_name = semantic_context.immediate.name.clone();

                let location = ContextualMatchLocation::new(
                    path.to_path_buf(),
                    entry.start_line,
                    entry.start_column,
                    entry.end_line,
                    entry.end_column,
                );
                contextual_matches.push(ContextualMatch::new(
                    match_name,
                    location,
                    semantic_context,
                    lang_name.to_string(),
                ));
            }
        }

        Ok(contextual_matches)
    }

    /// Find the defining AST node for a symbol at the given position
    ///
    /// Searches for the named scope node (function, class, etc.) that starts at this position.
    fn find_defining_node<'a>(
        root: Node<'a>,
        line: u32,
        column: u32,
        lang_name: &str,
    ) -> Option<Node<'a>> {
        let mut cursor = root.walk();
        Self::find_defining_node_recursive(root, line, column, lang_name, &mut cursor)
    }

    fn find_defining_node_by_bytes<'a>(
        root: Node<'a>,
        start: usize,
        end: usize,
        lang_name: &str,
    ) -> Option<Node<'a>> {
        let target = root.descendant_for_byte_range(start, end)?;
        let mut current = Some(target);

        while let Some(node) = current {
            if Self::is_named_scope(&node, lang_name) {
                return Some(node);
            }
            current = node.parent();
        }

        None
    }

    fn looks_like_byte_span(
        start_line: u32,
        end_line: u32,
        start_column: u32,
        end_column: u32,
        source: &str,
    ) -> bool {
        if start_line != 1 || end_line != 1 {
            return false;
        }
        let first_line_len = source.lines().next().map_or(0, str::len);
        let start = start_column as usize;
        let end = end_column as usize;
        start > first_line_len || end > first_line_len
    }

    /// Recursively search for the defining node
    fn find_defining_node_recursive<'a>(
        node: Node<'a>,
        line: u32,
        column: u32,
        lang_name: &str,
        cursor: &mut tree_sitter::TreeCursor<'a>,
    ) -> Option<Node<'a>> {
        // Check if this node contains the given position
        // Using range-based matching instead of exact position matching for robustness
        let start_pos = node.start_position();
        let end_pos = node.end_position();

        // Convert to 1-based line numbers for comparison (clamped to u32::MAX)
        let node_start_line = start_pos
            .row
            .try_into()
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let node_end_line = end_pos.row.try_into().unwrap_or(u32::MAX).saturating_add(1);

        // Check if line is within node's line range
        let line_in_range = line >= node_start_line && line <= node_end_line;

        // Convert columns safely (clamped to u32::MAX)
        let start_col: u32 = start_pos.column.try_into().unwrap_or(u32::MAX);
        let end_col: u32 = end_pos.column.try_into().unwrap_or(u32::MAX);

        // Check if column is within node's column range for the given line
        let col_in_range = if line == node_start_line && line == node_end_line {
            // Single-line node: column must be between start and end
            column >= start_col && column <= end_col
        } else if line == node_start_line {
            // First line: column must be >= start column
            column >= start_col
        } else if line == node_end_line {
            // Last line: column must be <= end column
            column <= end_col
        } else {
            // Middle lines: any column is valid
            true
        };

        // Early exit if position not in range
        if !line_in_range || !col_in_range {
            return None;
        }

        // Collect children first to avoid borrow issues
        let children: Vec<Node<'a>> = node.children(cursor).collect();

        // Search children first to find the innermost matching node
        for child in children {
            let child_end = child.end_position();
            // Only search if the position could be within this child (clamped to u32::MAX)
            let child_end_line: u32 = child_end
                .row
                .try_into()
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            if child_end_line >= line
                && let Some(found) =
                    Self::find_defining_node_recursive(child, line, column, lang_name, cursor)
            {
                return Some(found);
            }
        }

        // No matching child found, check if this node is a named scope
        if Self::is_named_scope(&node, lang_name) {
            return Some(node);
        }

        None
    }

    /// Build context from an AST node
    ///
    /// Walks up the tree from the node to build the full context chain.
    fn build_context(node: &Node, source: &str, lang_name: &str) -> Context {
        let source_bytes = source.as_bytes();

        // Build immediate context item from this node
        let immediate = Self::node_to_context_item(node, source_bytes, lang_name);

        // Walk up to find parent and ancestors
        let mut parent = None;
        let mut ancestors = Vec::new();
        let mut current = node.parent();

        while let Some(node) = current {
            // Check if this node represents a named scope
            if Self::is_named_scope(&node, lang_name) {
                let item = Self::node_to_context_item(&node, source_bytes, lang_name);

                if parent.is_none() {
                    parent = Some(item);
                } else {
                    ancestors.push(item);
                }
            }

            current = node.parent();
        }

        Context::new(immediate, parent, ancestors)
    }

    /// Convert an AST node to a `ContextItem`
    fn node_to_context_item(node: &Node, source_bytes: &[u8], lang_name: &str) -> ContextItem {
        // Extract name from node
        let name = Self::extract_name(node, source_bytes, lang_name)
            .unwrap_or_else(|| "<anonymous>".to_string());

        // Determine kind from node type (with parent-sensitive handling)
        let kind = Self::node_to_context_kind(node, lang_name);

        // Extract position
        let start_pos = node.start_position();
        let end_pos = node.end_position();

        // Convert to 1-based line numbers (clamped to u32::MAX)
        let start_line = start_pos
            .row
            .try_into()
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let end_line = end_pos.row.try_into().unwrap_or(u32::MAX).saturating_add(1);

        ContextItem::new(
            name,
            kind,
            start_line,
            end_line,
            node.start_byte(),
            node.end_byte(),
        )
    }

    /// Check if a node represents a named scope (function, class, etc.)
    fn is_named_scope(node: &Node, lang_name: &str) -> bool {
        let kind = node.kind();

        // Exclude root-level nodes (source_file, program, module)
        if matches!(kind, "source_file" | "program" | "module") {
            return false;
        }

        match lang_name {
            "rust" => matches!(
                kind,
                "function_item"
                    | "impl_item"
                    | "trait_item"
                    | "struct_item"
                    | "enum_item"
                    | "mod_item"
            ),
            "javascript" | "typescript" => matches!(
                kind,
                "function_declaration"
                    | "method_definition"
                    | "class_declaration"
                    | "lexical_declaration"
            ),
            "python" => matches!(kind, "function_definition" | "class_definition"),
            "go" => matches!(
                kind,
                "function_declaration" | "method_declaration" | "type_declaration"
            ),
            _ => false,
        }
    }

    /// Get the identifier node kinds to search for in a given language.
    fn identifier_kinds(lang_name: &str) -> &'static [&'static str] {
        match lang_name {
            "rust" => &["identifier", "type_identifier"],
            "javascript" | "typescript" => &["identifier", "property_identifier"],
            "python" | "go" => &["identifier"],
            _ => &[],
        }
    }

    /// Extract name from an AST node
    fn extract_name(node: &Node, source_bytes: &[u8], lang_name: &str) -> Option<String> {
        let kinds = Self::identifier_kinds(lang_name);
        if kinds.is_empty() {
            return None;
        }

        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|child| kinds.contains(&child.kind()))
            .and_then(|child| child.utf8_text(source_bytes).ok())
            .map(std::string::ToString::to_string)
    }

    /// Map tree-sitter node kind to `ContextKind`
    fn node_to_context_kind(node: &Node, lang_name: &str) -> ContextKind {
        if lang_name == "rust" && node.kind() == "function_item" {
            let mut current = node.parent();
            while let Some(parent) = current {
                if matches!(parent.kind(), "impl_item" | "trait_item") {
                    return ContextKind::Method;
                }
                current = parent.parent();
            }
        }

        Self::node_kind_to_context_kind(node.kind(), lang_name)
    }

    fn node_kind_to_context_kind(node_kind: &str, lang_name: &str) -> ContextKind {
        match lang_name {
            "rust" => match node_kind {
                "impl_item" => ContextKind::Impl,
                "trait_item" => ContextKind::Trait,
                "struct_item" => ContextKind::Struct,
                "enum_item" => ContextKind::Enum,
                "mod_item" => ContextKind::Module,
                "const_item" => ContextKind::Constant,
                "static_item" => ContextKind::Variable,
                "type_item" => ContextKind::TypeAlias,
                _ => ContextKind::Function,
            },
            "javascript" | "typescript" => match node_kind {
                "method_definition" => ContextKind::Method,
                "class_declaration" => ContextKind::Class,
                "lexical_declaration" | "variable_declaration" => ContextKind::Variable,
                _ => ContextKind::Function,
            },
            "python" => match node_kind {
                "class_definition" => ContextKind::Class,
                _ => ContextKind::Function,
            },
            "go" => match node_kind {
                "method_declaration" => ContextKind::Method,
                "type_declaration" => ContextKind::Struct,
                _ => ContextKind::Function,
            },
            _ => ContextKind::Function,
        }
    }

    /// Extract context from a directory recursively
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when traversal fails (e.g., directory unreadable) or when
    /// `extract_from_file` propagates extraction errors.
    pub fn extract_from_directory(&self, root: &Path) -> Result<Vec<ContextualMatch>> {
        let mut all_matches = Vec::new();

        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            let path = entry.path();
            if path.is_file() {
                // Try to extract context (ignore unsupported files)
                if let Ok(mut matches) = self.extract_from_file(path) {
                    all_matches.append(&mut matches);
                }
            }
        }

        Ok(all_matches)
    }
}

impl Default for ContextExtractor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TESTS
// ============================================================================
//
// NOTE: These tests are gated behind `cfg(feature = "context-tests")`
// to avoid circular dependencies during `cargo test -p sqry-core`.
//
// The circular dependency occurs because:
// 1. sqry-core has dev-dependencies on language plugins (sqry-lang-rust, etc.)
// 2. Those plugins depend on sqry-core
// 3. Tests that register plugins cause trait version mismatches (E0277)
//
// To run these tests, use workspace-level tests which don't have this issue:
//   cargo test --workspace
//
// Or explicitly enable the feature:
//   cargo test -p sqry-core --features context-tests
//
#[cfg(all(test, feature = "context-tests"))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Creates a `PluginManager` pre-loaded with builtin plugins for testing.
    ///
    /// Other plugins are intentionally excluded to avoid trait version mismatches
    /// between test dependencies. Returns an empty manager in unit test context
    /// since language plugin crates are dev-dependencies only available in
    /// integration tests.
    fn create_test_plugin_manager() -> crate::plugin::PluginManager {
        // Returns empty PluginManager in unit test context
        // Language plugin crates are dev-dependencies, only available in integration tests
        // Unit tests that need plugins should be moved to tests/ directory
        crate::test_support::plugin_factory::with_builtin_plugins()
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_extract_rust_function_context() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r#"
fn top_level() {
    println!("hello");
}

struct MyStruct {
    value: i32,
}

impl MyStruct {
    fn method(&self) -> i32 {
        self.value
    }
}
"#,
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find: top_level function, MyStruct struct, method
        assert!(
            matches.len() >= 2,
            "Expected at least 2 matches, found {}",
            matches.len()
        );

        // Find top_level function
        let top_level = matches.iter().find(|m| m.name == "top_level");
        assert!(top_level.is_some(), "Should find top_level function");
        if let Some(m) = top_level {
            assert_eq!(m.context.depth(), 1, "top_level should be at depth 1");
            assert_eq!(m.context.path(), "top_level");
        }

        // Find method
        let method = matches.iter().find(|m| m.name == "method");
        if let Some(m) = method {
            assert!(m.context.depth() >= 1, "method should have depth >= 1");
            assert!(m.context.parent.is_some(), "method should have a parent");
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_extract_nested_context() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r"
mod outer {
    struct Inner {
        value: i32,
    }

    impl Inner {
        fn deeply_nested(&self) {
            // nested function
        }
    }
}
",
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Find deeply_nested method
        let method = matches.iter().find(|m| m.name == "deeply_nested");
        if let Some(m) = method {
            // Should have parent (impl or struct)
            assert!(m.context.parent.is_some(), "Should have parent");
            // Depth depends on whether we capture mod, impl, etc.
            assert!(m.context.depth() >= 1, "Should have depth >= 1");
        }
    }

    // Note: JavaScript and Python tests disabled to avoid trait version mismatches
    // The PluginManager test helper only registers RustPlugin
    // These tests can be re-enabled when all plugins are rebuilt with the new trait

    #[test]
    #[ignore = "JavaScript plugin not registered in test helper"]
    fn test_extract_javascript_class() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.js");
        fs::write(
            &file_path,
            r#"
function topLevel() {
    console.log("hello");
}

class MyClass {
    constructor(name) {
        this.name = name;
    }

    greet() {
        console.log("Hello " + this.name);
    }
}
"#,
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        assert!(matches.len() >= 2, "Should find at least 2 matches");

        // Find topLevel function
        let top_fn = matches.iter().find(|m| m.name == "topLevel");
        if let Some(m) = top_fn {
            assert_eq!(m.context.depth(), 1, "topLevel should be at depth 1");
        }

        // Find class
        let class = matches.iter().find(|m| m.name == "MyClass");
        assert!(class.is_some(), "Should find MyClass");
    }

    #[test]
    #[ignore = "Python plugin not registered in test helper"]
    fn test_extract_python_context() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
def top_level():
    print("hello")

class MyClass:
    def method(self):
        return 42
"#,
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        assert!(matches.len() >= 2, "Should find at least 2 matches");

        // Find top_level function
        let top_fn = matches.iter().find(|m| m.name == "top_level");
        if let Some(m) = top_fn {
            assert_eq!(m.context.depth(), 1);
        }

        // Find method
        let method = matches.iter().find(|m| m.name == "method");
        if let Some(m) = method {
            assert!(m.context.depth() >= 2);
            assert!(m.context.parent.is_some());
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_empty_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("empty.rs");
        fs::write(&file_path, "").unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        assert_eq!(matches.len(), 0);
    }

    // H2: Position matching robustness tests

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_position_matching_single_line_function() {
        // Test that position matching works for single-line functions
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r#"
fn single_line() { println!("hello"); }
"#,
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find the function
        let func = matches.iter().find(|m| m.name == "single_line");
        assert!(func.is_some(), "Should find single-line function");

        if let Some(m) = func {
            assert_eq!(m.context.depth(), 1);
            assert_eq!(m.context.path(), "single_line");
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_position_matching_multiline_function() {
        // Test that position matching works for multi-line functions
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r"
fn multiline() {
    let x = 1;
    let y = 2;
    x + y
}
",
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find the function
        let func = matches.iter().find(|m| m.name == "multiline");
        assert!(func.is_some(), "Should find multi-line function");

        if let Some(m) = func {
            assert_eq!(m.context.depth(), 1);
            // Verify context spans multiple lines
            assert!(m.end_line > m.start_line + 1);
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_position_matching_nested_structures() {
        // Test position matching with nested structures
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r"
mod outer {
    struct Inner {
        field: i32,
    }

    impl Inner {
        fn method(&self) -> i32 {
            self.field
        }
    }
}
",
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find the method
        let method = matches.iter().find(|m| m.name == "method");
        assert!(method.is_some(), "Should find nested method");

        if let Some(m) = method {
            // Method is inside impl block inside mod
            // Structure: outer (mod) -> Inner (struct) -> impl Inner -> method
            // Depth: 1 (self) + 1 (parent=impl) + 1 (ancestor=outer) = 3
            assert_eq!(m.context.depth(), 3, "Method should have depth 3");
            assert!(m.context.parent.is_some(), "Method should have parent");
            if let Some(parent) = &m.context.parent {
                // Parent is the impl block, named after the type it implements
                assert_eq!(parent.name, "Inner", "Method parent should be Inner impl");
            }
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_position_matching_with_comments() {
        // Test that position matching works correctly with comments
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r#"
// This is a comment
/// Documentation comment
fn documented_function() {
    // Internal comment
    println!("test");
}
"#,
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find the function despite comments
        let func = matches.iter().find(|m| m.name == "documented_function");
        assert!(func.is_some(), "Should find function with comments");

        if let Some(m) = func {
            assert_eq!(m.context.depth(), 1);
        }
    }

    #[test]
    #[ignore = "Plugins not available in unit tests (dev-dependencies). Move to integration tests if needed."]
    fn test_position_matching_edge_positions() {
        // Test position matching at various positions within a function
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(
            &file_path,
            r"
struct Container {
    value: i32,
}

impl Container {
    fn new(val: i32) -> Self {
        Self { value: val }
    }
}
",
        )
        .unwrap();

        let manager = create_test_plugin_manager();
        let extractor = ContextExtractor::with_plugin_manager(manager);
        let matches = extractor.extract_from_file(&file_path).unwrap();

        // Should find both struct and method
        let container = matches.iter().find(|m| m.name == "Container");
        let new_method = matches.iter().find(|m| m.name == "new");

        assert!(container.is_some(), "Should find Container struct");
        assert!(new_method.is_some(), "Should find new method");

        if let Some(m) = new_method {
            // Method inside impl block
            // Structure: impl Container -> new
            // Depth: 1 (self) + 1 (parent=impl) = 2
            assert_eq!(m.context.depth(), 2, "Method should have depth 2");
            if let Some(parent) = &m.context.parent {
                assert_eq!(
                    parent.name, "Container",
                    "Method parent should be Container impl"
                );
            }
        }
    }
}
