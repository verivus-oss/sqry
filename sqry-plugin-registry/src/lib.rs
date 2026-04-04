//! Shared plugin registry for SQRY.
//!
//! This crate is the single source of truth for built-in plugin registration,
//! plugin cost-tier metadata, and deterministic plugin-selection resolution.

use std::collections::BTreeSet;

use sqry_core::plugin::PluginManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginCostTier {
    Fast,
    HighWallClock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum HighCostMode {
    #[default]
    FastPathDefault,
    IncludeAll,
    ExcludeAll,
}

impl HighCostMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FastPathDefault => "fast_path_default",
            Self::IncludeAll => "include_all",
            Self::ExcludeAll => "exclude_all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PluginSelectionConfig {
    pub high_cost_mode: HighCostMode,
    pub enable_plugins: BTreeSet<String>,
    pub disable_plugins: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSelectionResolution {
    pub high_cost_mode: HighCostMode,
    pub active_plugin_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSelectionError {
    UnknownPluginIds {
        ids: Vec<String>,
        supported_ids: Vec<String>,
    },
}

impl std::fmt::Display for PluginSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPluginIds { ids, supported_ids } => write!(
                f,
                "unknown plugin ids: {} (supported ids: {})",
                ids.join(", "),
                supported_ids.join(", ")
            ),
        }
    }
}

impl std::error::Error for PluginSelectionError {}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinPluginSpec {
    pub id: &'static str,
    pub cost_tier: PluginCostTier,
    pub register: fn(&mut PluginManager),
}

macro_rules! builtin_plugin_specs {
    ($([$id:literal, $tier:expr, $register:expr]),+ $(,)?) => {
        const BUILTIN_PLUGIN_SPECS: &[BuiltinPluginSpec] = &[
            $(
                BuiltinPluginSpec {
                    id: $id,
                    cost_tier: $tier,
                    register: $register,
                },
            )+
        ];
    };
}

builtin_plugin_specs!(
    ["c", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_c::CPlugin::default())
    )],
    ["cpp", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_cpp::CppPlugin::default())
    )],
    ["csharp", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_csharp::CSharpPlugin::default())
    )],
    ["css", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_css::CssPlugin::default())
    )],
    ["dart", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_dart::DartPlugin::default())
    )],
    ["elixir", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_elixir::ElixirPlugin::default())
    )],
    ["go", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_go::GoPlugin::default())
    )],
    ["groovy", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_groovy::GroovyPlugin::default())
    )],
    ["haskell", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_haskell::HaskellPlugin::default())
    )],
    ["html", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_html::HtmlPlugin::default())
    )],
    ["java", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_java::JavaPlugin::default())
    )],
    ["javascript", PluginCostTier::Fast, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_javascript::JavaScriptPlugin::default()
        ))],
    ["kotlin", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_kotlin::KotlinPlugin::default())
    )],
    ["lua", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_lua::LuaPlugin::default())
    )],
    ["perl", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_perl::PerlPlugin::default())
    )],
    ["php", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_php::PhpPlugin::default())
    )],
    ["python", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_python::PythonPlugin::default())
    )],
    ["r", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_r::RPlugin::default())
    )],
    ["ruby", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_ruby::RubyPlugin::default())
    )],
    ["rust", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_rust::RustPlugin::default())
    )],
    ["scala", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_scala::ScalaPlugin::default())
    )],
    ["shell", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_shell::ShellPlugin::default())
    )],
    ["sql", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_sql::SqlPlugin::default())
    )],
    ["svelte", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_svelte::SveltePlugin::default())
    )],
    ["swift", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_swift::SwiftPlugin::default())
    )],
    ["typescript", PluginCostTier::Fast, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_typescript::TypeScriptPlugin::default()
        ))],
    ["vue", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_vue::VuePlugin::default())
    )],
    ["zig", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_zig::ZigPlugin::default())
    )],
    ["plsql", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_oracle_plsql::OraclePlsqlPlugin::default())
    )],
    ["apex", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_salesforce_apex::SalesforceApexPlugin::default())
    )],
    ["abap", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_sap_abap::SapAbapPlugin::default())
    )],
    ["servicenow-xanadu-js", PluginCostTier::Fast, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin::default()
        ))],
    ["servicenow-xml", PluginCostTier::HighWallClock, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_servicenow_xml::ServiceNowXmlPlugin::default()
        ))],
    ["terraform", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_terraform::TerraformPlugin::default())
    )],
    ["puppet", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_puppet::PuppetPlugin::default())
    )],
    ["pulumi", PluginCostTier::Fast, |pm| pm.register_builtin(
        Box::new(sqry_lang_pulumi::PulumiPlugin::default())
    )],
    ["json", PluginCostTier::HighWallClock, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_json::JsonPlugin::new()
        ))]
);

#[must_use]
pub fn builtin_plugin_ids() -> Vec<String> {
    BUILTIN_PLUGIN_SPECS
        .iter()
        .map(|spec| spec.id.to_string())
        .collect()
}

/// Create a `PluginManager` using the default fast-path plugin selection.
#[must_use]
pub fn create_plugin_manager() -> PluginManager {
    let resolution = resolve_plugin_selection(&PluginSelectionConfig::default())
        .unwrap_or_else(|_| unreachable!("default plugin selection must resolve"));
    create_plugin_manager_for_plugin_ids(&resolution.active_plugin_ids)
        .unwrap_or_else(|_| unreachable!("default plugin ids must be valid"))
}

/// Create a `PluginManager` containing the full built-in plugin roster.
#[must_use]
pub fn create_plugin_manager_all() -> PluginManager {
    create_plugin_manager_with_config(&PluginSelectionConfig {
        high_cost_mode: HighCostMode::IncludeAll,
        ..PluginSelectionConfig::default()
    })
    .unwrap_or_else(|_| unreachable!("full built-in plugin roster must resolve"))
}

/// Create a `PluginManager` from an explicit selection config.
///
/// # Errors
///
/// Returns an error if the config references unknown built-in plugin ids.
pub fn create_plugin_manager_with_config(
    config: &PluginSelectionConfig,
) -> Result<PluginManager, PluginSelectionError> {
    let resolution = resolve_plugin_selection(config)?;
    create_plugin_manager_for_plugin_ids(&resolution.active_plugin_ids)
}

/// Create a `PluginManager` from an explicit ordered built-in plugin subset.
///
/// # Errors
///
/// Returns an error if any requested built-in plugin id is unknown.
pub fn create_plugin_manager_for_plugin_ids(
    plugin_ids: &[String],
) -> Result<PluginManager, PluginSelectionError> {
    validate_plugin_ids(plugin_ids.iter().map(String::as_str))?;

    let selected_ids: BTreeSet<&str> = plugin_ids.iter().map(String::as_str).collect();
    let mut pm = PluginManager::new();

    for spec in BUILTIN_PLUGIN_SPECS {
        if selected_ids.contains(spec.id) {
            (spec.register)(&mut pm);
        }
    }

    Ok(pm)
}

/// Resolve the effective active plugin ids for a selection config.
///
/// # Errors
///
/// Returns an error if the config references unknown built-in plugin ids.
pub fn resolve_plugin_selection(
    config: &PluginSelectionConfig,
) -> Result<PluginSelectionResolution, PluginSelectionError> {
    validate_plugin_ids(
        config
            .enable_plugins
            .iter()
            .map(String::as_str)
            .chain(config.disable_plugins.iter().map(String::as_str)),
    )?;

    let active_plugin_ids = BUILTIN_PLUGIN_SPECS
        .iter()
        .filter(|spec| is_plugin_enabled(spec, config))
        .map(|spec| spec.id.to_string())
        .collect();

    Ok(PluginSelectionResolution {
        high_cost_mode: config.high_cost_mode,
        active_plugin_ids,
    })
}

fn is_plugin_enabled(spec: &BuiltinPluginSpec, config: &PluginSelectionConfig) -> bool {
    if config.disable_plugins.contains(spec.id) {
        return false;
    }
    if config.enable_plugins.contains(spec.id) {
        return true;
    }

    match config.high_cost_mode {
        HighCostMode::IncludeAll => true,
        HighCostMode::FastPathDefault | HighCostMode::ExcludeAll => {
            matches!(spec.cost_tier, PluginCostTier::Fast)
        }
    }
}

fn validate_plugin_ids<'a>(
    plugin_ids: impl IntoIterator<Item = &'a str>,
) -> Result<(), PluginSelectionError> {
    let supported_ids: BTreeSet<&str> = BUILTIN_PLUGIN_SPECS.iter().map(|spec| spec.id).collect();
    let mut unknown_ids = plugin_ids
        .into_iter()
        .filter(|id| !supported_ids.contains(id))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    unknown_ids.sort();
    unknown_ids.dedup();

    if unknown_ids.is_empty() {
        return Ok(());
    }

    Err(PluginSelectionError::UnknownPluginIds {
        ids: unknown_ids,
        supported_ids: supported_ids.into_iter().map(ToString::to_string).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_mode(high_cost_mode: HighCostMode) -> PluginSelectionConfig {
        PluginSelectionConfig {
            high_cost_mode,
            ..PluginSelectionConfig::default()
        }
    }

    #[test]
    fn test_default_fast_path_excludes_high_cost_plugins() {
        let pm = create_plugin_manager();
        let plugins = pm.plugins();

        assert_eq!(
            plugins.len(),
            35,
            "default fast path should exclude 2 plugins"
        );
        assert!(pm.plugin_by_id("json").is_none());
        assert!(pm.plugin_by_id("servicenow-xml").is_none());
    }

    #[test]
    fn test_create_plugin_manager_has_rust() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("rs").is_some());
        assert!(pm.plugin_by_id("rust").is_some());
    }

    #[test]
    fn test_include_all_restores_high_cost_plugins() {
        let pm = create_plugin_manager_with_config(&config_with_mode(HighCostMode::IncludeAll))
            .expect("include-all config should be valid");

        assert!(pm.plugin_by_id("json").is_some());
        assert!(pm.plugin_by_id("servicenow-xml").is_some());
    }

    #[test]
    fn test_explicit_enable_beats_fast_path_default() {
        let mut config = PluginSelectionConfig::default();
        config.enable_plugins.insert("json".to_string());

        let resolution = resolve_plugin_selection(&config).expect("selection should resolve");
        assert!(resolution.active_plugin_ids.iter().any(|id| id == "json"));
    }

    #[test]
    fn test_explicit_disable_beats_explicit_enable() {
        let mut config = config_with_mode(HighCostMode::IncludeAll);
        config.enable_plugins.insert("json".to_string());
        config.disable_plugins.insert("json".to_string());

        let resolution = resolve_plugin_selection(&config).expect("selection should resolve");
        assert!(!resolution.active_plugin_ids.iter().any(|id| id == "json"));
    }

    #[test]
    fn test_unknown_plugin_ids_fail_validation() {
        let mut config = PluginSelectionConfig::default();
        config.enable_plugins.insert("missing-plugin".to_string());

        let err = resolve_plugin_selection(&config).expect_err("selection should fail");
        assert!(
            matches!(err, PluginSelectionError::UnknownPluginIds { .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_spec_id_matches_registered_plugin_id() {
        for spec in BUILTIN_PLUGIN_SPECS {
            let mut pm = PluginManager::new();
            (spec.register)(&mut pm);

            let plugin = pm
                .plugin_by_id(spec.id)
                .unwrap_or_else(|| panic!("registry spec {} did not register by id", spec.id));
            assert_eq!(plugin.metadata().id, spec.id);
        }
    }
}
