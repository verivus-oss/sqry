//! Shared plugin registry for SQRY.
//!
//! This crate is the single source of truth for built-in plugin registration,
//! plugin cost-tier metadata, and deterministic plugin-selection resolution.

use std::collections::BTreeSet;
use std::path::PathBuf;

use sqry_core::plugin::PluginManager;

pub mod feature_table;
pub use feature_table::{
    PLUGIN_FEATURE_TABLE, PluginFeatureSpec, all_unknown_ids_have_features, missing_features_for,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginCostTier {
    Fast,
    HighWallClock,
    Optional,
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
#[non_exhaustive]
pub enum PluginSelectionError {
    /// Legacy variant — retained for downstream consumers (LSP, MCP)
    /// during the migration window. New call sites should use
    /// [`Self::UnknownPluginIdsCtx`] for richer diagnostics.
    #[deprecated(note = "use UnknownPluginIdsCtx for richer diagnostics (cluster-E §E.2)")]
    UnknownPluginIds {
        ids: Vec<String>,
        supported_ids: Vec<String>,
    },
    /// Manifest references plugin ids the runtime does not know,
    /// enriched with the manifest path and the Cargo feature flags
    /// that, if enabled, would register them (cluster-E §E.2).
    UnknownPluginIdsCtx {
        /// Unknown plugin ids in the order they appeared in the manifest.
        ids: Vec<String>,
        /// Plugin ids this binary recognises.
        supported_ids: Vec<String>,
        /// Absolute path to the manifest that produced the unknown
        /// ids. `None` only on the synthetic in-memory manifest path
        /// used by unit tests.
        manifest_path: Option<PathBuf>,
        /// Cargo feature flags that, if enabled at build time, would
        /// register the unknown plugin ids. May be empty if the
        /// unknown ids are not gated by any feature flag (i.e.
        /// genuinely unknown / typo).
        suggested_features: Vec<&'static str>,
        /// `true` iff every unknown id has a corresponding feature
        /// flag. Drives the "rebuild with --features" suggestion vs
        /// the "rebuild the index" suggestion.
        all_unknown_ids_have_features: bool,
    },
}

impl std::fmt::Display for PluginSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[allow(deprecated)]
            Self::UnknownPluginIds { ids, supported_ids } => write!(
                f,
                "unknown plugin ids: {} (supported ids: {})",
                ids.join(", "),
                supported_ids.join(", ")
            ),
            Self::UnknownPluginIdsCtx {
                ids,
                supported_ids,
                manifest_path,
                suggested_features,
                all_unknown_ids_have_features,
            } => {
                writeln!(
                    f,
                    "unknown plugin ids: {} (this binary supports: {})",
                    ids.join(", "),
                    supported_ids.join(", ")
                )?;
                if let Some(p) = manifest_path {
                    writeln!(f, "  manifest: {}", p.display())?;
                }
                if !suggested_features.is_empty() {
                    writeln!(
                        f,
                        "  rebuild this binary with: cargo install --path sqry-cli --features {}",
                        suggested_features.join(",")
                    )?;
                }
                if *all_unknown_ids_have_features {
                    write!(
                        f,
                        "  …or rebuild the index with the binary that produced it: \
                         sqry index --force <workspace-root>"
                    )
                } else {
                    write!(
                        f,
                        "  the unknown ids do not match any known feature flag — \
                         the manifest may be from a newer sqry version. Rebuild \
                         the index: sqry index --force <workspace-root>"
                    )
                }
            }
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
    ($($(#[$meta:meta])* [$id:literal, $tier:expr, $register:expr]),+ $(,)?) => {
        const BUILTIN_PLUGIN_SPECS: &[BuiltinPluginSpec] = &[
            $(
                $(#[$meta])*
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
    #[cfg(feature = "plugin-apex")]
    ["apex", PluginCostTier::Optional, |pm| pm.register_builtin(
        Box::new(sqry_lang_salesforce_apex::SalesforceApexPlugin::default())
    )],
    #[cfg(feature = "plugin-abap")]
    ["abap", PluginCostTier::Optional, |pm| pm.register_builtin(
        Box::new(sqry_lang_sap_abap::SapAbapPlugin::default())
    )],
    #[cfg(feature = "plugin-servicenow-xanadu")]
    ["servicenow-xanadu-js", PluginCostTier::Optional, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_servicenow_xanadu::ServiceNowXanaduPlugin::default()
        ))],
    #[cfg(feature = "plugin-servicenow-xml")]
    ["servicenow-xml", PluginCostTier::Optional, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_servicenow_xml::ServiceNowXmlPlugin::default()
        ))],
    #[cfg(feature = "plugin-terraform")]
    ["terraform", PluginCostTier::Optional, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_terraform::TerraformPlugin::default()
        ))],
    #[cfg(feature = "plugin-puppet")]
    ["puppet", PluginCostTier::Optional, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_puppet::PuppetPlugin::default()
        ))],
    #[cfg(feature = "plugin-pulumi")]
    ["pulumi", PluginCostTier::Optional, |pm| pm
        .register_builtin(Box::new(
            sqry_lang_pulumi::PulumiPlugin::default()
        ))],
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

    let suggested_features = missing_features_for(&unknown_ids);
    let all_have_features = all_unknown_ids_have_features(&unknown_ids);
    Err(PluginSelectionError::UnknownPluginIdsCtx {
        ids: unknown_ids,
        supported_ids: supported_ids.into_iter().map(ToString::to_string).collect(),
        manifest_path: None,
        suggested_features,
        all_unknown_ids_have_features: all_have_features,
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
        let expected_len = BUILTIN_PLUGIN_SPECS
            .iter()
            .filter(|spec| matches!(spec.cost_tier, PluginCostTier::Fast))
            .count();

        assert_eq!(plugins.len(), expected_len);
        assert!(pm.plugin_by_id("json").is_none());
        #[cfg(feature = "plugin-servicenow-xml")]
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
        #[cfg(feature = "plugin-servicenow-xml")]
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
            matches!(err, PluginSelectionError::UnknownPluginIdsCtx { .. }),
            "unexpected error: {err}"
        );
    }

    /// `Display` for `UnknownPluginIdsCtx` includes the manifest path and
    /// the `--features` suggestion when the unknown ids correspond to
    /// known feature gates (cluster-E §E.2). Pinning this output is the
    /// load-bearing assertion for the user-facing diagnostic.
    #[test]
    fn display_includes_manifest_and_feature_hint() {
        // Build the variant directly so the test is independent of the
        // current binary's compile-time feature set.
        let err = PluginSelectionError::UnknownPluginIdsCtx {
            ids: vec!["terraform".to_string(), "made-up".to_string()],
            supported_ids: vec!["rust".to_string(), "go".to_string()],
            manifest_path: Some(PathBuf::from("/repo/proj/.sqry/graph/manifest.json")),
            suggested_features: vec!["plugin-terraform"],
            all_unknown_ids_have_features: false,
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("terraform"),
            "must list every unknown id, got: {rendered}"
        );
        assert!(
            rendered.contains("/repo/proj/.sqry/graph/manifest.json"),
            "must include the manifest path, got: {rendered}"
        );
        assert!(
            rendered.contains("--features plugin-terraform"),
            "must include the rebuild-with-features suggestion, got: {rendered}"
        );
        assert!(
            rendered.contains("Rebuild the index"),
            "must include the rebuild-the-index suggestion when not all ids have features, \
             got: {rendered}"
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
