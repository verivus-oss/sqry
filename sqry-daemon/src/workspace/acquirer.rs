//! Daemon-side [`GraphAcquirer`] adapter (DAG unit SGA04).
//!
//! [`DaemonGraphProvider`] is the daemon-resident implementation of the
//! shared graph acquisition contract defined in
//! [`sqry_core::graph::acquisition`]. It wraps a long-lived
//! [`WorkspaceManager`] + [`WorkspaceBuilder`] pair so every read-only
//! daemon-hosted query (`tool_core::classify_and_execute` callers,
//! daemon-hosted MCP read-only tools, daemon-hosted LSP) can resolve a
//! graph through the same `acquire(...) -> GraphAcquisition` boundary
//! as the filesystem provider used by CLI / standalone MCP / standalone
//! LSP.
//!
//! ## Scope (SGA04 only)
//!
//! - Resolves and canonicalises the requested path through
//!   [`tool_core::resolve_path`] (matches the existing daemon path
//!   policy).
//! - Maps [`ServeVerdict::Fresh`] / [`ServeVerdict::Stale`] /
//!   [`ServeVerdict::NotReady`] into [`GraphAcquisition`].
//! - On [`DaemonError::WorkspaceEvicted`] for
//!   [`AcquisitionOperation::ReadOnlyQuery`], performs **exactly one**
//!   bounded read-only persisted-graph rehydrate via
//!   [`WorkspaceManager::reload_from_disk_read_only`]. A failed reload of
//!   a workspace that was resident surfaces as
//!   [`GraphAcquisitionError::Evicted`] with the original lifecycle and
//!   the reload's failure, which the wire carries as `-32004`
//!   [`DaemonError::WorkspaceReloadFailed`]; a failed reload of one that
//!   never was surfaces as the reload's own error (the absent index is
//!   `-32001` [`DaemonError::WorkspaceNotIndexed`], naming the file and
//!   `sqry index <root>`). A slot a failed load left `Failed` over the
//!   placeholder (which `classify_for_serve` answers as
//!   `ServeVerdict::FailedWithoutGraph`) gets the same one reload on the
//!   next query when a reload may clear it
//!   (`reload_or_refuse_failed_without_graph`), so a repair on disk is
//!   served without `daemon/load`.
//! - Rejects [`AcquisitionOperation::MutatingRebuild`] — the daemon's
//!   `rebuild_index` flow stays explicit and never falls back to the
//!   read-only reload path.
//!
//! ## Out of scope (handled in later DAG units)
//!
//! - Routing read-only tool dispatch through this provider (SGA05).
//! - LSP integration (SGA06).
//! - Parity tests across all surfaces (SGA07).
//!
//! ## Contract guarantees
//!
//! - Path validation runs **before** any [`WorkspaceManager`]
//!   classification — see [`Self::acquire`]. An invalid path therefore
//!   surfaces as [`GraphAcquisitionError::InvalidPath`] without ever
//!   touching admission accounting or reload counters.
//! - The bounded reload is one-shot: the provider never recurses into
//!   itself and never loops on `WorkspaceEvicted`. The caller's request
//!   is the unit of work.
//! - Mutating rebuild paths cannot use the read-only reload fallback —
//!   the [`AcquisitionOperation::MutatingRebuild`] branch returns a
//!   typed [`GraphAcquisitionError::Internal`] documenting that the
//!   daemon provider deliberately does not serve this mode.
//! - An index whose manifest cannot be read is refused as the daemon's
//!   own [`DaemonError::WorkspaceManifestUnreadable`] (`-32001`, naming
//!   the file and `sqry index --force <root>`) on both the resident path
//!   and the reload after eviction (surface parity W1 round 3, design
//!   D15). The refusal keeps its type from the file to the wire through
//!   [`AcquireRefusal`]: the shared [`GraphAcquisitionError`] taxonomy
//!   has no variant carrying a manifest path and a repair command, and
//!   the envelope is a daemon wire contract, so the daemon variant rides
//!   beside the shared taxonomy instead of being flattened into it.
//! - The reload after an eviction describes one generation (surface
//!   parity W1 round 4, design D20): the graph and the roster record both
//!   come from the [`PublishedGraph`] value
//!   [`WorkspaceManager::reload_from_disk_read_only`] returns, the pair it
//!   published or the pair a concurrent loader published. The slot is not
//!   re-read after the reload, so a publisher that runs between the reload
//!   and the classification cannot pair this reload's graph with its own
//!   record.
//! - The pair a concurrent loader published is observed once (surface
//!   parity W1 round 5, design D24): both `Loaded` returns of the manager's
//!   load gate (`WorkspaceManager::prepare_load_gate`, reached from
//!   [`WorkspaceManager::reload_from_disk_read_only`] and from
//!   `get_or_load_published`) read the slot's state and capture its
//!   generation under one `workspaces.read()`, and a `Loaded` slot whose
//!   generation carries no record is refused as `DaemonError::Internal`
//!   rather than handed back. Before round 5 the second return read the
//!   two with no guard, so an eviction completing between them handed this
//!   provider the placeholder (an empty graph, no record) as `Loaded`.
//!
//! ## Test hooks (`cfg(any(test, feature = "test-hooks"))`)
//!
//! - [`acquire_counter_snapshot`] / [`acquire_counter_reset`]: the
//!   process-wide acquisition counter the SGA07 parity tests read.
//! - [`DaemonGraphProvider::with_reload_pair_plant_for_test`]: a closure
//!   the reload arm runs exactly once between the reload's return and the
//!   classification of its pair, so a test can plant a second publication
//!   at the point where the pre-round-4 arm re-read the record. Not
//!   compiled into release builds.

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(test, feature = "test-hooks"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use sqry_core::graph::acquisition::{
    AcquisitionOperation, AcquisitionSource, FilesystemGraphProvider, GraphAcquirer,
    GraphAcquisition, GraphAcquisitionError, GraphAcquisitionMetadata, GraphAcquisitionRequest,
    GraphFreshness, PluginSelectionPolicy, PluginSelectionStatus, ReloadOrigin,
};
use sqry_core::project::ProjectRootMode;

use crate::error::DaemonError;
use crate::ipc::tool_core;
#[cfg(any(test, feature = "test-hooks"))]
use crate::workspace::loaded::PublishedGraph;
use crate::workspace::roster::{ManifestVerdict, RosterRecord, check_record_against_manifest};
use crate::workspace::{
    ServeVerdict, WorkspaceBuilder, WorkspaceKey, WorkspaceManager, WorkspaceState, clone_err,
};

fn u64_hours_to_f64(hours: u64) -> f64 {
    f64::from(u32::try_from(hours).unwrap_or(u32::MAX))
}

/// Initial admission-control reservation used by the daemon provider's
/// bounded read-only rehydrate. The same constant the MCP host uses for
/// initial loads — keeps admission accounting consistent across daemon
/// read-only paths.
const RELOAD_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024;

/// Why the daemon provider refused to serve a graph (surface parity W1
/// round 3, design D15).
///
/// `Acquisition` is the shared taxonomy every surface maps to the wire
/// through `From<GraphAcquisitionError> for DaemonError`. `Daemon` is a
/// refusal that already is the daemon's typed error and must reach the
/// wire unchanged: today only
/// [`DaemonError::WorkspaceManifestUnreadable`], whose `error.data`
/// carries `manifest_path` and `repair_command`, keys the shared taxonomy
/// has no place for. `tool_core::acquire_and_execute` converts with
/// `.map_err(DaemonError::from)`, which the `From` impl below serves for
/// both arms.
#[derive(Debug)]
pub(crate) enum AcquireRefusal {
    /// A refusal in the shared acquisition taxonomy.
    Acquisition(GraphAcquisitionError),
    /// A refusal that is already the daemon's own wire error.
    Daemon(DaemonError),
}

impl From<GraphAcquisitionError> for AcquireRefusal {
    fn from(err: GraphAcquisitionError) -> Self {
        Self::Acquisition(err)
    }
}

impl From<AcquireRefusal> for DaemonError {
    fn from(refusal: AcquireRefusal) -> Self {
        match refusal {
            AcquireRefusal::Acquisition(err) => Self::from(err),
            AcquireRefusal::Daemon(err) => err,
        }
    }
}

/// Daemon-side [`GraphAcquirer`] backed by a shared
/// [`WorkspaceManager`].
///
/// One instance per logical caller is fine — the struct only holds
/// `Arc` clones of the long-lived manager + builder, so construction is
/// cheap. SGA05 will route read-only tool dispatch through helpers in
/// `tool_core` and `mcp_host` that build a provider per request; in the
/// meantime the type is `pub(crate)` so it stays an internal building
/// block.
pub(crate) struct DaemonGraphProvider {
    manager: Arc<WorkspaceManager>,
    builder: Arc<dyn WorkspaceBuilder>,
    tool_name: Option<&'static str>,
    /// Test-only: run once by the reload arm of
    /// [`Self::handle_classify_error`] between the reload's return and
    /// the classification of its pair (design D20, T43 form 1).
    #[cfg(any(test, feature = "test-hooks"))]
    reload_pair_plant: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Process-wide acquisition counter — gated on `test-hooks` so it does
/// not exist in default release builds. Bumped at the top of every
/// [`DaemonGraphProvider::acquire`] call. SGA07 parity tests use this
/// to prove every daemon-hosted read-only tool call is routed through
/// this provider exactly once (rather than bypassing into a direct
/// `classify_for_serve`).
///
/// Tests `reset` the counter at setup, fire a single tool dispatch,
/// and assert the counter bumped by exactly one. Concurrent test
/// binaries are kept honest by Cargo's default per-binary serialisation
/// (the daemon's integration tests do not run in parallel with each
/// other inside the same binary unless they explicitly opt in).
#[cfg(any(test, feature = "test-hooks"))]
static GLOBAL_ACQUIRE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Test-only — snapshot the global acquisition counter.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn acquire_counter_snapshot() -> usize {
    GLOBAL_ACQUIRE_COUNTER.load(Ordering::Acquire)
}

/// Test-only — reset the global acquisition counter to zero. Returns
/// the previous value so callers can sanity-check a reset between
/// dispatches.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn acquire_counter_reset() -> usize {
    GLOBAL_ACQUIRE_COUNTER.swap(0, Ordering::AcqRel)
}

impl DaemonGraphProvider {
    /// Construct a new provider bound to the daemon's shared manager
    /// and persistent workspace builder.
    pub(crate) fn new(manager: Arc<WorkspaceManager>, builder: Arc<dyn WorkspaceBuilder>) -> Self {
        Self {
            manager,
            builder,
            tool_name: None,
            #[cfg(any(test, feature = "test-hooks"))]
            reload_pair_plant: None,
        }
    }

    /// Test-only: install a closure the reload arm of
    /// [`Self::handle_classify_error`] calls exactly once after
    /// [`WorkspaceManager::reload_from_disk_read_only`] returns and before
    /// the returned pair is classified. A test uses it to publish a second
    /// generation for the same key at that point (evict again, load a
    /// second builder); the acquisition must still carry the reload's own
    /// graph beside the reload's own record (design D20, T43 form 1). Not
    /// compiled into release builds.
    // The provider is `pub(crate)`, so under the `test-hooks` feature (a
    // non-test build of the library) this hook has no caller: its only
    // caller is T43 in this module's `cfg(test)` block.
    #[cfg(any(test, feature = "test-hooks"))]
    #[cfg_attr(not(test), allow(dead_code))]
    #[doc(hidden)]
    #[must_use]
    pub(crate) fn with_reload_pair_plant_for_test(
        mut self,
        plant: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.reload_pair_plant = Some(plant);
        self
    }

    /// Tag the provider with a fixed tool name for diagnostics. The
    /// resulting [`GraphAcquisitionMetadata::tool_name`] field is
    /// surfaced in logs and the canonical 4-key error envelope.
    ///
    /// SGA05 wired this per-tool tag: `tool_core::acquire_and_execute`
    /// calls it for every daemon-hosted read-only dispatch, so the
    /// `dead_code` allow that guarded the pre-wiring gap is gone.
    pub(crate) fn with_tool_name(mut self, tool_name: &'static str) -> Self {
        self.tool_name = Some(tool_name);
        self
    }

    /// Derive the [`WorkspaceKey`] used by the manager. Mirrors the
    /// in-tree convention: `ProjectRootMode::GitRoot` + zero
    /// `config_fingerprint`. Daemon dispatch paths use the same shape
    /// (see `tool_core::classify_and_execute`).
    fn key_for(canonical_root: &std::path::Path) -> WorkspaceKey {
        WorkspaceKey::new(canonical_root.to_path_buf(), ProjectRootMode::GitRoot, 0)
    }

    /// Build a [`GraphAcquisition`] from a captured graph arc, its roster
    /// record and per-state freshness metadata, classifying the record
    /// against the on-disk manifest (surface parity W1, design D4).
    ///
    /// The acquisition source is always one of
    /// [`AcquisitionSource::DaemonReadOnly`] or
    /// [`AcquisitionSource::DaemonReloaded`]. The verdict ladder:
    ///
    /// - no manifest: `Exact` (nothing to disagree with);
    /// - manifest unreadable: [`DaemonError::WorkspaceManifestUnreadable`]
    ///   through [`AcquireRefusal::Daemon`] (design D15), `-32001` naming
    ///   the file and the repair; the graph stays resident, this is a
    ///   refusal and not an unload, and a later call after the manifest is
    ///   repaired serves without a reload;
    /// - a manifest id the full compiled roster cannot load:
    ///   `IncompatibleUnknownPluginIds`, terminal under
    ///   [`PluginSelectionPolicy::StrictMatch`] exactly as on the CLI;
    /// - manifest ids not equal (as a set) to the record's ids:
    ///   `DivergesFromManifest`, served with a warning;
    /// - otherwise `Exact`.
    fn acquisition_from_parts(
        &self,
        graph: Arc<sqry_core::graph::CodeGraph>,
        workspace_root: PathBuf,
        requested_canonical: &std::path::Path,
        request: &GraphAcquisitionRequest,
        freshness: GraphFreshness,
        source: AcquisitionSource,
        roster: &RosterRecord,
    ) -> Result<GraphAcquisition, AcquireRefusal> {
        // SGA02 contract: `query_scope` is `None` when the request
        // targeted the workspace root, and `Some(subtree)` when the
        // request targeted a subdirectory of the owning workspace
        // (#394 Part 1b). The subtree filter itself is applied by the
        // shared inner tool body via `subtree_within`; `query_scope`
        // records the honest scope for diagnostics.
        let (query_scope, is_file_scope) = scope_for_request(requested_canonical, &workspace_root);
        // The tool name reaches the metadata via the request OR the
        // provider tag; request wins so per-call diagnostics dominate.
        let tool_name = request.tool_name.or(self.tool_name);

        let load_roster = self.builder.load_roster();
        let check = check_record_against_manifest(&workspace_root, roster, &load_roster);
        let mut identity = FilesystemGraphProvider::identity_from_manifest(
            check.manifest.as_ref(),
            &workspace_root,
        );
        identity.plugin_selection_status = match check.verdict {
            ManifestVerdict::NoManifest | ManifestVerdict::Exact => PluginSelectionStatus::Exact,
            ManifestVerdict::Unreadable { reason } => {
                // Design D15: the daemon's own typed refusal, naming the
                // file and the repair, instead of the shared `LoadFailed`
                // that the wire showed as a bare `-32001` build failure.
                return Err(AcquireRefusal::Daemon(
                    DaemonError::WorkspaceManifestUnreadable {
                        root: workspace_root,
                        manifest_path: check.manifest_path,
                        reason,
                    },
                ));
            }
            ManifestVerdict::UnknownIds {
                mut unknown_plugin_ids,
            } => {
                if let PluginSelectionPolicy::AllowUnknownIds { allowed } =
                    &request.plugin_selection_policy
                {
                    unknown_plugin_ids.retain(|id| !allowed.contains(id));
                }
                if unknown_plugin_ids.is_empty() {
                    // Every unknown id was explicitly allowed; mirror the
                    // filesystem provider, which reports `Exact` here.
                    PluginSelectionStatus::Exact
                } else {
                    return Err(GraphAcquisitionError::IncompatibleGraph {
                        source_root: workspace_root,
                        status: PluginSelectionStatus::IncompatibleUnknownPluginIds {
                            unknown_plugin_ids,
                            manifest_path: Some(check.manifest_path),
                        },
                    }
                    .into());
                }
            }
            ManifestVerdict::Diverges {
                missing_plugin_ids,
                extra_plugin_ids,
            } => PluginSelectionStatus::DivergesFromManifest {
                missing_plugin_ids,
                extra_plugin_ids,
                manifest_path: Some(check.manifest_path),
            },
        };

        Ok(GraphAcquisition {
            graph,
            workspace_root,
            query_scope,
            is_file_scope,
            freshness,
            identity,
            metadata: GraphAcquisitionMetadata {
                acquisition_source: source,
                tool_name,
                notes: vec![],
            },
        })
    }

    /// [`GraphAcquirer::acquire`] plus the roster record of the served
    /// graph. `tool_core::acquire_and_execute` needs the record to render
    /// the `plugin_selection_warning` envelope key (its `resident_source`
    /// field is the record's provenance, which the shared
    /// [`GraphAcquisition`] type does not carry).
    pub(crate) fn acquire_with_roster(
        &self,
        request: GraphAcquisitionRequest,
    ) -> Result<(GraphAcquisition, Arc<RosterRecord>), AcquireRefusal> {
        // ----- Step 0: test instrumentation --------------------------
        //
        // Bump the process-wide acquisition counter at the very top of
        // the function, before path validation, classification, or
        // any reload work. SGA07 parity tests use the counter to prove
        // every daemon-hosted read-only tool call is routed through
        // this provider exactly once (rather than bypassing into a
        // direct `classify_for_serve`). The counter is gated on the
        // `test-hooks` feature so it does not exist in release builds.
        #[cfg(any(test, feature = "test-hooks"))]
        GLOBAL_ACQUIRE_COUNTER.fetch_add(1, Ordering::AcqRel);

        // ----- Step 1: path validation -------------------------------
        //
        // Runs BEFORE any workspace classification or reload counter so
        // an invalid path is a typed `InvalidPath` error rather than a
        // generic admission/eviction failure (see acceptance criteria
        // and SGA02 contract `InvalidPath` precedence).
        let canonical_root = match tool_core::resolve_path_for_acquisition(&request.requested_path)
        {
            Ok(p) => p,
            Err(err) => {
                return Err(GraphAcquisitionError::InvalidPath {
                    path: request.requested_path.clone(),
                    reason: invalid_argument_reason(&err),
                }
                .into());
            }
        };

        // ----- Step 2: mutating-rebuild guard ------------------------
        //
        // The daemon's `rebuild_index` flow drives `get_or_load` (build
        // pipeline + durable publish) directly. It MUST NOT silently
        // fall back to the read-only persisted-graph reload path the
        // `ReadOnlyQuery` branch uses below; the durable rebuild
        // contract owns those semantics, not this provider.
        if matches!(request.operation, AcquisitionOperation::MutatingRebuild) {
            return Err(GraphAcquisitionError::Internal {
                reason: format!(
                    "daemon graph provider does not serve MutatingRebuild for {}; \
                     route through WorkspaceManager::get_or_load via the explicit \
                     rebuild_index flow",
                    canonical_root.display()
                ),
            }
            .into());
        }

        // ----- Step 2b: owning-workspace resolution (#394 Part 1b) ----
        //
        // The requested path may name a SUBDIRECTORY of a loaded
        // workspace rather than the workspace root itself. Classifying
        // the subtree path directly would fail (it is not a registered
        // `WorkspaceKey`), so resolve it to the longest registered
        // workspace root that contains it and classify against THAT.
        // The shared inner tool body then derives the subtree from
        // `subtree_within(&args.path, &ctx.workspace_root)` and scopes
        // results (standalone parity, Slice B).
        //
        // When the requested path IS a registered workspace root, the
        // longest ancestor is the root itself, so `effective_root ==
        // canonical_root` and the exact-root path stays byte-identical.
        // When no registered root contains it, resolution returns `None`
        // and we keep `canonical_root`, preserving the existing
        // `NotReady` / `Evicted` "not loaded" error for unknown paths.
        let effective_root = self
            .manager
            .find_owning_workspace_root(&canonical_root)
            .unwrap_or_else(|| canonical_root.clone());

        // ----- Step 3: classify + map verdict ------------------------
        let key = Self::key_for(&effective_root);
        let now = SystemTime::now();
        match self.manager.classify_for_serve(&key, now) {
            Ok(ServeVerdict::Fresh {
                graph,
                state,
                roster,
            }) => {
                // Preserve the actual workspace lifecycle label so the
                // wire envelope's `meta.workspace_state` reports
                // `Loaded` vs. `Rebuilding` accurately. SGA05's
                // `acquire_and_execute` parses this label back into a
                // `WorkspaceState` for the JSON-RPC `ResponseMeta`.
                let lifecycle_label = match state {
                    crate::workspace::WorkspaceState::Rebuilding => Some("rebuilding"),
                    // Fresh verdicts only ever carry Loaded / Rebuilding
                    // (see `WorkspaceManager::classify_for_serve` table).
                    // Defensive fallback uses the Debug rendering.
                    _ => Some("loaded"),
                };
                let freshness = GraphFreshness::Fresh { lifecycle_label };
                let acquisition = self.acquisition_from_parts(
                    graph,
                    effective_root,
                    &canonical_root,
                    &request,
                    freshness,
                    AcquisitionSource::DaemonReadOnly,
                    &roster,
                )?;
                Ok((acquisition, roster))
            }
            Ok(ServeVerdict::Stale {
                graph,
                age_hours,
                last_good_at,
                last_error,
                roster,
            }) => {
                let freshness = GraphFreshness::Stale {
                    last_good_at: Some(rfc3339_utc(last_good_at)),
                    last_error,
                    age_hours: Some(u64_hours_to_f64(age_hours)),
                };
                let acquisition = self.acquisition_from_parts(
                    graph,
                    effective_root,
                    &canonical_root,
                    &request,
                    freshness,
                    AcquisitionSource::DaemonReadOnly,
                    &roster,
                )?;
                Ok((acquisition, roster))
            }
            Ok(ServeVerdict::NotReady { state }) => Err(GraphAcquisitionError::NotReady {
                workspace_root: effective_root,
                lifecycle: format!("{state:?}"),
            }
            .into()),
            Ok(ServeVerdict::FailedWithoutGraph {
                had_been_loaded,
                last_error,
            }) => {
                let daemon_err = Self::reload_or_refuse_failed_without_graph(
                    &key,
                    had_been_loaded,
                    last_error.as_deref(),
                )?;
                self.handle_classify_error(
                    &request,
                    &key,
                    effective_root,
                    &canonical_root,
                    daemon_err,
                )
            }
            Err(daemon_err) => self.handle_classify_error(
                &request,
                &key,
                effective_root,
                &canonical_root,
                daemon_err,
            ),
        }
    }

    /// What a query does with a slot a failed load left `Failed` over the
    /// placeholder (`classify_for_serve` answers it as
    /// [`ServeVerdict::FailedWithoutGraph`], in the same read as the
    /// state): `Ok(WorkspaceEvicted)` when the bounded reload in
    /// [`Self::handle_classify_error`] may clear it, otherwise the refusal
    /// to answer (`Ok` of an error the caller passes on unchanged, or `Err`
    /// of the final refusal).
    ///
    /// Reloaded: a slot that had been loaded (`had_been_loaded`), and one
    /// whose recorded failure is a refusal of the on-disk index (absent,
    /// unreadable manifest or snapshot, unknown plugin id) that a repair on
    /// disk clears. Left alone: a never-loaded slot whose failure was a
    /// build (`daemon/load`) or any other load error, which is answered
    /// with that recorded failure, typed (a build failure keeps its
    /// `-32001` and its reason).
    ///
    /// One exception answers without a reload (the `Err` return): a recorded
    /// absent-index refusal whose named file is still absent. The reload
    /// reserves admission before the builder looks at the disk, so under a
    /// tight budget each query would evict a resident workspace only to be
    /// refused again; the recorded refusal is answered instead, in the form
    /// a failed reload of this slot takes (`-32004` carrying it for a slot
    /// that had been loaded, the refusal itself for one that had not), and
    /// the reload runs on the first query after the file appears.
    ///
    /// The manager's load gate decides what the reload may do, so a slot
    /// that changed since the classification is served or refused by the
    /// gate as it then stands.
    fn reload_or_refuse_failed_without_graph(
        key: &WorkspaceKey,
        had_been_loaded: bool,
        last_error: Option<&DaemonError>,
    ) -> Result<DaemonError, AcquireRefusal> {
        if let Some(recorded @ DaemonError::WorkspaceNotIndexed { missing_path, .. }) = last_error
            && !missing_path.exists()
        {
            return Err(Self::refusal_for_failed_reload(
                key.source_root.clone(),
                had_been_loaded,
                "evicted".to_string(),
                clone_err(recorded),
            ));
        }
        let refused_on_disk = matches!(
            last_error,
            Some(
                DaemonError::WorkspaceNotIndexed { .. }
                    | DaemonError::WorkspaceManifestUnreadable { .. }
                    | DaemonError::WorkspaceSnapshotUnreadable { .. }
                    | DaemonError::WorkspaceIncompatibleGraph { .. }
            )
        );
        if had_been_loaded || refused_on_disk {
            Ok(DaemonError::WorkspaceEvicted {
                root: key.source_root.clone(),
            })
        } else {
            // The recorded failure itself, typed, rather than its text
            // wrapped in another build failure (which repeated the
            // "workspace build failed" prefix on every query).
            Ok(last_error.map_or_else(
                || DaemonError::WorkspaceBuildFailed {
                    root: key.source_root.clone(),
                    reason: "no prior successful build".to_string(),
                },
                clone_err,
            ))
        }
    }

    /// Whether `key`'s slot held a published graph before this request's
    /// reload: an eviction tombstone, or a slot with a successful publish
    /// recorded (`last_good_at`, which an eviction does not clear). Read
    /// before the reload. A slot that never held one (no entry, an entry a
    /// load gate inserted, an entry `daemon/unload` removed) was not
    /// evicted, so a failed reload of it answers with the load's own error
    /// instead of `-32004`.
    fn was_resident(&self, key: &WorkspaceKey) -> bool {
        self.manager.lookup(key).is_some_and(|ws| {
            ws.load_state() == WorkspaceState::Evicted || ws.last_good_at.read().is_some()
        })
    }

    /// The refusal a failed bounded reload answers with, for every reload
    /// error except the two typed arms (D-13, D15) the caller handles
    /// first.
    ///
    /// - A workspace that was resident (`was_resident`): `Evicted` with the
    ///   reload's own error as `reload_failure`, which the wire carries as
    ///   `-32004` [`DaemonError::WorkspaceReloadFailed`]. Before, the text
    ///   was dropped and the wire read "evicted mid-rebuild" with no cause.
    /// - A workspace that was not: the reload's own error, unchanged. It was
    ///   never evicted, so the load's error is the cause: the absent index
    ///   is `-32001` [`DaemonError::WorkspaceNotIndexed`] naming the file
    ///   and the repair, a corrupt snapshot `-32001` with its reason, a
    ///   refused admission `-32003`, and so on.
    fn refusal_for_failed_reload(
        effective_root: PathBuf,
        was_resident: bool,
        original_lifecycle: String,
        reload_err: DaemonError,
    ) -> AcquireRefusal {
        if was_resident {
            GraphAcquisitionError::Evicted {
                workspace_root: effective_root,
                original_lifecycle,
                reload_failure: Some(reload_err.to_string()),
            }
            .into()
        } else {
            AcquireRefusal::Daemon(reload_err)
        }
    }
}

impl GraphAcquirer for DaemonGraphProvider {
    /// The shared-taxonomy view of [`Self::acquire_with_roster`]. Called
    /// only by this module's unit tests (bounded by
    /// `git grep -n "\.acquire(" -- 'sqry-daemon/src/*.rs'`); the daemon's
    /// dispatch path calls `acquire_with_roster` and keeps the typed
    /// refusal. Here an unreadable manifest becomes
    /// [`GraphAcquisitionError::LoadFailed`], the filesystem provider's
    /// own verdict for the same file, the absent index
    /// [`GraphAcquisitionError::NoGraph`] (likewise the filesystem
    /// provider's verdict), a stale-serve expiry
    /// [`GraphAcquisitionError::StaleExpired`], and any other daemon-typed
    /// refusal [`GraphAcquisitionError::Internal`] (design D15).
    fn acquire(
        &self,
        request: GraphAcquisitionRequest,
    ) -> Result<GraphAcquisition, GraphAcquisitionError> {
        match self.acquire_with_roster(request) {
            Ok((acquisition, _roster)) => Ok(acquisition),
            Err(AcquireRefusal::Acquisition(err)) => Err(err),
            Err(AcquireRefusal::Daemon(err)) => match err {
                DaemonError::WorkspaceNotIndexed { root, .. } => {
                    Err(GraphAcquisitionError::NoGraph {
                        workspace_root: root,
                    })
                }
                DaemonError::WorkspaceStaleExpired {
                    root, age_hours, ..
                } => Err(GraphAcquisitionError::StaleExpired {
                    workspace_root: root,
                    age_hours: Some(u64_hours_to_f64(age_hours)),
                }),
                DaemonError::WorkspaceManifestUnreadable { ref root, .. } => {
                    Err(GraphAcquisitionError::LoadFailed {
                        source_root: root.clone(),
                        reason: err.to_string(),
                    })
                }
                other => Err(GraphAcquisitionError::Internal {
                    reason: format!("daemon refusal outside the shared taxonomy: {other}"),
                }),
            },
        }
    }
}

impl DaemonGraphProvider {
    /// Map a [`DaemonError`] surfaced by [`WorkspaceManager::classify_for_serve`]
    /// into a [`GraphAcquisitionError`]. Owns the bounded one-shot
    /// reload rule for read-only `WorkspaceEvicted`.
    fn handle_classify_error(
        &self,
        request: &GraphAcquisitionRequest,
        key: &WorkspaceKey,
        // The owning workspace root classification ran against (#394
        // Part 1b): equal to the canonical requested path for an
        // exact-root or unknown-path request, or the ancestor workspace
        // root for a subtree request.
        effective_root: PathBuf,
        // The canonical requested path (may be a subtree of
        // `effective_root`); used for the acquisition `query_scope`.
        requested_canonical: &std::path::Path,
        daemon_err: DaemonError,
    ) -> Result<(GraphAcquisition, Arc<RosterRecord>), AcquireRefusal> {
        match daemon_err {
            // Bounded one-shot reload, only for ReadOnlyQuery. The
            // operation enum was already exhausted upstream
            // (`MutatingRebuild` is rejected before this match), so
            // reaching this arm implies ReadOnlyQuery.
            DaemonError::WorkspaceEvicted { ref root } => {
                debug_assert!(
                    matches!(request.operation, AcquisitionOperation::ReadOnlyQuery),
                    "MutatingRebuild must be rejected before classify_for_serve",
                );
                // `classify_for_serve` answers `WorkspaceEvicted` for a key
                // with no entry too, so whether this workspace was ever
                // loaded is read from the slot, before the reload changes it.
                let was_resident = self.was_resident(key);
                let (original_lifecycle, original_detail) = if was_resident {
                    (
                        "evicted".to_string(),
                        format!("workspace {} evicted", root.display()),
                    )
                } else {
                    (
                        "not loaded".to_string(),
                        format!("workspace {} not loaded", root.display()),
                    )
                };
                match self.manager.reload_from_disk_read_only(
                    key,
                    self.builder.as_ref(),
                    RELOAD_WORKING_SET_BYTES,
                ) {
                    Ok(published) => {
                        // Surface parity W1 round 4 (design D20): the reload
                        // returns the generation it published (or the one a
                        // concurrent loader published), and both halves are
                        // taken from that value. The slot is not re-read: a
                        // publisher that ran between the reload's return and
                        // this point would otherwise supply its own record
                        // beside this reload's graph. The `None` arm is the
                        // placeholder guard, unreachable on a publish path.
                        #[cfg(any(test, feature = "test-hooks"))]
                        Self::run_reload_pair_plant(self.reload_pair_plant.as_ref(), &published);
                        let roster = published.roster.clone().ok_or_else(|| {
                            AcquireRefusal::from(GraphAcquisitionError::Internal {
                                reason: format!(
                                    "reload of {} published no roster record",
                                    effective_root.display()
                                ),
                            })
                        })?;
                        let graph = Arc::clone(&published.graph);
                        let freshness = GraphFreshness::Reloaded {
                            original_lifecycle: if was_resident {
                                ReloadOrigin::Evicted {
                                    detail: original_detail,
                                }
                            } else {
                                ReloadOrigin::Unloaded {
                                    detail: original_detail,
                                }
                            },
                            final_lifecycle_label: "loaded",
                            reload_attempts: std::num::NonZeroU8::new(1).expect("1 is non-zero"),
                        };
                        let acquisition = self.acquisition_from_parts(
                            graph,
                            effective_root,
                            requested_canonical,
                            request,
                            freshness,
                            AcquisitionSource::DaemonReloaded,
                            &roster,
                        )?;
                        Ok((acquisition, roster))
                    }
                    // Surface parity W1 round 2 (D10): the reload itself
                    // now refuses a manifest naming an id this binary did
                    // not compile, before anything is published. Keep the
                    // verdict this acquirer gave when it made that check
                    // after publish (`-32005` naming the ids and the
                    // manifest) instead of collapsing it into `-32004`,
                    // which reads as a transient eviction race. The ids
                    // are re-derived through the same comparison the
                    // post-publish check uses; a snapshot the loader
                    // refused for its format keeps that label.
                    Err(DaemonError::WorkspaceIncompatibleGraph { reason, .. }) => {
                        let load_roster = self.builder.load_roster();
                        let check = check_record_against_manifest(
                            &effective_root,
                            &RosterRecord::fast_path_default(),
                            &load_roster,
                        );
                        let status = match check.verdict {
                            ManifestVerdict::UnknownIds { unknown_plugin_ids } => {
                                PluginSelectionStatus::IncompatibleUnknownPluginIds {
                                    unknown_plugin_ids,
                                    manifest_path: Some(check.manifest_path),
                                }
                            }
                            _ => PluginSelectionStatus::IncompatibleSnapshotFormat { reason },
                        };
                        Err(GraphAcquisitionError::IncompatibleGraph {
                            source_root: effective_root,
                            status,
                        }
                        .into())
                    }
                    // Surface parity W1 round 3 (D15): the reload refuses an
                    // index whose manifest cannot be read with the daemon's
                    // own typed error, and that error reaches the wire as
                    // `-32001` naming the file and `sqry index --force
                    // <root>`. Collapsing it into `Evicted` below turned it
                    // into `-32004` "evicted mid-rebuild", which names no
                    // file and no repair. The arm sits beside the D-13 arm
                    // and leaves it untouched.
                    Err(err @ DaemonError::WorkspaceManifestUnreadable { .. }) => {
                        Err(AcquireRefusal::Daemon(err))
                    }
                    // Every other reload failure (the absent index, a corrupt
                    // snapshot, a refused admission, a race with an eviction
                    // or another loader): `-32004` carrying the failure for a
                    // workspace that was evicted, the failure itself for one
                    // that was never loaded. Before, all of them became
                    // `-32004` "evicted mid-rebuild" with the text dropped.
                    Err(reload_err) => Err(Self::refusal_for_failed_reload(
                        effective_root,
                        was_resident,
                        original_lifecycle,
                        reload_err,
                    )),
                }
            }
            // The daemon's own refusal, unchanged: routed through the shared
            // `StaleExpired` it lost its cap, its last good time and its last
            // error, and the wire read "stale-serve window expired (<age>h >=
            // 0h cap)" with both left null.
            err @ DaemonError::WorkspaceStaleExpired { .. } => Err(AcquireRefusal::Daemon(err)),
            DaemonError::WorkspaceBuildFailed { root: _, reason } => {
                Err(GraphAcquisitionError::BuildFailed {
                    workspace_root: effective_root,
                    reason,
                }
                .into())
            }
            DaemonError::WorkspaceNotLoaded { root: _ } => Err(GraphAcquisitionError::NoGraph {
                workspace_root: effective_root,
            }
            .into()),
            // Every other error is the daemon's own typed refusal, answered
            // unchanged: a failure a slot recorded (a refusal of a request,
            // `-32602` or `-32022`; a refused admission) keeps its own code
            // and data. The catch-all used to wrap it in
            // `GraphAcquisitionError::Internal` (`-32603` "classify_for_serve
            // returned unexpected error"), which read as a daemon bug (B2,
            // round 7 audit).
            other => Err(AcquireRefusal::Daemon(other)),
        }
    }
}

impl DaemonGraphProvider {
    /// Test-only: run the installed plant once, after the reload handed
    /// back `published` and before it is classified. Takes the pair so
    /// the call site reads as what it is: the point at which the pair is
    /// already in hand.
    #[cfg(any(test, feature = "test-hooks"))]
    fn run_reload_pair_plant(
        plant: Option<&Arc<dyn Fn() + Send + Sync>>,
        published: &Arc<PublishedGraph>,
    ) {
        let _ = published;
        if let Some(plant) = plant {
            plant();
        }
    }
}

/// Extract a clean reason string from a [`DaemonError::InvalidArgument`]
/// (or related path-policy error) for inclusion in
/// [`GraphAcquisitionError::InvalidPath`].
fn invalid_argument_reason(err: &DaemonError) -> String {
    match err {
        DaemonError::InvalidArgument { reason } => reason.clone(),
        other => other.to_string(),
    }
}

/// Render a [`SystemTime`] to RFC3339 UTC-Zulu (`YYYY-MM-DDTHH:MM:SSZ`).
/// Matches the format used by the daemon's stale-warning rendering and
/// the `WorkspaceStaleExpired::error_data` payload.
fn rfc3339_utc(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Decide the [`GraphAcquisition::query_scope`] / `is_file_scope`
/// pair for a request against a resolved workspace root.
///
/// `requested_canonical` is the canonical path the caller asked for;
/// `workspace_root` is the owning workspace root classification ran
/// against. When the two are equal (an exact-root or unknown-path
/// request) the scope is `None`. When the request targeted a
/// subdirectory of the owning workspace (#394 Part 1b), the subtree
/// directory is recorded as `query_scope` for diagnostics; the daemon
/// always canonicalises to a directory, so `is_file_scope` is `false`.
/// The subtree FILTER is applied downstream by the shared inner tool
/// body (`subtree_within` + `path_in_subtree`), not here.
fn scope_for_request(
    requested_canonical: &std::path::Path,
    workspace_root: &std::path::Path,
) -> (Option<PathBuf>, bool) {
    if requested_canonical == workspace_root {
        (None, false)
    } else {
        (Some(requested_canonical.to_path_buf()), false)
    }
}

// ---------------------------------------------------------------------------
// Tests (unit + integration-style; live in this module so they can use
// `pub(crate)` symbols without a dedicated test crate dance).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_core::project::canonicalize_path;
    use tempfile::TempDir;

    use crate::config::DaemonConfig;
    use crate::workspace::{BuiltGraph, WorkspaceState};

    // -----------------------------------------------------------------
    // Builder fakes
    // -----------------------------------------------------------------

    /// Builder that always yields an empty graph for `build` AND
    /// `load_persisted` — so the read-only reload path succeeds without
    /// touching disk in the parity tests.
    #[derive(Debug, Default)]
    struct InMemoryBuilder;

    impl WorkspaceBuilder for InMemoryBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }
    }

    /// Builder whose [`load_persisted`] always errors. Used to drive
    /// the "reload after eviction fails → Evicted error" path
    /// deterministically.
    #[derive(Debug)]
    struct ReloadFailsBuilder {
        reason: String,
        attempts: parking_lot::Mutex<u32>,
    }

    impl ReloadFailsBuilder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                reason: "synthetic reload failure".to_string(),
                attempts: parking_lot::Mutex::new(0),
            })
        }
    }

    impl WorkspaceBuilder for ReloadFailsBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, root: &Path) -> Result<BuiltGraph, DaemonError> {
            *self.attempts.lock() += 1;
            Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: self.reason.clone(),
            })
        }
    }

    /// Builder used for the `MutatingRebuild` short-circuit test. Its
    /// [`load_persisted`] increments a counter; the assertion is that
    /// the counter stays at zero after a `MutatingRebuild` request.
    #[derive(Debug, Default)]
    struct CountingLoadPersistedBuilder {
        load_persisted_count: parking_lot::Mutex<u32>,
        /// Round 5 (T50, T52): `build` is counted too, so "the builder was
        /// untouched" is asserted for both entry points of the gate.
        build_count: parking_lot::Mutex<u32>,
    }

    impl WorkspaceBuilder for CountingLoadPersistedBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            *self.build_count.lock() += 1;
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            *self.load_persisted_count.lock() += 1;
            Ok(BuiltGraph::empty_fast_path())
        }
    }

    // -----------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------

    fn make_manager() -> Arc<WorkspaceManager> {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()))
    }

    fn make_request(path: PathBuf, operation: AcquisitionOperation) -> GraphAcquisitionRequest {
        GraphAcquisitionRequest {
            requested_path: path,
            operation,
            path_policy: sqry_core::graph::acquisition::PathPolicy::default(),
            missing_graph_policy: sqry_core::graph::acquisition::MissingGraphPolicy::Error,
            stale_policy: sqry_core::graph::acquisition::StalePolicy::default(),
            plugin_selection_policy: sqry_core::graph::acquisition::PluginSelectionPolicy::default(
            ),
            tool_name: Some("sga04_test"),
        }
    }

    /// Persist an empty CodeGraph into `<root>/.sqry/graph/snapshot.sqry`
    /// so a real `RealWorkspaceBuilder::load_persisted` would succeed
    /// against the fixture if it were used. Tests in this module use the
    /// fake builders for determinism, but seeding a snapshot keeps the
    /// fixture honest about the "valid persisted graph available"
    /// precondition the SGA04 spec requires.
    fn seed_persisted_snapshot(root: &Path) {
        let graph_dir = root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let snapshot_path = graph_dir.join("snapshot.sqry");
        save_to_path(&CodeGraph::new(), &snapshot_path).unwrap();
    }

    // -----------------------------------------------------------------
    // Test 1 — Fresh workspace returns Fresh acquisition
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_fresh_workspace_returns_fresh_acquisition() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, WorkspaceState::Loaded);

        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::new(InMemoryBuilder) as Arc<dyn WorkspaceBuilder>,
        );

        let acq = provider
            .acquire(make_request(
                root.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect("Loaded workspace must produce Fresh acquisition");
        match acq.freshness {
            GraphFreshness::Fresh { lifecycle_label } => {
                assert_eq!(lifecycle_label, Some("loaded"));
            }
            other => panic!("expected Fresh, got {other:?}"),
        }
        assert_eq!(
            acq.metadata.acquisition_source,
            AcquisitionSource::DaemonReadOnly
        );
        assert_eq!(acq.workspace_root, root);
        assert_eq!(acq.metadata.tool_name, Some("sga04_test"));
    }

    // -----------------------------------------------------------------
    // Test 1b (#394) - a subtree path resolves to its owning workspace
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_subtree_path_resolves_to_owning_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("rust").join("kernel")).unwrap();
        let subdir = canonicalize_path(&root.join("rust")).unwrap();

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key, WorkspaceState::Loaded);

        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::new(InMemoryBuilder) as Arc<dyn WorkspaceBuilder>,
        );

        // Requesting the SUBTREE must not fail classification: the acquirer
        // resolves it to the owning workspace root and serves the loaded graph.
        let acq = provider
            .acquire(make_request(
                subdir.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect("subtree path must resolve to the owning loaded workspace");

        // Classified against the owning root, NOT the subtree path.
        assert_eq!(acq.workspace_root, root);
        assert!(matches!(acq.freshness, GraphFreshness::Fresh { .. }));
        // The requested subtree is recorded as the query scope.
        assert_eq!(acq.query_scope.as_deref(), Some(subdir.as_path()));
        assert!(!acq.is_file_scope);
    }

    // -----------------------------------------------------------------
    // Test 1c (#394) - a path under no loaded workspace still errors
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_subtree_path_under_no_workspace_errors() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("orphan")).unwrap();
        let orphan = canonicalize_path(&root.join("orphan")).unwrap();

        // No workspace registered -> owning resolution returns None -> the
        // classification runs against the orphan path (unknown key ->
        // WorkspaceEvicted) and the one reload runs. Nothing was ever
        // loaded there, so the refusal is the load's own error naming the
        // orphan path, not an eviction.
        let manager = make_manager();
        let builder = ReloadFailsBuilder::new();
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );

        let refusal = provider
            .acquire_with_roster(make_request(
                orphan.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect_err("a path under no loaded workspace must error clearly");
        match refusal {
            AcquireRefusal::Daemon(DaemonError::WorkspaceBuildFailed { root, reason }) => {
                assert_eq!(
                    root, orphan,
                    "the error must reference the unresolved requested path"
                );
                assert_eq!(reason, "synthetic reload failure");
            }
            other => {
                panic!("expected the load's own error for a path under no workspace, got {other:?}")
            }
        }
        assert_eq!(*builder.attempts.lock(), 1, "exactly one load attempt");
    }

    // -----------------------------------------------------------------
    // Test 2 — Evicted read-only triggers exactly one reload
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_evicted_readonly_reloads_once() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        seed_persisted_snapshot(&root);

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);

        // Drive the deterministic eviction.
        assert!(manager.evict_for_test(&key));

        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::new(InMemoryBuilder) as Arc<dyn WorkspaceBuilder>,
        );

        let acq = provider
            .acquire(make_request(
                root.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect("Evicted ReadOnlyQuery must reload and serve");
        match acq.freshness {
            GraphFreshness::Reloaded {
                original_lifecycle,
                final_lifecycle_label,
                reload_attempts,
            } => {
                assert_eq!(reload_attempts.get(), 1);
                assert_eq!(final_lifecycle_label, "loaded");
                match original_lifecycle {
                    ReloadOrigin::Evicted { detail } => {
                        assert!(
                            detail.contains("evicted"),
                            "expected eviction detail, got: {detail}"
                        );
                    }
                    other => panic!("expected Evicted origin, got {other:?}"),
                }
            }
            other => panic!("expected Reloaded freshness, got {other:?}"),
        }
        assert_eq!(
            acq.metadata.acquisition_source,
            AcquisitionSource::DaemonReloaded
        );
    }

    // -----------------------------------------------------------------
    // Test 3 — Repeated eviction surfaces Evicted with reload context
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_repeated_eviction_returns_evicted_error_after_one_reload() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        assert!(manager.evict_for_test(&key));

        let builder = ReloadFailsBuilder::new();
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );

        let err = provider
            .acquire(make_request(
                root.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect_err("reload-fails builder must surface Evicted");
        match err {
            GraphAcquisitionError::Evicted {
                workspace_root,
                original_lifecycle,
                reload_failure,
            } => {
                assert_eq!(workspace_root, root);
                // The eviction context is the lifecycle; the failure is the
                // reload's own error, rendered whole.
                assert_eq!(original_lifecycle, "evicted");
                let reload = reload_failure.expect("reload failure must be recorded");
                assert_eq!(
                    reload,
                    DaemonError::WorkspaceBuildFailed {
                        root: root.clone(),
                        reason: "synthetic reload failure".to_string(),
                    }
                    .to_string(),
                    "reload diagnostic must be the builder's failure, got: {reload}"
                );
            }
            other => panic!("expected Evicted with reload_failure, got {other:?}"),
        }
        // Exactly one reload attempt — no looping.
        assert_eq!(*builder.attempts.lock(), 1);
    }

    // -----------------------------------------------------------------
    // Test 4 — MutatingRebuild does NOT use read-only reload fallback
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_mutating_rebuild_does_not_use_readonly_fallback() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        assert!(manager.evict_for_test(&key));

        let builder = Arc::new(CountingLoadPersistedBuilder::default());
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );

        let err = provider
            .acquire(make_request(
                root.clone(),
                AcquisitionOperation::MutatingRebuild,
            ))
            .expect_err("MutatingRebuild must not use read-only fallback");
        match err {
            GraphAcquisitionError::Internal { reason } => {
                assert!(
                    reason.contains("MutatingRebuild"),
                    "internal error must explain the rejection, got: {reason}"
                );
            }
            other => panic!("expected Internal rejection of MutatingRebuild, got {other:?}"),
        }
        assert_eq!(
            *builder.load_persisted_count.lock(),
            0,
            "load_persisted MUST NOT be invoked for MutatingRebuild"
        );
    }

    // -----------------------------------------------------------------
    // Test 5 — Invalid path short-circuits before classify_for_serve
    // -----------------------------------------------------------------
    #[test]
    fn daemon_provider_invalid_path_short_circuits_before_classify_for_serve() {
        // We instrument by counting `load_persisted` invocations: an
        // invalid path must NOT reach the manager (and therefore
        // cannot trigger eviction/reload work). The counter staying at
        // zero plus `InvalidPath` proves the precedence.
        let manager = make_manager();
        let builder = Arc::new(CountingLoadPersistedBuilder::default());
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );

        let err = provider
            .acquire(make_request(
                PathBuf::from("/this/path/does/not/exist/for/sga04"),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect_err("non-existent path must fail");
        match err {
            GraphAcquisitionError::InvalidPath { path, reason } => {
                assert_eq!(path, PathBuf::from("/this/path/does/not/exist/for/sga04"));
                assert!(
                    reason.contains("path_policy") || reason.contains("does not exist"),
                    "expected path-policy reason, got: {reason}"
                );
            }
            other => panic!("expected InvalidPath, got {other:?}"),
        }
        assert_eq!(
            *builder.load_persisted_count.lock(),
            0,
            "load_persisted must not run when the path is invalid"
        );
    }

    // -----------------------------------------------------------------
    // T43 (surface parity W1 round 4, design D20, codex R4-1): the reload
    // after an eviction serves one generation. Two builder doubles publish
    // distinguishable generations: A's graph carries `A_NODES` functions
    // beside record A, B's carries `B_NODES` beside record B. The graph
    // `Arc` is minted by `publish_and_retain` (a `BuiltGraph` carries the
    // graph by value), so the generation of the served graph is read from
    // its node count and the generation of the served record from its
    // pointer. Form 1 plants a second publication (evict again, load B)
    // from inside the reload arm, at the point where the pre-round-4 arm
    // re-read the record from the slot; form 2 is the structural control
    // on `reload_from_disk_read_only`'s return value.
    // -----------------------------------------------------------------

    const A_NODES: u32 = 3;
    const B_NODES: u32 = 7;

    /// A graph with `count` function nodes, registered in the indices.
    fn graph_with_functions(count: u32) -> CodeGraph {
        use sqry_core::graph::node::Language;
        use sqry_core::graph::unified::{NodeEntry, NodeKind};

        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("/repos/example/src/lib.rs"), Some(Language::Rust))
            .expect("register file");
        for index in 0..count {
            let name = format!("generation_fn_{index}");
            let name_id = graph.strings_mut().intern(&name).expect("intern name");
            let entry = NodeEntry::new(NodeKind::Function, name_id, file_id).with_location(
                index + 1,
                0,
                index + 1,
                10,
            );
            let node_id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
            graph.indices_mut().add(
                node_id,
                entry.kind,
                entry.name,
                entry.qualified_name,
                entry.file,
            );
        }
        graph
    }

    /// A builder double that publishes one generation: a graph with
    /// `nodes` functions beside the record it holds, on `build` and on
    /// `load_persisted` alike.
    #[derive(Debug)]
    struct GenerationBuilder {
        nodes: u32,
        record: Arc<RosterRecord>,
    }

    impl GenerationBuilder {
        fn generation(&self) -> BuiltGraph {
            BuiltGraph::new(graph_with_functions(self.nodes), Arc::clone(&self.record))
        }
    }

    impl WorkspaceBuilder for GenerationBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(self.generation())
        }

        fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(self.generation())
        }
    }

    /// S8 (round 7 audit): a read-only query whose reload the memory budget
    /// cannot admit is refused `MemoryBudgetExceeded` on every query until
    /// the budget admits it, and the first query after that reloads and
    /// serves. Before the repair the refused reload left a `Failed` slot
    /// ("workspace load aborted unexpectedly") that every later query read
    /// without reloading, so the refusal became permanent.
    #[test]
    fn a_budget_refused_reload_is_retried_once_the_budget_admits_it() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        seed_persisted_snapshot(&root);
        let manager = {
            let _env = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // 3 MiB: one reload's 2 MiB fits, two do not.
            WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig {
                memory_limit_mb: 3,
                ..DaemonConfig::default()
            }))
        };
        // Another workspace holds a 2 MiB reservation, which no eviction
        // frees, so the query's reload cannot be admitted while it is held.
        let other = WorkspaceKey::new(root.join("other"), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(other.clone(), WorkspaceState::Loaded);
        let hold = manager
            .reserve_rebuild(&other, RELOAD_WORKING_SET_BYTES)
            .expect("the other workspace's reservation fits");
        let builder: Arc<dyn WorkspaceBuilder> = Arc::new(GenerationBuilder {
            nodes: A_NODES,
            record: Arc::new(RosterRecord::fast_path_default()),
        });
        let provider = DaemonGraphProvider::new(Arc::clone(&manager), builder);
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

        let mut states = Vec::new();
        for query in 1..=3 {
            let refusal = provider
                .acquire_with_roster(read_only(&root))
                .expect_err("the budget cannot admit the reload");
            let state = manager.lookup(&key).map(|ws| ws.load_state());
            println!("query {query} while the budget is full: {refusal:?} state={state:?}");
            assert!(
                matches!(
                    refusal,
                    AcquireRefusal::Daemon(DaemonError::MemoryBudgetExceeded { .. })
                ),
                "query {query}: refused for the budget, as the first query was; got {refusal:?}"
            );
            states.push(state);
        }
        assert!(
            !states.contains(&Some(WorkspaceState::Failed)),
            "a refused admission leaves no Failed slot: {states:?}"
        );

        drop(hold);
        let (acquisition, _roster) = provider
            .acquire_with_roster(read_only(&root))
            .expect("the budget admits the reload now: the query reloads and serves");
        println!(
            "query after the budget freed: source={:?}",
            acquisition.metadata.acquisition_source
        );
        assert_eq!(
            acquisition.metadata.acquisition_source,
            AcquisitionSource::DaemonReloaded
        );
        assert_eq!(acquisition.graph.node_count(), A_NODES as usize);
    }

    /// One evicted read-only acquisition through builder A, with or
    /// without the plant that publishes generation B from inside the
    /// reload arm. Returns `(old_graph, new_roster)`: whether the served
    /// graph is A's and whether the served record is B's.
    fn reload_plant_case(plant: bool) -> (bool, bool) {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        seed_persisted_snapshot(&root);

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        assert!(manager.evict_for_test(&key), "the seeded workspace evicts");

        let record_a = Arc::new(RosterRecord::fast_path_default());
        let record_b = Arc::new(RosterRecord::fast_path_default());
        assert!(
            !Arc::ptr_eq(&record_a, &record_b),
            "the two records are distinct allocations"
        );
        let builder_a: Arc<dyn WorkspaceBuilder> = Arc::new(GenerationBuilder {
            nodes: A_NODES,
            record: Arc::clone(&record_a),
        });
        let builder_b: Arc<dyn WorkspaceBuilder> = Arc::new(GenerationBuilder {
            nodes: B_NODES,
            record: Arc::clone(&record_b),
        });

        let mut provider = DaemonGraphProvider::new(Arc::clone(&manager), builder_a);
        if plant {
            let manager = Arc::clone(&manager);
            let key = key.clone();
            let builder_b = Arc::clone(&builder_b);
            provider = provider.with_reload_pair_plant_for_test(Arc::new(move || {
                assert!(
                    manager.evict_for_test(&key),
                    "the plant evicts the generation the reload published"
                );
                let graph = manager
                    .get_or_load(&key, builder_b.as_ref(), 0)
                    .expect("the plant publishes generation B");
                assert_eq!(
                    graph.node_count(),
                    B_NODES as usize,
                    "the plant's publication is generation B"
                );
            }));
        }

        let (acquisition, roster) = provider
            .acquire_with_roster(make_request(
                root.clone(),
                AcquisitionOperation::ReadOnlyQuery,
            ))
            .expect("the evicted read-only query reloads and serves");
        assert!(
            matches!(
                acquisition.identity.plugin_selection_status,
                PluginSelectionStatus::Exact
            ),
            "no manifest: the record classifies Exact; got {:?}",
            acquisition.identity.plugin_selection_status
        );
        assert_eq!(
            acquisition.metadata.acquisition_source,
            AcquisitionSource::DaemonReloaded
        );
        let old_graph = acquisition.graph.node_count() == A_NODES as usize;
        let new_roster = Arc::ptr_eq(&roster, &record_b);
        let served_nodes = acquisition.graph.node_count();
        println!("R4-1 reload plant: old_graph={old_graph} new_roster={new_roster}");
        assert!(
            served_nodes == A_NODES as usize || served_nodes == B_NODES as usize,
            "the served graph is one of the two generations; got {served_nodes} nodes"
        );
        assert!(
            Arc::ptr_eq(&roster, &record_a) || new_roster,
            "the served record is one of the two generations"
        );

        if plant {
            // Control: the plant did publish, so the slot now holds B.
            let slot = manager
                .lookup(&key)
                .expect("resident after the plant")
                .published();
            assert!(
                Arc::ptr_eq(slot.roster.as_ref().expect("record B"), &record_b),
                "the slot holds record B after the plant"
            );
            assert_eq!(
                slot.graph.node_count(),
                B_NODES as usize,
                "the slot holds graph B after the plant"
            );
        }
        (old_graph, new_roster)
    }

    /// T43 form 1. With the plant, the acquisition must carry A's graph
    /// beside A's record. On the pre-round-4 arm (the record re-read from
    /// the slot after the reload) the same plant yields A's graph beside
    /// B's record: `old_graph=true new_roster=true`.
    #[test]
    fn reload_keeps_the_graph_and_roster_of_one_generation() {
        // Control leg: no plant, one generation by construction.
        let (old_graph, new_roster) = reload_plant_case(false);
        assert!(
            old_graph && !new_roster,
            "control: without the plant the reload serves A's graph and A's record; \
             old_graph={old_graph} new_roster={new_roster}"
        );

        // The plant.
        let (old_graph, new_roster) = reload_plant_case(true);
        assert!(
            old_graph,
            "the reload serves the graph it published (A); old_graph={old_graph}"
        );
        assert!(
            !new_roster,
            "reload must keep the graph and roster from one generation \
             (old_graph={old_graph} new_roster={new_roster})"
        );
    }

    /// T43 form 2 (structural control): `reload_from_disk_read_only`
    /// returns the pair it published, both halves from the double, and the
    /// slot holds that same pair immediately after. A compile error on the
    /// pre-round-4 head, where the reload returned `Arc<CodeGraph>`.
    #[test]
    fn reload_returns_the_pair_it_published_control() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        seed_persisted_snapshot(&root);

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        assert!(manager.evict_for_test(&key), "the seeded workspace evicts");

        let record_a = Arc::new(RosterRecord::fast_path_default());
        let builder_a: Arc<dyn WorkspaceBuilder> = Arc::new(GenerationBuilder {
            nodes: A_NODES,
            record: Arc::clone(&record_a),
        });

        let published = manager
            .reload_from_disk_read_only(&key, builder_a.as_ref(), RELOAD_WORKING_SET_BYTES)
            .expect("the reload publishes generation A");
        assert!(
            Arc::ptr_eq(published.roster.as_ref().expect("record A"), &record_a),
            "the returned record is the double's"
        );
        assert_eq!(
            published.graph.node_count(),
            A_NODES as usize,
            "the returned graph is the double's generation"
        );
        let slot = manager
            .lookup(&key)
            .expect("resident after the reload")
            .published();
        assert!(
            Arc::ptr_eq(&slot, &published),
            "the slot holds the pair the reload returned"
        );
        assert!(
            Arc::ptr_eq(&slot.graph, &published.graph),
            "the slot's graph half is the returned graph"
        );
        println!(
            "T43 form 2: returned_pair_is_slot={} nodes={}",
            Arc::ptr_eq(&slot, &published),
            published.graph.node_count()
        );
    }

    /// T43 form 3 (structural control; battery row K37's oracle): when
    /// the load gate finds the key already `Loaded`, `get_or_load_published`
    /// hands back the slot's own generation, the very `Arc` one load of the
    /// slot returns, not a value re-paired from two loads (`ws.graph()`
    /// beside `ws.roster()`), which a publisher between the two loads could
    /// split. Pointer identity is the deterministic form of that claim: a
    /// re-paired copy is a fresh allocation and can never be the slot's
    /// `Arc`. A compile error on the pre-round-4 head (`get_or_load_published`
    /// absent).
    #[test]
    fn loaded_gate_hands_back_the_slot_pair_control() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        let slot_before = manager.lookup(&key).expect("seeded workspace").published();

        let counting = Arc::new(CountingLoadPersistedBuilder::default());
        let published = manager
            .get_or_load_published(&key, counting.as_ref(), 0)
            .expect("a Loaded key is served from the gate");
        assert_eq!(
            *counting.load_persisted_count.lock(),
            0,
            "the gate served the resident generation without a load"
        );
        assert!(
            Arc::ptr_eq(&published, &slot_before),
            "the gate hands back the slot's own generation, not a re-paired copy"
        );
        let slot_after = manager.lookup(&key).expect("still resident").published();
        assert!(
            Arc::ptr_eq(&published, &slot_after),
            "the slot still holds the generation the gate handed back"
        );
        println!(
            "T43 form 3: gate_pair_is_slot={} loads={}",
            Arc::ptr_eq(&published, &slot_before),
            *counting.load_persisted_count.lock()
        );
    }

    // -----------------------------------------------------------------
    // T50 and T52 (surface parity W1 round 5, design D24, codex R5-1):
    // the load gate observes the state and captures the generation under
    // one guard at both `Loaded` returns, and refuses a `Loaded` slot that
    // carries no record. T50 reproduces codex's plant deterministically:
    // the observation plant publishes generation A when the gate's first
    // lookup misses (so the gate's compare-exchange loses to a `Loaded`
    // slot and the second return is reached), then attempts an eviction
    // at the instant the state was observed `Loaded` and the generation
    // is about to be loaded. Under the repaired gate that attempt cannot
    // take the write lock (`WouldBlock`) and the return is A; under the
    // pre-round-5 arm it evicted and the return was the placeholder.
    // -----------------------------------------------------------------

    use crate::workspace::manager::{ObservationPhase, TryEvictOutcome};

    /// The two callers of the gate, named for the printed line.
    #[derive(Clone, Copy)]
    enum GateCaller {
        GetOrLoadPublished,
        ReloadFromDiskReadOnly,
    }

    impl GateCaller {
        fn name(self) -> &'static str {
            match self {
                Self::GetOrLoadPublished => "get_or_load_published",
                Self::ReloadFromDiskReadOnly => "reload_from_disk_read_only",
            }
        }

        fn call(
            self,
            manager: &Arc<WorkspaceManager>,
            key: &WorkspaceKey,
            builder: &dyn WorkspaceBuilder,
        ) -> Result<Arc<PublishedGraph>, DaemonError> {
            match self {
                Self::GetOrLoadPublished => manager.get_or_load_published(key, builder, 0),
                Self::ReloadFromDiskReadOnly => manager.reload_from_disk_read_only(key, builder, 0),
            }
        }
    }

    /// What one leg of T50 observed: the counted phases, the eviction
    /// outcome the plant recorded (if it attempted one), and the gate's
    /// answer.
    struct GatePlantObservation {
        phases: Vec<ObservationPhase>,
        evict: Option<TryEvictOutcome>,
        published: Arc<PublishedGraph>,
        slot_state_after_gate: WorkspaceState,
    }

    /// One leg of T50: a fresh manager with no entry for the key and a
    /// plant that publishes A at the first `GateFirstLookupMissed` and,
    /// when `attempt_eviction`, records `try_evict_for_test` at the first
    /// `GateLoadedObserved`. Returns the observation; the caller asserts.
    fn gate_plant_leg(
        caller: GateCaller,
        attempt_eviction: bool,
        record_a: &Arc<RosterRecord>,
        counting: &CountingLoadPersistedBuilder,
    ) -> (Arc<WorkspaceManager>, WorkspaceKey, GatePlantObservation) {
        use std::sync::Weak;

        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        seed_persisted_snapshot(&root);

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let builder_a: Arc<dyn WorkspaceBuilder> = Arc::new(GenerationBuilder {
            nodes: A_NODES,
            record: Arc::clone(record_a),
        });

        let phases: Arc<parking_lot::Mutex<Vec<ObservationPhase>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let evict: Arc<parking_lot::Mutex<Option<TryEvictOutcome>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let weak: Weak<WorkspaceManager> = Arc::downgrade(&manager);
        let plant_key = key.clone();
        let plant_phases = Arc::clone(&phases);
        let plant_evict = Arc::clone(&evict);
        manager.install_observation_plant_for_test(Arc::new(move |phase| {
            let first_of_its_kind = !plant_phases.lock().contains(&phase);
            plant_phases.lock().push(phase);
            let Some(manager) = weak.upgrade() else {
                return;
            };
            match phase {
                ObservationPhase::GateFirstLookupMissed if first_of_its_kind => {
                    // The publication that makes the gate's CAS lose: the
                    // nested gate takes the CAS from `Unloaded`, builds A,
                    // publishes and stores `Loaded`. Its own phases are
                    // counted above and otherwise ignored.
                    manager
                        .get_or_load(&plant_key, builder_a.as_ref(), 0)
                        .expect("the plant publishes generation A");
                }
                ObservationPhase::GateLoadedObserved if attempt_eviction && first_of_its_kind => {
                    *plant_evict.lock() = Some(manager.try_evict_for_test(&plant_key));
                }
                _ => {}
            }
        }));

        let published = caller
            .call(&manager, &key, counting)
            .expect("a Loaded slot is served from the gate's second return");
        let slot_state_after_gate = manager
            .lookup(&key)
            .expect("the slot is resident after the gate answered")
            .load_state();
        let observation = GatePlantObservation {
            phases: phases.lock().clone(),
            evict: *evict.lock(),
            published,
            slot_state_after_gate,
        };
        (manager, key, observation)
    }

    /// T50 (design D24, battery row K39's oracle):
    /// `loaded_gate_never_hands_back_the_eviction_placeholder`. For each
    /// caller of the gate, the plant leg (an eviction attempted at
    /// `GateLoadedObserved`) and the control leg (no attempt). Every
    /// printed value is asserted; the phase counts prove the second
    /// return was reached, so the test cannot pass vacuously through the
    /// first return.
    #[test]
    fn loaded_gate_never_hands_back_the_eviction_placeholder() {
        const CALLERS: [GateCaller; 2] = [
            GateCaller::GetOrLoadPublished,
            GateCaller::ReloadFromDiskReadOnly,
        ];

        // Observe both callers first, so the printed line of each is the
        // record whatever the first assertion says.
        let legs: Vec<_> = CALLERS
            .into_iter()
            .map(|caller| {
                let record_a = Arc::new(RosterRecord::fast_path_default());
                let counting = CountingLoadPersistedBuilder::default();
                let (manager, key, seen) = gate_plant_leg(caller, true, &record_a, &counting);
                let evict = seen.evict.expect("the plant reached GateLoadedObserved");
                println!(
                    "R5-1 gate plant: caller={} evict={evict:?} nodes={} roster_present={} \
                     slot_state={:?}",
                    caller.name(),
                    seen.published.graph.node_count(),
                    seen.published.roster.is_some(),
                    seen.slot_state_after_gate
                );
                (caller, record_a, counting, manager, key, seen, evict)
            })
            .collect();

        for (caller, record_a, counting, manager, key, seen, evict) in legs {
            let nodes = seen.published.graph.node_count();
            let roster_present = seen.published.roster.is_some();
            let count = |wanted: ObservationPhase| {
                seen.phases.iter().filter(|phase| **phase == wanted).count()
            };
            assert_eq!(
                (nodes, roster_present),
                (A_NODES as usize, true),
                "{}: a successful Loaded return must carry a published graph and its roster, \
                 never an eviction placeholder",
                caller.name()
            );
            assert_eq!(
                (
                    count(ObservationPhase::GateFirstLookupMissed),
                    count(ObservationPhase::GateCasLost),
                    count(ObservationPhase::GateLoadedObserved),
                ),
                (2, 1, 1),
                "{}: the outer gate's first lookup missed, the nested publication's first \
                 lookup missed, the outer CAS lost and the second Loaded return was reached \
                 (phases seen: {:?})",
                caller.name(),
                seen.phases
            );
            let served_record = seen
                .published
                .roster
                .as_ref()
                .expect("roster_present asserted above");
            assert!(
                Arc::ptr_eq(served_record, &record_a),
                "{}: the served record is A's by pointer",
                caller.name()
            );
            assert_eq!(
                evict,
                TryEvictOutcome::WouldBlock,
                "{}: the eviction attempted at GateLoadedObserved cannot take the write lock \
                 while the gate holds its read guard",
                caller.name()
            );
            assert_eq!(
                seen.slot_state_after_gate,
                WorkspaceState::Loaded,
                "{}: the slot is still Loaded when the gate answers",
                caller.name()
            );
            assert_eq!(
                (
                    *counting.build_count.lock(),
                    *counting.load_persisted_count.lock()
                ),
                (0, 0),
                "{}: the caller's own builder is untouched (the resident generation was served)",
                caller.name()
            );
            // Control that the guard deferred the eviction and did not lose it.
            assert!(
                manager.evict_for_test(&key),
                "{}: the eviction the plant could not perform runs once the gate has answered",
                caller.name()
            );
            let slot = manager.lookup(&key).expect("tombstone stays in the map");
            assert_eq!(slot.load_state(), WorkspaceState::Evicted);
            assert!(
                slot.roster().is_none(),
                "{}: the tombstone carries the placeholder (no record)",
                caller.name()
            );
        }

        // Control legs: the plant publishes A and attempts nothing.
        for caller in CALLERS {
            let record_a = Arc::new(RosterRecord::fast_path_default());
            let counting = CountingLoadPersistedBuilder::default();
            let (_manager, _key, seen) = gate_plant_leg(caller, false, &record_a, &counting);
            println!(
                "R5-1 gate control: caller={} evict=None nodes={} roster_present={} slot_state={:?}",
                caller.name(),
                seen.published.graph.node_count(),
                seen.published.roster.is_some(),
                seen.slot_state_after_gate
            );
            assert!(seen.evict.is_none(), "the control leg attempts no eviction");
            assert_eq!(seen.published.graph.node_count(), A_NODES as usize);
            assert!(
                seen.published
                    .roster
                    .as_ref()
                    .is_some_and(|record| Arc::ptr_eq(record, &record_a)),
                "{}: the control leg serves A's record",
                caller.name()
            );
            assert_eq!(seen.slot_state_after_gate, WorkspaceState::Loaded);
            assert_eq!(
                (
                    *counting.build_count.lock(),
                    *counting.load_persisted_count.lock()
                ),
                (0, 0)
            );
        }
    }

    /// T52 (design D24, battery row K39b's oracle):
    /// `loaded_gate_refuses_a_loaded_slot_without_a_record`. A `Loaded`
    /// slot seeded without a record is the state every publish path is
    /// required to make unreachable; the gate refuses it as
    /// `classify_for_serve` does (T13) instead of handing the placeholder
    /// back under the label `Loaded`. Pre-round-5: `Ok` holding the
    /// placeholder (nodes 0, no record).
    #[test]
    fn loaded_gate_refuses_a_loaded_slot_without_a_record() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();

        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_without_roster_for_test(key.clone(), WorkspaceState::Loaded);

        let counting = CountingLoadPersistedBuilder::default();
        let outcome = manager.get_or_load_published(&key, &counting, 0);
        let (is_internal, text) = match &outcome {
            Ok(published) => (
                false,
                format!(
                    "Ok(nodes={} roster_present={})",
                    published.graph.node_count(),
                    published.roster.is_some()
                ),
            ),
            Err(DaemonError::Internal(err)) => (true, format!("Internal: {err}")),
            Err(other) => (false, format!("{other:?}")),
        };
        println!("R5-1 record-less gate: is_internal={is_internal} outcome={text}");
        assert!(
            is_internal,
            "a Loaded slot without a record must be refused as DaemonError::Internal, not \
             handed back: {text}"
        );
        assert!(
            text.contains(&root.display().to_string()) && text.contains("no roster record"),
            "the refusal names the root and the missing record: {text}"
        );
        assert_eq!(
            (
                *counting.build_count.lock(),
                *counting.load_persisted_count.lock()
            ),
            (0, 0),
            "the refusal builds and loads nothing"
        );
        let slot = manager.lookup(&key).expect("the slot stays resident");
        assert_eq!(
            slot.load_state(),
            WorkspaceState::Loaded,
            "the refusal changes nothing: the slot is still Loaded"
        );
        assert!(
            slot.roster().is_none(),
            "the refusal changes nothing: the slot still carries no record"
        );
    }

    // -----------------------------------------------------------------
    // Test 6 — `evict_for_test` is not reachable via public re-export
    // -----------------------------------------------------------------
    //
    // Two-level guard for the SGA04 Gate-A blocker fix:
    //   1. The crate's `lib.rs` must not re-export the symbol.
    //   2. The `manager.rs` definition must be wrapped in
    //      `#[cfg(any(test, feature = "test-hooks"))]` immediately
    //      preceding the `pub fn evict_for_test` declaration, so
    //      default release builds (`cargo build -p sqry-daemon`) do
    //      not compile the symbol at all.
    //
    // Compile-fail is overkill for this affordance. Source-text
    // assertions match the rest of the daemon's structural invariants
    // (cf. `mcp_host` envelope-shape tests).
    #[test]
    fn evict_for_test_is_not_reachable_via_public_re_export() {
        let lib_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
            .expect("read sqry-daemon/src/lib.rs");
        // The substring `evict_for_test` must not appear in the public
        // re-export prelude — a search across the whole file is enough
        // because the file does not name the symbol anywhere else.
        assert!(
            !lib_rs.contains("evict_for_test"),
            "evict_for_test must NOT be re-exported through sqry-daemon's public API \
             (release/IPC/MCP/HTTP surfaces would otherwise reach a test-only hook)"
        );

        // Layer 2: the definition itself must carry the
        // `#[cfg(any(test, feature = "test-hooks"))]` gate. We scan the
        // raw source text of `manager.rs` and assert the gate appears
        // on the same definition site — guarding against a future
        // refactor that drops the cfg and re-exposes the helper to
        // release builds.
        let manager_rs = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/workspace/manager.rs"
        ))
        .expect("read sqry-daemon/src/workspace/manager.rs");
        // Match exact definition with the cfg attribute on a preceding
        // line. `#[doc(hidden)]` may sit between the cfg and the fn,
        // so allow whitespace and other attributes between them.
        let needle = "#[cfg(any(test, feature = \"test-hooks\"))]";
        let fn_decl = "pub fn evict_for_test(";
        let cfg_pos = manager_rs.find(needle).unwrap_or_else(|| {
            panic!(
                "expected `{needle}` somewhere in manager.rs to gate evict_for_test \
                 (SGA04 Gate-A blocker fix)"
            )
        });
        let fn_pos = manager_rs
            .find(fn_decl)
            .unwrap_or_else(|| panic!("expected `{fn_decl}` definition in manager.rs"));
        assert!(
            cfg_pos < fn_pos,
            "`{needle}` must appear BEFORE `{fn_decl}` so it gates the definition; \
             evict_for_test must be unreachable in default release builds"
        );
        // Sanity: the segment between the cfg and the fn must only
        // contain attributes / whitespace / comments — no other top-level
        // item should sneak in and steal the gate.
        let between = &manager_rs[cfg_pos..fn_pos];
        assert!(
            between.matches("\nfn ").count() == 0
                && between.matches("\npub fn ").count() == 0
                && between.matches("\nstruct ").count() == 0
                && between.matches("\nimpl ").count() == 0,
            "no other item may appear between the cfg gate and `{fn_decl}`; \
             between segment was: {between:?}"
        );
    }

    // -----------------------------------------------------------------
    // The absent index and the failed reload (round 7 integration repair)
    // -----------------------------------------------------------------

    /// Builder whose `load_persisted` answers with the error `refuse`
    /// makes for the root, counting the attempts. `build` succeeds, so a
    /// test can tell a reload from a build by the counter.
    struct CountingRefusingBuilder {
        refuse: fn(&Path) -> DaemonError,
        loads: parking_lot::Mutex<u32>,
    }

    impl CountingRefusingBuilder {
        fn new(refuse: fn(&Path) -> DaemonError) -> Arc<Self> {
            Arc::new(Self {
                refuse,
                loads: parking_lot::Mutex::new(0),
            })
        }

        fn loads(&self) -> u32 {
            *self.loads.lock()
        }
    }

    impl std::fmt::Debug for CountingRefusingBuilder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CountingRefusingBuilder")
                .finish_non_exhaustive()
        }
    }

    impl WorkspaceBuilder for CountingRefusingBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, root: &Path) -> Result<BuiltGraph, DaemonError> {
            *self.loads.lock() += 1;
            Err((self.refuse)(root))
        }
    }

    fn not_indexed(root: &Path) -> DaemonError {
        DaemonError::workspace_not_indexed(
            root,
            root.join(".sqry").join("graph").join("manifest.json"),
        )
    }

    fn corrupt_snapshot(root: &Path) -> DaemonError {
        DaemonError::WorkspaceBuildFailed {
            root: root.to_path_buf(),
            reason: "snapshot load failed: checksum mismatch".to_string(),
        }
    }

    fn read_only(root: &Path) -> GraphAcquisitionRequest {
        make_request(root.to_path_buf(), AcquisitionOperation::ReadOnlyQuery)
    }

    /// The production builder over a root with no `.sqry/`: the refusal is
    /// the typed absent index, `-32001`, naming the manifest and
    /// `sqry index <root>`, never `-32004`.
    #[test]
    fn never_loaded_root_without_an_index_is_refused_as_not_indexed() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let builder = Arc::new(crate::workspace::RealWorkspaceBuilder::new(Arc::new(
            crate::workspace::WorkspaceRosterResolver::new(),
        )));
        let provider = DaemonGraphProvider::new(Arc::clone(&manager), builder);

        let refusal = provider
            .acquire_with_roster(read_only(&root))
            .expect_err("a root with no index cannot be served");
        let err = DaemonError::from(refusal);
        assert_eq!(err.jsonrpc_code(), Some(-32001), "{err}");
        match &err {
            DaemonError::WorkspaceNotIndexed {
                root: refused_root,
                missing_path,
                repair_command,
            } => {
                assert_eq!(refused_root, &root);
                assert_eq!(
                    missing_path,
                    &root.join(".sqry").join("graph").join("manifest.json")
                );
                assert_eq!(repair_command, &format!("sqry index {}", root.display()));
            }
            other => panic!("expected WorkspaceNotIndexed, got {other:?}"),
        }
    }

    /// A slot the absent-index refusal left `Failed` over the placeholder
    /// answers that refusal, typed, on every later query: from the record,
    /// with no load attempt, while the file it names is still absent (a
    /// reload would reserve admission only to be refused again), and from a
    /// fresh reload once the file appears. Before the repair the later
    /// answers were the flattened `-32001` build failure `classify_for_serve`
    /// gives for `Failed` with no prior good, which no repair on disk cleared.
    #[test]
    fn failed_slot_left_by_an_absent_index_is_reloaded_once_the_file_appears() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let builder = CountingRefusingBuilder::new(not_indexed);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );
        let assert_not_indexed = |query: &str| {
            let refusal = provider
                .acquire_with_roster(read_only(&root))
                .expect_err("no index, no graph");
            assert!(
                matches!(
                    refusal,
                    AcquireRefusal::Daemon(DaemonError::WorkspaceNotIndexed { .. })
                ),
                "{query} must be refused as the absent index, got {refusal:?}"
            );
        };

        assert_not_indexed("the first query");
        assert_eq!(builder.loads(), 1, "the first query loads once");
        assert_not_indexed("the second query");
        assert_eq!(
            builder.loads(),
            1,
            "the manifest is still absent: answered from the record, no load"
        );
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let ws = manager
            .lookup(&key)
            .expect("the reload registered the slot");
        assert_eq!(ws.load_state(), WorkspaceState::Failed);
        assert!(ws.roster().is_none(), "nothing was published");

        let graph_dir = root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
        assert_not_indexed("the query after the manifest appears");
        assert_eq!(builder.loads(), 2, "the named file appeared: one reload");
    }

    /// A never-loaded slot that holds a recorded refusal of a request (B2,
    /// round 7 audit: what a refused `daemon/load` or `rebuild_index` load
    /// route used to leave) is answered with that refusal, typed, never the
    /// catch-all `GraphAcquisitionError::Internal` ("classify_for_serve
    /// returned unexpected error", `-32603`), which read as a daemon bug.
    #[test]
    fn a_recorded_request_refusal_is_answered_typed_not_internal() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_without_roster_for_test(key.clone(), WorkspaceState::Failed);
        let reason = format!(
            "rebuild of {} refused: the index manifest records cfg flag \" a\"",
            root.display()
        );
        manager
            .lookup(&key)
            .expect("inserted")
            .record_failure(DaemonError::InvalidArgument {
                reason: reason.clone(),
            });
        let builder = CountingRefusingBuilder::new(not_indexed);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );
        let refusal = provider
            .acquire_with_roster(read_only(&root))
            .expect_err("the slot has nothing to serve");
        println!("recorded request refusal answered as: {refusal:?}");
        match refusal {
            AcquireRefusal::Daemon(DaemonError::InvalidArgument { reason: answered }) => {
                assert_eq!(answered, reason, "the recorded refusal, unchanged");
            }
            other => panic!("expected the recorded InvalidArgument, got {other:?}"),
        }
        assert_eq!(builder.loads(), 0, "answered from the record, no load");
    }

    /// A workspace that had been loaded, then evicted, whose reload fails,
    /// is `Failed` over the placeholder with `last_good_at` still set. The
    /// next query reloads again and carries the failure; before the repair
    /// it was refused as the internal "stale-servable but carries no roster
    /// record" error, which names a publish-path bug that did not happen.
    #[test]
    fn evicted_slot_whose_reload_failed_is_reloaded_on_the_next_query() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        manager
            .lookup(&key)
            .expect("inserted")
            .record_success(SystemTime::now());
        assert!(manager.evict_for_test(&key));

        let builder = CountingRefusingBuilder::new(corrupt_snapshot);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );

        for attempt in 1..=2u32 {
            let err = provider
                .acquire(read_only(&root))
                .expect_err("the reload fails");
            match err {
                GraphAcquisitionError::Evicted {
                    original_lifecycle,
                    reload_failure,
                    ..
                } => {
                    assert_eq!(original_lifecycle, "evicted");
                    let reload = reload_failure.expect("the failure is carried");
                    assert!(
                        reload.contains("snapshot load failed: checksum mismatch"),
                        "query {attempt}: {reload}"
                    );
                }
                other => panic!("query {attempt}: expected Evicted, got {other:?}"),
            }
            assert_eq!(builder.loads(), attempt, "one reload per query");
        }
    }

    /// Control, the predicate's reject side: a never-loaded slot whose
    /// failure was a build (`daemon/load`) keeps its `-32001` and its
    /// recorded reason, and the query attempts no reload.
    #[test]
    fn never_loaded_slot_whose_build_failed_is_not_reloaded() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager
            .get_or_load(
                &key,
                &crate::workspace::FailingGraphBuilder::new("plugin panic"),
                RELOAD_WORKING_SET_BYTES,
            )
            .expect_err("the build fails");

        let builder = CountingRefusingBuilder::new(not_indexed);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );
        let refusal = provider
            .acquire_with_roster(read_only(&root))
            .expect_err("a failed build serves nothing");
        let err = DaemonError::from(refusal);
        assert_eq!(err.jsonrpc_code(), Some(-32001));
        assert!(
            matches!(&err, DaemonError::WorkspaceBuildFailed { reason, .. } if reason.contains("plugin panic")),
            "the recorded build failure is the answer, got {err:?}"
        );
        assert_eq!(builder.loads(), 0, "no reload over a failed build");
    }

    /// Control, the predicate's other reject side: a `Failed` slot that
    /// holds a published generation (a record) is not the placeholder, so
    /// it is never re-labelled; with no prior good it keeps `-32001`.
    #[test]
    fn failed_slot_holding_a_generation_is_not_reloaded() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Failed);
        manager
            .lookup(&key)
            .expect("inserted")
            .record_failure(not_indexed(&root));

        let builder = CountingRefusingBuilder::new(not_indexed);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );
        let refusal = provider
            .acquire_with_roster(read_only(&root))
            .expect_err("Failed with no prior good serves nothing");
        assert!(
            matches!(
                refusal,
                AcquireRefusal::Acquisition(GraphAcquisitionError::BuildFailed { .. })
            ),
            "got {refusal:?}"
        );
        assert_eq!(builder.loads(), 0, "a slot with a record is not reloaded");
    }

    /// A never-loaded root whose load fails for a reason other than the
    /// absent index answers with the load's own error: nothing was evicted,
    /// so the refusal is not `-32004`.
    #[test]
    fn never_loaded_root_whose_load_fails_answers_with_the_load_error() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let builder = CountingRefusingBuilder::new(corrupt_snapshot);
        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::clone(&builder) as Arc<dyn WorkspaceBuilder>,
        );
        let err = DaemonError::from(
            provider
                .acquire_with_roster(read_only(&root))
                .expect_err("the load fails"),
        );
        assert_eq!(err.jsonrpc_code(), Some(-32001), "{err}");
        assert!(
            err.to_string()
                .contains("snapshot load failed: checksum mismatch"),
            "{err}"
        );
        assert_eq!(builder.loads(), 1);
    }

    /// `WorkspaceStaleExpired` keeps the daemon's own fields: routed
    /// through the shared `StaleExpired` it lost its cap (rendered as 0),
    /// its last good time and its last error.
    #[test]
    fn stale_expired_keeps_its_cap_and_last_error() {
        let tmp = TempDir::new().unwrap();
        let root = canonicalize_path(tmp.path()).unwrap();
        let manager = make_manager();
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Failed);
        let ws = manager.lookup(&key).expect("inserted");
        let last_good = SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        ws.set_last_good_at_for_test(Some(last_good));
        ws.record_failure(corrupt_snapshot(&root));

        let provider = DaemonGraphProvider::new(
            Arc::clone(&manager),
            Arc::new(InMemoryBuilder) as Arc<dyn WorkspaceBuilder>,
        );
        let err = DaemonError::from(
            provider
                .acquire_with_roster(read_only(&root))
                .expect_err("48h is past the cap"),
        );
        // The cap `make_manager`'s default config carries; read from the
        // constant because `DaemonConfig::default()` reads the environment.
        let cap = crate::config::DEFAULT_STALE_SERVE_MAX_AGE_HOURS;
        match err {
            DaemonError::WorkspaceStaleExpired {
                cap_hours,
                last_good_at,
                last_error,
                age_hours,
                ..
            } => {
                assert_eq!(cap_hours, cap, "the configured cap, not 0");
                assert!(age_hours >= u64::from(cap));
                assert_eq!(last_good_at, Some(last_good));
                assert!(
                    last_error
                        .as_deref()
                        .is_some_and(|e| e.contains("checksum mismatch")),
                    "{last_error:?}"
                );
            }
            other => panic!("expected WorkspaceStaleExpired, got {other:?}"),
        }
    }
}
