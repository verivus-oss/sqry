//! [`LoadedWorkspace`] — per-workspace runtime state.
//!
//! Corresponds to Task 6 Step 2 of the sqryd plan, plus the
//! Amendment-2 additions:
//!
//! - `memory_high_water_bytes` (§D) — monotonic peak over the loaded
//!   lifetime; reset only on unload/eviction, never on rebuilds.
//! - `last_good_at` (§C) — stamped on every successful build; the
//!   stale-serve router uses this to enforce the
//!   `stale_serve_max_age_hours` cap and surface JSON-RPC `-32002`
//!   on expiry.
//! - `rebuild_cancelled` (§J) — lock-free cancellation signal used by
//!   the dispatcher's background rebuild task to abort at pass
//!   boundaries when the workspace is evicted mid-rebuild.
//! - `rebuild_lane` (§J) — at most one queued rebuild per workspace.
//! - `rebuild_in_flight` (§J, Task 7 Phase 7b1) — runner-role gate that
//!   serializes `RebuildDispatcher::handle_changes` callers per
//!   workspace. Transitions happen under `rebuild_lane` on the normal
//!   path; `DrainLoopSentinel` is the sole recovery exception.
//!
//! [`ArcSwap<PublishedGraph>`] owns the published generation: the graph
//! and the roster record it was built with, as one value (surface parity
//! W1 round 3, design D14). Queries take [`LoadedWorkspace::published`]
//! (or [`LoadedWorkspace::graph`]) to get a stable `Arc` that survives a
//! concurrent `publish_and_retain` swap; the retention reaper is
//! responsible for eventually dropping the superseded graph `Arc`. Because
//! the pair is one slot, a reader never observes one generation's graph
//! beside another generation's record: there is no second store to run
//! between.
//!
//! `pinned` workspaces are LRU-exempt. They are still counted against
//! `memory_bytes` / `total_memory` — a pinned workspace cannot push
//! the daemon over its budget; admission rejects the load / rebuild
//! per §G.7 if the only way to fit is to evict the pin itself.

use std::{
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    time::{Instant, SystemTime},
};

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::MacroOptionsRequest;
use sqry_core::watch::{ChangeSet, LastIndexedGitState};
use sqry_daemon_protocol::ResidentHandleKind;

use crate::error::DaemonError;
use crate::rebuild::RebuildReport;

use super::roster::RosterRecord;
use super::state::{WorkspaceKey, WorkspaceState};

/// Rebuild-lane entry enqueued by the Task 7 `RebuildDispatcher`.
///
/// The dispatcher's per-workspace call path (A2 §J) holds at most one
/// pending rebuild per workspace. When a new `ChangeSet` arrives while
/// another rebuild is in flight, the two are coalesced via
/// [`Self::coalesce_with`] — union of changed files, OR of
/// `git_state_changed`, max of `enqueued_at`, full-rebuild-dominance
/// merge on `git_change_class`, and absorb-None + later-wins merge on
/// `git_state_at_enqueue`.
#[derive(Debug, Clone)]
pub struct PendingRebuild {
    /// The coalesced [`ChangeSet`] waiting to be processed. Carries
    /// every file path observed so far plus the worst-case git-state
    /// classification across all enqueues.
    pub changes: ChangeSet,
    /// Wall-clock `Instant` of the most recent enqueue. Updated to
    /// `max(prior, incoming)` on every coalesce — A2 §J.2.
    pub enqueued_at: Instant,
    /// Git-state snapshot captured by the watcher bridge at the moment
    /// it received this change (Task 7 Phase 7b2). When the runner
    /// publishes a graph produced from this `PendingRebuild`, it
    /// commits this snapshot to
    /// [`LoadedWorkspace::last_indexed_git_state`] — tying baseline
    /// advance to actual publish consumption rather than a
    /// bridge-side proxy counter.
    ///
    /// `None` for callers that do not attach a git-state snapshot
    /// (direct tests, future IPC `workspace/force_rebuild`). The
    /// runner leaves the baseline untouched when consuming a `None`
    /// entry.
    ///
    /// Merge rule under [`Self::coalesce_with`]: absorb-None from
    /// either side, later wins when both are `Some`.
    pub git_state_at_enqueue: Option<LastIndexedGitState>,
    /// The macro build options this rebuild was asked for (surface parity
    /// W4, design W4-D7): empty for a watcher-driven rebuild (reuse the
    /// manifest's record), explicit for a `daemon/rebuild` call (or a
    /// daemon-hosted `rebuild_index`) that carried `cfg_flags`,
    /// `expand_cache` or `reset_macro_options`. A request that arrives
    /// through `RebuildDispatcher::handle_changes_with_macro_options` is
    /// normalised there before it parks (a plain relative expand cache
    /// directory anchored to the workspace root and canonicalised).
    ///
    /// Merge rule (decision D-i7-1 in
    /// `docs/development/surface-parity/04_PROGRESS-surface-parity.md`,
    /// which replaces D-w4r1-8): the dispatcher merges two entries only
    /// when their requests mean the same (the same reset, the same
    /// directory, the same cfg flags as a set) or when one side is a
    /// watcher-driven enqueue (no waiters and an empty request); otherwise
    /// it refuses the later request (`-32602`), so no caller is answered
    /// for options it did not ask for. Under [`Self::coalesce_with`] an
    /// empty later request keeps the earlier one and a later non-empty
    /// request replaces it, which is exact for every pair the dispatcher
    /// merges.
    pub macro_request: MacroOptionsRequest,
    /// The callers waiting on this rebuild's own outcome. Empty for a
    /// watcher-driven rebuild; one entry per `daemon/rebuild` call (or
    /// daemon-hosted `rebuild_index`) merged into it. Merge rule under
    /// [`Self::coalesce_with`]: union.
    pub waiters: RebuildWaiters,
    /// Who asked for this rebuild, for the provenance its durable persist
    /// records (the manifest's `build_command`) and the refusal policy it
    /// runs with. Merge rule under [`Self::coalesce_with`]:
    /// [`RebuildRequester::merge`], so an explicit caller's label is kept
    /// over the watcher's whichever parked first, and a `daemon/rebuild`
    /// merged with a `rebuild_index` keeps `daemon/rebuild`'s refusal.
    pub requester: RebuildRequester,
}

/// Who asked for a queued rebuild. The durable persist records it as the
/// manifest's `build_command`: `daemon:rebuild_index` for the daemon-hosted
/// `rebuild_index` (as its load route records), `daemon:rebuild:full` or
/// `daemon:rebuild:incremental` (the mode) for the others. Ordered by
/// precedence when two requests merge into one iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RebuildRequester {
    /// The file watcher, or a direct caller that names none.
    #[default]
    Watcher,
    /// `daemon/rebuild` (`sqry daemon rebuild`).
    DaemonRebuild,
    /// The daemon-hosted `rebuild_index` over a resident workspace.
    RebuildIndex,
    /// A `rebuild_index` and a `daemon/rebuild` merged into one iteration.
    /// It records `rebuild_index`'s provenance, but refuses an unreadable
    /// manifest as `daemon/rebuild` does: a merge never gives the
    /// `daemon/rebuild` caller the fall-back only `rebuild_index` asked for
    /// (decision D-i8-40, audit S3).
    RebuildIndexWithDaemonRebuild,
}

impl RebuildRequester {
    /// The requester of the iteration two merged requests share: the later
    /// in precedence order, except that `daemon/rebuild` and
    /// `rebuild_index` together give [`Self::RebuildIndexWithDaemonRebuild`]
    /// (the provenance of the one, the refusal policy of the other).
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::DaemonRebuild, Self::RebuildIndex)
            | (Self::RebuildIndex, Self::DaemonRebuild) => Self::RebuildIndexWithDaemonRebuild,
            (a, b) => a.max(b),
        }
    }

    /// Whether this rebuild falls back to the fast-path default roster over
    /// a manifest it cannot read: only a `rebuild_index` no `daemon/rebuild`
    /// merged into (design D9, decision D-i7-8). Every other requester
    /// refuses.
    #[must_use]
    pub const fn falls_back_over_an_unreadable_manifest(self) -> bool {
        matches!(self, Self::RebuildIndex)
    }

    /// The `build_command` a rebuild this requester asked for records,
    /// for an iteration that ran in `mode`.
    #[must_use]
    pub const fn build_command(self, mode: crate::rebuild::RebuildMode) -> &'static str {
        match (self, mode) {
            (Self::RebuildIndex | Self::RebuildIndexWithDaemonRebuild, _) => "daemon:rebuild_index",
            (_, crate::rebuild::RebuildMode::Full) => "daemon:rebuild:full",
            (_, crate::rebuild::RebuildMode::Incremental) => "daemon:rebuild:incremental",
        }
    }
}

/// The callers waiting on one queued rebuild's own outcome (integration of
/// W1 and W4). A `daemon/rebuild` call parked behind a running rebuild used
/// to be told `Completed` whatever its own iteration did, so a refused
/// request read as a success. The runner now delivers each iteration's
/// result to the waiters of the [`PendingRebuild`] it consumed.
///
/// Each sender is taken once. Clones of a `PendingRebuild` share the slots,
/// so a waiter receives at most one result however often the entry is
/// cloned.
#[derive(Debug, Clone, Default)]
pub struct RebuildWaiters(Vec<WaiterSlot>);

/// One waiter's sender, taken by the first delivery.
type WaiterSlot = Arc<parking_lot::Mutex<Option<OutcomeSender>>>;

/// The sending half a waiter's caller receives its outcome on: the report
/// of the iteration that consumed its request, or that iteration's error.
type OutcomeSender = tokio::sync::oneshot::Sender<Result<RebuildReport, DaemonError>>;

impl RebuildWaiters {
    /// One waiter, the receiving half of which its caller holds.
    #[must_use]
    pub fn one(sender: OutcomeSender) -> Self {
        Self(vec![Arc::new(parking_lot::Mutex::new(Some(sender)))])
    }

    /// Append the waiters of a request merged into this one.
    pub fn absorb(&mut self, later: RebuildWaiters) {
        self.0.extend(later.0);
    }

    /// Number of waiters still holding an undelivered sender.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.0.iter().filter(|slot| slot.lock().is_some()).count()
    }

    /// `true` when no caller waits on this entry (a watcher-driven
    /// enqueue, or an entry whose waiters were all merged elsewhere).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Deliver `result` to every waiter not yet delivered to. A waiter
    /// whose receiver is gone (its caller timed out) is skipped.
    pub fn deliver(&self, result: &Result<RebuildReport, DaemonError>) {
        for slot in &self.0 {
            if let Some(sender) = slot.lock().take() {
                let _ = sender.send(match result {
                    Ok(report) => Ok(report.clone()),
                    Err(err) => Err(super::manager::clone_err(err)),
                });
            }
        }
    }
}

/// One published generation: a graph and the roster record it was built
/// with (surface parity W1 round 3, design D14).
///
/// `LoadedWorkspace` holds exactly one of these behind an [`ArcSwap`], so
/// publishing is one swap and reading is one load; the two halves cannot
/// be observed from different generations. `roster` is `None` only for
/// the placeholder that stands in before the first publish and after
/// eviction, both states `classify_for_serve` refuses to serve.
#[derive(Debug)]
pub struct PublishedGraph {
    /// The graph queries run against.
    pub graph: Arc<CodeGraph>,
    /// The record of the plugin roster `graph` was built with.
    pub roster: Option<Arc<RosterRecord>>,
}

impl PublishedGraph {
    /// A generation built from `graph` with the record `roster`.
    #[must_use]
    pub fn new(graph: Arc<CodeGraph>, roster: Option<Arc<RosterRecord>>) -> Self {
        Self { graph, roster }
    }

    /// The pre-first-publish and post-eviction generation: an empty graph
    /// with no record. Cheap (the empty graph is sub-kilobyte), so the
    /// slot stays non-null for the whole workspace lifetime and the query
    /// path needs no `Option` layer.
    #[must_use]
    pub fn placeholder() -> Self {
        Self {
            graph: Arc::new(CodeGraph::new()),
            roster: None,
        }
    }
}

/// Per-workspace runtime state owned by the
/// [`super::WorkspaceManager::workspaces`] map.
///
/// Mutating state:
///
/// - `published`, `state`, `memory_bytes`, `memory_high_water_bytes`,
///   `retry_count`, `rebuild_cancelled` are all atomic — queries and
///   status readers can observe them without taking a mutex.
/// - `last_accessed`, `last_error`, `last_good_at` are short-critical-
///   section `RwLock`s — writers are the dispatcher / router at
///   publish/fail time.
/// - `rebuild_lane` is a `tokio::sync::Mutex` per A2 §J.4; the
///   dispatcher holds it briefly to coalesce pending work.
///
/// Construction is expensive only in that [`ArcSwap::new`] allocates
/// one `Arc<CodeGraph>` up front. A fresh workspace is initialised with
/// an empty-but-live [`CodeGraph`]; the real graph is installed by the
/// first `publish_and_retain`.
#[derive(Debug)]
pub struct LoadedWorkspace {
    /// Identity key under which the manager stores this workspace.
    ///
    /// Immutable for the lifetime of the workspace; if the config
    /// fingerprint or root mode change, the workspace is unloaded
    /// under the old key and freshly loaded under a new key.
    pub key: WorkspaceKey,

    /// The published generation: the graph and the roster record it was
    /// built with, as one value (surface parity W1 round 3, design D14).
    /// Readers call [`Self::published`] for the coherent pair, or
    /// [`Self::graph`] / [`Self::roster`] for one half of it.
    ///
    /// Only `WorkspaceManager::publish_and_retain` swaps a new generation
    /// in, with exactly one `swap`; eviction swaps in
    /// [`PublishedGraph::placeholder`] so the `ArcSwap` remains non-null
    /// (simpler than Option-wrapping). The record is `None` only in the
    /// placeholder, both of whose states `classify_for_serve` refuses.
    pub published: ArcSwap<PublishedGraph>,

    /// Current lifecycle state. Stored as `AtomicU8` to keep the
    /// status read path lock-free; round-trip via
    /// [`WorkspaceState::as_u8`] / [`WorkspaceState::from_u8`].
    pub state: std::sync::atomic::AtomicU8,

    /// Last wall-clock time a query observed this workspace, used by
    /// LRU eviction (§G.7). Short critical section; `RwLock` keeps
    /// contention negligible.
    pub last_accessed: RwLock<Instant>,

    /// Current `heap_bytes` of the published graph. Updated on every
    /// successful `publish_and_retain`. Reads use `Relaxed` ordering —
    /// the authoritative aggregate lives on
    /// [`super::admission::AdmissionState`].
    pub memory_bytes: AtomicUsize,

    /// Peak `memory_bytes` observed over this workspace's loaded
    /// lifetime. Per Amendment 2 §D:
    ///
    /// > High-water marks are monotonic over the workspace's loaded
    /// > lifetime — they reset only on unload/eviction (fresh
    /// > LoadedWorkspace), not on rebuilds or backoff.
    ///
    /// Updated via `fetch_max` alongside every `memory_bytes` store.
    pub memory_high_water_bytes: AtomicUsize,

    /// Whether LRU eviction must skip this workspace.
    pub pinned: bool,

    /// Most recent build/load error, if any. `None` in the
    /// [`WorkspaceState::Loaded`] steady state. Populated on transition
    /// into [`WorkspaceState::Failed`] and read back by the router
    /// when surfacing `meta.last_error` on stale responses.
    pub last_error: RwLock<Option<DaemonError>>,

    /// Wall-clock of the most recent successful rebuild. Used by the
    /// router to compute `age_hours` for the
    /// `stale_serve_max_age_hours` cap and the JSON-RPC `-32002`
    /// error `error.data.age_hours` payload.
    pub last_good_at: RwLock<Option<SystemTime>>,

    /// Count of consecutive failed rebuilds. Drives the exponential
    /// backoff schedule (§G.7 / plan Step 6: 30s → 60s → 120s → 300s
    /// → 600s). Reset to 0 on every successful publish.
    pub retry_count: AtomicU32,

    /// At most one queued rebuild per workspace. `None` when the
    /// dispatcher's lane is idle. Filled / coalesced by the
    /// dispatcher (Task 7). Every runner-role transition of
    /// [`Self::rebuild_in_flight`] happens under it, so a holder reads
    /// whether a runner is in flight as a fact that cannot change while it
    /// holds the lane: `RebuildDispatcher::cancel_rebuild` and
    /// `WorkspaceManager::reset` (with `try_lock`) rely on that. No holder
    /// awaits while holding it.
    pub rebuild_lane: tokio::sync::Mutex<Option<PendingRebuild>>,

    /// Lock-free cancellation signal for in-flight rebuilds. Set by the
    /// tombstone writers (LRU eviction, `unload`, `reset`; `reset` clears it
    /// again before it returns), by `daemon/cancel_rebuild`
    /// (`RebuildDispatcher::cancel_rebuild`) and `daemon/reset`
    /// (`WorkspaceManager::reset`), each under the rebuild lane and only
    /// while a runner holds the role, and by daemon shutdown through
    /// `cancel_rebuild`. So no writer but an eviction leaves it set with no
    /// runner to consume it, and an eviction's flag is the next load's to
    /// consume. Polled by the rebuild
    /// pipeline at each pass boundary, by the reservation and by the
    /// publish recheck. Once set, the running rebuild aborts, drops its
    /// `RebuildReservation`, and never publishes; the runner then consumes
    /// the flag at its cancellation gate, except after a completed eviction,
    /// whose flag the next load consumes. It does not stop the file watcher:
    /// that is [`Self::watcher_stop`].
    pub rebuild_cancelled: AtomicBool,

    /// Whether the last eviction to tombstone this workspace found it
    /// `Rebuilding`: a runner's iteration between its entry and its
    /// publish, which the eviction's [`Self::rebuild_cancelled`] must reach
    /// before it publishes. Written by every tombstone write
    /// (`WorkspaceManager::evict_to_tombstone_locked`) under
    /// `workspaces.write()`, before the `Evicted` store; read by the load
    /// gate (`WorkspaceManager::honor_preexisting_cancel`), which leaves
    /// the flag to that runner instead of consuming it (round 8 review,
    /// note (a)). An eviction that found the slot `Loaded` or `Failed`
    /// (a runner between iterations, which enters its next one only from
    /// those states and never from `Evicted` or `Loading`) writes `false`.
    pub evicted_mid_iteration: AtomicBool,

    /// The stop signal of the file watcher attached to this workspace
    /// (each watcher gets its own, from [`Self::arm_watcher_stop`], which
    /// sets the one it replaces). Set by [`Self::stop_watcher`], which the
    /// tombstone writers (eviction, `unload`, `reset`) and daemon shutdown
    /// call: cancelling a rebuild must not stop the watcher, so the watcher
    /// does not poll [`Self::rebuild_cancelled`]. A set signal also refuses
    /// the watcher's own enqueues under the rebuild lane
    /// (`RebuildDispatcher::handle_changes_with_git_state`).
    pub watcher_stop: parking_lot::Mutex<Arc<AtomicBool>>,

    /// Whether this workspace is meant to be watched: set when a loader
    /// starts its watcher (`RebuildDispatcher::start_watching`: `daemon/load`,
    /// the pinned pre-load at startup, the daemon-hosted `rebuild_index`).
    /// An LRU eviction stops the watcher but keeps the intent, so the
    /// read-only reload that makes the workspace resident again
    /// (`tool_core::acquire_and_execute`) starts the watcher again rather
    /// than serve a graph no edit refreshes. A workspace only ever made
    /// resident by such a reload (a query on a root nobody loaded) has no
    /// intent and is not watched. `unload` removes the entry, and the intent
    /// with it; a reset workspace is not reloaded by a query (`Unloaded` is
    /// not served), and the next load sets the intent again.
    pub watch_wanted: AtomicBool,

    /// Runner-role gate for the per-workspace rebuild serial consumer
    /// (A2 §J.2, Task 7 Phase 7b1). When `true`, exactly one drain loop
    /// (Phase B in `rebuild.rs`, on the caller's task for
    /// [`crate::RebuildDispatcher::handle_changes`], on a spawned task for
    /// `handle_changes_with_macro_options`) is actively running the full
    /// rebuild pipeline. A concurrent caller observing `true` under the
    /// [`Self::rebuild_lane`] lock MUST park its merged [`PendingRebuild`]
    /// in the lane and return `Ok(())` without executing (the active
    /// runner will drain the lane at its next drain-loop iteration), or be
    /// refused when its request does not merge with the parked one
    /// (`PendingRebuild::merges_with`).
    ///
    /// # Invariant
    ///
    /// All normal-path transitions of this flag happen while
    /// [`Self::rebuild_lane`] is held. `DrainLoopSentinel::drop` in
    /// `rebuild.rs` is the sole recovery exception — it stores `false`
    /// without the lane on the unwind path, with the narrow-race
    /// semantics documented on that type.
    ///
    /// # Authorised modifiers
    ///
    /// - `RebuildDispatcher::acquire_or_park` (Phase A, `false → true`
    ///   under lane).
    /// - `RebuildDispatcher::drain` exits (Phase B, `true → false` under
    ///   lane, when the lane is empty and no cancellation is pending, or
    ///   when the cancellation gate fires).
    /// - `DrainLoopSentinel::drop` panic-safety path (`true → false`,
    ///   under the lane when `try_lock` gets it, otherwise a bare atomic
    ///   store with the narrow race documented there).
    ///
    /// `RebuildDispatcher::cancel_rebuild` reads it under the lane.
    ///
    /// Nothing in `WorkspaceManager` (`execute_eviction`,
    /// `publish_and_retain`, the retention reaper) touches this flag.
    /// It is dispatcher-local coordination, independent of the
    /// `rebuild_cancelled` eviction signal.
    pub rebuild_in_flight: AtomicBool,

    /// Git state that the currently-published graph was indexed
    /// against (Task 7 Phase 7b2, A2 §I / §J.2).
    ///
    /// Read by the per-workspace watcher bridge as the `last_git_state`
    /// argument to
    /// [`sqry_core::watch::SourceTreeWatcher::wait_for_changes_cancellable`]
    /// so the classifier has a valid baseline on every debounce window.
    ///
    /// Advanced ONLY by [`crate::RebuildDispatcher::execute_one_rebuild`]
    /// after [`crate::WorkspaceManager::publish_and_retain`] succeeds,
    /// using the `git_state_at_enqueue` snapshot attached to the
    /// [`PendingRebuild`] that produced the publish. A failed rebuild
    /// MUST leave this field unchanged so the next
    /// `wait_for_changes_cancellable` call still sees the divergent
    /// state and retries.
    ///
    /// # Invariant
    ///
    /// - Only `execute_one_rebuild`'s successful-publish arm writes
    ///   this field.
    /// - Watcher-side event receipt, cancellation, or rebuild failure
    ///   never write this field.
    /// - The write happens under `parking_lot::RwLock` — short critical
    ///   section, no cross-lock ordering concerns.
    ///
    /// `None` on workspace construction (no rebuild has published
    /// yet). The first successful `execute_one_rebuild` driven by the
    /// watcher bridge (which attaches `git_state_at_enqueue = Some(...)`)
    /// populates this field.
    pub last_indexed_git_state: RwLock<Option<LastIndexedGitState>>,
}

impl LoadedWorkspace {
    /// Construct a fresh workspace entry with the placeholder generation
    /// (an empty graph and no roster record).
    ///
    /// The placeholder is `Arc`-cheap (sub-kilobyte) so keeping the
    /// `ArcSwap` non-null for the entire workspace lifetime avoids an
    /// `Option` layer on the query path. Eviction stores another
    /// placeholder through the same `ArcSwap`; re-load overwrites it.
    #[must_use]
    pub fn new(key: WorkspaceKey, pinned: bool) -> Self {
        Self {
            key,
            published: ArcSwap::from_pointee(PublishedGraph::placeholder()),
            state: std::sync::atomic::AtomicU8::new(WorkspaceState::Unloaded.as_u8()),
            last_accessed: RwLock::new(Instant::now()),
            memory_bytes: AtomicUsize::new(0),
            memory_high_water_bytes: AtomicUsize::new(0),
            pinned,
            last_error: RwLock::new(None),
            last_good_at: RwLock::new(None),
            retry_count: AtomicU32::new(0),
            rebuild_lane: tokio::sync::Mutex::new(None),
            rebuild_cancelled: AtomicBool::new(false),
            evicted_mid_iteration: AtomicBool::new(false),
            watcher_stop: parking_lot::Mutex::new(Arc::new(AtomicBool::new(false))),
            watch_wanted: AtomicBool::new(false),
            rebuild_in_flight: AtomicBool::new(false),
            last_indexed_git_state: RwLock::new(None),
        }
    }

    /// The published generation: one load of the slot, so the graph and
    /// the record are from the same publish (design D14).
    #[must_use]
    pub fn published(&self) -> Arc<PublishedGraph> {
        self.published.load_full()
    }

    /// The published graph, for readers that need only that half.
    #[must_use]
    pub fn graph(&self) -> Arc<CodeGraph> {
        Arc::clone(&self.published().graph)
    }

    /// The published roster record, for readers that need only that
    /// half. `None` for the placeholder generation.
    #[must_use]
    pub fn roster(&self) -> Option<Arc<RosterRecord>> {
        self.published().roster.clone()
    }

    /// Atomic state read. Round-trips through [`WorkspaceState::from_u8`];
    /// a discriminant outside the current range panics (it is a
    /// telemetry-corruption bug, not a recoverable condition).
    pub fn load_state(&self) -> WorkspaceState {
        let raw = self.state.load(Ordering::Acquire);
        WorkspaceState::from_u8(raw)
            .unwrap_or_else(|| unreachable!("invalid WorkspaceState discriminant {raw}"))
    }

    /// Atomic state write.
    ///
    /// This is the unconditional write, and it is correct only where the
    /// store is atomic with the observation that justifies it: under the
    /// `workspaces` guard a tombstone writer would need, or because the
    /// store is itself the tombstone writer. Surface parity W1 round 7
    /// (design D37) enumerates those sites; every other lifecycle store
    /// that reports the outcome of work a task owns uses
    /// [`Self::transition_state`] or [`Self::transition_state_from_any`],
    /// so a completed eviction cannot be overwritten by a claim about a
    /// generation that is gone.
    pub fn store_state(&self, new_state: WorkspaceState) {
        self.state.store(new_state.as_u8(), Ordering::Release);
    }

    /// Move the lifecycle state from `from` to `to`, and write nothing
    /// at all when the observed state is not `from`.
    ///
    /// One `compare_exchange` on the state atomic, `Ordering::AcqRel` on
    /// success and `Ordering::Acquire` on failure, matching the load
    /// gate's own compare-exchange. Surface parity W1 round 7, design
    /// D37: a store that reports the outcome of work this task owns is a
    /// claim about the generation in the slot, so it is only safe when it
    /// is atomic with the observation that installed `from`. The caller
    /// that loses the exchange has been overtaken by a writer that owns
    /// the state now (eviction, reset), and returns the same typed error
    /// it returns today.
    ///
    /// # Errors
    ///
    /// The observed state, when it is not `from`. Nothing is written in
    /// that case and the state is left exactly as its writer left it.
    pub fn transition_state(
        &self,
        from: WorkspaceState,
        to: WorkspaceState,
    ) -> Result<(), WorkspaceState> {
        self.state
            .compare_exchange(
                from.as_u8(),
                to.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|observed| {
                WorkspaceState::from_u8(observed).unwrap_or_else(|| {
                    unreachable!("invalid WorkspaceState discriminant {observed}")
                })
            })
    }

    /// Move the lifecycle state to `to` from any one of `from`, and write
    /// nothing at all when the observed state is outside that set.
    ///
    /// One `compare_exchange` per attempt, in the order `from` gives, with
    /// the orderings [`Self::transition_state`] uses. This is the shape
    /// `WorkspaceManager::enter_loading_state` already uses for the load
    /// gate; it exists here for the rebuild iteration's entry, which has
    /// no single predecessor state (design D37).
    ///
    /// # Errors
    ///
    /// The state observed by the last attempt, when it is outside `from`.
    /// Nothing is written in that case.
    pub fn transition_state_from_any(
        &self,
        from: &[WorkspaceState],
        to: WorkspaceState,
    ) -> Result<WorkspaceState, WorkspaceState> {
        let mut observed = self.load_state();
        for prior in from {
            match self.transition_state(*prior, to) {
                Ok(()) => return Ok(*prior),
                Err(found) => observed = found,
            }
        }
        Err(observed)
    }

    /// Update `memory_bytes` and keep `memory_high_water_bytes`
    /// monotonic. Matches Amendment 2 §D:
    ///
    /// > Every time `memory_bytes` is assigned (initial load, full
    /// > rebuild completion, incremental rebuild's `ArcSwap::store`),
    /// > immediately call
    /// > `memory_high_water_bytes.fetch_max(new, Relaxed)`.
    ///
    /// Returns the previous `memory_bytes` value so the caller can
    /// compute the delta the admission accounting needs.
    pub fn update_memory(&self, new_bytes: usize) -> usize {
        let prior = self.memory_bytes.swap(new_bytes, Ordering::AcqRel);
        self.memory_high_water_bytes
            .fetch_max(new_bytes, Ordering::Relaxed);
        prior
    }

    /// Resident handle kind represented by the existing live workspace state.
    #[must_use]
    pub fn resident_handle_kind(&self) -> ResidentHandleKind {
        ResidentHandleKind::LiveWorkspace
    }

    /// Current graph memory bytes as a wire-sized integer.
    #[must_use]
    pub fn current_memory_bytes(&self) -> u64 {
        self.memory_bytes.load(Ordering::Acquire) as u64
    }

    /// Stamp `last_accessed = now` on a query. Held under an `RwLock`
    /// so the query hot path never blocks — writers contend only with
    /// other writers.
    pub fn touch(&self) {
        *self.last_accessed.write() = Instant::now();
    }

    /// Record a successful build's wall-clock + reset retry counter.
    pub fn record_success(&self, at: SystemTime) {
        *self.last_good_at.write() = Some(at);
        *self.last_error.write() = None;
        self.retry_count.store(0, Ordering::Release);
    }

    /// Stop the file watcher attached to this workspace, if any: its
    /// blocking loop observes the signal on its next poll and exits, and
    /// its dispatcher task exits with it. Called by the tombstone writers
    /// (eviction, `unload`, `reset`) and by daemon shutdown.
    pub fn stop_watcher(&self) {
        self.watcher_stop.lock().store(true, Ordering::Release);
    }

    /// Stop the file watcher and move to `state` (`Evicted` or `Unloaded`)
    /// under the one lock [`Self::arm_watcher_stop`] takes, so a watcher
    /// armed concurrently either is stopped here or sees the tombstone
    /// state and is refused: no watcher is left running with a clear
    /// signal on a tombstone.
    pub fn stop_watcher_and_store_state(&self, state: WorkspaceState) {
        let slot = self.watcher_stop.lock();
        slot.store(true, Ordering::Release);
        self.store_state(state);
    }

    /// A fresh stop signal for a watcher about to attach to this
    /// workspace, or `None` when the workspace is a tombstone (`Evicted`,
    /// or `Unloaded` after a reset), which no watcher may serve. The
    /// previous watcher's signal is set either way, so a watcher still
    /// draining from an earlier attachment exits rather than run beside
    /// the new one. The state is read under the signal's lock, which the
    /// tombstone writers hold while they store the tombstone state
    /// ([`Self::stop_watcher_and_store_state`]), so the check and the
    /// arming are one step against them.
    #[must_use]
    pub fn arm_watcher_stop(&self) -> Option<Arc<AtomicBool>> {
        let mut slot = self.watcher_stop.lock();
        slot.store(true, Ordering::Release);
        if matches!(
            self.load_state(),
            WorkspaceState::Evicted | WorkspaceState::Unloaded
        ) {
            return None;
        }
        let fresh = Arc::new(AtomicBool::new(false));
        *slot = Arc::clone(&fresh);
        Some(fresh)
    }

    /// Whether `stop` is the signal this workspace currently holds for its
    /// watcher, so the watcher that owns it serves this workspace object.
    #[must_use]
    pub fn holds_watcher_stop(&self, stop: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(&self.watcher_stop.lock(), stop)
    }

    /// Record a failed build and return the new retry count. The
    /// dispatcher uses this to pick the exponential-backoff schedule.
    pub fn record_failure(&self, err: DaemonError) -> u32 {
        *self.last_error.write() = Some(err);
        self.retry_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Test-only setter for [`Self::last_good_at`].
    ///
    /// Task 7 Phase 7c: lets `classify_for_serve` integration tests
    /// drive the stale-serve age arithmetic against synthetic
    /// timestamps without needing to run a real rebuild.
    ///
    /// `#[doc(hidden)]` to signal "test affordance only" — follows
    /// the [`crate::TestGate`] / [`crate::TestCapture`] pattern.
    /// Production code should not call this.
    #[doc(hidden)]
    pub fn set_last_good_at_for_test(&self, at: Option<SystemTime>) {
        *self.last_good_at.write() = at;
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use sqry_core::project::ProjectRootMode;

    use super::*;

    fn make_key() -> WorkspaceKey {
        WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0x1,
        )
    }

    #[test]
    fn new_workspace_defaults() {
        let ws = LoadedWorkspace::new(make_key(), false);
        assert_eq!(ws.load_state(), WorkspaceState::Unloaded);
        assert_eq!(ws.memory_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(ws.memory_high_water_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(ws.retry_count.load(Ordering::Relaxed), 0);
        assert!(!ws.rebuild_cancelled.load(Ordering::Relaxed));
        assert!(
            !ws.rebuild_in_flight.load(Ordering::Relaxed),
            "new workspace must start with in_flight=false (no runner)"
        );
        assert!(!ws.pinned);
        assert!(ws.last_error.read().is_none());
        assert!(ws.last_good_at.read().is_none());
        assert!(
            ws.roster().is_none(),
            "no roster record before the first publish"
        );
        assert_eq!(
            ws.graph().node_count(),
            0,
            "the placeholder generation is the empty graph"
        );
    }

    #[test]
    fn state_atomicity_round_trips() {
        let ws = LoadedWorkspace::new(make_key(), false);
        ws.store_state(WorkspaceState::Loading);
        assert_eq!(ws.load_state(), WorkspaceState::Loading);
        ws.store_state(WorkspaceState::Rebuilding);
        assert_eq!(ws.load_state(), WorkspaceState::Rebuilding);
        ws.store_state(WorkspaceState::Failed);
        assert_eq!(ws.load_state(), WorkspaceState::Failed);
    }

    /// T55's primitive, surface parity W1 round 7 (design D37). A
    /// declared control: the two symbols do not exist on the pre-change
    /// head, so there is no red to show. The rows it exists for are C66
    /// (`transition_state` stores unconditionally, ignoring `from`) and
    /// C67 (`transition_state` returns `Ok(())` when the
    /// compare-exchange fails), and both are killed here.
    #[test]
    fn transition_state_refuses_a_state_it_did_not_install() {
        // Leg 1: the transition this task's own work justifies.
        let owned = LoadedWorkspace::new(make_key(), false);
        owned.store_state(WorkspaceState::Rebuilding);
        assert_eq!(
            owned.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Loaded),
            Ok(()),
            "a transition from the state this task installed must store"
        );
        assert_eq!(
            owned.load_state(),
            WorkspaceState::Loaded,
            "the state it stored must be readable"
        );

        // Leg 2: a completed eviction owns the state, so the transition
        // writes nothing and names what it found. The state read comes
        // first on purpose: it is the half C66 (an unconditional store)
        // fails, and the returned value below is the half C67 (an
        // `Ok(())` over a failed compare-exchange) fails, so the two rows
        // are told apart by which assertion this test stops on.
        let evicted = LoadedWorkspace::new(make_key(), false);
        evicted.store_state(WorkspaceState::Evicted);
        let refused = evicted.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Loaded);
        assert_eq!(
            evicted.load_state(),
            WorkspaceState::Evicted,
            "a refused transition must write nothing"
        );
        assert_eq!(
            refused,
            Err(WorkspaceState::Evicted),
            "a transition from a state this task did not install must refuse and name the observed state"
        );

        // Leg 3: the set form, which the rebuild iteration's entry uses
        // because it has no single predecessor state.
        let entering = LoadedWorkspace::new(make_key(), false);
        entering.store_state(WorkspaceState::Loaded);
        assert_eq!(
            entering.transition_state_from_any(
                &[
                    WorkspaceState::Loaded,
                    WorkspaceState::Failed,
                    WorkspaceState::Unloaded,
                    WorkspaceState::Rebuilding,
                ],
                WorkspaceState::Rebuilding,
            ),
            Ok(WorkspaceState::Loaded),
            "the set form must name the state it moved from"
        );
        assert_eq!(
            entering.load_state(),
            WorkspaceState::Rebuilding,
            "the set form must store when the observed state is inside the set"
        );

        // Leg 4: the same set form against a tombstone. `Evicted` is
        // outside the set by construction (design D37), so nothing is
        // written and the tombstone survives.
        let tombstone = LoadedWorkspace::new(make_key(), false);
        tombstone.store_state(WorkspaceState::Evicted);
        let refused_set = tombstone.transition_state_from_any(
            &[
                WorkspaceState::Loaded,
                WorkspaceState::Failed,
                WorkspaceState::Unloaded,
                WorkspaceState::Rebuilding,
            ],
            WorkspaceState::Rebuilding,
        );
        assert_eq!(
            tombstone.load_state(),
            WorkspaceState::Evicted,
            "a refused set transition must write nothing"
        );
        assert_eq!(
            refused_set,
            Err(WorkspaceState::Evicted),
            "the set form must refuse a state outside the set and name it"
        );

        // Leg 5: an already-installed `to` is inside the set, so a second
        // iteration entry on the same workspace is not a refusal.
        let already = LoadedWorkspace::new(make_key(), false);
        already.store_state(WorkspaceState::Rebuilding);
        assert_eq!(
            already.transition_state_from_any(
                &[
                    WorkspaceState::Loaded,
                    WorkspaceState::Failed,
                    WorkspaceState::Unloaded,
                    WorkspaceState::Rebuilding,
                ],
                WorkspaceState::Rebuilding,
            ),
            Ok(WorkspaceState::Rebuilding),
            "a workspace already Rebuilding must re-enter, naming Rebuilding as the prior"
        );
        assert_eq!(already.load_state(), WorkspaceState::Rebuilding);
    }

    #[test]
    fn update_memory_is_monotonic_high_water() {
        let ws = LoadedWorkspace::new(make_key(), false);
        assert_eq!(ws.update_memory(1_000), 0);
        assert_eq!(ws.memory_bytes.load(Ordering::Relaxed), 1_000);
        assert_eq!(ws.memory_high_water_bytes.load(Ordering::Relaxed), 1_000);

        // Grow — both must increase.
        assert_eq!(ws.update_memory(5_000), 1_000);
        assert_eq!(ws.memory_high_water_bytes.load(Ordering::Relaxed), 5_000);

        // Shrink — current drops, high-water stays.
        assert_eq!(ws.update_memory(2_000), 5_000);
        assert_eq!(ws.memory_bytes.load(Ordering::Relaxed), 2_000);
        assert_eq!(
            ws.memory_high_water_bytes.load(Ordering::Relaxed),
            5_000,
            "high-water mark must be monotonic across rebuilds with smaller graphs",
        );
    }

    #[test]
    fn record_failure_increments_retry_count() {
        let ws = LoadedWorkspace::new(make_key(), false);
        let err = || DaemonError::WorkspaceBuildFailed {
            root: PathBuf::from("/repos/example"),
            reason: "boom".into(),
        };
        assert_eq!(ws.record_failure(err()), 1);
        assert_eq!(ws.record_failure(err()), 2);
        assert_eq!(ws.record_failure(err()), 3);
        assert!(ws.last_error.read().is_some());
    }

    #[test]
    fn record_success_clears_error_and_resets_retry() {
        let ws = LoadedWorkspace::new(make_key(), false);
        let err = DaemonError::WorkspaceBuildFailed {
            root: PathBuf::from("/repos/example"),
            reason: "boom".into(),
        };
        assert_eq!(ws.record_failure(err), 1);
        ws.record_success(SystemTime::now());
        assert!(ws.last_error.read().is_none());
        assert!(ws.last_good_at.read().is_some());
        assert_eq!(ws.retry_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn touch_updates_last_accessed() {
        let ws = LoadedWorkspace::new(make_key(), false);
        let before = *ws.last_accessed.read();
        // Small sleep so the second Instant is strictly later.
        std::thread::sleep(Duration::from_millis(5));
        ws.touch();
        let after = *ws.last_accessed.read();
        assert!(after > before);
    }

    #[test]
    fn pinned_flag_is_immutable_via_constructor() {
        let pinned = LoadedWorkspace::new(make_key(), true);
        assert!(pinned.pinned);
        let unpinned = LoadedWorkspace::new(make_key(), false);
        assert!(!unpinned.pinned);
    }
}
