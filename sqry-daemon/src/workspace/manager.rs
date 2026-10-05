//! [`WorkspaceManager`] — admission accounting entry points.
//!
//! Covers Task 6 Steps 3 / 4 / 4a / 4b / 4c / 4d of the sqryd plan
//! (Amendment 2 §G.1–§G.7). This file lands the admission-accounting
//! half of the manager — `reserve_rebuild`, `publish_and_retain`,
//! `RollbackGuard`, and the retention reaper. Workspace lifecycle
//! (`get_or_load`, `evict_lru`, `unload`, `status`, Failed-state
//! handling) lands in Phase 6b.
//!
//! ## Lock order (authoritative — referenced by §J.4)
//!
//! All code paths that acquire more than one lock MUST follow this
//! total order; acquiring out of order is a bug enforced by code
//! review.
//!
//! 1. `WorkspaceManager.workspaces: RwLock<HashMap<...>>`
//! 2. `LoadedWorkspace.rebuild_lane: tokio::sync::Mutex<_>` *(Task 7)*
//! 3. `WorkspaceManager.admission: parking_lot::Mutex<AdmissionState>`
//!
//! `WorkspaceManager.hook: RwLock<SharedHook>` is a disjoint
//! sibling — it is NEVER acquired while any of the three locks
//! above are held. In particular, the post-publish hook dispatch
//! (`hook_snapshot` + `SqrydHook::on_publish`) is fired from
//! `get_or_load` AFTER dropping `workspaces_guard` so the hook
//! dispatch, and any re-entrant manager method a hook impl might
//! call, cannot deadlock against the loader that fired it
//! (Codex Task 6 Phase 6c iter-2 MAJOR).
//!
//! Rules:
//! - A holder of `admission` may NOT acquire `rebuild_lane` or
//!   `workspaces` — it is the innermost lock.
//! - A holder of `rebuild_lane` may NOT acquire `workspaces`.
//!   `rebuild_lane` is used only for scheduling/coalescing pending
//!   rebuilds; it is never held across a call that takes `workspaces`
//!   or `admission` nestedly.
//! - A holder of `workspaces` (reader or writer) may NOT acquire
//!   `hook`. Hook dispatch happens only after every outer
//!   workspaces-lock holder has released.
//! - Eviction iterates `workspaces`, sets the per-workspace atomic
//!   `rebuild_cancelled` flag (no lock), then acquires `admission`
//!   alone to update accounting. Eviction never takes `rebuild_lane`.
//! - The retention reaper acquires only `admission`.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use parking_lot::{Mutex, RwLock};
use sqry_core::graph::{CodeGraph, unified::GraphMemorySize};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{config::DaemonConfig, error::DaemonError};

use super::{
    admission::{AdmissionState, RetainedEntry},
    builder::{BuiltGraph, WorkspaceBuilder},
    hook::{NoOpHook, SharedHook, SqrydHook},
    loaded::{LoadedWorkspace, PublishedGraph},
    revision::{
        ResidentQueryGuard, ResidentRevisionHandle, ResidentRevisionLoad, ResidentRevisionRegistry,
        recover_startup,
    },
    roster::{ManifestVerdict, RosterRecord, check_record_against_manifest, shared_load_roster},
    staleness::{StalenessVerdict, classify_staleness},
    state::{OldGraphToken, WorkspaceKey, WorkspaceState},
    status::{DaemonStatus, MemoryStatus, RosterDivergence, RosterStatus, WorkspaceStatus},
};

// ---------------------------------------------------------------------------
// ServeVerdict
// ---------------------------------------------------------------------------

/// Outcome of [`WorkspaceManager::classify_for_serve`].
///
/// Task 7 Phase 7c. Rich-enum return so the IPC router (Task 8) can
/// decide how to shape its response without re-classifying.
#[derive(Debug, Clone)]
pub enum ServeVerdict {
    /// Workspace is healthy; serve from `graph`. Wraps an `Arc` — the
    /// caller holds a strong reference until it is dropped, independent
    /// of any subsequent publish or eviction.
    Fresh {
        graph: Arc<CodeGraph>,
        /// Observed workspace state at classification time — either
        /// [`WorkspaceState::Loaded`] or [`WorkspaceState::Rebuilding`].
        /// Task 7's envelope populates `meta.workspace_state` from this
        /// field so clients can tell which flavour of Fresh they
        /// received (a freshly-loaded snapshot vs. one whose successor
        /// rebuild is already in flight).
        state: WorkspaceState,
        /// The roster `graph` was built with, captured in the same read
        /// critical section as `graph` (surface parity W1).
        roster: Arc<RosterRecord>,
    },
    /// Workspace is in `Failed` state but within the
    /// `stale_serve_max_age_hours` cap. Serve from `graph` with
    /// `meta.stale = true` and `age_hours` in the response envelope.
    Stale {
        graph: Arc<CodeGraph>,
        age_hours: u64,
        /// Timestamp of the last successful build. Task 7 renders this
        /// into the `_stale_warning` string as RFC3339 / UTC-Zulu.
        last_good_at: SystemTime,
        /// Textual diagnostic from the most recent failed build, if any.
        /// `None` when the workspace has been Failed since the last good
        /// build but no error text was captured.
        last_error: Option<String>,
        /// The roster the last-good `graph` was built with.
        roster: Arc<RosterRecord>,
    },
    /// Workspace exists in the manager map but is not yet ready to
    /// serve (`Unloaded` or `Loading`). The IPC router decides what to
    /// do next (retry-after-delay, enqueue, surface a client-appropriate
    /// code); the manager does not prescribe a retry policy.
    NotReady { state: WorkspaceState },
    /// Workspace is `Failed` and holds no generation: its load, or the
    /// reload after an eviction, failed before publishing, so the slot
    /// carries the placeholder and there is nothing to serve, fresh or
    /// stale (DAEMON_FOLLOWUP, round 7). Classified here, in the same read
    /// as the state, rather than as a stale serve with no record or a
    /// build failure flattened to its text, so the caller decides whether
    /// a reload may clear it from these facts.
    FailedWithoutGraph {
        /// The slot published a graph before (`last_good_at` is set; an
        /// eviction does not clear it).
        had_been_loaded: bool,
        /// The failure the slot recorded, typed.
        last_error: Option<Arc<DaemonError>>,
    },
}

// ---------------------------------------------------------------------------
// WorkspaceManager
// ---------------------------------------------------------------------------

/// How long [`WorkspaceManager::reset`] retries for a workspace's rebuild
/// lane before it answers `ResetCancellationDispatched` with nothing
/// dispatched. A lane holder never awaits while it holds the lane, so the
/// lane is free again within microseconds unless its thread is descheduled.
pub const RESET_LANE_WAIT: Duration = Duration::from_secs(1);

/// The reason a load of an evicted workspace is refused while the evicted
/// generation's rebuild runner still holds the runner role
/// ([`WorkspaceManager::honor_preexisting_cancel`]): the same "already in
/// progress" refusal a load of a `Rebuilding` workspace gets, retried by
/// the caller once the runner has stopped.
pub(crate) const EVICTED_REBUILD_STILL_RUNNING: &str =
    "workspace load already in progress (the evicted workspace's rebuild has not stopped)";

/// Owns every loaded workspace plus the admission-accounting state.
///
/// Construction spawns the retention reaper task (§G.3). The handle is
/// stored so `Drop` can abort it cleanly — on daemon shutdown the
/// reaper is aborted, then the admission state drops, dropping every
/// retained `Arc<CodeGraph>` in one pass. No accounting leak, no
/// dangling `Arc`.
#[derive(Debug)]
pub struct WorkspaceManager {
    /// Immutable daemon configuration — used for the memory budget,
    /// the reaper interval, and the drain-timeout warning threshold.
    config: Arc<DaemonConfig>,

    /// Per-workspace state, keyed by [`WorkspaceKey`]. `RwLock` so
    /// the read-only status path contends only with infrequent
    /// insert / remove writers.
    workspaces: RwLock<HashMap<WorkspaceKey, Arc<LoadedWorkspace>>>,

    /// Single-mutex admission accounting — see [`AdmissionState`]
    /// module docs for the §G.5 invariant.
    admission: Mutex<AdmissionState>,

    /// Join handle of the spawned retention reaper. `Option` so
    /// `Drop` can `.take().abort()` without requiring `&mut self`.
    reaper: Mutex<Option<JoinHandle<()>>>,

    /// Instant captured at construction. `daemon/status` reports
    /// `uptime_seconds` = `Instant::now() - started_at`.
    started_at: Instant,

    /// Monotonic peak of `AdmissionState::total_committed_bytes`
    /// observed across the daemon's uptime. Updated via `fetch_max`
    /// on every admission-mutating operation. Amendment 2 §D.
    total_memory_high_water: AtomicU64,

    /// Post-publish persistence hook. Defaults to a no-op; Task 9's
    /// daemon binary installs the production `QueryDbHook` that
    /// wraps `sqry_db::persistence::save_derived`. Swapped via
    /// [`Self::set_hook`] at daemon boot after the `QueryDb` is
    /// constructed.
    ///
    /// `RwLock` rather than `ArcSwap` because `SharedHook = Arc<dyn
    /// Trait + Send + Sync>` is cheap to clone inside the read
    /// critical section, and the hook is only consulted on publish
    /// (not on every query) — the `RwLock` is never a hot path.
    hook: RwLock<SharedHook>,

    /// Resident non-live revision handles. Kept separate from the live
    /// workspace map so immutable revisions never inherit watcher or rebuild
    /// semantics.
    resident_revisions: ResidentRevisionRegistry,

    /// Observation plant for tests (surface parity W1 round 5, design
    /// D24): a closure run at the named [`ObservationPhase`] points where
    /// the manager observes a workspace's state and captures its
    /// generation, so a test can act (publish, attempt an eviction) at the
    /// exact instruction a plant names. [`ObservationPlantSlot`] holds the
    /// closure only under `cfg(any(test, feature = "test-hooks"))`; in a
    /// release build it is a zero-sized type and
    /// [`Self::run_observation_plant`] compiles to nothing.
    observation_plant: ObservationPlantSlot,
}

/// Whether [`WorkspaceManager::load_published`] built the generation it
/// returns or found it already `Loaded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOrigin {
    /// This call built and published the generation.
    Built,
    /// The workspace was `Loaded` at the gate: another caller published the
    /// generation, and this call built nothing.
    Found,
}

/// What the load gate found for a key: the published generation already
/// resident (surface parity W1 round 4, design D20: the pair, so a caller
/// never re-reads the record beside a graph it was handed), or the
/// workspace the caller now owns the load of.
enum LoadGate {
    Loaded(Arc<PublishedGraph>),
    Acquired {
        workspace: Arc<LoadedWorkspace>,
        registered_key: WorkspaceKey,
        entry: GateEntry,
    },
}

/// What the load gate found before it took `Loading`, so a load whose
/// failure is its caller's own can put the slot back as it was
/// ([`WorkspaceManager::abandon_load`]).
#[derive(Debug, Clone, Copy)]
struct GateEntry {
    /// The state the gate's compare-exchange replaced.
    prior_state: WorkspaceState,
    /// Whether this load inserted the entry it gated.
    inserted: bool,
    /// The slot's cancellation flag as the gate found it (an eviction
    /// tombstone carries a set one, which the gate clears).
    prior_cancelled: bool,
}

fn run_revision_startup_recovery(config: &DaemonConfig) {
    match recover_startup(config) {
        Ok(summary) => {
            tracing::debug!(
                partial_artifacts_removed = summary.partial_artifacts_removed.len(),
                worktree_repos_reconciled = summary.worktree_repos_reconciled.len(),
                orphaned_worktree_dirs_removed = summary.orphaned_worktree_dirs_removed.len(),
                "revision workspace startup recovery completed"
            );
        }
        Err(err) => {
            warn!(error = %err, "revision workspace startup recovery failed");
        }
    }
}

impl WorkspaceManager {
    /// Construct a fresh manager and spawn the retention reaper.
    ///
    /// The reaper is spawned on the current Tokio runtime. Callers
    /// must therefore construct the manager from a Tokio context
    /// (`#[tokio::main]`, an `async` block driven by `Runtime::block_on`,
    /// etc.). Tests that don't need the reaper can use
    /// [`Self::new_without_reaper`].
    #[must_use]
    pub fn new(config: &Arc<DaemonConfig>) -> Arc<Self> {
        run_revision_startup_recovery(config);
        let mgr = Arc::new(Self {
            config: Arc::clone(config),
            workspaces: RwLock::new(HashMap::new()),
            admission: Mutex::new(AdmissionState::default()),
            reaper: Mutex::new(None),
            started_at: Instant::now(),
            total_memory_high_water: AtomicU64::new(0),
            hook: RwLock::new(Arc::new(NoOpHook) as SharedHook),
            resident_revisions: ResidentRevisionRegistry::new(),
            observation_plant: ObservationPlantSlot::new(),
        });
        let handle = tokio::spawn(retention_reaper(Arc::downgrade(&mgr)));
        *mgr.reaper.lock() = Some(handle);
        mgr
    }

    /// Like [`Self::new`] but does not spawn the reaper — useful in
    /// unit tests that drive the retention map synchronously via
    /// [`Self::reap_once`].
    #[doc(hidden)]
    #[must_use]
    pub fn new_without_reaper(config: Arc<DaemonConfig>) -> Arc<Self> {
        Arc::new(Self {
            config,
            workspaces: RwLock::new(HashMap::new()),
            admission: Mutex::new(AdmissionState::default()),
            reaper: Mutex::new(None),
            started_at: Instant::now(),
            total_memory_high_water: AtomicU64::new(0),
            hook: RwLock::new(Arc::new(NoOpHook) as SharedHook),
            resident_revisions: ResidentRevisionRegistry::new(),
            observation_plant: ObservationPlantSlot::new(),
        })
    }

    /// Install a post-publish hook. Task 9's daemon binary calls
    /// this once at startup after constructing the shared
    /// `QueryDb`; unit tests call it to install a recording hook.
    /// The old hook is dropped immediately; no retention semantics
    /// apply.
    pub fn set_hook(&self, hook: SharedHook) {
        *self.hook.write() = hook;
    }

    /// Snapshot the currently installed hook. Internal — used by
    /// `get_or_load` (Phase 6c iter-2) after dropping the
    /// `workspaces.read()` guard so the `on_publish` dispatch
    /// never nests under `workspaces`. Taking the hook under its
    /// own short read-lock avoids holding the lock across the
    /// dispatch so a misbehaving hook cannot block a concurrent
    /// `set_hook` swap.
    fn hook_snapshot(&self) -> SharedHook {
        Arc::clone(&*self.hook.read())
    }

    /// Dispatch the post-publish hook for a freshly published graph.
    ///
    /// Both production publish callers funnel through here: the loader in
    /// `get_or_load` and the rebuild runner in
    /// `RebuildDispatcher::execute_one_rebuild`. Routing both through one
    /// method means a published graph always triggers `on_publish` (the
    /// derived-cache save), no matter which path produced it. Before the
    /// rebuild path was wired in, only the load path dispatched, so
    /// `derived.sqry` went stale after every rebuild and was discarded on
    /// the next query (verivus-oss/sqry#358).
    ///
    /// The caller MUST have dropped the `workspaces` guard first: the only
    /// lock taken here is the brief `self.hook.read()` inside
    /// [`Self::hook_snapshot`], so a hook impl is free to call back into
    /// manager methods (e.g. `unload`, needing `workspaces.write()`)
    /// without deadlocking. See the lock-order note in `publish_and_retain`.
    pub(crate) fn dispatch_publish_hook(&self, workspace_root: &Path, graph: Arc<CodeGraph>) {
        let hook = self.hook_snapshot();
        hook.on_publish(workspace_root, graph);
    }

    /// Resident non-live revision registry.
    #[must_use]
    pub fn resident_revisions(&self) -> &ResidentRevisionRegistry {
        &self.resident_revisions
    }

    /// Load or reuse a resident revision graph.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if graph hydration/building fails or a
    /// coalesced load failed in another caller.
    pub fn load_resident_revision<F>(
        &self,
        load: &ResidentRevisionLoad,
        build_graph: F,
    ) -> Result<Arc<ResidentRevisionHandle>, DaemonError>
    where
        F: FnOnce() -> Result<CodeGraph, DaemonError>,
    {
        self.resident_revisions.load_or_coalesce(load, build_graph)
    }

    /// Acquire a query guard for a resident revision.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] if the handle is
    /// absent or not queryable.
    pub fn acquire_resident_query(
        &self,
        revision_id: &sqry_daemon_protocol::RevisionId,
    ) -> Result<ResidentQueryGuard, DaemonError> {
        self.resident_revisions.acquire_query(revision_id)
    }

    /// Unload a resident revision handle.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] when the handle is
    /// pinned or has active queries and `force` is false.
    pub fn unload_resident_revision(
        &self,
        revision_id: &sqry_daemon_protocol::RevisionId,
        force: bool,
    ) -> Result<bool, DaemonError> {
        self.resident_revisions.unload(revision_id, force)
    }

    /// Status rows for resident revisions.
    #[must_use]
    pub fn resident_revision_statuses(
        &self,
        root: Option<&Path>,
        include_unloaded: bool,
    ) -> Vec<sqry_daemon_protocol::RevisionStatus> {
        self.resident_revisions.statuses(root, include_unloaded)
    }

    /// Artifact ids protected from artifact pruning by active or pinned handles.
    #[must_use]
    pub fn pinned_revision_artifact_ids(&self) -> Vec<sqry_daemon_protocol::ArtifactId> {
        self.resident_revisions.pinned_artifact_ids()
    }

    /// Evict the least-recently-used inactive resident revision handle.
    #[must_use]
    pub fn evict_inactive_resident_revision_lru(&self) -> Option<sqry_daemon_protocol::RevisionId> {
        self.resident_revisions.evict_inactive_lru()
    }

    /// Memory budget in bytes (derived from `config.memory_limit_mb`).
    #[must_use]
    pub fn memory_limit_bytes(&self) -> u64 {
        self.config.memory_limit_bytes()
    }

    /// Access to the workspace registry (read-only view).
    ///
    /// Intentionally `pub(crate)` and `#[allow(dead_code)]` in Phase 6a:
    /// Phase 6b consumers (`get_or_load`, `evict_lru`, `status`) are the
    /// first real callers. Keeping the accessor here documents the
    /// intended visibility boundary rather than forcing later code to
    /// reach into the field directly.
    #[allow(dead_code)]
    pub(crate) fn workspaces(&self) -> &RwLock<HashMap<WorkspaceKey, Arc<LoadedWorkspace>>> {
        &self.workspaces
    }

    /// Access to the admission mutex (internal). See
    /// [`Self::workspaces`] for the `#[allow(dead_code)]` rationale.
    #[allow(dead_code)]
    pub(crate) fn admission(&self) -> &Mutex<AdmissionState> {
        &self.admission
    }

    /// Look up a loaded workspace by key without acquiring `rebuild_lane`
    /// or `admission`.
    ///
    /// Returns `Some(Arc<LoadedWorkspace>)` if a workspace is currently
    /// registered under `key`, or `None` otherwise. The `workspaces`
    /// read guard is acquired and released inside the call — callers
    /// never observe it nested with any other lock.
    ///
    /// Added for the Task 7 [`crate::rebuild::RebuildDispatcher`] which
    /// needs a cheap handle on `Arc<LoadedWorkspace>` as a precondition
    /// before entering the canonical §J.4 ordered sequence
    /// (`rebuild_lane` → `admission`). This is *not* part of the
    /// ordered sequence itself — the §J.4 contract only constrains
    /// paths that hold more than one lock simultaneously. Here, the
    /// `workspaces` guard is dropped before the caller takes
    /// `rebuild_lane`, so there is no nesting.
    #[allow(dead_code)] // Consumed by rebuild.rs once Task 7 `rebuild` module lands.
    /// Shared lookup: returns the `Arc<LoadedWorkspace>` keyed by
    /// `key` if present. Used by `RebuildDispatcher::handle_changes`
    /// (inside the crate) and by external test harnesses (Task 7
    /// Phase 7b1 `rebuild_runner_gate.rs`) that need to inspect
    /// workspace-level atomics (`rebuild_in_flight`, `rebuild_cancelled`)
    /// or the `rebuild_lane` mutex directly.
    ///
    /// This is NOT a JSON-RPC surface — the IPC layer should use
    /// `status()` for point-in-time workspace state. Direct `lookup`
    /// access bypasses the LRU touch that `status()` performs.
    pub fn lookup(&self, key: &WorkspaceKey) -> Option<Arc<LoadedWorkspace>> {
        let guard = self.workspaces.read();
        Self::resolve_locked(&guard, key).map(Arc::clone)
    }

    /// The map entry `key` names, under the caller's guard: the one copy
    /// of the resolution [`Self::lookup`], [`Self::resident_snapshot`] and
    /// [`Self::loaded_published_for_key`] share (surface parity W1 round
    /// 5, design D25).
    ///
    /// #393: anonymous workspaces are coalesced by source_root even when
    /// historical duplicate keys remain in the map. Use the deterministic
    /// source-root winner rather than HashMap iteration order so divergent
    /// callers all observe the same workspace.
    fn resolve_locked<'a>(
        workspaces: &'a HashMap<WorkspaceKey, Arc<LoadedWorkspace>>,
        key: &WorkspaceKey,
    ) -> Option<&'a Arc<LoadedWorkspace>> {
        if key.workspace_id.is_none()
            && let Some((_, ws)) =
                Self::anonymous_workspace_by_source_root(workspaces, &key.source_root)
        {
            return Some(ws);
        }
        workspaces.get(key)
    }

    /// Every registered workspace, tombstones included, as a snapshot taken
    /// under one `workspaces.read()`. The caller acts on each without the
    /// guard (shutdown cancels their rebuilds under each rebuild lane).
    pub(crate) fn workspaces_snapshot(&self) -> Vec<Arc<LoadedWorkspace>> {
        self.workspaces.read().values().map(Arc::clone).collect()
    }

    /// Whether `ws` is the workspace registered under its own key in
    /// `workspaces`: the entry keyed by [`LoadedWorkspace::key`] is this
    /// very workspace, not merely one with the same key.
    ///
    /// The publish rechecks (a load, a reload, a rebuild iteration) ask this
    /// under the `workspaces.read()` guard they publish under. Asking by the
    /// caller's key would miss a workspace registered under another
    /// anonymous key for the same root (a pinned workspace is registered
    /// under `ProjectRootMode::WorkspaceFolder`), and asking whether the key
    /// is present would accept a workspace an unload removed and a later
    /// load replaced.
    pub(crate) fn registers(
        workspaces: &HashMap<WorkspaceKey, Arc<LoadedWorkspace>>,
        ws: &LoadedWorkspace,
    ) -> bool {
        workspaces
            .get(&ws.key)
            .is_some_and(|registered| std::ptr::eq(registered.as_ref(), ws))
    }

    /// One observation of the workspace `key` names: its lifecycle state
    /// and the generation it published, read under one `workspaces.read()`
    /// (surface parity W1 round 5, design D25).
    ///
    /// The guard serialises the two reads against the three tombstone
    /// writers (`execute_eviction`, `unload`, `reset`, each holding
    /// `workspaces.write()` across the placeholder swap and the `Evicted`
    /// store), so the generation returned is the one the returned state
    /// describes. `None` when no entry resolves. Unlike
    /// [`Self::loaded_published_for_key`] this neither touches the LRU
    /// clock ([`Self::lookup`] does not either) nor refuses a generation
    /// without a record: a reader that answers `(None, None)` for a
    /// tombstone (the daemon-hosted `rebuild_index` cache-hit leg, Q8)
    /// reads the placeholder here and decides for itself.
    pub(crate) fn resident_snapshot(
        &self,
        key: &WorkspaceKey,
    ) -> Option<(WorkspaceState, Arc<PublishedGraph>)> {
        let workspaces = self.workspaces.read();
        let ws = Self::resolve_locked(&workspaces, key)?;
        let state = ws.load_state();
        self.run_observation_plant(ObservationPhase::SnapshotObserved);
        let published = ws.published();
        Some((state, published))
    }

    /// Retention reaper: a single pass over `retained_old`.
    ///
    /// Removes entries whose `Arc::strong_count` has dropped to 1 —
    /// meaning the admission map is the last holder. Emits a
    /// one-shot WARN log line when an entry exceeds
    /// `rebuild_drain_timeout_ms` without dropping.
    ///
    /// **This is the only code path that removes tokens from
    /// `retained_old`.** Any other code that mutates the retention
    /// map is a violation of §G.3.
    pub fn reap_once(&self) {
        let timeout = Duration::from_millis(self.config.rebuild_drain_timeout_ms);
        let now = Instant::now();
        let mut to_log: Vec<OldGraphToken> = Vec::new();
        {
            let mut state = self.admission.lock();
            state.retained_old.retain(|token, entry| {
                if Arc::strong_count(&entry.graph) == 1 {
                    false // Last holder: drop entry + Arc together.
                } else {
                    if !entry.warned_past_timeout
                        && now.saturating_duration_since(entry.published_at) > timeout
                    {
                        entry.warned_past_timeout = true;
                        to_log.push(*token);
                    }
                    true
                }
            });
        }
        for token in to_log {
            warn!(
                token = %token,
                drain_timeout_ms = self.config.rebuild_drain_timeout_ms,
                "sqryd retention reaper: retained old graph still held past drain timeout \
                 (not an accounting deadline — bytes stay accounted until strong_count == 1)",
            );
        }
    }

    /// Amendment 2 §G.1 two-phase reservation protocol.
    ///
    /// ```text
    /// Phase 1 (workspaces read + admission read):
    ///     project_total + estimate ≤ limit?  → commit
    ///     otherwise                          → pick LRU non-pinned
    ///                                          victims (`for_key` is
    ///                                          exempt — a workspace
    ///                                          cannot evict itself)
    /// Phase 2 (no locks held):
    ///     for each victim: execute_eviction()
    /// Phase 3 (admission alone):
    ///     re-check projected vs limit     → authoritative commit
    ///     reserved_bytes += estimate     → return RebuildReservation
    /// ```
    ///
    /// Lock order is `workspaces → admission` in Phase 1, nothing in
    /// Phase 2, `admission` alone in Phase 3. No nesting of
    /// `rebuild_lane` — Task 7 adds that layer outside this function.
    ///
    /// Returns a [`RebuildReservation`] RAII guard on success. On
    /// `Err`, the admission state is exactly pre-call — either no
    /// eviction happened (headroom already available) or the
    /// eviction cleared retained entries but could not fit.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WorkspaceEvicted`] if the requesting
    /// workspace was removed before the reservation could be made, or
    /// [`DaemonError::MemoryBudgetExceeded`] if the configured daemon
    /// memory limit cannot admit the estimated rebuild working set
    /// after eligible retained graphs are evicted.
    pub fn reserve_rebuild(
        self: &Arc<Self>,
        for_key: &WorkspaceKey,
        working_set_estimate: u64,
    ) -> Result<RebuildReservation, DaemonError> {
        let limit = self.memory_limit_bytes();

        // --- Phase 1: peek + plan (holds workspaces → admission) ---
        //
        // Task 7 Phase 7b1 tightening: reject if the requester has been
        // evicted or removed between dispatch and reservation. Both the
        // membership check and the `rebuild_cancelled` read happen under
        // the Phase-1 `workspaces.read()` so they serialise against
        // `execute_eviction`'s `workspaces.write()` (which holds across
        // both `rebuild_cancelled.store(true)` and `workspaces.remove`).
        //
        // Post-serialisation snapshot: the reader sees EITHER pre-eviction
        // state (`Some(ws)` with `cancelled == false`) OR post-eviction
        // state (`None` OR `cancelled == true`). Keeping both checks is
        // belt-and-suspenders against any future eviction-protocol change
        // that could reorder the two mutations.
        let victims = {
            let workspaces = self.workspaces.read();

            // #393 reload regression fix: for anonymous keys, the caller's
            // WorkspaceKey may differ in (root_mode, fingerprint) from the
            // first-inserted canonical key under which the entry is stored
            // (see coalesce in get_or_insert_workspace_tracked). Exact get(for_key)
            // would spuriously return WorkspaceEvicted for a still-registered
            // same-source_root anon workspace (e.g. post-reset get_or_load
            // under divergent anon key). Fallback to source_root among anon
            // entries so reserve (and thus get_or_load) succeeds.
            let requester_ws = if for_key.workspace_id.is_none() {
                match Self::anonymous_workspace_by_source_root(&workspaces, &for_key.source_root) {
                    Some((_, ws)) => ws,
                    None => {
                        return Err(DaemonError::WorkspaceEvicted {
                            root: for_key.source_root.clone(),
                        });
                    }
                }
            } else if let Some(ws) = workspaces.get(for_key) {
                ws
            } else {
                return Err(DaemonError::WorkspaceEvicted {
                    root: for_key.source_root.clone(),
                });
            };
            if requester_ws.rebuild_cancelled.load(Ordering::Acquire) {
                return Err(DaemonError::WorkspaceEvicted {
                    root: for_key.source_root.clone(),
                });
            }

            let state = self.admission.lock();
            let projected = state
                .total_committed_bytes()
                .saturating_add(working_set_estimate);
            if projected <= limit {
                Vec::new() // no victim selection needed
            } else {
                let need = projected - limit;
                Self::plan_eviction(&workspaces, &state, need, for_key)
            }
            // Both guards drop here — Phase 2 runs with no locks.
        };

        // --- Phase 2: execute each eviction with no locks held ---
        for key in &victims {
            self.execute_eviction(key);
        }

        // --- Phase 2.5: opportunistic reap ----------------------
        //
        // `execute_eviction` moves the evicted workspace's bytes
        // from `loaded_bytes` into `retained_old`. If no slow query
        // still holds the evicted `Arc<CodeGraph>`, the retention
        // reaper's next tick (25 ms) would free those bytes — but
        // Phase 3's authoritative re-check runs *now*, before the
        // reaper gets the chance. Run a synchronous reap pass so
        // admission sees the free bytes immediately on the common
        // case of "no outstanding slow queries". Slow-query-held
        // entries stay retained and still count against the budget,
        // which is correct per §G.5.
        if !victims.is_empty() {
            self.reap_once();
        }

        // --- Phase 3: authoritative commit (admission alone) ------
        let mut state = self.admission.lock();
        let projected = state
            .total_committed_bytes()
            .saturating_add(working_set_estimate);
        if projected > limit {
            return Err(DaemonError::MemoryBudgetExceeded {
                limit_bytes: limit,
                current_bytes: state.loaded_bytes,
                reserved_bytes: state.reserved_bytes,
                retained_bytes: state.retained_total_bytes(),
                requested_bytes: working_set_estimate,
            });
        }
        state.reserved_bytes = state.reserved_bytes.saturating_add(working_set_estimate);
        self.bump_high_water(&state);
        drop(state);

        Ok(RebuildReservation {
            manager: Arc::downgrade(self),
            bytes: working_set_estimate,
            released: false,
        })
    }

    /// Phase-1 helper: pick the LRU-ordered set of non-pinned
    /// workspace keys (excluding `for_key`) whose cumulative
    /// `memory_bytes` meets or exceeds `need`.
    ///
    /// Returns keys in eviction order (oldest-first). Callers execute
    /// evictions in Phase 2 without holding any lock.
    fn plan_eviction(
        workspaces: &HashMap<WorkspaceKey, Arc<LoadedWorkspace>>,
        _state: &AdmissionState,
        need: u64,
        for_key: &WorkspaceKey,
    ) -> Vec<WorkspaceKey> {
        let mut candidates: Vec<(Instant, u64, WorkspaceKey)> = workspaces
            .iter()
            .filter(|(k, ws)| {
                // Skip the requester (§G.7: a pinned workspace that
                // exceeds the budget must fail, not evict itself) and
                // every pinned workspace. Also skip a workspace with no
                // graph to reclaim (`Self::holds_evictable_graph`).
                //
                // #393: when reserve_rebuild is called with a divergent
                // anonymous key (different secondary fields) for the same
                // source_root, **k != *for_key would fail to exempt the
                // actual registered entry (stored under the first-inserted
                // key). Treat same anon source_root as "self" for exemption.
                let is_requester = **k == *for_key
                    || (for_key.workspace_id.is_none()
                        && k.workspace_id.is_none()
                        && k.source_root == for_key.source_root);
                !is_requester && !ws.pinned && Self::holds_evictable_graph(ws)
            })
            .map(|(k, ws)| {
                let last = *ws.last_accessed.read();
                let bytes = ws.memory_bytes.load(Ordering::Acquire) as u64;
                (last, bytes, k.clone())
            })
            .collect();
        // Oldest last_accessed first.
        candidates.sort_by_key(|(ts, _, _)| *ts);

        let mut plan = Vec::new();
        let mut reclaimed: u64 = 0;
        for (_, bytes, key) in candidates {
            if reclaimed >= need {
                break;
            }
            plan.push(key);
            reclaimed = reclaimed.saturating_add(bytes);
        }
        plan
    }

    /// Execute Phase-2 of an eviction.
    ///
    /// Steps, in order:
    ///
    /// 1. Swap the workspace's `ArcSwap<CodeGraph>` to an empty
    ///    placeholder. This releases the old `Arc` from the
    ///    `ArcSwap` itself — any outstanding slow-query `Arc`s
    ///    still exist at the same strong count.
    /// 2. Move those bytes from `loaded_bytes` into `retained_old`
    ///    (under the admission mutex) — keying on a fresh
    ///    [`OldGraphToken`]. This preserves the §G.5 invariant:
    ///    bytes shift from the loaded tier to the retained tier
    ///    rather than disappearing. The retention reaper frees the
    ///    entry (and therefore the bytes) when `strong_count` drops
    ///    to 1, i.e. when every slow query has released its `Arc`.
    /// 3. Set `rebuild_cancelled = true` so any concurrent
    ///    `get_or_load` / rebuild running against this workspace
    ///    observes the signal at its next pass boundary and aborts
    ///    without publishing.
    /// 4. Mark the state `Evicted` — and **leave the entry in the
    ///    manager map** as a tombstone. `STEP_6` (workspace-aware-
    ///    cross-repo, 2026-04-26): keeping the tombstone is what
    ///    makes per-source-root partial eviction observable through
    ///    `daemon/workspaceStatus`. The aggregate must report
    ///    `state == Evicted` for individually-evicted source roots
    ///    while siblings remain `Loaded`. Removing the entry would
    ///    silently hide the eviction from the aggregate — exactly
    ///    the codex iter-1 BLOCK item.
    ///
    /// The order is load-bearing: the cancellation flag is set
    /// *before* the state transition so a concurrent loader that
    /// re-checks `rebuild_cancelled` after its build (per
    /// [`Self::get_or_load`]) sees the cancel.
    ///
    /// To **fully unload** a workspace (drop the tombstone too),
    /// callers route through [`Self::unload`] / `daemon/unload`,
    /// which calls this function and then explicitly removes the
    /// map entry. LRU eviction (`evict_lru`, `reserve_rebuild`'s
    /// Phase 2) keeps the tombstone; only an explicit user-driven
    /// unload removes it.
    ///
    /// Codex Task 6 Phase 6b iter-1 MAJOR: the pre-fix version
    /// dropped the evicted `Arc` at function end and subtracted
    /// bytes from `loaded_bytes` without inserting a retained
    /// entry — leaking accounting for any graph still held by a
    /// slow query.
    ///
    /// Codex `STEP_6` iter-1 BLOCK: the pre-fix version unconditionally
    /// removed the entry from `self.workspaces` after marking it
    /// `Evicted`, defeating partial-eviction reporting. The
    /// remove-entry step now lives in [`Self::unload`] alone.
    fn execute_eviction(&self, key: &WorkspaceKey) {
        // Hold `workspaces.write()` across the ENTIRE eviction —
        // from the initial lookup through the final state store —
        // so no concurrent `get_or_load` post-build re-check can
        // interleave with us. Loaders serialize against eviction
        // by holding `workspaces.read()` across their own publish
        // critical section (see `get_or_load` step 7+).
        //
        // Lock order is `workspaces → admission` per plan §J.4.
        // We take `admission` INSIDE this write-lock in Step 2,
        // which is the outermost-first order the contract
        // requires.
        //
        // Codex Task 6 Phase 6b iter-2 MAJOR: the iter-1 version
        // took `workspaces.read()` only briefly for the initial
        // lookup, then dropped it — leaving a window where a
        // concurrent load's post-build re-check could observe
        // workspace-still-in-map / cancelled-still-false and then
        // publish into an already-evicted workspace. Holding
        // `workspaces.write()` across the full eviction closes
        // that window.
        let mut workspaces = self.workspaces.write();
        // Steps 1–3 (ArcSwap, admission tier transfer, cancellation
        // + state store) are factored into the shared helper so
        // [`Self::unload`] can reuse them under a single
        // workspaces.write() guard.
        //
        // Step 4 (DO NOT remove from `self.workspaces`) is implicit
        // here — the entry stays in the map as a tombstone. The
        // tombstone is what STEP_6 partial-eviction reporting
        // depends on. `unload` (the explicit user-driven path)
        // removes the entry separately after this function returns.
        self.evict_to_tombstone_locked(&mut workspaces, key);
        drop(workspaces);
    }

    /// The load gate: the generation already resident for `key`, or the
    /// workspace the caller now owns the load of.
    ///
    /// Both `Loaded` returns are one observation (surface parity W1 round
    /// 5, design D24, codex R5-1): the state is read and the generation
    /// captured by [`Self::resident_generation`] under one
    /// `workspaces.read()`, at the first lookup and again after the
    /// compare-exchange into `Loading` loses to a concurrent loader. The
    /// pre-round-5 second return read `load_state()` and then
    /// `published()` with no guard held, so an eviction completing between
    /// the two (the tombstone writers need only `workspaces.write()`,
    /// which nothing here contested) handed the caller the placeholder
    /// (an empty graph, no record) under the label `Loaded`. The three
    /// [`ObservationPhase`] calls are the test seam that reproduces that
    /// window deterministically; in a release build each compiles to
    /// nothing.
    ///
    /// When the CAS loses and the observation finds the slot not `Loaded`
    /// (a loader or rebuild in flight, or a state that moved to `Evicted`,
    /// `Failed` or `Unloaded` between the CAS and the observation) the
    /// answer is the `already in progress` refusal, with the state read
    /// again for the message only, as before round 5.
    fn prepare_load_gate(self: &Arc<Self>, key: &WorkspaceKey) -> Result<LoadGate, DaemonError> {
        if let Some(published) = self.loaded_published_for_key(key)? {
            return Ok(LoadGate::Loaded(published));
        }
        self.run_observation_plant(ObservationPhase::GateFirstLookupMissed);

        let (workspace, inserted) = self.get_or_insert_workspace_tracked(key);
        let registered_key = Self::registered_key_for_load(key, &workspace);
        let Some(prior_state) = Self::enter_loading_state(&workspace) else {
            self.run_observation_plant(ObservationPhase::GateCasLost);
            let workspaces = self.workspaces.read();
            if let Some(published) = self.resident_generation(
                &workspace,
                &workspaces,
                Some(ObservationPhase::GateLoadedObserved),
            )? {
                drop(workspaces);
                return Ok(LoadGate::Loaded(published));
            }
            drop(workspaces);
            let current = workspace.load_state();
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: format!("workspace load already in progress ({current})"),
            });
        };
        let prior_cancelled = workspace.rebuild_cancelled.load(Ordering::Acquire);
        Self::honor_preexisting_cancel(&workspace, key, prior_state)?;
        self.run_observation_plant(ObservationPhase::GateAcquired);
        Ok(LoadGate::Acquired {
            workspace,
            registered_key,
            entry: GateEntry {
                prior_state,
                inserted,
                prior_cancelled,
            },
        })
    }

    /// The published generation of a `Loaded` workspace at `key`, one
    /// observation of the slot under `workspaces.read()` (design D20, D24),
    /// or `None` when the key is absent or not `Loaded`.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Internal`] when the slot is `Loaded` but its
    /// generation carries no roster record (see
    /// [`Self::resident_generation`]).
    fn loaded_published_for_key(
        &self,
        key: &WorkspaceKey,
    ) -> Result<Option<Arc<PublishedGraph>>, DaemonError> {
        let workspaces = self.workspaces.read();
        let Some(ws) = Self::resolve_locked(&workspaces, key) else {
            return Ok(None);
        };
        self.resident_generation(ws, &workspaces, None)
    }

    /// Observe `ws`'s state and capture its generation under the caller's
    /// `workspaces.read()` guard (surface parity W1 round 5, design D24).
    ///
    /// The guard is a parameter so the observation is bound to a held
    /// guard by the signature: no caller can observe without one. Under
    /// it, a slot that is not `Loaded` answers `Ok(None)`; a `Loaded` slot
    /// has its generation loaded once (`ws.published()`), the LRU clock
    /// touched, and the generation returned. The guard closes the window
    /// by the argument [`Self::classify_for_serve`] records: the three
    /// tombstone writers hold `workspaces.write()` across the placeholder
    /// swap and the `Evicted` store, so under the read guard the state
    /// and the generation are one observation.
    ///
    /// `phase`, when given, names the [`ObservationPhase`] to run between
    /// the state read and the generation load: the second `Loaded` return
    /// of [`Self::prepare_load_gate`] passes `GateLoadedObserved`, the
    /// point at which an eviction would have to complete to split the
    /// two reads.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Internal`], with the sentence `classify_for_serve`
    /// uses for the same condition, when the `Loaded` slot's generation
    /// carries no roster record. The placeholder is the only generation
    /// without a record and a `Loaded` slot holding it is a publish-path
    /// bug, never something to hand back under the label `Loaded`.
    fn resident_generation(
        &self,
        ws: &LoadedWorkspace,
        _guard: &parking_lot::RwLockReadGuard<'_, HashMap<WorkspaceKey, Arc<LoadedWorkspace>>>,
        phase: Option<ObservationPhase>,
    ) -> Result<Option<Arc<PublishedGraph>>, DaemonError> {
        if ws.load_state() != WorkspaceState::Loaded {
            return Ok(None);
        }
        if let Some(phase) = phase {
            self.run_observation_plant(phase);
        }
        let published = ws.published();
        if published.roster.is_none() {
            return Err(DaemonError::Internal(anyhow::anyhow!(
                "workspace {} is Loaded but carries no roster record; publish paths must \
                 publish the record with the graph",
                ws.key.source_root.display()
            )));
        }
        ws.touch();
        Ok(Some(published))
    }

    fn registered_key_for_load(key: &WorkspaceKey, workspace: &LoadedWorkspace) -> WorkspaceKey {
        if key.workspace_id.is_none()
            && workspace.key.workspace_id.is_none()
            && workspace.key.source_root == key.source_root
        {
            workspace.key.clone()
        } else {
            key.clone()
        }
    }

    fn enter_loading_state(workspace: &LoadedWorkspace) -> Option<WorkspaceState> {
        [
            WorkspaceState::Unloaded,
            WorkspaceState::Failed,
            WorkspaceState::Evicted,
        ]
        .into_iter()
        .find(|prior| {
            workspace
                .state
                .compare_exchange(
                    prior.as_u8(),
                    WorkspaceState::Loading.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        })
    }

    /// A load-gate failure arm whose compare-exchange lost (design
    /// D37). The state belongs to the writer that installed it, a
    /// completed eviction or a reset, so nothing is written and the arm
    /// returns the typed error it returns today; this names the state
    /// it observed so an operator can see the transition that was not
    /// taken.
    fn log_lost_load_transition(root: &Path, observed: WorkspaceState) {
        tracing::debug!(
            workspace = %root.display(),
            observed = %observed,
            "a load-gate failure arm left the state to its writer"
        );
    }

    /// A load refused by its builder's preparation, before the
    /// reservation: the same outcome as a build that fails (the refusal is
    /// recorded as `last_error` and the `Loading` gate is left for `Failed`
    /// when no writer that owns the state has replaced it, design D37), but
    /// reached before any memory was reserved or any sibling evicted.
    fn fail_load_before_reservation(
        ws: &LoadedWorkspace,
        loading: &mut LoadingGuard<'_>,
        root: &Path,
        refusal: &DaemonError,
    ) {
        ws.record_failure(clone_err(refusal));
        loading.armed = false;
        if let Err(observed) = ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
        {
            Self::log_lost_load_transition(root, observed);
        }
    }

    /// Put a gated slot back as the gate found it (B2 and S8, round 7
    /// audit): a load whose failure is its caller's own (a refusal of the
    /// request, a budget that cannot admit it, or any failure of a builder
    /// whose failed load leaves the index as it found it) records nothing,
    /// counts no retry, and leaves no `Failed` slot that a later query of
    /// a healthy index would read. The `Loading` gate goes back to the
    /// state it replaced (with an eviction tombstone's cancellation flag),
    /// and an entry this load inserted is removed, under one
    /// `workspaces.write()`. A writer that replaced `Loading` meanwhile (a
    /// completed eviction, a reset) owns the state and the entry.
    fn abandon_load(
        &self,
        ws: &Arc<LoadedWorkspace>,
        loading: &mut LoadingGuard<'_>,
        entry: GateEntry,
    ) {
        loading.armed = false;
        let mut workspaces = self.workspaces.write();
        if let Err(observed) = ws.transition_state(WorkspaceState::Loading, entry.prior_state) {
            Self::log_lost_load_transition(&ws.key.source_root, observed);
            return;
        }
        if entry.prior_state == WorkspaceState::Evicted && entry.prior_cancelled {
            ws.rebuild_cancelled.store(true, Ordering::Release);
        }
        if entry.inserted
            && entry.prior_state == WorkspaceState::Unloaded
            && workspaces
                .get(&ws.key)
                .is_some_and(|registered| Arc::ptr_eq(registered, ws))
        {
            workspaces.remove(&ws.key);
        }
    }

    /// The load gate's answer to a cancellation set before this load
    /// entered `Loading`.
    ///
    /// From `Evicted` the flag is the completed eviction's, and is consumed
    /// here so it cannot fail this load; except when the eviction landed
    /// mid-iteration (`evicted_mid_iteration`: it found the slot
    /// `Rebuilding`) and that runner still holds the runner role
    /// (`rebuild_in_flight`). Such a runner has not published, reads the
    /// flag at its pass boundaries, at its publish recheck and at its gate,
    /// and has not necessarily read it yet: consuming it here let the
    /// runner publish its graph into the slot this load then owned (round
    /// 8 review, note (a)). So the flag is left for the runner, the
    /// tombstone is put back, and the load is refused as already in
    /// progress, for the caller to retry once the runner has stopped. The
    /// flag is read, not swapped, on that arm, so the runner never sees it
    /// cleared. An eviction between iterations (the slot `Loaded` or
    /// `Failed`) is consumed as before: the runner's next iteration cannot
    /// begin on `Evicted` or `Loading`. From any other state a set flag
    /// fails the load (`workspace evicted mid-load`).
    fn honor_preexisting_cancel(
        workspace: &LoadedWorkspace,
        key: &WorkspaceKey,
        prior_state: WorkspaceState,
    ) -> Result<(), DaemonError> {
        if prior_state == WorkspaceState::Evicted
            && workspace.rebuild_in_flight.load(Ordering::Acquire)
            && workspace.evicted_mid_iteration.load(Ordering::Acquire)
            && workspace.rebuild_cancelled.load(Ordering::Acquire)
        {
            if let Err(observed) =
                workspace.transition_state(WorkspaceState::Loading, WorkspaceState::Evicted)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: EVICTED_REBUILD_STILL_RUNNING.to_string(),
            });
        }
        let pre_cancelled = workspace.rebuild_cancelled.swap(false, Ordering::AcqRel);
        if !pre_cancelled || prior_state == WorkspaceState::Evicted {
            return Ok(());
        }
        workspace.rebuild_cancelled.store(true, Ordering::Release);
        if let Err(observed) =
            workspace.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
        {
            Self::log_lost_load_transition(&key.source_root, observed);
        }
        Err(DaemonError::WorkspaceBuildFailed {
            root: key.source_root.clone(),
            reason: "workspace evicted mid-load".to_string(),
        })
    }

    /// Load the workspace's graph, building it via `builder` if not
    /// already present.
    ///
    /// Lifecycle gate:
    ///
    /// 1. Cache-hit fast path — if the workspace is present AND in
    ///    [`WorkspaceState::Loaded`], touch + return.
    /// 2. CAS `Unloaded`/`Evicted`/`Failed` → `Loading`. Exactly one
    ///    caller wins. If another caller already holds the gate
    ///    (`Loading`/`Rebuilding`), return an error — Phase 6c /
    ///    Task 7 will introduce a wait-for-done notify channel.
    /// 3. The winner arms a [`LoadingGuard`] RAII wrapper that
    ///    transitions the workspace into [`WorkspaceState::Failed`]
    ///    on *any* non-success exit (`Err`, early `return`, or
    ///    panic). This covers the Codex iter-1 MAJOR that a panic
    ///    from `builder.build()` would leave the workspace stuck
    ///    in Loading.
    ///
    ///    Step 3b then prepares the build
    ///    ([`WorkspaceBuilder::prepare_build`]): every input the build would
    ///    refuse is resolved here, so a load refused for its own input fails
    ///    before the reservation and evicts no sibling workspace.
    /// 4. Reserve admission headroom (§G.1 three-phase).
    /// 5. Run the prepared build.
    /// 6. Re-check `rebuild_cancelled` + workspace map membership
    ///    before publishing. If eviction ran during the build, the
    ///    reservation refunds via RAII and no graph is published.
    /// 7. Publish via `publish_and_retain`. Disarm the `LoadingGuard`
    ///    + record success + touch.
    /// 8. Release `workspaces_guard`, THEN dispatch the
    ///    post-publish `SqrydHook`. The hook fires outside every
    ///    outer manager lock so a hook impl is free to call back
    ///    into `unload` / `get_or_load` / `set_hook` / `status`
    ///    without deadlocking against the loader that fired it.
    ///
    /// Codex Task 6 Phase 6b iter-1 MAJOR (×2): the pre-fix version
    /// clobbered a concurrent eviction's `rebuild_cancelled` signal
    /// and could publish into a workspace already removed from the
    /// map. The CAS + post-build re-check + `LoadingGuard` together
    /// close both holes.
    ///
    /// Codex Task 6 Phase 6c iter-2 MAJOR: the pre-fix version
    /// dispatched the hook from inside `publish_and_retain` while
    /// the caller still held `workspaces.read()`, giving a hook
    /// impl that needed `workspaces.write()` (e.g. via `unload`)
    /// a guaranteed re-entrancy deadlock. Splitting publish and
    /// hook dispatch into Steps 7 and 8 closes that hole.
    ///
    /// # Errors
    ///
    /// - The builder's refusals, raised by its preparation before the
    ///   reservation: with [`RealWorkspaceBuilder`](super::RealWorkspaceBuilder),
    ///   [`DaemonError::WorkspaceManifestUnreadable`],
    ///   [`DaemonError::WorkspaceIncompatibleGraph`] and, for a macro
    ///   options request or record it cannot honour,
    ///   [`DaemonError::RebuildMacroOptionsUnavailable`] or
    ///   [`DaemonError::InvalidArgument`]. A refusal of the on-disk index
    ///   (the first two) leaves the workspace `Failed` with the refusal as
    ///   `last_error`; a refusal of the request (`is_request_refusal`)
    ///   leaves the slot as the gate found it (B2, round 7 audit), as does
    ///   any failure of a builder whose
    ///   [`WorkspaceBuilder::failed_load_leaves_no_slot`] holds.
    /// - [`DaemonError::MemoryBudgetExceeded`] if Phase 3 cannot
    ///   admit the reservation even after LRU eviction; the slot is left
    ///   as the gate found it (S8, round 7 audit).
    /// - [`DaemonError::WorkspaceBuildFailed`] surfaced from the
    ///   builder OR synthesised when a concurrent eviction races
    ///   the load (`reason = "workspace evicted mid-load"`).
    pub fn get_or_load(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        builder: &dyn WorkspaceBuilder,
        working_set_estimate: u64,
    ) -> Result<Arc<CodeGraph>, DaemonError> {
        self.get_or_load_published(key, builder, working_set_estimate)
            .map(|published| Arc::clone(&published.graph))
    }

    /// [`Self::get_or_load`] returning the published generation it found or
    /// published: the graph and the roster record as one value (surface
    /// parity W1 round 4, design D20). A caller that answers with the
    /// record (the daemon-hosted `rebuild_index` envelope) takes both halves
    /// from this value and never re-reads the slot, which a second publisher
    /// could have advanced in the meantime. `get_or_load` is the graph-half
    /// accessor over this method, the same shape as
    /// [`LoadedWorkspace::graph`] over [`LoadedWorkspace::published`].
    ///
    /// # Errors
    ///
    /// As [`Self::get_or_load`].
    pub fn get_or_load_published(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        builder: &dyn WorkspaceBuilder,
        working_set_estimate: u64,
    ) -> Result<Arc<PublishedGraph>, DaemonError> {
        self.load_published(key, builder, working_set_estimate)
            .map(|(published, _origin)| published)
    }

    /// [`Self::get_or_load_published`], also saying whether this call built
    /// the generation it returns ([`LoadOrigin::Built`]) or found it
    /// `Loaded` at the gate ([`LoadOrigin::Found`]): another load (or a
    /// rebuild) published it, with that caller's builder. A caller whose
    /// answer depends on its own build (the daemon-hosted `rebuild_index`,
    /// which records the options it was given) must not report a found
    /// generation as the one it built (integration round 7, S2).
    ///
    /// # Errors
    ///
    /// As [`Self::get_or_load`].
    pub fn load_published(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        builder: &dyn WorkspaceBuilder,
        working_set_estimate: u64,
    ) -> Result<(Arc<PublishedGraph>, LoadOrigin), DaemonError> {
        let (ws, registered_key, entry) = match self.prepare_load_gate(key)? {
            LoadGate::Loaded(published) => return Ok((published, LoadOrigin::Found)),
            LoadGate::Acquired {
                workspace,
                registered_key,
                entry,
            } => (workspace, registered_key, entry),
        };
        let leaves_no_slot = builder.failed_load_leaves_no_slot();

        // --- Step 3: arm LoadingGuard for panic / early-return --
        let mut loading = LoadingGuard {
            ws: &ws,
            key: &registered_key,
            armed: true,
        };

        // --- Step 3b: refuse before reserving ---------------------
        //
        // The reservation below can evict sibling workspaces (its LRU
        // phase runs before it commits), so every input the build would
        // refuse (an unreadable manifest, an id this binary did not
        // compile, an empty, missing or unrecordable expand cache) is
        // resolved first. The prepared build runs with what was resolved.
        //
        // A refusal of the request (B2, round 7 audit), or any failure of
        // a builder whose failed load leaves the index as it found it, is
        // the caller's own: the slot is put back as the gate found it
        // (`Self::abandon_load`), so it never breaks later queries of a
        // healthy index. A refusal of the on-disk index (an unreadable
        // manifest, an uncompiled id) is recorded as before (D-12); the
        // query path's reload rule re-reads that index on the next query.
        let prepared = match builder.prepare_build(&key.source_root) {
            Ok(prepared) => prepared,
            Err(refusal) => {
                if leaves_no_slot || is_request_refusal(&refusal) {
                    self.abandon_load(&ws, &mut loading, entry);
                } else {
                    Self::fail_load_before_reservation(
                        &ws,
                        &mut loading,
                        &key.source_root,
                        &refusal,
                    );
                }
                return Err(refusal);
            }
        };

        // --- Step 4: reserve admission headroom ------------------
        //
        // A budget that cannot admit the load (S8, round 7 audit) is a
        // refusal before anything was built: the slot is put back as the
        // gate found it, so the next query retries the load, and is
        // refused the same way until the budget admits it.
        let reservation = match self.reserve_rebuild(&registered_key, working_set_estimate) {
            Ok(reservation) => reservation,
            Err(err) => {
                if leaves_no_slot || is_request_refusal(&err) {
                    self.abandon_load(&ws, &mut loading, entry);
                }
                return Err(err);
            }
        };

        // --- Step 5: build the graph ----------------------------
        let built = match prepared() {
            Ok(g) => g,
            Err(err) => {
                drop(reservation);
                if leaves_no_slot || is_request_refusal(&err) {
                    self.abandon_load(&ws, &mut loading, entry);
                    return Err(err);
                }
                // The LoadingGuard will flip us to Failed + record
                // a synthetic error; overwrite with the builder's
                // real error for diagnostic fidelity.
                ws.record_failure(clone_err(&err));
                loading.armed = false;
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
                {
                    Self::log_lost_load_transition(&key.source_root, observed);
                }
                return Err(err);
            }
        };

        // --- Step 5b: compact edges (delta -> CSR) before publish ---
        //
        // `build_unified_graph` (via the builder) leaves every edge in the
        // delta buffer; compaction lives only in the persist transaction, which
        // this cold-load path does not run. Without compacting, `edges_from` /
        // `edges_to` rescan the whole delta on every call, so per-node graph
        // traversals (find_cycles, is_node_in_cycle, complexity_metrics,
        // generate_overview) are O(N x |delta|) on the resident graph and time
        // out. Compacting here makes `edges_from` O(1). The CPU-heavy CSR build
        // runs before we take `workspaces.read()`, so no manager lock is held.
        // Fail closed on error, mirroring the build-failure arm above.
        if let Err(err) =
            sqry_core::graph::unified::compaction::compact_edges_in_place(&built.graph)
        {
            drop(reservation);
            let compact_err = DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: format!("edge compaction failed: {err}"),
            };
            ws.record_failure(clone_err(&compact_err));
            loading.armed = false;
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(compact_err);
        }

        // --- Step 6+7: atomic re-check + publish -------------
        //
        // Hold `workspaces.read()` across the final cancellation
        // / map-membership re-check AND the `publish_and_retain`
        // call. `execute_eviction` holds `workspaces.write()` for
        // the duration of every eviction, so the RwLock makes the
        // publish critical section atomic with respect to
        // eviction: either eviction has fully completed (the map
        // lookup fails), or eviction has not started (and cannot
        // start while we hold the read lock).
        //
        // Lock order per plan §J.4: `workspaces → admission`.
        // `publish_and_retain` takes `admission` internally;
        // that nests under our `workspaces.read()` correctly.
        //
        // Codex Task 6 Phase 6b iter-2 MAJOR: the iter-1 version
        // released `workspaces.read()` after the map-membership
        // check and then called `publish_and_retain` unlocked.
        // Eviction could slip in between the two, satisfying
        // both re-checks yet still reaching `remove(key)` after
        // our publish. Holding the read lock across the publish
        // closes the window.
        let workspaces_guard = self.workspaces.read();

        // Cancellation check INSIDE the read lock. If cancellation
        // was set before we grabbed the lock, we still observe it;
        // if it's set after we release, a future load will see it.
        if ws.rebuild_cancelled.load(Ordering::Acquire) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-load".to_string(),
            });
            loading.armed = false;
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-load".to_string(),
            });
        }
        if !Self::registers(&workspaces_guard, &ws) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-load".to_string(),
            });
            loading.armed = false;
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-load".to_string(),
            });
        }

        // Publish while still holding `workspaces.read()`. An
        // eviction started in parallel is blocked on
        // `workspaces.write()` and cannot observe / mutate this
        // workspace until we release.
        //
        // Per Codex Task 6 Phase 6c iter-2 MAJOR: the hook dispatch
        // is deliberately NOT performed inside `publish_and_retain`
        // — firing it here would nest `self.hook.read()` under
        // `workspaces.read()`, creating a re-entrancy deadlock for
        // any hook impl that calls back into manager methods
        // needing `workspaces.write()` (e.g. `unload`). The fix
        // returns the published generation (graph and record, design
        // D20) from `publish_and_retain`, releases `workspaces_guard`,
        // and THEN invokes `on_publish` under a disjoint short-lived
        // `self.hook.read()` acquisition.
        //
        // `G_daemon_control_plane.md` §3.5 caller-migration table —
        // get_or_load (production caller 1). On post-build oversize,
        // surface `DaemonError::WorkspaceOversize`; admission bytes
        // are refunded by the reservation's RAII Drop on early
        // return.
        let (_token, published) = match self.publish_and_retain(reservation, &ws, built) {
            Ok((token, published)) => (token, published),
            Err(e) => {
                drop(workspaces_guard);
                ws.record_failure(clone_err(&e));
                loading.armed = false;
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
                {
                    Self::log_lost_load_transition(&key.source_root, observed);
                }
                return Err(e);
            }
        };
        ws.record_success(std::time::SystemTime::now());
        ws.store_state(WorkspaceState::Loaded);
        ws.touch();
        loading.armed = false;
        drop(workspaces_guard);

        // Hook fires OUTSIDE every outer lock. The only lock taken
        // here is `self.hook.read()` (for the brief clone inside
        // `hook_snapshot`). A hook impl is now free to call any
        // manager method — including `unload`, which needs
        // `workspaces.write()` — without deadlocking against the
        // loader that fired it. The dispatch itself is synchronous
        // but spawn-only: hook impls are expected to return
        // immediately after scheduling background work.
        self.dispatch_publish_hook(&key.source_root, Arc::clone(&published.graph));

        Ok((published, LoadOrigin::Built))
    }

    /// Test-only: [`Self::get_or_insert_workspace_tracked`]'s entry alone.
    #[cfg(test)]
    fn get_or_insert_workspace(&self, key: &WorkspaceKey) -> Arc<LoadedWorkspace> {
        self.get_or_insert_workspace_tracked(key).0
    }

    /// Look up or insert a [`LoadedWorkspace`] for `key`. Returns the
    /// shared `Arc` so both the caller and the manager map reference the
    /// same state, and whether this call inserted the entry (`true`) or
    /// found it (`false`), which a load whose failure is its caller's own
    /// reads to remove an entry it inserted (`Self::abandon_load`).
    fn get_or_insert_workspace_tracked(&self, key: &WorkspaceKey) -> (Arc<LoadedWorkspace>, bool) {
        // Upgrade path — try a read first to avoid the write-lock
        // cost when the entry already exists.
        {
            let workspaces = self.workspaces.read();
            if key.workspace_id.is_none()
                && let Some((_, ws)) =
                    Self::anonymous_workspace_by_source_root(&workspaces, &key.source_root)
            {
                return (Arc::clone(ws), false);
            }
            if let Some(ws) = workspaces.get(key) {
                return (Arc::clone(ws), false);
            }
        }
        let mut workspaces = self.workspaces.write();

        // Coalesce anonymous (workspace_id=None) loads for the same
        // canonical source_root. Different callers (preload with
        // WorkspaceFolder, daemon/load handler with its default
        // root_mode + fingerprint=0, mcp_host/acquirer paths, etc.)
        // may construct WorkspaceKey values that differ in the
        // secondary dimensions even though they target the identical
        // on-disk path. Without this, the HashMap would contain
        // multiple entries for the "same" workspace (see #393).
        // Any subsequent load for the path, regardless of the exact
        // secondary fields in the caller's key, must operate on the
        // registered ws instance for that source_root. Clean coalesce
        // returns the first entry because it is the only existing match;
        // historical duplicate maps use a stable key ordering so the
        // winner is deterministic rather than HashMap-order dependent.
        // This ensures:
        //   - get_or_load hits the Loaded fast-path on the real entry
        //   - no second Arc<LoadedWorkspace> is ever inserted
        //   - status shows the path only once
        //   - reset-by-path can clear the (logical) workspace.
        if key.workspace_id.is_none()
            && let Some((_, ws)) =
                Self::anonymous_workspace_by_source_root(&workspaces, &key.source_root)
        {
            return (Arc::clone(ws), false);
        }

        let mut inserted = false;
        let ws = Arc::clone(workspaces.entry(key.clone()).or_insert_with(|| {
            inserted = true;
            Arc::new(LoadedWorkspace::new(key.clone(), false))
        }));
        (ws, inserted)
    }

    /// Whether an eviction of `ws` would reclaim a graph, the test both
    /// eviction paths ([`Self::evict_lru`] and the reservation's eviction
    /// plan) apply before choosing it.
    ///
    /// An `Evicted` tombstone holds nothing. An `Unloaded` workspace holds
    /// nothing after `daemon/reset` or before its first load, but a rebuild
    /// cancelled into `Unloaded` (`daemon/cancel_rebuild`, or a reset that
    /// had to dispatch a cancellation) keeps its graph and its admission
    /// bytes, because its watcher's next rebuild reads them (decision
    /// D-i8-44 in `docs/development/surface-parity/04_PROGRESS-surface-parity.md`).
    /// Before D-i8-44 both paths skipped every `Unloaded` workspace, so
    /// those bytes stayed counted with nothing able to reclaim them. The
    /// counted bytes decide it: an `Unloaded` workspace is a candidate
    /// exactly when it counts some.
    fn holds_evictable_graph(ws: &LoadedWorkspace) -> bool {
        match ws.load_state() {
            WorkspaceState::Evicted => false,
            WorkspaceState::Unloaded => ws.memory_bytes.load(Ordering::Acquire) > 0,
            _ => true,
        }
    }

    /// Evict the least-recently-accessed non-pinned workspace, if
    /// any. Returns the evicted key on success, `None` if there are
    /// no eligible candidates.
    pub fn evict_lru(&self) -> Option<WorkspaceKey> {
        let candidate = {
            let workspaces = self.workspaces.read();
            workspaces
                .iter()
                .filter(|(_, ws)| !ws.pinned && Self::holds_evictable_graph(ws))
                .min_by_key(|(_, ws)| *ws.last_accessed.read())
                .map(|(k, _)| k.clone())
        };
        if let Some(key) = &candidate {
            self.execute_eviction(key);
        }
        candidate
    }

    /// Explicitly unload a workspace. Drives a full eviction
    /// (releases graph data + admission accounting via
    /// [`Self::evict_to_tombstone_locked`]) **and** removes the
    /// tombstone entry from the manager map atomically under a
    /// single `workspaces.write()` critical section.
    ///
    /// This is the only path that removes the map entry. LRU
    /// eviction (`evict_lru`, `reserve_rebuild`'s Phase 2) leaves
    /// the tombstone in place so per-source-root partial-eviction
    /// state stays observable through `daemon/workspaceStatus` —
    /// see [`Self::execute_eviction`] doc and `STEP_6` iter-1 BLOCK.
    ///
    /// Returns `true` if the workspace was present, `false` if it
    /// was already absent.
    pub fn unload(&self, key: &WorkspaceKey) -> bool {
        let mut workspaces = self.workspaces.write();
        let target_key = if workspaces.contains_key(key) {
            key.clone()
        } else if key.workspace_id.is_none() {
            // #393: tolerate unload by divergent anon key; remove the
            // deterministic registered entry for the source_root.
            match Self::anonymous_workspace_by_source_root(&workspaces, &key.source_root) {
                Some((k, _)) => k.clone(),
                None => return false,
            }
        } else {
            return false;
        };
        // Drop graph + admission bytes under the same write lock
        // we will use for `remove`. Holding the lock across both
        // operations means external observers see EITHER "entry
        // present + Loaded" OR "entry absent" — never the "entry
        // present + Evicted but about to be removed" intermediate
        // state. (LRU eviction is a separate flow that DOES expose
        // the Evicted tombstone — that is the STEP_6 contract.)
        self.evict_to_tombstone_locked(&mut workspaces, &target_key);
        workspaces.remove(&target_key);
        true
    }

    /// Helper: run the eviction body (steps 1–4 of
    /// [`Self::execute_eviction`]) with the caller's
    /// `workspaces.write()` guard already held. Used by
    /// [`Self::unload`] so unloading remains atomic — no observer
    /// sees the `Evicted`-but-still-in-map intermediate window.
    ///
    /// Re-eviction safety mirrors `execute_eviction` — an entry
    /// already in `Evicted` is left alone.
    fn evict_to_tombstone_locked(
        &self,
        workspaces: &mut HashMap<WorkspaceKey, Arc<LoadedWorkspace>>,
        key: &WorkspaceKey,
    ) {
        let Some(ws) = workspaces.get(key).cloned() else {
            return;
        };
        if ws.load_state() == WorkspaceState::Evicted {
            // Already a tombstone, so there is nothing to drop; but the
            // watcher signal is set again, so an `unload` or a `reset` of a
            // tombstone stops any watcher still attached to it.
            ws.stop_watcher();
            return;
        }

        // One swap replaces the graph and its record together (design
        // D14): a tombstone carries the placeholder generation, never the
        // old graph beside no record or the reverse. The retained entry
        // keeps the old graph `Arc` alive for slow readers; the pair it
        // came from is dropped here so the reaper's `strong_count == 1`
        // test sees only the retained holder once every reader lets go.
        let old_published = ws.published.swap(Arc::new(PublishedGraph::placeholder()));
        let old_arc = Arc::clone(&old_published.graph);
        drop(old_published);
        let prior_bytes_usize = ws.memory_bytes.swap(0, Ordering::AcqRel);
        let prior_bytes = prior_bytes_usize as u64;

        let token = OldGraphToken::new();
        {
            let mut state = self.admission.lock();
            state.loaded_bytes = state.loaded_bytes.saturating_sub(prior_bytes);
            state.retained_old.insert(
                token,
                RetainedEntry {
                    bytes: prior_bytes,
                    graph: old_arc,
                    published_at: Instant::now(),
                    warned_past_timeout: false,
                },
            );
            self.bump_high_water(&state);
        }

        // A runner's iteration that has not published yet must still see
        // this eviction's cancellation after a load gate has run
        // (`Self::honor_preexisting_cancel`): it is mid-iteration exactly
        // when the slot is `Rebuilding` here, because its publish moves
        // the slot to `Loaded` under the read guard this write excludes.
        ws.evicted_mid_iteration.store(
            ws.load_state() == WorkspaceState::Rebuilding,
            Ordering::Release,
        );
        ws.rebuild_cancelled.store(true, Ordering::Release);
        // The tombstone writers are the only callers that stop the file
        // watcher; cancelling a rebuild leaves it watching. The signal and
        // the state are stored under one lock, the one a watcher's arming
        // reads the state under (`LoadedWorkspace::arm_watcher_stop`), so
        // a watcher started as this eviction lands is never armed with a
        // clear signal on the tombstone.
        ws.stop_watcher_and_store_state(WorkspaceState::Evicted);
    }

    /// Cluster-G §3.2: reset a workspace to `Unloaded` *without*
    /// removing its manager-map entry.
    ///
    /// Takes the in-memory graph out of the workspace and refunds its
    /// admission bytes (the old graph moves to the retained set, which
    /// keeps it alive for readers still holding it until the reaper frees
    /// it), and stops its file watcher, but preserves the
    /// `WorkspaceKey`, the `pinned` bit and `last_error`. Files under
    /// `<root>/.sqry/` are left untouched: destructive cleanup is owned by
    /// `sqry workspace clean` (cluster-E IMP-E.4). The workspace stays
    /// `Unloaded` until a `daemon/load` (or a tool call's load) brings it
    /// back.
    ///
    /// A workspace whose rebuild is in flight is not reset (decision
    /// D-i7-3 in `docs/development/surface-parity/04_PROGRESS-surface-parity.md`):
    /// "in flight" is a runner holding the rebuild runner role, read under
    /// the workspace's rebuild lane, which every runner-role transition
    /// takes, not the `Rebuilding` state. The runner is cancelled as
    /// `daemon/cancel_rebuild` cancels it, the requests parked behind it
    /// are answered `-32004` when it consumes the cancellation, and the
    /// call answers [`DaemonError::ResetCancellationDispatched`] for the
    /// caller to retry. So a request parked before the reset never runs
    /// after it: there is no rebuild left to undo the reset. Before the
    /// repair the reset keyed on the state, so it reset a workspace whose
    /// runner was between iterations (and the parked request then rebuilt
    /// it, `Loaded` with its watcher stopped), and it set the flag without
    /// the lane, so a `Rebuilding` workspace with no runner kept the flag
    /// with nothing to consume it (the next rebuild was cancelled at its
    /// gate).
    ///
    /// The lane is taken with `try_lock` under `workspaces.write()` (the
    /// order `workspaces` then `rebuild_lane`), retried with the guard
    /// released: a lane holder never awaits and never takes `workspaces`,
    /// so the retry ends as soon as that holder's few instructions do.
    ///
    /// Returns `Ok(true)` if the workspace was present and reset,
    /// `Ok(false)` if not present. `key` must be a registered key (the
    /// `daemon/reset` handler passes each entry `find_all_by_source_root`
    /// returns, historical duplicates included), so it is not resolved.
    ///
    /// State transitions, with no runner in flight:
    ///   `Loaded` / `Failed` / `Evicted` / `Unloaded` / `Rebuilding` → `Unloaded`
    ///   `Loading` → [`Err(ResetWhileLoading)`]
    /// With a runner in flight: cancellation dispatched, the state left to
    /// the runner, [`Err(ResetCancellationDispatched)`].
    ///
    /// `pinned` workspaces require `force = true` to reset; without
    /// it, [`Err(WorkspacePinned)`] is returned.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspacePinned`] when the workspace is pinned
    ///   and `force = false`.
    /// - [`DaemonError::ResetWhileLoading`] when the workspace is
    ///   currently loading (caller must wait or cancel via the
    ///   existing `daemon/cancel_rebuild` path).
    /// - [`DaemonError::ResetCancellationDispatched`] when a runner holds
    ///   the rebuild runner role; the caller should retry after
    ///   `retry_after_ms`. Also answered, with nothing dispatched, in the
    ///   one case the lane stays held for [`RESET_LANE_WAIT`] (a lane
    ///   holder descheduled that long), so the caller retries then too.
    pub fn reset(self: &Arc<Self>, key: &WorkspaceKey, force: bool) -> Result<bool, DaemonError> {
        use crate::error::DaemonError;
        let started = Instant::now();
        let mut spins: u32 = 0;
        loop {
            let mut workspaces = self.workspaces.write();
            let Some(ws) = workspaces.get(key).cloned() else {
                return Ok(false);
            };
            if ws.pinned && !force {
                return Err(DaemonError::WorkspacePinned {
                    root: key.source_root.clone(),
                });
            }
            if ws.load_state() == WorkspaceState::Loading {
                return Err(DaemonError::ResetWhileLoading {
                    root: key.source_root.clone(),
                });
            }
            let Ok(lane) = ws.rebuild_lane.try_lock() else {
                drop(workspaces);
                if started.elapsed() >= RESET_LANE_WAIT {
                    return Err(DaemonError::ResetCancellationDispatched {
                        root: key.source_root.clone(),
                        retry_after_ms: 250,
                    });
                }
                spins = spins.saturating_add(1);
                if spins < 64 {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
                continue;
            };
            if ws.rebuild_in_flight.load(Ordering::Acquire) {
                // A runner holds the role and cannot release it while this
                // lane guard is held, so it observes the flag (at its gate,
                // at the latest before it releases) and consumes it.
                ws.rebuild_cancelled.store(true, Ordering::Release);
                drop(lane);
                drop(workspaces);
                return Err(DaemonError::ResetCancellationDispatched {
                    root: key.source_root.clone(),
                    retry_after_ms: 250,
                });
            }
            // No runner, and none can take the role while the lane is held.
            // Drop the graph and refund its admission bytes through the
            // tombstone helper (which also stops the watcher), then move to
            // `Unloaded`, preserving the map entry, `pinned` and
            // `last_error`. A `Rebuilding` state here has no runner behind
            // it, so it is reset like any other.
            self.evict_to_tombstone_locked(&mut workspaces, key);
            // Cluster-G iter-2 BLOCKER 1: the tombstone helper sets
            // `rebuild_cancelled`. With no runner to consume it, the next
            // load's gate (`honor_preexisting_cancel`, from `Unloaded`)
            // would refuse it as "workspace evicted mid-load", so `daemon
            // reset` could not recover the workspace it just reset. Clear
            // it: a reset leaves no cancellation behind.
            ws.rebuild_cancelled.store(false, Ordering::Release);
            ws.stop_watcher_and_store_state(WorkspaceState::Unloaded);
            drop(lane);
            return Ok(true);
        }
    }

    /// Find a loaded workspace by its directory path.
    ///
    /// Linear scan over all registered workspaces comparing each workspace's
    /// `index_root` against `path`. Callers (e.g. `daemon/rebuild`) supply a
    /// canonicalised path but not the full [`WorkspaceKey`].
    /// O(n) in the number of loaded workspaces; in practice n is small.
    ///
    /// Returns `None` if no workspace with a matching root is found.
    #[must_use]
    pub fn find_key_and_workspace_by_path(
        &self,
        path: &std::path::Path,
    ) -> Option<(WorkspaceKey, Arc<LoadedWorkspace>)> {
        let workspaces = self.workspaces.read();
        workspaces
            .iter()
            .filter(|(k, _)| k.source_root == path)
            .min_by(|(left, _), (right, _)| Self::workspace_key_stable_cmp(left, right))
            .map(|(k, ws)| (k.clone(), Arc::clone(ws)))
    }

    /// Return *every* registered entry whose `source_root` matches the
    /// given canonical path.
    ///
    /// This is the path-based counterpart to lookup-by-exact-`WorkspaceKey`.
    /// It exists so that `daemon/reset <path>` (and any future path-based
    /// recovery) can affect all entries that a user or script would
    /// consider "the workspace at this path", even if historical bugs
    /// (#393) left multiple `WorkspaceKey`s (differing only in
    /// `root_mode`/`config_fingerprint`/`workspace_id`) for the same
    /// `source_root`.
    ///
    /// After the coalesce logic in `get_or_insert_workspace_tracked`, new
    /// plain-path anonymous loads will no longer create such dups;
    /// this method + the reset handler change below let operators
    /// recover from any pre-existing duplicates.
    #[must_use]
    pub fn find_all_by_source_root(
        &self,
        path: &std::path::Path,
    ) -> Vec<(WorkspaceKey, Arc<LoadedWorkspace>)> {
        let workspaces = self.workspaces.read();
        let mut matches: Vec<_> = workspaces
            .iter()
            .filter(|(k, _)| k.source_root == path)
            .map(|(k, ws)| (k.clone(), Arc::clone(ws)))
            .collect();
        matches.sort_by(|(left, _), (right, _)| Self::workspace_key_stable_cmp(left, right));
        matches
    }

    /// Resolve a canonical path to the loaded workspace root that owns it.
    ///
    /// Issue #394 Part 1b: a daemon-hosted tool `path` may name a subdirectory
    /// of a loaded workspace rather than the workspace root itself. This returns
    /// the longest registered `source_root` that is an ancestor-or-equal of
    /// `path` (the "owning" workspace), so the acquirer can classify against the
    /// owning root and let the shared inner tool body scope results to the
    /// requested subtree. Returns `None` when `path` is not contained by any
    /// registered workspace, in which case the caller keeps the path as-is and
    /// classification surfaces the existing "not loaded" error.
    ///
    /// O(n) in the number of registered workspaces; in practice n is small.
    #[must_use]
    pub fn find_owning_workspace_root(&self, path: &std::path::Path) -> Option<std::path::PathBuf> {
        let workspaces = self.workspaces.read();
        let roots: Vec<std::path::PathBuf> =
            workspaces.keys().map(|k| k.source_root.clone()).collect();
        sqry_core::workspace::scope::owning_workspace_root(
            path,
            roots.iter().map(std::path::PathBuf::as_path),
        )
    }

    /// Snapshot of daemon-wide status. Point-in-time, non-transactional.
    pub fn status(&self) -> DaemonStatus {
        self.status_with_watcher_state(|_| false)
    }

    /// Snapshot of daemon-wide status with caller-supplied watcher
    /// liveness. The [`WorkspaceManager`] does not own file watchers;
    /// production `daemon/status` supplies this from
    /// [`crate::RebuildDispatcher`].
    pub fn status_with_watcher_state<F>(&self, mut is_watching: F) -> DaemonStatus
    where
        F: FnMut(&WorkspaceKey) -> bool,
    {
        // Records are captured under the read lock and compared against the
        // on-disk manifest AFTER the lock drops: the comparison reads a file,
        // and holding `workspaces.read()` across file IO would stall every
        // eviction for the duration of a status call.
        let mut roster_records: Vec<Option<Arc<RosterRecord>>> = Vec::new();
        let mut workspaces_snapshot: Vec<WorkspaceStatus> = {
            let workspaces = self.workspaces.read();
            let mut raw_entries: Vec<_> = workspaces.iter().collect();
            raw_entries
                .sort_by(|(left, _), (right, _)| Self::workspace_key_stable_cmp(left, right));

            let mut seen_anonymous_roots = HashSet::new();
            let entries: Vec<_> = raw_entries
                .into_iter()
                .filter_map(|(k, ws)| {
                    debug_assert_eq!(
                        ws.resident_handle_kind(),
                        sqry_daemon_protocol::ResidentHandleKind::LiveWorkspace
                    );
                    if k.workspace_id.is_none()
                        && !seen_anonymous_roots.insert(k.source_root.clone())
                    {
                        return None;
                    }
                    roster_records.push(ws.roster());
                    Some(WorkspaceStatus {
                        index_root: k.source_root.clone(),
                        state: ws.load_state(),
                        pinned: ws.pinned,
                        watching: is_watching(k),
                        current_bytes: ws.current_memory_bytes(),
                        high_water_bytes: ws.memory_high_water_bytes.load(Ordering::Acquire) as u64,
                        last_good_at: *ws.last_good_at.read(),
                        last_error: ws
                            .last_error
                            .read()
                            .as_ref()
                            .map(std::string::ToString::to_string),
                        retry_count: ws.retry_count.load(Ordering::Acquire),
                        // STEP_12 telemetry: surface both display and machine
                        // identity hex forms when the key carries a logical
                        // workspace_id; anonymous keys leave both as None so
                        // the wire shape is uniform.
                        workspace_id_short: k
                            .workspace_id
                            .as_ref()
                            .map(sqry_daemon_protocol::WorkspaceId::as_short_hex),
                        workspace_id_full: k
                            .workspace_id
                            .as_ref()
                            .map(sqry_daemon_protocol::WorkspaceId::as_full_hex),
                        plugin_roster: None,
                    })
                })
                .collect();
            entries
        };
        let load_roster = shared_load_roster();
        for (status, record) in workspaces_snapshot.iter_mut().zip(roster_records) {
            status.plugin_roster =
                record.map(|record| roster_status_for(&status.index_root, &record, &load_roster));
        }

        let revisions = self.resident_revision_statuses(None, false);
        let resident_revision_bytes = self.resident_revisions.memory_bytes();
        let resident_revision_high_water = self.resident_revisions.memory_high_water_bytes();

        let (live_workspace_bytes, reserved_bytes, high_water_bytes) = {
            let state = self.admission.lock();
            let current = state.total_committed_bytes();
            let reserved = state.reserved_bytes;
            let combined_current = current.saturating_add(resident_revision_bytes);
            // Bump high-water here in case the status read saw a
            // higher value than the last mutation captured. The
            // `drop(state)` at the end of this block keeps the
            // admission lock held across the `fetch_max` — serialising
            // the high-water update with any concurrent publish.
            let peak = self
                .total_memory_high_water
                .fetch_max(combined_current, Ordering::AcqRel);
            let peak = peak.max(combined_current).max(resident_revision_high_water);
            drop(state);
            (current, reserved, peak)
        };
        let current_bytes = live_workspace_bytes.saturating_add(resident_revision_bytes);

        DaemonStatus {
            uptime_seconds: self.started_at.elapsed().as_secs(),
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            memory: MemoryStatus {
                limit_bytes: self.memory_limit_bytes(),
                current_bytes,
                reserved_bytes,
                live_workspace_bytes,
                resident_revision_bytes,
                high_water_bytes,
            },
            workspaces: workspaces_snapshot,
            revisions,
        }
    }

    fn anonymous_workspace_by_source_root<'a>(
        workspaces: &'a HashMap<WorkspaceKey, Arc<LoadedWorkspace>>,
        source_root: &Path,
    ) -> Option<(&'a WorkspaceKey, &'a Arc<LoadedWorkspace>)> {
        workspaces
            .iter()
            .filter(|(k, _)| k.workspace_id.is_none() && k.source_root == source_root)
            .min_by(|(left, _), (right, _)| Self::workspace_key_stable_cmp(left, right))
    }

    fn workspace_key_stable_cmp(left: &WorkspaceKey, right: &WorkspaceKey) -> std::cmp::Ordering {
        left.source_root
            .cmp(&right.source_root)
            .then_with(|| match (&left.workspace_id, &right.workspace_id) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(left_id), Some(right_id)) => {
                    left_id.as_full_hex().cmp(&right_id.as_full_hex())
                }
            })
            .then_with(|| left.root_mode.as_str().cmp(right.root_mode.as_str()))
            .then_with(|| left.config_fingerprint.cmp(&right.config_fingerprint))
    }

    /// Enumerate the `.sqry/graph` directories belonging to every
    /// workspace currently in `state ∈ {Loading, Loaded, Rebuilding}`.
    ///
    /// This is the data source for the `daemon/active-artifacts`
    /// IPC method (per `00_contracts.md` §3.CC-4 + `E_p1_cluster.md`
    /// §E.4 DPG hand-off). The returned paths are absolute, in stable
    /// `WorkspaceKey::source_root` order, and include only the
    /// concrete `.sqry/graph` subdirectory — `<source_root>/.sqry/graph`
    /// — because that is the path `sqry workspace clean` discovers
    /// when it walks for stale artifacts.
    ///
    /// Read-only, concurrent-safe: takes `self.workspaces.read()`
    /// for the duration of the iteration; the caller is expected to
    /// honour the 250 ms response budget so the read lock does not
    /// stall a concurrent admission write.
    ///
    /// `Unloaded`, `Evicted`, and `Failed` states are deliberately
    /// excluded — those workspaces are not "live" artifacts and may
    /// be safely cleaned by the operator.
    #[must_use]
    pub fn active_artifact_dirs(&self) -> Vec<std::path::PathBuf> {
        use sqry_daemon_protocol::WorkspaceState;

        let workspaces = self.workspaces.read();
        let mut out: Vec<std::path::PathBuf> = workspaces
            .iter()
            .filter_map(|(key, ws)| {
                let state = ws.load_state();
                let live = matches!(
                    state,
                    WorkspaceState::Loading | WorkspaceState::Loaded | WorkspaceState::Rebuilding
                );
                if live {
                    Some(key.source_root.join(".sqry").join("graph"))
                } else {
                    None
                }
            })
            .collect();
        out.sort();
        out
    }

    /// Aggregate `daemon/workspaceStatus` snapshot for a single
    /// `workspace_id` (`STEP_6` of the workspace-aware-cross-repo plan).
    ///
    /// Walks the manager's workspace map, collects every
    /// [`WorkspaceKey`] whose `workspace_id == Some(target_id)`, and
    /// renders a deterministic per-source-root rollup. Per-source-root
    /// LRU eviction means individual entries can carry
    /// [`WorkspaceState::Evicted`] while siblings remain
    /// [`WorkspaceState::Loaded`] — the aggregate exposes that
    /// "partially evicted" shape unchanged via
    /// [`sqry_daemon_protocol::WorkspaceIndexStatus::partially_evicted`].
    ///
    /// Returns `None` when no entry in the map carries the requested
    /// `workspace_id`. The IPC layer surfaces that as
    /// `DaemonError::WorkspaceNotLoaded`; the manager itself does not
    /// classify "no entries" as an error so callers can distinguish a
    /// genuinely absent grouping from an empty workspace.
    #[must_use]
    pub fn workspace_index_status(
        &self,
        target_id: &sqry_daemon_protocol::WorkspaceId,
    ) -> Option<sqry_daemon_protocol::WorkspaceIndexStatus> {
        let workspaces = self.workspaces.read();
        let mut rows: Vec<sqry_daemon_protocol::WorkspaceSourceRootStatus> = workspaces
            .iter()
            .filter_map(|(k, ws)| {
                k.workspace_id
                    .as_ref()
                    .filter(|id| *id == target_id)
                    .map(|_| sqry_daemon_protocol::WorkspaceSourceRootStatus {
                        source_root: k.source_root.clone(),
                        state: ws.load_state(),
                        current_bytes: ws.memory_bytes.load(Ordering::Acquire) as u64,
                        // STEP_11_4 — probe `<source_root>/.sqry/classpath/`
                        // for presence. Status path; never blocks on
                        // anything heavier than `fs::metadata`. Probe
                        // failures (permission denied, racy unlink, …)
                        // collapse to `false`; the LSP-side
                        // `WorkspaceIndexStatus.warnings` channel surfaces
                        // the underlying error detail when the daemon's
                        // workspace builder hits the same probe.
                        classpath_present: probe_classpath_present(&k.source_root),
                    })
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        rows.sort_by(|a, b| a.source_root.cmp(&b.source_root));
        Some(sqry_daemon_protocol::WorkspaceIndexStatus {
            workspace_id: *target_id,
            // STEP_12 — derive the hex display strings here so JSON
            // consumers (`sqry daemon status --json`, MCP redaction,
            // CI scripts) never have to re-encode the 32-byte digest
            // themselves. The two strings are byte-derivative of
            // `workspace_id`; they do not introduce a new identity
            // axis.
            workspace_id_short: target_id.as_short_hex(),
            workspace_id_full: target_id.as_full_hex(),
            source_roots: rows,
        })
    }

    /// Bump the daemon-wide high-water mark using the current
    /// `AdmissionState`. Must be called with `admission` held.
    fn bump_high_water(&self, state: &AdmissionState) {
        let current = state.total_committed_bytes();
        self.total_memory_high_water
            .fetch_max(current, Ordering::AcqRel);
    }

    /// Test-only helper: insert a `LoadedWorkspace` into the manager
    /// map in a specific state, bypassing `get_or_load`. Used by
    /// `classify_for_serve` integration tests that need to observe
    /// the `Unloaded` / `Loading` arms (both states are transient
    /// during the normal load path).
    ///
    /// `#[doc(hidden)]` to signal "test affordance only" — same
    /// pattern as [`crate::TestGate`] / [`crate::TestCapture`].
    /// Production code should not call this.
    #[doc(hidden)]
    pub fn insert_workspace_in_state_for_test(&self, key: WorkspaceKey, state: WorkspaceState) {
        let ws = Arc::new(LoadedWorkspace::new(key.clone(), false));
        ws.store_state(state);
        // A synthetic workspace carries the fast-path record so servable
        // states classify the way a published one does: one generation,
        // the placeholder's empty graph beside the record.
        ws.published.store(Arc::new(PublishedGraph::new(
            ws.graph(),
            Some(Arc::new(RosterRecord::fast_path_default())),
        )));
        self.workspaces.write().insert(key, ws);
    }

    /// Test-only helper: like [`Self::insert_workspace_in_state_for_test`]
    /// but with NO roster record, to plant the state every publish path
    /// is required to make unreachable. `classify_for_serve` must refuse
    /// such a workspace with [`DaemonError::Internal`] (T13).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn insert_workspace_without_roster_for_test(
        &self,
        key: WorkspaceKey,
        state: WorkspaceState,
    ) {
        let ws = Arc::new(LoadedWorkspace::new(key.clone(), false));
        ws.store_state(state);
        self.workspaces.write().insert(key, ws);
    }

    /// Test-only helper: insert a `LoadedWorkspace` into the manager
    /// map with explicit state, pinning, and pre-set `memory_bytes`.
    /// STEP_6 LRU + workspace-aggregate tests use this to exercise
    /// per-source-root eviction without spinning up a full
    /// `RealWorkspaceBuilder` pipeline. Returns the inserted Arc so
    /// the caller can keep observing it (e.g. to assert `load_state`
    /// after a follow-up mutation).
    ///
    /// `#[doc(hidden)]` to signal "test affordance only".
    #[doc(hidden)]
    pub fn insert_workspace_for_test_with_bytes(
        &self,
        key: WorkspaceKey,
        state: WorkspaceState,
        pinned: bool,
        bytes: usize,
    ) -> Arc<LoadedWorkspace> {
        let ws = Arc::new(LoadedWorkspace::new(key.clone(), pinned));
        ws.store_state(state);
        ws.update_memory(bytes);
        ws.published.store(Arc::new(PublishedGraph::new(
            ws.graph(),
            Some(Arc::new(RosterRecord::fast_path_default())),
        )));
        self.workspaces.write().insert(key, Arc::clone(&ws));
        ws
    }

    /// Acquire the internal `workspaces` `RwLock` in read mode.
    ///
    /// Task 7 Phase 7c: exposed so
    /// [`crate::RebuildDispatcher::execute_one_rebuild`] can hold the
    /// read lock across its cancel/membership re-check and
    /// [`Self::publish_and_retain`], matching the pattern in
    /// [`Self::get_or_load`] (Codex Task 6 Phase 6b iter-2 MAJOR — the
    /// publish critical section MUST exclude concurrent
    /// [`Self::execute_eviction`] on the same key to avoid
    /// orphaned-publish / admission-drift).
    ///
    /// Callers MUST respect lock order §J.4: acquire `workspaces`
    /// BEFORE `admission`. The returned guard is released when the
    /// caller drops it.
    ///
    /// `pub(crate)` (iter-2 design Codex MAJOR): the accessor is only
    /// used within the daemon crate; exposing it publicly would leak
    /// lock mechanics and broaden the blast radius for future callers
    /// that might violate the §J.4 discipline.
    pub(crate) fn workspaces_read(
        &self,
    ) -> parking_lot::RwLockReadGuard<'_, HashMap<WorkspaceKey, Arc<LoadedWorkspace>>> {
        self.workspaces.read()
    }

    /// Classify a workspace's readiness to serve a query.
    ///
    /// Task 7 Phase 7c. Used by the Task 8 IPC router on every query
    /// dispatch. Pure-read: no mutations, no `.await` (sync).
    ///
    /// # Returns
    ///
    /// | Workspace state | Map present | Result |
    /// |-----------------|-------------|--------|
    /// | `Loaded` or `Rebuilding` | yes | `Ok(ServeVerdict::Fresh { graph, state })` |
    /// | `Failed`, holding no generation (the placeholder) | yes | `Ok(ServeVerdict::FailedWithoutGraph { had_been_loaded, last_error })` |
    /// | `Failed`, age < cap (or cap == 0) | yes | `Ok(ServeVerdict::Stale { graph, age_hours, last_good_at, last_error })` |
    /// | `Failed`, age >= cap | yes | `Err(WorkspaceStaleExpired { age_hours, cap_hours, last_good_at, last_error })` (→ JSON-RPC -32002) |
    /// | `Failed`, no prior good | yes | `Err(WorkspaceBuildFailed { reason })` (→ -32001) |
    /// | `Unloaded` or `Loading` | yes | `Ok(ServeVerdict::NotReady { state })` |
    /// | `Evicted` | yes (transient window) | `Err(WorkspaceEvicted)` (→ -32004) |
    /// | any | no | `Err(WorkspaceEvicted)` (→ -32004) |
    ///
    /// # Lock order
    ///
    /// Task 7 Phase 7c feat iter-1 Codex BLOCKER fix: takes
    /// `workspaces.read()` across the FULL snapshot — state, graph,
    /// `last_good`, and `last_error_text` are all captured inside the
    /// read critical section. Dropping the read lock before reading
    /// the graph would allow `execute_eviction` (which needs
    /// `workspaces.write()` for the full graph-swap + state-store +
    /// map-remove sequence) to interleave, surfacing the empty
    /// post-eviction placeholder graph as a `Fresh` verdict.
    ///
    /// Does not acquire `admission` or `rebuild_lane`; only
    /// `workspaces` + per-workspace field locks. §J.4 order preserved.
    ///
    /// # Errors
    ///
    /// Returns the variants listed in the table above.
    ///
    /// # Panics
    ///
    /// Panics only if [`classify_staleness`] returns
    /// [`StalenessVerdict::Stale`] while `last_good_at` is absent.
    /// That would violate the staleness classifier invariant: stale
    /// verdicts are emitted only for workspaces with a prior successful
    /// publish timestamp.
    pub fn classify_for_serve(
        &self,
        key: &WorkspaceKey,
        now: std::time::SystemTime,
    ) -> Result<ServeVerdict, DaemonError> {
        // Task 7 Phase 7c — feat iter-0 Codex BLOCKER fix: the
        // previous iter-0 implementation cloned the workspace Arc and
        // dropped `workspaces.read()` BEFORE reading state and graph.
        // `execute_eviction` (see Self::execute_eviction at line 494)
        // holds `workspaces.write()` across:
        //   - ws.published.swap(PublishedGraph::placeholder())
        //   - admission accounting transfer
        //   - ws.rebuild_cancelled.store(true)
        //   - ws.store_state(WorkspaceState::Evicted)
        //   - workspaces.remove(key)
        //
        // Without the read-lock hold extending across graph capture,
        // a classifier could observe `state == Loaded` but fetch the
        // post-eviction empty placeholder graph, returning
        // `Fresh { graph: empty }` — a correctness bug.
        //
        // Iter-1: snapshot every field under the read lock. The
        // returned `Arc<CodeGraph>` is a strong reference independent
        // of the lock lifetime; dropping the lock after capture is
        // safe for the caller.
        //
        // `last_error` is captured as a display-string (the error
        // type is not Clone; see `clone_err` rationale) because
        // `NoPriorGood` returns a `WorkspaceBuildFailed { reason }`
        // that embeds the stringified prior error.
        let snapshot = {
            let workspaces = self.workspaces.read();
            let Some(ws) = Self::resolve_locked(&workspaces, key).cloned() else {
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            };
            let state = ws.load_state();
            // One load of the published slot (design D14): the graph and
            // the record are the same generation by construction, not by
            // the order of two stores in `publish_and_retain`.
            let published = ws.published();
            let last_good = *ws.last_good_at.read();
            let last_error = ws.last_error.read().as_ref().map(clone_err);
            (state, published, last_good, last_error)
            // workspaces.read() dropped here: the (state, published)
            // pair is a coherent snapshot taken atomically w.r.t.
            // execute_eviction's workspaces.write().
        };
        let (state, published, last_good, last_error) = snapshot;
        let last_error_text = last_error.as_ref().map(std::string::ToString::to_string);
        let graph = Arc::clone(&published.graph);
        let roster = published.roster.clone();

        // Every publish path publishes the record with the graph, so a
        // servable state without one is a bug in a publish path, not a
        // condition to serve through. Refuse rather than guess a roster.
        let roster_for_serve = |what: &str| -> Result<Arc<RosterRecord>, DaemonError> {
            roster.clone().ok_or_else(|| {
                DaemonError::Internal(anyhow::anyhow!(
                    "workspace {} is {what} but carries no roster record; publish paths must \
                     publish the record with the graph",
                    key.source_root.display()
                ))
            })
        };

        match state {
            WorkspaceState::Loaded | WorkspaceState::Rebuilding => {
                let roster = roster_for_serve("servable")?;
                Ok(ServeVerdict::Fresh {
                    graph,
                    state,
                    roster,
                })
            }
            // Nothing was published since the slot became `Failed` over
            // the placeholder: neither a stale serve (no graph, no record)
            // nor a build failure to flatten to its text.
            WorkspaceState::Failed if roster.is_none() => Ok(ServeVerdict::FailedWithoutGraph {
                had_been_loaded: last_good.is_some(),
                last_error: last_error.map(Arc::new),
            }),
            WorkspaceState::Failed => {
                let cap = self.config.stale_serve_max_age_hours;
                match classify_staleness(last_good, cap, now) {
                    StalenessVerdict::NoPriorGood => Err(DaemonError::WorkspaceBuildFailed {
                        root: key.source_root.clone(),
                        reason: last_error_text
                            .unwrap_or_else(|| "no prior successful build".into()),
                    }),
                    StalenessVerdict::Stale { age_hours } => {
                        let roster = roster_for_serve("stale-servable")?;
                        Ok(ServeVerdict::Stale {
                            graph,
                            age_hours,
                            // Invariant: `classify_staleness` only returns
                            // `Stale` when `last_good.is_some()` (see
                            // `workspace/staleness.rs:54-73`).
                            last_good_at: last_good
                                .expect("Stale verdict only emitted when last_good.is_some()"),
                            last_error: last_error_text,
                            roster,
                        })
                    }
                    StalenessVerdict::Expired { age_hours } => {
                        Err(DaemonError::WorkspaceStaleExpired {
                            root: key.source_root.clone(),
                            age_hours,
                            cap_hours: cap,
                            last_good_at: last_good,
                            last_error: last_error_text,
                        })
                    }
                }
            }
            WorkspaceState::Unloaded | WorkspaceState::Loading => {
                Ok(ServeVerdict::NotReady { state })
            }
            // Transient window between store_state(Evicted) and
            // workspaces.remove; same semantics as map-absent.
            WorkspaceState::Evicted => Err(DaemonError::WorkspaceEvicted {
                root: key.source_root.clone(),
            }),
        }
    }

    /// Consume a [`RebuildReservation`] plus a freshly-built
    /// [`CodeGraph`] and atomically publish it to the workspace.
    ///
    /// Implements Amendment 2 §G.2:
    ///
    /// - Captures the prior `Arc<CodeGraph>` and `memory_bytes` into
    ///   a [`RollbackGuard`] **before** any swap — so a panic at any
    ///   point before the admission update reverts cleanly.
    /// - Swaps the `ArcSwap<CodeGraph>` to the new graph.
    /// - Swaps the per-workspace `memory_bytes` to the new size.
    /// - Under the admission mutex: moves `bytes_delta` from
    ///   `reserved_bytes` into `loaded_bytes`, inserts a
    ///   [`RetainedEntry`] holding the old `Arc` until the retention
    ///   reaper frees it.
    /// - Disarms the [`RollbackGuard`] on success.
    ///
    /// Sync `fn`. There is no `.await` between the first swap and the
    /// admission insert — tokio task cancellation can only interrupt
    /// at `.await` points, so this sequence is atomic with respect
    /// to cancellation per §G.2.
    ///
    /// Returns the minted [`OldGraphToken`] for tracing / integration
    /// tests, together with the [`PublishedGraph`] generation this call
    /// swapped in (the graph and its roster record as one value, surface
    /// parity W1 round 4, design D20): every publisher hands that pair to
    /// its reader, so no reader pairs this graph with a record a later
    /// publish swapped in. Per Codex Task 6 Phase 6c iter-2 MAJOR the
    /// post-publish `SqrydHook` dispatch is NOT performed here —
    /// firing `on_publish` under the `workspaces.read()` guard
    /// `get_or_load` holds across this call would nest
    /// `self.hook.read()` inside `workspaces`, giving hook impls a
    /// re-entrancy deadlock hole if they call back into manager
    /// methods needing `workspaces.write()`. The caller is
    /// responsible for dispatching the hook after dropping every
    /// outer workspaces-lock holder.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WorkspaceOversize`] when the fully-built
    /// graph exceeds the daemon admission limit after replacing the
    /// workspace's prior contribution. The reservation is still owned
    /// by this function on that path, so its RAII drop refunds the
    /// reserved bytes before the error reaches the caller.
    pub fn publish_and_retain(
        self: &Arc<Self>,
        reservation: RebuildReservation,
        workspace: &LoadedWorkspace,
        built: BuiltGraph,
    ) -> Result<(OldGraphToken, Arc<PublishedGraph>), DaemonError> {
        let BuiltGraph {
            graph: new_graph,
            roster: new_roster,
        } = built;
        // Compute the new graph's heap bytes before handing it to the
        // ArcSwap — once published, a concurrent reader holds it
        // alive, and measuring after publish race-races with the
        // admission update.
        let new_bytes_usize = new_graph.heap_bytes();
        // `usize as u64` is a no-op on 64-bit and a widen on 32-bit.
        let new_bytes = new_bytes_usize as u64;

        // Post-build oversize gate (`G_daemon_control_plane.md` §1.4
        // + `00_contracts.md` §3.CC-3 admission boundary). Reject
        // BEFORE any visibility mutation so a ground-truth-too-big
        // workspace can never enter the serve path. The reservation
        // drops on early return — bytes are refunded via RAII and no
        // `OldGraphToken` is allocated.
        //
        // Subtract the prior workspace bytes from the projected
        // total because we will REPLACE this workspace's contribution
        // (the swap below subtracts `prev_memory_bytes` from
        // `loaded_bytes` and adds `new_bytes`); only the delta from
        // the prior contribution counts against the cap, while
        // every other workspace's loaded contribution and any
        // retained-old bytes still count.
        let limit = self.memory_limit_bytes();
        let prior_workspace_bytes = workspace
            .memory_bytes
            .load(std::sync::atomic::Ordering::Acquire) as u64;
        let projected = {
            let state = self.admission.lock();
            state
                .loaded_bytes
                .saturating_sub(prior_workspace_bytes)
                .saturating_add(state.retained_total_bytes())
                .saturating_add(new_bytes)
        };
        if projected > limit {
            return Err(DaemonError::WorkspaceOversize {
                root: workspace.key.source_root.clone(),
                measured_bytes: new_bytes,
                limit_bytes: limit,
                current_loaded_bytes: projected.saturating_sub(new_bytes),
            });
        }

        // Take the reservation by value so this function owns it and
        // the Drop impl fires on any unwind path. `released` stays
        // `false` until *after* the admission commit succeeds, so a
        // panic before or during the admission mutex section refunds
        // `reserved_bytes` back to the pool (Codex Task 6 Phase 6a
        // iter-1 MAJOR: the previous ordering disarmed before the
        // commit and could leak reserved bytes on unwind).
        let mut reservation = reservation;
        let reservation_bytes = reservation.bytes;

        let new_arc = Arc::new(new_graph);
        // The generation to publish: graph and record as one value
        // (design D14), built before the non-recoverable zone so the
        // allocation cannot panic inside it.
        let new_published = Arc::new(PublishedGraph::new(new_arc, Some(new_roster)));
        // Clone the generation BEFORE the swap so the caller receives the
        // exact pair this call publishes (design D20). Re-reading
        // `workspace.published()` after the swap would hand back whatever
        // a publisher that ran between the swap and the load swapped in,
        // and a reader would then describe one generation's graph with
        // another's record.
        let published = Arc::clone(&new_published);
        let token = OldGraphToken::new();

        // --- RollbackGuard setup --------------------------------
        let prior_published_for_rollback = workspace.published();
        let prior_bytes = workspace
            .memory_bytes
            .load(std::sync::atomic::Ordering::Acquire);

        let mut rollback = RollbackGuard {
            ws: workspace,
            prior_published: Some(prior_published_for_rollback),
            prior_bytes,
            armed: true,
        };

        // --- Non-recoverable zone (no .await; no fallible ops) ---
        //
        // If any code between this point and `reservation.released = true`
        // panics, the following Drop order runs on unwind:
        //   1. `rollback` Drop reverts `workspace.published` and
        //      `workspace.memory_bytes` to the pre-swap values
        //      (because `armed == true`).
        //   2. `reservation` Drop reacquires the admission mutex and
        //      refunds `reservation_bytes` back to `reserved_bytes`
        //      (because `released == false`).
        // This is the §G.5 invariant-preserving rollback described in
        // the plan; the reservation refund was missing before the
        // iter-1 fix.
        // Surface parity W1 round 3 (D14): the graph and its record are
        // one generation and this is the one swap that publishes it, so a
        // reader can never observe the old graph beside the new record or
        // the reverse. `RollbackGuard` restores the prior generation on
        // unwind. The old generation is dropped here after its graph
        // `Arc` is taken for the retained entry, so the reaper's
        // `strong_count == 1` test sees only the retained holder once
        // every reader lets go.
        let old_published = workspace.published.swap(new_published);
        let old_arc = Arc::clone(&old_published.graph);
        drop(old_published);
        let prev_memory_bytes = workspace.update_memory(new_bytes_usize);
        debug_assert_eq!(
            prev_memory_bytes, prior_bytes,
            "RollbackGuard prior_bytes must match update_memory's returned prior",
        );

        // --- Admission commit (mutex-only; no other locks) -------
        //
        // The critical section is ordered so the only *fallible* op —
        // `HashMap::insert`, which can allocate on grow and therefore
        // panic — runs FIRST, before any admission counter is mutated
        // and before the reservation is disarmed. Everything that
        // follows (`saturating_*` arithmetic + `reservation.released
        // = true`) is guaranteed infallible, so once we reach those
        // lines the critical section cannot unwind mid-way and leave
        // admission state inconsistent.
        //
        // Codex Task 6 Phase 6a iter-2 MAJOR: the iter-1 ordering
        // disarmed the reservation before `retained_old.insert`
        // completed. A panic from the insert would leave
        // `reserved_bytes` drained and `loaded_bytes` updated while
        // no retained entry existed; rollback reverts ws.published +
        // ws.memory_bytes but cannot refund the reservation
        // (released=true). The fix moves insert to the front of the
        // section so any unwind preserves the §G.5 invariant.
        //
        // Pre-build the `RetainedEntry` outside the lock so only the
        // `HashMap::insert` itself can allocate; the struct
        // construction is a field-by-field move.
        let retained_entry = RetainedEntry {
            bytes: prev_memory_bytes as u64,
            graph: old_arc,
            published_at: Instant::now(),
            warned_past_timeout: false,
        };

        let mut state = self.admission.lock();

        // Step 1 — fallible. `HashMap::insert` may reallocate; if it
        // panics the state is left unchanged (hashbrown's insert is
        // exception-safe: a failed grow leaves the map in its prior
        // capacity and does not insert the new entry). Unwind drops
        // `state` (releasing the mutex), then `rollback` reverts
        // ws.published + ws.memory_bytes, then the `reservation`
        // (released=false) refunds `reservation_bytes` from
        // `reserved_bytes`. `loaded_bytes` is not mutated because
        // the lines below never run.
        state.retained_old.insert(token, retained_entry);

        // Step 2 — infallible arithmetic (saturating ops on u64).
        // Move reservation → loaded. The prior workspace bytes are
        // already counted in `loaded_bytes` (they were added the
        // last time this workspace published). Swap by subtracting
        // the old and adding the new — keeps the §G.5 invariant
        // monotonic w.r.t. the commit.
        state.reserved_bytes = state.reserved_bytes.saturating_sub(reservation_bytes);
        state.loaded_bytes = state
            .loaded_bytes
            .saturating_sub(prev_memory_bytes as u64)
            .saturating_add(new_bytes);

        // Step 3 — infallible disarm. The admission commit is
        // complete; the reservation's Drop is now a no-op so it
        // does not double-refund.
        reservation.released = true;
        self.bump_high_water(&state);
        drop(state);

        rollback.armed = false; // disarm on success

        // Observation point (surface parity W1 round 5, design D26): the
        // admission commit is complete, the admission mutex is released,
        // and the generation this call swapped in is about to be returned.
        // A test plant here may publish again for the same workspace; the
        // return below is still `published`, the clone taken before the
        // swap, never a re-read of the slot (T48, battery row K36). A
        // plant that panics here leaves the function in its post-commit
        // state, which is the state the caller would have observed anyway.
        // A release build compiles this to nothing.
        self.run_observation_plant(ObservationPhase::PublishCommitted);

        // NOTE: `SqrydHook::on_publish` is NOT dispatched here.
        // `get_or_load` holds `workspaces.read()` across this call
        // (to make the re-check + publish critical section atomic
        // with respect to eviction, see that function's Step 6+7
        // comment block). Firing the hook here would acquire
        // `self.hook.read()` nested under `workspaces`, giving a
        // hook impl that calls back into manager methods needing
        // `workspaces.write()` (e.g. `unload`) a guaranteed
        // deadlock. The caller dispatches the hook after dropping
        // `workspaces_guard` — see `get_or_load` post-publish.
        //
        // `NoOpHook` remains the default; Task 9's daemon binary
        // installs the production `QueryDbHook` that wraps
        // `sqry_db::persistence::save_derived` with a timeout.
        Ok((token, published))
    }

    /// Release the reaper handle on Drop. Safe to call from any
    /// context — abort is a best-effort signal.
    fn shutdown_reaper(&self) {
        if let Some(handle) = self.reaper.lock().take() {
            handle.abort();
        }
    }

    // ---------------------------------------------------------------------
    // SGA04 — Bounded read-only rehydrate after eviction
    // ---------------------------------------------------------------------

    /// Read-only rehydrate of an existing persisted graph for `key`.
    ///
    /// Implements the daemon side of the bounded one-shot reload rule
    /// described in `docs/development/shared-graph-acquisition/02_DESIGN.md`.
    /// Used by [`crate::workspace::acquirer::DaemonGraphProvider`] when
    /// a [`AcquisitionOperation::ReadOnlyQuery`] finds the workspace not
    /// resident: [`Self::classify_for_serve`] answers
    /// [`DaemonError::WorkspaceEvicted`] (an eviction tombstone, or a key
    /// with no entry), or [`ServeVerdict::FailedWithoutGraph`] for a slot a
    /// failed load left with nothing to serve, when a reload may clear it.
    /// An `Unloaded` workspace (`daemon/reset`) is `NotReady` and is not
    /// reloaded.
    ///
    /// Behaviour contract:
    ///
    /// 1. Drives the same lifecycle CAS gate as
    ///    [`Self::get_or_load`] — only one caller can rehydrate per
    ///    workspace at a time.
    /// 2. Prepares the load ([`WorkspaceBuilder::prepare_load_persisted`]):
    ///    the checks the load makes before reading the snapshot (an index
    ///    and a snapshot exist, the manifest is readable and names no id
    ///    this binary did not compile) run first, so a refused reload evicts
    ///    no sibling workspace; then reserves admission headroom via
    ///    [`Self::reserve_rebuild`].
    /// 3. Runs the prepared load, which reads
    ///    `<source_root>/.sqry/graph/snapshot.sqry`. Never calls
    ///    `WorkspaceBuilder::build`, never mutates `.sqry/graph/*`,
    ///    `.sqry/analysis/*`, or `derived.sqry`, and never invokes the
    ///    post-publish hook (the snapshot is bit-identical with what
    ///    the hook would produce — no fresh derived cache to warm).
    /// 4. Publishes through [`Self::publish_and_retain`] under the
    ///    standard `workspaces.read()` re-check + cancellation gate
    ///    so eviction races are caught the same way as `get_or_load`.
    ///
    /// Returns the published generation (graph and roster record as one
    /// value, design D20): the one this reload published, or the one a
    /// concurrent loader already published for the key. The caller takes
    /// both halves from it and never re-reads the slot.
    ///
    /// `pub(crate)` because the entrypoint is internal to the daemon
    /// crate; SGA04's public surface is the
    /// [`crate::workspace::acquirer::DaemonGraphProvider`] adapter.
    ///
    /// # Errors
    ///
    /// Returns the same set of [`DaemonError`] variants as
    /// [`Self::get_or_load`]. The caller maps these into the shared
    /// [`sqry_core::graph::acquisition::GraphAcquisitionError`]
    /// taxonomy (typically [`GraphAcquisitionError::Evicted`] when the
    /// reload is the daemon-provider's bounded retry).
    ///
    /// [`AcquisitionOperation::ReadOnlyQuery`]: sqry_core::graph::acquisition::AcquisitionOperation::ReadOnlyQuery
    /// [`GraphAcquisitionError::Evicted`]: sqry_core::graph::acquisition::GraphAcquisitionError::Evicted
    pub(crate) fn reload_from_disk_read_only(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        builder: &dyn WorkspaceBuilder,
        working_set_estimate: u64,
    ) -> Result<Arc<PublishedGraph>, DaemonError> {
        let (ws, registered_key, entry) = match self.prepare_load_gate(key)? {
            LoadGate::Loaded(published) => return Ok(published),
            LoadGate::Acquired {
                workspace,
                registered_key,
                entry,
            } => (workspace, registered_key, entry),
        };

        // --- Step 3: arm LoadingGuard for panic / early-return ----
        let mut loading = LoadingGuard {
            ws: &ws,
            key: &registered_key,
            armed: true,
        };

        // --- Step 3b: refuse before reserving ----------------------
        //
        // As in `get_or_load_published`: the checks the load would make
        // before reading the snapshot (an index and a snapshot exist, the
        // manifest is readable and names no uncompiled id) run before the
        // reservation, whose LRU phase can evict sibling workspaces.
        let prepared = match builder.prepare_load_persisted(&key.source_root) {
            Ok(prepared) => prepared,
            Err(refusal) => {
                Self::fail_load_before_reservation(&ws, &mut loading, &key.source_root, &refusal);
                return Err(refusal);
            }
        };

        // --- Step 4: reserve admission headroom -------------------
        //
        // As in `load_published` (S8, round 7 audit): a budget that cannot
        // admit the reload puts the slot back as the gate found it, so the
        // next query retries the reload and is refused the same way until
        // the budget admits it, instead of reading a `Failed` slot that no
        // query reloads again.
        let reservation = match self.reserve_rebuild(&registered_key, working_set_estimate) {
            Ok(reservation) => reservation,
            Err(err) => {
                if is_request_refusal(&err) {
                    self.abandon_load(&ws, &mut loading, entry);
                }
                return Err(err);
            }
        };

        // --- Step 5: load_persisted (read-only, no build pipeline)
        let built = match prepared() {
            Ok(g) => g,
            Err(err) => {
                drop(reservation);
                ws.record_failure(clone_err(&err));
                loading.armed = false;
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
                {
                    Self::log_lost_load_transition(&key.source_root, observed);
                }
                return Err(err);
            }
        };

        // --- Step 6+7: atomic re-check + publish ------------------
        let workspaces_guard = self.workspaces.read();
        if ws.rebuild_cancelled.load(Ordering::Acquire) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-reload".to_string(),
            });
            loading.armed = false;
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-reload".to_string(),
            });
        }
        if !Self::registers(&workspaces_guard, &ws) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-reload".to_string(),
            });
            loading.armed = false;
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
            {
                Self::log_lost_load_transition(&key.source_root, observed);
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-reload".to_string(),
            });
        }

        // `G_daemon_control_plane.md` §3.5 + §3.6 — read-only
        // reload exemption proof: in steady-state operation this
        // path cannot observe `WorkspaceOversize` because the
        // snapshot-on-disk was bounded by a prior successful
        // publish + the deserialization size cap. Defensive match
        // arm preserved so a contract violation surfaces as the
        // typed error rather than silently masquerading as a
        // success.
        let (_token, published) = match self.publish_and_retain(reservation, &ws, built) {
            Ok((token, published)) => (token, published),
            Err(e) => {
                drop(workspaces_guard);
                ws.record_failure(clone_err(&e));
                loading.armed = false;
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
                {
                    Self::log_lost_load_transition(&key.source_root, observed);
                }
                return Err(e);
            }
        };
        ws.record_success(std::time::SystemTime::now());
        ws.store_state(WorkspaceState::Loaded);
        ws.touch();
        loading.armed = false;
        drop(workspaces_guard);

        // No post-publish `SqrydHook::on_publish` dispatch on the
        // read-only reload path — the snapshot we just loaded is the
        // SAME bytes the hook would have re-serialised, so the derived
        // cache must already match it. Firing the hook here would be
        // redundant work (and on the spec contract: this path "must
        // not write any artifact").

        Ok(published)
    }

    /// Test-only: synchronously evict `key` regardless of memory
    /// pressure.
    ///
    /// Used by SGA04 / SGA07 parity tests to drive a workspace from
    /// `Loaded` into `Evicted` deterministically (the production
    /// eviction paths are budget-driven and time-sensitive). Behaves
    /// exactly like the LRU eviction path: graph is swapped out, bytes
    /// move from `loaded_bytes` into `retained_old`, the entry stays
    /// in the manager map as a tombstone (matching STEP_6 partial
    /// eviction reporting).
    ///
    /// Returns `true` if the key was present and evicted, `false`
    /// otherwise.
    ///
    /// # Visibility
    ///
    /// Marked `#[doc(hidden)]` and named with the `_for_test` suffix
    /// to advertise "test affordance only" (matching
    /// [`Self::insert_workspace_in_state_for_test`] /
    /// [`crate::TestGate`] / [`crate::TestCapture`]). It is **not**
    /// re-exported through `sqry-daemon`'s public prelude
    /// (`pub use workspace::{...}` in `lib.rs` does not list it), so
    /// release / IPC / MCP / HTTP surfaces cannot reach it. Production
    /// code MUST NOT call this; the canonical eviction entrypoints
    /// remain [`Self::evict_lru`] and [`Self::unload`].
    ///
    /// # Visibility (SGA04 Gate-A blocker fix)
    ///
    /// Even though `lib.rs` does not re-export this method, it was
    /// previously declared `pub fn` on a `pub struct WorkspaceManager`,
    /// which means callers could reach it through any path that already
    /// holds a `&WorkspaceManager` — including any public re-export of
    /// the type. The Codex Gate-A review flagged this as a leak of a
    /// test-only hook into the release surface.
    ///
    /// The fix is a compile-time gate: the entire item is now
    /// `#[cfg(any(test, feature = "test-hooks"))]`, so default release
    /// builds (`cargo build -p sqry-daemon`) cannot see the symbol at
    /// all. SGA07 parity tests that live in the integration-test crate
    /// (`sqry-daemon/tests/`) opt in via
    /// `cargo test -p sqry-daemon --features test-hooks --tests`, while
    /// in-crate `#[cfg(test)] mod tests` blocks reach it through
    /// `cfg(test)`.
    ///
    /// Two siblings under the same gate (surface parity W1 round 5,
    /// design D24): [`Self::try_evict_for_test`], the non-blocking form
    /// that answers whether an eviction could run at the instant it is
    /// called, and [`Self::install_observation_plant_for_test`], which
    /// runs a closure at the named [`ObservationPhase`] points so a test
    /// can call either eviction from inside the manager's own
    /// observation.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn evict_for_test(&self, key: &WorkspaceKey) -> bool {
        let registered =
            Self::resolve_locked(&self.workspaces.read(), key).map(|ws| ws.key.clone());
        let Some(registered) = registered else {
            return false;
        };
        self.execute_eviction(&registered);
        true
    }

    /// Test-only: the deterministic form of "an eviction here". Takes
    /// `workspaces.try_write()`: [`TryEvictOutcome::WouldBlock`] when the
    /// lock is held (a reader observing under `workspaces.read()`, which
    /// is what design D24 requires at every `Loaded` return),
    /// [`TryEvictOutcome::Absent`] when no entry is keyed by `key`, and
    /// otherwise the tombstone eviction
    /// [`Self::evict_to_tombstone_locked`] performs under that guard,
    /// answering [`TryEvictOutcome::Evicted`]. Under the pre-round-5 gate
    /// arm no guard was held and a plant here evicted every time; under
    /// the repaired arm the plant's own thread holds the read guard and
    /// the answer is `WouldBlock` every time. Not compiled into release
    /// builds; not re-exported through `lib.rs`.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn try_evict_for_test(&self, key: &WorkspaceKey) -> TryEvictOutcome {
        let Some(mut workspaces) = self.workspaces.try_write() else {
            return TryEvictOutcome::WouldBlock;
        };
        if !workspaces.contains_key(key) {
            return TryEvictOutcome::Absent;
        }
        self.evict_to_tombstone_locked(&mut workspaces, key);
        TryEvictOutcome::Evicted
    }

    /// Test-only: install the closure [`Self::run_observation_plant`]
    /// calls at every [`ObservationPhase`]. Replaces any earlier plant.
    /// The closure runs with the plant's own mutex released, so it may
    /// call back into the manager (publish through `get_or_load`, attempt
    /// an eviction through [`Self::try_evict_for_test`]); whether a
    /// manager lock is held at the phase is the property under test, and
    /// the phase documentation on [`ObservationPhase`] states it.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn install_observation_plant_for_test(&self, plant: ObservationPlant) {
        self.observation_plant.install(plant);
    }

    /// Run the installed observation plant at `phase`. A release build
    /// compiles this to nothing ([`ObservationPlantSlot`] is zero-sized
    /// there); a test build clones the `Arc` out of the slot's mutex and
    /// calls it with the mutex released.
    fn run_observation_plant(&self, phase: ObservationPhase) {
        self.observation_plant.run(phase);
    }
}

impl Drop for WorkspaceManager {
    fn drop(&mut self) {
        self.shutdown_reaper();
    }
}

/// The points at which [`WorkspaceManager`] observes a workspace's state
/// and captures its published generation, named so a test plant can act at
/// one of them (surface parity W1 round 5, designs D24, D25 and D26). The
/// enum exists in every build because the manager's own code names the
/// phases; the plant that receives them exists only under
/// `cfg(any(test, feature = "test-hooks"))`.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservationPhase {
    /// `prepare_load_gate`: the first lookup found the key absent or not
    /// `Loaded`. No manager lock is held.
    GateFirstLookupMissed,
    /// `prepare_load_gate`: the compare-exchange into `Loading` lost (the
    /// slot is `Loading`, `Rebuilding` or `Loaded`). No manager lock is
    /// held.
    GateCasLost,
    /// `prepare_load_gate`, second `Loaded` return: the state was observed
    /// `Loaded` and the generation is about to be loaded. The gate's
    /// `workspaces.read()` guard is held.
    GateLoadedObserved,
    /// `resident_snapshot`: the state was observed and the generation is
    /// about to be loaded. The snapshot's `workspaces.read()` guard is
    /// held.
    SnapshotObserved,
    /// `publish_and_retain`: the admission commit is complete and the
    /// generation this call swapped in is about to be returned. Whatever
    /// guard the caller holds is still held; the admission mutex is
    /// released.
    PublishCommitted,
    /// `prepare_load_gate`: the `Loading` gate has been won and
    /// `honor_preexisting_cancel` has answered `Ok`, so the caller is
    /// about to receive [`LoadGate::Acquired`]. NO manager lock is held
    /// here, which is what makes it the seam an eviction can complete
    /// inside a load the gate has already been won for (surface parity
    /// W1 round 7, design D37; T60). Added at the end of the enum,
    /// which battery row S23 already declares is not a contract.
    GateAcquired,
}

/// A test plant: the closure [`WorkspaceManager::run_observation_plant`]
/// calls at each [`ObservationPhase`].
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub type ObservationPlant = Arc<dyn Fn(ObservationPhase) + Send + Sync>;

/// What [`WorkspaceManager::try_evict_for_test`] found.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryEvictOutcome {
    /// The write lock was taken and the entry was evicted to a tombstone.
    Evicted,
    /// `workspaces.try_write()` failed: a guard is held (the repaired
    /// gate's read guard, or any other holder at that instant).
    WouldBlock,
    /// The write lock was taken but no entry is keyed by the key.
    Absent,
}

/// The slot that holds the observation plant. Under
/// `cfg(any(test, feature = "test-hooks"))` it is a mutex around an
/// optional [`ObservationPlant`]; otherwise it is a zero-sized type whose
/// `run` is empty, so the release build carries no closure, no lock and
/// no call.
#[cfg(any(test, feature = "test-hooks"))]
struct ObservationPlantSlot {
    plant: Mutex<Option<ObservationPlant>>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl ObservationPlantSlot {
    fn new() -> Self {
        Self {
            plant: Mutex::new(None),
        }
    }

    fn install(&self, plant: ObservationPlant) {
        *self.plant.lock() = Some(plant);
    }

    fn run(&self, phase: ObservationPhase) {
        // Clone out under the lock, then release it before the call: a
        // plant may call back into the manager and reach a phase again.
        let plant = self.plant.lock().clone();
        if let Some(plant) = plant {
            plant(phase);
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl std::fmt::Debug for ObservationPlantSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationPlantSlot")
            .field("installed", &self.plant.lock().is_some())
            .finish()
    }
}

/// Release form of the observation-plant slot: zero-sized, nothing to run.
#[cfg(not(any(test, feature = "test-hooks")))]
#[derive(Debug)]
struct ObservationPlantSlot;

#[cfg(not(any(test, feature = "test-hooks")))]
impl ObservationPlantSlot {
    const fn new() -> Self {
        Self
    }

    #[inline]
    fn run(&self, phase: ObservationPhase) {
        let _ = phase;
    }
}

/// `STEP_11_4` — probe `<source_root>/.sqry/classpath/` for presence at
/// `daemon/workspaceStatus` time.
///
/// Status path: cheap (`fs::metadata`), never blocks on anything
/// heavier, and degrades silently to `false` on any error so a racy
/// classpath unlink or a permission denial cannot fail the status
/// response. The LSP-side `WorkspaceIndexStatus.warnings` channel
/// surfaces the underlying error detail when the daemon's workspace
/// builder hits the same probe and wants to record the failure.
fn probe_classpath_present(source_root: &std::path::Path) -> bool {
    let probe = source_root.join(".sqry").join("classpath");
    std::fs::metadata(&probe)
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// LoadingGuard (panic-safety for get_or_load)
// ---------------------------------------------------------------------------

/// Whether a load's error refuses the request itself rather than the index
/// on disk (B2 and S8, round 7 audit): a request the check refuses or a
/// recorded option the build cannot honour ([`DaemonError::InvalidArgument`],
/// [`DaemonError::RebuildMacroOptionsUnavailable`]), a roster that would
/// narrow the recorded selection
/// ([`DaemonError::RebuildWouldNarrowSelection`]), or a budget that cannot
/// admit the load ([`DaemonError::MemoryBudgetExceeded`]). Each is raised
/// before anything is built or written, and none says the index cannot be
/// read, so a load refused for one leaves no `Failed` slot behind
/// ([`WorkspaceManager::abandon_load`]).
fn is_request_refusal(err: &DaemonError) -> bool {
    matches!(
        err,
        DaemonError::InvalidArgument { .. }
            | DaemonError::RebuildMacroOptionsUnavailable { .. }
            | DaemonError::RebuildWouldNarrowSelection { .. }
            | DaemonError::MemoryBudgetExceeded { .. }
    )
}

/// RAII guard that transitions the workspace into
/// [`WorkspaceState::Failed`] on any non-success exit from
/// [`WorkspaceManager::get_or_load`] — including panics.
///
/// Codex Task 6 Phase 6b iter-1 MAJOR: without this guard, a panic
/// in `builder.build()` would leave the workspace stuck in
/// `Loading` with `last_error = None`, permanently blocking
/// re-load attempts and corrupting status output.
///
/// The guard is armed until the final `loaded.armed = false` on
/// the success path (after publish succeeds). Every other exit
/// path — `Err` from admission, `Err` from builder, panic from
/// builder, early returns on the cancellation/map-membership
/// re-check — fires `Drop` with `armed == true` and performs the
/// Failed-state transition.
pub(crate) struct LoadingGuard<'a> {
    pub(crate) ws: &'a LoadedWorkspace,
    pub(crate) key: &'a WorkspaceKey,
    pub(crate) armed: bool,
}

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Only overwrite `last_error` if it hasn't been populated
        // with a more specific diagnostic by the explicit `Err`
        // branches above — those set last_error before `armed =
        // false`, so seeing None here means we are in the panic
        // window or an early-return path that did not record one.
        {
            let mut slot = self.ws.last_error.write();
            if slot.is_none() {
                *slot = Some(DaemonError::WorkspaceBuildFailed {
                    root: self.key.source_root.clone(),
                    reason: "workspace load aborted unexpectedly".to_string(),
                });
            }
        }
        self.ws.retry_count.fetch_add(1, Ordering::AcqRel);
        // Surface parity W1 round 7 (design D37): `armed` is true
        // exactly while the `Loading` gate this task won is
        // unreleased, and that is two operations away from the store
        // it justified. An eviction that completed in between owns the
        // state, so the guard leaves it alone and the slot stays a
        // reloadable tombstone. T58 is the oracle.
        if let Err(observed) = self
            .ws
            .transition_state(WorkspaceState::Loading, WorkspaceState::Failed)
        {
            WorkspaceManager::log_lost_load_transition(&self.key.source_root, observed);
        }
    }
}

/// Clone a [`DaemonError`] for storage on [`LoadedWorkspace::last_error`]
/// or for propagation to `handle_changes` error returns in
/// [`crate::RebuildDispatcher::execute_one_rebuild`] (Task 7 Phase 7b1).
///
/// [`DaemonError`] is not `Clone` because some variants wrap
/// non-`Clone` types (notably [`std::io::Error`] and
/// [`anyhow::Error`]). `last_error` is a diagnostic surface only —
/// it is serialised as `e.to_string()` by the status endpoint — so
/// reducing the error to a textual form is the right trade-off here.
/// Compare a resident record against the manifest at `index_root` for the
/// `daemon/status` payload. Only a set divergence is reported; an
/// unreadable manifest or an uncompiled id is refused on the serve path
/// and would be misleading as a "divergence" here.
fn roster_status_for(
    index_root: &Path,
    record: &RosterRecord,
    load_roster: &sqry_core::plugin::PluginManager,
) -> RosterStatus {
    let diverges_from_manifest =
        match check_record_against_manifest(index_root, record, load_roster).verdict {
            ManifestVerdict::Diverges {
                missing_plugin_ids,
                extra_plugin_ids,
            } => Some(RosterDivergence {
                missing_plugin_ids,
                extra_plugin_ids,
            }),
            ManifestVerdict::NoManifest
            | ManifestVerdict::Unreadable { .. }
            | ManifestVerdict::UnknownIds { .. }
            | ManifestVerdict::Exact => None,
        };
    RosterStatus {
        active_plugin_ids: record.active_plugin_ids.clone(),
        high_cost_mode: record.high_cost_mode.clone(),
        source: record.source.as_str().to_string(),
        diverges_from_manifest,
    }
}

pub(crate) fn clone_err(err: &DaemonError) -> DaemonError {
    match err {
        DaemonError::WorkspaceBuildFailed { root, reason } => DaemonError::WorkspaceBuildFailed {
            root: root.clone(),
            reason: reason.clone(),
        },
        DaemonError::WorkspaceStaleExpired {
            root,
            age_hours,
            cap_hours,
            last_good_at,
            last_error,
        } => DaemonError::WorkspaceStaleExpired {
            root: root.clone(),
            age_hours: *age_hours,
            cap_hours: *cap_hours,
            // `SystemTime` is `Copy`; `Option<String>` needs `.clone()`.
            last_good_at: *last_good_at,
            last_error: last_error.clone(),
        },
        DaemonError::MemoryBudgetExceeded {
            limit_bytes,
            current_bytes,
            reserved_bytes,
            retained_bytes,
            requested_bytes,
        } => DaemonError::MemoryBudgetExceeded {
            limit_bytes: *limit_bytes,
            current_bytes: *current_bytes,
            reserved_bytes: *reserved_bytes,
            retained_bytes: *retained_bytes,
            requested_bytes: *requested_bytes,
        },
        DaemonError::WorkspaceEvicted { root } => {
            DaemonError::WorkspaceEvicted { root: root.clone() }
        }
        DaemonError::WorkspaceNotLoaded { root } => {
            DaemonError::WorkspaceNotLoaded { root: root.clone() }
        }
        DaemonError::RebuildWouldNarrowSelection {
            root,
            missing_plugin_ids,
            restore_command,
        } => DaemonError::RebuildWouldNarrowSelection {
            root: root.clone(),
            missing_plugin_ids: missing_plugin_ids.clone(),
            restore_command: restore_command.clone(),
        },
        DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } => DaemonError::RebuildMacroOptionsUnavailable {
            root: root.clone(),
            expand_cache_dir: expand_cache_dir.clone(),
            origin: *origin,
        },
        DaemonError::WorkspaceManifestUnreadable {
            root,
            manifest_path,
            reason,
        } => DaemonError::WorkspaceManifestUnreadable {
            root: root.clone(),
            manifest_path: manifest_path.clone(),
            reason: reason.clone(),
        },
        DaemonError::WorkspaceNotIndexed {
            root,
            missing_path,
            repair_command,
        } => DaemonError::WorkspaceNotIndexed {
            root: root.clone(),
            missing_path: missing_path.clone(),
            repair_command: repair_command.clone(),
        },
        DaemonError::WorkspaceSnapshotUnreadable {
            root,
            snapshot_path,
            reason,
        } => DaemonError::WorkspaceSnapshotUnreadable {
            root: root.clone(),
            snapshot_path: snapshot_path.clone(),
            reason: reason.clone(),
        },
        DaemonError::WorkspaceReloadFailed {
            root,
            reload_failure,
        } => DaemonError::WorkspaceReloadFailed {
            root: root.clone(),
            reload_failure: reload_failure.clone(),
        },
        // SGA04 Gate-A major #5 — round-trip the path-policy variant
        // distinctly. Collapsing it into `WorkspaceBuildFailed` would
        // re-introduce the exact bug Codex flagged.
        DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
            DaemonError::WorkspaceIncompatibleGraph {
                root: root.clone(),
                reason: reason.clone(),
            }
        }
        // Task 8 Phase 8c U5 — tool-dispatch variants surfaced by
        // `tool_core::classify_and_execute` (Phase 8c U6). Each
        // variant must round-trip cleanly so `classify_for_serve`
        // reproduces the original typed error on every read path —
        // collapsing any of these into `WorkspaceBuildFailed` would
        // break the wire-contract codes registered in
        // [`crate::lib`] / the design doc §O.
        DaemonError::ToolTimeout {
            root,
            secs,
            deadline_ms,
        } => DaemonError::ToolTimeout {
            root: root.clone(),
            secs: *secs,
            deadline_ms: *deadline_ms,
        },
        DaemonError::RebuildOutcomeTimeout {
            root,
            secs,
            deadline_ms,
        } => DaemonError::RebuildOutcomeTimeout {
            root: root.clone(),
            secs: *secs,
            deadline_ms: *deadline_ms,
        },
        DaemonError::InvalidArgument { reason } => DaemonError::InvalidArgument {
            reason: reason.clone(),
        },
        // Cluster-C iter-3: RpcError implements Clone, so this is a
        // direct deep copy.
        DaemonError::RpcErrorPreserved(rpc) => DaemonError::RpcErrorPreserved(rpc.clone()),
        DaemonError::Internal(err) => {
            // `anyhow::Error` is not `Clone`; re-create it from its
            // full-chain `Display` form (`{:#}`) so every layer of
            // the causal chain survives the round-trip. Callers only
            // read this via `to_string()` on the status endpoint, so
            // losing the typed causes (if any) is acceptable.
            DaemonError::Internal(anyhow::anyhow!("{err:#}"))
        }
        other => clone_lifecycle_or_storage_err(other),
    }
}

fn clone_lifecycle_or_storage_err(err: &DaemonError) -> DaemonError {
    if let Some(cloned) = clone_lifecycle_err(err) {
        return cloned;
    }
    if let Some(cloned) = clone_storage_or_revision_err(err) {
        return cloned;
    }
    match err {
        DaemonError::WorkspaceBuildFailed { .. }
        | DaemonError::WorkspaceStaleExpired { .. }
        | DaemonError::MemoryBudgetExceeded { .. }
        | DaemonError::WorkspaceEvicted { .. }
        | DaemonError::WorkspaceNotLoaded { .. }
        | DaemonError::WorkspaceIncompatibleGraph { .. }
        | DaemonError::ToolTimeout { .. }
        | DaemonError::InvalidArgument { .. }
        | DaemonError::RpcErrorPreserved(_)
        | DaemonError::Internal(_) => {
            unreachable!("workspace errors handled by clone_err")
        }
        _ => unreachable!("lifecycle/storage errors handled above"),
    }
}

fn clone_lifecycle_err(err: &DaemonError) -> Option<DaemonError> {
    match err {
        DaemonError::AlreadyRunning { socket, lock, .. } => DaemonError::WorkspaceBuildFailed {
            root: Path::new("<unknown>").to_path_buf(),
            reason: format!(
                "daemon already running on socket {} (lock: {})",
                socket.display(),
                lock.display()
            ),
        },
        DaemonError::AutoStartTimeout {
            timeout_secs,
            socket,
        } => DaemonError::WorkspaceBuildFailed {
            root: Path::new("<unknown>").to_path_buf(),
            reason: format!(
                "daemon did not become ready within {timeout_secs}s on socket {}",
                socket.display()
            ),
        },
        DaemonError::SignalSetup { source } => DaemonError::WorkspaceBuildFailed {
            root: Path::new("<unknown>").to_path_buf(),
            reason: format!("failed to install signal handlers: {source}"),
        },
        other @ (DaemonError::Config { .. } | DaemonError::Io(_)) => {
            DaemonError::WorkspaceBuildFailed {
                root: Path::new("<unknown>").to_path_buf(),
                reason: other.to_string(),
            }
        }
        _ => return None,
    }
    .into()
}

fn clone_storage_or_revision_err(err: &DaemonError) -> Option<DaemonError> {
    match err {
        DaemonError::WorkspaceOversize {
            root,
            measured_bytes,
            limit_bytes,
            current_loaded_bytes,
        } => DaemonError::WorkspaceOversize {
            root: root.clone(),
            measured_bytes: *measured_bytes,
            limit_bytes: *limit_bytes,
            current_loaded_bytes: *current_loaded_bytes,
        },
        DaemonError::WorkspacePinned { root } => {
            DaemonError::WorkspacePinned { root: root.clone() }
        }
        DaemonError::ResetWhileLoading { root } => {
            DaemonError::ResetWhileLoading { root: root.clone() }
        }
        DaemonError::ResetCancellationDispatched {
            root,
            retry_after_ms,
        } => DaemonError::ResetCancellationDispatched {
            root: root.clone(),
            retry_after_ms: *retry_after_ms,
        },
        DaemonError::SocketSetup { path, reason } => DaemonError::SocketSetup {
            path: path.clone(),
            reason: reason.clone(),
        },
        DaemonError::QueryTooBroad { reason, details } => DaemonError::QueryTooBroad {
            reason: reason.clone(),
            details: details.clone(),
        },
        DaemonError::RevisionSelectorAmbiguous { selector, matches } => {
            DaemonError::RevisionSelectorAmbiguous {
                selector: selector.clone(),
                matches: matches.clone(),
            }
        }
        DaemonError::RevisionObjectMissing { object, path } => DaemonError::RevisionObjectMissing {
            object: object.clone(),
            path: path.clone(),
        },
        DaemonError::RevisionSourceUnavailable { reason, path } => {
            DaemonError::RevisionSourceUnavailable {
                reason: reason.clone(),
                path: path.clone(),
            }
        }
        DaemonError::CheckoutFilterUnsupported { filter, path } => {
            DaemonError::CheckoutFilterUnsupported {
                filter: filter.clone(),
                path: path.clone(),
            }
        }
        DaemonError::SubmoduleUnavailable { path, gitlink_oid } => {
            DaemonError::SubmoduleUnavailable {
                path: path.clone(),
                gitlink_oid: gitlink_oid.clone(),
            }
        }
        DaemonError::DirtySnapshotChanged { root } => {
            DaemonError::DirtySnapshotChanged { root: root.clone() }
        }
        DaemonError::ArtifactKeyMismatch {
            artifact_id,
            reason,
        } => DaemonError::ArtifactKeyMismatch {
            artifact_id: artifact_id.clone(),
            reason: reason.clone(),
        },
        DaemonError::ManagedWorktreeInUse { worktree, reason } => {
            DaemonError::ManagedWorktreeInUse {
                worktree: worktree.clone(),
                reason: reason.clone(),
            }
        }
        DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes,
            requested_bytes,
            current_bytes,
        } => DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes: *limit_bytes,
            requested_bytes: *requested_bytes,
            current_bytes: *current_bytes,
        },
        DaemonError::RevisionQueryRequiresExplicitSelector { reason } => {
            DaemonError::RevisionQueryRequiresExplicitSelector {
                reason: reason.clone(),
            }
        }
        _ => return None,
    }
    .into()
}

// ---------------------------------------------------------------------------
// RebuildReservation (RAII)
// ---------------------------------------------------------------------------

/// RAII guard representing an in-flight rebuild's admission headroom.
///
/// - On the success path, the guard is consumed by
///   [`WorkspaceManager::publish_and_retain`], which sets
///   `released = true` before draining `bytes` from `reserved_bytes`.
/// - On any other drop path (rebuild panic, cancellation, early
///   return on plugin error) the guard's `Drop` releases the reserved
///   bytes back to the admission pool. This keeps the §G.5 invariant
///   intact across every exit path.
///
/// The manager pointer is a [`Weak`] so a guard that outlives its
/// manager (e.g. the daemon is dropped mid-rebuild) does not try to
/// touch freed memory. A `None` upgrade on drop is silently ignored —
/// the manager took the retained bytes with it when it dropped.
#[must_use = "RebuildReservation must either be consumed by publish_and_retain() \
              or intentionally dropped to return its bytes to the admission pool"]
pub struct RebuildReservation {
    manager: Weak<WorkspaceManager>,
    bytes: u64,
    released: bool,
}

impl RebuildReservation {
    /// How many bytes this reservation currently holds.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl std::fmt::Debug for RebuildReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RebuildReservation")
            .field("bytes", &self.bytes)
            .field("released", &self.released)
            .finish_non_exhaustive()
    }
}

impl Drop for RebuildReservation {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Some(mgr) = self.manager.upgrade() {
            let mut state = mgr.admission.lock();
            state.reserved_bytes = state.reserved_bytes.saturating_sub(self.bytes);
        }
    }
}

// ---------------------------------------------------------------------------
// RollbackGuard (panic-safety for publish_and_retain)
// ---------------------------------------------------------------------------

/// Panic-safe rollback wrapper used by [`WorkspaceManager::publish_and_retain`].
///
/// Captures the prior published generation (graph and record as one
/// `Arc<PublishedGraph>`, design D14) and the prior `memory_bytes`
/// *before* the swap. If the thread unwinds between the swap and the
/// admission-mutex acquisition, the guard's `Drop` restores both,
/// leaving the workspace serving its pre-rebuild generation as if the
/// publish never happened. Restoring the pair as one value means a
/// rollback cannot leave the old graph beside the new record.
///
/// Correctness depends on three contracts:
///
/// 1. The guard is constructed *before* the `ArcSwap::swap` call.
/// 2. `armed` is set to `false` only on the success path, after the
///    admission mutex has released.
/// 3. No fallible operation (heap allocation failure, etc.) runs
///    between the swap and the admission commit; otherwise the guard
///    would be asked to reverse a partial publish.
pub(crate) struct RollbackGuard<'a> {
    pub(crate) ws: &'a LoadedWorkspace,
    /// The generation published before the swap (the placeholder before
    /// the first publish). Restored as one value so the pair never
    /// disagrees after a rollback.
    pub(crate) prior_published: Option<Arc<PublishedGraph>>,
    pub(crate) prior_bytes: usize,
    pub(crate) armed: bool,
}

impl Drop for RollbackGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(prior) = self.prior_published.take() {
            self.ws.published.store(prior);
        }
        self.ws
            .memory_bytes
            .store(self.prior_bytes, std::sync::atomic::Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Retention reaper task
// ---------------------------------------------------------------------------

/// Long-lived tokio task: polls [`WorkspaceManager::reap_once`] on a
/// fixed 25 ms cadence (A2 §G.3).
///
/// Takes a `Weak<WorkspaceManager>` so a `WorkspaceManager::drop`
/// before the task notices the abort signal does not dereference
/// freed memory. The first failed `Weak::upgrade` exits the loop
/// cleanly.
async fn retention_reaper(mgr: Weak<WorkspaceManager>) {
    let interval = Duration::from_millis(25);
    loop {
        tokio::time::sleep(interval).await;
        let Some(mgr) = mgr.upgrade() else {
            return;
        };
        mgr.reap_once();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::atomic::Ordering,
    };

    use sqry_core::project::ProjectRootMode;
    use sqry_daemon_protocol::{
        ArtifactId, ArtifactInputDigest, ObjectFormat, RepositoryIdentity, ResidentHandleKind,
        ResolvedRevision, RevisionId, RevisionSelector, SourceByteMode,
    };

    use crate::config::DaemonConfig;

    use super::{
        super::{loaded::LoadedWorkspace, state::WorkspaceKey},
        *,
    };

    fn make_config() -> Arc<DaemonConfig> {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 1 MiB budget keeps the arithmetic tractable in assertions.
        Arc::new(DaemonConfig {
            memory_limit_mb: 1,
            ..DaemonConfig::default()
        })
    }

    fn make_workspace() -> Arc<LoadedWorkspace> {
        Arc::new(LoadedWorkspace::new(
            WorkspaceKey::new(
                PathBuf::from("/repos/example"),
                ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ))
    }

    fn resident_load_request(artifact: &str, revision: &str) -> ResidentRevisionLoad {
        ResidentRevisionLoad {
            source_root: PathBuf::from("/repos/example"),
            revision_id: RevisionId(revision.to_owned()),
            handle_kind: ResidentHandleKind::ImmutableRevision,
            artifact_id: ArtifactId(artifact.to_owned()),
            artifact_inputs: ArtifactInputDigest {
                schema_version: 1,
                digest: "digest".to_owned(),
            },
            resolved: ResolvedRevision {
                selector: RevisionSelector::Commit {
                    oid: "b".repeat(40),
                },
                repository: RepositoryIdentity {
                    repo_identity_hash: "repo".to_owned(),
                    object_format: ObjectFormat::Sha1,
                    remote_fingerprint: None,
                },
                commit_oid: Some("b".repeat(40)),
                tree_oid: "a".repeat(40),
                object_format: ObjectFormat::Sha1,
                source_byte_mode: SourceByteMode::RawGitObjects,
                resolved_at: "2026-06-26T00:00:00Z".to_owned(),
            },
            pinned: false,
        }
    }

    /// Register a workspace under `key` on `mgr` so that
    /// `reserve_rebuild` sees it present in its Phase-1
    /// `workspaces.read()` scope. Phase 7b1 tightens `reserve_rebuild`
    /// to reject unregistered keys with `DaemonError::WorkspaceEvicted`,
    /// so every admission-level test that expects a reservation (or a
    /// memory-budget rejection) must insert a workspace first.
    fn register_workspace(mgr: &WorkspaceManager, key: &WorkspaceKey) {
        mgr.workspaces.write().insert(
            key.clone(),
            Arc::new(LoadedWorkspace::new(key.clone(), false)),
        );
    }

    #[test]
    fn reserve_rebuild_succeeds_when_headroom_available() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        register_workspace(&mgr, &key);
        let reservation = mgr
            .reserve_rebuild(&key, 500_000) // 500 kB into 1 MiB budget
            .expect("reservation fits");
        assert_eq!(reservation.bytes(), 500_000);
        assert_eq!(mgr.admission.lock().reserved_bytes, 500_000);
        drop(reservation);
        assert_eq!(
            mgr.admission.lock().reserved_bytes,
            0,
            "dropping an unconsumed reservation must return its bytes",
        );
    }

    #[test]
    fn reserve_rebuild_rejects_oversized_request() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        register_workspace(&mgr, &key);
        let err = mgr.reserve_rebuild(&key, 10 * 1024 * 1024).expect_err(
            "a reservation bigger than the budget must be rejected with MemoryBudgetExceeded",
        );
        match err {
            DaemonError::MemoryBudgetExceeded {
                limit_bytes,
                requested_bytes,
                ..
            } => {
                assert_eq!(limit_bytes, 1024 * 1024);
                assert_eq!(requested_bytes, 10 * 1024 * 1024);
            }
            other => panic!("wrong error variant: {other:?}"),
        }
        assert_eq!(
            mgr.admission.lock().reserved_bytes,
            0,
            "a rejected reservation must not mutate admission state",
        );
    }

    #[test]
    fn reserve_rebuild_rejects_when_running_total_would_exceed_budget() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        register_workspace(&mgr, &key);
        let a = mgr.reserve_rebuild(&key, 600_000).expect("first fits");
        let err = mgr
            .reserve_rebuild(&key, 600_000)
            .expect_err("second pushes over 1 MiB budget");
        match err {
            DaemonError::MemoryBudgetExceeded { reserved_bytes, .. } => {
                assert_eq!(reserved_bytes, 600_000, "first reservation still held");
            }
            other => panic!("wrong error variant: {other:?}"),
        }
        drop(a);
    }

    #[test]
    fn reserve_rebuild_rejects_unknown_key() {
        // Task 7 Phase 7b1: unregistered keys must be rejected with
        // WorkspaceEvicted instead of succeeding. Prevents publishing
        // into an orphaned LoadedWorkspace after a race with eviction.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/never-registered"),
            ProjectRootMode::GitRoot,
            0xDEAD,
        );
        let err = mgr
            .reserve_rebuild(&key, 100_000)
            .expect_err("unknown key must surface WorkspaceEvicted");
        match err {
            DaemonError::WorkspaceEvicted { root } => {
                assert_eq!(root, PathBuf::from("/repos/never-registered"));
            }
            other => panic!("wrong error variant: {other:?}"),
        }
        assert_eq!(
            mgr.admission.lock().reserved_bytes,
            0,
            "a rejected reservation must not mutate admission state",
        );
    }

    #[test]
    fn reserve_rebuild_rejects_cancelled_workspace() {
        // Task 7 Phase 7b1: a workspace whose `rebuild_cancelled` flag
        // is set (by `execute_eviction`) must be rejected even if still
        // present in the map (the two mutations run under the same
        // `workspaces.write()` scope, but defensive reads should catch
        // either signal).
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/cancelled"),
            ProjectRootMode::GitRoot,
            0xCAFE,
        );
        let ws = Arc::new(LoadedWorkspace::new(key.clone(), false));
        ws.rebuild_cancelled.store(true, Ordering::Release);
        mgr.workspaces.write().insert(key.clone(), ws);

        let err = mgr
            .reserve_rebuild(&key, 100_000)
            .expect_err("cancelled workspace must surface WorkspaceEvicted");
        match err {
            DaemonError::WorkspaceEvicted { root } => {
                assert_eq!(root, PathBuf::from("/repos/cancelled"));
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    #[test]
    fn publish_and_retain_moves_bytes_and_retains_old_arc() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr.reserve_rebuild(&ws.key, 100_000).expect("reserve fits");

        // Pre-seed workspace memory_bytes so publish exercises the
        // loaded-bytes swap (subtract prior, add new).
        ws.memory_bytes.store(50_000, Ordering::Release);
        mgr.admission.lock().loaded_bytes = 50_000;

        let new_graph = CodeGraph::new();
        let new_bytes = new_graph.heap_bytes() as u64;
        let (token, _published) = mgr
            .publish_and_retain(
                reservation,
                &ws,
                BuiltGraph::new(new_graph, Arc::new(RosterRecord::fast_path_default())),
            )
            .expect("publish_and_retain succeeds within memory budget");

        let state = mgr.admission.lock();
        assert_eq!(
            state.reserved_bytes, 0,
            "reservation bytes must drain on publish"
        );
        assert_eq!(
            state.loaded_bytes, new_bytes,
            "loaded_bytes = prior(50k) - prior(50k) + new(heap_bytes())",
        );
        assert_eq!(state.retained_old.len(), 1, "exactly one retained entry");
        let retained = state.retained_old.get(&token).expect("token present");
        assert_eq!(
            retained.bytes, 50_000,
            "retained bytes is the prior workspace memory_bytes",
        );
        assert_eq!(
            Arc::strong_count(&retained.graph),
            1,
            "admission map is the sole holder of the old Arc after publish",
        );
    }

    #[test]
    fn rollback_guard_restores_workspace_on_panic_path() {
        // Synthesise the exact field layout publish_and_retain sets up
        // so the guard's Drop behaviour can be exercised directly,
        // without the heavy publish path.
        let ws = make_workspace();
        let old_graph = Arc::new(CodeGraph::new());
        let old_published = Arc::new(PublishedGraph::new(Arc::clone(&old_graph), None));
        ws.published.store(Arc::clone(&old_published));
        ws.memory_bytes.store(10_000, Ordering::Release);

        {
            let mut guard = RollbackGuard {
                ws: &ws,
                prior_published: Some(Arc::clone(&old_published)),
                prior_bytes: 10_000,
                armed: true,
            };

            // Simulate a partial publish: swap the generation + memory_bytes.
            let stomped = Arc::new(CodeGraph::new());
            ws.published
                .store(Arc::new(PublishedGraph::new(Arc::clone(&stomped), None)));
            ws.memory_bytes.store(99_999, Ordering::Release);

            // `armed == true` so the guard reverses both fields on drop.
            // The disarm is intentionally skipped: this mimics the panic path.
            let _ = &mut guard;
        }

        // After the guard drops, both fields must match the prior.
        let restored = ws.graph();
        assert!(Arc::ptr_eq(&restored, &old_graph));
        assert_eq!(ws.memory_bytes.load(Ordering::Acquire), 10_000);
    }

    /// T40 (surface parity W1 round 3, D14): the graph and its record are
    /// one published generation; after a panic between the swap and the
    /// admission commit the guard restores that generation as one value,
    /// so both halves are the prior pair by pointer identity, and a
    /// successful publish replaces both with the caller's pair.
    #[test]
    fn rollback_guard_restores_the_published_pair() {
        let ws = make_workspace();
        let old_graph = Arc::new(CodeGraph::new());
        let old_roster = Arc::new(RosterRecord::fast_path_default());
        let old_published = Arc::new(PublishedGraph::new(
            Arc::clone(&old_graph),
            Some(Arc::clone(&old_roster)),
        ));
        ws.published.store(Arc::clone(&old_published));
        ws.memory_bytes.store(10_000, Ordering::Release);

        {
            let _guard = RollbackGuard {
                ws: &ws,
                prior_published: Some(Arc::clone(&old_published)),
                prior_bytes: 10_000,
                armed: true,
            };
            let stomped_roster = Arc::new(RosterRecord {
                active_plugin_ids: vec!["rust".to_string()],
                high_cost_mode: None,
                source: sqry_plugin_registry::RosterSource::Fallback,
            });
            ws.published.store(Arc::new(PublishedGraph::new(
                Arc::new(CodeGraph::new()),
                Some(stomped_roster),
            )));
            ws.memory_bytes.store(99_999, Ordering::Release);
        }
        let restored = ws.published();
        assert!(
            Arc::ptr_eq(&restored, &old_published),
            "the guard restores the prior generation as one value"
        );
        let restored_roster = restored.roster.clone().expect("roster restored");
        assert!(Arc::ptr_eq(&restored_roster, &old_roster));
        assert!(Arc::ptr_eq(&restored.graph, &old_graph));
        assert!(Arc::ptr_eq(&ws.graph(), &old_graph));
        assert_eq!(ws.memory_bytes.load(Ordering::Acquire), 10_000);

        // A successful publish replaces both and the published record is
        // the one the caller passed in.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 0)
            .expect("zero-size reservation always fits");
        let published_record = Arc::new(RosterRecord {
            active_plugin_ids: vec!["rust".to_string(), "json".to_string()],
            high_cost_mode: Some("include_all".to_string()),
            source: sqry_plugin_registry::RosterSource::PersistedManifest,
        });
        mgr.publish_and_retain(
            reservation,
            &ws,
            BuiltGraph::new(CodeGraph::new(), Arc::clone(&published_record)),
        )
        .expect("publish succeeds");
        let after = ws.published();
        assert!(Arc::ptr_eq(
            after.roster.as_ref().expect("published record"),
            &published_record
        ));
        assert!(
            !Arc::ptr_eq(&after.graph, &old_graph),
            "the publish replaced the graph half with the caller's graph"
        );
    }

    /// A small non-empty graph, so a generation can be told apart from
    /// the empty one by content (`publish_and_retain` takes the graph by
    /// value and wraps it in a fresh `Arc`, so the graph half of a
    /// generation has no fixed pointer to compare against).
    fn graph_with_nodes(count: u32) -> CodeGraph {
        use sqry_core::graph::node::Language;
        use sqry_core::graph::unified::{NodeEntry, NodeKind};

        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("/repos/example/src/lib.rs"), Some(Language::Rust))
            .expect("register file");
        for index in 0..count {
            let name = format!("generation_b_{index}");
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

    /// T30 (surface parity W1 round 3, D14, codex R3-1): a reader that
    /// classifies while a publisher alternates between two generations
    /// never observes one generation's graph beside the other's record.
    ///
    /// Generation A is the empty graph with record A; generation B is a
    /// graph with `B_NODES` nodes and record B. The publisher thread
    /// alternates them through the production `publish_and_retain`; the
    /// reader thread calls `classify_for_serve` until the publisher is
    /// done and, for every `Fresh` verdict, pairs the graph's node count
    /// with the record's pointer identity. A mixed sample is (empty, B)
    /// or (`B_NODES`, A). The sampler prints its iteration count and the
    /// mixed count, and asserts the mixed count is zero. On the two-slot
    /// publish this window is a few instructions wide, so the sampler may
    /// miss it; codex's paused control is the red evidence for the race
    /// and the compiler (one slot, nothing to pause between) is the oracle
    /// for its absence. The sampler stays as the runtime control.
    #[test]
    fn published_graph_and_roster_are_one_generation() {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use std::sync::atomic::AtomicBool;

        const B_NODES: u32 = 3;
        const PUBLISHES: usize = 4_000;

        let mgr = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
        let ws = make_workspace();
        ws.store_state(WorkspaceState::Loaded);
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let record_a = Arc::new(RosterRecord::fast_path_default());
        let record_b = Arc::new(RosterRecord {
            active_plugin_ids: vec!["rust".to_string(), "json".to_string()],
            high_cost_mode: Some("include_all".to_string()),
            source: sqry_plugin_registry::RosterSource::PersistedManifest,
        });
        assert!(!Arc::ptr_eq(&record_a, &record_b));

        // Start from a coherent generation A so the first sample is well
        // defined.
        let reservation = mgr.reserve_rebuild(&ws.key, 0).expect("reserve");
        mgr.publish_and_retain(
            reservation,
            &ws,
            BuiltGraph::new(CodeGraph::new(), Arc::clone(&record_a)),
        )
        .expect("initial publish");

        let done = Arc::new(AtomicBool::new(false));
        let publisher = {
            let mgr = Arc::clone(&mgr);
            let ws = Arc::clone(&ws);
            let record_a = Arc::clone(&record_a);
            let record_b = Arc::clone(&record_b);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                for publish in 0..PUBLISHES {
                    let (graph, record) = if publish % 2 == 0 {
                        (graph_with_nodes(B_NODES), Arc::clone(&record_b))
                    } else {
                        (CodeGraph::new(), Arc::clone(&record_a))
                    };
                    let reservation = mgr.reserve_rebuild(&ws.key, 0).expect("reserve");
                    mgr.publish_and_retain(reservation, &ws, BuiltGraph::new(graph, record))
                        .expect("publish");
                    // Keep the retained tier bounded; entries whose graph is
                    // still held by the reader survive the reap.
                    mgr.reap_once();
                }
                done.store(true, Ordering::Release);
            })
        };

        let mut iterations: u64 = 0;
        let mut fresh: u64 = 0;
        let mut mixed: u64 = 0;
        let now = std::time::SystemTime::now();
        while !done.load(Ordering::Acquire) {
            iterations += 1;
            match mgr.classify_for_serve(&ws.key, now) {
                Ok(ServeVerdict::Fresh { graph, roster, .. }) => {
                    fresh += 1;
                    let is_a_graph = graph.node_count() == 0;
                    let is_b_graph = graph.node_count() == B_NODES as usize;
                    let is_a_record = Arc::ptr_eq(&roster, &record_a);
                    let is_b_record = Arc::ptr_eq(&roster, &record_b);
                    assert!(
                        is_a_graph || is_b_graph,
                        "every served graph is generation A or B, got node_count={}",
                        graph.node_count()
                    );
                    assert!(
                        is_a_record || is_b_record,
                        "every served record is record A or B"
                    );
                    if (is_a_graph && is_b_record) || (is_b_graph && is_a_record) {
                        mixed += 1;
                    }
                }
                other => panic!("a Loaded workspace with a record classifies Fresh, got {other:?}"),
            }
        }
        publisher.join().expect("publisher thread");

        println!("T30 sampler: iterations={iterations} fresh={fresh} mixed={mixed}");
        assert!(iterations > 0, "the sampler must run at least once");
        assert_eq!(
            fresh, iterations,
            "every sample of a Loaded workspace with a record is Fresh"
        );
        assert_eq!(
            mixed, 0,
            "a sample must never pair one generation's graph with the other's record \
             (iterations={iterations})"
        );
        // The final generation is coherent by content as well.
        let last = ws.published();
        let last_record = last.roster.clone().expect("record present");
        if Arc::ptr_eq(&last_record, &record_b) {
            assert_eq!(last.graph.node_count(), B_NODES as usize);
        } else {
            assert!(Arc::ptr_eq(&last_record, &record_a));
            assert_eq!(last.graph.node_count(), 0);
        }
    }

    /// T48 (surface parity W1 round 5, design D26; battery row K36's
    /// discriminating oracle): `publish_and_retain` returns the generation
    /// it swapped in, not whatever the slot holds when it returns. The
    /// observation plant at `PublishCommitted` (after the admission commit,
    /// before the return) takes a second reservation and publishes B for
    /// the same workspace, so the slot holds B when the outer call
    /// returns; the outer call must still return A's pair. Under K36's
    /// plant (`Ok((token, workspace.published()))`) it returns B's. Green
    /// on both heads (the function already returns the clone taken before
    /// the swap): a declared control whose purpose is the row.
    #[test]
    fn publish_and_retain_returns_the_generation_it_swapped_in() {
        use std::sync::Weak;

        const A_NODES: u32 = 3;
        const B_NODES: u32 = 7;

        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let record_a = Arc::new(RosterRecord::fast_path_default());
        let record_b = Arc::new(RosterRecord::fast_path_default());
        assert!(
            !Arc::ptr_eq(&record_a, &record_b),
            "the two records are distinct allocations"
        );

        let nested_publishes = Arc::new(AtomicU64::new(0));
        let weak: Weak<WorkspaceManager> = Arc::downgrade(&mgr);
        let plant_ws = Arc::clone(&ws);
        let plant_record_b = Arc::clone(&record_b);
        let plant_count = Arc::clone(&nested_publishes);
        mgr.install_observation_plant_for_test(Arc::new(move |phase| {
            if phase != ObservationPhase::PublishCommitted {
                return;
            }
            // Only the outer call's commit publishes B; the nested call's
            // own `PublishCommitted` is counted and ignored.
            if plant_count.fetch_add(1, Ordering::AcqRel) != 0 {
                return;
            }
            let Some(mgr) = weak.upgrade() else {
                return;
            };
            let reservation = mgr
                .reserve_rebuild(&plant_ws.key, 100_000)
                .expect("the plant's reservation fits");
            mgr.publish_and_retain(
                reservation,
                &plant_ws,
                BuiltGraph::new(graph_with_nodes(B_NODES), Arc::clone(&plant_record_b)),
            )
            .expect("the plant publishes generation B");
        }));

        let reservation = mgr.reserve_rebuild(&ws.key, 100_000).expect("reserve fits");
        let (_token, returned) = mgr
            .publish_and_retain(
                reservation,
                &ws,
                BuiltGraph::new(graph_with_nodes(A_NODES), Arc::clone(&record_a)),
            )
            .expect("the outer publish succeeds");
        let slot = ws.published();

        let label = |record: &Option<Arc<RosterRecord>>| match record {
            Some(record) if Arc::ptr_eq(record, &record_a) => "A",
            Some(record) if Arc::ptr_eq(record, &record_b) => "B",
            Some(_) => "other",
            None => "none",
        };
        println!(
            "R5-4 publish plant: returned_record={} returned_nodes={} slot_record={} \
             slot_nodes={} nested_publishes={}",
            label(&returned.roster),
            returned.graph.node_count(),
            label(&slot.roster),
            slot.graph.node_count(),
            nested_publishes.load(Ordering::Acquire)
        );
        assert_eq!(
            nested_publishes.load(Ordering::Acquire),
            2,
            "the plant's commit fired for the outer call and for its own nested call"
        );
        assert_eq!(
            (label(&slot.roster), slot.graph.node_count()),
            ("B", B_NODES as usize),
            "the plant did publish: the slot holds B when the outer call returns"
        );
        assert_eq!(
            (label(&returned.roster), returned.graph.node_count()),
            ("A", A_NODES as usize),
            "publish_and_retain must return the generation it swapped in"
        );
    }

    /// T54 (surface parity W1 round 6, design D32; the discriminating
    /// oracle of battery row C63 and the second oracle of VR61 and VR61b):
    /// `try_evict_for_test` yields to a held read guard. With
    /// `workspaces_read()` held on the calling thread the hook must answer
    /// `WouldBlock` and leave the slot untouched (`Loaded`, with a record);
    /// once the guard drops it must answer `Evicted` and leave the
    /// tombstone (`Evicted`, no record); an unknown key answers `Absent`.
    /// Under C63 (`self.workspaces.read().clone()` in place of
    /// `try_write()`) the held call takes a second read guard on this
    /// thread, the clone shares the `Arc<LoadedWorkspace>`, and the answer
    /// is `Evicted` with the slot already tombstoned. Green on both heads
    /// (the hook already takes `try_write`): a declared control whose
    /// purpose is the rows. The verifier's pin (`w1r5_verifier_pins.rs`)
    /// establishes the outcomes and the tombstone; this test establishes
    /// the lock.
    #[test]
    fn try_evict_for_test_yields_to_a_held_read_guard() {
        let manager = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/r6-lock-pin"),
            ProjectRootMode::GitRoot,
            0x6,
        );
        let unknown = WorkspaceKey::new(
            PathBuf::from("/repos/r6-lock-pin-unknown"),
            ProjectRootMode::GitRoot,
            0x7,
        );
        manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);

        let guard = manager.workspaces_read();
        let held = manager.try_evict_for_test(&key);
        let (held_state, held_record) = {
            let ws = guard.get(&key).expect("the seeded slot is in the map");
            (ws.load_state(), ws.roster().is_some())
        };
        drop(guard);

        let released = manager.try_evict_for_test(&key);
        let (after_state, after_record) = {
            let workspaces = manager.workspaces_read();
            let ws = workspaces
                .get(&key)
                .expect("the tombstone stays in the map");
            (ws.load_state(), ws.roster().is_some())
        };
        let absent = manager.try_evict_for_test(&unknown);

        println!(
            "R6-5 lock pin: held={held:?} held_state={held_state:?} held_record={held_record} \
             released={released:?} after_state={after_state:?} after_record={after_record} \
             absent={absent:?}"
        );
        assert_eq!(
            held,
            TryEvictOutcome::WouldBlock,
            "the hook must yield to a held read guard"
        );
        assert_eq!(
            (held_state, held_record),
            (WorkspaceState::Loaded, true),
            "under the held guard the slot is untouched: Loaded with a record"
        );
        assert_eq!(
            released,
            TryEvictOutcome::Evicted,
            "with the guard released the hook evicts"
        );
        assert_eq!(
            (after_state, after_record),
            (WorkspaceState::Evicted, false),
            "the eviction left the tombstone: Evicted, holding the placeholder with no record"
        );
        assert_eq!(
            absent,
            TryEvictOutcome::Absent,
            "an unknown key answers Absent"
        );
    }

    #[test]
    fn rollback_guard_disarmed_is_noop() {
        let ws = make_workspace();
        let old_graph = Arc::new(CodeGraph::new());
        let old_published = Arc::new(PublishedGraph::new(Arc::clone(&old_graph), None));
        ws.published.store(Arc::clone(&old_published));
        ws.memory_bytes.store(10_000, Ordering::Release);

        {
            let mut guard = RollbackGuard {
                ws: &ws,
                prior_published: Some(Arc::clone(&old_published)),
                prior_bytes: 10_000,
                armed: true,
            };
            let stomped = Arc::new(CodeGraph::new());
            ws.published
                .store(Arc::new(PublishedGraph::new(Arc::clone(&stomped), None)));
            ws.memory_bytes.store(99_999, Ordering::Release);

            // Success path disarms the guard.
            guard.armed = false;
        }
        assert!(
            !Arc::ptr_eq(&ws.graph(), &old_graph),
            "a disarmed guard leaves the stomped generation in place"
        );

        // State must stay "stomped" — the guard was disarmed.
        assert_eq!(ws.memory_bytes.load(Ordering::Acquire), 99_999);
    }

    #[test]
    fn reap_once_drops_last_holder_entries() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 0)
            .expect("zero-size reservation always fits");
        // Publish-and-retain with a fresh empty graph; the old graph
        // becomes retained.
        mgr.publish_and_retain(reservation, &ws, BuiltGraph::empty_fast_path())
            .expect("publish_and_retain succeeds within memory budget");
        assert_eq!(mgr.admission.lock().retained_old.len(), 1);

        // No query holds the old Arc, so the next reap tick frees it.
        mgr.reap_once();
        assert_eq!(
            mgr.admission.lock().retained_old.len(),
            0,
            "reaper must free entries whose strong_count == 1",
        );
    }

    #[test]
    fn reap_once_retains_entries_with_outstanding_holders() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 0)
            .expect("zero-size reservation always fits");
        mgr.publish_and_retain(reservation, &ws, BuiltGraph::empty_fast_path())
            .expect("publish_and_retain succeeds within memory budget");

        // Simulate a slow query holding the retained Arc.
        let held = {
            let state = mgr.admission.lock();
            let token = *state.retained_old.keys().next().expect("one entry");
            Arc::clone(&state.retained_old.get(&token).unwrap().graph)
        };
        assert_eq!(Arc::strong_count(&held), 2);

        mgr.reap_once();
        assert_eq!(
            mgr.admission.lock().retained_old.len(),
            1,
            "reaper must not drop entries that slow queries still hold",
        );
        drop(held);

        mgr.reap_once();
        assert_eq!(
            mgr.admission.lock().retained_old.len(),
            0,
            "reaper frees the entry once the last slow query releases",
        );
    }

    #[test]
    fn unconsumed_reservation_refunds_reserved_bytes_on_drop() {
        // Regression for Codex Task 6 Phase 6a iter-1 MAJOR:
        // if a rebuild panics *between* `reserve_rebuild` and the
        // admission-mutex section of `publish_and_retain`, the
        // reservation's Drop must refund `reserved_bytes` back to
        // the admission pool. A pre-fix bug disarmed the reservation
        // too early and leaked bytes on any unwind path.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 250_000)
            .expect("reservation fits");
        assert_eq!(mgr.admission.lock().reserved_bytes, 250_000);

        // Simulate a rebuild that panics after reservation but
        // before publish by letting the reservation drop on the
        // unwind-equivalent code path (explicit drop here; the
        // RAII guard fires the same way under `catch_unwind`).
        drop(reservation);

        assert_eq!(
            mgr.admission.lock().reserved_bytes,
            0,
            "unconsumed reservation must refund reserved_bytes on drop \
             (Codex Task 6 Phase 6a iter-1 MAJOR regression)",
        );
    }

    #[test]
    fn publish_and_retain_leaves_reservation_fully_disarmed_on_success() {
        // Companion to the refund regression: once publish_and_retain
        // completes successfully, the reservation must be disarmed —
        // otherwise its Drop at scope-exit would double-refund and
        // corrupt admission state.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 100_000)
            .expect("reservation fits");
        let admission_before = mgr.admission.lock().reserved_bytes;
        assert_eq!(admission_before, 100_000);

        // Drive the full commit path. After this returns the
        // reservation is already moved into the function, so we can
        // only observe the *absence* of any stray refund.
        let (_token, _published) = mgr
            .publish_and_retain(reservation, &ws, BuiltGraph::empty_fast_path())
            .expect("publish_and_retain succeeds within memory budget");
        let admission_after = mgr.admission.lock().reserved_bytes;
        assert_eq!(
            admission_after, 0,
            "publish must drain reserved_bytes exactly once, not double-drain or leak",
        );

        // A fresh reservation should see headroom = budget - loaded - retained;
        // if the previous publish leaked reserved_bytes this would fail.
        let again = mgr
            .reserve_rebuild(&ws.key, 100_000)
            .expect("post-publish admission must still admit a same-size reservation");
        drop(again);
        assert_eq!(mgr.admission.lock().reserved_bytes, 0);
    }

    #[test]
    fn unwind_after_swap_before_admission_commit_restores_full_state() {
        // Regression for Codex Task 6 Phase 6a iter-2 MAJOR:
        // simulate a panic *between* the ArcSwap swap and the
        // admission mutex acquisition. After unwind, the admission
        // state must be exactly pre-call: reserved_bytes refunded,
        // loaded_bytes untouched, retained_old empty, workspace.published
        // and workspace.memory_bytes restored to their prior values.
        //
        // We can't inject a panic into the real `publish_and_retain`
        // without mocking the allocator, so we reproduce the exact
        // Drop-order interaction using the public types: build a
        // RollbackGuard + RebuildReservation in the same geometry as
        // the real function, run `catch_unwind` over the non-
        // recoverable zone, and panic inside it.
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws = Arc::new(LoadedWorkspace::new(
            WorkspaceKey::new(
                PathBuf::from("/repos/example"),
                ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));

        // Pre-seed workspace bytes so we can observe rollback.
        let prior_bytes_usize = 50_000usize;
        ws.memory_bytes.store(prior_bytes_usize, Ordering::Release);
        mgr.admission.lock().loaded_bytes = 50_000;
        let prior_published = ws.published();
        let prior_arc = Arc::clone(&prior_published.graph);

        // Reserve headroom as the real function does.
        let reservation = mgr
            .reserve_rebuild(&ws.key, 100_000)
            .expect("reservation fits");
        assert_eq!(mgr.admission.lock().reserved_bytes, 100_000);

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            // Mirror `publish_and_retain` up to and INCLUDING the
            // generation swap + update_memory, then panic *before* we
            // would have acquired the admission mutex. This is the
            // exact unwind window the iter-2 finding describes.
            let new_published = Arc::new(PublishedGraph::new(
                Arc::new(CodeGraph::new()),
                Some(Arc::new(RosterRecord::fast_path_default())),
            ));
            let prior_published_clone = ws.published();
            // The guard is armed and has no visible use after this
            // point; its Drop is the entire reason the scope exists,
            // so the binding is deliberately underscore-prefixed and
            // held until the panic unwinds the stack.
            let _rollback = RollbackGuard {
                ws: &ws,
                prior_published: Some(prior_published_clone),
                prior_bytes: prior_bytes_usize,
                armed: true,
            };
            let _old_published = ws.published.swap(new_published);
            let _prev = ws.update_memory(99_999);

            // Hand the reservation into the scope so its Drop fires
            // on unwind if we never disarm it — which we won't.
            let _hold = reservation;

            // Simulate the panic site (e.g. retained_old.insert OOM).
            panic!("simulated panic inside publish_and_retain");
        }));
        assert!(outcome.is_err(), "catch_unwind must observe the panic");

        // Post-unwind assertions — every piece of admission state and
        // every observable piece of workspace state must match the
        // pre-call snapshot exactly.
        let restored = ws.published();
        assert!(
            Arc::ptr_eq(&restored, &prior_published),
            "RollbackGuard must restore ws.published to the prior generation after unwind",
        );
        assert!(
            Arc::ptr_eq(&restored.graph, &prior_arc),
            "the restored generation carries the prior graph Arc",
        );
        assert_eq!(
            ws.memory_bytes.load(Ordering::Acquire),
            prior_bytes_usize,
            "RollbackGuard must restore ws.memory_bytes after unwind",
        );
        let state = mgr.admission.lock();
        assert_eq!(
            state.reserved_bytes, 0,
            "reservation refund must return reserved_bytes to pre-call value (0)",
        );
        assert_eq!(
            state.loaded_bytes, 50_000,
            "loaded_bytes must not be mutated when admission commit is never entered",
        );
        assert_eq!(
            state.retained_old.len(),
            0,
            "retained_old must be empty when admission commit is never entered",
        );
    }

    // --- Phase 6b: lifecycle primitives --------------------------

    fn make_key_at(path: &str, fingerprint: u64) -> WorkspaceKey {
        WorkspaceKey::new(PathBuf::from(path), ProjectRootMode::GitRoot, fingerprint)
    }

    #[test]
    fn get_or_load_builds_on_miss_and_caches() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let builder = super::super::builder::EmptyGraphBuilder;

        let g1 = mgr
            .get_or_load(&key, &builder, 1_000)
            .expect("first load succeeds");
        let g2 = mgr
            .get_or_load(&key, &builder, 1_000)
            .expect("second load hits cache");
        assert!(
            Arc::ptr_eq(&g1, &g2),
            "cache hit must return the same Arc as the initial build",
        );
    }

    #[test]
    fn get_or_load_surfaces_builder_failures_and_sets_failed_state() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let failing = super::super::builder::FailingGraphBuilder::new("simulated plugin panic");

        let err = mgr
            .get_or_load(&key, &failing, 1_000)
            .expect_err("builder failure must bubble up");
        match err {
            DaemonError::WorkspaceBuildFailed { reason, .. } => {
                assert_eq!(reason, "simulated plugin panic");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // Workspace should be in Failed state with retry_count==1.
        let workspaces = mgr.workspaces.read();
        let ws = workspaces.get(&key).expect("workspace registered");
        assert_eq!(ws.load_state(), WorkspaceState::Failed);
        assert_eq!(ws.retry_count.load(Ordering::Acquire), 1);
        assert!(ws.last_error.read().is_some());
        drop(workspaces);

        // Admission state must NOT have leaked the reservation —
        // RebuildReservation's Drop fires on the error path.
        assert_eq!(mgr.admission.lock().reserved_bytes, 0);
    }

    #[test]
    fn evict_lru_picks_oldest_non_pinned_workspace() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let builder = super::super::builder::EmptyGraphBuilder;

        let a = make_key_at("/repos/a", 0x1);
        let b = make_key_at("/repos/b", 0x1);
        mgr.get_or_load(&a, &builder, 100_000).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        mgr.get_or_load(&b, &builder, 100_000).unwrap();

        // `a` was touched first, so it should be the LRU victim.
        let victim = mgr.evict_lru().expect("one candidate");
        assert_eq!(victim, a, "oldest workspace must be evicted first");
        // STEP_6 iter-2 contract change: LRU eviction keeps the
        // tombstone in the map (state == Evicted) so partial-
        // eviction reporting via `daemon/workspaceStatus` can
        // still surface the source root. Only `unload` removes
        // the entry.
        let workspaces = mgr.workspaces.read();
        let evicted_ws = workspaces
            .get(&a)
            .expect("LRU victim stays as tombstone in the manager map");
        assert_eq!(
            evicted_ws.load_state(),
            WorkspaceState::Evicted,
            "LRU victim must transition to Evicted, not be removed",
        );
        assert!(
            workspaces.contains_key(&b),
            "non-victim workspace must remain",
        );
    }

    #[test]
    fn evict_lru_returns_none_when_no_candidates() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        assert!(
            mgr.evict_lru().is_none(),
            "empty manager has no eviction candidate",
        );
    }

    #[test]
    fn evict_lru_skips_pinned_workspaces() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let builder = super::super::builder::EmptyGraphBuilder;
        let pinned_key = make_key_at("/repos/pinned", 0x1);

        // Insert a pinned workspace by manually constructing + registering.
        {
            let mut ws_map = mgr.workspaces.write();
            ws_map.insert(
                pinned_key.clone(),
                Arc::new(LoadedWorkspace::new(
                    pinned_key.clone(),
                    /*pinned*/ true,
                )),
            );
        }
        // And drive it into Loaded state via a no-op publish.
        {
            let ws = mgr.workspaces.read().get(&pinned_key).unwrap().clone();
            ws.store_state(WorkspaceState::Loaded);
            ws.touch();
        }

        // Plus a regular unpinned workspace.
        let other = make_key_at("/repos/other", 0x1);
        mgr.get_or_load(&other, &builder, 100_000).unwrap();

        // Evict should pick `other`, not the pinned one.
        let victim = mgr.evict_lru().expect("one candidate");
        assert_eq!(victim, other);
        assert!(mgr.workspaces.read().contains_key(&pinned_key));
    }

    /// D-i8-44: the reservation's eviction plan, the memory-pressure path,
    /// chooses an `Unloaded` workspace that still counts graph bytes (the
    /// shape a cancelled rebuild leaves), and still skips an `Unloaded`
    /// one that counts none (the shape `daemon/reset` leaves) and an
    /// `Evicted` tombstone. Before D-i8-44 it skipped every `Unloaded`
    /// workspace and the plan here was empty.
    #[test]
    fn eviction_plan_reclaims_an_unloaded_workspace_that_counts_bytes() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let requester = make_key_at("/repos/requester", 0x1);
        let cancelled = make_key_at("/repos/cancelled", 0x1);
        let reset = make_key_at("/repos/reset", 0x1);
        let tombstone = make_key_at("/repos/tombstone", 0x1);
        // Oldest first, so `evict_lru` would pick the reset slot and the
        // tombstone ahead of the requester if it chose them at all.
        mgr.insert_workspace_in_state_for_test(reset.clone(), WorkspaceState::Unloaded);
        mgr.insert_workspace_in_state_for_test(tombstone.clone(), WorkspaceState::Evicted);
        mgr.insert_workspace_in_state_for_test(cancelled.clone(), WorkspaceState::Unloaded);
        std::thread::sleep(Duration::from_millis(5));
        mgr.insert_workspace_in_state_for_test(requester.clone(), WorkspaceState::Loaded);
        for key in [&cancelled, &tombstone] {
            mgr.workspaces.read()[key]
                .memory_bytes
                .store(4096, Ordering::Release);
        }
        let plan = {
            let workspaces = mgr.workspaces.read();
            let state = mgr.admission.lock();
            WorkspaceManager::plan_eviction(&workspaces, &state, u64::MAX, &requester)
        };
        assert_eq!(plan, vec![cancelled.clone()], "only the slot with a graph");

        let victim = mgr.evict_lru();
        assert_eq!(victim, Some(cancelled.clone()));
        assert_eq!(
            mgr.workspaces.read()[&cancelled].load_state(),
            WorkspaceState::Evicted
        );
        // The requester (Loaded) is the only candidate left; the reset slot
        // and the tombstone never are.
        assert_eq!(mgr.evict_lru(), Some(requester));
        assert_eq!(mgr.evict_lru(), None);
        assert_eq!(
            mgr.workspaces.read()[&reset].load_state(),
            WorkspaceState::Unloaded
        );
    }

    #[test]
    fn unload_removes_workspace_and_reclaims_bytes() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let builder = super::super::builder::EmptyGraphBuilder;
        let key = make_key_at("/repos/example", 0x1);
        mgr.get_or_load(&key, &builder, 100_000).unwrap();
        assert!(mgr.workspaces.read().contains_key(&key));

        assert!(mgr.unload(&key), "unload must report present");
        assert!(!mgr.workspaces.read().contains_key(&key));

        assert!(!mgr.unload(&key), "unload on missing key returns false");
    }

    #[test]
    fn status_reflects_loaded_workspaces_and_memory() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let builder = super::super::builder::EmptyGraphBuilder;
        let key = make_key_at("/repos/example", 0x1);
        mgr.get_or_load(&key, &builder, 100_000).unwrap();

        let status = mgr.status();
        assert_eq!(status.daemon_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(status.workspaces.len(), 1);
        assert_eq!(
            status.workspaces[0].index_root,
            PathBuf::from("/repos/example")
        );
        assert_eq!(status.workspaces[0].state, WorkspaceState::Loaded);
        assert!(!status.workspaces[0].pinned);
        assert!(!status.workspaces[0].watching);
        assert_eq!(status.memory.limit_bytes, 1024 * 1024);
        // current_bytes is at least as large as the graph (empty here,
        // but loaded_bytes tracks an entry regardless).
        assert!(
            status.memory.high_water_bytes >= status.memory.current_bytes,
            "high_water_bytes must be monotonic wrt current_bytes",
        );
    }

    #[test]
    fn status_with_watcher_state_reflects_supplied_watcher_snapshot() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let builder = super::super::builder::EmptyGraphBuilder;
        let key = make_key_at("/repos/example", 0x1);
        mgr.get_or_load(&key, &builder, 100_000).unwrap();

        let status = mgr.status_with_watcher_state(|candidate| candidate == &key);

        assert_eq!(status.workspaces.len(), 1);
        assert!(status.workspaces[0].watching);
    }

    #[test]
    fn status_includes_resident_revision_memory_and_rows() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let load = resident_load_request("artifact-a", "rev-a");
        mgr.load_resident_revision(&load, || Ok(CodeGraph::new()))
            .unwrap();

        let status = mgr.status();

        assert_eq!(status.revisions.len(), 1);
        assert_eq!(
            status.revisions[0].handle_kind,
            ResidentHandleKind::ImmutableRevision
        );
        assert_eq!(
            status.memory.resident_revision_bytes,
            status.revisions[0].memory_bytes
        );
        assert_eq!(status.memory.live_workspace_bytes, 0);
        assert_eq!(
            status.memory.current_bytes,
            status.memory.resident_revision_bytes
        );
    }

    #[test]
    fn manager_query_guard_prevents_resident_lru_eviction_until_drop() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let load = resident_load_request("artifact-a", "rev-a");
        mgr.load_resident_revision(&load, || Ok(CodeGraph::new()))
            .unwrap();

        let guard = mgr.acquire_resident_query(&load.revision_id).unwrap();

        assert_eq!(mgr.evict_inactive_resident_revision_lru(), None);
        assert_eq!(
            mgr.pinned_revision_artifact_ids(),
            vec![ArtifactId("artifact-a".to_owned())]
        );

        drop(guard);

        assert_eq!(
            mgr.evict_inactive_resident_revision_lru(),
            Some(load.revision_id)
        );
    }

    #[test]
    fn reserve_rebuild_triggers_eviction_when_budget_tight() {
        // Budget is 1 MiB (from make_config). Fill it with a 700 kB
        // workspace, then reserve 600 kB — Phase 1 must pick the
        // 700 kB workspace as a victim, Phase 2 evicts it, Phase 3
        // commits the reservation.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let victim_key = make_key_at("/repos/victim", 0x1);
        let victim = Arc::new(LoadedWorkspace::new(victim_key.clone(), false));
        victim.memory_bytes.store(700_000, Ordering::Release);
        victim.store_state(WorkspaceState::Loaded);
        victim.touch();
        mgr.workspaces
            .write()
            .insert(victim_key.clone(), Arc::clone(&victim));
        mgr.admission.lock().loaded_bytes = 700_000;

        let new_key = make_key_at("/repos/new", 0x1);
        mgr.workspaces.write().insert(
            new_key.clone(),
            Arc::new(LoadedWorkspace::new(new_key.clone(), false)),
        );
        let reservation = mgr
            .reserve_rebuild(&new_key, 600_000)
            .expect("Phase 2 eviction must free headroom");
        // STEP_6 iter-2 contract: LRU eviction (Phase 2 of
        // `reserve_rebuild`) leaves the tombstone in the map.
        // The entry is now `Evicted` with `memory_bytes == 0` —
        // accounting moved to `retained_old`, but the key stays
        // visible to `daemon/workspaceStatus`.
        let workspaces = mgr.workspaces.read();
        let victim_tombstone = workspaces
            .get(&victim_key)
            .expect("victim stays as tombstone");
        assert_eq!(victim_tombstone.load_state(), WorkspaceState::Evicted);
        assert_eq!(
            victim_tombstone.memory_bytes.load(Ordering::Acquire),
            0,
            "evicted tombstone must hold no resident bytes",
        );
        drop(workspaces);
        // Admission reserved the new bytes.
        assert_eq!(mgr.admission.lock().reserved_bytes, 600_000);
        drop(reservation);
    }

    #[test]
    fn reserve_rebuild_rejects_when_only_pinned_workspaces_remain() {
        // Budget 1 MiB. Pin a 900 kB workspace. Requesting 600 kB
        // cannot evict the pin, so Phase 3 must reject.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let pinned_key = make_key_at("/repos/pinned", 0x1);
        let pinned = Arc::new(LoadedWorkspace::new(
            pinned_key.clone(),
            /*pinned*/ true,
        ));
        pinned.memory_bytes.store(900_000, Ordering::Release);
        pinned.store_state(WorkspaceState::Loaded);
        mgr.workspaces
            .write()
            .insert(pinned_key.clone(), Arc::clone(&pinned));
        mgr.admission.lock().loaded_bytes = 900_000;

        let new_key = make_key_at("/repos/new", 0x1);
        mgr.workspaces.write().insert(
            new_key.clone(),
            Arc::new(LoadedWorkspace::new(new_key.clone(), false)),
        );
        let err = mgr
            .reserve_rebuild(&new_key, 600_000)
            .expect_err("pinned workspace makes budget unfittable");
        match err {
            DaemonError::MemoryBudgetExceeded {
                requested_bytes,
                current_bytes,
                ..
            } => {
                assert_eq!(requested_bytes, 600_000);
                assert_eq!(
                    current_bytes, 900_000,
                    "pinned workspace bytes still count after Phase 2",
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // Pinned workspace must still be present.
        assert!(mgr.workspaces.read().contains_key(&pinned_key));
    }

    #[test]
    fn execute_eviction_routes_bytes_through_retained_old() {
        // Regression for Codex Task 6 Phase 6b iter-1 MAJOR #1:
        // eviction previously dropped the evicted Arc without
        // inserting a retained entry, leaking bytes if a slow
        // query still held the graph.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let ws_key = make_key_at("/repos/example", 0x1);
        let ws = Arc::new(LoadedWorkspace::new(ws_key.clone(), false));
        ws.memory_bytes.store(300_000, Ordering::Release);
        ws.store_state(WorkspaceState::Loaded);
        mgr.workspaces
            .write()
            .insert(ws_key.clone(), Arc::clone(&ws));
        mgr.admission.lock().loaded_bytes = 300_000;

        // Pin the current graph Arc via a simulated slow query
        // holder so the retained entry stays past the first reap.
        let slow_query_arc = ws.graph();

        mgr.execute_eviction(&ws_key);

        let state = mgr.admission.lock();
        assert_eq!(
            state.loaded_bytes, 0,
            "evicted workspace bytes must leave the loaded tier",
        );
        assert_eq!(
            state.retained_total_bytes(),
            300_000,
            "evicted workspace bytes must enter the retained tier",
        );
        assert_eq!(state.retained_old.len(), 1);
        drop(state);

        // The slow query still holds the Arc. A reap does NOT free
        // yet — §G.5 is preserved until strong_count == 1.
        mgr.reap_once();
        assert_eq!(mgr.admission.lock().retained_total_bytes(), 300_000);

        // Once the slow query releases, the next reap frees bytes.
        drop(slow_query_arc);
        mgr.reap_once();
        assert_eq!(
            mgr.admission.lock().retained_total_bytes(),
            0,
            "reaper must free retained entry once slow query releases",
        );
    }

    #[test]
    fn get_or_load_state_cas_rejects_concurrent_load() {
        // Regression for Codex Task 6 Phase 6b iter-1 MAJOR #2:
        // two loaders must not both run the slow path. The state
        // CAS gates exactly one winner.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let ws = mgr.get_or_insert_workspace(&key);
        // Simulate another loader holding the gate.
        ws.store_state(WorkspaceState::Loading);

        let builder = super::super::builder::EmptyGraphBuilder;
        let err = mgr
            .get_or_load(&key, &builder, 1_000)
            .expect_err("concurrent load must be rejected");
        match err {
            DaemonError::WorkspaceBuildFailed { reason, .. } => {
                assert!(
                    reason.contains("already in progress"),
                    "unexpected reason: {reason}",
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // Restore state so Drop order is clean; sanity-check that
        // the admission state was not mutated by the rejected call.
        assert_eq!(mgr.admission.lock().reserved_bytes, 0);
    }

    #[test]
    fn get_or_load_detects_cancellation_between_cas_and_publish() {
        // Regression for Codex Task 6 Phase 6b iter-1 MAJOR #2
        // (cancellation-detection subcase): if rebuild_cancelled was
        // set before our CAS — i.e. evict raced in front of us on
        // the prior state — get_or_load must honour the signal
        // instead of clobbering it and publishing into an evicted
        // workspace.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let ws = mgr.get_or_insert_workspace(&key);
        // Simulate "evict ran on an earlier state but left the
        // workspace in the map": cancellation flag set, state
        // Unloaded (so CAS succeeds).
        ws.rebuild_cancelled.store(true, Ordering::Release);
        ws.store_state(WorkspaceState::Unloaded);

        let builder = super::super::builder::EmptyGraphBuilder;
        let err = mgr
            .get_or_load(&key, &builder, 1_000)
            .expect_err("pre-CAS cancellation must be honoured");
        match err {
            DaemonError::WorkspaceBuildFailed { reason, .. } => {
                assert!(
                    reason.contains("evicted mid-load"),
                    "unexpected reason: {reason}",
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // rebuild_cancelled must still be true (we didn't clobber).
        assert!(ws.rebuild_cancelled.load(Ordering::Acquire));
        assert_eq!(ws.load_state(), WorkspaceState::Failed);
    }

    #[test]
    fn get_or_load_loading_guard_recovers_from_builder_panic() {
        // Regression for Codex Task 6 Phase 6b iter-1 MAJOR #3:
        // a panic from builder.build must not leave the workspace
        // stuck in Loading with last_error unset.
        use std::panic::{AssertUnwindSafe, catch_unwind};

        #[derive(Debug)]
        struct PanickingBuilder;
        impl WorkspaceBuilder for PanickingBuilder {
            fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
                panic!("simulated builder panic");
            }
        }

        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let builder = PanickingBuilder;

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = mgr.get_or_load(&key, &builder, 1_000);
        }));
        assert!(outcome.is_err(), "panic must propagate through get_or_load");

        let workspaces = mgr.workspaces.read();
        let ws = workspaces.get(&key).expect("workspace still registered");
        assert_eq!(
            ws.load_state(),
            WorkspaceState::Failed,
            "LoadingGuard must transition Loading → Failed on unwind",
        );
        assert!(
            ws.last_error.read().is_some(),
            "LoadingGuard must populate last_error on unwind",
        );
        assert!(
            ws.retry_count.load(Ordering::Acquire) >= 1,
            "LoadingGuard must increment retry_count",
        );
        drop(workspaces);

        // Admission: the RebuildReservation Drop on unwind refunds
        // reserved_bytes, so the state is clean.
        assert_eq!(mgr.admission.lock().reserved_bytes, 0);
    }

    #[test]
    fn concurrent_load_and_evict_never_publishes_into_evicted_workspace() {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Regression for Codex Task 6 Phase 6b iter-2 MAJOR:
        // the post-build re-check was not atomic with
        // `publish_and_retain`. A concurrent eviction could slip
        // in between the re-check and the publish, so we'd end
        // up accounting bytes for an evicted workspace.
        //
        // Stress test: run many iterations of `get_or_load` and
        // `execute_eviction` concurrently; every iteration
        // should leave the admission state consistent (§G.5),
        // the workspace either fully loaded or fully evicted,
        // and never in a half-committed "loaded_bytes points at
        // a graph that isn't in the map" state.
        use std::sync::Barrier;
        use std::thread;

        const ITERATIONS: usize = 64;
        for iter in 0..ITERATIONS {
            let mgr = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig {
                memory_limit_mb: 64,
                ..DaemonConfig::default()
            }));
            let key = make_key_at("/repos/example", iter as u64);
            let builder = Arc::new(super::super::builder::EmptyGraphBuilder);

            let start = Arc::new(Barrier::new(2));
            let mgr_clone = Arc::clone(&mgr);
            let key_clone = key.clone();
            let builder_clone = Arc::clone(&builder);
            let start_load = Arc::clone(&start);
            let loader = thread::spawn(move || {
                start_load.wait();
                // Intentionally ignore the result — either success
                // or failure is valid; we assert post-hoc invariants.
                let _ = mgr_clone.get_or_load(&key_clone, &*builder_clone, 100_000);
            });

            let mgr_clone = Arc::clone(&mgr);
            let key_clone = key.clone();
            let start_evict = Arc::clone(&start);
            let evictor = thread::spawn(move || {
                start_evict.wait();
                // Run unload against the same key; either it races
                // ahead of the loader (no-op), or evicts after the
                // loader publishes.
                mgr_clone.unload(&key_clone);
            });

            loader.join().expect("loader panicked");
            evictor.join().expect("evictor panicked");

            // Post-hoc invariants:
            // 1. The workspace is either Loaded AND in the map, or
            //    not in the map at all. No "evicted-but-in-map"
            //    intermediate state.
            // 2. Admission state is consistent: loaded_bytes +
            //    reserved_bytes + retained_total is whatever it is,
            //    but reserved_bytes must be zero (no in-flight
            //    reservations) and the invariant must hold as
            //    evidenced by positive counters.
            let workspaces = mgr.workspaces.read();
            if let Some(ws) = workspaces.get(&key) {
                assert_eq!(
                    ws.load_state(),
                    WorkspaceState::Loaded,
                    "iter {iter}: workspace in map must be Loaded, not {}",
                    ws.load_state(),
                );
            }
            drop(workspaces);

            let state = mgr.admission.lock();
            assert_eq!(
                state.reserved_bytes, 0,
                "iter {iter}: no reservations should leak after the race"
            );
            // §G.5 is intrinsically maintained by the arithmetic
            // operations; assert the totals are non-negative and
            // fit the budget.
            assert!(
                state.total_committed_bytes() <= mgr.memory_limit_bytes(),
                "iter {iter}: total_committed {} over budget {}",
                state.total_committed_bytes(),
                mgr.memory_limit_bytes(),
            );
        }
    }

    #[test]
    fn publish_fires_installed_hook() {
        // Phase 6c iter-2: `get_or_load` must invoke the installed
        // SqrydHook once the admission commit succeeds AND after
        // releasing `workspaces_guard`. This test drives the full
        // load path end-to-end so the fix (moving the hook out of
        // `publish_and_retain` and into the caller, outside every
        // workspaces-lock holder) is exercised — not just the raw
        // `publish_and_retain` critical section.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let hook = super::super::hook::RecordingHook::new();
        mgr.set_hook(Arc::clone(&hook) as super::super::hook::SharedHook);

        let key = make_key_at("/repos/example", 0x1);
        let builder = super::super::builder::EmptyGraphBuilder;
        mgr.get_or_load(&key, &builder, 0)
            .expect("load on empty builder succeeds");

        assert_eq!(
            hook.invocation_count(),
            1,
            "hook must fire exactly once per publish",
        );
        assert_eq!(
            hook.invocation_roots(),
            vec![key.source_root.clone()],
            "hook must receive the workspace's index_root",
        );
    }

    #[test]
    fn set_hook_replaces_prior_hook_for_subsequent_publishes() {
        // Phase 6c iter-2: install hook A, load, evict, install
        // hook B, load again. Hook A sees one invocation; hook B
        // sees one. Driving through `get_or_load` exercises the
        // post-`workspaces_guard`-drop dispatch path the iter-2
        // fix added.
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let hook_a = super::super::hook::RecordingHook::new();
        let hook_b = super::super::hook::RecordingHook::new();
        let builder = super::super::builder::EmptyGraphBuilder;
        let key = make_key_at("/repos/example", 0x1);

        mgr.set_hook(Arc::clone(&hook_a) as super::super::hook::SharedHook);
        mgr.get_or_load(&key, &builder, 0)
            .expect("first load with hook A");

        // Evict so the next `get_or_load` rebuilds and re-publishes
        // rather than hitting the Loaded-state cache fast path.
        mgr.unload(&key);

        mgr.set_hook(Arc::clone(&hook_b) as super::super::hook::SharedHook);
        mgr.get_or_load(&key, &builder, 0)
            .expect("second load with hook B");

        assert_eq!(hook_a.invocation_count(), 1);
        assert_eq!(hook_b.invocation_count(), 1);
    }

    #[test]
    fn hook_can_call_manager_unload_without_deadlock() {
        // Regression for Codex Task 6 Phase 6c iter-1 MAJOR: the
        // hook must fire OUTSIDE the `workspaces.read()` guard
        // that `get_or_load` holds across `publish_and_retain`,
        // so a hook impl that calls back into `manager.unload(key)`
        // — which acquires `workspaces.write()` inside
        // `execute_eviction` — must NOT deadlock against the
        // loader that fired it.
        //
        // Pre-fix: the hook dispatched from inside
        // `publish_and_retain` under the caller's
        // `workspaces.read()` guard, so the re-entrant
        // `workspaces.write()` in `unload` would block forever.
        //
        // We run the load on a background thread and fail the
        // test if the thread is still alive after a generous
        // timeout — that turns any deadlock regression into a
        // deterministic failure rather than a stuck runner.
        use std::{sync::Weak, thread, time::Duration};

        #[derive(Debug)]
        struct UnloadingHook {
            manager: Weak<WorkspaceManager>,
            key: WorkspaceKey,
        }

        impl super::super::hook::SqrydHook for UnloadingHook {
            fn on_publish(&self, _workspace_root: &Path, _graph: Arc<CodeGraph>) {
                if let Some(mgr) = self.manager.upgrade() {
                    // If the iter-2 fix regressed and this fires
                    // under `workspaces.read()`, the `.write()`
                    // inside `execute_eviction` deadlocks here
                    // and the test's join timeout triggers below.
                    let _present = mgr.unload(&self.key);
                }
            }
        }

        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/example", 0x1);
        let builder = super::super::builder::EmptyGraphBuilder;
        let hook = Arc::new(UnloadingHook {
            manager: Arc::downgrade(&mgr),
            key: key.clone(),
        });
        mgr.set_hook(Arc::clone(&hook) as super::super::hook::SharedHook);

        let mgr_for_thread = Arc::clone(&mgr);
        let key_for_thread = key.clone();
        let builder_for_thread = builder;
        let handle = thread::spawn(move || {
            mgr_for_thread
                .get_or_load(&key_for_thread, &builder_for_thread, 0)
                .expect("load succeeds even with re-entrant hook");
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !handle.is_finished() {
            if std::time::Instant::now() > deadline {
                panic!(
                    "get_or_load deadlocked while firing hook \
                     (Codex Task 6 Phase 6c iter-2 regression: \
                     hook must dispatch outside workspaces.read())",
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        handle
            .join()
            .expect("loader thread completed without panic");

        // Hook's `unload` ran, so the workspace must no longer be
        // in the manager map.
        assert!(
            !mgr.workspaces.read().contains_key(&key),
            "hook's re-entrant unload must have removed the workspace",
        );
        // And the hook observation: it fired exactly once.
        // (The hook itself doesn't record invocations; the
        // absence-of-workspace assertion above is the positive
        // signal that `on_publish` ran to completion.)
    }

    #[tokio::test]
    async fn retention_reaper_task_eventually_drops_free_entries() {
        let mgr = WorkspaceManager::new(&make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 0)
            .expect("zero-size reservation always fits");
        mgr.publish_and_retain(reservation, &ws, BuiltGraph::empty_fast_path())
            .expect("publish_and_retain succeeds within memory budget");
        assert_eq!(mgr.admission.lock().retained_old.len(), 1);

        // Reaper ticks every 25 ms; 200 ms is generous.
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if mgr.admission.lock().retained_old.is_empty() {
                return;
            }
        }
        panic!("reaper task never freed the entry within 200 ms");
    }

    // -----------------------------------------------------------------
    // Cluster-G §3.2 — `WorkspaceManager::reset` tests
    // -----------------------------------------------------------------

    /// Resetting an unregistered workspace returns `Ok(false)` and is
    /// a no-op.
    #[test]
    fn reset_returns_false_when_workspace_absent() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        let reset = mgr.reset(&key, false).expect("reset must succeed");
        assert!(!reset, "absent workspace should report `false`");
    }

    /// Resetting a `Loaded` workspace transitions it to `Unloaded` and
    /// preserves the manager-map entry.
    #[test]
    fn reset_loaded_workspace_preserves_entry() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        register_workspace(&mgr, &key);
        // Force the workspace into Loaded for the test.
        if let Some(ws) = mgr.workspaces.read().get(&key).cloned() {
            ws.store_state(crate::workspace::state::WorkspaceState::Loaded);
        }

        let reset = mgr
            .reset(&key, false)
            .expect("reset must succeed for Loaded workspace");
        assert!(reset, "present workspace should report `true`");
        assert!(
            mgr.workspaces.read().contains_key(&key),
            "reset must preserve the manager-map entry"
        );
    }

    /// Resetting a `pinned` workspace without `force` returns
    /// `WorkspacePinned` and leaves the entry alone.
    #[test]
    fn reset_pinned_without_force_returns_pinned_error() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        // Insert a pinned workspace directly.
        mgr.workspaces.write().insert(
            key.clone(),
            Arc::new(LoadedWorkspace::new(key.clone(), true)),
        );
        let err = mgr
            .reset(&key, false)
            .expect_err("pinned workspace must reject reset without force");
        assert!(
            matches!(err, crate::error::DaemonError::WorkspacePinned { .. }),
            "expected WorkspacePinned, got {err:?}"
        );
    }

    /// `force = true` allows resetting a `pinned` workspace.
    #[test]
    fn reset_pinned_with_force_succeeds() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        mgr.workspaces.write().insert(
            key.clone(),
            Arc::new(LoadedWorkspace::new(key.clone(), true)),
        );
        let reset = mgr
            .reset(&key, true)
            .expect("force-reset must succeed for pinned workspace");
        assert!(reset);
    }

    /// Cluster-G iter-2 BLOCKER 1 regression: after a successful
    /// `reset`, `rebuild_cancelled` MUST be cleared so the next
    /// `get_or_load` does not hit the `pre_cancelled && prior_state
    /// != Evicted` branch and surface `WorkspaceBuildFailed`. Codex
    /// iter-1 review flagged that `evict_to_tombstone_locked` set
    /// the flag and `reset` never cleared it, leaving `daemon reset
    /// → daemon load` permanently broken.
    #[test]
    fn reset_clears_rebuild_cancelled_so_next_load_does_not_fail() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        );
        register_workspace(&mgr, &key);
        if let Some(ws) = mgr.workspaces.read().get(&key).cloned() {
            ws.store_state(crate::workspace::state::WorkspaceState::Loaded);
        }
        let _ = mgr.reset(&key, false).expect("reset must succeed");
        let ws = mgr
            .workspaces
            .read()
            .get(&key)
            .cloned()
            .expect("entry preserved");
        assert!(
            !ws.rebuild_cancelled.load(Ordering::Acquire),
            "rebuild_cancelled must be CLEARED after reset; otherwise the next \
             get_or_load fails with WorkspaceBuildFailed and `daemon reset` is broken"
        );
    }

    fn anonymous_keys_for_same_root() -> (PathBuf, WorkspaceKey, WorkspaceKey, WorkspaceKey) {
        let root = PathBuf::from("/repos/same-path");
        let key1 = WorkspaceKey::new(root.clone(), ProjectRootMode::WorkspaceFolder, 0);
        let key2 = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let key3 = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 99);
        (root, key1, key2, key3)
    }

    fn status_count_for_root(mgr: &WorkspaceManager, root: &Path) -> usize {
        mgr.status()
            .workspaces
            .iter()
            .filter(|workspace_status| workspace_status.index_root == root)
            .count()
    }

    fn reset_all_for_source_root(mgr: &Arc<WorkspaceManager>, root: &Path) -> bool {
        let mut reset_any = false;
        for (candidate_key, _) in mgr.find_all_by_source_root(root) {
            if mgr
                .reset(&candidate_key, false)
                .expect("path candidate reset must succeed")
            {
                reset_any = true;
            }
        }
        reset_any
    }

    /// Regression for #393: clean anonymous loads that arrive with
    /// differing secondary key fields for the same source_root coalesce
    /// to the first registered workspace instead of inserting a second
    /// map entry.
    #[test]
    fn get_or_insert_coalesces_anonymous_keys_with_same_source_root() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let (root, key1, key2, _) = anonymous_keys_for_same_root();
        let ws1 = mgr.get_or_insert_workspace(&key1);
        ws1.store_state(WorkspaceState::Loaded);
        let ws2 = mgr.get_or_insert_workspace(&key2);

        assert!(
            Arc::ptr_eq(&ws1, &ws2),
            "coalesce must return the exact same Arc<LoadedWorkspace> instance"
        );
        assert_eq!(
            mgr.find_all_by_source_root(&root).len(),
            1,
            "clean anonymous loads must leave one map entry for the source_root"
        );
        assert_eq!(
            status_count_for_root(&mgr, &root),
            1,
            "status must list the clean coalesced source_root once"
        );
    }

    /// After a reset leaves the coalesced workspace as an `Unloaded`
    /// tombstone, divergent anonymous callers must still find that
    /// registered entry. In particular, `reserve_rebuild` must not
    /// report `WorkspaceEvicted` merely because the caller constructed a
    /// different secondary key.
    #[test]
    fn divergent_anonymous_key_recovers_after_reset_without_workspace_evicted() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let (root, key1, key2, key3) = anonymous_keys_for_same_root();
        let ws1 = mgr.get_or_insert_workspace(&key1);
        ws1.store_state(WorkspaceState::Loaded);
        let did_reset = mgr.reset(&key1, false).expect("single reset must succeed");
        assert!(did_reset, "reset of the coalesced entry must report true");

        let ws3 = mgr.get_or_insert_workspace(&key3);
        assert!(
            Arc::ptr_eq(&ws1, &ws3),
            "post-reset divergent anon key must still coalesce to the registered ws"
        );
        assert_eq!(
            ws3.load_state(),
            WorkspaceState::Unloaded,
            "coalesced workspace after reset must be Unloaded and ready for reload"
        );
        let res = mgr.reserve_rebuild(&key2, 0);
        assert!(
            res.is_ok(),
            "reserve_rebuild with divergent anon key post-reset/coalesce must not hit WorkspaceEvicted: {:?}",
            res.err()
        );
        assert_eq!(
            mgr.find_all_by_source_root(&root).len(),
            1,
            "divergent post-reset access must not insert a second anonymous entry"
        );
    }

    /// Historical duplicate anonymous entries must be recoverable by the
    /// reset handler's path-level fan-out. Reset preserves tombstone
    /// entries by design, but none may remain Loaded afterward.
    #[test]
    fn path_reset_clears_historical_anonymous_duplicates() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let (root, key1, key2, _) = anonymous_keys_for_same_root();
        let ws1 = Arc::new(LoadedWorkspace::new(key1.clone(), false));
        ws1.store_state(WorkspaceState::Loaded);
        let legacy_duplicate = Arc::new(LoadedWorkspace::new(key2.clone(), false));
        legacy_duplicate.store_state(WorkspaceState::Loaded);
        {
            let mut workspaces = mgr.workspaces.write();
            workspaces.insert(key1, ws1);
            workspaces.insert(key2, legacy_duplicate);
        }

        let path_candidates = mgr.find_all_by_source_root(&root);
        assert_eq!(
            path_candidates.len(),
            2,
            "path finder used by daemon/reset must return every same-source_root entry"
        );
        assert!(
            path_candidates
                .iter()
                .all(|(_, ws)| ws.load_state() == WorkspaceState::Loaded),
            "both same-source_root entries start Loaded before path reset"
        );

        assert!(
            reset_all_for_source_root(&mgr, &root),
            "handler-style path reset must report reset: true"
        );
        let path_candidates_after_reset = mgr.find_all_by_source_root(&root);
        assert_eq!(
            path_candidates_after_reset.len(),
            2,
            "reset preserves tombstone entries for historical duplicates"
        );
        assert!(
            path_candidates_after_reset
                .iter()
                .all(|(_, ws)| ws.load_state() == WorkspaceState::Unloaded),
            "path reset must leave no same-source_root entry Loaded"
        );
    }

    /// Status is the user-visible surface from #393. Even when a
    /// historical duplicate is still present as a tombstone, anonymous
    /// rows with the same index_root are collapsed to one deterministic
    /// status row.
    #[test]
    fn status_does_not_emit_duplicate_anonymous_rows_for_historical_duplicates() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let (root, key1, key2, _) = anonymous_keys_for_same_root();
        mgr.workspaces.write().insert(
            key1.clone(),
            Arc::new(LoadedWorkspace::new(key1.clone(), false)),
        );
        mgr.workspaces.write().insert(
            key2.clone(),
            Arc::new(LoadedWorkspace::new(key2.clone(), false)),
        );

        assert_eq!(
            mgr.find_all_by_source_root(&root).len(),
            2,
            "test setup must contain the historical duplicate entries"
        );
        assert_eq!(
            status_count_for_root(&mgr, &root),
            1,
            "daemon status must collapse duplicate anonymous index_root rows"
        );
    }

    /// Historical duplicate winner selection must not rely on
    /// `HashMap::iter()` order. Anonymous same-root lookups use the
    /// stable key ordering, so the GitRoot/0 entry wins over the
    /// WorkspaceFolder/0 entry regardless of insertion order.
    #[test]
    fn historical_anonymous_duplicate_winner_is_deterministic() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let (root, key1, key2, key3) = anonymous_keys_for_same_root();
        let workspace_folder_ws = Arc::new(LoadedWorkspace::new(key1.clone(), false));
        let git_root_ws = Arc::new(LoadedWorkspace::new(key2.clone(), false));
        // The fixture stands for two published `Loaded` workspaces, so each
        // carries the record every publish path publishes beside the graph
        // (the load gate refuses a `Loaded` slot without one, design D24).
        for ws in [&workspace_folder_ws, &git_root_ws] {
            ws.published.store(Arc::new(PublishedGraph::new(
                ws.graph(),
                Some(Arc::new(RosterRecord::fast_path_default())),
            )));
            ws.store_state(WorkspaceState::Loaded);
        }
        {
            let mut workspaces = mgr.workspaces.write();
            workspaces.insert(key1.clone(), Arc::clone(&workspace_folder_ws));
            workspaces.insert(key2, Arc::clone(&git_root_ws));
        }

        let workspace_folder_last_accessed = *workspace_folder_ws.last_accessed.read();
        let git_root_last_accessed = *git_root_ws.last_accessed.read();
        std::thread::sleep(Duration::from_millis(1));
        let builder = super::super::builder::EmptyGraphBuilder;
        mgr.get_or_load(&key1, &builder, 0)
            .expect("loaded historical winner should be returned from cache");
        assert_eq!(
            *workspace_folder_ws.last_accessed.read(),
            workspace_folder_last_accessed,
            "get_or_load must not touch the exact non-winner historical duplicate"
        );
        assert!(
            *git_root_ws.last_accessed.read() > git_root_last_accessed,
            "get_or_load must use the deterministic anonymous winner even when the caller key exactly matches a non-winner duplicate"
        );

        let selected = mgr.get_or_insert_workspace(&key3);
        assert!(
            Arc::ptr_eq(&selected, &git_root_ws),
            "deterministic anonymous winner should be the stable minimum key"
        );
        let first_candidate = mgr
            .find_all_by_source_root(&root)
            .into_iter()
            .next()
            .expect("historical duplicate candidates present");
        assert_eq!(
            first_candidate.0.root_mode,
            ProjectRootMode::GitRoot,
            "path candidate ordering must expose the same deterministic winner first"
        );
    }

    /// T58 (surface parity W1 round 7, design D37; R7-0's class in
    /// `LoadingGuard::drop`). `armed` is true exactly while the
    /// `Loading` gate this task won is unreleased, which is an
    /// observation made two operations before the store it justified.
    /// An eviction that completed in between owns the state, so the
    /// guard must leave the tombstone alone.
    ///
    /// Leg two is the control that the compare-exchange did not disable
    /// the guard: from `Loading`, which is the state the guard is armed
    /// for, it still transitions to `Failed`.
    ///
    /// Row: C74, the eleven load-path `transition_state(Loading,
    /// Failed)` calls reverted to `store_state(WorkspaceState::Failed)`.
    #[test]
    fn loading_guard_drop_leaves_a_completed_eviction_evicted() {
        // Leg 1: the tombstone. The guard is armed, as it is on every
        // early return and every panic between the gate and the
        // publish, but an eviction has completed since.
        let tombstoned = make_workspace();
        let key = make_key_at("/repos/w1r7-guard-tombstone", 0x1);
        tombstoned.store_state(WorkspaceState::Evicted);
        assert!(
            tombstoned.roster().is_none(),
            "the placeholder generation carries no roster record"
        );
        {
            let _guard = LoadingGuard {
                ws: &tombstoned,
                key: &key,
                armed: true,
            };
        }
        let tombstone_state = tombstoned.load_state();
        let tombstone_record = tombstoned.roster().is_some();

        // Leg 2: the state the guard IS armed for. It must still fire.
        let loading = make_workspace();
        let loading_key = make_key_at("/repos/w1r7-guard-loading", 0x2);
        loading.store_state(WorkspaceState::Loading);
        {
            let _guard = LoadingGuard {
                ws: &loading,
                key: &loading_key,
                armed: true,
            };
        }
        let loading_state = loading.load_state();
        let loading_error = loading.last_error.read().is_some();
        let loading_retries = loading.retry_count.load(Ordering::Acquire);

        println!(
            "R7-0 load-gate guard: tombstone=({tombstone_state},{tombstone_record}) \
             loading=({loading_state},{loading_error},{loading_retries})"
        );
        assert_eq!(
            tombstone_state,
            WorkspaceState::Evicted,
            "an armed guard must not relabel a completed tombstone as Failed"
        );
        assert!(
            !tombstone_record,
            "nothing on this path publishes a roster record"
        );
        assert_eq!(
            loading_state,
            WorkspaceState::Failed,
            "the guard must still transition the gate it was armed for"
        );
        assert!(
            loading_error,
            "the guard's diagnostic write is unconditional"
        );
        assert_eq!(
            loading_retries, 1,
            "the guard counted exactly one failed attempt"
        );
    }

    /// T60 (surface parity W1 round 7, design D37; R7-0's class on the
    /// load path). A declared control: the `ObservationPhase`
    /// `GateAcquired` variant leg 0 plants at does not exist on the
    /// pre-change head, so there is no red to show and the row is the
    /// measurement.
    ///
    /// Three legs, each a load that fails after an eviction has
    /// completed underneath it, and each reaching a different arm:
    ///
    /// - Leg 0 plants at `GateAcquired`, the phase added this round,
    ///   fired after the `Loading` gate is won and
    ///   `honor_preexisting_cancel` has answered `Ok`, with no manager
    ///   lock held. The plant's own outcome is asserted `Evicted`, which
    ///   is what establishes that no lock is held there; the load then
    ///   ends at `reserve_rebuild`'s Phase-1 cancellation check and the
    ///   arm under test is `LoadingGuard::drop`'s.
    /// - Leg A evicts from inside the builder double, which the manager
    ///   calls with no lock held and after the reservation, so the load
    ///   reaches `get_or_load_published`'s cancelled-recheck arm.
    /// - Leg B does the same and then returns an error, so the load
    ///   reaches the builder-error arm.
    ///
    /// Why the builder and not `GateAcquired` for legs A and B, recorded
    /// because the design named `GateAcquired` for both: an eviction at
    /// that phase sets `rebuild_cancelled`, and `reserve_rebuild`'s
    /// Phase-1 check reads that flag under `workspaces.read()` and
    /// refuses before the builder runs, so neither of those two arms is
    /// reachable from there. The builder double is the seam that
    /// reaches them, and it is a production call site, not a test hook.
    ///
    /// All three legs must leave the slot exactly as the eviction left
    /// it and must return the typed error they return today.
    ///
    /// Row: C74, the eleven load-path `transition_state(Loading,
    /// Failed)` calls reverted to `store_state(WorkspaceState::Failed)`.
    /// Under it every leg fails with left Failed right Evicted.
    #[test]
    fn a_load_that_fails_after_an_eviction_leaves_the_tombstone() {
        /// A builder that completes an eviction of `key` from inside
        /// `build`, records the outcome, and then answers `outcome_ok`.
        #[derive(Debug)]
        struct EvictingBuilder {
            manager: Weak<WorkspaceManager>,
            key: WorkspaceKey,
            evicted: Mutex<Option<TryEvictOutcome>>,
            build_succeeds: bool,
        }

        impl WorkspaceBuilder for EvictingBuilder {
            fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
                if let Some(manager) = self.manager.upgrade() {
                    *self.evicted.lock() = Some(manager.try_evict_for_test(&self.key));
                }
                if self.build_succeeds {
                    Ok(BuiltGraph::empty_fast_path())
                } else {
                    Err(DaemonError::WorkspaceBuildFailed {
                        root: workspace_root.to_path_buf(),
                        reason: "the double refuses to build".to_string(),
                    })
                }
            }
        }

        // ---- Leg 0: the GateAcquired seam -------------------------
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r7-gate-leg-0", 0x1);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Evicted);
        let ws0 = mgr.lookup(&key).expect("the seeded workspace is resident");
        let seam_outcome: Arc<Mutex<Option<TryEvictOutcome>>> = Arc::new(Mutex::new(None));
        let seam_fired = Arc::new(AtomicU64::new(0));
        {
            let weak: Weak<WorkspaceManager> = Arc::downgrade(&mgr);
            let plant_key = key.clone();
            let plant_outcome = Arc::clone(&seam_outcome);
            let plant_fired = Arc::clone(&seam_fired);
            mgr.install_observation_plant_for_test(Arc::new(move |phase| {
                if phase != ObservationPhase::GateAcquired {
                    return;
                }
                if plant_fired.fetch_add(1, Ordering::AcqRel) != 0 {
                    return;
                }
                if let Some(mgr) = weak.upgrade() {
                    *plant_outcome.lock() = Some(mgr.try_evict_for_test(&plant_key));
                }
            }));
        }
        let result_0 = mgr.get_or_load(&key, &super::super::builder::EmptyGraphBuilder, 1_000);
        let seam_recorded = *seam_outcome.lock();
        let seam_count = seam_fired.load(Ordering::Acquire);
        let state_0 = ws0.load_state();
        let record_0 = ws0.roster().is_some();

        // ---- Legs A and B: the two arms inside the load -----------
        let run_builder_leg = |path: &str,
                               fingerprint: u64,
                               build_succeeds: bool|
         -> (
            Option<TryEvictOutcome>,
            Result<Arc<CodeGraph>, DaemonError>,
            WorkspaceState,
            bool,
        ) {
            let mgr = WorkspaceManager::new_without_reaper(make_config());
            let key = make_key_at(path, fingerprint);
            // `Unloaded` is the cold-load state the gate CASes from, so
            // the eviction under test is the builder's, not a seeded one.
            mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
            let ws = mgr.lookup(&key).expect("the seeded workspace is resident");
            let builder = EvictingBuilder {
                manager: Arc::downgrade(&mgr),
                key: key.clone(),
                evicted: Mutex::new(None),
                build_succeeds,
            };
            let result = mgr.get_or_load(&key, &builder, 1_000);
            let evicted = *builder.evicted.lock();
            (evicted, result, ws.load_state(), ws.roster().is_some())
        };

        let (evicted_a, result_a, state_a, record_a) =
            run_builder_leg("/repos/w1r7-gate-leg-a", 0xA, true);
        let (evicted_b, result_b, state_b, record_b) =
            run_builder_leg("/repos/w1r7-gate-leg-b", 0xB, false);

        println!(
            "R7-0 load gate: leg0=(plant={seam_recorded:?},fired={seam_count},\
             err={},state={state_0},record={record_0}) \
             legA=(evict={evicted_a:?},err={},state={state_a},record={record_a}) \
             legB=(evict={evicted_b:?},err={},state={state_b},record={record_b})",
            result_0.is_err(),
            result_a.is_err(),
            result_b.is_err()
        );

        // The seams fired and did what the legs rest on.
        assert_eq!(
            seam_recorded,
            Some(TryEvictOutcome::Evicted),
            "GateAcquired must hold no manager lock, so the plant's eviction completes there"
        );
        assert_eq!(seam_count, 1, "GateAcquired fired exactly once");
        assert_eq!(
            evicted_a,
            Some(TryEvictOutcome::Evicted),
            "leg A's builder must have completed an eviction before it returned"
        );
        assert_eq!(
            evicted_b,
            Some(TryEvictOutcome::Evicted),
            "leg B's builder must have completed an eviction before it returned"
        );

        // Each leg returns the typed error its arm returns today.
        match &result_0 {
            Err(DaemonError::WorkspaceEvicted { root }) => {
                assert_eq!(root, &key.source_root, "leg 0 names the evicted root");
            }
            other => panic!("leg 0 returned the wrong result: {other:?}"),
        }
        match &result_a {
            Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => assert!(
                reason.contains("evicted mid-load"),
                "leg A must return the cancelled-recheck refusal: {reason}"
            ),
            other => panic!("leg A returned the wrong result: {other:?}"),
        }
        match &result_b {
            Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => assert!(
                reason.contains("the double refuses to build"),
                "leg B must return the builder's own error: {reason}"
            ),
            other => panic!("leg B returned the wrong result: {other:?}"),
        }

        // And none of them relabels the tombstone.
        assert_eq!(
            (state_0, record_0),
            (WorkspaceState::Evicted, false),
            "leg 0 must leave the slot as the eviction left it"
        );
        assert_eq!(
            (state_a, record_a),
            (WorkspaceState::Evicted, false),
            "leg A must leave the slot as the eviction left it"
        );
        assert_eq!(
            (state_b, record_b),
            (WorkspaceState::Evicted, false),
            "leg B must leave the slot as the eviction left it"
        );
    }

    // --- Surface parity W1 round 8 (design D41): each load-gate failure arm
    // observed alone ------------------------------------------------------
    //
    // Round 7's C74 reverts the eleven `transition_state(Loading, Failed)`
    // arms together, so it shows only that one of them is observed. The
    // tests below plant, through production calls only, the one condition
    // under which a single arm's compare-exchange differs from the
    // unconditional store it replaced: the state is no longer `Loading` when
    // the arm stores. Each asserts that its planted condition happened (the
    // eviction outcome, `unload`, the flag swap, `reset`, the error only its
    // arm produces) before it asserts the state, so a green cannot mean the
    // arm was never reached. Each is green on the pre-change head by
    // construction (D37's compare-exchange is already there) and red under
    // its own row's plant.

    /// What [`ArmDouble`] does inside `build` or `load_persisted` before it
    /// answers. The manager calls both entry points after the reservation
    /// and outside every `workspaces` guard, so each action runs with no
    /// manager lock held, as a concurrent production caller would.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ArmAction {
        /// [`WorkspaceManager::try_evict_for_test`] on the double's key: the
        /// eviction body, run to completion.
        TryEvict,
        /// [`WorkspaceManager::unload`] on the double's key.
        Unload,
        /// `rebuild_cancelled.swap(false)` on the double's held workspace,
        /// standing for the runner's top-of-loop swap in
        /// `RebuildDispatcher::handle_changes_inner`: a production holder of
        /// the same `Arc` that consumes the flag and installs no state.
        ConsumeCancelFlag,
        /// [`WorkspaceManager::reset`] on the double's key, without force.
        Reset,
        /// A second loader of the same workspace, run to the point the first
        /// loader cannot see (surface parity W1 round 9, design D45):
        /// [`WorkspaceManager::enter_loading_state`] on the double's held
        /// workspace, then [`WorkspaceManager::honor_preexisting_cancel`] with
        /// the prior state that compare-exchange returned. Both are the
        /// production steps `prepare_load_gate` performs, in the order it
        /// performs them, and it holds no manager lock between them.
        SecondLoaderGate,
    }

    /// The answer one [`ArmAction`] gave, recorded so each test asserts that
    /// its planted condition happened.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ArmAnswer {
        TryEvict(TryEvictOutcome),
        Unload(bool),
        ConsumeCancelFlag(bool),
        Reset(Result<bool, String>),
        /// The prior state the second loader's compare-exchange answered,
        /// `honor_preexisting_cancel`'s answer rendered through
        /// [`build_failed_reason`], and the cancellation flag after it.
        SecondLoaderGate {
            prior: Option<WorkspaceState>,
            answer: String,
            flag: bool,
        },
    }

    /// The value [`ArmDouble`] answers with after its actions.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ArmResult {
        /// `BuiltGraph::empty_fast_path()`.
        EmptyGraph,
        /// `graph_with_nodes(1)` beside the fast-path record: a graph whose
        /// heap bytes exceed a zero memory limit.
        OneNodeGraph,
        /// [`graph_with_an_out_of_arena_edge`] beside the fast-path record: a
        /// graph `compact_edges_in_place` refuses.
        OutOfArenaEdge,
        /// `WorkspaceBuildFailed` carrying [`ARM_DOUBLE_REASON`].
        Refusal,
    }

    /// The reason [`ArmResult::Refusal`] carries, which no production arm
    /// produces.
    const ARM_DOUBLE_REASON: &str = "the round 8 double refuses to answer with a graph";

    /// The one builder double T62 to T68 share (design D41). `build` and
    /// `load_persisted` count their calls, run the configured actions in
    /// order and then answer with the configured result, so a test drives
    /// `get_or_load` or `reload_from_disk_read_only` into exactly one
    /// failure arm with the state already moved underneath the load.
    #[derive(Debug)]
    struct ArmDouble {
        manager: Weak<WorkspaceManager>,
        key: WorkspaceKey,
        workspace: Arc<LoadedWorkspace>,
        actions: Vec<ArmAction>,
        result: ArmResult,
        build_calls: AtomicU64,
        load_persisted_calls: AtomicU64,
        answers: Mutex<Vec<ArmAnswer>>,
    }

    impl ArmDouble {
        /// A double over the workspace resident at `key`, holding its `Arc`
        /// the way a production holder of the same workspace does.
        fn new(
            manager: &Arc<WorkspaceManager>,
            key: &WorkspaceKey,
            actions: &[ArmAction],
            result: ArmResult,
        ) -> Self {
            Self {
                manager: Arc::downgrade(manager),
                key: key.clone(),
                workspace: manager
                    .lookup(key)
                    .expect("the double's workspace is resident when the double is made"),
                actions: actions.to_vec(),
                result,
                build_calls: AtomicU64::new(0),
                load_persisted_calls: AtomicU64::new(0),
                answers: Mutex::new(Vec::new()),
            }
        }

        /// `(build calls, load_persisted calls)`.
        fn calls(&self) -> (u64, u64) {
            (
                self.build_calls.load(Ordering::Acquire),
                self.load_persisted_calls.load(Ordering::Acquire),
            )
        }

        fn answers(&self) -> Vec<ArmAnswer> {
            self.answers.lock().clone()
        }

        fn act_then_answer(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
            let manager = self
                .manager
                .upgrade()
                .expect("the manager outlives the load it runs");
            let answers: Vec<ArmAnswer> = self
                .actions
                .iter()
                .map(|action| match action {
                    ArmAction::TryEvict => {
                        ArmAnswer::TryEvict(manager.try_evict_for_test(&self.key))
                    }
                    ArmAction::Unload => ArmAnswer::Unload(manager.unload(&self.key)),
                    ArmAction::ConsumeCancelFlag => ArmAnswer::ConsumeCancelFlag(
                        self.workspace
                            .rebuild_cancelled
                            .swap(false, Ordering::AcqRel),
                    ),
                    ArmAction::Reset => ArmAnswer::Reset(
                        manager
                            .reset(&self.key, false)
                            .map_err(|err| err.to_string()),
                    ),
                    ArmAction::SecondLoaderGate => {
                        let prior = WorkspaceManager::enter_loading_state(&self.workspace);
                        let answered = WorkspaceManager::honor_preexisting_cancel(
                            &self.workspace,
                            &self.key,
                            prior.unwrap_or(WorkspaceState::Loading),
                        );
                        ArmAnswer::SecondLoaderGate {
                            prior,
                            answer: build_failed_reason(&answered),
                            flag: self.workspace.rebuild_cancelled.load(Ordering::Acquire),
                        }
                    }
                })
                .collect();
            *self.answers.lock() = answers;
            let fast_path = || Arc::new(RosterRecord::fast_path_default());
            match self.result {
                ArmResult::EmptyGraph => Ok(BuiltGraph::empty_fast_path()),
                ArmResult::OneNodeGraph => Ok(BuiltGraph::new(graph_with_nodes(1), fast_path())),
                ArmResult::OutOfArenaEdge => Ok(BuiltGraph::new(
                    graph_with_an_out_of_arena_edge(),
                    fast_path(),
                )),
                ArmResult::Refusal => Err(DaemonError::WorkspaceBuildFailed {
                    root: workspace_root.to_path_buf(),
                    reason: ARM_DOUBLE_REASON.to_string(),
                }),
            }
        }
    }

    impl WorkspaceBuilder for ArmDouble {
        fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
            self.build_calls.fetch_add(1, Ordering::AcqRel);
            self.act_then_answer(workspace_root)
        }

        fn load_persisted(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
            self.load_persisted_calls.fetch_add(1, Ordering::AcqRel);
            self.act_then_answer(workspace_root)
        }
    }

    /// One node from `graph_with_nodes(1)` and one `Calls` edge from it to a
    /// node index outside the arena. `compact_edges_in_place` builds a CSR
    /// per direction and `CsrGraph::validate` refuses the forward one
    /// (`ColIdxOutOfBounds`), so the compaction arm of
    /// `get_or_load_published` runs on this graph.
    fn graph_with_an_out_of_arena_edge() -> CodeGraph {
        use sqry_core::graph::unified::{EdgeKind, NodeId, ResolvedVia};

        let graph = graph_with_nodes(1);
        let (source, file) = {
            let (id, entry) = graph
                .nodes()
                .iter()
                .next()
                .expect("graph_with_nodes(1) allocates one node");
            (id, entry.file)
        };
        let outside = u32::try_from(graph.node_count()).expect("one node fits a u32") + 7;
        graph.edges().add_edge(
            source,
            NodeId::new(outside, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        graph
    }

    /// A manager whose memory limit is zero bytes, so any non-empty graph is
    /// refused by `publish_and_retain` with `WorkspaceOversize` while a
    /// reservation of zero bytes is still admitted.
    fn zero_limit_manager() -> Arc<WorkspaceManager> {
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig {
            memory_limit_mb: 0,
            ..DaemonConfig::default()
        }))
    }

    /// The reason of a `WorkspaceBuildFailed`, or a description of whatever
    /// else the load answered.
    fn build_failed_reason<T: std::fmt::Debug>(result: &Result<T, DaemonError>) -> String {
        match result {
            Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => reason.clone(),
            other => format!("not WorkspaceBuildFailed: {other:?}"),
        }
    }

    /// T61 (surface parity W1 round 8, design D41; section 2.1.8 row 8).
    /// `honor_preexisting_cancel`'s arm. `prepare_load_gate` holds no
    /// manager lock between `enter_loading_state`'s compare-exchange and
    /// this function, and `evict_lru` and `plan_eviction` skip only
    /// `Evicted` and `Unloaded`, so a `Loading` workspace can be evicted in
    /// that window. There is no observation seam there, so the test calls
    /// the gate's three production steps in the order the window allows,
    /// the shape T58 uses for `LoadingGuard::drop`: the compare-exchange,
    /// the eviction body run to completion, then this function with the
    /// prior state the compare-exchange returned.
    ///
    /// Leg 2 is the control: the same three steps without the eviction and
    /// with the flag set by the test must still end `Failed`.
    ///
    /// Row: C75, this arm alone reverted to
    /// `workspace.store_state(WorkspaceState::Failed)`. Under it leg 1 fails
    /// with left Failed right Evicted.
    #[test]
    fn honor_preexisting_cancel_leaves_a_completed_eviction_evicted() {
        // ---- Leg 1: an eviction completed inside the gate's window -----
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-honor-evicted", 0x61);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
        let ws = mgr.lookup(&key).expect("the seeded workspace is resident");
        let prior_1 = WorkspaceManager::enter_loading_state(&ws);
        let evicted_1 = mgr.try_evict_for_test(&key);
        let flag_planted_1 = ws.rebuild_cancelled.load(Ordering::Acquire);
        let result_1 = WorkspaceManager::honor_preexisting_cancel(
            &ws,
            &key,
            prior_1.unwrap_or(WorkspaceState::Loading),
        );
        let reason_1 = build_failed_reason(&result_1);
        let state_1 = ws.load_state();
        let record_1 = ws.roster().is_some();
        let flag_1 = ws.rebuild_cancelled.load(Ordering::Acquire);

        // ---- Leg 2 (control): the flag set, no eviction -----------------
        let mgr_2 = WorkspaceManager::new_without_reaper(make_config());
        let key_2 = make_key_at("/repos/w1r8-honor-control", 0x62);
        mgr_2.insert_workspace_in_state_for_test(key_2.clone(), WorkspaceState::Unloaded);
        let ws_2 = mgr_2
            .lookup(&key_2)
            .expect("the seeded workspace is resident");
        ws_2.rebuild_cancelled.store(true, Ordering::Release);
        let prior_2 = WorkspaceManager::enter_loading_state(&ws_2);
        let result_2 = WorkspaceManager::honor_preexisting_cancel(
            &ws_2,
            &key_2,
            prior_2.unwrap_or(WorkspaceState::Loading),
        );
        let reason_2 = build_failed_reason(&result_2);
        let state_2 = ws_2.load_state();
        let record_2 = ws_2.roster().is_some();
        let flag_2 = ws_2.rebuild_cancelled.load(Ordering::Acquire);

        println!(
            "R8-1 honor_preexisting_cancel: leg1=(prior={prior_1:?},evict={evicted_1:?},\
             flag_planted={flag_planted_1},reason={reason_1},state={state_1},record={record_1},\
             flag={flag_1}) leg2=(prior={prior_2:?},reason={reason_2},state={state_2},\
             record={record_2},flag={flag_2})"
        );

        // The planted condition happened.
        assert_eq!(
            prior_1,
            Some(WorkspaceState::Unloaded),
            "leg 1's compare-exchange must win from Unloaded"
        );
        assert_eq!(
            evicted_1,
            TryEvictOutcome::Evicted,
            "leg 1's eviction must complete inside the gate's window"
        );
        assert!(
            flag_planted_1,
            "the completed eviction must leave the cancellation flag set"
        );
        assert_eq!(
            reason_1, "workspace evicted mid-load",
            "leg 1 must return the arm's own refusal"
        );
        // And the arm left the tombstone alone.
        assert_eq!(
            state_1,
            WorkspaceState::Evicted,
            "honor_preexisting_cancel must not relabel a completed tombstone as Failed"
        );
        assert!(
            !record_1,
            "the tombstone carries the placeholder, no record"
        );
        assert!(flag_1, "the arm re-arms the cancellation flag it consumed");

        // The control: the arm still fires for the state it is for.
        assert_eq!(
            prior_2,
            Some(WorkspaceState::Unloaded),
            "leg 2's compare-exchange must win from Unloaded"
        );
        assert_eq!(
            reason_2, "workspace evicted mid-load",
            "leg 2 must return the arm's own refusal"
        );
        assert_eq!(
            state_2,
            WorkspaceState::Failed,
            "without an eviction the arm must still transition Loading to Failed"
        );
        assert!(record_2, "nothing on leg 2 replaces the seeded record");
        assert!(flag_2, "the arm re-arms the cancellation flag it consumed");
    }

    /// Round 8 review, note (a): an eviction while a rebuild runner holds
    /// the runner role, then a load of the tombstone. The gate consumed the
    /// eviction's cancellation for a load from `Evicted`, so the runner,
    /// which had not read the flag yet, published its graph into the slot
    /// the load then owned (`rebuild_abort_on_eviction.rs` drives that end
    /// to end). With the runner in flight the gate now refuses the load as
    /// in progress, puts the tombstone back and leaves the flag set for the
    /// runner. Two controls: once the runner has stopped, the same gate
    /// consumes the flag and the load owns the slot; and an eviction that
    /// found the slot `Loaded` (a runner between iterations, already
    /// published: `rebuild_guard.rs`'s
    /// `the_runner_is_answered_from_its_report_even_when_the_slot_moved_on`)
    /// is consumed as before although the runner holds the role.
    #[test]
    fn the_load_gate_leaves_an_eviction_to_the_rebuild_still_running() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/r8-gate-runner", 0x81);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        let ws = mgr.lookup(&key).expect("the seeded workspace is resident");
        // A runner holds the role and installed `Rebuilding`.
        ws.rebuild_in_flight.store(true, Ordering::Release);
        ws.store_state(WorkspaceState::Rebuilding);
        assert!(mgr.evict_for_test(&key), "the eviction finds the workspace");
        let evicted_state = ws.load_state();
        let evicted_flag = ws.rebuild_cancelled.load(Ordering::Acquire);

        let refused = match mgr.prepare_load_gate(&key) {
            Ok(LoadGate::Loaded(_)) => "Loaded".to_string(),
            Ok(LoadGate::Acquired { entry, .. }) => {
                format!("Acquired(prior={:?})", entry.prior_state)
            }
            Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => reason,
            Err(other) => format!("{other:?}"),
        };
        let refused_state = ws.load_state();
        let refused_flag = ws.rebuild_cancelled.load(Ordering::Acquire);

        // The control: the runner stopped (its gate leaves the flag set
        // after a completed eviction and releases the role).
        ws.rebuild_in_flight.store(false, Ordering::Release);
        let acquired = match mgr.prepare_load_gate(&key) {
            Ok(LoadGate::Loaded(_)) => "Loaded".to_string(),
            Ok(LoadGate::Acquired { entry, .. }) => {
                format!("Acquired(prior={:?})", entry.prior_state)
            }
            Err(error) => format!("{error:?}"),
        };
        let acquired_state = ws.load_state();
        let acquired_flag = ws.rebuild_cancelled.load(Ordering::Acquire);

        // The second control: the runner holds the role between iterations
        // (it published, so the slot is `Loaded`) when the eviction lands.
        let key_2 = make_key_at("/repos/r8-gate-between", 0x82);
        mgr.insert_workspace_in_state_for_test(key_2.clone(), WorkspaceState::Loaded);
        let ws_2 = mgr
            .lookup(&key_2)
            .expect("the seeded workspace is resident");
        ws_2.rebuild_in_flight.store(true, Ordering::Release);
        assert!(
            mgr.evict_for_test(&key_2),
            "the eviction finds the workspace"
        );
        let between = match mgr.prepare_load_gate(&key_2) {
            Ok(LoadGate::Loaded(_)) => "Loaded".to_string(),
            Ok(LoadGate::Acquired { entry, .. }) => {
                format!("Acquired(prior={:?})", entry.prior_state)
            }
            Err(error) => format!("{error:?}"),
        };
        let between_flag = ws_2.rebuild_cancelled.load(Ordering::Acquire);

        println!(
            "R8 note (a) gate: evicted=({evicted_state},flag={evicted_flag}) \
             running=({refused},{refused_state},flag={refused_flag}) \
             stopped=({acquired},{acquired_state},flag={acquired_flag}) \
             between=({between},flag={between_flag})"
        );
        assert_eq!(
            evicted_state,
            WorkspaceState::Evicted,
            "the eviction landed"
        );
        assert!(evicted_flag, "the eviction set the cancellation flag");
        assert_eq!(
            refused, EVICTED_REBUILD_STILL_RUNNING,
            "a load while the evicted generation's runner holds the role is refused"
        );
        assert_eq!(
            refused_state,
            WorkspaceState::Evicted,
            "the refused load puts the tombstone back"
        );
        assert!(
            refused_flag,
            "the refused load leaves the eviction's cancellation for the runner"
        );
        assert_eq!(
            acquired, "Acquired(prior=Evicted)",
            "with no runner the load owns the slot"
        );
        assert_eq!(acquired_state, WorkspaceState::Loading);
        assert!(!acquired_flag, "with no runner the gate consumes the flag");
        assert_eq!(
            between, "Acquired(prior=Evicted)",
            "an eviction between iterations does not hold the load back"
        );
        assert!(!between_flag, "and its flag is consumed as before");
    }

    /// T62 (surface parity W1 round 8, design D41; section 2.1.8 row 10).
    /// `get_or_load_published`'s compaction-error arm. The double completes
    /// an eviction inside `build` and answers with a graph whose one edge
    /// names a node outside its arena, so `compact_edges_in_place` (which
    /// runs with no manager lock) fails after the state has moved to
    /// `Evicted`.
    ///
    /// Row: C76, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Evicted.
    #[test]
    fn a_load_whose_compaction_fails_after_an_eviction_leaves_the_tombstone() {
        let precondition_refused = sqry_core::graph::unified::compaction::compact_edges_in_place(
            &graph_with_an_out_of_arena_edge(),
        )
        .is_err();

        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-compaction", 0x62);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
        let double = ArmDouble::new(
            &mgr,
            &key,
            &[ArmAction::TryEvict],
            ArmResult::OutOfArenaEdge,
        );
        let ws = Arc::clone(&double.workspace);
        let result = mgr.get_or_load(&key, &double, 1_000);
        let reason = build_failed_reason(&result);
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let state = ws.load_state();
        let record = ws.roster().is_some();
        let error_recorded = ws.last_error.read().is_some();
        let names_compaction = reason.starts_with("edge compaction failed");

        println!(
            "R8-1 compaction arm: precondition_refused={precondition_refused} \
             calls=({build_calls},{load_persisted_calls}) answers={answers:?} \
             names_compaction={names_compaction} state={state} record={record} \
             error_recorded={error_recorded} reason={reason}"
        );

        assert!(
            precondition_refused,
            "the helper's graph must be one compact_edges_in_place refuses"
        );
        assert_eq!(
            (build_calls, load_persisted_calls),
            (1, 0),
            "the load must build once and never read a snapshot"
        );
        assert_eq!(
            answers,
            vec![ArmAnswer::TryEvict(TryEvictOutcome::Evicted)],
            "the eviction must complete inside the build"
        );
        assert!(
            names_compaction,
            "the load must return the compaction arm's refusal: {reason}"
        );
        assert_eq!(
            state,
            WorkspaceState::Evicted,
            "a compaction failure must not relabel a completed tombstone as Failed"
        );
        assert!(!record, "the tombstone carries the placeholder, no record");
        assert!(
            error_recorded,
            "the arm's diagnostic write is unconditional (design D37)"
        );
    }

    /// T63 (surface parity W1 round 8, design D41; section 2.1.8 row 12).
    /// `get_or_load_published`'s removed-recheck arm. The double runs
    /// `unload` inside `build` (the entry is removed and its workspace left
    /// `Evicted` with the flag set), then consumes the flag on the `Arc` it
    /// holds, as the rebuild runner's top-of-loop swap does, so the
    /// cancelled recheck passes and the membership recheck is the arm that
    /// stores.
    ///
    /// Row: C77, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Evicted.
    #[test]
    fn a_load_that_finds_its_workspace_unloaded_leaves_the_orphan_evicted() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-removed-load", 0x63);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
        let double = ArmDouble::new(
            &mgr,
            &key,
            &[ArmAction::Unload, ArmAction::ConsumeCancelFlag],
            ArmResult::EmptyGraph,
        );
        let ws = Arc::clone(&double.workspace);
        let result = mgr.get_or_load(&key, &double, 1_000);
        let reason = build_failed_reason(&result);
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let present = mgr.lookup(&key).is_some();
        let state = ws.load_state();
        let record = ws.roster().is_some();

        println!(
            "R8-1 removed-recheck arm (load): calls=({build_calls},{load_persisted_calls}) \
             answers={answers:?} reason={reason} present={present} state={state} record={record}"
        );

        assert_eq!(
            (build_calls, load_persisted_calls),
            (1, 0),
            "the load must build once and never read a snapshot"
        );
        assert_eq!(
            answers,
            vec![ArmAnswer::Unload(true), ArmAnswer::ConsumeCancelFlag(true)],
            "unload must remove the entry and the holder must consume the flag it set"
        );
        assert_eq!(
            reason, "workspace removed mid-load",
            "the load must return the removed-recheck arm's refusal"
        );
        assert!(!present, "unload removed the entry");
        assert_eq!(
            state,
            WorkspaceState::Evicted,
            "a load that finds its entry removed must not relabel the orphan as Failed"
        );
        assert!(!record, "the orphan carries the placeholder, no record");
    }

    /// T64 (surface parity W1 round 8, design D41; section 2.1.8 row 13).
    /// `get_or_load_published`'s publish-error arm. Under a zero memory
    /// limit and a zero working-set estimate `reserve_rebuild` admits the
    /// load; the double completes an eviction and then `reset` (which
    /// refuses `Loading` but accepts the `Evicted` the eviction left,
    /// clears the flag and stores `Unloaded`), and answers with a
    /// non-empty graph, so both rechecks pass and `publish_and_retain`
    /// answers `WorkspaceOversize` over a state `reset` owns.
    ///
    /// Row: C78, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Unloaded.
    #[test]
    fn a_load_whose_publish_fails_after_an_eviction_and_a_reset_leaves_the_reset() {
        let mgr = zero_limit_manager();
        let key = make_key_at("/repos/w1r8-publish-load", 0x64);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Unloaded);
        let double = ArmDouble::new(
            &mgr,
            &key,
            &[ArmAction::TryEvict, ArmAction::Reset],
            ArmResult::OneNodeGraph,
        );
        let ws = Arc::clone(&double.workspace);
        let result = mgr.get_or_load(&key, &double, 0);
        let oversize = matches!(result, Err(DaemonError::WorkspaceOversize { .. }));
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let present = mgr.lookup(&key).is_some();
        let state = ws.load_state();
        let record = ws.roster().is_some();
        let error_recorded = ws.last_error.read().is_some();

        println!(
            "R8-1 publish-error arm (load): calls=({build_calls},{load_persisted_calls}) \
             answers={answers:?} oversize={oversize} present={present} state={state} \
             record={record} error_recorded={error_recorded}"
        );

        assert_eq!(
            (build_calls, load_persisted_calls),
            (1, 0),
            "the load must build once and never read a snapshot"
        );
        assert_eq!(
            answers,
            vec![
                ArmAnswer::TryEvict(TryEvictOutcome::Evicted),
                ArmAnswer::Reset(Ok(true)),
            ],
            "the eviction and then the reset must complete inside the build"
        );
        assert!(
            oversize,
            "the load must return the publish arm's WorkspaceOversize: {result:?}"
        );
        assert!(present, "reset keeps the entry");
        assert_eq!(
            state,
            WorkspaceState::Unloaded,
            "a publish failure must not relabel the state reset installed as Failed"
        );
        assert!(!record, "reset leaves the placeholder, no record");
        assert!(
            error_recorded,
            "the arm's diagnostic write is unconditional (design D37)"
        );
    }

    /// A workspace resident at `key`, seeded `Loaded` and then evicted
    /// through the production eviction (`evict_for_test` reaches
    /// `execute_eviction`), so a reload starts from the genuine tombstone its
    /// production caller `DaemonGraphProvider::handle_classify_error`
    /// reloads. Answers the eviction's result and the tombstone's
    /// `(state, record, flag)`.
    fn seed_genuine_tombstone(
        mgr: &Arc<WorkspaceManager>,
        key: &WorkspaceKey,
    ) -> (bool, (WorkspaceState, bool, bool)) {
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        let evicted = mgr.evict_for_test(key);
        let ws = mgr.lookup(key).expect("the tombstone stays resident");
        (
            evicted,
            (
                ws.load_state(),
                ws.roster().is_some(),
                ws.rebuild_cancelled.load(Ordering::Acquire),
            ),
        )
    }

    /// T65 (surface parity W1 round 8, design D41; section 2.1.8 row 14).
    /// `reload_from_disk_read_only`'s `load_persisted`-error arm. From a
    /// genuine tombstone the reload wins the gate (from `Evicted`, which
    /// consumes the flag the eviction set); the double completes a second
    /// eviction inside `load_persisted` and refuses, so the error arm
    /// stores over `Evicted`.
    ///
    /// Row: C79, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Evicted.
    #[test]
    fn a_reload_whose_snapshot_load_fails_after_an_eviction_leaves_the_tombstone() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-reload-refused", 0x65);
        let (seeded, tombstone) = seed_genuine_tombstone(&mgr, &key);
        let double = ArmDouble::new(&mgr, &key, &[ArmAction::TryEvict], ArmResult::Refusal);
        let ws = Arc::clone(&double.workspace);
        let result = mgr.reload_from_disk_read_only(&key, &double, 1_000);
        let reason = build_failed_reason(&result);
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let state = ws.load_state();
        let record = ws.roster().is_some();

        println!(
            "R8-1 load_persisted-error arm: seeded={seeded} tombstone={tombstone:?} \
             calls=({build_calls},{load_persisted_calls}) answers={answers:?} reason={reason} \
             state={state} record={record}"
        );

        assert!(
            seeded,
            "the seeded workspace evicts through the production path"
        );
        assert_eq!(
            tombstone,
            (WorkspaceState::Evicted, false, true),
            "the reload starts from the genuine tombstone"
        );
        assert_eq!(
            (build_calls, load_persisted_calls),
            (0, 1),
            "the reload must read the snapshot once and never build"
        );
        assert_eq!(
            answers,
            vec![ArmAnswer::TryEvict(TryEvictOutcome::Evicted)],
            "the eviction must complete inside load_persisted"
        );
        assert_eq!(
            reason, ARM_DOUBLE_REASON,
            "the reload must return the double's own error"
        );
        assert_eq!(
            state,
            WorkspaceState::Evicted,
            "a snapshot load failure must not relabel a completed tombstone as Failed"
        );
        assert!(!record, "the tombstone carries the placeholder, no record");
    }

    /// T66 (surface parity W1 round 8, design D41; section 2.1.8 row 15).
    /// `reload_from_disk_read_only`'s cancelled-recheck arm: as T65, the
    /// snapshot load succeeding, so the recheck under `workspaces.read()`
    /// finds the flag the second eviction set.
    ///
    /// Row: C80, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Evicted.
    #[test]
    fn a_reload_evicted_before_its_recheck_leaves_the_tombstone() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-reload-evicted", 0x66);
        let (seeded, tombstone) = seed_genuine_tombstone(&mgr, &key);
        let double = ArmDouble::new(&mgr, &key, &[ArmAction::TryEvict], ArmResult::EmptyGraph);
        let ws = Arc::clone(&double.workspace);
        let result = mgr.reload_from_disk_read_only(&key, &double, 1_000);
        let reason = build_failed_reason(&result);
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let state = ws.load_state();
        let record = ws.roster().is_some();

        println!(
            "R8-1 cancelled-recheck arm (reload): seeded={seeded} tombstone={tombstone:?} \
             calls=({build_calls},{load_persisted_calls}) answers={answers:?} reason={reason} \
             state={state} record={record}"
        );

        assert!(
            seeded,
            "the seeded workspace evicts through the production path"
        );
        assert_eq!(
            tombstone,
            (WorkspaceState::Evicted, false, true),
            "the reload starts from the genuine tombstone"
        );
        assert_eq!(
            (build_calls, load_persisted_calls),
            (0, 1),
            "the reload must read the snapshot once and never build"
        );
        assert_eq!(
            answers,
            vec![ArmAnswer::TryEvict(TryEvictOutcome::Evicted)],
            "the eviction must complete inside load_persisted"
        );
        assert_eq!(
            reason, "workspace evicted mid-reload",
            "the reload must return the cancelled-recheck arm's refusal"
        );
        assert_eq!(
            state,
            WorkspaceState::Evicted,
            "a reload evicted before its recheck must not relabel the tombstone as Failed"
        );
        assert!(!record, "the tombstone carries the placeholder, no record");
    }

    /// T67 (surface parity W1 round 8, design D41; section 2.1.8 row 16).
    /// `reload_from_disk_read_only`'s removed-recheck arm: as T63, inside
    /// `load_persisted`, from a genuine tombstone.
    ///
    /// Row: C81, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Evicted.
    #[test]
    fn a_reload_that_finds_its_workspace_unloaded_leaves_the_orphan_evicted() {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at("/repos/w1r8-reload-removed", 0x67);
        let (seeded, tombstone) = seed_genuine_tombstone(&mgr, &key);
        let double = ArmDouble::new(
            &mgr,
            &key,
            &[ArmAction::Unload, ArmAction::ConsumeCancelFlag],
            ArmResult::EmptyGraph,
        );
        let ws = Arc::clone(&double.workspace);
        let result = mgr.reload_from_disk_read_only(&key, &double, 1_000);
        let reason = build_failed_reason(&result);
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let present = mgr.lookup(&key).is_some();
        let state = ws.load_state();
        let record = ws.roster().is_some();

        println!(
            "R8-1 removed-recheck arm (reload): seeded={seeded} tombstone={tombstone:?} \
             calls=({build_calls},{load_persisted_calls}) answers={answers:?} reason={reason} \
             present={present} state={state} record={record}"
        );

        assert!(
            seeded,
            "the seeded workspace evicts through the production path"
        );
        assert_eq!(
            tombstone,
            (WorkspaceState::Evicted, false, true),
            "the reload starts from the genuine tombstone"
        );
        assert_eq!(
            (build_calls, load_persisted_calls),
            (0, 1),
            "the reload must read the snapshot once and never build"
        );
        assert_eq!(
            answers,
            vec![ArmAnswer::Unload(true), ArmAnswer::ConsumeCancelFlag(true)],
            "unload must remove the entry and the holder must consume the flag it set"
        );
        assert_eq!(
            reason, "workspace removed mid-reload",
            "the reload must return the removed-recheck arm's refusal"
        );
        assert!(!present, "unload removed the entry");
        assert_eq!(
            state,
            WorkspaceState::Evicted,
            "a reload that finds its entry removed must not relabel the orphan as Failed"
        );
        assert!(!record, "the orphan carries the placeholder, no record");
    }

    /// T68 (surface parity W1 round 8, design D41; section 2.1.8 row 17).
    /// `reload_from_disk_read_only`'s publish-error arm: as T64, inside
    /// `load_persisted`, from a genuine tombstone.
    ///
    /// Row: C82, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Failed)`. Under it the state
    /// assertion fails with left Failed right Unloaded.
    #[test]
    fn a_reload_whose_publish_fails_after_an_eviction_and_a_reset_leaves_the_reset() {
        let mgr = zero_limit_manager();
        let key = make_key_at("/repos/w1r8-reload-publish", 0x68);
        let (seeded, tombstone) = seed_genuine_tombstone(&mgr, &key);
        let double = ArmDouble::new(
            &mgr,
            &key,
            &[ArmAction::TryEvict, ArmAction::Reset],
            ArmResult::OneNodeGraph,
        );
        let ws = Arc::clone(&double.workspace);
        let result = mgr.reload_from_disk_read_only(&key, &double, 0);
        let oversize = matches!(result, Err(DaemonError::WorkspaceOversize { .. }));
        let (build_calls, load_persisted_calls) = double.calls();
        let answers = double.answers();
        let present = mgr.lookup(&key).is_some();
        let state = ws.load_state();
        let record = ws.roster().is_some();

        println!(
            "R8-1 publish-error arm (reload): seeded={seeded} tombstone={tombstone:?} \
             calls=({build_calls},{load_persisted_calls}) answers={answers:?} \
             oversize={oversize} present={present} state={state} record={record}"
        );

        assert!(
            seeded,
            "the seeded workspace evicts through the production path"
        );
        assert_eq!(
            tombstone,
            (WorkspaceState::Evicted, false, true),
            "the reload starts from the genuine tombstone"
        );
        assert_eq!(
            (build_calls, load_persisted_calls),
            (0, 1),
            "the reload must read the snapshot once and never build"
        );
        assert_eq!(
            answers,
            vec![
                ArmAnswer::TryEvict(TryEvictOutcome::Evicted),
                ArmAnswer::Reset(Ok(true)),
            ],
            "the eviction and then the reset must complete inside load_persisted"
        );
        assert!(
            oversize,
            "the reload must return the publish arm's WorkspaceOversize: {result:?}"
        );
        assert!(present, "reset keeps the entry");
        assert_eq!(
            state,
            WorkspaceState::Unloaded,
            "a publish failure must not relabel the state reset installed as Failed"
        );
        assert!(!record, "reset leaves the placeholder, no record");
    }

    // --- Surface parity W1 round 8 (design D38 (b), D42): the writer half of
    // the eviction machinery's exclusivity -------------------------------

    /// The caller of `evict_to_tombstone_locked` a T69 leg drives.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TombstoneWriter {
        /// [`WorkspaceManager::try_evict_for_test`].
        TryEvictForTest,
        /// [`WorkspaceManager::evict_for_test`], which reaches
        /// `execute_eviction`, the body `evict_lru` and `reserve_rebuild`'s
        /// Phase 2 run.
        EvictForTest,
        /// [`WorkspaceManager::unload`].
        Unload,
        /// [`WorkspaceManager::reset`], without force.
        Reset,
    }

    /// The writer's own answer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum WriterAnswer {
        TryEvictForTest(TryEvictOutcome),
        EvictForTest(bool),
        Unload(bool),
        Reset(Result<bool, String>),
    }

    /// Everything one T69 leg observed, recorded before any assertion runs.
    #[derive(Debug)]
    struct WriterLeg {
        writer: TombstoneWriter,
        /// Observations of the slot, the last one being the first that found
        /// the placeholder (no record) or the bound.
        polls: u32,
        /// Whether an observation found the placeholder in the slot.
        swapped: bool,
        /// Whether the writer thread had returned while the test held the
        /// admission mutex.
        finished_while_held: bool,
        /// `workspaces.try_read().is_some()` while the writer was inside the
        /// eviction body (the first probe).
        guard_free_while_held: bool,
        answer: WriterAnswer,
        /// `workspaces.try_read().is_some()` after the writer returned (the
        /// second probe).
        guard_free_after: bool,
        /// Whether the entry is still keyed in the map at the end.
        present: bool,
        /// The workspace's state and record at the end, read through the
        /// `Arc` the test held from before the writer ran.
        state: WorkspaceState,
        record: bool,
    }

    /// Slot observations a leg makes, 1 ms apart, before it gives up.
    const WRITER_POLL_BOUND: u32 = 20_000;

    /// One T69 leg (design D42). The test takes the admission mutex and
    /// holds it; one thread runs `writer`, whose body swaps the placeholder
    /// into the slot and then blocks on that mutex; the test observes the
    /// swap, checks the thread has not returned, probes
    /// `workspaces.try_read()` (dropping any guard at once), releases the
    /// mutex, joins, and probes again. No assertion runs while the mutex is
    /// held: a failing assertion there would leave the writer blocked and
    /// turn a kill into a hang.
    fn run_tombstone_writer_leg(
        writer: TombstoneWriter,
        path: &str,
        fingerprint: u64,
    ) -> WriterLeg {
        let mgr = WorkspaceManager::new_without_reaper(make_config());
        let key = make_key_at(path, fingerprint);
        mgr.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        let ws = mgr.lookup(&key).expect("the seeded workspace is resident");

        let admission = mgr.admission.lock();
        let writer_mgr = Arc::clone(&mgr);
        let writer_key = key.clone();
        let handle = std::thread::spawn(move || match writer {
            TombstoneWriter::TryEvictForTest => {
                WriterAnswer::TryEvictForTest(writer_mgr.try_evict_for_test(&writer_key))
            }
            TombstoneWriter::EvictForTest => {
                WriterAnswer::EvictForTest(writer_mgr.evict_for_test(&writer_key))
            }
            TombstoneWriter::Unload => WriterAnswer::Unload(writer_mgr.unload(&writer_key)),
            TombstoneWriter::Reset => WriterAnswer::Reset(
                writer_mgr
                    .reset(&writer_key, false)
                    .map_err(|err| err.to_string()),
            ),
        });
        let mut polls = 0;
        let mut swapped = false;
        while polls < WRITER_POLL_BOUND {
            polls += 1;
            if ws.roster().is_none() {
                swapped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let finished_while_held = handle.is_finished();
        let guard_free_while_held = mgr.workspaces.try_read().is_some();
        drop(admission);
        let answer = handle.join().expect("the writer thread returns");
        let guard_free_after = mgr.workspaces.try_read().is_some();
        let present = mgr.lookup(&key).is_some();
        WriterLeg {
            writer,
            polls,
            swapped,
            finished_while_held,
            guard_free_while_held,
            answer,
            guard_free_after,
            present,
            state: ws.load_state(),
            record: ws.roster().is_some(),
        }
    }

    /// T69 (surface parity W1 round 8, design D38 (b) and D42; R8-2). Each
    /// of the four callers of `evict_to_tombstone_locked` holds a
    /// `workspaces` write guard it acquired itself for the whole of that
    /// call. Observed from outside at one interior instant: the body waits
    /// on the admission mutex after its placeholder swap, and the admission
    /// mutex is the one lock the body takes that no `workspaces` guard
    /// holder needs before it, so the test can hold it without adding a
    /// seam. One interior instant is the whole call for a guard the caller
    /// holds: the caller runs no code while the call is in progress.
    ///
    /// `parking_lot`'s `RwLock::try_read` fails while a writer holds the
    /// lock and succeeds while no guard is held, so the first probe is
    /// `false` under the shipped code at every leg and `true` under a plant
    /// that runs the body on a clone of the map with the guard released. If a
    /// later change moves the admission section ahead of the swap, the poll
    /// bound fails with its own message rather than passing.
    ///
    /// Rows: C87 (codex's plant verbatim, at `try_evict_for_test`), C88
    /// (`execute_eviction`), C89 (`unload`) and C90 (`reset`), each the
    /// guard's map cloned with the guard dropped; under each, that leg's
    /// first-probe assertion fails. Must survive: S33 (grok's plant, the
    /// guard released and taken again before the body reads anything) and
    /// S26 (the explicit drop removed), which D38 (c) does not claim.
    #[test]
    fn every_tombstone_writer_holds_the_write_guard_inside_the_eviction_body() {
        let legs = [
            run_tombstone_writer_leg(
                TombstoneWriter::TryEvictForTest,
                "/repos/w1r8-writer-try-evict",
                0x691,
            ),
            run_tombstone_writer_leg(
                TombstoneWriter::EvictForTest,
                "/repos/w1r8-writer-evict",
                0x692,
            ),
            run_tombstone_writer_leg(TombstoneWriter::Unload, "/repos/w1r8-writer-unload", 0x693),
            run_tombstone_writer_leg(TombstoneWriter::Reset, "/repos/w1r8-writer-reset", 0x694),
        ];
        println!(
            "R8-2 tombstone writers: {}",
            legs.iter()
                .map(|leg| format!("{leg:?}"))
                .collect::<Vec<_>>()
                .join(" ")
        );

        for leg in &legs {
            let writer = leg.writer;
            // The writer was inside the body when the probe ran.
            assert!(
                leg.swapped,
                "{writer:?}: the writer must swap in the placeholder within {WRITER_POLL_BOUND} polls"
            );
            assert!(
                leg.polls >= 1 && leg.polls < WRITER_POLL_BOUND,
                "{writer:?}: the swap must be observed under the poll bound ({} polls)",
                leg.polls
            );
            assert!(
                !leg.finished_while_held,
                "{writer:?}: the writer cannot return while the admission mutex is held"
            );
            // The claim.
            assert_eq!(
                (writer, leg.guard_free_while_held),
                (writer, false),
                "a tombstone writer must hold a workspaces write guard while it is inside the \
                 eviction body"
            );
            assert_eq!(
                (writer, leg.guard_free_after),
                (writer, true),
                "the writer must release the guard when it returns"
            );
            let (answer, present, state) = match writer {
                TombstoneWriter::TryEvictForTest => (
                    WriterAnswer::TryEvictForTest(TryEvictOutcome::Evicted),
                    true,
                    WorkspaceState::Evicted,
                ),
                TombstoneWriter::EvictForTest => (
                    WriterAnswer::EvictForTest(true),
                    true,
                    WorkspaceState::Evicted,
                ),
                TombstoneWriter::Unload => {
                    (WriterAnswer::Unload(true), false, WorkspaceState::Evicted)
                }
                TombstoneWriter::Reset => (
                    WriterAnswer::Reset(Ok(true)),
                    true,
                    WorkspaceState::Unloaded,
                ),
            };
            assert_eq!(
                (writer, &leg.answer),
                (writer, &answer),
                "the writer's own answer"
            );
            assert_eq!(
                (writer, leg.present, leg.state, leg.record),
                (writer, present, state, false),
                "the end state the writer leaves: the entry kept or removed, the state, no record"
            );
        }
    }

    /// T73 (surface parity W1 round 9, design D45, target R9-3). The
    /// two-loader interleaving D41 records as outside D37's claim, measured
    /// rather than read.
    ///
    /// `WorkspaceManager::enter_loading_state` compares a state value, not an
    /// owner: it offers `Unloaded`, `Failed` and `Evicted` as prior states, so
    /// after an eviction completes during a load a second loader can win the
    /// gate from the completed tombstone. `WorkspaceManager::honor_preexisting_cancel`
    /// then swaps the cancellation flag to `false` before it decides anything
    /// and returns `Ok(())` without re-arming it, because the prior state is
    /// `Evicted`. The state left behind is `Loading`, installed by the second
    /// loader, and every failure arm of `WorkspaceManager::get_or_load_published`
    /// stores through `transition_state(Loading, Failed)`, which cannot see
    /// whose gate installed it.
    ///
    /// Both legs drive that sequence through production calls only, single
    /// threaded, with `ArmDouble` performing the second loader's gate inside
    /// `build` where the manager holds no lock. Both assert the planted
    /// condition first (the eviction answered `Evicted`, the gate won from
    /// `Evicted`, `honor_preexisting_cancel` answered `Ok`, the flag is
    /// consumed), so a green cannot mean the sequence was never reached.
    ///
    /// Measured at `60afdcdd1`, rule (a) of D45: the interleaving is observed
    /// at both halves. Leg A's builder refusal stores `Failed` over the second
    /// loader's `Loading` gate, and leg B publishes and stores `Loaded` with
    /// the first loader's record while the second loader's gate is unfinished.
    /// W1 does not repair it: a compare-exchange that could tell one gate from
    /// another needs an owner token beside the state, which changes the gate's
    /// contract, and question Q18 carries the measurement to the owner. So
    /// this test is a characterisation oracle: a repair of Q18 must change it
    /// deliberately rather than discover it.
    ///
    /// Rows: C91 (the `Evicted` disjunct removed from `honor_preexisting_cancel`'s
    /// early return) and C92 (its flag read turned into a peek), killed at the
    /// gate-answer and the flag assertion respectively; S34 (the early return's
    /// two conditions swapped) must survive.
    #[test]
    fn a_second_loader_winning_the_gate_from_a_tombstone_is_measured_at_both_halves() {
        // ---- Leg A: the first loader's builder refuses ------------------
        let mgr_a = WorkspaceManager::new_without_reaper(make_config());
        let key_a = make_key_at("/repos/w1r9-two-loader-fail", 0x73);
        mgr_a.insert_workspace_in_state_for_test(key_a.clone(), WorkspaceState::Unloaded);
        let double_a = ArmDouble::new(
            &mgr_a,
            &key_a,
            &[ArmAction::TryEvict, ArmAction::SecondLoaderGate],
            ArmResult::Refusal,
        );
        let ws_a = Arc::clone(&double_a.workspace);
        let result_a = mgr_a.get_or_load(&key_a, &double_a, 1_000);
        let reason_a = build_failed_reason(&result_a);
        let answers_a = double_a.answers();
        let state_a = ws_a.load_state();
        let record_a = ws_a.roster().is_some();
        let error_a = ws_a.last_error.read().is_some();
        let present_a = mgr_a.lookup(&key_a).is_some();
        let flag_a = ws_a.rebuild_cancelled.load(Ordering::Acquire);

        // ---- Leg B: the first loader's builder answers with a graph -----
        let mgr_b = WorkspaceManager::new_without_reaper(make_config());
        let key_b = make_key_at("/repos/w1r9-two-loader-ok", 0x74);
        mgr_b.insert_workspace_in_state_for_test(key_b.clone(), WorkspaceState::Unloaded);
        let double_b = ArmDouble::new(
            &mgr_b,
            &key_b,
            &[ArmAction::TryEvict, ArmAction::SecondLoaderGate],
            ArmResult::OneNodeGraph,
        );
        let ws_b = Arc::clone(&double_b.workspace);
        let result_b = mgr_b.get_or_load(&key_b, &double_b, 1_000);
        let nodes_b = result_b.as_ref().ok().map(|graph| graph.node_count());
        let answers_b = double_b.answers();
        let state_b = ws_b.load_state();
        let record_b = ws_b.roster().is_some();
        let error_b = ws_b.last_error.read().is_some();
        let present_b = mgr_b.lookup(&key_b).is_some();
        let flag_b = ws_b.rebuild_cancelled.load(Ordering::Acquire);

        println!(
            "R9-3 two loaders: legA=(answers={answers_a:?},reason={reason_a},state={state_a},\
             record={record_a},last_error={error_a},present={present_a},flag={flag_a}) \
             legB=(answers={answers_b:?},nodes={nodes_b:?},state={state_b},record={record_b},\
             last_error={error_b},present={present_b},flag={flag_b})"
        );

        // The planted condition happened, both legs: the eviction completed
        // inside the load and a second loader took the gate from its
        // tombstone, which is the window D41 describes.
        for (leg, answers) in [("A", &answers_a), ("B", &answers_b)] {
            assert_eq!(answers.len(), 2, "leg {leg} runs both actions");
            assert_eq!(
                answers[0],
                ArmAnswer::TryEvict(TryEvictOutcome::Evicted),
                "leg {leg}'s eviction must complete inside the load"
            );
            match &answers[1] {
                ArmAnswer::SecondLoaderGate {
                    prior,
                    answer,
                    flag,
                } => {
                    assert_eq!(
                        *prior,
                        Some(WorkspaceState::Evicted),
                        "leg {leg}: the second loader's gate must win from the tombstone"
                    );
                    // `build_failed_reason` renders anything that is not a
                    // `WorkspaceBuildFailed` with its Debug, so this string is
                    // exactly "honor_preexisting_cancel answered Ok(())".
                    assert_eq!(
                        answer, "not WorkspaceBuildFailed: Ok(())",
                        "leg {leg}: honor_preexisting_cancel must answer Ok for an Evicted prior"
                    );
                    assert!(
                        !flag,
                        "leg {leg}: the second loader's gate consumes the cancellation flag \
                         and does not re-arm it for an Evicted prior"
                    );
                }
                other => panic!("leg {leg}'s second action must be the gate, got {other:?}"),
            }
        }

        // Leg A, the measured outcome: the first loader's builder-error arm
        // finds `Loading` and succeeds over the second loader's gate.
        assert_eq!(
            reason_a, ARM_DOUBLE_REASON,
            "leg A must answer with the double's own refusal"
        );
        assert_eq!(
            state_a,
            WorkspaceState::Failed,
            "the measured fact (D45 rule a): the first loader's failure store lands \
             over the second loader's Loading gate"
        );
        assert!(!record_a, "the tombstone's placeholder carries no record");
        assert!(
            error_a,
            "the builder-error arm records the failure it answers with"
        );
        assert!(present_a, "the tombstoned entry stays in the map");
        assert!(
            !flag_a,
            "the flag stays consumed: the arm that would re-arm it was not taken"
        );

        // Leg B, the measured outcome: the post-build recheck sees the flag
        // the second loader's gate consumed and the entry the tombstone kept,
        // so the first loader publishes and stores `Loaded` beside a gate
        // that is still unfinished.
        assert_eq!(
            nodes_b,
            Some(1),
            "leg B's load must answer with the double's one-node graph"
        );
        assert_eq!(
            state_b,
            WorkspaceState::Loaded,
            "the measured fact (D45 rule a): the first loader publishes and stores Loaded \
             while the second loader's gate is unfinished"
        );
        assert!(record_b, "the publish installs the first loader's record");
        assert!(!error_b, "leg B's load answered Ok, so nothing is recorded");
        assert!(present_b, "the entry the tombstone kept is published into");
        assert!(
            !flag_b,
            "the flag stays consumed, which is why the recheck admitted the publish"
        );
    }

    /// F1 at the read-only reload: `reload_from_disk_read_only` prepares the
    /// load before it reserves, so a reload refused for its own input (no
    /// persisted index, a manifest without its snapshot, an unreadable
    /// manifest, a manifest naming an id this binary did not compile) evicts
    /// no sibling. Before the repair the reservation's LRU phase evicted the
    /// sibling first. The control reloads a valid index under the same
    /// pressure and evicts the sibling.
    #[test]
    fn a_reload_refused_for_its_own_input_evicts_no_sibling() {
        use super::super::builder::{
            FunctionGraphBuilder, RealWorkspaceBuilder, graph_with_function_nodes,
        };
        use super::super::roster::WorkspaceRosterResolver;
        use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};

        fn pressure() -> (
            Arc<WorkspaceManager>,
            Arc<LoadedWorkspace>,
            u64,
            tempfile::TempDir,
        ) {
            let config = {
                let _env = crate::TEST_ENV_LOCK
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                Arc::new(DaemonConfig {
                    memory_limit_mb: 4,
                    ..DaemonConfig::default()
                })
            };
            let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
            let limit = config.memory_limit_bytes();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let estimate =
                ((2 * 1024 * 1024) as f64 * crate::config::WORKING_SET_MULTIPLIER) as u64;
            let mut nodes = 500_u32;
            while (graph_with_function_nodes(nodes).heap_bytes() as u64) + estimate <= limit {
                nodes += 500;
            }
            let b_dir = tempfile::TempDir::new().expect("tempdir");
            let b_key = WorkspaceKey::new(
                b_dir.path().canonicalize().expect("canonical"),
                ProjectRootMode::GitRoot,
                0,
            );
            manager
                .get_or_load(
                    &b_key,
                    &FunctionGraphBuilder::with_fast_path_record(nodes),
                    1,
                )
                .expect("B loads");
            let ws_b = manager.lookup(&b_key).expect("B resident");
            (manager, ws_b, estimate, b_dir)
        }

        fn index(root: &Path, ids: Vec<String>) {
            let plugins = sqry_plugin_registry::create_plugin_manager();
            sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
                root,
                &plugins,
                &sqry_core::graph::unified::build::BuildConfig::default(),
                "test:reload_refusal",
                Some(PluginSelectionManifest {
                    active_plugin_ids: ids,
                    high_cost_mode: Some("fast_path_default".to_string()),
                }),
                sqry_core::progress::no_op_reporter(),
            )
            .expect("index persists");
        }

        let fast_ids: Vec<String> = sqry_plugin_registry::create_plugin_manager()
            .plugins()
            .iter()
            .map(|plugin| plugin.metadata().id.to_string())
            .collect();
        type Plant = Box<dyn Fn(&Path)>;
        let plants: Vec<(&str, Plant)> = vec![
            ("no persisted index", Box::new(|_root: &Path| {})),
            (
                "manifest without its snapshot",
                Box::new({
                    let ids = fast_ids.clone();
                    move |root: &Path| {
                        index(root, ids.clone());
                        std::fs::remove_file(GraphStorage::new(root).snapshot_path())
                            .expect("remove the snapshot");
                    }
                }),
            ),
            (
                "unreadable manifest",
                Box::new({
                    let ids = fast_ids.clone();
                    move |root: &Path| {
                        index(root, ids.clone());
                        std::fs::write(GraphStorage::new(root).manifest_path(), b"{ not json")
                            .expect("corrupt the manifest");
                    }
                }),
            ),
            (
                "uncompiled plugin id",
                Box::new({
                    let ids = fast_ids.clone();
                    move |root: &Path| {
                        index(root, ids.clone());
                        let storage = GraphStorage::new(root);
                        let mut manifest = storage.load_manifest().expect("manifest");
                        manifest
                            .plugin_selection
                            .as_mut()
                            .expect("selection")
                            .active_plugin_ids
                            .push("reload-refusal-planted-plugin".to_string());
                        manifest.save(storage.manifest_path()).expect("manifest");
                    }
                }),
            ),
        ];
        for (label, plant) in &plants {
            let (manager, ws_b, estimate, _b_dir) = pressure();
            let c_dir = tempfile::TempDir::new().expect("tempdir");
            let c_root = c_dir.path().canonicalize().expect("canonical");
            std::fs::write(c_root.join("lib.rs"), b"pub fn c() {}\n").expect("source");
            plant(&c_root);
            let c_key = WorkspaceKey::new(c_root, ProjectRootMode::default(), 0);
            let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
            let refused = manager.reload_from_disk_read_only(&c_key, &builder, estimate);
            assert!(refused.is_err(), "{label}: the reload must be refused");
            assert_eq!(
                ws_b.load_state(),
                WorkspaceState::Loaded,
                "{label}: a reload refused for its own input must not evict a sibling"
            );
        }

        // Control: a valid index reloads under the same pressure and evicts B.
        let (manager, ws_b, estimate, _b_dir) = pressure();
        let c_dir = tempfile::TempDir::new().expect("tempdir");
        let c_root = c_dir.path().canonicalize().expect("canonical");
        std::fs::write(c_root.join("lib.rs"), b"pub fn c() {}\n").expect("source");
        index(&c_root, fast_ids);
        let c_key = WorkspaceKey::new(c_root, ProjectRootMode::default(), 0);
        let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
        manager
            .reload_from_disk_read_only(&c_key, &builder, estimate)
            .expect("a valid index reloads");
        assert_eq!(
            ws_b.load_state(),
            WorkspaceState::Evicted,
            "the pressure is real: the valid reload evicts the sibling"
        );
    }
}
