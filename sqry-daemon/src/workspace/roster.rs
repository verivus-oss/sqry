//! The plugin roster a workspace was built with, and the resolver that
//! chooses it (surface parity, unit W1).
//!
//! Before W1 the daemon built every workspace with one process-wide
//! fast-path `PluginManager`, loaded persisted snapshots with that same
//! manager, and stamped every served graph `PluginSelectionStatus::Exact`
//! without reading the manifest. A workspace indexed by the CLI with
//! `--include-high-cost` (or `--enable-plugin`) therefore came back from the
//! daemon without those plugins, and a daemon rebuild rewrote the manifest
//! to the narrower set (verivus-oss/sqry#842).
//!
//! Three types close that gap:
//!
//! - [`RosterRecord`]: the ids and high-cost mode a resident graph was
//!   actually built with, recorded at build or load time and published
//!   beside the graph (`LoadedWorkspace::roster`), never inferred later.
//! - [`WorkspaceRosterResolver`]: resolves the build roster for a root
//!   from its manifest through
//!   [`sqry_plugin_registry::resolve_persisted_selection`], falling back to
//!   the fast path for a root with no index, and owns the full compiled
//!   load roster ([`sqry_plugin_registry::create_plugin_manager_all`]).
//! - [`ManifestCheck`]: the comparison between a record and the manifest on
//!   disk that the acquirer, the status payload and the `rebuild_index`
//!   envelope all use, so one predicate decides what "diverges" means.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::{GraphStorage, Manifest, PluginSelectionManifest};
use sqry_core::plugin::PluginManager;
use sqry_plugin_registry::{
    PluginSelectionConfig, PluginSelectionError, ResolvedWorkspaceSelection, RosterSource,
    UnreadableManifest, create_plugin_manager_all, create_plugin_manager_for_selection,
    resolve_persisted_selection, resolve_plugin_selection, selection_from_manifest,
};

use crate::error::DaemonError;

/// The plugin selection a resident graph was built with.
///
/// Published in the same step as the graph it describes
/// (`WorkspaceManager::publish_and_retain`), so a reader that observes the
/// graph observes the record that matches it. Design D3: a record published
/// with the graph cannot disagree with it after an eviction or a rebuild,
/// where a separate memoised map could.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterRecord {
    /// Ordered built-in plugin ids the graph was built with.
    pub active_plugin_ids: Vec<String>,
    /// High-cost mode string the selection was resolved under, when known.
    pub high_cost_mode: Option<String>,
    /// Where the selection came from.
    pub source: RosterSource,
}

impl RosterRecord {
    /// Record a resolved selection.
    #[must_use]
    pub fn from_selection(selection: &ResolvedWorkspaceSelection) -> Self {
        Self {
            active_plugin_ids: selection.active_plugin_ids.clone(),
            high_cost_mode: selection.high_cost_mode.clone(),
            source: selection.source,
        }
    }

    /// Record the ids a manager registers. Used by builders that build with
    /// an explicit `PluginManager` (test fakes); the mode is unknown.
    #[must_use]
    pub fn from_manager(plugins: &PluginManager, source: RosterSource) -> Self {
        Self {
            active_plugin_ids: plugin_ids_of(plugins),
            high_cost_mode: None,
            source,
        }
    }

    /// The fast-path default selection with [`RosterSource::Fallback`]
    /// provenance, the roster every surface builds a brand-new index with.
    #[must_use]
    pub fn fast_path_default() -> Self {
        let selection =
            ResolvedWorkspaceSelection::from_fallback(&PluginSelectionConfig::default())
                .unwrap_or_else(|_| unreachable!("default plugin selection must resolve"));
        Self::from_selection(&selection)
    }

    /// The record as a manifest `plugin_selection` block, carrying the
    /// recorded `high_cost_mode` through rather than `None`.
    #[must_use]
    pub fn selection_manifest(&self) -> PluginSelectionManifest {
        PluginSelectionManifest {
            active_plugin_ids: self.active_plugin_ids.clone(),
            high_cost_mode: self.high_cost_mode.clone(),
        }
    }

    /// Whether the record names `id`.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.active_plugin_ids.iter().any(|known| known == id)
    }
}

/// A resolved build roster: the manager to build with and the record to
/// publish beside the result.
#[derive(Clone)]
pub struct ResolvedRoster {
    /// Manager registering exactly `record.active_plugin_ids`.
    pub plugins: Arc<PluginManager>,
    /// The record describing `plugins`.
    pub record: Arc<RosterRecord>,
}

impl std::fmt::Debug for ResolvedRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedRoster")
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

/// The full compiled roster, built once per process. It is the
/// load-and-classify roster for every daemon surface: a snapshot that
/// records a plugin this binary compiled must load whatever the build
/// default is (#314, #352). `PluginManager` carries no interior mutability,
/// so one shared instance serves every reader.
pub fn shared_load_roster() -> Arc<PluginManager> {
    static LOAD_ROSTER: OnceLock<Arc<PluginManager>> = OnceLock::new();
    Arc::clone(LOAD_ROSTER.get_or_init(|| Arc::new(create_plugin_manager_all())))
}

/// Resolves the build roster for a workspace root from its manifest.
///
/// `resolve` re-reads the manifest on every call (it is a small JSON file
/// and the CLI reads it on every query) and reuses the memoised
/// `Arc<PluginManager>` for a root only when the resolved id list is equal,
/// so a manifest rewritten by `sqry index` while the daemon is resident is
/// never served from a stale memo. `PluginManager` is not `Clone`, which is
/// why the memo holds `Arc`.
/// One memoised manager per root: the id list it registers and the manager.
type MemoEntry = (Vec<String>, Arc<PluginManager>);

pub struct WorkspaceRosterResolver {
    fallback: PluginSelectionConfig,
    load_roster: Arc<PluginManager>,
    memo: Mutex<HashMap<PathBuf, MemoEntry>>,
    /// Test-only planted roster returned for every root regardless of the
    /// manifest. Production code has no constructor that sets it.
    #[cfg(any(test, feature = "test-hooks"))]
    pinned: Option<ResolvedRoster>,
}

impl std::fmt::Debug for WorkspaceRosterResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let memo_roots: Vec<PathBuf> = self.memo.lock().keys().cloned().collect();
        f.debug_struct("WorkspaceRosterResolver")
            .field("fallback", &self.fallback)
            .field("memo_roots", &memo_roots)
            .finish_non_exhaustive()
    }
}

impl Default for WorkspaceRosterResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceRosterResolver {
    /// A resolver whose fallback for a root with no index is the fast-path
    /// default and whose load roster is the full compiled roster.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fallback: PluginSelectionConfig::default(),
            load_roster: shared_load_roster(),
            memo: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "test-hooks"))]
            pinned: None,
        }
    }

    /// Test-only: a resolver that returns `plugins` and `record` for every
    /// root, ignoring the manifest, so a test can plant a resident roster
    /// narrower (or wider) than the manifest records. The load roster stays
    /// the full compiled roster so unknown-id classification is unchanged.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    #[must_use]
    pub fn pinned(plugins: Arc<PluginManager>, record: RosterRecord) -> Self {
        Self {
            fallback: PluginSelectionConfig::default(),
            load_roster: shared_load_roster(),
            memo: Mutex::new(HashMap::new()),
            pinned: Some(ResolvedRoster {
                plugins,
                record: Arc::new(record),
            }),
        }
    }

    /// The full compiled roster used to load snapshots and to classify
    /// manifest ids as loadable or unknown.
    #[must_use]
    pub fn load_roster(&self) -> &Arc<PluginManager> {
        &self.load_roster
    }

    /// Resolve the build roster for `root`.
    ///
    /// An unreadable manifest is refused here rather than resolved to the
    /// fallback: the file carries no recorded selection to preserve, and
    /// writing over it without an explicit instruction would be the silent
    /// narrowing this resolver exists to prevent (surface parity W1 round 2,
    /// design D9). Every daemon path but one resolves through this: loads,
    /// the watcher's rebuilds and `daemon/rebuild` (whose `force` reaches the
    /// dispatcher only as an injected change class the watcher also
    /// produces), so their repair is the CLI command the error names. The
    /// daemon-hosted `rebuild_index`, a caller's explicit request to replace
    /// the index, resolves through [`Self::resolve_for_rebuild`].
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspaceIncompatibleGraph`] (JSON-RPC `-32005`)
    ///   when the manifest names a plugin id this binary did not compile.
    /// - [`DaemonError::WorkspaceManifestUnreadable`] (JSON-RPC `-32001`)
    ///   naming the manifest and `sqry index --force <root>` when the
    ///   manifest exists but cannot be read.
    pub fn resolve(&self, root: &Path) -> Result<ResolvedRoster, DaemonError> {
        #[cfg(any(test, feature = "test-hooks"))]
        if let Some(pinned) = &self.pinned {
            return Ok(pinned.clone());
        }

        let selection = match resolve_persisted_selection(root) {
            Ok(Some(selection)) => selection,
            Ok(None) => ResolvedWorkspaceSelection::from_fallback(&self.fallback)
                .map_err(|err| map_selection_error(root, &err))?,
            Err(err) => return Err(map_selection_error(root, &err)),
        };
        let plugins = self.manager_for(root, &selection)?;
        Ok(ResolvedRoster {
            plugins,
            record: Arc::new(RosterRecord::from_selection(&selection)),
        })
    }

    /// [`Self::resolve`] for a rebuild the caller explicitly asked for (the
    /// daemon-hosted `rebuild_index` with `force`, or with no index yet):
    /// a manifest that exists but cannot be read carries no recorded
    /// selection to preserve, so it resolves to the fallback roster and is
    /// returned beside it for the caller to report, as the standalone
    /// `rebuild_index` and `sqry index --force` fall back (surface parity
    /// W1 round 2, design D9). Everything else is [`Self::resolve`].
    ///
    /// # Errors
    ///
    /// [`DaemonError::WorkspaceIncompatibleGraph`] (JSON-RPC `-32005`) when
    /// a readable manifest names a plugin id this binary did not compile.
    pub fn resolve_for_rebuild(
        &self,
        root: &Path,
    ) -> Result<(ResolvedRoster, Option<UnreadableManifest>), DaemonError> {
        #[cfg(any(test, feature = "test-hooks"))]
        if let Some(pinned) = &self.pinned {
            return Ok((pinned.clone(), None));
        }

        let (selection, unreadable) = match resolve_persisted_selection(root) {
            Ok(Some(selection)) => (selection, None),
            Ok(None) => (
                ResolvedWorkspaceSelection::from_fallback(&self.fallback)
                    .map_err(|err| map_selection_error(root, &err))?,
                None,
            ),
            Err(PluginSelectionError::ManifestUnreadable {
                manifest_path,
                reason,
            }) => (
                ResolvedWorkspaceSelection::from_fallback(&self.fallback)
                    .map_err(|err| map_selection_error(root, &err))?,
                Some(UnreadableManifest {
                    manifest_path,
                    reason,
                }),
            ),
            Err(err) => return Err(map_selection_error(root, &err)),
        };
        let plugins = self.manager_for(root, &selection)?;
        Ok((
            ResolvedRoster {
                plugins,
                record: Arc::new(RosterRecord::from_selection(&selection)),
            },
            unreadable,
        ))
    }

    /// Reuse the memoised manager for `root` when its id list equals the
    /// resolved one; otherwise build a new manager (outside the memo lock)
    /// and replace the entry.
    fn manager_for(
        &self,
        root: &Path,
        selection: &ResolvedWorkspaceSelection,
    ) -> Result<Arc<PluginManager>, DaemonError> {
        if let Some((ids, plugins)) = self.memo.lock().get(root)
            && ids == &selection.active_plugin_ids
        {
            return Ok(Arc::clone(plugins));
        }
        let built = Arc::new(
            create_plugin_manager_for_selection(selection)
                .map_err(|err| map_selection_error(root, &err))?,
        );
        let mut memo = self.memo.lock();
        // A concurrent resolve may have inserted an equal entry while we
        // were building; prefer it so both callers share one manager.
        if let Some((ids, plugins)) = memo.get(root)
            && ids == &selection.active_plugin_ids
        {
            return Ok(Arc::clone(plugins));
        }
        memo.insert(
            root.to_path_buf(),
            (selection.active_plugin_ids.clone(), Arc::clone(&built)),
        );
        Ok(built)
    }

    /// Number of roots with a memoised manager (observability for tests).
    #[must_use]
    pub fn memo_len(&self) -> usize {
        self.memo.lock().len()
    }
}

/// Map a registry error to the daemon error class CLAUDE.md assigns:
/// unknown ids are `WorkspaceIncompatibleGraph` (`-32005`); an unreadable
/// manifest is `WorkspaceManifestUnreadable`, which shares `-32001` with
/// `WorkspaceBuildFailed` and names the file and the repair command.
///
/// `pub(crate)` so `RealWorkspaceBuilder::load_persisted` gives the reload
/// after eviction the same mapping the resolver gives `build` (surface
/// parity W1 round 3, design D15).
pub(crate) fn map_selection_error(root: &Path, err: &PluginSelectionError) -> DaemonError {
    #[allow(deprecated)]
    match err {
        PluginSelectionError::UnknownPluginIdsCtx { .. }
        | PluginSelectionError::UnknownPluginIds { .. } => {
            DaemonError::WorkspaceIncompatibleGraph {
                root: root.to_path_buf(),
                reason: err.to_string(),
            }
        }
        PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        } => DaemonError::WorkspaceManifestUnreadable {
            root: root.to_path_buf(),
            manifest_path: manifest_path.clone(),
            reason: reason.clone(),
        },
        other => DaemonError::WorkspaceBuildFailed {
            root: root.to_path_buf(),
            reason: format!("plugin selection failed: {other}"),
        },
    }
}

fn plugin_ids_of(plugins: &PluginManager) -> Vec<String> {
    plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Record-versus-manifest comparison
// ---------------------------------------------------------------------------

/// The verdict of comparing a resident record against the manifest on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestVerdict {
    /// The root has no index; nothing to compare against.
    NoManifest,
    /// The manifest exists but could not be read.
    Unreadable { reason: String },
    /// The manifest names ids this binary cannot load. Terminal under the
    /// default strict policy, exactly as on the CLI.
    UnknownIds { unknown_plugin_ids: Vec<String> },
    /// Every manifest id is loadable but the resident record differs from
    /// the manifest as a set.
    Diverges {
        /// Manifest ids the record lacks (resident narrower).
        missing_plugin_ids: Vec<String>,
        /// Record ids the manifest lacks (resident wider).
        extra_plugin_ids: Vec<String>,
    },
    /// Record and manifest name the same ids.
    Exact,
}

/// A record compared against the manifest at a root.
#[derive(Debug)]
pub struct ManifestCheck {
    /// The manifest, when it exists and parsed.
    pub manifest: Option<Manifest>,
    /// Where the manifest is (or would be).
    pub manifest_path: PathBuf,
    /// The comparison verdict.
    pub verdict: ManifestVerdict,
}

/// Compare `record` against the manifest at `root`, classifying manifest
/// ids as loadable or unknown with `load_roster`.
#[must_use]
pub fn check_record_against_manifest(
    root: &Path,
    record: &RosterRecord,
    load_roster: &PluginManager,
) -> ManifestCheck {
    let storage = GraphStorage::new(root);
    let manifest_path = storage.manifest_path().to_path_buf();
    if !storage.exists() {
        return ManifestCheck {
            manifest: None,
            manifest_path,
            verdict: ManifestVerdict::NoManifest,
        };
    }
    let manifest = match storage.load_manifest() {
        Ok(manifest) => manifest,
        Err(err) => {
            return ManifestCheck {
                manifest: None,
                manifest_path,
                verdict: ManifestVerdict::Unreadable {
                    reason: err.to_string(),
                },
            };
        }
    };
    let selection = selection_from_manifest(&manifest, &manifest_path);
    let verdict = compare_ids(&selection.active_plugin_ids, record, load_roster);
    ManifestCheck {
        manifest: Some(manifest),
        manifest_path,
        verdict,
    }
}

/// The set comparison behind [`check_record_against_manifest`], separated
/// so it can be exercised without a manifest on disk.
#[must_use]
pub fn compare_ids(
    manifest_ids: &[String],
    record: &RosterRecord,
    load_roster: &PluginManager,
) -> ManifestVerdict {
    let unknown_plugin_ids: Vec<String> = manifest_ids
        .iter()
        .filter(|id| load_roster.plugin_by_id(id).is_none())
        .cloned()
        .collect();
    if !unknown_plugin_ids.is_empty() {
        return ManifestVerdict::UnknownIds { unknown_plugin_ids };
    }
    let manifest_set: BTreeSet<&str> = manifest_ids.iter().map(String::as_str).collect();
    let record_set: BTreeSet<&str> = record
        .active_plugin_ids
        .iter()
        .map(String::as_str)
        .collect();
    let missing_plugin_ids: Vec<String> = manifest_set
        .difference(&record_set)
        .map(|id| (*id).to_string())
        .collect();
    let extra_plugin_ids: Vec<String> = record_set
        .difference(&manifest_set)
        .map(|id| (*id).to_string())
        .collect();
    if missing_plugin_ids.is_empty() && extra_plugin_ids.is_empty() {
        ManifestVerdict::Exact
    } else {
        ManifestVerdict::Diverges {
            missing_plugin_ids,
            extra_plugin_ids,
        }
    }
}

/// The `sqry index` invocation that rebuilds `root` with the ids in
/// `missing_plugin_ids` restored: `--include-high-cost` when any missing id
/// is outside the fast-path default, plus `--enable-plugin <id>` for each
/// missing id that is a fast-path plugin (such an id can only be missing
/// when the resident roster was narrowed explicitly).
#[must_use]
pub fn restore_command(root: &Path, missing_plugin_ids: &[String]) -> String {
    let fast_path: BTreeSet<String> = resolve_plugin_selection(&PluginSelectionConfig::default())
        .map(|resolution| resolution.active_plugin_ids.into_iter().collect())
        .unwrap_or_default();
    let mut command = String::from("sqry index --force");
    if missing_plugin_ids.iter().any(|id| !fast_path.contains(id)) {
        command.push_str(" --include-high-cost");
    }
    for id in missing_plugin_ids
        .iter()
        .filter(|id| fast_path.contains(*id))
    {
        command.push_str(" --enable-plugin ");
        command.push_str(id);
    }
    command.push(' ');
    command.push_str(&root.display().to_string());
    command
}

/// Render the `plugin_selection_warning` envelope value for a divergence.
///
/// Shape (surface parity design, section 2.3):
/// `{"status":"diverges_from_manifest","missing_plugin_ids":[..],
/// "extra_plugin_ids":[..],"manifest_path":"..","resident_source":"..",
/// "hint":"<restore command>"}`. When only `extra_plugin_ids` is set (the
/// resident graph is wider than the manifest) the hint names the daemon
/// rebuild that re-records the manifest's selection.
#[must_use]
pub fn render_plugin_selection_warning(
    root: &Path,
    record: &RosterRecord,
    missing_plugin_ids: &[String],
    extra_plugin_ids: &[String],
    manifest_path: Option<&Path>,
) -> Value {
    let hint = if missing_plugin_ids.is_empty() {
        format!("sqry daemon rebuild --force {}", root.display())
    } else {
        restore_command(root, missing_plugin_ids)
    };
    json!({
        "status": "diverges_from_manifest",
        "missing_plugin_ids": missing_plugin_ids,
        "extra_plugin_ids": extra_plugin_ids,
        "manifest_path": manifest_path.map(|p| p.display().to_string()),
        "resident_source": record.source.as_str(),
        "hint": hint,
    })
}

/// Compute the `plugin_selection_warning` value for a resident record at
/// `root`, or `None` when the record matches the manifest (or there is no
/// manifest, or the manifest is refused elsewhere as unreadable/unknown).
#[must_use]
pub fn plugin_selection_warning_for(
    root: &Path,
    record: &RosterRecord,
    load_roster: &PluginManager,
) -> Option<Value> {
    let check = check_record_against_manifest(root, record, load_roster);
    match check.verdict {
        ManifestVerdict::Diverges {
            missing_plugin_ids,
            extra_plugin_ids,
        } => Some(render_plugin_selection_warning(
            root,
            record,
            &missing_plugin_ids,
            &extra_plugin_ids,
            Some(&check.manifest_path),
        )),
        ManifestVerdict::NoManifest
        | ManifestVerdict::Unreadable { .. }
        | ManifestVerdict::UnknownIds { .. }
        | ManifestVerdict::Exact => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::persistence::BuildProvenance;
    use sqry_plugin_registry::create_plugin_manager;
    use tempfile::TempDir;

    fn write_manifest(root: &Path, selection: Option<PluginSelectionManifest>) {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        Manifest::new(
            root.to_string_lossy().to_string(),
            1,
            1,
            "fixture-sha256",
            BuildProvenance::new("test", "test"),
        )
        .with_plugin_selection(selection)
        .save(storage.manifest_path())
        .expect("manifest saved");
    }

    fn include_all_manifest() -> PluginSelectionManifest {
        PluginSelectionManifest {
            active_plugin_ids: plugin_ids_of(&create_plugin_manager_all()),
            high_cost_mode: Some("include_all".to_string()),
        }
    }

    // T12: resolver memo reused only when ids are equal; a manifest change
    // between two resolve calls yields a new manager; error mappings.

    #[test]
    fn resolve_without_index_is_the_fast_path_fallback() {
        let tmp = TempDir::new().expect("tempdir");
        let resolver = WorkspaceRosterResolver::new();
        let resolved = resolver.resolve(tmp.path()).expect("fallback resolves");
        assert_eq!(resolved.record.source, RosterSource::Fallback);
        assert!(resolved.plugins.plugin_by_id("json").is_none());
        assert!(!resolved.record.contains("json"));
        assert_eq!(
            resolved.record.high_cost_mode.as_deref(),
            Some("fast_path_default")
        );
        assert_eq!(resolver.memo_len(), 1);
    }

    #[test]
    fn resolve_reuses_the_memoised_manager_only_for_equal_ids() {
        let tmp = TempDir::new().expect("tempdir");
        write_manifest(tmp.path(), Some(include_all_manifest()));
        let resolver = WorkspaceRosterResolver::new();

        let first = resolver.resolve(tmp.path()).expect("include_all resolves");
        assert_eq!(first.record.source, RosterSource::PersistedManifest);
        assert!(first.plugins.plugin_by_id("json").is_some());
        assert!(first.record.contains("json"));
        assert_eq!(first.record.high_cost_mode.as_deref(), Some("include_all"));

        let second = resolver.resolve(tmp.path()).expect("second resolve");
        assert!(
            Arc::ptr_eq(&first.plugins, &second.plugins),
            "equal ids must reuse the memoised manager"
        );
        assert_eq!(resolver.memo_len(), 1);

        // The manifest is rewritten narrower (as `sqry index` would); the
        // next resolve must not serve the stale memo.
        write_manifest(
            tmp.path(),
            Some(PluginSelectionManifest {
                active_plugin_ids: vec!["rust".to_string()],
                high_cost_mode: Some("fast_path_default".to_string()),
            }),
        );
        let third = resolver
            .resolve(tmp.path())
            .expect("rewritten manifest resolves");
        assert!(
            !Arc::ptr_eq(&first.plugins, &third.plugins),
            "changed ids must build a new manager"
        );
        assert!(third.plugins.plugin_by_id("json").is_none());
        assert_eq!(third.record.active_plugin_ids, vec!["rust".to_string()]);
        assert_eq!(resolver.memo_len(), 1, "one entry per root");
    }

    #[test]
    fn resolve_legacy_manifest_records_every_builtin() {
        let tmp = TempDir::new().expect("tempdir");
        write_manifest(tmp.path(), None);
        let resolver = WorkspaceRosterResolver::new();
        let resolved = resolver.resolve(tmp.path()).expect("legacy resolves");
        assert_eq!(
            resolved.record.source,
            RosterSource::LegacyManifestWithoutSelection
        );
        assert_eq!(
            resolved.record.active_plugin_ids,
            sqry_plugin_registry::builtin_plugin_ids()
        );
        assert!(resolved.plugins.plugin_by_id("json").is_some());
    }

    #[test]
    fn resolve_unknown_id_maps_to_workspace_incompatible_graph() {
        let tmp = TempDir::new().expect("tempdir");
        write_manifest(
            tmp.path(),
            Some(PluginSelectionManifest {
                active_plugin_ids: vec!["rust".to_string(), "w1-not-compiled".to_string()],
                high_cost_mode: None,
            }),
        );
        let resolver = WorkspaceRosterResolver::new();
        let err = resolver
            .resolve(tmp.path())
            .expect_err("unknown id refuses");
        match err {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, tmp.path());
                assert!(reason.contains("w1-not-compiled"), "reason: {reason}");
                assert!(
                    reason.contains("manifest.json"),
                    "reason must name the manifest: {reason}"
                );
            }
            other => panic!("expected WorkspaceIncompatibleGraph, got {other:?}"),
        }
        assert_eq!(err_code(tmp.path()), Some(-32005));
    }

    fn err_code(root: &Path) -> Option<i32> {
        WorkspaceRosterResolver::new()
            .resolve(root)
            .err()
            .and_then(|err| err.jsonrpc_code())
    }

    /// Round 2 (D9, D12): an index whose manifest cannot be read is refused
    /// with `-32001`, naming the file and `sqry index --force <root>`; the
    /// daemon never resolves the fallback over it (round 1 fell back and
    /// then every tool call failed as `LoadFailed`). The memo is untouched.
    #[test]
    fn resolve_unreadable_manifest_is_refused_naming_the_repair() {
        let tmp = TempDir::new().expect("tempdir");
        let storage = GraphStorage::new(tmp.path());
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"not json").expect("garbage");
        let resolver = WorkspaceRosterResolver::new();
        let err = resolver
            .resolve(tmp.path())
            .expect_err("an unreadable manifest is refused");
        match &err {
            DaemonError::WorkspaceManifestUnreadable {
                root,
                manifest_path,
                reason,
            } => {
                assert_eq!(root, tmp.path());
                assert_eq!(manifest_path, storage.manifest_path());
                assert!(!reason.is_empty());
            }
            other => panic!("expected WorkspaceManifestUnreadable, got {other:?}"),
        }
        assert_eq!(err.jsonrpc_code(), Some(-32001));
        let rendered = err.to_string();
        assert!(
            rendered.contains(&storage.manifest_path().display().to_string()),
            "must name the manifest: {rendered}"
        );
        assert!(
            rendered.contains(&format!("sqry index --force {}", tmp.path().display())),
            "must name the repair: {rendered}"
        );
        assert_eq!(resolver.memo_len(), 0, "a refusal memoises nothing");
    }

    #[test]
    fn pinned_resolver_ignores_the_manifest() {
        let tmp = TempDir::new().expect("tempdir");
        write_manifest(tmp.path(), Some(include_all_manifest()));
        let fast = Arc::new(create_plugin_manager());
        let resolver = WorkspaceRosterResolver::pinned(
            Arc::clone(&fast),
            RosterRecord::from_manager(&fast, RosterSource::Fallback),
        );
        let resolved = resolver.resolve(tmp.path()).expect("pinned resolves");
        assert!(Arc::ptr_eq(&resolved.plugins, &fast));
        assert!(!resolved.record.contains("json"));
        assert!(
            resolver.load_roster().plugin_by_id("json").is_some(),
            "pinning the build roster must not narrow the load roster"
        );
    }

    #[test]
    fn compare_ids_reports_both_directions_and_unknown_first() {
        let load = shared_load_roster();
        let fast = RosterRecord::fast_path_default();
        let all_ids = plugin_ids_of(&create_plugin_manager_all());

        match compare_ids(&all_ids, &fast, &load) {
            ManifestVerdict::Diverges {
                missing_plugin_ids,
                extra_plugin_ids,
            } => {
                assert!(missing_plugin_ids.contains(&"json".to_string()));
                assert!(extra_plugin_ids.is_empty());
            }
            other => panic!("expected Diverges, got {other:?}"),
        }

        let wide = RosterRecord::from_manager(&create_plugin_manager_all(), RosterSource::Fallback);
        match compare_ids(&fast.active_plugin_ids, &wide, &load) {
            ManifestVerdict::Diverges {
                missing_plugin_ids,
                extra_plugin_ids,
            } => {
                assert!(missing_plugin_ids.is_empty());
                assert!(extra_plugin_ids.contains(&"json".to_string()));
            }
            other => panic!("expected Diverges, got {other:?}"),
        }

        assert_eq!(
            compare_ids(&fast.active_plugin_ids, &fast, &load),
            ManifestVerdict::Exact
        );

        let mut with_unknown = all_ids.clone();
        with_unknown.push("w1-not-compiled".to_string());
        assert_eq!(
            compare_ids(&with_unknown, &fast, &load),
            ManifestVerdict::UnknownIds {
                unknown_plugin_ids: vec!["w1-not-compiled".to_string()]
            },
            "an unknown id must win over the divergence verdict"
        );
    }

    #[test]
    fn restore_command_names_the_flags_that_restore_the_ids() {
        let root = Path::new("/w");
        assert_eq!(
            restore_command(root, &["json".to_string()]),
            "sqry index --force --include-high-cost /w"
        );
        assert_eq!(
            restore_command(root, &["rust".to_string()]),
            "sqry index --force --enable-plugin rust /w"
        );
        assert_eq!(
            restore_command(root, &["json".to_string(), "rust".to_string()]),
            "sqry index --force --include-high-cost --enable-plugin rust /w"
        );
    }

    #[test]
    fn warning_is_absent_when_the_record_matches_the_manifest() {
        let tmp = TempDir::new().expect("tempdir");
        write_manifest(tmp.path(), Some(include_all_manifest()));
        let load = shared_load_roster();
        let wide = RosterRecord::from_manager(&create_plugin_manager_all(), RosterSource::Fallback);
        assert!(plugin_selection_warning_for(tmp.path(), &wide, &load).is_none());

        let fast = RosterRecord::fast_path_default();
        let warning = plugin_selection_warning_for(tmp.path(), &fast, &load)
            .expect("narrower record must warn");
        assert_eq!(warning["status"], "diverges_from_manifest");
        assert_eq!(warning["missing_plugin_ids"], json!(["json"]));
        assert_eq!(warning["extra_plugin_ids"], json!([]));
        assert_eq!(warning["resident_source"], "fallback");
        assert_eq!(
            warning["manifest_path"],
            GraphStorage::new(tmp.path())
                .manifest_path()
                .display()
                .to_string()
        );
        assert_eq!(
            warning["hint"],
            format!(
                "sqry index --force --include-high-cost {}",
                tmp.path().display()
            )
        );

        let none = TempDir::new().expect("tempdir");
        assert!(
            plugin_selection_warning_for(none.path(), &fast, &load).is_none(),
            "no manifest, no warning"
        );
    }
}
