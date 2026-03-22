//! Default plugin registration for SQRY
//!
//! This module now delegates to `sqry_plugin_registry` to ensure a single source of truth.

use sqry_core::plugin::PluginManager;

/// Create a `PluginManager` with all built-in language plugins registered.
#[must_use]
pub fn create_plugin_manager() -> PluginManager {
    sqry_plugin_registry::create_plugin_manager()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_plugin_manager_delegates() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("rs").is_some());
    }
}
