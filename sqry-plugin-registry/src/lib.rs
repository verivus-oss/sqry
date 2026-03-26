//! Shared plugin registry for SQRY
//!
//! This module provides a centralized function to create a `PluginManager` with all
//! built-in language plugins registered. This is the single source of truth for
//! the supported plugin roster, used by CLI, index, and other consumers.
//!
//! # Auto-registration of Built-in Plugins
//!
//! This module centralizes the registration of all 35 language plugins to ensure
//! consistency across the entire ecosystem (CLI, LSP, MCP, etc.).

use sqry_core::plugin::PluginManager;

/// Create a `PluginManager` with all built-in language plugins registered.
///
/// This function registers all supported language plugins in a consistent order.
///
/// # Returns
///
/// A fully-initialized `PluginManager` ready for symbol extraction.
///
/// # Example
///
/// ```
/// use sqry_plugin_registry::create_plugin_manager;
///
/// let plugin_manager = create_plugin_manager();
/// assert!(plugin_manager.plugin_for_extension("rs").is_some());
/// ```
///
/// # Plugin Registration Order
///
/// Plugins are registered in deterministic order:
/// - General-purpose languages (alphabetical by language ID; 28 total)
/// - Domain-specific languages (ordered as listed; 4 total: Oracle PL/SQL, Salesforce Apex, SAP ABAP, `ServiceNow` Xanadu)
/// - `IaC` plugins (3 total: terraform, puppet, pulumi)
#[must_use]
pub fn create_plugin_manager() -> PluginManager {
    let mut pm = PluginManager::new();

    // Tier 1 languages (28 languages with full call/import/export support)
    pm.register_builtin(Box::new(sqry_lang_c::CPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_cpp::CppPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_csharp::CSharpPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_css::CssPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_dart::DartPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_elixir::ElixirPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_go::GoPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_groovy::GroovyPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_haskell::HaskellPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_html::HtmlPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_java::JavaPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_kotlin::KotlinPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_lua::LuaPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_perl::PerlPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_php::PhpPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_r::RPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_ruby::RubyPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_scala::ScalaPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_shell::ShellPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_sql::SqlPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_svelte::SveltePlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_swift::SwiftPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_vue::VuePlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_zig::ZigPlugin::default()));

    // Domain-specific plugins
    pm.register_builtin(Box::new(
        sqry_lang_oracle_plsql::OraclePlsqlPlugin::default(),
    ));
    pm.register_builtin(Box::new(
        sqry_lang_salesforce_apex::SalesforceApexPlugin::default(),
    ));
    pm.register_builtin(Box::new(sqry_lang_sap_abap::SapAbapPlugin::default()));
    pm.register_builtin(Box::new(
        sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin::default(),
    ));

    // IaC plugins (formerly feature-gated, now always included)
    pm.register_builtin(Box::new(sqry_lang_terraform::TerraformPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_puppet::PuppetPlugin::default()));
    pm.register_builtin(Box::new(sqry_lang_pulumi::PulumiPlugin::default()));

    // Config file plugins
    pm.register_builtin(Box::new(sqry_lang_json::JsonPlugin::new()));

    pm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_plugin_manager_has_all_plugins() {
        let pm = create_plugin_manager();
        let plugins = pm.plugins();

        // Should have 36 plugins (28 general-purpose + 4 domain-specific + 3 IaC + 1 config)
        assert_eq!(
            plugins.len(),
            36,
            "Expected 36 plugins, got {}",
            plugins.len()
        );
    }

    #[test]
    fn test_create_plugin_manager_has_rust() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("rs").is_some());
        assert!(pm.plugin_by_id("rust").is_some());
    }

    #[test]
    fn test_create_plugin_manager_has_javascript() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("js").is_some());
        assert!(pm.plugin_by_id("javascript").is_some());
    }

    #[test]
    fn test_create_plugin_manager_has_python() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("py").is_some());
        assert!(pm.plugin_by_id("python").is_some());
    }

    #[test]
    fn test_create_plugin_manager_has_elixir() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("ex").is_some());
        assert!(pm.plugin_by_id("elixir").is_some());
    }

    #[test]
    fn test_create_plugin_manager_has_sql() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("sql").is_some());
        assert!(pm.plugin_by_id("sql").is_some());
    }

    #[test]
    fn test_create_plugin_manager_has_zig() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("zig").is_some());
        assert!(pm.plugin_by_id("zig").is_some());
    }
}
