//! Test helper module for creating `PluginManager` with built-in plugins.
//!
//! This module provides factory methods for creating `PluginManager` instances.
//! **NOTE**: Plugin crates are `[dev-dependencies]` and only available in
//! integration tests, not unit tests. Unit tests that need plugins should be
//! moved to `tests/` or use mock plugins.

use crate::plugin::PluginManager;

/// Create a `PluginManager` for tests.
///
/// Returns an empty `PluginManager`. Actual plugin loading happens in integration
/// tests where dev-dependencies are available.
///
/// **For integration tests**: Use the helper in `tests/plugin_factory_helpers.rs`
/// which has access to all language plugin crates.
///
/// **For unit tests**: This returns an empty manager. If your test needs plugins,
/// either move it to `tests/` or mock the plugin functionality.
///
/// # Example
///
/// ```ignore
/// use sqry_core::test_support::plugin_factory::with_builtin_plugins;
///
/// let manager = with_builtin_plugins();
/// // Returns empty manager - plugins not available in src/ unit tests
/// ```
#[must_use]
pub fn with_builtin_plugins() -> PluginManager {
    // Language plugin crates are dev-dependencies, only available in integration tests
    // Unit tests in src/ cannot load them due to circular dependencies
    // See: sqry-core <- relations-shared <- sqry-lang-rust
    PluginManager::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_with_builtin_plugins_returns_manager() {
        // In unit test context, returns empty PluginManager
        // (plugins only available in integration tests)
        let manager = with_builtin_plugins();

        // Should return a valid manager (may be empty in unit test context)
        assert_eq!(
            manager.plugins().len(),
            0,
            "Unit tests return empty manager (plugins only in integration tests)"
        );
    }
}
