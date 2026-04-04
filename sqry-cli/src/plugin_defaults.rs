//! Plugin-selection helpers for CLI entry points.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::plugin::PluginManager;
use sqry_plugin_registry::{
    HighCostMode, PluginSelectionConfig, PluginSelectionResolution,
    create_plugin_manager_for_plugin_ids,
    resolve_plugin_selection as resolve_registry_plugin_selection,
};

use crate::args::{Cli, PluginSelectionArgs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPluginSelection {
    pub active_plugin_ids: Vec<String>,
    pub high_cost_mode: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSelectionMode {
    FreshWrite,
    ExistingWrite,
    ReadOnly,
    Diff,
}

pub struct ResolvedPluginManager {
    pub plugin_manager: PluginManager,
    pub persisted_selection: Option<PluginSelectionManifest>,
}

#[must_use]
pub fn create_plugin_manager() -> PluginManager {
    sqry_plugin_registry::create_plugin_manager()
}

/// Resolve the effective plugin manager and persisted selection metadata.
///
/// # Errors
///
/// Returns an error if plugin overrides are invalid, a persisted manifest cannot
/// be loaded safely, or a read-only command would reinterpret an indexed workspace.
pub fn resolve_plugin_selection(
    cli: &Cli,
    root: &Path,
    mode: PluginSelectionMode,
) -> Result<ResolvedPluginManager> {
    let selection_args = cli.plugin_selection_args();
    let selection = match mode {
        PluginSelectionMode::FreshWrite => resolve_index_selection(&selection_args)?,
        PluginSelectionMode::ExistingWrite => resolve_update_selection(root, &selection_args)?,
        PluginSelectionMode::ReadOnly => resolve_read_only_selection(root, &selection_args)?,
        PluginSelectionMode::Diff => {
            if resolve_explicit_selection(&selection_args)?.is_some() {
                bail!(
                    "plugin-selection overrides are not allowed for `sqry diff`; rebuild or update the indexed workspace with the desired plugins first"
                );
            }
            resolve_read_only_selection(root, &PluginSelectionArgs::default())?
        }
    };
    let plugin_manager = create_manager_from_selection(&selection)?;
    let persisted_selection = Some(PluginSelectionManifest {
        active_plugin_ids: selection.active_plugin_ids,
        high_cost_mode: selection.high_cost_mode,
    });

    Ok(ResolvedPluginManager {
        plugin_manager,
        persisted_selection,
    })
}

/// Resolve the plugin selection for `sqry index`.
///
/// # Errors
///
/// Returns an error if CLI or environment overrides reference unknown plugins.
pub fn resolve_index_selection(args: &PluginSelectionArgs) -> Result<ResolvedPluginSelection> {
    resolve_explicit_selection(args)?.map_or_else(resolve_default_fast_path_selection, Ok)
}

/// Resolve the plugin selection for `sqry update`.
///
/// # Errors
///
/// Returns an error if manifest loading fails or plugin overrides are invalid.
pub fn resolve_update_selection(
    root: &Path,
    args: &PluginSelectionArgs,
) -> Result<ResolvedPluginSelection> {
    resolve_explicit_selection(args)?
        .map_or_else(|| resolve_persisted_or_legacy_selection(root), Ok)
}

fn resolve_default_fast_path_selection() -> Result<ResolvedPluginSelection> {
    resolved_from_registry_config(&PluginSelectionConfig::default())
}

fn resolve_read_only_selection(
    root: &Path,
    args: &PluginSelectionArgs,
) -> Result<ResolvedPluginSelection> {
    let storage = GraphStorage::new(root);
    if storage.exists() {
        let persisted_selection = resolve_persisted_or_legacy_selection(root)?;
        if let Some(explicit_selection) = resolve_explicit_selection(args)?
            && explicit_selection.active_plugin_ids != persisted_selection.active_plugin_ids
        {
            bail!(
                "plugin-selection overrides conflict with the persisted index selection; check CLI flags and SQRY_* plugin-selection environment variables, then rebuild the index if you want a new plugin set"
            );
        }
        return Ok(persisted_selection);
    }

    resolve_explicit_selection(args)?.map_or_else(resolve_default_fast_path_selection, Ok)
}

fn resolve_persisted_or_legacy_selection(root: &Path) -> Result<ResolvedPluginSelection> {
    let storage = GraphStorage::new(root);
    let manifest = storage.load_manifest().with_context(|| {
        format!(
            "failed to load manifest for plugin selection at {}",
            storage.manifest_path().display()
        )
    })?;

    if let Some(plugin_selection) = manifest.plugin_selection {
        return Ok(ResolvedPluginSelection {
            active_plugin_ids: plugin_selection.active_plugin_ids,
            high_cost_mode: plugin_selection.high_cost_mode,
        });
    }

    Ok(ResolvedPluginSelection {
        active_plugin_ids: sqry_plugin_registry::builtin_plugin_ids(),
        high_cost_mode: Some(HighCostMode::IncludeAll.as_str().to_string()),
    })
}

fn create_manager_from_selection(selection: &ResolvedPluginSelection) -> Result<PluginManager> {
    create_plugin_manager_for_plugin_ids(&selection.active_plugin_ids)
        .with_context(|| "failed to create plugin manager from resolved selection".to_string())
}

fn resolve_explicit_selection(
    args: &PluginSelectionArgs,
) -> Result<Option<ResolvedPluginSelection>> {
    let env_selection = EnvPluginSelection::from_env()?;
    if !args.include_high_cost
        && !args.exclude_high_cost
        && args.enable_plugins.is_empty()
        && args.disable_plugins.is_empty()
        && !env_selection.is_explicit()
    {
        return Ok(None);
    }

    let high_cost_mode = if args.include_high_cost {
        HighCostMode::IncludeAll
    } else if args.exclude_high_cost {
        HighCostMode::ExcludeAll
    } else if env_selection.include_high_cost {
        HighCostMode::IncludeAll
    } else if env_selection.exclude_high_cost {
        HighCostMode::ExcludeAll
    } else {
        HighCostMode::FastPathDefault
    };

    let mut enable_plugins = env_selection.enable_plugins;
    enable_plugins.extend(args.enable_plugins.clone());
    let mut disable_plugins = env_selection.disable_plugins;
    disable_plugins.extend(args.disable_plugins.clone());

    resolved_from_registry_config(&PluginSelectionConfig {
        high_cost_mode,
        enable_plugins: enable_plugins.into_iter().collect::<BTreeSet<_>>(),
        disable_plugins: disable_plugins.into_iter().collect::<BTreeSet<_>>(),
    })
    .map(Some)
}

fn resolved_from_registry_config(
    config: &PluginSelectionConfig,
) -> Result<ResolvedPluginSelection> {
    let PluginSelectionResolution {
        high_cost_mode,
        active_plugin_ids,
    } = resolve_registry_plugin_selection(config)
        .with_context(|| "failed to resolve plugin selection configuration".to_string())?;

    Ok(ResolvedPluginSelection {
        active_plugin_ids,
        high_cost_mode: Some(high_cost_mode.as_str().to_string()),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EnvPluginSelection {
    include_high_cost: bool,
    exclude_high_cost: bool,
    enable_plugins: Vec<String>,
    disable_plugins: Vec<String>,
}

impl EnvPluginSelection {
    fn from_env() -> Result<Self> {
        let include_high_cost = parse_env_bool("SQRY_INCLUDE_HIGH_COST")?;
        let exclude_high_cost = parse_env_bool("SQRY_EXCLUDE_HIGH_COST")?;
        if include_high_cost && exclude_high_cost {
            bail!("SQRY_INCLUDE_HIGH_COST and SQRY_EXCLUDE_HIGH_COST cannot both be enabled");
        }

        Ok(Self {
            include_high_cost,
            exclude_high_cost,
            enable_plugins: parse_env_plugin_list("SQRY_ENABLE_PLUGINS"),
            disable_plugins: parse_env_plugin_list("SQRY_DISABLE_PLUGINS"),
        })
    }

    fn is_explicit(&self) -> bool {
        self.include_high_cost
            || self.exclude_high_cost
            || !self.enable_plugins.is_empty()
            || !self.disable_plugins.is_empty()
    }
}

fn parse_env_bool(name: &str) -> Result<bool> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(false);
    };

    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        _ => bail!("{name} must be one of 0/1/false/true/no/yes/off/on"),
    }
}

fn parse_env_plugin_list(name: &str) -> Vec<String> {
    let Ok(raw) = std::env::var(name) else {
        return Vec::new();
    };

    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;
    use clap::Parser;
    use serial_test::serial;
    use sqry_core::graph::unified::persistence::{BuildProvenance, Manifest};
    use tempfile::TempDir;

    #[test]
    fn test_create_plugin_manager_delegates() {
        let pm = create_plugin_manager();
        assert!(pm.plugin_for_extension("rs").is_some());
    }

    #[test]
    #[serial]
    fn test_default_index_selection_excludes_json() {
        with_cleared_plugin_env(|| {
            let selection = resolve_index_selection(&PluginSelectionArgs::default())
                .expect("selection resolves");
            assert!(!selection.active_plugin_ids.iter().any(|id| id == "json"));
        });
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_cli_and_env_plugin_lists_are_merged() {
        with_cleared_plugin_env(|| {
            unsafe {
                std::env::set_var("SQRY_ENABLE_PLUGINS", "json");
                std::env::set_var("SQRY_DISABLE_PLUGINS", "shell");
            }

            let cli = Cli::parse_from([
                "sqry",
                "index",
                "--enable-plugin",
                "rust",
                "--disable-plugin",
                "sql",
            ]);

            let resolved =
                resolve_plugin_selection(&cli, Path::new("."), PluginSelectionMode::FreshWrite)
                    .expect("selection should resolve");
            let plugin_ids = resolved
                .persisted_selection
                .expect("persisted selection should exist")
                .active_plugin_ids;

            assert!(plugin_ids.iter().any(|id| id == "json"));
            assert!(plugin_ids.iter().any(|id| id == "rust"));
            assert!(!plugin_ids.iter().any(|id| id == "shell"));
            assert!(!plugin_ids.iter().any(|id| id == "sql"));
        });
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_read_only_selection_accepts_matching_explicit_selection() {
        with_cleared_plugin_env(|| {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let selection = resolve_index_selection(&PluginSelectionArgs::default())
                .expect("selection resolves");
            write_manifest_with_selection(temp_dir.path(), &selection);

            let cli = Cli::parse_from([
                "sqry",
                "query",
                "kind:function",
                temp_dir.path().to_str().expect("temp path should be utf-8"),
                "--disable-plugin",
                "json",
            ]);

            let resolved =
                resolve_plugin_selection(&cli, temp_dir.path(), PluginSelectionMode::ReadOnly)
                    .expect("matching explicit selection should be accepted");
            assert_eq!(
                resolved
                    .persisted_selection
                    .expect("persisted selection should be present")
                    .active_plugin_ids,
                selection.active_plugin_ids
            );
        });
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_read_only_selection_rejects_conflicting_explicit_selection() {
        with_cleared_plugin_env(|| {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let selection = resolve_index_selection(&PluginSelectionArgs::default())
                .expect("selection resolves");
            write_manifest_with_selection(temp_dir.path(), &selection);

            let cli = Cli::parse_from([
                "sqry",
                "query",
                "kind:function",
                temp_dir.path().to_str().expect("temp path should be utf-8"),
                "--include-high-cost",
            ]);

            let result =
                resolve_plugin_selection(&cli, temp_dir.path(), PluginSelectionMode::ReadOnly);
            assert!(
                result.is_err(),
                "conflicting explicit selection should be rejected"
            );
            let err = match result {
                Ok(_) => unreachable!("conflicting explicit selection should be rejected"),
                Err(err) => err,
            };
            assert!(err.to_string().contains("conflict"));
        });
    }
    }

    fn write_manifest_with_selection(root: &Path, selection: &ResolvedPluginSelection) {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir should exist");
        Manifest::new(
            root.to_string_lossy().to_string(),
            1,
            1,
            "fixture-sha256",
            BuildProvenance::new("test", "test"),
        )
        .with_plugin_selection(Some(PluginSelectionManifest {
            active_plugin_ids: selection.active_plugin_ids.clone(),
            high_cost_mode: selection.high_cost_mode.clone(),
        }))
        .save(storage.manifest_path())
        .expect("manifest should be written");
    }

    fn with_cleared_plugin_env(test_fn: impl FnOnce()) {
        let saved_values = [
            (
                "SQRY_INCLUDE_HIGH_COST",
                std::env::var("SQRY_INCLUDE_HIGH_COST").ok(),
            ),
            (
                "SQRY_EXCLUDE_HIGH_COST",
                std::env::var("SQRY_EXCLUDE_HIGH_COST").ok(),
            ),
            (
                "SQRY_ENABLE_PLUGINS",
                std::env::var("SQRY_ENABLE_PLUGINS").ok(),
            ),
            (
                "SQRY_DISABLE_PLUGINS",
                std::env::var("SQRY_DISABLE_PLUGINS").ok(),
            ),
        ];

        for (key, _) in &saved_values {
            unsafe {
                std::env::remove_var(key);
            }
        }

        test_fn();

        for (key, value) in saved_values {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}
