//! Workspace roster resolution from the persisted graph manifest.
//!
//! This module is the single implementation of "which plugins does this
//! workspace's index use" for every surface that builds, loads, classifies
//! or rebuilds a graph: the CLI (`sqry-cli::plugin_defaults`), the daemon
//! (`sqry-daemon::workspace::roster`), the standalone MCP auto-build and
//! `rebuild_index`, and every LSP build site. Before this module existed the
//! manifest-to-roster resolution lived only in the CLI, so the daemon, the
//! MCP auto-build and the LSP built every workspace with the fast-path
//! default and silently dropped a high-cost or explicitly enabled plugin the
//! manifest had recorded (verivus-oss/sqry#842).
//!
//! Two roles are distinguished on purpose:
//!
//! - **Build roster**: resolved per workspace root from the manifest by
//!   [`resolve_workspace_roster`], with the caller's fallback (normally
//!   [`PluginSelectionConfig::default`], the fast path) when the workspace
//!   has no index yet.
//! - **Load-and-classify roster**: the full compiled roster
//!   ([`crate::create_plugin_manager_all`]), so a snapshot recording a
//!   plugin this binary compiled but does not build with by default (for
//!   example `json`) loads instead of failing as "not installed"
//!   (verivus-oss/sqry#314 and #352).
//!
//! Nothing here reads CLI flags or `SQRY_*` environment variables; those
//! stay in `sqry-cli::plugin_defaults`, which delegates the manifest branch
//! to [`resolve_persisted_selection`].
//!
//! Every non-durable site that resolves a roster and then builds and
//! persists (the standalone MCP auto-build and `rebuild_index`, the LSP
//! self-heal, project auto-rebuild and `sqry.index`) goes through
//! [`build_and_persist_with_workspace_roster`], which always records the
//! resolved selection. Five hand-copied resolve-warn-build-persist
//! sequences is how a site drops the selection without a compiler
//! noticing (surface parity W1 round 2, design D8); the caller states its
//! unreadable-manifest rule in the signature through
//! [`UnreadableManifestPolicy`] (design D9).

use std::path::{Path, PathBuf};

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::{
    BuildConfig, BuildResult, CancellationToken, MacroOptionsError, MacroOptionsRequest,
    MacroRequestError, ResolvedMacroOptions, UnreadableManifestRule,
    build_unified_graph_with_progress_cancellable, persist_and_analyze_graph,
    resolve_macro_options,
};
use sqry_core::graph::unified::persistence::{
    GraphStorage, IndexWriteLock, Manifest, PluginSelectionManifest,
};
use sqry_core::plugin::PluginManager;
use sqry_core::progress::SharedReporter;

use crate::{
    HighCostMode, PluginSelectionConfig, PluginSelectionError, builtin_plugin_ids,
    create_plugin_manager_for_plugin_ids, resolve_plugin_selection,
};

/// Where a resolved roster came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RosterSource {
    /// The manifest carried a `plugin_selection` block and it was used
    /// verbatim.
    PersistedManifest,
    /// The manifest predates `plugin_selection` (schema written before the
    /// selection was recorded). Such indexes were built with every built-in
    /// plugin, so the roster is [`builtin_plugin_ids`] with
    /// `high_cost_mode: include_all`.
    LegacyManifestWithoutSelection,
    /// No index exists at the root; the caller's fallback config was used.
    Fallback,
}

impl RosterSource {
    /// Stable wire spelling, used by the daemon status payload and the
    /// `plugin_selection_warning` envelope key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersistedManifest => "persisted_manifest",
            Self::LegacyManifestWithoutSelection => "legacy_manifest",
            Self::Fallback => "fallback",
        }
    }
}

/// The plugin selection a workspace resolves to, with its provenance.
///
/// `active_plugin_ids` and `high_cost_mode` are the two fields of
/// [`PluginSelectionManifest`]; [`From`] impls convert in both directions
/// so persistence callers pass the selection straight through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspaceSelection {
    /// Ordered built-in plugin ids the workspace was (or will be) built with.
    pub active_plugin_ids: Vec<String>,
    /// High-cost mode string as recorded (`fast_path_default`,
    /// `include_all`, `exclude_all`), or `None` when the recorded selection
    /// carried no mode.
    pub high_cost_mode: Option<String>,
    /// Provenance of the selection.
    pub source: RosterSource,
    /// Path of the manifest the selection was read from; `None` for
    /// [`RosterSource::Fallback`].
    pub manifest_path: Option<PathBuf>,
}

impl ResolvedWorkspaceSelection {
    /// The selection as the manifest records it.
    #[must_use]
    pub fn selection_manifest(&self) -> PluginSelectionManifest {
        PluginSelectionManifest {
            active_plugin_ids: self.active_plugin_ids.clone(),
            high_cost_mode: self.high_cost_mode.clone(),
        }
    }

    /// Resolve the fallback config into a selection with
    /// [`RosterSource::Fallback`] provenance.
    ///
    /// # Errors
    ///
    /// Returns [`PluginSelectionError::UnknownPluginIdsCtx`] when the
    /// fallback config names a plugin id this binary did not compile.
    pub fn from_fallback(fallback: &PluginSelectionConfig) -> Result<Self, PluginSelectionError> {
        let resolution = resolve_plugin_selection(fallback)?;
        Ok(Self {
            active_plugin_ids: resolution.active_plugin_ids,
            high_cost_mode: Some(resolution.high_cost_mode.as_str().to_string()),
            source: RosterSource::Fallback,
            manifest_path: None,
        })
    }
}

impl From<ResolvedWorkspaceSelection> for PluginSelectionManifest {
    fn from(selection: ResolvedWorkspaceSelection) -> Self {
        Self {
            active_plugin_ids: selection.active_plugin_ids,
            high_cost_mode: selection.high_cost_mode,
        }
    }
}

impl From<&ResolvedWorkspaceSelection> for PluginSelectionManifest {
    fn from(selection: &ResolvedWorkspaceSelection) -> Self {
        selection.selection_manifest()
    }
}

/// Derive the selection a loaded manifest records.
///
/// A manifest without a `plugin_selection` block is the legacy shape: those
/// indexes were built with every built-in plugin, so the selection is
/// [`builtin_plugin_ids`] with `high_cost_mode: include_all` and
/// [`RosterSource::LegacyManifestWithoutSelection`] provenance. This is the
/// branch `sqry-cli::plugin_defaults` carried before the registry owned it,
/// moved verbatim.
#[must_use]
pub fn selection_from_manifest(
    manifest: &Manifest,
    manifest_path: &Path,
) -> ResolvedWorkspaceSelection {
    match manifest.plugin_selection.as_ref() {
        Some(persisted) => ResolvedWorkspaceSelection {
            active_plugin_ids: persisted.active_plugin_ids.clone(),
            high_cost_mode: persisted.high_cost_mode.clone(),
            source: RosterSource::PersistedManifest,
            manifest_path: Some(manifest_path.to_path_buf()),
        },
        None => ResolvedWorkspaceSelection {
            active_plugin_ids: builtin_plugin_ids(),
            high_cost_mode: Some(HighCostMode::IncludeAll.as_str().to_string()),
            source: RosterSource::LegacyManifestWithoutSelection,
            manifest_path: Some(manifest_path.to_path_buf()),
        },
    }
}

/// Read the plugin selection the index at `root` records.
///
/// Returns `Ok(None)` when `root` has no index (`GraphStorage::exists`
/// is false), so callers that build a brand-new index choose their own
/// fallback. A manifest another writer has only moved aside for its
/// persist is not "no index": the read waits for that persist and returns
/// what it committed (decision D-i8-1). To publish with the record current
/// at publication, hold the index's persist lock from this read to the
/// commit, as [`build_and_persist_with_workspace_roster`] does (D-i8-2). Returns `Ok(Some(..))` with [`RosterSource::PersistedManifest`]
/// or [`RosterSource::LegacyManifestWithoutSelection`] provenance otherwise.
///
/// # Errors
///
/// Returns [`PluginSelectionError::ManifestUnreadable`] naming the manifest
/// path when the manifest exists but cannot be parsed. An unreadable
/// manifest is never silently treated as "no index": doing so would rebuild
/// over an index the user may be able to repair.
pub fn resolve_persisted_selection(
    root: &Path,
) -> Result<Option<ResolvedWorkspaceSelection>, PluginSelectionError> {
    let storage = GraphStorage::new(root);
    if !storage.exists() {
        return Ok(None);
    }
    let manifest_path = storage.manifest_path().to_path_buf();
    let manifest =
        storage
            .load_manifest()
            .map_err(|err| PluginSelectionError::ManifestUnreadable {
                manifest_path: manifest_path.clone(),
                reason: err.to_string(),
            })?;
    Ok(Some(selection_from_manifest(&manifest, &manifest_path)))
}

/// The manifest an index has but a rebuild could not read, recorded so the
/// caller can say why it fell back instead of silently doing so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableManifest {
    /// Absolute path to the manifest that failed to load.
    pub manifest_path: PathBuf,
    /// The persistence layer's error text.
    pub reason: String,
}

/// A resolved build roster: the plugin manager plus the selection it was
/// built from.
pub struct WorkspaceRoster {
    /// Manager registering exactly `selection.active_plugin_ids`.
    pub plugin_manager: PluginManager,
    /// The selection the manager was built from, with provenance.
    pub selection: ResolvedWorkspaceSelection,
    /// Set only by [`resolve_workspace_roster_for_rebuild`] when the index's
    /// manifest exists but could not be read and the fallback was used in
    /// its place. `None` from [`resolve_workspace_roster`], which refuses
    /// that case.
    pub unreadable_manifest: Option<UnreadableManifest>,
}

impl std::fmt::Debug for WorkspaceRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PluginManager` is deliberately not `Debug`; the selection names
        // every registered id, which is the useful part.
        f.debug_struct("WorkspaceRoster")
            .field("selection", &self.selection)
            .field("unreadable_manifest", &self.unreadable_manifest)
            .finish_non_exhaustive()
    }
}

/// Resolve the build roster for the workspace at `root`.
///
/// Composes [`resolve_persisted_selection`] with
/// [`create_plugin_manager_for_plugin_ids`]. When `root` has no index the
/// roster is resolved from `fallback` (callers pass
/// [`PluginSelectionConfig::default`] for the fast path), so the
/// selection's `high_cost_mode` string is carried through in that case
/// too rather than left `None`.
///
/// # Errors
///
/// - [`PluginSelectionError::ManifestUnreadable`] when the manifest exists
///   but cannot be parsed.
/// - [`PluginSelectionError::UnknownPluginIdsCtx`] when the manifest (or
///   the fallback config) names a plugin id this binary did not compile;
///   the error carries the manifest path when it came from a manifest.
pub fn resolve_workspace_roster(
    root: &Path,
    fallback: &PluginSelectionConfig,
) -> Result<WorkspaceRoster, PluginSelectionError> {
    let selection = match resolve_persisted_selection(root)? {
        Some(selection) => selection,
        None => ResolvedWorkspaceSelection::from_fallback(fallback)?,
    };
    let plugin_manager = create_plugin_manager_for_selection(&selection)?;
    Ok(WorkspaceRoster {
        plugin_manager,
        selection,
        unreadable_manifest: None,
    })
}

/// Resolve the build roster for a rebuild or a repair of the index at
/// `root`: like [`resolve_workspace_roster`], except that a manifest which
/// exists but cannot be read resolves to `fallback` and is reported in
/// [`WorkspaceRoster::unreadable_manifest`] instead of refusing.
///
/// This is the rule for paths that replace the index (an explicit forced
/// rebuild, or the LSP self-heal after a corrupt load): an unreadable
/// manifest carries no recorded selection to preserve, and the CLI's
/// `sqry index --force` already builds such a workspace from its flags or
/// the fast path. A readable manifest naming an id this binary did not
/// compile is still refused, because that selection is real and a rebuild
/// would silently narrow it.
///
/// # Errors
///
/// Returns [`PluginSelectionError::UnknownPluginIdsCtx`] when a readable
/// manifest (or the fallback config) names a plugin id this binary did not
/// compile.
pub fn resolve_workspace_roster_for_rebuild(
    root: &Path,
    fallback: &PluginSelectionConfig,
) -> Result<WorkspaceRoster, PluginSelectionError> {
    let (selection, unreadable_manifest) = match resolve_persisted_selection(root) {
        Ok(Some(selection)) => (selection, None),
        Ok(None) => (ResolvedWorkspaceSelection::from_fallback(fallback)?, None),
        Err(PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        }) => (
            ResolvedWorkspaceSelection::from_fallback(fallback)?,
            Some(UnreadableManifest {
                manifest_path,
                reason,
            }),
        ),
        Err(other) => return Err(other),
    };
    let plugin_manager = create_plugin_manager_for_selection(&selection)?;
    Ok(WorkspaceRoster {
        plugin_manager,
        selection,
        unreadable_manifest,
    })
}

/// Build the plugin manager registering exactly `selection.active_plugin_ids`.
///
/// Callers that memoise managers per root (the daemon) use this instead of
/// [`resolve_workspace_roster`] so the unknown-id diagnostic still names the
/// manifest the id came from.
///
/// # Errors
///
/// Returns [`PluginSelectionError::UnknownPluginIdsCtx`] naming every id in
/// the selection this binary did not compile, with `manifest_path` filled
/// from the selection when it has one.
pub fn create_plugin_manager_for_selection(
    selection: &ResolvedWorkspaceSelection,
) -> Result<PluginManager, PluginSelectionError> {
    create_plugin_manager_for_plugin_ids(&selection.active_plugin_ids)
        .map_err(|err| attach_manifest_path(err, selection.manifest_path.as_deref()))
}

/// Fill in the manifest path on an unknown-id error produced while
/// resolving a persisted selection, so the diagnostic names the file that
/// recorded the id.
fn attach_manifest_path(
    err: PluginSelectionError,
    manifest_path: Option<&Path>,
) -> PluginSelectionError {
    match (err, manifest_path) {
        (
            PluginSelectionError::UnknownPluginIdsCtx {
                ids,
                supported_ids,
                manifest_path: None,
                suggested_features,
                all_unknown_ids_have_features,
            },
            Some(path),
        ) => PluginSelectionError::UnknownPluginIdsCtx {
            ids,
            supported_ids,
            manifest_path: Some(path.to_path_buf()),
            suggested_features,
            all_unknown_ids_have_features,
        },
        (other, _) => other,
    }
}

/// How a build-and-persist caller treats a manifest that exists but cannot
/// be read (surface parity W1 round 2, design D9).
///
/// A manifest that cannot be parsed carries no recorded selection, so any
/// path that would write over it without an explicit instruction from the
/// caller refuses and names the file and the repair command; a path the
/// caller explicitly asked to rebuild, and the LSP self-heal, fall back to
/// the fast path, say so, and record the fallback selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnreadableManifestPolicy {
    /// Resolve through [`resolve_workspace_roster`]: an unreadable manifest
    /// is [`BuildWithRosterError::Selection`] with
    /// [`PluginSelectionError::ManifestUnreadable`], and nothing is written.
    Refuse,
    /// Resolve through [`resolve_workspace_roster_for_rebuild`]: an
    /// unreadable manifest resolves to the fallback, the fallback selection
    /// is recorded, and the file is reported in
    /// [`WorkspaceRoster::unreadable_manifest`] so the caller can log it.
    FallBack,
}

/// What [`build_and_persist_with_workspace_roster`] produced: the graph,
/// the build result, the roster it was built and recorded with, and the
/// macro options it was built and recorded with.
pub struct PersistedBuild {
    /// The built graph, already persisted and analysed.
    pub graph: CodeGraph,
    /// The build pipeline's result (counts, active plugin ids, timings).
    pub build_result: BuildResult,
    /// The roster the graph was built with; `roster.selection` is exactly
    /// what the manifest now records, and `roster.unreadable_manifest` is
    /// set when [`UnreadableManifestPolicy::FallBack`] was exercised.
    pub roster: WorkspaceRoster,
    /// The macro options the graph was built with and the manifest now
    /// records (surface parity W4, design W4-D7), with their source.
    pub macro_options: ResolvedMacroOptions,
}

impl std::fmt::Debug for PersistedBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistedBuild")
            .field("build_result", &self.build_result)
            .field("roster", &self.roster)
            .field("macro_options", &self.macro_options)
            .finish_non_exhaustive()
    }
}

/// Why [`build_and_persist_with_workspace_roster`] did not produce an
/// index.
///
/// No variant marks a `source`: the inner error is rendered into the
/// message so a single-line `{}` render (which is what every caller's log
/// or wire envelope shows) still names the manifest, the unknown ids or
/// the build failure.
#[derive(Debug, thiserror::Error)]
pub enum BuildWithRosterError {
    /// The roster could not be resolved: the manifest is unreadable under
    /// [`UnreadableManifestPolicy::Refuse`], or a readable manifest (or the
    /// fallback config) names a plugin id this binary did not compile.
    /// Nothing was written.
    #[error("plugin roster resolution failed: {0}")]
    Selection(PluginSelectionError),
    /// The macro options request names nothing a build can use
    /// ([`MacroOptionsRequest::validate`]): an empty or blank cfg flag, an
    /// empty expand cache directory, or a relative expand cache directory
    /// that carries a drive prefix or a root. Checked after the roster
    /// resolves and before the manifest's macro record is read. Nothing was
    /// written.
    #[error("macro options request refused: {0}")]
    MacroRequest(MacroRequestError),
    /// The macro options could not be resolved (surface parity W4, design
    /// W4-D7, W4-D11): the requested expand cache directory is empty
    /// ([`MacroOptionsError::ExpandCacheEmpty`]); the recorded (or
    /// requested) one does not exist, is not a directory, or cannot be
    /// anchored to the root ([`MacroOptionsError::ExpandCacheMissing`]); its
    /// canonical path is not valid UTF-8 so the manifest could not record it
    /// ([`MacroOptionsError::ExpandCachePathNotUtf8`]); or the manifest
    /// cannot be read under [`UnreadableManifestPolicy::Refuse`]
    /// ([`MacroOptionsError::ManifestUnreadable`]). Nothing was written.
    #[error("macro options resolution failed: {0}")]
    MacroOptions(MacroOptionsError),
    /// The build or the persistence transaction failed after the roster
    /// resolved.
    #[error("build and persist failed: {0:#}")]
    Build(anyhow::Error),
}

impl From<PluginSelectionError> for BuildWithRosterError {
    fn from(err: PluginSelectionError) -> Self {
        Self::Selection(err)
    }
}

impl From<MacroRequestError> for BuildWithRosterError {
    fn from(err: MacroRequestError) -> Self {
        Self::MacroRequest(err)
    }
}

impl From<MacroOptionsError> for BuildWithRosterError {
    fn from(err: MacroOptionsError) -> Self {
        Self::MacroOptions(err)
    }
}

/// Build and persist the index at `root` with the roster its manifest
/// records, and record that roster in the manifest (design D8).
///
/// Composes [`resolve_workspace_roster`] (under
/// [`UnreadableManifestPolicy::Refuse`]) or
/// [`resolve_workspace_roster_for_rebuild`] (under
/// [`UnreadableManifestPolicy::FallBack`]) with
/// [`build_and_persist_graph_with_progress`], always passing
/// `Some(roster.selection.selection_manifest())` as the persisted
/// selection. A root with no index resolves to `fallback` (every surface
/// passes [`PluginSelectionConfig::default`], the fast path), and that
/// fallback selection, including its `high_cost_mode`, is what the new
/// manifest records.
///
/// The macro options follow the same rule (surface parity W4, design
/// W4-D7): `macro_request` is checked on its own
/// ([`MacroOptionsRequest::validate`]) after the roster, then the manifest's
/// record overlaid by it is resolved through [`resolve_macro_options`] under
/// the unreadable-manifest rule the roster's `policy` implies, and applied
/// to a clone of `config` before the build, so the manifest records exactly
/// what was built. A relative expand cache directory resolves against
/// `root`.
///
/// The resolution and the publication are one operation (decision D-i8-2):
/// when the index directory exists, this thread holds the index's persist
/// lock ([`IndexWriteLock`]) from before the roster and macro options are
/// read until the manifest recording them is committed (the transaction
/// takes the lock again without blocking). No other writer can move the
/// manifest aside or publish in between, so the manifest this build records
/// is built from the record current at its publication, not from defaults
/// read during another transaction's set-aside window. A root with no index
/// directory resolves without the lock (nothing is created if the request is
/// refused) and takes it before the build; if a manifest appeared in the
/// meantime, the inputs are resolved again under the lock. Holding the lock
/// across the build serialises concurrent builds of one root.
///
/// This function does not log. The recorded selection is the invariant;
/// the log line is not, and each surface has its own facility (`log` in
/// the LSP, `tracing` in the MCP), so a caller that wants to report
/// [`WorkspaceRoster::unreadable_manifest`] does so itself.
///
/// # Errors
///
/// - [`BuildWithRosterError::Selection`] when the roster cannot be
///   resolved (see [`UnreadableManifestPolicy`]); nothing is written.
/// - [`BuildWithRosterError::MacroRequest`] when `macro_request` names an
///   empty or blank cfg flag, an empty expand cache directory, or a relative
///   expand cache directory with a drive prefix or a root; nothing is
///   written.
/// - [`BuildWithRosterError::MacroOptions`] when the requested expand cache
///   directory is empty, when the recorded or requested one does not exist
///   or is not a directory, when it cannot be recorded because its
///   canonical path is not valid UTF-8, or when the manifest cannot be read
///   under [`UnreadableManifestPolicy::Refuse`]; nothing is written.
/// - [`BuildWithRosterError::Build`] when the build pipeline or the
///   persistence transaction fails, including an expand cache directory
///   removed after the options were resolved (the build refuses it rather
///   than recreating it).
pub fn build_and_persist_with_workspace_roster(
    root: &Path,
    fallback: &PluginSelectionConfig,
    policy: UnreadableManifestPolicy,
    build_command: &str,
    config: &BuildConfig,
    macro_request: &MacroOptionsRequest,
    progress: SharedReporter,
) -> Result<PersistedBuild, BuildWithRosterError> {
    // A first resolution refuses whatever the request or the record
    // refuses before anything is created or locked, so a refusal writes
    // nothing under `.sqry`.
    let unlocked = resolve_build_inputs(root, fallback, policy, config, macro_request)?;
    let storage = GraphStorage::new(root);
    let graph_dir = storage.graph_dir();
    let (built, held) = match IndexWriteLock::acquire_if_present(graph_dir)
        .map_err(BuildWithRosterError::Build)?
    {
        // An index directory: hold the lock from here to the commit, and
        // build with the inputs the record holds now.
        Some(held) => {
            let (roster, macro_options, config) =
                resolve_build_inputs(root, fallback, policy, config, macro_request)?;
            let built = build_unified_graph_with_progress_cancellable(
                root,
                &roster.plugin_manager,
                &config,
                progress.clone(),
                &CancellationToken::default(),
            )
            .map_err(|err| BuildWithRosterError::Build(err.into()))?;
            ((roster, macro_options, config, built), held)
        }
        // No index directory, so no record: build without creating one,
        // then take the lock. A manifest that appeared during the build is
        // a record this build did not see, so resolve and build again under
        // the lock.
        None => {
            let (roster, macro_options, build_config) = unlocked;
            let built = build_unified_graph_with_progress_cancellable(
                root,
                &roster.plugin_manager,
                &build_config,
                progress.clone(),
                &CancellationToken::default(),
            )
            .map_err(|err| BuildWithRosterError::Build(err.into()))?;
            let held = IndexWriteLock::acquire(graph_dir).map_err(BuildWithRosterError::Build)?;
            if storage.exists() {
                drop(built);
                let (roster, macro_options, config) =
                    resolve_build_inputs(root, fallback, policy, config, macro_request)?;
                let built = build_unified_graph_with_progress_cancellable(
                    root,
                    &roster.plugin_manager,
                    &config,
                    progress.clone(),
                    &CancellationToken::default(),
                )
                .map_err(|err| BuildWithRosterError::Build(err.into()))?;
                ((roster, macro_options, config, built), held)
            } else {
                ((roster, macro_options, build_config, built), held)
            }
        }
    };
    let (roster, macro_options, config, (graph, effective_threads)) = built;
    let (graph, build_result) = persist_and_analyze_graph(
        graph,
        root,
        &roster.plugin_manager,
        &config,
        build_command,
        Some(roster.selection.selection_manifest()),
        progress,
        effective_threads,
    )
    .map_err(BuildWithRosterError::Build)?;
    drop(held);
    Ok(PersistedBuild {
        graph,
        build_result,
        roster,
        macro_options,
    })
}

/// The roster, the macro options and the build configuration a build of
/// `root` uses, read from the manifest under `policy`. The caller holds the
/// index's persist lock when the index directory exists (decision D-i8-2).
fn resolve_build_inputs(
    root: &Path,
    fallback: &PluginSelectionConfig,
    policy: UnreadableManifestPolicy,
    config: &BuildConfig,
    macro_request: &MacroOptionsRequest,
) -> Result<(WorkspaceRoster, ResolvedMacroOptions, BuildConfig), BuildWithRosterError> {
    let roster = match policy {
        UnreadableManifestPolicy::Refuse => resolve_workspace_roster(root, fallback),
        UnreadableManifestPolicy::FallBack => resolve_workspace_roster_for_rebuild(root, fallback),
    }?;
    let unreadable_rule = match policy {
        UnreadableManifestPolicy::Refuse => UnreadableManifestRule::Refuse,
        UnreadableManifestPolicy::FallBack => UnreadableManifestRule::TreatAsNoRecord,
    };
    macro_request.validate()?;
    let macro_options = resolve_macro_options(root, macro_request, unreadable_rule)?;
    let config = BuildConfig {
        macro_options: macro_options.options.clone(),
        ..config.clone()
    };
    Ok((roster, macro_options, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_source_wire_spellings_are_stable() {
        assert_eq!(
            RosterSource::PersistedManifest.as_str(),
            "persisted_manifest"
        );
        assert_eq!(
            RosterSource::LegacyManifestWithoutSelection.as_str(),
            "legacy_manifest"
        );
        assert_eq!(RosterSource::Fallback.as_str(), "fallback");
    }

    #[test]
    fn fallback_selection_carries_the_mode_string() {
        let selection =
            ResolvedWorkspaceSelection::from_fallback(&PluginSelectionConfig::default())
                .expect("default fallback resolves");
        assert_eq!(selection.source, RosterSource::Fallback);
        assert_eq!(
            selection.high_cost_mode.as_deref(),
            Some("fast_path_default")
        );
        assert!(selection.manifest_path.is_none());
        assert!(
            !selection.active_plugin_ids.iter().any(|id| id == "json"),
            "fast-path fallback must not enable json: {:?}",
            selection.active_plugin_ids
        );
    }

    #[test]
    fn attach_manifest_path_fills_only_a_missing_path() {
        let err = PluginSelectionError::UnknownPluginIdsCtx {
            ids: vec!["x".into()],
            supported_ids: vec![],
            manifest_path: None,
            suggested_features: vec![],
            all_unknown_ids_have_features: false,
        };
        let filled = attach_manifest_path(err, Some(Path::new("/w/.sqry/graph/manifest.json")));
        match filled {
            PluginSelectionError::UnknownPluginIdsCtx { manifest_path, .. } => {
                assert_eq!(
                    manifest_path.as_deref(),
                    Some(Path::new("/w/.sqry/graph/manifest.json"))
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }

        let preset = PluginSelectionError::UnknownPluginIdsCtx {
            ids: vec!["x".into()],
            supported_ids: vec![],
            manifest_path: Some(PathBuf::from("/already")),
            suggested_features: vec![],
            all_unknown_ids_have_features: false,
        };
        match attach_manifest_path(preset, Some(Path::new("/other"))) {
            PluginSelectionError::UnknownPluginIdsCtx { manifest_path, .. } => {
                assert_eq!(manifest_path.as_deref(), Some(Path::new("/already")));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
