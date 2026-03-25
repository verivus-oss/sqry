//! Test helper module for creating `PluginManager` with built-in plugins.
//!
//! This module provides factory methods for creating `PluginManager` instances
//! pre-loaded with all built-in language plugins. These are only available
//! in integration tests where plugins are accessible as dev-dependencies.

use sqry_core::plugin::{PluginError, PluginManager};

/// Create a `PluginManager` with all built-in plugins (test helper).
///
/// Loads all statically-linked language plugins in deterministic order.
///
/// # Panics
///
/// Panics if plugin initialization fails.
///
/// # Example
///
/// ```ignore
/// use sqry_core_tests::plugin_factory_helpers::with_builtin_plugins;
///
/// let manager = with_builtin_plugins();
/// assert!(manager.plugin_for_extension("rs").is_some());
/// ```
#[must_use]
pub fn with_builtin_plugins() -> PluginManager {
    try_with_builtin_plugins().expect(
        "Failed to initialize built-in plugins - this indicates a plugin implementation bug",
    )
}

/// Create a `PluginManager` with all built-in plugins, fallibly (test helper).
///
/// Attempts to load all statically-linked language plugins. Returns an error
/// if any plugin fails to initialize.
///
/// # Errors
///
/// Returns [`PluginError::InvalidPlugin`] if any built-in plugin fails to
/// initialize.
///
/// # Example
///
/// ```ignore
/// use sqry_core_tests::plugin_factory_helpers::try_with_builtin_plugins;
///
/// match try_with_builtin_plugins() {
///     Ok(manager) => println!("Loaded {} plugins", manager.plugins().len()),
///     Err(e) => eprintln!("Plugin initialization failed: {}", e),
/// }
/// ```
pub fn try_with_builtin_plugins() -> Result<PluginManager, PluginError> {
    // Deterministic built-in plugin roster (alphabetical within tiers)
    // Note: Dart, Oracle PL/SQL, Puppet, and Shell excluded per language plugin audits
    // (SQRY_LANG_*_AUDIT_20251121.md - marked as NOT READY)

    let mut manager = PluginManager::new();

    // Tier 1: Core languages (alphabetical order for determinism)
    manager.register_builtin(Box::new(sqry_lang_go::GoPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_java::JavaPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_swift::SwiftPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));

    // Tier 2: Additional languages (alphabetical order)
    manager.register_builtin(Box::new(sqry_lang_kotlin::KotlinPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_lua::LuaPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_php::PhpPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_r::RPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_ruby::RubyPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_scala::ScalaPlugin::default()));

    // Validate expected plugin count (13 plugins loaded)
    let expected_count = 13;
    let actual_count = manager.plugins().len();
    if actual_count != expected_count {
        return Err(PluginError::InvalidPlugin(format!(
            "Plugin roster mismatch: expected {expected_count} plugins, got {actual_count}"
        )));
    }

    Ok(manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_roster_integrity() {
        // Validates deterministic plugin loading and count
        let result = try_with_builtin_plugins();

        assert!(
            result.is_ok(),
            "Built-in plugin roster should load successfully"
        );

        let manager = result.unwrap();
        assert_eq!(
            manager.plugins().len(),
            13,
            "Expected exactly 13 built-in plugins (Tier 1 + Tier 2, excluding NOT READY plugins)"
        );

        // Spot-check: Verify key plugins are loaded (alphabetical check)
        let loaded_ids: Vec<String> = manager
            .plugins()
            .iter()
            .map(|p| p.metadata().id.to_string())
            .collect();

        for id in &["go", "java", "javascript", "python", "rust", "typescript"] {
            assert!(
                loaded_ids.contains(&(*id).to_string()),
                "Expected {id} plugin to be loaded"
            );
        }
    }

    #[test]
    fn test_fallible_factory() {
        // Tests try_with_builtin_plugins() returns Ok and plugins work
        let result = try_with_builtin_plugins();

        assert!(result.is_ok(), "try_with_builtin_plugins() should succeed");

        let manager = result.unwrap();

        // Verify plugins are actually loaded and functional
        assert!(
            !manager.plugins().is_empty(),
            "Manager should have plugins loaded"
        );
        assert_eq!(
            manager.plugins().len(),
            13,
            "Expected 13 plugins to be loaded"
        );

        // Spot-check: Rust plugin should work
        let rust_plugin = manager.plugin_for_extension("rs");
        assert!(rust_plugin.is_some(), "Rust plugin should be available");

        let rust_meta = rust_plugin.unwrap().metadata();
        assert_eq!(rust_meta.id, "rust", "Rust plugin ID should be 'rust'");
    }

    #[test]
    fn test_with_builtin_plugins_succeeds_normally() {
        // Validates convenience wrapper works in normal conditions (non-panicking)
        // If this test fails with a panic, it indicates a plugin implementation bug

        let manager = with_builtin_plugins();

        assert!(
            !manager.plugins().is_empty(),
            "Manager should have plugins loaded"
        );
        assert_eq!(
            manager.plugins().len(),
            13,
            "Expected 13 plugins (same as try_ variant)"
        );

        // Verify plugins are functional
        assert!(
            manager.plugin_for_extension("rs").is_some(),
            "Rust plugin should be available"
        );
        assert!(
            manager.plugin_by_id("javascript").is_some(),
            "JavaScript plugin should be available by ID"
        );
    }
}
