//! Plugin manager for language plugin registration and lookup.

use super::types::LanguagePlugin;
use std::collections::HashMap;
use std::path::Path;

/// Manages language plugins for sqry.
///
/// The `PluginManager` is responsible for:
/// - Registering built-in plugins (statically linked)
/// - Looking up plugins by file extension or language name
/// - Enumerating available plugins
///
/// # Phase 2 Scope
///
/// Phase 2 supports built-in plugins only (statically linked). External plugin loading
/// (.so/.dylib) will be added in Phase 3.
///
/// # Example
///
/// ```ignore
/// use sqry_core::plugin::PluginManager;
///
/// // Create manager with default built-in plugins
/// let manager = PluginManager::new();
///
/// // Lookup plugin by extension
/// if let Some(plugin) = manager.plugin_for_extension("rs") {
///     println!("Found Rust plugin");
/// }
///
/// // Lookup plugin by language ID
/// if let Some(plugin) = manager.plugin_by_id("rust") {
///     println!("Found Rust plugin by ID");
/// }
/// ```
pub struct PluginManager {
    /// Built-in plugins (statically linked)
    builtin_plugins: Vec<Box<dyn LanguagePlugin>>,

    /// Fast lookup cache: extension -> index in `builtin_plugins`
    extension_cache: HashMap<String, usize>,

    /// Fast lookup cache: language ID -> index in `builtin_plugins`
    id_cache: HashMap<String, usize>,

    /// Cached hash of all plugin versions (for cache invalidation)
    plugin_state_hash: u64,
}

impl PluginManager {
    /// Create a new `PluginManager` with default built-in plugins.
    ///
    /// Default built-ins will be registered in Phase 2 after the first plugin is implemented.
    /// For now, creates an empty manager.
    ///
    /// # Note
    ///
    /// For tests requiring built-in plugins, use the `plugin_factory_helpers` test module:
    /// ```ignore
    /// use plugin_factory_helpers::with_builtin_plugins;
    /// let manager = with_builtin_plugins();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::with_plugins(Vec::new())
    }

    /// Create an empty `PluginManager` for testing (test-only).
    ///
    /// Creates a manager with no plugins registered. Useful for tests that need
    /// isolation or want to manually register specific plugins.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sqry_core::plugin::PluginManager;
    ///
    /// // Create empty manager for isolated testing
    /// let manager = PluginManager::empty();
    /// assert_eq!(manager.plugins().len(), 0);
    ///
    /// // Manually register specific plugin
    /// let mut manager = PluginManager::empty();
    /// manager.register_builtin(Box::new(RustPlugin::new()));
    /// assert_eq!(manager.plugins().len(), 1);
    /// ```
    #[cfg(test)]
    #[must_use]
    pub fn empty() -> Self {
        Self::with_plugins(Vec::new())
    }

    /// Create a `PluginManager` with custom plugins.
    ///
    /// This is primarily for testing, but can also be used to create a manager with
    /// a specific set of plugins.
    ///
    /// # Arguments
    ///
    /// * `plugins` - Vector of boxed plugins to register
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sqry_core::plugin::PluginManager;
    ///
    /// let plugins = vec![
    ///     Box::new(RustPlugin::new()) as Box<dyn LanguagePlugin>,
    /// ];
    ///
    /// let manager = PluginManager::with_plugins(plugins);
    /// ```
    #[must_use]
    pub fn with_plugins(plugins: Vec<Box<dyn LanguagePlugin>>) -> Self {
        let plugin_state_hash = Self::compute_plugin_hash_static(&plugins);

        let mut manager = Self {
            builtin_plugins: plugins,
            extension_cache: HashMap::new(),
            id_cache: HashMap::new(),
            plugin_state_hash,
        };

        // Build caches
        manager.rebuild_caches();

        manager
    }

    /// Register a built-in plugin.
    ///
    /// Adds a plugin to the manager and updates lookup caches.
    ///
    /// # Arguments
    ///
    /// * `plugin` - Boxed plugin to register
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut manager = PluginManager::new();
    /// manager.register_builtin(Box::new(RustPlugin::new()));
    /// ```
    pub fn register_builtin(&mut self, plugin: Box<dyn LanguagePlugin>) {
        let index = self.builtin_plugins.len();
        self.builtin_plugins.push(plugin);

        // Update caches for the new plugin
        let plugin_ref = &self.builtin_plugins[index];
        let metadata = plugin_ref.metadata();

        // Cache language ID
        self.id_cache.insert(metadata.id.to_string(), index);

        // Cache extensions
        for ext in plugin_ref.extensions() {
            self.extension_cache.insert((*ext).to_string(), index);
        }

        // Update plugin state hash
        self.update_plugin_hash();
    }

    /// Get plugin for file extension.
    ///
    /// Returns a reference to the plugin that handles the given file extension.
    ///
    /// # Arguments
    ///
    /// * `ext` - File extension without leading dot (e.g., "rs" not ".rs")
    ///
    /// # Returns
    ///
    /// Optional reference to the language plugin, or None if no plugin handles this extension.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(plugin) = manager.plugin_for_extension("rs") {
    ///     println!("Found {} plugin", plugin.metadata().name);
    /// }
    /// ```
    #[must_use]
    pub fn plugin_for_extension(&self, ext: &str) -> Option<&dyn LanguagePlugin> {
        let ext = ext.to_ascii_lowercase();
        self.extension_cache
            .get(ext.as_str())
            .and_then(|&index| self.builtin_plugins.get(index).map(|p| &**p))
    }

    /// Get plugin for a file path, including special filename routing.
    ///
    /// This helper supports standard extension matching and special-case routing
    /// for filenames like `Pulumi.<stack>.yaml` that cannot be resolved by extension alone.
    #[must_use]
    pub fn plugin_for_path(&self, path: &Path) -> Option<&dyn LanguagePlugin> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim_start_matches('.').to_ascii_lowercase());

        if let Some(name) = filename.as_deref()
            && name.starts_with("pulumi.")
        {
            let ext = Path::new(name).extension().and_then(|e| e.to_str());
            if matches!(ext, Some("yaml" | "yml" | "json"))
                && let Some(plugin) = self.plugin_by_id("pulumi")
            {
                return Some(plugin);
            }
        }

        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(plugin) = self.plugin_for_extension(ext)
        {
            return Some(plugin);
        }

        filename
            .as_deref()
            .and_then(|name| self.plugin_for_extension(name))
    }

    /// Get plugin by language ID.
    ///
    /// Returns a reference to the plugin with the given stable language ID.
    ///
    /// # Arguments
    ///
    /// * `id` - Stable language ID (e.g., "rust", "javascript")
    ///
    /// # Returns
    ///
    /// Optional reference to the language plugin, or None if no plugin has this ID.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(plugin) = manager.plugin_by_id("rust") {
    ///     println!("Found Rust plugin");
    /// }
    /// ```
    #[must_use]
    pub fn plugin_by_id(&self, id: &str) -> Option<&dyn LanguagePlugin> {
        self.id_cache
            .get(id)
            .and_then(|&index| self.builtin_plugins.get(index).map(|p| &**p))
    }

    /// List all registered plugins.
    ///
    /// Returns a vector of references to all plugins in the manager.
    ///
    /// # Returns
    ///
    /// Vector of plugin references
    ///
    /// # Example
    ///
    /// ```ignore
    /// for plugin in manager.plugins() {
    ///     let metadata = plugin.metadata();
    ///     println!("Plugin: {} v{}", metadata.name, metadata.version);
    /// }
    /// ```
    #[must_use]
    pub fn plugins(&self) -> Vec<&dyn LanguagePlugin> {
        self.builtin_plugins.iter().map(|p| &**p).collect()
    }

    /// Compute deterministic hash of all plugin versions
    fn compute_plugin_hash_static(plugins: &[Box<dyn LanguagePlugin>]) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut hasher = DefaultHasher::new();

        // Sort plugin IDs for deterministic ordering
        let mut plugin_data: Vec<_> = plugins
            .iter()
            .map(|p| {
                let meta = p.metadata();
                (meta.id.to_string(), meta.version.to_string())
            })
            .collect();
        plugin_data.sort_by(|a, b| a.0.cmp(&b.0));

        for (id, version) in plugin_data {
            hasher.write(id.as_bytes());
            hasher.write(version.as_bytes());
        }

        hasher.finish()
    }

    /// Get cached plugin state hash (for cache key)
    #[must_use]
    pub fn plugin_state_hash(&self) -> u64 {
        self.plugin_state_hash
    }

    /// Update plugin state hash (call after plugin changes)
    fn update_plugin_hash(&mut self) {
        self.plugin_state_hash = Self::compute_plugin_hash_static(&self.builtin_plugins);
    }

    /// Rebuild lookup caches.
    ///
    /// Called internally after plugins are modified. Rebuilds the `extension_cache`
    /// and `id_cache` from the current plugin list.
    fn rebuild_caches(&mut self) {
        self.extension_cache.clear();
        self.id_cache.clear();

        for (index, plugin) in self.builtin_plugins.iter().enumerate() {
            let metadata = plugin.metadata();

            // Cache language ID
            self.id_cache.insert(metadata.id.to_string(), index);

            // Cache extensions
            for ext in plugin.extensions() {
                self.extension_cache.insert((*ext).to_string(), index);
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Scope;
    use crate::plugin::LanguageMetadata;
    use std::path::Path;

    // Mock plugin for testing
    struct MockPlugin {
        id: &'static str,
        name: &'static str,
        extensions: &'static [&'static str],
    }

    impl MockPlugin {
        fn new(id: &'static str, name: &'static str, extensions: &'static [&'static str]) -> Self {
            Self {
                id,
                name,
                extensions,
            }
        }
    }

    impl LanguagePlugin for MockPlugin {
        fn metadata(&self) -> LanguageMetadata {
            LanguageMetadata {
                id: self.id,
                name: self.name,
                version: "1.0.0",
                author: "Test",
                description: "Mock plugin for testing",
                tree_sitter_version: "0.24",
            }
        }

        fn extensions(&self) -> &'static [&'static str] {
            self.extensions
        }

        fn language(&self) -> tree_sitter::Language {
            // Return a test language (this is just for testing)
            // In real plugins, this would return tree_sitter_rust::LANGUAGE, etc.
            sqry_test_support::test_language()
        }

        fn parse_ast(
            &self,
            _content: &[u8],
        ) -> Result<tree_sitter::Tree, super::super::error::ParseError> {
            Err(super::super::error::ParseError::TreeSitterFailed)
        }

        fn extract_scopes(
            &self,
            _tree: &tree_sitter::Tree,
            _content: &[u8],
            _file_path: &Path,
        ) -> Result<Vec<Scope>, super::super::error::ScopeError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn test_empty_manager() {
        let manager = PluginManager::new();
        assert!(manager.plugin_for_extension("rs").is_none());
        assert!(manager.plugin_by_id("rust").is_none());
        assert_eq!(manager.plugins().len(), 0);
    }

    #[test]
    fn test_register_plugin() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));

        assert!(manager.plugin_for_extension("rs").is_some());
        assert!(manager.plugin_by_id("rust").is_some());
        assert_eq!(manager.plugins().len(), 1);
    }

    #[test]
    fn test_plugin_lookup_by_extension() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));

        let plugin = manager.plugin_for_extension("rs").unwrap();
        assert_eq!(plugin.metadata().id, "rust");
        assert_eq!(plugin.metadata().name, "Rust");
    }

    #[test]
    fn test_plugin_lookup_by_extension_case_insensitive() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));

        assert!(manager.plugin_for_extension("RS").is_some());
    }

    #[test]
    fn test_plugin_lookup_by_path_pulumi_stack() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new(
            "pulumi",
            "Pulumi",
            &["pulumi.yaml"],
        )));

        let plugin = manager
            .plugin_for_path(Path::new("Pulumi.dev.yaml"))
            .expect("pulumi plugin should match");
        assert_eq!(plugin.metadata().id, "pulumi");
    }

    #[test]
    fn test_plugin_lookup_by_id() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));

        let plugin = manager.plugin_by_id("rust").unwrap();
        assert_eq!(plugin.metadata().id, "rust");
        assert_eq!(plugin.metadata().name, "Rust");
    }

    #[test]
    fn test_plugin_lookup_miss() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));

        assert!(manager.plugin_for_extension("js").is_none());
        assert!(manager.plugin_by_id("javascript").is_none());
    }

    #[test]
    fn test_multiple_extensions() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new(
            "typescript",
            "TypeScript",
            &["ts", "tsx"],
        )));

        assert!(manager.plugin_for_extension("ts").is_some());
        assert!(manager.plugin_for_extension("tsx").is_some());

        let plugin_ts = manager.plugin_for_extension("ts").unwrap();
        let plugin_tsx = manager.plugin_for_extension("tsx").unwrap();

        // Both extensions should return the same plugin
        assert_eq!(plugin_ts.metadata().id, "typescript");
        assert_eq!(plugin_tsx.metadata().id, "typescript");
    }

    #[test]
    fn test_multiple_plugins() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));
        manager.register_builtin(Box::new(MockPlugin::new(
            "javascript",
            "JavaScript",
            &["js"],
        )));

        assert_eq!(manager.plugins().len(), 2);
        assert!(manager.plugin_for_extension("rs").is_some());
        assert!(manager.plugin_for_extension("js").is_some());
    }

    #[test]
    fn test_with_plugins() {
        let plugins: Vec<Box<dyn LanguagePlugin>> = vec![
            Box::new(MockPlugin::new("rust", "Rust", &["rs"])),
            Box::new(MockPlugin::new("javascript", "JavaScript", &["js"])),
        ];

        let manager = PluginManager::with_plugins(plugins);

        assert_eq!(manager.plugins().len(), 2);
        assert!(manager.plugin_for_extension("rs").is_some());
        assert!(manager.plugin_for_extension("js").is_some());
    }

    #[test]
    fn test_plugin_state_hash_consistent() {
        let manager1 = PluginManager::new();
        let manager2 = PluginManager::new();
        assert_eq!(manager1.plugin_state_hash(), manager2.plugin_state_hash());
    }

    #[test]
    fn test_plugin_state_hash_changes() {
        let mut manager = PluginManager::new();
        let hash_before = manager.plugin_state_hash();

        manager.register_builtin(Box::new(MockPlugin::new("rust", "Rust", &["rs"])));
        let hash_after = manager.plugin_state_hash();

        assert_ne!(hash_before, hash_after);
    }

    #[test]
    fn test_plugin_manager_is_send_sync() {
        // Compile-time verification that PluginManager is thread-safe
        // This test will fail to compile if PluginManager doesn't implement Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginManager>();
    }

    // Task 1.2: Enhanced factory method tests

    #[test]
    fn test_selective_loading() {
        // Tests with_plugins() with a subset of plugins
        let selective_plugins: Vec<Box<dyn LanguagePlugin>> = vec![Box::new(MockPlugin::new(
            "test-lang",
            "Test Language",
            &["test"],
        ))];

        let manager = PluginManager::with_plugins(selective_plugins);

        // Should have exactly 1 plugin
        assert_eq!(manager.plugins().len(), 1, "Expected exactly 1 plugin");

        // Should be able to find the test plugin
        assert!(
            manager.plugin_for_extension("test").is_some(),
            "Test plugin should be available"
        );

        // Should NOT have other plugins
        assert!(
            manager.plugin_for_extension("rs").is_none(),
            "Rust plugin should not be loaded (selective loading)"
        );
        assert!(
            manager.plugin_for_extension("js").is_none(),
            "JavaScript plugin should not be loaded (selective loading)"
        );
    }

    #[test]
    fn test_empty_has_no_plugins() {
        // Validates empty() creates isolated manager with no plugins
        let manager = PluginManager::empty();

        assert_eq!(
            manager.plugins().len(),
            0,
            "Empty manager should have 0 plugins"
        );

        // Should not find any plugins
        assert!(
            manager.plugin_for_extension("rs").is_none(),
            "Empty manager should not have Rust plugin"
        );
        assert!(
            manager.plugin_by_id("rust").is_none(),
            "Empty manager should not have Rust plugin by ID"
        );

        // Verify can manually register after creation
        let mut mutable_manager = PluginManager::empty();
        mutable_manager.register_builtin(Box::new(MockPlugin::new("test", "Test", &["test"])));

        assert_eq!(
            mutable_manager.plugins().len(),
            1,
            "After manual registration, should have 1 plugin"
        );
    }
}
