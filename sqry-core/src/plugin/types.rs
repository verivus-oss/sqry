//! Core plugin types and traits.

use crate::ast::Scope;
use crate::query::results::QueryMatch;
use crate::query::types::{FieldDescriptor, Value};
use std::path::Path;
use tree_sitter::Tree;

use super::error::{ParseError, PluginResult, ScopeError};

/// Core trait that all language plugins must implement.
///
/// This trait defines the contract for language support in sqry. Plugins are responsible for:
/// - Parsing source code into ASTs
/// - Building graph nodes and edges from ASTs
/// - Extracting scope information for context-aware search
///
/// # Thread Safety
///
/// Plugins must be `Send + Sync` to support multi-threaded operation.
///
/// # Example
///
/// ```ignore
/// use sqry_core::plugin::LanguagePlugin;
///
/// struct RustPlugin;
///
/// impl LanguagePlugin for RustPlugin {
///     fn metadata(&self) -> LanguageMetadata {
///         LanguageMetadata {
///             id: "rust",
///             name: "Rust",
///             version: "0.1.0",
///             author: "Verivus Pty Ltd",
///             description: "Rust language support",
///             tree_sitter_version: "0.24",
///         }
///     }
///
///     fn extensions(&self) -> &'static [&'static str] {
///         &["rs"]
///     }
///
///     // ... implement other required methods
/// }
/// ```
pub trait LanguagePlugin: Send + Sync {
    /// Returns language metadata (name, version, author, etc.).
    fn metadata(&self) -> LanguageMetadata;

    /// Returns file extensions this plugin handles (.rs, .js, .py, etc.).
    ///
    /// Extensions should be lowercase without the leading dot (e.g., "rs" not ".rs").
    /// Returns static slice of static strings for future-proof lifetime handling.
    fn extensions(&self) -> &'static [&'static str];

    /// Get tree-sitter Language for this plugin.
    ///
    /// Used by core to compile and cache queries. The Language object is cheap to construct
    /// (constant time) so plugins don't need to cache it.
    fn language(&self) -> tree_sitter::Language;

    /// Build AST for querying.
    ///
    /// Parses source code and returns tree-sitter AST.
    ///
    /// # Arguments
    ///
    /// * `content` - Source code as bytes (UTF-8 encoded)
    ///
    /// # Returns
    ///
    /// tree-sitter AST (`Tree`)
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if the parser cannot build an AST (invalid UTF-8 input,
    /// language configuration errors, or tree-sitter failures).
    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError>;

    /// Extract scope information (for context-aware search).
    ///
    /// Analyzes AST to extract scope information like function boundaries, class definitions,
    /// module structure, etc. Used for context-aware search and navigation.
    ///
    /// # Arguments
    ///
    /// * `tree` - Parsed AST from `parse_ast()`
    /// * `content` - Source code as bytes (UTF-8 encoded), required for tree-sitter query captures
    ///
    /// # Returns
    ///
    /// Vector of scopes (functions, classes, modules, etc.)
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError`] when scope queries fail to compile or when extraction hits
    /// invalid AST structures.
    ///
    /// # Breaking Change (v0.4.0)
    ///
    /// This method signature changed in v0.4.0 to add the `content` parameter.
    /// Plugin authors must update their implementations to accept source bytes.
    ///
    /// # Breaking Change (P2-34)
    ///
    /// This method signature changed to add the `file_path` parameter for scope nesting.
    /// Plugin authors must update implementations to pass `file_path` to Scope construction.
    fn extract_scopes(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError>;

    /// Get language-specific field descriptors (Phase 7+).
    ///
    /// Returns field descriptors for plugin-specific queryable fields (e.g., `async`, `visibility`).
    /// These fields extend the core field set and allow for language-specific queries.
    ///
    /// # Returns
    ///
    /// Static slice of field descriptors
    ///
    /// # Default Implementation
    ///
    /// Returns empty slice (no plugin-specific fields)
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn fields(&self) -> &'static [FieldDescriptor] {
    ///     &[
    ///         FieldDescriptor {
    ///             name: "async",
    ///             field_type: FieldType::Bool,
    ///             operators: &[Operator::Equal],
    ///             indexed: false,
    ///             doc: "Whether function is async",
    ///         },
    ///     ]
    /// }
    /// ```
    fn fields(&self) -> &'static [FieldDescriptor] {
        &[]
    }

    /// Evaluate a plugin-specific field for a query match (Phase 7+).
    ///
    /// Checks if a query match satisfies a given field condition. This method is called by the query
    /// executor when evaluating conditions on plugin-specific fields.
    ///
    /// # Arguments
    ///
    /// * `entry` - Graph-backed query match to check
    /// * `field` - Field name (one of the fields returned by `fields()`)
    /// * `value` - Value to match against
    ///
    /// # Returns
    ///
    /// `true` if the entry matches the condition, `false` otherwise
    ///
    /// # Default Implementation
    ///
    /// Returns `false` (no plugin-specific field evaluation)
    ///
    /// # Errors
    ///
    /// Returns [`PluginError`](super::error::PluginError) when the implementation needs to
    /// surface type mismatches or evaluation failures.
    ///
    fn evaluate_field(
        &self,
        _entry: &QueryMatch,
        _field: &str,
        _value: &Value,
    ) -> PluginResult<bool> {
        Ok(false)
    }

    /// Extract return type metadata from function signatures (Sprint 2).
    ///
    /// Analyzes function signatures to extract return type information.
    /// Returns a simple string representation of the return type.
    ///
    /// # Arguments
    ///
    /// * `entry` - Graph-backed query match to extract return type from
    /// * `tree` - Parsed AST from `parse_ast()`
    /// * `content` - Source code as bytes (UTF-8 encoded)
    ///
    /// # Returns
    ///
    /// Optional return type string (e.g., "Result", "String", "Option")
    ///
    /// # Default Implementation
    ///
    /// Returns None (no type extraction)
    ///
    /// # Errors
    ///
    /// Returns [`PluginError`](super::error::PluginError) when the implementation cannot
    /// evaluate the return type (e.g., due to malformed signatures).
    fn extract_return_type(
        &self,
        _entry: &QueryMatch,
        _tree: &Tree,
        _content: &[u8],
    ) -> PluginResult<Option<String>> {
        Ok(None)
    }

    /// Get `GraphBuilder` implementation for this language.
    ///
    /// **DEPRECATED**: This method uses the legacy DashMap-based `CodeGraph`.
    /// New code should implement `build_unified_graph()` instead.
    ///
    /// Returns the `GraphBuilder` trait object that can build `CodeGraph` edges
    /// for this language. This enables graph-based indexing mode.
    ///
    /// # Returns
    ///
    /// Optional reference to `GraphBuilder` implementation. Returns None if
    /// this language doesn't support graph-based indexing.
    ///
    /// # Default Implementation
    ///
    /// Returns None (no graph builder)
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
    ///     Some(&self.graph_builder)
    /// }
    /// ```
    fn graph_builder(&self) -> Option<&dyn crate::graph::GraphBuilder> {
        None
    }

    /// Build unified graph for this file.
    ///
    /// This method populates the unified graph (Arena+CSR storage) using a
    /// transaction-based API. Plugins should extract nodes and edges from the
    /// AST and add them to the transaction.
    ///
    /// # Arguments
    ///
    /// * `tree` - Parsed AST from `parse_ast()`
    /// * `content` - Source code as bytes (UTF-8 encoded)
    /// * `file` - Path to source file
    /// * `txn` - Write transaction for graph mutations
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, error if graph building fails
    ///
    /// # Default Implementation
    ///
    /// Returns `Ok(())` (no-op). Plugins should implement this to enable
    /// unified graph indexing.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError`](super::error::PluginError) when graph construction
    /// fails (e.g., due to malformed AST nodes or transaction errors).
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn build_unified_graph(
    ///     &self,
    ///     tree: &Tree,
    ///     content: &[u8],
    ///     file: &Path,
    ///     txn: &mut GraphWriteTxn,
    /// ) -> PluginResult<()> {
    ///     // Extract function definitions
    ///     let query = self.language().query(FUNCTION_QUERY)?;
    ///     for capture in query.captures(tree.root_node(), content) {
    ///         let node = capture.node;
    ///         let name = node.utf8_text(content)?;
    ///         txn.add_node(
    ///             Language::Rust,
    ///             file,
    ///             name,
    ///             NodeKind::Function,
    ///             None,
    ///         )?;
    ///     }
    ///     Ok(())
    /// }
    /// ```
    fn build_unified_graph(
        &self,
        _tree: &Tree,
        _content: &[u8],
        _file: &Path,
        _txn: &mut crate::graph::unified::GraphWriteTxn,
    ) -> PluginResult<()> {
        Ok(())
    }
}

/// Language metadata returned by plugins.
///
/// Provides information about the language plugin for display, logging, and version checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageMetadata {
    /// Stable identifier for lookups (e.g., "rust", "javascript").
    ///
    /// This ID is used for plugin registration and lookup by name. It should be:
    /// - Lowercase
    /// - Alphanumeric + hyphens only
    /// - Stable across versions
    pub id: &'static str,

    /// Human-readable display name (e.g., "Rust", "JavaScript").
    pub name: &'static str,

    /// Plugin version (semver format, e.g., "0.1.0").
    pub version: &'static str,

    /// Plugin author (e.g., "Verivus Pty Ltd", "Community").
    pub author: &'static str,

    /// Human-readable description of the plugin.
    pub description: &'static str,

    /// Tree-sitter grammar version (e.g., "0.24").
    ///
    /// Used for compatibility checking in future phases.
    pub tree_sitter_version: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test that LanguageMetadata is Send + Sync
    #[test]
    fn test_metadata_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LanguageMetadata>();
    }

    // Test LanguageMetadata construction
    #[test]
    fn test_metadata_construction() {
        let metadata = LanguageMetadata {
            id: "test",
            name: "Test Language",
            version: "1.0.0",
            author: "Test Author",
            description: "A test language plugin",
            tree_sitter_version: "0.24",
        };

        assert_eq!(metadata.id, "test");
        assert_eq!(metadata.name, "Test Language");
        assert_eq!(metadata.version, "1.0.0");
    }

    // Test trait object safety (can create Box<dyn LanguagePlugin>)
    #[test]
    fn test_trait_object_safe() {
        // This test verifies that LanguagePlugin is object-safe
        // by attempting to create a trait object type
        fn _assert_object_safe(_: &dyn LanguagePlugin) {}
    }
}
