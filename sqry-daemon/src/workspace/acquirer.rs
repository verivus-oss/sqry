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
//!   [`WorkspaceManager::reload_from_disk_read_only`]. Repeated eviction
//!   surfaces as [`GraphAcquisitionError::Evicted`] with both the
//!   original lifecycle and the reload-failure context preserved.
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

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(test, feature = "test-hooks"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use sqry_core::graph::acquisition::{
    AcquisitionOperation, AcquisitionSource, GraphAcquirer, GraphAcquisition,
    GraphAcquisitionError, GraphAcquisitionMetadata, GraphAcquisitionRequest, GraphFreshness,
    GraphIdentity, PluginSelectionStatus, ReloadOrigin,
};
use sqry_core::project::ProjectRootMode;

use crate::error::DaemonError;
use crate::ipc::tool_core;
use crate::workspace::{ServeVerdict, WorkspaceBuilder, WorkspaceKey, WorkspaceManager};

fn u64_hours_to_f64(hours: u64) -> f64 {
    f64::from(u32::try_from(hours).unwrap_or(u32::MAX))
}

/// Initial admission-control reservation used by the daemon provider's
/// bounded read-only rehydrate. The same constant the MCP host uses for
/// initial loads — keeps admission accounting consistent across daemon
/// read-only paths.
const RELOAD_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024;

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
        }
    }

    /// Tag the provider with a fixed tool name for diagnostics. The
    /// resulting [`GraphAcquisitionMetadata::tool_name`] field is
    /// surfaced in logs and the canonical 4-key error envelope.
    #[allow(dead_code)] // SGA05 wires the per-tool tag.
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

    /// Build a successful [`GraphAcquisition`] from a captured graph
    /// arc plus per-state freshness metadata. The acquisition source is
    /// always one of [`AcquisitionSource::DaemonReadOnly`] or
    /// [`AcquisitionSource::DaemonReloaded`].
    fn acquisition_from_parts(
        &self,
        graph: Arc<sqry_core::graph::CodeGraph>,
        canonical_root: PathBuf,
        request: &GraphAcquisitionRequest,
        freshness: GraphFreshness,
        source: AcquisitionSource,
    ) -> GraphAcquisition {
        // SGA02 contract: `query_scope` is `None` when the request
        // targeted the workspace root. The daemon provider canonicalises
        // to the workspace root itself, so we set the scope based on
        // whether the canonical path differs from the request.
        let (query_scope, is_file_scope) = scope_for_request(request, &canonical_root);
        // The tool name reaches the metadata via the request OR the
        // provider tag — request wins so per-call diagnostics dominate.
        let tool_name = request.tool_name.or(self.tool_name);
        GraphAcquisition {
            graph,
            workspace_root: canonical_root.clone(),
            query_scope,
            is_file_scope,
            freshness,
            identity: GraphIdentity {
                snapshot_sha256: None,
                manifest_built_at: None,
                snapshot_format_version: None,
                source_root: canonical_root,
                // The daemon path does not validate manifest plugin
                // selection here — `WorkspaceManager` already loaded the
                // graph via the configured plugin manager. Surfacing
                // `Exact` matches the spec contract for graphs that the
                // daemon successfully serves.
                plugin_selection_status: PluginSelectionStatus::Exact,
            },
            metadata: GraphAcquisitionMetadata {
                acquisition_source: source,
                tool_name,
                notes: vec![],
            },
        }
    }
}

impl GraphAcquirer for DaemonGraphProvider {
    fn acquire(
        &self,
        request: GraphAcquisitionRequest,
    ) -> Result<GraphAcquisition, GraphAcquisitionError> {
        // ----- Step 0: test instrumentation --------------------------
        //
        // Bump the process-wide acquisition counter at the very top of
        // the function — before path validation, classification, or
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
                });
            }
        };

        // ----- Step 2: mutating-rebuild guard ------------------------
        //
        // The daemon's `rebuild_index` flow drives `get_or_load` (build
        // pipeline + durable publish) directly. It MUST NOT silently
        // fall back to the read-only persisted-graph reload path the
        // `ReadOnlyQuery` branch uses below — the durable rebuild
        // contract owns those semantics, not this provider.
        if matches!(request.operation, AcquisitionOperation::MutatingRebuild) {
            return Err(GraphAcquisitionError::Internal {
                reason: format!(
                    "daemon graph provider does not serve MutatingRebuild for {}; \
                     route through WorkspaceManager::get_or_load via the explicit \
                     rebuild_index flow",
                    canonical_root.display()
                ),
            });
        }

        // ----- Step 3: classify + map verdict ------------------------
        let key = Self::key_for(&canonical_root);
        let now = SystemTime::now();
        match self.manager.classify_for_serve(&key, now) {
            Ok(ServeVerdict::Fresh { graph, state }) => {
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
                Ok(self.acquisition_from_parts(
                    graph,
                    canonical_root,
                    &request,
                    freshness,
                    AcquisitionSource::DaemonReadOnly,
                ))
            }
            Ok(ServeVerdict::Stale {
                graph,
                age_hours,
                last_good_at,
                last_error,
            }) => {
                let freshness = GraphFreshness::Stale {
                    last_good_at: Some(rfc3339_utc(last_good_at)),
                    last_error,
                    age_hours: Some(u64_hours_to_f64(age_hours)),
                };
                Ok(self.acquisition_from_parts(
                    graph,
                    canonical_root,
                    &request,
                    freshness,
                    AcquisitionSource::DaemonReadOnly,
                ))
            }
            Ok(ServeVerdict::NotReady { state }) => Err(GraphAcquisitionError::NotReady {
                workspace_root: canonical_root,
                lifecycle: format!("{state:?}"),
            }),
            Err(daemon_err) => {
                self.handle_classify_error(&request, &key, canonical_root, daemon_err)
            }
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
        canonical_root: PathBuf,
        daemon_err: DaemonError,
    ) -> Result<GraphAcquisition, GraphAcquisitionError> {
        match daemon_err {
            // Bounded one-shot reload — only for ReadOnlyQuery. The
            // operation enum was already exhausted upstream
            // (`MutatingRebuild` is rejected before this match), so
            // reaching this arm implies ReadOnlyQuery.
            DaemonError::WorkspaceEvicted { ref root } => {
                debug_assert!(
                    matches!(request.operation, AcquisitionOperation::ReadOnlyQuery),
                    "MutatingRebuild must be rejected before classify_for_serve",
                );
                let original_lifecycle = "evicted".to_string();
                let original_detail = format!("workspace {} evicted", root.display());
                match self.manager.reload_from_disk_read_only(
                    key,
                    self.builder.as_ref(),
                    RELOAD_WORKING_SET_BYTES,
                ) {
                    Ok(graph) => {
                        let freshness = GraphFreshness::Reloaded {
                            original_lifecycle: ReloadOrigin::Evicted {
                                detail: original_detail,
                            },
                            final_lifecycle_label: "loaded",
                            reload_attempts: std::num::NonZeroU8::new(1).expect("1 is non-zero"),
                        };
                        Ok(self.acquisition_from_parts(
                            graph,
                            canonical_root,
                            request,
                            freshness,
                            AcquisitionSource::DaemonReloaded,
                        ))
                    }
                    Err(reload_err) => Err(GraphAcquisitionError::Evicted {
                        workspace_root: canonical_root,
                        original_lifecycle,
                        reload_failure: Some(format!(
                            "evicted({original_detail}); reload: {reload_err}"
                        )),
                    }),
                }
            }
            DaemonError::WorkspaceStaleExpired {
                root: _,
                age_hours,
                cap_hours: _,
                last_good_at: _,
                last_error: _,
            } => Err(GraphAcquisitionError::StaleExpired {
                workspace_root: canonical_root,
                age_hours: Some(u64_hours_to_f64(age_hours)),
            }),
            DaemonError::WorkspaceBuildFailed { root: _, reason } => {
                Err(GraphAcquisitionError::BuildFailed {
                    workspace_root: canonical_root,
                    reason,
                })
            }
            DaemonError::WorkspaceNotLoaded { root: _ } => Err(GraphAcquisitionError::NoGraph {
                workspace_root: canonical_root,
            }),
            other => Err(GraphAcquisitionError::Internal {
                reason: format!("daemon classify_for_serve returned unexpected error: {other}"),
            }),
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
/// pair. The daemon canonicalises every accepted path to a workspace
/// root (see [`crate::ipc::path_policy`]), so the scope is always
/// `None` here — the field is reserved for filesystem-provider sub-
/// directory / file scopes.
fn scope_for_request(
    _request: &GraphAcquisitionRequest,
    _canonical_root: &std::path::Path,
) -> (Option<PathBuf>, bool) {
    (None, false)
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
    use crate::workspace::WorkspaceState;

    // -----------------------------------------------------------------
    // Builder fakes
    // -----------------------------------------------------------------

    /// Builder that always yields an empty graph for `build` AND
    /// `load_persisted` — so the read-only reload path succeeds without
    /// touching disk in the parity tests.
    #[derive(Debug, Default)]
    struct InMemoryBuilder;

    impl WorkspaceBuilder for InMemoryBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }

        fn load_persisted(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
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
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }

        fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
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
    }

    impl WorkspaceBuilder for CountingLoadPersistedBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }

        fn load_persisted(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            *self.load_persisted_count.lock() += 1;
            Ok(CodeGraph::new())
        }
    }

    // -----------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------

    fn make_manager() -> Arc<WorkspaceManager> {
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
                assert_eq!(original_lifecycle, "evicted");
                let reload = reload_failure.expect("reload failure must be recorded");
                assert!(
                    reload.contains("synthetic reload failure"),
                    "reload diagnostic must carry the builder's failure detail, got: {reload}"
                );
                assert!(
                    reload.contains("evicted"),
                    "reload diagnostic must carry the original eviction context, got: {reload}"
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
}
