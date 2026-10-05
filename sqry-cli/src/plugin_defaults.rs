//! Plugin-selection helpers for CLI entry points.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::plugin::PluginManager;
use sqry_plugin_registry::{
    HighCostMode, PluginSelectionConfig, PluginSelectionError, PluginSelectionResolution,
    ResolvedWorkspaceSelection, RosterSource, create_plugin_manager_for_plugin_ids,
    resolve_plugin_selection as resolve_registry_plugin_selection,
};

use crate::args::{Cli, PluginSelectionArgs};

/// Where a resolved CLI plugin selection came from, so `sqry index` can
/// say why a plugin is on and so a fallback over an unreadable manifest is
/// never silent (surface parity W1 round 2, design D11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSelectionSource {
    /// CLI flags or `SQRY_*` variables named the selection.
    Explicit,
    /// The manifest at `manifest_path` records it (a persisted
    /// `plugin_selection` block, or the legacy every-builtin shape).
    RecordedInManifest { manifest_path: PathBuf },
    /// The root has no index; the fast-path default was used.
    Default,
    /// The manifest at `manifest_path` exists but cannot be read
    /// (`reason`), so the fast-path default was used and is being
    /// recorded in its place. Reachable only through `sqry index --force`
    /// or `--no-incremental`; the read-only and `sqry update` paths refuse.
    FallbackAfterUnreadableManifest {
        manifest_path: PathBuf,
        reason: String,
    },
}

impl PluginSelectionSource {
    /// One-line description for the `sqry index` banner.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Explicit => "explicit (CLI flags or SQRY_* variables)".to_string(),
            Self::RecordedInManifest { manifest_path } => {
                format!("recorded in manifest {}", manifest_path.display())
            }
            Self::Default => "default (fast path; no index at the root)".to_string(),
            Self::FallbackAfterUnreadableManifest { manifest_path, .. } => format!(
                "default (fast path; manifest {} cannot be read, fallback recorded)",
                manifest_path.display()
            ),
        }
    }

    /// The stderr warning `sqry index` prints when the manifest could not
    /// be read and the fallback is being recorded; `None` otherwise.
    #[must_use]
    pub fn unreadable_manifest_warning(&self) -> Option<String> {
        match self {
            Self::FallbackAfterUnreadableManifest {
                manifest_path,
                reason,
            } => Some(format!(
                "manifest {} cannot be read ({reason}); rebuilding with the fast-path default and recording that selection",
                manifest_path.display()
            )),
            Self::Explicit | Self::RecordedInManifest { .. } | Self::Default => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPluginSelection {
    pub active_plugin_ids: Vec<String>,
    pub high_cost_mode: Option<String>,
    /// Where the selection came from.
    pub source: PluginSelectionSource,
}

impl From<ResolvedWorkspaceSelection> for ResolvedPluginSelection {
    fn from(selection: ResolvedWorkspaceSelection) -> Self {
        let source = match (selection.source, selection.manifest_path) {
            (
                RosterSource::PersistedManifest | RosterSource::LegacyManifestWithoutSelection,
                Some(manifest_path),
            ) => PluginSelectionSource::RecordedInManifest { manifest_path },
            // A manifest-sourced selection always carries its path; the
            // fallback never does. Anything else is the fallback.
            (RosterSource::Fallback, _)
            | (
                RosterSource::PersistedManifest | RosterSource::LegacyManifestWithoutSelection,
                None,
            ) => PluginSelectionSource::Default,
        };
        Self {
            active_plugin_ids: selection.active_plugin_ids,
            high_cost_mode: selection.high_cost_mode,
            source,
        }
    }
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
    /// Where `persisted_selection` came from (design D11).
    pub selection_source: PluginSelectionSource,
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
    resolve_plugin_selection_from_args(&cli.plugin_selection_args(), root, mode)
}

/// [`resolve_plugin_selection`] from the selection flags alone, for a
/// caller that must resolve again later without the [`Cli`] (the auto-build
/// hook resolves again under the index's persist lock, decision D-i8-4).
///
/// # Errors
///
/// As [`resolve_plugin_selection`].
pub fn resolve_plugin_selection_from_args(
    selection_args: &PluginSelectionArgs,
    root: &Path,
    mode: PluginSelectionMode,
) -> Result<ResolvedPluginManager> {
    let selection_args = selection_args.clone();
    let selection = match mode {
        PluginSelectionMode::FreshWrite => resolve_index_selection(root, &selection_args)?,
        PluginSelectionMode::ExistingWrite => resolve_update_selection(root, &selection_args)?,
        PluginSelectionMode::ReadOnly => resolve_read_only_selection(root, &selection_args)?,
        PluginSelectionMode::Diff => resolve_diff_selection(root, &selection_args)?,
    };
    let plugin_manager = create_manager_from_selection(&selection)?;
    let persisted_selection = Some(PluginSelectionManifest {
        active_plugin_ids: selection.active_plugin_ids,
        high_cost_mode: selection.high_cost_mode,
    });

    Ok(ResolvedPluginManager {
        plugin_manager,
        persisted_selection,
        selection_source: selection.source,
    })
}

/// The selection `sqry diff` builds both refs with: the one the index at
/// `root` records, or the fast-path default when there is none. `diff`
/// compares what was built, so it takes no override: it has no plugin
/// flags, and `SQRY_*` variables that name a different plugin set are
/// refused, naming the plugins they add and drop. One that names the same
/// set overrides nothing and is accepted: an exported
/// `SQRY_INCLUDE_HIGH_COST=1` over an index built with it used to refuse
/// every diff, whatever the refs. The sets are compared as sets
/// ([`plugin_set_change`]), so a manifest listing its ids in another order,
/// or one id twice, records the same set. An unreadable manifest is refused
/// before the comparison, as on every read-only path.
///
/// # Errors
///
/// Returns an error if the overrides are invalid, the manifest cannot be
/// read or names an unknown plugin id, or an override names another plugin
/// set.
fn resolve_diff_selection(
    root: &Path,
    args: &PluginSelectionArgs,
) -> Result<ResolvedPluginSelection> {
    let explicit = resolve_explicit_selection(args)?;
    // Not `resolve_read_only_selection`: that one reads the `SQRY_*`
    // variables too, and with no index it would build with them.
    let selection = if GraphStorage::new(root).exists() {
        resolve_persisted_or_legacy_selection(root)?
    } else {
        resolve_default_fast_path_selection()?
    };
    if let Some(explicit) = explicit
        && let Some(change) =
            plugin_set_change(&explicit.active_plugin_ids, &selection.active_plugin_ids)
    {
        let (built_with, remedy) = match &selection.source {
            PluginSelectionSource::RecordedInManifest { manifest_path } => (
                format!(
                    "the plugin set the index records (manifest {})",
                    manifest_path.display()
                ),
                "rebuild or update the indexed workspace with the desired plugins first",
            ),
            _ => (
                "the fast-path default plugin set (there is no index at the root)".to_string(),
                "index the workspace with the desired plugins first",
            ),
        };
        bail!(
            "plugin-selection override refused: `sqry diff` builds both refs with {built_with}, \
             and the SQRY_* plugin-selection variables name another set: {change}. Variables \
             naming the same set are accepted; {remedy}"
        );
    }
    Ok(selection)
}

/// How the plugin set `override_ids` differs from `built_ids`, compared as
/// sets: `None` when they name the same plugins, whatever the order or a
/// repeated id, and otherwise the sentence naming the plugins the override
/// adds and drops (`it adds json and drops nothing`).
fn plugin_set_change(override_ids: &[String], built_ids: &[String]) -> Option<String> {
    let named: BTreeSet<&str> = override_ids.iter().map(String::as_str).collect();
    let built: BTreeSet<&str> = built_ids.iter().map(String::as_str).collect();
    if named == built {
        return None;
    }
    let list = |ids: Vec<&str>| {
        if ids.is_empty() {
            "nothing".to_string()
        } else {
            ids.join(", ")
        }
    };
    Some(format!(
        "it adds {} and drops {}",
        list(named.difference(&built).copied().collect()),
        list(built.difference(&named).copied().collect())
    ))
}

/// Classify the selection an existing index records without building
/// (surface parity W1 round 3, design D17): `run_index`'s early exit calls
/// this before it reports "Index already exists", so a manifest naming an
/// id this binary did not compile is refused by name and a manifest that
/// cannot be read is refused by file, with the read-only path's existing
/// text. The resolved manager is dropped.
///
/// Explicit selection flags and `SQRY_*` variables are not consulted: the
/// early exit ignored them before this change (nothing is built there) and
/// still does, so this reads only what the manifest records, through the
/// same registry resolver every read surface uses.
///
/// # Errors
///
/// Returns an error when the manifest cannot be read or names an unknown
/// plugin id.
pub fn classify_recorded_selection(root: &Path) -> Result<()> {
    let selection = resolve_persisted_or_legacy_selection(root)?;
    create_manager_from_selection(&selection).map(drop)
}

/// Resolve the read-only plugin manager used by query-like execution surfaces.
///
/// Session-backed CLI paths use this helper so their `SessionManager`
/// validates and deserializes persisted graphs with the same plugin roster as
/// normal `sqry query` execution.
///
/// # Errors
///
/// Returns an error if plugin overrides are invalid, a persisted manifest cannot
/// be loaded safely, or a read-only command would reinterpret an indexed workspace.
pub fn resolve_read_only_plugin_manager(cli: &Cli, root: &Path) -> Result<PluginManager> {
    Ok(resolve_plugin_selection(cli, root, PluginSelectionMode::ReadOnly)?.plugin_manager)
}

/// Resolve the plugin selection for `sqry index` (surface parity W1 round
/// 2, design D11).
///
/// An explicit selection (flags or `SQRY_*` variables) wins, because that
/// is the caller changing the selection. Otherwise the selection the index
/// at `root` records is reused, so `sqry index --force` and
/// `sqry index --no-incremental` over an `include_all` index keep `json`
/// instead of silently narrowing the manifest to the fast path (the one
/// surface that still did, at round 1). A root with no index resolves to
/// the fast-path default. A manifest that exists but cannot be read
/// resolves to the fast-path default with
/// [`PluginSelectionSource::FallbackAfterUnreadableManifest`], which the
/// caller prints as a warning naming the file; this arm is reachable only
/// with `--force` or `--no-incremental`, since the early-exit gate in
/// `run_index` stops otherwise. A readable manifest naming an id this
/// binary did not compile is refused by `create_manager_from_selection`
/// exactly as on every other surface.
///
/// # Errors
///
/// Returns an error if CLI or environment overrides reference unknown
/// plugins, or if the manifest fails for a reason other than being
/// unreadable.
pub fn resolve_index_selection(
    root: &Path,
    args: &PluginSelectionArgs,
) -> Result<ResolvedPluginSelection> {
    if let Some(explicit) = resolve_explicit_selection(args)? {
        return Ok(explicit);
    }
    match sqry_plugin_registry::resolve_persisted_selection(root) {
        Ok(Some(selection)) => Ok(selection.into()),
        Ok(None) => resolve_default_fast_path_selection(),
        Err(PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        }) => {
            let mut selection = resolve_default_fast_path_selection()?;
            selection.source = PluginSelectionSource::FallbackAfterUnreadableManifest {
                manifest_path,
                reason,
            };
            Ok(selection)
        }
        Err(other) => Err(anyhow::Error::new(other)).with_context(|| {
            format!(
                "failed to resolve the recorded plugin selection at {}",
                GraphStorage::new(root).manifest_path().display()
            )
        }),
    }
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
    resolved_from_registry_config(
        &PluginSelectionConfig::default(),
        PluginSelectionSource::Default,
    )
}

fn resolve_read_only_selection(
    root: &Path,
    args: &PluginSelectionArgs,
) -> Result<ResolvedPluginSelection> {
    let storage = GraphStorage::new(root);
    if storage.exists() {
        let persisted_selection = resolve_persisted_or_legacy_selection(root)?;
        if let Some(explicit_selection) = resolve_explicit_selection(args)?
            && let Some(change) = plugin_set_change(
                &explicit_selection.active_plugin_ids,
                &persisted_selection.active_plugin_ids,
            )
        {
            bail!(
                "plugin-selection overrides conflict with the persisted index selection ({change}); check CLI flags and SQRY_* plugin-selection environment variables, then rebuild the index if you want a new plugin set"
            );
        }
        return Ok(persisted_selection);
    }

    resolve_explicit_selection(args)?.map_or_else(resolve_default_fast_path_selection, Ok)
}

/// Read the selection the index at `root` records, through the registry's
/// single resolver (`sqry_plugin_registry::resolve_persisted_selection`).
///
/// The legacy branch (a manifest without `plugin_selection` resolves to
/// every built-in plugin with `include_all`) lives in the registry now, so
/// the daemon, the standalone MCP and the LSP read a manifest exactly the
/// way this command does.
fn resolve_persisted_or_legacy_selection(root: &Path) -> Result<ResolvedPluginSelection> {
    let storage = GraphStorage::new(root);
    match sqry_plugin_registry::resolve_persisted_selection(root) {
        Ok(Some(selection)) => Ok(selection.into()),
        // Unreachable from `resolve_read_only_selection` (it checks
        // `storage.exists()` first) and from `resolve_update_selection`
        // (`sqry update` requires an index). If a future caller reaches it
        // with no index, the honest answer is the same default a fresh
        // `sqry index` would use, not an error about a manifest that was
        // never written.
        Ok(None) => resolve_default_fast_path_selection(),
        Err(err) => Err(anyhow::Error::new(err)).with_context(|| {
            format!(
                "failed to load manifest for plugin selection at {}",
                storage.manifest_path().display()
            )
        }),
    }
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

    resolved_from_registry_config(
        &PluginSelectionConfig {
            high_cost_mode,
            enable_plugins: enable_plugins.into_iter().collect::<BTreeSet<_>>(),
            disable_plugins: disable_plugins.into_iter().collect::<BTreeSet<_>>(),
        },
        PluginSelectionSource::Explicit,
    )
    .map(Some)
}

fn resolved_from_registry_config(
    config: &PluginSelectionConfig,
    source: PluginSelectionSource,
) -> Result<ResolvedPluginSelection> {
    let PluginSelectionResolution {
        high_cost_mode,
        active_plugin_ids,
    } = resolve_registry_plugin_selection(config)
        .with_context(|| "failed to resolve plugin selection configuration".to_string())?;

    Ok(ResolvedPluginSelection {
        active_plugin_ids,
        high_cost_mode: Some(high_cost_mode.as_str().to_string()),
        source,
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

/// Test-only: run `test_fn` with every `SQRY_*` plugin-selection variable
/// removed, restoring the prior values afterwards. Shared by the index
/// command tests, which must not inherit an operator's environment.
#[cfg(test)]
pub(crate) fn with_cleared_plugin_env(test_fn: impl FnOnce()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;
    use clap::Parser;
    use serial_test::serial;
    use sqry_core::graph::unified::persistence::{BuildProvenance, Manifest};
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn test_default_index_selection_excludes_json() {
        with_cleared_plugin_env(|| {
            let no_index = TempDir::new().expect("temp dir");
            let selection =
                resolve_index_selection(no_index.path(), &PluginSelectionArgs::default())
                    .expect("selection resolves");
            assert!(!selection.active_plugin_ids.iter().any(|id| id == "json"));
            assert_eq!(selection.source, PluginSelectionSource::Default);
        });
    }

    /// D11: `resolve_index_selection` reuses a readable manifest's
    /// selection, prefers an explicit flag over it, and reports an
    /// unreadable manifest as the fallback source naming the file. On
    /// `abefdd8e3` the function takes no root and `PluginSelectionSource`
    /// does not exist (compile error).
    #[test]
    #[serial]
    fn test_index_selection_reuses_the_manifest_unless_explicit() {
        with_cleared_plugin_env(|| {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let recorded = ResolvedPluginSelection {
                active_plugin_ids: vec!["rust".to_string(), "json".to_string()],
                high_cost_mode: Some("include_all".to_string()),
                source: PluginSelectionSource::Explicit,
            };
            write_manifest_with_selection(temp_dir.path(), &recorded);
            let manifest_path = GraphStorage::new(temp_dir.path())
                .manifest_path()
                .to_path_buf();

            let reused = resolve_index_selection(temp_dir.path(), &PluginSelectionArgs::default())
                .expect("recorded selection resolves");
            assert_eq!(reused.active_plugin_ids, recorded.active_plugin_ids);
            assert_eq!(reused.high_cost_mode, recorded.high_cost_mode);
            assert_eq!(
                reused.source,
                PluginSelectionSource::RecordedInManifest {
                    manifest_path: manifest_path.clone()
                }
            );

            let explicit = resolve_index_selection(
                temp_dir.path(),
                &PluginSelectionArgs {
                    exclude_high_cost: true,
                    ..PluginSelectionArgs::default()
                },
            )
            .expect("explicit selection resolves");
            assert_eq!(explicit.source, PluginSelectionSource::Explicit);
            assert!(!explicit.active_plugin_ids.iter().any(|id| id == "json"));
            assert_eq!(explicit.high_cost_mode.as_deref(), Some("exclude_all"));

            std::fs::write(&manifest_path, b"{}").expect("unparseable manifest");
            let fallback =
                resolve_index_selection(temp_dir.path(), &PluginSelectionArgs::default())
                    .expect("an unreadable manifest falls back on the index path");
            assert_eq!(
                fallback.high_cost_mode.as_deref(),
                Some("fast_path_default")
            );
            assert!(!fallback.active_plugin_ids.iter().any(|id| id == "json"));
            match &fallback.source {
                PluginSelectionSource::FallbackAfterUnreadableManifest {
                    manifest_path: named,
                    reason,
                } => {
                    assert_eq!(named, &manifest_path);
                    assert!(!reason.is_empty());
                }
                other => panic!("expected FallbackAfterUnreadableManifest, got {other:?}"),
            }
            let warning = fallback
                .source
                .unreadable_manifest_warning()
                .expect("the fallback source carries a warning");
            assert!(
                warning.contains(&manifest_path.display().to_string()),
                "the warning must name the manifest: {warning}"
            );
            assert!(reused.source.unreadable_manifest_warning().is_none());
            assert!(explicit.source.unreadable_manifest_warning().is_none());
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
            let selection = resolve_index_selection(temp_dir.path(), &PluginSelectionArgs::default())
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
            let selection = resolve_index_selection(temp_dir.path(), &PluginSelectionArgs::default())
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
            #[allow(clippy::manual_let_else)] // Plugin lookup uses match for error path
            let err = match result {
                Ok(_) => unreachable!("conflicting explicit selection should be rejected"),
                Err(err) => err,
            };
            assert!(err.to_string().contains("conflict"));
        });
    }
    }

    large_stack_test! {
    /// Round 7 surfaces audit: `sqry diff` and the read-only commands
    /// compared the override's ids with the recorded ids as ordered lists,
    /// so a manifest listing the same plugins in another order, or one id
    /// twice, refused an override naming that very set. Both compare sets
    /// now: the same set is accepted (the recorded list kept as it is), and
    /// another set is refused naming the plugins the override adds and drops.
    #[test]
    #[serial]
    fn diff_and_read_only_selections_compare_the_plugin_set_not_its_order() {
        with_cleared_plugin_env(|| {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let mut recorded =
                resolve_index_selection(temp_dir.path(), &PluginSelectionArgs::default())
                    .expect("selection resolves");
            recorded.active_plugin_ids.reverse();
            let first = recorded.active_plugin_ids[0].clone();
            recorded.active_plugin_ids.push(first);
            write_manifest_with_selection(temp_dir.path(), &recorded);
            let manifest = GraphStorage::new(temp_dir.path())
                .manifest_path()
                .display()
                .to_string();
            let root = temp_dir.path().to_str().expect("temp path should be utf-8");
            let set_env = |vars: &[(&str, &str)]| {
                for name in [
                    "SQRY_INCLUDE_HIGH_COST",
                    "SQRY_EXCLUDE_HIGH_COST",
                    "SQRY_ENABLE_PLUGINS",
                    "SQRY_DISABLE_PLUGINS",
                ] {
                    unsafe { std::env::remove_var(name) };
                }
                for (name, value) in vars {
                    unsafe { std::env::set_var(name, value) };
                }
            };

            for mode in [PluginSelectionMode::Diff, PluginSelectionMode::ReadOnly] {
                // `sqry diff` has no plugin flags, so both name the
                // selection through the variables.
                let cli = match mode {
                    PluginSelectionMode::Diff => Cli::parse_from(["sqry", "diff", "HEAD~1", "HEAD"]),
                    _ => Cli::parse_from(["sqry", "query", "kind:function", root]),
                };
                set_env(&[("SQRY_DISABLE_PLUGINS", "json")]);
                let resolved = resolve_plugin_selection(&cli, temp_dir.path(), mode)
                    .unwrap_or_else(|err| panic!("{mode:?}: the same set is accepted: {err:#}"));
                assert_eq!(
                    resolved
                        .persisted_selection
                        .expect("a selection")
                        .active_plugin_ids,
                    recorded.active_plugin_ids,
                    "{mode:?}: the recorded list is used as it is"
                );

                set_env(&[
                    ("SQRY_INCLUDE_HIGH_COST", "1"),
                    ("SQRY_DISABLE_PLUGINS", "rust"),
                ]);
                let refused = resolve_plugin_selection(&cli, temp_dir.path(), mode);
                set_env(&[]);
                let Err(err) = refused else {
                    panic!("{mode:?}: another set is refused");
                };
                let err = err.to_string();
                assert!(
                    err.contains("it adds json") && err.contains(" and drops rust"),
                    "{mode:?}: {err}"
                );
                if mode == PluginSelectionMode::Diff {
                    assert!(
                        err.starts_with(&format!(
                            "plugin-selection override refused: `sqry diff` builds both refs \
                             with the plugin set the index records (manifest {manifest}), and \
                             the SQRY_* plugin-selection variables name another set: it adds \
                             json"
                        )) && err.ends_with(
                            " and drops rust. Variables naming the same set are accepted; \
                             rebuild or update the indexed workspace with the desired plugins \
                             first"
                        ),
                        "{err}"
                    );
                } else {
                    assert!(
                        err.starts_with(
                            "plugin-selection overrides conflict with the persisted index \
                             selection (it adds json"
                        ),
                        "{err}"
                    );
                }
            }
        });
    }
    }

    #[test]
    fn plugin_set_change_names_what_an_override_adds_and_drops() {
        let ids = |ids: &[&str]| ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            plugin_set_change(&ids(&["a", "b"]), &ids(&["b", "a", "a"])),
            None
        );
        assert_eq!(
            plugin_set_change(&ids(&["a", "c", "b"]), &ids(&["b", "a"])),
            Some("it adds c and drops nothing".to_string())
        );
        assert_eq!(
            plugin_set_change(&ids(&["a"]), &ids(&["c", "a", "b"])),
            Some("it adds nothing and drops b, c".to_string())
        );
        assert_eq!(
            plugin_set_change(&ids(&["d", "a"]), &ids(&["a", "b"])),
            Some("it adds d and drops b".to_string())
        );
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
}
