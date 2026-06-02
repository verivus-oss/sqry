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
    collections::HashMap,
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
    builder::WorkspaceBuilder,
    hook::{NoOpHook, SharedHook, SqrydHook},
    loaded::LoadedWorkspace,
    staleness::{StalenessVerdict, classify_staleness},
    state::{OldGraphToken, WorkspaceKey, WorkspaceState},
    status::{DaemonStatus, MemoryStatus, WorkspaceStatus},
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
    },
    /// Workspace exists in the manager map but is not yet ready to
    /// serve (`Unloaded` or `Loading`). The IPC router decides what to
    /// do next (retry-after-delay, enqueue, surface a client-appropriate
    /// code) — the manager does not prescribe a retry policy.
    NotReady { state: WorkspaceState },
}

// ---------------------------------------------------------------------------
// WorkspaceManager
// ---------------------------------------------------------------------------

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
    /// (not on every query) — the RwLock is never a hot path.
    hook: RwLock<SharedHook>,
}

impl WorkspaceManager {
    /// Construct a fresh manager and spawn the retention reaper.
    ///
    /// The reaper is spawned on the current Tokio runtime. Callers
    /// must therefore construct the manager from a Tokio context
    /// (`#[tokio::main]`, an `async` block driven by `Runtime::block_on`,
    /// etc.). Tests that don't need the reaper can use
    /// [`Self::new_without_reaper`].
    pub fn new(config: Arc<DaemonConfig>) -> Arc<Self> {
        let mgr = Arc::new(Self {
            config: Arc::clone(&config),
            workspaces: RwLock::new(HashMap::new()),
            admission: Mutex::new(AdmissionState::default()),
            reaper: Mutex::new(None),
            started_at: Instant::now(),
            total_memory_high_water: AtomicU64::new(0),
            hook: RwLock::new(Arc::new(NoOpHook) as SharedHook),
        });
        let handle = tokio::spawn(retention_reaper(Arc::downgrade(&mgr)));
        *mgr.reaper.lock() = Some(handle);
        mgr
    }

    /// Like [`Self::new`] but does not spawn the reaper — useful in
    /// unit tests that drive the retention map synchronously via
    /// [`Self::reap_once`].
    #[doc(hidden)]
    pub fn new_without_reaper(config: Arc<DaemonConfig>) -> Arc<Self> {
        Arc::new(Self {
            config,
            workspaces: RwLock::new(HashMap::new()),
            admission: Mutex::new(AdmissionState::default()),
            reaper: Mutex::new(None),
            started_at: Instant::now(),
            total_memory_high_water: AtomicU64::new(0),
            hook: RwLock::new(Arc::new(NoOpHook) as SharedHook),
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
        self.workspaces.read().get(key).cloned()
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

            let Some(requester_ws) = workspaces.get(for_key) else {
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
                // every pinned workspace. Also skip workspaces in
                // Evicted or Unloaded state — they have no bytes to
                // reclaim and would be no-ops.
                **k != *for_key
                    && !ws.pinned
                    && ws.load_state() != WorkspaceState::Evicted
                    && ws.load_state() != WorkspaceState::Unloaded
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
    ///    manager map** as a tombstone. STEP_6 (workspace-aware-
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
    /// Codex STEP_6 iter-1 BLOCK: the pre-fix version unconditionally
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
    /// 4. Reserve admission headroom (§G.1 three-phase).
    /// 5. Build the graph via the injected `builder`.
    /// 6. Re-check `rebuild_cancelled` + workspace map membership
    ///    before publishing. If eviction ran during the build, the
    ///    reservation refunds via RAII and no graph is published.
    /// 7. Publish via `publish_and_retain`. Disarm the LoadingGuard
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
    /// map. The CAS + post-build re-check + LoadingGuard together
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
    /// - [`DaemonError::MemoryBudgetExceeded`] if Phase 3 cannot
    ///   admit the reservation even after LRU eviction.
    /// - [`DaemonError::WorkspaceBuildFailed`] surfaced from the
    ///   builder OR synthesised when a concurrent eviction races
    ///   the load (`reason = "workspace evicted mid-load"`).
    pub fn get_or_load(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        builder: &dyn WorkspaceBuilder,
        working_set_estimate: u64,
    ) -> Result<Arc<CodeGraph>, DaemonError> {
        // --- Step 1: cache-hit fast path ------------------------
        {
            let workspaces = self.workspaces.read();
            if let Some(ws) = workspaces.get(key)
                && ws.load_state() == WorkspaceState::Loaded
            {
                ws.touch();
                return Ok(ws.graph.load_full());
            }
        }

        // --- Step 2: take the lifecycle gate via state CAS ------
        let ws = self.get_or_insert_workspace(key);
        let allowed = [
            WorkspaceState::Unloaded.as_u8(),
            WorkspaceState::Failed.as_u8(),
            WorkspaceState::Evicted.as_u8(),
        ];
        let mut acquired_from: Option<WorkspaceState> = None;
        for prior in allowed {
            if ws
                .state
                .compare_exchange(
                    prior,
                    WorkspaceState::Loading.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                acquired_from = WorkspaceState::from_u8(prior);
                break;
            }
        }
        let Some(prior_state) = acquired_from else {
            // Someone else already holds the gate (Loading /
            // Rebuilding) OR raced us into Loaded. Cache-read and
            // return if Loaded, else surface a transient error.
            let current = ws.load_state();
            if current == WorkspaceState::Loaded {
                ws.touch();
                return Ok(ws.graph.load_full());
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: format!("workspace load already in progress ({current})"),
            });
        };
        // We own the gate. Clear the cancellation flag AFTER the
        // CAS, but interpret a pre-cleared `cancelled = true`
        // differently depending on the prior state we won from:
        //
        // - Prior = `Evicted`: STEP_6 iter-2. LRU eviction
        //   completed on this entry (workspaces.write() was held
        //   across both the `cancelled.store(true)` and the
        //   `state.store(Evicted)` in `execute_eviction`). The
        //   cancelled flag is a stale residue of that completed
        //   eviction; this `get_or_load` is a fresh reload and
        //   must clear cancelled unconditionally.
        // - Prior = `Unloaded` / `Failed`: a concurrent eviction
        //   is racing us. The flag is a live cancel signal — the
        //   eviction reached `cancelled.store(true)` before our
        //   CAS but the state had not yet been moved to
        //   `Evicted`. Honour the cancel and fail this load.
        let pre_cancelled = ws.rebuild_cancelled.swap(false, Ordering::AcqRel);
        if pre_cancelled && prior_state != WorkspaceState::Evicted {
            // Evict raced us out of the allowed-state list. Put
            // the cancelled flag back, transition to Failed (so
            // this caller's LoadingGuard doesn't fire), and fail.
            ws.rebuild_cancelled.store(true, Ordering::Release);
            ws.store_state(WorkspaceState::Failed);
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-load".to_string(),
            });
        }

        // --- Step 3: arm LoadingGuard for panic / early-return --
        let mut loading = LoadingGuard {
            ws: &ws,
            key,
            armed: true,
        };

        // --- Step 4: reserve admission headroom ------------------
        let reservation = self.reserve_rebuild(key, working_set_estimate)?;

        // --- Step 5: build the graph ----------------------------
        let graph = match builder.build(&key.source_root) {
            Ok(g) => g,
            Err(err) => {
                drop(reservation);
                // The LoadingGuard will flip us to Failed + record
                // a synthetic error; overwrite with the builder's
                // real error for diagnostic fidelity.
                ws.record_failure(clone_err(&err));
                loading.armed = false;
                ws.store_state(WorkspaceState::Failed);
                return Err(err);
            }
        };

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
            ws.store_state(WorkspaceState::Failed);
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-load".to_string(),
            });
        }
        if !workspaces_guard.contains_key(key) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-load".to_string(),
            });
            loading.armed = false;
            ws.store_state(WorkspaceState::Failed);
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
        // returns the published `Arc<CodeGraph>` from
        // `publish_and_retain`, releases `workspaces_guard`, and
        // THEN invokes `on_publish` under a disjoint short-lived
        // `self.hook.read()` acquisition.
        //
        // `G_daemon_control_plane.md` §3.5 caller-migration table —
        // get_or_load (production caller 1). On post-build oversize,
        // surface `DaemonError::WorkspaceOversize`; admission bytes
        // are refunded by the reservation's RAII Drop on early
        // return.
        let (_token, published_arc) = match self.publish_and_retain(reservation, &ws, graph) {
            Ok((token, arc)) => (token, arc),
            Err(e) => {
                drop(workspaces_guard);
                ws.record_failure(clone_err(&e));
                loading.armed = false;
                ws.store_state(WorkspaceState::Failed);
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
        self.dispatch_publish_hook(&key.source_root, Arc::clone(&published_arc));

        Ok(published_arc)
    }

    /// Look up or insert a [`LoadedWorkspace`] for `key`. Returns
    /// the shared `Arc` so both the caller and the manager map
    /// reference the same state.
    fn get_or_insert_workspace(&self, key: &WorkspaceKey) -> Arc<LoadedWorkspace> {
        // Upgrade path — try a read first to avoid the write-lock
        // cost when the entry already exists.
        if let Some(ws) = self.workspaces.read().get(key) {
            return Arc::clone(ws);
        }
        let mut workspaces = self.workspaces.write();
        Arc::clone(
            workspaces
                .entry(key.clone())
                .or_insert_with(|| Arc::new(LoadedWorkspace::new(key.clone(), false))),
        )
    }

    /// Evict the least-recently-accessed non-pinned workspace, if
    /// any. Returns the evicted key on success, `None` if there are
    /// no eligible candidates.
    pub fn evict_lru(&self) -> Option<WorkspaceKey> {
        let candidate = {
            let workspaces = self.workspaces.read();
            workspaces
                .iter()
                .filter(|(_, ws)| {
                    !ws.pinned
                        && ws.load_state() != WorkspaceState::Evicted
                        && ws.load_state() != WorkspaceState::Unloaded
                })
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
    /// see [`Self::execute_eviction`] doc and STEP_6 iter-1 BLOCK.
    ///
    /// Returns `true` if the workspace was present, `false` if it
    /// was already absent.
    pub fn unload(&self, key: &WorkspaceKey) -> bool {
        let mut workspaces = self.workspaces.write();
        if !workspaces.contains_key(key) {
            return false;
        }
        // Drop graph + admission bytes under the same write lock
        // we will use for `remove`. Holding the lock across both
        // operations means external observers see EITHER "entry
        // present + Loaded" OR "entry absent" — never the "entry
        // present + Evicted but about to be removed" intermediate
        // state. (LRU eviction is a separate flow that DOES expose
        // the Evicted tombstone — that is the STEP_6 contract.)
        self.evict_to_tombstone_locked(&mut workspaces, key);
        workspaces.remove(key);
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
            return;
        }

        let old_arc = ws.graph.swap(Arc::new(CodeGraph::new()));
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

        ws.rebuild_cancelled.store(true, Ordering::Release);
        ws.store_state(WorkspaceState::Evicted);
    }

    /// Cluster-G §3.2 — reset a workspace to `Unloaded` *without*
    /// removing its manager-map entry.
    ///
    /// Drops the in-memory graph + admission bytes + retained
    /// old-graph entries owned by this workspace, but preserves the
    /// `WorkspaceKey`, `pinned` bit, and `last_error`. Files under
    /// `<root>/.sqry/` are left untouched — destructive cleanup is
    /// owned by `sqry workspace clean` (cluster-E IMP-E.4).
    ///
    /// Returns `Ok(true)` if the workspace was present and reset,
    /// `Ok(false)` if not present.
    ///
    /// State transitions:
    ///   `Loaded` / `Failed` / `Evicted` / `Unloaded` → `Unloaded`
    ///   `Rebuilding` → cancellation dispatched, [`Err(ResetCancellationDispatched)`]
    ///   `Loading`    → [`Err(ResetWhileLoading)`]
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
    /// - [`DaemonError::ResetCancellationDispatched`] when a rebuild
    ///   was in flight; the caller should retry after `retry_after_ms`.
    pub fn reset(self: &Arc<Self>, key: &WorkspaceKey, force: bool) -> Result<bool, DaemonError> {
        use crate::error::DaemonError;
        let mut workspaces = self.workspaces.write();
        let Some(ws) = workspaces.get(key).cloned() else {
            return Ok(false);
        };
        if ws.pinned && !force {
            return Err(DaemonError::WorkspacePinned {
                root: key.source_root.clone(),
            });
        }
        let current = ws.load_state();
        match current {
            WorkspaceState::Loading => {
                return Err(DaemonError::ResetWhileLoading {
                    root: key.source_root.clone(),
                });
            }
            WorkspaceState::Rebuilding => {
                ws.rebuild_cancelled.store(true, Ordering::Release);
                drop(workspaces);
                return Err(DaemonError::ResetCancellationDispatched {
                    root: key.source_root.clone(),
                    retry_after_ms: 250,
                });
            }
            _ => {}
        }
        // Drop the graph + refund admission bytes via the existing
        // tombstone helper, then transition to `Unloaded` (preserving
        // the map entry, `pinned`, and `last_error`).
        self.evict_to_tombstone_locked(&mut workspaces, key);
        // Cluster-G iter-2 BLOCKER 1: `evict_to_tombstone_locked`
        // sets `rebuild_cancelled = true` (`manager.rs:948`). Without
        // clearing it here, the next `get_or_load` from `Unloaded`
        // hits the `pre_cancelled && prior_state != Evicted` branch
        // at `manager.rs:693-704` and fails with `WorkspaceBuildFailed`
        // ("workspace evicted mid-load") — `daemon reset` would be
        // unable to recover the workspace it just reset (codex iter-1
        // review). Clear the flag now so the next reload starts from
        // a clean cancellation state.
        ws.rebuild_cancelled.store(false, Ordering::Release);
        ws.store_state(WorkspaceState::Unloaded);
        Ok(true)
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
            .find(|(k, _)| k.source_root == path)
            .map(|(k, ws)| (k.clone(), Arc::clone(ws)))
    }

    /// Snapshot of daemon-wide status. Point-in-time, non-transactional.
    pub fn status(&self) -> DaemonStatus {
        let workspaces_snapshot: Vec<WorkspaceStatus> = {
            let workspaces = self.workspaces.read();
            let mut entries: Vec<_> = workspaces
                .iter()
                .map(|(k, ws)| WorkspaceStatus {
                    index_root: k.source_root.clone(),
                    state: ws.load_state(),
                    pinned: ws.pinned,
                    current_bytes: ws.memory_bytes.load(Ordering::Acquire) as u64,
                    high_water_bytes: ws.memory_high_water_bytes.load(Ordering::Acquire) as u64,
                    last_good_at: *ws.last_good_at.read(),
                    last_error: ws.last_error.read().as_ref().map(|e| e.to_string()),
                    retry_count: ws.retry_count.load(Ordering::Acquire),
                    // STEP_12 telemetry: surface both display and machine
                    // identity hex forms when the key carries a logical
                    // workspace_id; anonymous keys leave both as None so
                    // the wire shape is uniform.
                    workspace_id_short: k.workspace_id.as_ref().map(|id| id.as_short_hex()),
                    workspace_id_full: k.workspace_id.as_ref().map(|id| id.as_full_hex()),
                })
                .collect();
            entries.sort_by(|a, b| a.index_root.cmp(&b.index_root));
            entries
        };

        let (current_bytes, reserved_bytes, high_water_bytes) = {
            let state = self.admission.lock();
            let current = state.total_committed_bytes();
            let reserved = state.reserved_bytes;
            // Bump high-water here in case the status read saw a
            // higher value than the last mutation captured. The
            // `drop(state)` at the end of this block keeps the
            // admission lock held across the `fetch_max` — serialising
            // the high-water update with any concurrent publish.
            let peak = self
                .total_memory_high_water
                .fetch_max(current, Ordering::AcqRel);
            let peak = peak.max(current);
            drop(state);
            (current, reserved, peak)
        };

        DaemonStatus {
            uptime_seconds: self.started_at.elapsed().as_secs(),
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            memory: MemoryStatus {
                limit_bytes: self.memory_limit_bytes(),
                current_bytes,
                reserved_bytes,
                high_water_bytes,
            },
            workspaces: workspaces_snapshot,
        }
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
    /// `workspace_id` (STEP_6 of the workspace-aware-cross-repo plan).
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
        self.workspaces.write().insert(key, Arc::clone(&ws));
        ws
    }

    /// Acquire the internal `workspaces` RwLock in read mode.
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
    /// last_good, and last_error_text are all captured inside the
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
        //   - ws.graph.swap(CodeGraph::new())
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
            let Some(ws) = workspaces.get(key).cloned() else {
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            };
            let state = ws.load_state();
            let graph = ws.graph.load_full();
            let last_good = *ws.last_good_at.read();
            let last_error_text = ws.last_error.read().as_ref().map(|e| e.to_string());
            (state, graph, last_good, last_error_text)
            // workspaces.read() dropped here — the (state, graph)
            // pair is now a coherent snapshot taken atomically w.r.t.
            // execute_eviction's workspaces.write().
        };
        let (state, graph, last_good, last_error_text) = snapshot;

        match state {
            WorkspaceState::Loaded | WorkspaceState::Rebuilding => {
                Ok(ServeVerdict::Fresh { graph, state })
            }
            WorkspaceState::Failed => {
                let cap = self.config.stale_serve_max_age_hours;
                match classify_staleness(last_good, cap, now) {
                    StalenessVerdict::NoPriorGood => Err(DaemonError::WorkspaceBuildFailed {
                        root: key.source_root.clone(),
                        reason: last_error_text
                            .unwrap_or_else(|| "no prior successful build".into()),
                    }),
                    StalenessVerdict::Stale { age_hours } => Ok(ServeVerdict::Stale {
                        graph,
                        age_hours,
                        // Invariant: `classify_staleness` only returns
                        // `Stale` when `last_good.is_some()` (see
                        // `workspace/staleness.rs:54-73`).
                        last_good_at: last_good
                            .expect("Stale verdict only emitted when last_good.is_some()"),
                        last_error: last_error_text,
                    }),
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
    /// tests, together with an `Arc<CodeGraph>` handle to the freshly
    /// published graph. Per Codex Task 6 Phase 6c iter-2 MAJOR the
    /// post-publish `SqrydHook` dispatch is NOT performed here —
    /// firing `on_publish` under the `workspaces.read()` guard
    /// `get_or_load` holds across this call would nest
    /// `self.hook.read()` inside `workspaces`, giving hook impls a
    /// re-entrancy deadlock hole if they call back into manager
    /// methods needing `workspaces.write()`. The caller is
    /// responsible for dispatching the hook after dropping every
    /// outer workspaces-lock holder.
    pub fn publish_and_retain(
        self: &Arc<Self>,
        reservation: RebuildReservation,
        workspace: &LoadedWorkspace,
        new_graph: CodeGraph,
    ) -> Result<(OldGraphToken, Arc<CodeGraph>), DaemonError> {
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
        // Clone the Arc BEFORE the swap so the caller can still
        // obtain a handle to the published graph after the swap
        // moves `new_arc` into the ArcSwap. Re-reading via
        // `workspace.graph.load_full()` after the swap would work
        // today but is racy against any future swap path that
        // could run between the swap and the load — cheaper and
        // safer to clone the Arc once.
        let published_arc = Arc::clone(&new_arc);
        let token = OldGraphToken::new();

        // --- RollbackGuard setup --------------------------------
        let prior_arc_for_rollback = workspace.graph.load_full();
        let prior_bytes = workspace
            .memory_bytes
            .load(std::sync::atomic::Ordering::Acquire);

        let mut rollback = RollbackGuard {
            ws: workspace,
            prior_arc: Some(prior_arc_for_rollback),
            prior_bytes,
            armed: true,
        };

        // --- Non-recoverable zone (no .await; no fallible ops) ---
        //
        // If any code between this point and `reservation.released = true`
        // panics, the following Drop order runs on unwind:
        //   1. `rollback` Drop reverts `workspace.graph` and
        //      `workspace.memory_bytes` to the pre-swap values
        //      (because `armed == true`).
        //   2. `reservation` Drop reacquires the admission mutex and
        //      refunds `reservation_bytes` back to `reserved_bytes`
        //      (because `released == false`).
        // This is the §G.5 invariant-preserving rollback described in
        // the plan; the reservation refund was missing before the
        // iter-1 fix.
        let old_arc = workspace.graph.swap(new_arc);
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
        // no retained entry existed — rollback reverts ws.graph +
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
        // ws.graph + ws.memory_bytes, then the `reservation`
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
        Ok((token, published_arc))
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
    /// it observes [`DaemonError::WorkspaceEvicted`] (or an `Unloaded`
    /// state) for a [`AcquisitionOperation::ReadOnlyQuery`].
    ///
    /// Behaviour contract:
    ///
    /// 1. Drives the same lifecycle CAS gate as
    ///    [`Self::get_or_load`] — only one caller can rehydrate per
    ///    workspace at a time.
    /// 2. Reserves admission headroom via [`Self::reserve_rebuild`].
    /// 3. Calls [`WorkspaceBuilder::load_persisted`] to read
    ///    `<source_root>/.sqry/graph/snapshot.sqry`. Never calls
    ///    `WorkspaceBuilder::build`, never mutates `.sqry/graph/*`,
    ///    `.sqry/analysis/*`, or `derived.sqry`, and never invokes the
    ///    post-publish hook (the snapshot is bit-identical with what
    ///    the hook would produce — no fresh derived cache to warm).
    /// 4. Publishes through [`Self::publish_and_retain`] under the
    ///    standard `workspaces.read()` re-check + cancellation gate
    ///    so eviction races are caught the same way as `get_or_load`.
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
    ) -> Result<Arc<CodeGraph>, DaemonError> {
        // --- Step 1: cache-hit fast path ---------------------------
        {
            let workspaces = self.workspaces.read();
            if let Some(ws) = workspaces.get(key)
                && ws.load_state() == WorkspaceState::Loaded
            {
                ws.touch();
                return Ok(ws.graph.load_full());
            }
        }

        // --- Step 2: lifecycle CAS gate (mirrors get_or_load) -------
        let ws = self.get_or_insert_workspace(key);
        let allowed = [
            WorkspaceState::Unloaded.as_u8(),
            WorkspaceState::Failed.as_u8(),
            WorkspaceState::Evicted.as_u8(),
        ];
        let mut acquired_from: Option<WorkspaceState> = None;
        for prior in allowed {
            if ws
                .state
                .compare_exchange(
                    prior,
                    WorkspaceState::Loading.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                acquired_from = WorkspaceState::from_u8(prior);
                break;
            }
        }
        let Some(prior_state) = acquired_from else {
            let current = ws.load_state();
            if current == WorkspaceState::Loaded {
                ws.touch();
                return Ok(ws.graph.load_full());
            }
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: format!("workspace load already in progress ({current})"),
            });
        };

        let pre_cancelled = ws.rebuild_cancelled.swap(false, Ordering::AcqRel);
        if pre_cancelled && prior_state != WorkspaceState::Evicted {
            ws.rebuild_cancelled.store(true, Ordering::Release);
            ws.store_state(WorkspaceState::Failed);
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-load".to_string(),
            });
        }

        // --- Step 3: arm LoadingGuard for panic / early-return ----
        let mut loading = LoadingGuard {
            ws: &ws,
            key,
            armed: true,
        };

        // --- Step 4: reserve admission headroom -------------------
        let reservation = self.reserve_rebuild(key, working_set_estimate)?;

        // --- Step 5: load_persisted (read-only, no build pipeline)
        let graph = match builder.load_persisted(&key.source_root) {
            Ok(g) => g,
            Err(err) => {
                drop(reservation);
                ws.record_failure(clone_err(&err));
                loading.armed = false;
                ws.store_state(WorkspaceState::Failed);
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
            ws.store_state(WorkspaceState::Failed);
            return Err(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace evicted mid-reload".to_string(),
            });
        }
        if !workspaces_guard.contains_key(key) {
            drop(workspaces_guard);
            drop(reservation);
            ws.record_failure(DaemonError::WorkspaceBuildFailed {
                root: key.source_root.clone(),
                reason: "workspace removed mid-reload".to_string(),
            });
            loading.armed = false;
            ws.store_state(WorkspaceState::Failed);
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
        let (_token, published_arc) = match self.publish_and_retain(reservation, &ws, graph) {
            Ok((token, arc)) => (token, arc),
            Err(e) => {
                drop(workspaces_guard);
                ws.record_failure(clone_err(&e));
                loading.armed = false;
                ws.store_state(WorkspaceState::Failed);
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

        Ok(published_arc)
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
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn evict_for_test(&self, key: &WorkspaceKey) -> bool {
        let present = self.workspaces.read().contains_key(key);
        if !present {
            return false;
        }
        self.execute_eviction(key);
        true
    }
}

impl Drop for WorkspaceManager {
    fn drop(&mut self) {
        self.shutdown_reaper();
    }
}

/// STEP_11_4 — probe `<source_root>/.sqry/classpath/` for presence at
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

impl<'a> Drop for LoadingGuard<'a> {
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
        self.ws.store_state(WorkspaceState::Failed);
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
        // Task 9 U1 — lifecycle variants (AlreadyRunning, AutoStartTimeout,
        // SignalSetup). These errors all fire before IpcServer::bind and
        // therefore before any workspace is registered; they should never
        // reach `clone_err`. If they somehow do (e.g. a future code path
        // stores them in `last_error`), collapse to WorkspaceBuildFailed so
        // the clone contract is preserved without losing observability.
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
        // sqry-mcp flakiness P0/P1 admission + recovery variants
        // (G_daemon_control_plane.md §1.4 / §3.2 / §5.2 +
        // B_cost_gate.md §3 + 00_contracts.md §3.CC-2 / §3.CC-4).
        // Each carries cheap Clone-able payload; round-trip in place.
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
        other @ (DaemonError::Config { .. } | DaemonError::Io(_)) => {
            DaemonError::WorkspaceBuildFailed {
                root: Path::new("<unknown>").to_path_buf(),
                reason: other.to_string(),
            }
        }
    }
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
            .finish()
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
/// Captures the prior `Arc<CodeGraph>` and the prior `memory_bytes`
/// *before* any swap. If the thread unwinds between the swap and the
/// admission-mutex acquisition, the guard's `Drop` restores both
/// fields — leaving the workspace serving its pre-rebuild graph as if
/// the publish never happened.
///
/// Correctness depends on three contracts:
///
/// 1. The guard is constructed *before* the `ArcSwap::swap` call.
/// 2. `armed` is set to `false` only on the success path, after the
///    admission mutex has released.
/// 3. No fallible operation (heap allocation failure, etc.) runs
///    between the two swaps — otherwise the guard would be asked to
///    reverse a partial swap.
pub(crate) struct RollbackGuard<'a> {
    pub(crate) ws: &'a LoadedWorkspace,
    pub(crate) prior_arc: Option<Arc<CodeGraph>>,
    pub(crate) prior_bytes: usize,
    pub(crate) armed: bool,
}

impl<'a> Drop for RollbackGuard<'a> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(arc) = self.prior_arc.take() {
            self.ws.graph.store(arc);
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
    use std::{path::PathBuf, sync::atomic::Ordering};

    use sqry_core::project::ProjectRootMode;

    use crate::config::DaemonConfig;

    use super::{
        super::{loaded::LoadedWorkspace, state::WorkspaceKey},
        *,
    };

    fn make_config() -> Arc<DaemonConfig> {
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
        let (token, _published_arc) = mgr
            .publish_and_retain(reservation, &ws, new_graph)
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
        ws.graph.store(Arc::clone(&old_graph));
        ws.memory_bytes.store(10_000, Ordering::Release);

        {
            let mut guard = RollbackGuard {
                ws: &ws,
                prior_arc: Some(Arc::clone(&old_graph)),
                prior_bytes: 10_000,
                armed: true,
            };

            // Simulate a partial publish: swap the ArcSwap + memory_bytes.
            let stomped = Arc::new(CodeGraph::new());
            ws.graph.store(Arc::clone(&stomped));
            ws.memory_bytes.store(99_999, Ordering::Release);

            // `armed == true` so the guard reverses both fields on drop.
            // Flip the disarm check intentionally OFF — mimics panic path.
            let _ = &mut guard;
        }

        // After the guard drops, both fields must match the prior.
        let restored = ws.graph.load_full();
        assert!(Arc::ptr_eq(&restored, &old_graph));
        assert_eq!(ws.memory_bytes.load(Ordering::Acquire), 10_000);
    }

    #[test]
    fn rollback_guard_disarmed_is_noop() {
        let ws = make_workspace();
        let old_graph = Arc::new(CodeGraph::new());
        ws.graph.store(Arc::clone(&old_graph));
        ws.memory_bytes.store(10_000, Ordering::Release);

        {
            let mut guard = RollbackGuard {
                ws: &ws,
                prior_arc: Some(Arc::clone(&old_graph)),
                prior_bytes: 10_000,
                armed: true,
            };
            let stomped = Arc::new(CodeGraph::new());
            ws.graph.store(Arc::clone(&stomped));
            ws.memory_bytes.store(99_999, Ordering::Release);

            // Success path disarms the guard.
            guard.armed = false;
        }

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
        mgr.publish_and_retain(reservation, &ws, CodeGraph::new())
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
        mgr.publish_and_retain(reservation, &ws, CodeGraph::new())
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
        let (_token, _published_arc) = mgr
            .publish_and_retain(reservation, &ws, CodeGraph::new())
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
        // loaded_bytes untouched, retained_old empty, workspace.graph
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
        let prior_arc = ws.graph.load_full();

        // Reserve headroom as the real function does.
        let reservation = mgr
            .reserve_rebuild(&ws.key, 100_000)
            .expect("reservation fits");
        assert_eq!(mgr.admission.lock().reserved_bytes, 100_000);

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            // Mirror `publish_and_retain` up to and INCLUDING the
            // ArcSwap swap + update_memory, then panic *before* we
            // would have acquired the admission mutex. This is the
            // exact unwind window the iter-2 finding describes.
            let new_arc = Arc::new(CodeGraph::new());
            let prior_arc_clone = ws.graph.load_full();
            // The guard is armed and has no visible use after this
            // point; its Drop is the entire reason the scope exists,
            // so the binding is deliberately underscore-prefixed and
            // held until the panic unwinds the stack.
            let _rollback = RollbackGuard {
                ws: &ws,
                prior_arc: Some(prior_arc_clone),
                prior_bytes: prior_bytes_usize,
                armed: true,
            };
            let _old_arc = ws.graph.swap(new_arc);
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
        let restored = ws.graph.load_full();
        assert!(
            Arc::ptr_eq(&restored, &prior_arc),
            "RollbackGuard must restore ws.graph to the prior Arc after unwind",
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
        assert_eq!(status.memory.limit_bytes, 1024 * 1024);
        // current_bytes is at least as large as the graph (empty here,
        // but loaded_bytes tracks an entry regardless).
        assert!(
            status.memory.high_water_bytes >= status.memory.current_bytes,
            "high_water_bytes must be monotonic wrt current_bytes",
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
        let slow_query_arc = ws.graph.load_full();

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
            fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
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
        let mgr = WorkspaceManager::new(make_config());
        let ws = make_workspace();
        mgr.workspaces
            .write()
            .insert(ws.key.clone(), Arc::clone(&ws));
        let reservation = mgr
            .reserve_rebuild(&ws.key, 0)
            .expect("zero-size reservation always fits");
        mgr.publish_and_retain(reservation, &ws, CodeGraph::new())
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
}
