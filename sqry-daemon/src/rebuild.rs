//! Rebuild dispatcher for the sqryd daemon (Task 7 Phase 7a + 7b1).
//!
//! The [`RebuildDispatcher`] owns the per-workspace call path that
//! maps a debounced [`ChangeSet`] from the [`sqry_core::watch`] layer
//! through to either a full (`build_unified_graph`) or incremental
//! (`incremental_rebuild`) rebuild, and publishes the resulting
//! [`CodeGraph`] via
//! [`WorkspaceManager::publish_and_retain`](crate::workspace::WorkspaceManager::publish_and_retain).
//!
//! Phase 7a shipped the synchronous skeleton (coalesce-then-execute
//! single-caller driver). Phase 7b1 adds the A2 §J.2 runner-role gate
//! so concurrent callers serialise cleanly:
//!
//! - [`PendingRebuild::coalesce_with`] — A2 §J.2 lane-merge algebra
//!   (union of files, OR of `git_state_changed`, max of `enqueued_at`,
//!   full-rebuild-dominance merge on `git_change_class`).
//! - [`RebuildDispatcher::handle_changes`] — Phase A (acquire-or-park
//!   runner role via [`LoadedWorkspace::rebuild_in_flight`] CAS under
//!   the [`LoadedWorkspace::rebuild_lane`] mutex) + Phase B (drain
//!   loop: eviction gate → pipeline → re-lock-and-drain →
//!   loop-or-exit).
//! - [`RebuildDispatcher::execute_one_rebuild`] — one iteration of
//!   the pipeline: decide / estimate / reserve / execute
//!   (`spawn_blocking`) / publish / record success|failure.
//! - [`DrainLoopSentinel`] — panic-safety recovery for
//!   `rebuild_in_flight`; the sole out-of-lane transition exception.
//! - Hybrid decision: `git_change_class.requires_full_rebuild()` OR
//!   `changed_files.len() > incremental_threshold` OR
//!   `closure.len() > file_count * closure_limit_percent / 100` → Full;
//!   else Incremental.
//! - Working-set estimate via
//!   [`crate::workspace::working_set_estimate`] populated with
//!   [`crate::config::ESTIMATE_STAGING_PER_FILE_BYTES`] +
//!   [`crate::config::ESTIMATE_FINAL_PER_FILE_BYTES`] heuristic consts.
//!
//! Phase 7b2 (out of scope here) adds the per-workspace tokio watcher
//! event loop, the editor-pattern and git-scenario dispatcher-count
//! matrix, and the §J.2 serialization stress. Phase 7c (also out of
//! scope) wires the [`CancellationToken`] from
//! [`LoadedWorkspace::rebuild_cancelled`] through
//! [`incremental_rebuild`] at pass boundaries.
//!
//! # §J.2 runner-role invariant (Phase 7b1)
//!
//! At most one [`execute_one_rebuild`](RebuildDispatcher::execute_one_rebuild)
//! executes at a time per workspace, and at most one additional
//! [`PendingRebuild`] is parked in the lane awaiting the runner. A
//! caller arriving while the runner is active coalesces its incoming
//! `ChangeSet` into the lane (A2 §J.2 merge rules) and returns
//! `Ok(())` without running the pipeline — the active runner will
//! drain the lane at its next drain-loop iteration.
//!
//! All normal-path transitions of [`LoadedWorkspace::rebuild_in_flight`]
//! happen while [`LoadedWorkspace::rebuild_lane`] is held.
//! [`DrainLoopSentinel::drop`] is the sole recovery exception.
//!
//! # Eviction cooperation
//!
//! Every drain-loop iteration (including the first) checks
//! `ws.rebuild_cancelled` at the top of the loop. If set, the runner
//! abandons any parked pending, releases `rebuild_in_flight` under the
//! lane, and returns [`DaemonError::WorkspaceEvicted`]. The same
//! [`DaemonError::WorkspaceEvicted`] is surfaced by
//! [`WorkspaceManager::reserve_rebuild`]'s Phase-1 membership +
//! cancellation check — so a gate-check → `reserve_rebuild` race that
//! eviction wins cannot publish into an orphaned workspace.
//!
//! # Lock order (§J.4)
//!
//! [`RebuildDispatcher`] is the sole acquirer of
//! [`LoadedWorkspace::rebuild_lane`](crate::workspace::LoadedWorkspace::rebuild_lane).
//! The canonical call path honours the A2 §J.4 total order:
//!
//! ```text
//!   workspaces (manager.lookup)  →  rebuild_lane  →  admission (reserve_rebuild)
//! ```
//!
//! Rules enforced by this module:
//! - `manager.lookup` acquires `workspaces.read()` as a *precondition*
//!   and drops the guard before touching `rebuild_lane`.
//! - `rebuild_lane` is held **only** to coalesce/take `PendingRebuild`
//!   and to mutate `rebuild_in_flight`. The guard is dropped before
//!   [`WorkspaceManager::reserve_rebuild`] so §G.1's phase-1
//!   `workspaces.read()` does not nest under `rebuild_lane`.
//! - `admission` is strictly innermost and is held only inside
//!   [`WorkspaceManager::reserve_rebuild`] / `publish_and_retain` /
//!   retention-reaper paths — never reacquired by the dispatcher.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use sqry_core::graph::{
    CodeGraph, GraphBuilderError,
    unified::{
        build::{
            BuildConfig, BuildResult, CancellationToken, DurableGraphPersistenceRequest,
            build_unified_graph_with_progress_cancellable, compute_reverse_dep_closure,
            inferred_plugin_selection_manifest, persist_durable_graph_transaction,
        },
        memory::GraphMemorySize,
    },
};
use sqry_core::plugin::PluginManager;
use sqry_core::watch::{ChangeSet, GitChangeClass, LastIndexedGitState, SourceTreeWatcher};
use tokio::task::JoinHandle;

use crate::{
    config::{DaemonConfig, ESTIMATE_FINAL_PER_FILE_BYTES, ESTIMATE_STAGING_PER_FILE_BYTES},
    error::DaemonError,
    workspace::{
        LoadedWorkspace, PendingRebuild, WorkingSetInputs, WorkspaceKey, WorkspaceManager,
        WorkspaceState, clone_err, working_set_estimate,
    },
};

// ---------------------------------------------------------------------------
// RebuildMode
// ---------------------------------------------------------------------------

/// Outcome of the hybrid decision function: is this rebuild run via
/// [`build_unified_graph`] (Full) or [`incremental_rebuild`]
/// (Incremental)?
///
/// Encoded as `u8` for the [`RebuildDispatcher::last_mode`] atomic
/// observability surface. The encoding is stable across the
/// dispatcher's lifetime but is not part of any on-wire contract —
/// Task 8's IPC layer surfaces the mode only through structured
/// tracing, not a raw byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildMode {
    /// Run `build_unified_graph` from scratch.
    Full,
    /// Run `incremental_rebuild` over the reverse-dep closure.
    Incremental,
}

struct RebuildGraphOutput {
    graph: CodeGraph,
    effective_threads: usize,
}

struct DurableRebuildOutput {
    graph: CodeGraph,
    build_result: BuildResult,
}

impl RebuildMode {
    /// Encode for the [`AtomicU8`] slot: 0=None, 1=Full, 2=Incremental.
    const fn as_u8(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Incremental => 2,
        }
    }

    /// Decode from the atomic slot. Returns `None` for `0` (never set)
    /// or any unexpected discriminant (not observable through the
    /// `store_last_mode` path but round-tripped defensively).
    const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Full),
            2 => Some(Self::Incremental),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// PendingRebuild::coalesce_with (lane-merge algebra, A2 §J.2)
// ---------------------------------------------------------------------------

impl PendingRebuild {
    /// Coalesce two queued rebuilds per A2 §J.2.
    ///
    /// Merge rules:
    /// 1. **File union.** Deduplicated set of `changed_files` from
    ///    both sides, returned in lexicographic order so the merged
    ///    vector is deterministic across runs (important for
    ///    downstream decision-fork determinism and test assertions).
    /// 2. **OR of `git_state_changed`.** If either side observed a
    ///    `.git/` event, the merged entry records a change.
    /// 3. **Full-rebuild-dominance merge on `git_change_class`.**
    ///    If either side has a class with `requires_full_rebuild() ==
    ///    true` (currently `BranchSwitch` or `TreeDiverged`), the
    ///    merged class is canonically `Some(TreeDiverged)` — any
    ///    downstream dispatcher decision only checks
    ///    `requires_full_rebuild()`, so the specific discriminant
    ///    beyond the canonical "full trigger observed" marker is
    ///    unused. If neither side is a full trigger, the later-side
    ///    class wins (most-recent observation); `None` is absorbed
    ///    from either side when the other is `Some`.
    /// 4. **`enqueued_at = max(self, later)`.** Later wins so the
    ///    lane reflects the most-recent activity for staleness /
    ///    tracing purposes.
    /// 5. **`git_state_at_enqueue` — absorb-None, later wins
    ///    (Task 7 Phase 7b2).** When both sides carry a snapshot,
    ///    the newer observation wins (the merged coalesced pending
    ///    reflects the freshest valid baseline for the runner's
    ///    publish commit). When only one side carries a snapshot,
    ///    that one is preserved. Both `None` → `None`.
    ///
    /// `self` is treated as the earlier enqueue; `later` is the
    /// newly-arrived enqueue the dispatcher is merging in.
    ///
    /// # Determinism
    ///
    /// The merged `changed_files` vector is sorted. `coalesce_with`
    /// is commutative under the `requires_full_rebuild()` predicate
    /// (full-rebuild dominance is symmetric). It is **not**
    /// commutative under the raw `git_change_class` discriminant
    /// when neither side is a full trigger — `Some(LocalCommit) ⊕
    /// Some(Noise) = Some(Noise)` but `Some(Noise) ⊕ Some(LocalCommit)
    /// = Some(LocalCommit)`. Nor is `git_state_at_enqueue` commutative
    /// in general (later wins when both sides are `Some`); both
    /// asymmetries are by design.
    #[must_use]
    pub fn coalesce_with(self, later: PendingRebuild) -> PendingRebuild {
        // 1. File union, deterministic order.
        let mut file_set: std::collections::BTreeSet<PathBuf> =
            self.changes.changed_files.into_iter().collect();
        file_set.extend(later.changes.changed_files);
        let changed_files: Vec<PathBuf> = file_set.into_iter().collect();

        // 2. OR of git_state_changed.
        let git_state_changed = self.changes.git_state_changed || later.changes.git_state_changed;

        // 3. Full-rebuild-dominance merge on git_change_class.
        let git_change_class = merge_git_class(
            self.changes.git_change_class,
            later.changes.git_change_class,
        );

        // 4. enqueued_at = max.
        let enqueued_at = self.enqueued_at.max(later.enqueued_at);

        // 5. git_state_at_enqueue: absorb-None, later wins when both Some.
        let git_state_at_enqueue = later.git_state_at_enqueue.or(self.git_state_at_enqueue);

        PendingRebuild {
            changes: ChangeSet {
                changed_files,
                git_state_changed,
                git_change_class,
            },
            enqueued_at,
            git_state_at_enqueue,
        }
    }
}

/// Merge two `git_change_class` observations per §J.2
/// "full-rebuild-dominance" semantics.
///
/// See [`PendingRebuild::coalesce_with`] for the merge contract.
fn merge_git_class(a: Option<GitChangeClass>, b: Option<GitChangeClass>) -> Option<GitChangeClass> {
    let requires_full = a.is_some_and(GitChangeClass::requires_full_rebuild)
        || b.is_some_and(GitChangeClass::requires_full_rebuild);
    if requires_full {
        return Some(GitChangeClass::TreeDiverged);
    }
    // later wins for non-full; fallback to earlier if later is None.
    b.or(a)
}

// ---------------------------------------------------------------------------
// Decision fork
// ---------------------------------------------------------------------------

/// Hybrid rebuild-mode decision per plan line 1422 and Amendment 2
/// §J.
///
/// Full-rebuild triggers (in evaluation order):
///
/// 1. [`ChangeSet::requires_full_rebuild`] — a committed git state
///    change that mandates a full rebuild (currently `BranchSwitch`
///    or `TreeDiverged`; `LocalCommit` / `Noise` do not force full).
/// 2. `changed_files.len() > config.incremental_threshold` — too many
///    files for incremental economics to pay off.
/// 3. `closure.len() > graph.file_count() * closure_limit_percent /
///    100` — the reverse-dep closure would touch more files than the
///    full-rebuild cost; take the full path instead.
///
/// Otherwise, Incremental. Empty `ChangeSet` ([`ChangeSet::is_empty`])
/// returns Incremental as a legitimate no-op rebuild (Phase 3e
/// supports this explicitly — see `sqry-core/src/graph/unified/build/
/// incremental.rs:2842` for the empty-rebuild regression test).
///
/// Closure math resolves only paths already present in the graph's
/// file registry. Paths not yet registered (new files) contribute
/// zero to the closure but are still passed through to
/// [`incremental_rebuild`]; its internal `phase3e_discover_new_file_paths`
/// (Phase 3e, `incremental.rs:935`) handles them via a first-class
/// new-file discovery leg. This is intentional: forcing Full on every
/// new file would regress Phase 3e's shipped behavior.
#[must_use]
pub fn decide_mode(config: &DaemonConfig, changes: &ChangeSet, graph: &CodeGraph) -> RebuildMode {
    if changes.is_empty() {
        return RebuildMode::Incremental;
    }
    if changes.requires_full_rebuild() {
        return RebuildMode::Full;
    }
    if changes.changed_files.len() > config.incremental_threshold {
        return RebuildMode::Full;
    }

    // Resolve registered paths to FileId for closure math. Unresolved
    // paths (new files) simply don't contribute to the closure.
    let file_ids: Vec<_> = changes
        .changed_files
        .iter()
        .filter_map(|p| graph.files().get(p))
        .collect();

    let closure = compute_reverse_dep_closure(&file_ids, graph);
    let file_count = graph.files().len();
    // Integer math: > file_count * pct / 100 → Full. `closure_limit_percent`
    // is validated as 1..=100 in `DaemonConfig::validate`.
    let limit = file_count.saturating_mul(config.closure_limit_percent as usize) / 100;

    if closure.len() > limit {
        RebuildMode::Full
    } else {
        RebuildMode::Incremental
    }
}

// ---------------------------------------------------------------------------
// Working-set estimate
// ---------------------------------------------------------------------------

/// Compute the A2 §G.6 working-set estimate for the given rebuild
/// mode.
///
/// Formula (see
/// [`crate::workspace::working_set_estimate`] for the multipliers):
///
/// | Mode        | `new_graph_final_estimate`                                                 | `staging_overhead`                                               | `interner_snapshot_bytes`                |
/// |-------------|---------------------------------------------------------------------------|------------------------------------------------------------------|------------------------------------------|
/// | Full        | `prior.heap_bytes()`                                                      | `file_count * ESTIMATE_STAGING_PER_FILE_BYTES`                   | `prior.strings().heap_bytes()`           |
/// | Incremental | `prior.heap_bytes() + closure.len() * ESTIMATE_FINAL_PER_FILE_BYTES`      | `closure_file_count * ESTIMATE_STAGING_PER_FILE_BYTES`           | `prior.strings().heap_bytes()`           |
///
/// Where `closure_file_count` for Incremental uses
/// `changes.changed_files.len()` (an upper bound — we compute the
/// exact reverse-dep closure size only when we already decided to run
/// incremental, so the caller may pass the file count directly rather
/// than re-running closure math here).
fn compute_working_set_estimate(prior: &CodeGraph, changes: &ChangeSet, mode: RebuildMode) -> u64 {
    let prior_bytes = prior.heap_bytes() as u64;
    let interner_bytes = prior.strings().heap_bytes() as u64;
    let file_count = prior.files().len() as u64;

    let (final_estimate, staging_file_count) = match mode {
        RebuildMode::Full => (prior_bytes, file_count),
        RebuildMode::Incremental => {
            let n = changes.changed_files.len() as u64;
            let final_est =
                prior_bytes.saturating_add(n.saturating_mul(ESTIMATE_FINAL_PER_FILE_BYTES));
            (final_est, n)
        }
    };

    let staging = staging_file_count.saturating_mul(ESTIMATE_STAGING_PER_FILE_BYTES);

    working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: final_estimate,
        staging_overhead: staging,
        interner_snapshot_bytes: interner_bytes,
    })
}

// ---------------------------------------------------------------------------
// Test hooks — gate + capture (Task 7 Phase 7b2)
// ---------------------------------------------------------------------------
//
// Both hooks are gated behind `std::sync::OnceLock`. In production the
// `OnceLock::get()` fast path is a single relaxed atomic load per
// `execute_one_rebuild` iteration that returns `None` and short-circuits
// the hook entirely. Tests install the hooks once at harness setup;
// subsequent dispatcher iterations see the hook and behave
// deterministically.

/// **Test-only** gate for §J.2 serialization stress tests.
///
/// When installed, each `execute_one_rebuild` iteration awaits
/// [`Self::release`] while [`Self::hold`] is non-zero. The test fires
/// `release.notify_one()` once per iteration it wants to unblock, and
/// the gate atomically decrements `hold` on release. When `hold`
/// reaches zero, subsequent iterations pass through without waiting.
///
/// # Lost-wakeup safety
///
/// [`gate_check`](RebuildDispatcher::gate_check) obtains the
/// `notified()` future BEFORE re-checking `hold`, matching the 7b1
/// `Notify` handshake pattern (`tests/rebuild_runner_gate.rs` inline
/// comments). A test that first sets `hold = N` then fires N
/// `notify_one()` calls is guaranteed to release the first N
/// iterations without lost wakeups.
#[doc(hidden)]
#[derive(Debug)]
pub struct TestGate {
    /// Number of iterations remaining that must wait on `release`.
    /// Initialised by the test (e.g. `AtomicUsize::new(1)` to block
    /// only the first iteration); decremented on each gate release.
    pub hold: AtomicUsize,
    /// Notify fired by the test driver to release one waiting
    /// iteration.
    pub release: tokio::sync::Notify,
}

/// **Test-only** per-iteration capture for §J.2 file-union correctness
/// assertions.
///
/// When installed, each `execute_one_rebuild` iteration appends a
/// [`CapturedIteration`] to [`Self::iterations`] AFTER the mode
/// decision and BEFORE the gate check — so the test observes the
/// exact `ChangeSet` consumed by each iteration regardless of whether
/// the gate stalls that iteration.
///
/// Task 7 Phase 7c extension: three additional fields for the
/// eviction-during-rebuild abort test drive the
/// [`RebuildDispatcher::post_reservation_check`] hook. The hook fires
/// AFTER `reserve_rebuild` returns `Ok` and BEFORE the rebuild
/// pipeline starts, so tests can observe a live reservation and race
/// eviction against it.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct TestCapture {
    /// Records one entry per `execute_one_rebuild` invocation, in
    /// order. Never cleared by the dispatcher; the test inspects it
    /// after synchronising on the dispatchers completion.
    pub iterations: parking_lot::Mutex<Vec<CapturedIteration>>,

    /// Counter of iterations that must stall at the post-reservation
    /// hook. `0` = no hold; `> 0` = one iteration will stall per unit.
    /// Armed via [`Self::arm_post_reservation_hold`], released via
    /// [`Self::release_post_reservation`].
    pub post_reservation_hold: AtomicUsize,

    /// Fired when [`RebuildDispatcher::post_reservation_check`] is
    /// entered. The test awaits [`Self::wait_until_post_reservation`]
    /// to synchronise on "rebuild has reserved bytes and is about to
    /// run".
    pub post_reservation_reached: tokio::sync::Notify,

    /// Fired by the test driver to release one waiting iteration. Uses
    /// the same handshake pattern as [`TestGate`] (`notified()` future
    /// armed before re-checking `hold`).
    pub post_reservation_release: tokio::sync::Notify,

    /// Task 7 Phase 7c feat iter-1 (Codex MAJOR 2): counter for every
    /// `execute_one_rebuild` iteration where the §5e
    /// `workspaces.read()` recheck (or map-missing recheck) observed
    /// cancellation AFTER a successful pipeline run and BEFORE
    /// publish. Fires when eviction raced during a pipeline that
    /// completed before the forwarder's first poll.
    pub publish_path_evictions: AtomicUsize,
    /// Counter for every `execute_one_rebuild` iteration where the
    /// sqry-core pipeline itself returned
    /// `GraphBuilderError::Cancelled` from a pass boundary (the
    /// forwarder had time to flip the token before the pipeline
    /// completed).
    pub pass_boundary_cancellations: AtomicUsize,

    /// Test-only switch (iter-1): when `true`, the next
    /// `execute_rebuild` call does NOT spawn a
    /// [`spawn_cancellation_forwarder`]. Tests use this to
    /// deterministically force the §5e publish-path recheck
    /// (without a forwarder to flip the token, the pipeline
    /// completes Ok even though `ws.rebuild_cancelled = true`, and
    /// the §5e recheck picks up the eviction).
    ///
    /// Production leaves this `false`; the forwarder always runs.
    pub suppress_forwarder: AtomicBool,

    /// Test-only switch (iter-2 Codex MAJOR 1): when `true`,
    /// `execute_rebuild` synchronously calls `token.cancel()`
    /// immediately after spawning (or electing to suppress) the
    /// forwarder and BEFORE dispatching `spawn_blocking`. This
    /// guarantees the pipeline's very first `cancellation.check()?`
    /// observes the cancelled token, forcing the pass-boundary
    /// cancellation path deterministically.
    ///
    /// Production leaves this `false`.
    pub precancel_token_for_pass_boundary: AtomicBool,

    /// Durable flag (iter-2 Codex MAJOR 2): set by
    /// [`RebuildDispatcher::post_reservation_check`] when the hook
    /// fires. Paired with `post_reservation_reached` notify to
    /// provide lost-wakeup-safe synchronisation: tests that arm
    /// `post_reservation_hold` AFTER a rebuild has already reached
    /// the hook (rare but possible under fast scheduling) still see
    /// the flag and do not block waiting for a signal that already
    /// fired.
    ///
    /// Cleared by [`Self::reset_post_reservation_reached`] for
    /// multi-iteration tests.
    pub post_reservation_reached_flag: AtomicBool,
}

impl TestCapture {
    /// Construct a zero-initialised capture. Same as
    /// [`Default::default`]; named constructor for clarity in tests.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the §5e publish-path-recheck eviction counter (iter-1).
    #[must_use]
    pub fn publish_path_evictions(&self) -> usize {
        self.publish_path_evictions.load(Ordering::Acquire)
    }

    /// Read the pass-boundary cancellation counter (iter-1).
    #[must_use]
    pub fn pass_boundary_cancellations(&self) -> usize {
        self.pass_boundary_cancellations.load(Ordering::Acquire)
    }

    /// Arm a single post-reservation stall. The next
    /// `execute_one_rebuild` iteration that reaches
    /// [`RebuildDispatcher::post_reservation_check`] will block until
    /// [`Self::release_post_reservation`] is called. Stacks: calling
    /// this N times blocks N iterations.
    pub fn arm_post_reservation_hold(&self) {
        self.post_reservation_hold.fetch_add(1, Ordering::AcqRel);
    }

    /// Release exactly one stalled iteration. Matches the `TestGate`
    /// release semantics — the held iteration wakes, decrements
    /// `post_reservation_hold` one more step via the loop, and
    /// continues to `execute_rebuild`. Safe to call before an
    /// iteration arms (lost-wakeup-safe via the handshake in
    /// [`RebuildDispatcher::post_reservation_check`]).
    pub fn release_post_reservation(&self) {
        self.post_reservation_release.notify_one();
    }

    /// Await the next `execute_one_rebuild` iteration reaching the
    /// post-reservation hook. Returns as soon as the hook fires.
    ///
    /// Iter-2 Codex MAJOR 2: lost-wakeup-safe. If the hook has
    /// already fired (`post_reservation_reached_flag == true`),
    /// returns immediately without awaiting. Otherwise arms the
    /// `notified()` future BEFORE re-checking the flag (handshake
    /// pattern) so a signal that fires between arm and recheck is
    /// still observed.
    pub async fn wait_until_post_reservation(&self) {
        if self.post_reservation_reached_flag.load(Ordering::Acquire) {
            return;
        }
        let notified = self.post_reservation_reached.notified();
        if self.post_reservation_reached_flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    /// Reset the durable reached-flag so a second iteration of a
    /// multi-iteration test (e.g. the 100-iter stress) can await a
    /// fresh hook firing. Test-only.
    pub fn reset_post_reservation_reached(&self) {
        self.post_reservation_reached_flag
            .store(false, Ordering::Release);
    }
}

/// One captured `execute_one_rebuild` iteration (test-only).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct CapturedIteration {
    /// The ChangeSet as-consumed by this iteration (post-coalesce).
    pub changeset: ChangeSet,
    /// Mode decided for this iteration.
    pub mode: RebuildMode,
    /// Git-state snapshot attached to the consumed `PendingRebuild`.
    /// `None` for direct (non-bridge) callers.
    pub git_state_at_enqueue: Option<LastIndexedGitState>,
    /// Wall-clock when the iteration started.
    pub started_at: Instant,
}

// ---------------------------------------------------------------------------
// Watcher bridge registry (Task 7 Phase 7b2)
// ---------------------------------------------------------------------------

/// One per-workspace watcher + dispatcher task pair.
///
/// Both `JoinHandle`s are **observability-only**: they do not own the
/// task lifetimes. Dropping a `WatcherEntry` detaches both tasks
/// (Tokio's `JoinHandle::drop` is detach, not cancel — see
/// `tokio::task::JoinHandle` docs).
///
/// Shutdown is cooperative through two independent signals:
/// 1. `ws.rebuild_cancelled = true` (set by eviction) → the
///    cancellable watcher returns `Ok(None)` → blocking thread exits
///    → tokio mpsc sender drops → async task's `rx.recv()` returns
///    `None` → async task exits.
/// 2. `dispatcher.handle_changes_with_git_state` returns
///    `Err(WorkspaceEvicted)` → async task exits → drops receiver →
///    next `blocking_send` on the blocking thread fails → blocking
///    thread exits.
///
/// Before exiting, the async task flips [`Self::live`] to `false`
/// (so a concurrent `ensure_watching` call treats the entry as
/// draining) and then calls
/// [`RebuildDispatcher::reap_watcher`](crate::RebuildDispatcher::reap_watcher)
/// with its [`Self::generation`] to remove the entry from the map. The
/// generation token ensures a late old task cannot erase a newer
/// replacement entry after a fast evict+reload.
struct WatcherEntry {
    /// Monotonic generation token. Assigned at construction time from
    /// `RebuildDispatcher::next_watcher_generation`; used by
    /// `reap_watcher` to distinguish "my entry" from "a newer entry
    /// for the same WorkspaceKey".
    generation: u64,
    /// `true` while the async task is processing dispatches. Flipped
    /// to `false` as the first action of the post-loop cleanup
    /// sequence, BEFORE `reap_watcher` is called. Fast-path callers
    /// of `ensure_watching` read this as the authoritative liveness
    /// signal (rather than `JoinHandle::is_finished`, which returns
    /// `false` while the task is still inside its final cleanup
    /// closure).
    live: Arc<AtomicBool>,
    /// Handle to the async dispatcher task. Stored only to keep the
    /// task attached for the entry's lifetime — never awaited or
    /// aborted. Shutdown is cooperative (see struct-level docs).
    #[allow(dead_code)]
    async_handle: JoinHandle<()>,
    /// Handle to the blocking watcher thread. Stored only for
    /// attachment — dropping it detaches the task, which continues to
    /// completion via cooperative cancellation.
    #[allow(dead_code)]
    blocking_handle: JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// RebuildDispatcher
// ---------------------------------------------------------------------------

/// Sole acquirer of [`LoadedWorkspace::rebuild_lane`] (A2 §J).
///
/// Constructed once at daemon startup with a shared
/// [`Arc<PluginManager>`]. Every [`Self::handle_changes`] call honours
/// the canonical 7-step reservation call path (plan line 1495) and
/// the §J.4 lock-order contract documented on the module.
pub struct RebuildDispatcher {
    manager: Arc<WorkspaceManager>,
    config: Arc<DaemonConfig>,
    plugins: Arc<PluginManager>,
    build_config: BuildConfig,
    dispatched_count: AtomicU64,
    last_mode: AtomicU8,

    /// Per-workspace watcher+dispatcher task pairs (Task 7 Phase 7b2).
    /// Populated by [`Self::ensure_watching`]; pruned by
    /// [`Self::reap_watcher`] as each async task exits.
    watchers: parking_lot::Mutex<HashMap<WorkspaceKey, WatcherEntry>>,
    /// Monotonic counter used to tag each `WatcherEntry` with a unique
    /// generation, enabling `reap_watcher`'s compare-and-remove.
    next_watcher_generation: AtomicU64,

    /// Test-only synchronisation gate. `None` in production; tests
    /// install once at harness setup. See [`TestGate`] docstring.
    #[doc(hidden)]
    test_gate: OnceLock<Arc<TestGate>>,
    /// Test-only per-iteration capture recorder. `None` in production.
    /// See [`TestCapture`] docstring.
    #[doc(hidden)]
    test_capture: OnceLock<Arc<TestCapture>>,
}

impl std::fmt::Debug for RebuildDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PluginManager` does not implement `Debug` because its
        // internal registry is non-trivial; skip it here. `BuildConfig`
        // also does not implement `Debug` on HEAD. Report the shape of
        // the dispatcher instead of its full contents.
        f.debug_struct("RebuildDispatcher")
            .field(
                "dispatched_count",
                &self.dispatched_count.load(Ordering::Relaxed),
            )
            .field("last_mode", &self.last_mode())
            .field("memory_limit_mb", &self.config.memory_limit_mb)
            .finish_non_exhaustive()
    }
}

impl RebuildDispatcher {
    /// Construct a fresh dispatcher sharing the daemon's manager,
    /// config, and plugin manager.
    ///
    /// `build_config` defaults to [`BuildConfig::default`]; Task 14
    /// calibration may override this from [`DaemonConfig`] knobs.
    #[must_use]
    pub fn new(
        manager: Arc<WorkspaceManager>,
        config: Arc<DaemonConfig>,
        plugins: Arc<PluginManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            config,
            plugins,
            build_config: BuildConfig::default(),
            dispatched_count: AtomicU64::new(0),
            last_mode: AtomicU8::new(0),
            watchers: parking_lot::Mutex::new(HashMap::new()),
            next_watcher_generation: AtomicU64::new(0),
            test_gate: OnceLock::new(),
            test_capture: OnceLock::new(),
        })
    }

    // -----------------------------------------------------------------
    // Test-only hook installers (Task 7 Phase 7b2)
    // -----------------------------------------------------------------

    /// **Test-only.** Install a [`TestGate`] that stalls the FIRST
    /// `hold` iterations of `execute_one_rebuild` until the test
    /// driver fires `gate.release.notify_one()`. Returns `Err(gate)` on
    /// the original argument if a gate was already installed (one-shot
    /// per dispatcher lifetime).
    ///
    /// Zero production overhead — production callers never install a
    /// gate and `gate_check` short-circuits on the `OnceLock::get()
    /// == None` fast path.
    #[doc(hidden)]
    pub fn install_test_gate(&self, gate: Arc<TestGate>) -> Result<(), Arc<TestGate>> {
        self.test_gate.set(gate)
    }

    /// **Test-only.** Install a [`TestCapture`] recorder that pushes
    /// one [`CapturedIteration`] per `execute_one_rebuild` invocation.
    /// Returns `Err(capture)` on the original argument if a capture was
    /// already installed.
    #[doc(hidden)]
    pub fn install_test_capture(&self, capture: Arc<TestCapture>) -> Result<(), Arc<TestCapture>> {
        self.test_capture.set(capture)
    }

    /// Internal gate check, called by `execute_one_rebuild` after
    /// mode selection and before admission reservation (iter-2 §4
    /// placement rationale: the gate must NOT hold an admission
    /// reservation across a synthetic test stall).
    ///
    /// # Handshake pattern
    ///
    /// Obtain the `notified()` future BEFORE rechecking `hold`. If we
    /// load `hold > 0`, THEN enter `notified().await` only if `hold`
    /// is still `> 0` after the future is armed. This matches the 7b1
    /// pattern documented in `tests/rebuild_runner_gate.rs`.
    async fn gate_check(&self) {
        let Some(gate) = self.test_gate.get() else {
            return;
        };
        if gate.hold.load(Ordering::Acquire) == 0 {
            return;
        }
        let notified = gate.release.notified();
        tokio::pin!(notified);
        // Re-check AFTER arming the future: a concurrent
        // notify_one() between the first load and the await point
        // would otherwise be lost. Since `notified()` retroactively
        // matches any pending permit, re-checking `hold` after
        // arming is sufficient.
        if gate.hold.load(Ordering::Acquire) > 0 {
            notified.await;
            gate.hold.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Cumulative count of successful dispatches across this
    /// dispatcher's lifetime.
    ///
    /// Observability surface for the Task 7 §I dispatcher-count
    /// matrix (arrives in 7b). Not reset on workspace eviction; the
    /// counter is daemon-process-scoped.
    #[must_use]
    pub fn dispatched_count(&self) -> u64 {
        self.dispatched_count.load(Ordering::Relaxed)
    }

    /// Most-recent mode selected by [`Self::handle_changes`], or
    /// `None` if no dispatch has happened yet.
    ///
    /// Observability surface for tests and tracing spans.
    #[must_use]
    pub fn last_mode(&self) -> Option<RebuildMode> {
        RebuildMode::from_u8(self.last_mode.load(Ordering::Relaxed))
    }

    /// §J.2 runner-role handoff + drain-loop orchestrator (Phase 7b1).
    ///
    /// Callers (the Phase 7b2 watcher task, direct test drivers,
    /// future IPC `workspace/force_rebuild`) invoke this for each
    /// debounced [`ChangeSet`]. Exactly one concurrent invocation
    /// per workspace runs the full pipeline; others coalesce their
    /// [`ChangeSet`] into the lane and return `Ok(())` promptly.
    ///
    /// Preconditions (not part of the §J.4 ordered sequence):
    /// - The workspace must already be registered with the manager
    ///   (i.e. [`WorkspaceManager::get_or_load`] succeeded earlier).
    ///   A caller whose [`WorkspaceManager::lookup`] returns `None`
    ///   sees [`DaemonError::WorkspaceEvicted`].
    ///
    /// # Phase A (acquire-or-park) — single lane lock scope
    ///
    /// 1. Lock [`LoadedWorkspace::rebuild_lane`].
    /// 2. Coalesce incoming `changes` with any prior
    ///    [`PendingRebuild`] parked in the lane (A2 §J.2 merge rules).
    /// 3. CAS [`LoadedWorkspace::rebuild_in_flight`] `false → true`.
    ///    - On CAS success: we own the runner role; keep coalesced
    ///      pending as `current`, drop lane.
    ///    - On CAS failure: another runner is active; park coalesced
    ///      in lane, drop lane, return `Ok(())`.
    ///
    /// Holding the lane across the in-flight CAS makes the acquire
    /// race-free: every in-flight transition happens under the lane,
    /// so no two runners ever claim the role simultaneously.
    ///
    /// # Phase B (drain loop) — sentinel-protected
    ///
    /// Loop, armed with [`DrainLoopSentinel`] for panic-safety:
    ///
    /// 1. **Top-of-loop eviction gate.** Check
    ///    [`LoadedWorkspace::rebuild_cancelled`]. If set, take and
    ///    drop any parked pending (workspace is gone), release
    ///    `rebuild_in_flight` under the lane, disarm sentinel, return
    ///    [`DaemonError::WorkspaceEvicted`].
    /// 2. Call [`Self::execute_one_rebuild`] on `current`. Records
    ///    `record_success` / `record_failure` on the workspace; the
    ///    result flows into `last_result`.
    /// 3. Re-lock lane. If a new `PendingRebuild` is parked: take it
    ///    as the next `current`, loop to step 1 (in-flight stays
    ///    true). If the lane is empty: release `rebuild_in_flight`
    ///    under the lane, disarm sentinel, return `last_result`.
    ///
    /// # §J.4 lock order
    ///
    /// `manager.lookup` takes `workspaces.read()` and drops before
    /// `rebuild_lane`. `rebuild_lane` is dropped before
    /// [`WorkspaceManager::reserve_rebuild`] (which re-takes
    /// `workspaces.read() → admission.lock()` internally). No
    /// `rebuild_lane` ↔ `admission` nesting ever occurs.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspaceEvicted`] when the top-of-loop gate
    ///   observes `rebuild_cancelled == true`, or when
    ///   [`WorkspaceManager::reserve_rebuild`]'s Phase-1 check finds
    ///   the workspace missing from the manager map or cancelled.
    /// - [`DaemonError::MemoryBudgetExceeded`] when admission cannot
    ///   satisfy a reservation after eviction.
    /// - [`DaemonError::WorkspaceBuildFailed`] when either full or
    ///   incremental rebuild fails (including `spawn_blocking` join
    ///   failures).
    ///
    /// Only the FINAL drain-loop iteration's result is surfaced to
    /// the caller. Per-iteration success/failure is recorded on the
    /// workspace (`ws.last_error`, `ws.last_good_at`,
    /// `ws.retry_count`) via [`Self::execute_one_rebuild`].
    pub async fn handle_changes(
        &self,
        key: &WorkspaceKey,
        changes: ChangeSet,
    ) -> Result<(), DaemonError> {
        // Plain handle_changes — no git-state snapshot attached. The
        // runner will consume this PendingRebuild but will NOT advance
        // `ws.last_indexed_git_state` because `git_state_at_enqueue`
        // is `None`. Used by direct callers (tests, future IPC
        // `workspace/force_rebuild`) that don't own a watcher.
        self.handle_changes_inner(
            key,
            PendingRebuild {
                changes,
                enqueued_at: Instant::now(),
                git_state_at_enqueue: None,
            },
        )
        .await
    }

    /// Like [`Self::handle_changes`] but attaches a
    /// [`LastIndexedGitState`] snapshot to the enqueued
    /// [`PendingRebuild`]. When the runner (either us directly or a
    /// concurrent runner we parked against) successfully publishes
    /// the graph derived from this `PendingRebuild`, it commits the
    /// snapshot into [`LoadedWorkspace::last_indexed_git_state`]
    /// as the new classifier baseline.
    ///
    /// Task 7 Phase 7b2 — used exclusively by the per-workspace
    /// watcher bridge spawned by [`Self::ensure_watching`]. Plain
    /// [`Self::handle_changes`] remains the API for callers without a
    /// watcher-owned git-state snapshot.
    ///
    /// # §J.4 lock order, error taxonomy, gate, sentinel
    ///
    /// Identical to [`Self::handle_changes`] — Phase A
    /// (acquire-or-park under lane + CAS) and Phase B (drain loop)
    /// behave identically. The only difference is the
    /// `git_state_at_enqueue: Some(git_state)` construction.
    ///
    /// # Errors
    ///
    /// Same as [`Self::handle_changes`] —
    /// [`DaemonError::WorkspaceEvicted`],
    /// [`DaemonError::MemoryBudgetExceeded`],
    /// [`DaemonError::WorkspaceBuildFailed`].
    pub async fn handle_changes_with_git_state(
        &self,
        key: &WorkspaceKey,
        changes: ChangeSet,
        git_state: LastIndexedGitState,
    ) -> Result<(), DaemonError> {
        self.handle_changes_inner(
            key,
            PendingRebuild {
                changes,
                enqueued_at: Instant::now(),
                git_state_at_enqueue: Some(git_state),
            },
        )
        .await
    }

    /// Shared Phase A (acquire-or-park) + Phase B (drain loop) body
    /// for [`Self::handle_changes`] and
    /// [`Self::handle_changes_with_git_state`]. The public methods
    /// construct the incoming [`PendingRebuild`] and thread it here.
    async fn handle_changes_inner(
        &self,
        key: &WorkspaceKey,
        incoming: PendingRebuild,
    ) -> Result<(), DaemonError> {
        // --- Precondition: lookup Arc<LoadedWorkspace>. ---
        let ws: Arc<LoadedWorkspace> =
            self.manager
                .lookup(key)
                .ok_or_else(|| DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                })?;

        // ================================================================
        // Phase A — acquire-runner-or-park (single lane lock scope).
        //
        // Holding `rebuild_lane` across the `rebuild_in_flight` CAS is
        // the load-bearing invariant: every in-flight transition
        // happens under the lane, so no two runners ever claim the
        // role simultaneously.
        // ================================================================
        let mut current: PendingRebuild = {
            let mut lane_guard = ws.rebuild_lane.lock().await;

            // Coalesce incoming with any prior parked pending.
            let coalesced = match lane_guard.take() {
                Some(prior) => prior.coalesce_with(incoming),
                None => incoming,
            };

            // Try to acquire the runner role.
            match ws.rebuild_in_flight.compare_exchange(
                false,
                true,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => coalesced, // We own the runner role.
                Err(_) => {
                    // Another runner is active — park coalesced in
                    // the lane; that runner will drain it at its next
                    // drain-loop iteration. Return Ok(()) without
                    // executing the pipeline.
                    *lane_guard = Some(coalesced);
                    return Ok(());
                }
            }
            // lane_guard dropped at end of scope.
        };

        // ================================================================
        // Phase B — drain loop.
        //
        // Sentinel guarantees `rebuild_in_flight` is released if the
        // loop unwinds abnormally (documented narrow race on the
        // unwind path — plugin panics inside `spawn_blocking` are
        // caught by `execute_rebuild` and mapped to `Err`, so the
        // only realistic unwind trigger is a runtime-level failure).
        // ================================================================
        let mut sentinel = DrainLoopSentinel {
            ws: Arc::clone(&ws),
            armed: true,
        };

        // `last_result` is assigned by `execute_one_rebuild` on every
        // iteration before any exit path that reads it. The eviction
        // gate exits via its own `return Err(...)` without reading
        // `last_result`, and the drain-exit `return last_result`
        // branch is only reachable after an iteration has assigned.
        let mut last_result: Result<(), DaemonError>;
        loop {
            // --- Top-of-loop cancellation/eviction gate ---
            //
            // `rebuild_cancelled` is set either by eviction (under
            // `workspaces.write()` in `execute_eviction` BEFORE
            // `workspaces.remove(key)`) or by `daemon/cancel_rebuild`
            // (user-initiated cancel of an in-flight rebuild).
            //
            // `swap(false)` atomically reads AND clears the flag so a
            // user-cancel does not poison subsequent rebuilds. For
            // eviction this is harmless — the workspace is removed
            // from the map and this `LoadedWorkspace` instance will
            // not be reused.
            if ws.rebuild_cancelled.swap(false, Ordering::AcqRel) {
                let mut lane_guard = ws.rebuild_lane.lock().await;
                // Abandon any parked pending — the workspace is gone.
                let _dropped: Option<PendingRebuild> = lane_guard.take();
                ws.rebuild_in_flight.store(false, Ordering::Release);
                sentinel.armed = false;
                // Cluster-G iter-2 BLOCKER 3: distinguish an eviction
                // (the eviction path already set state to `Evicted`
                // under `workspaces.write()` BEFORE flipping the
                // flag) from a `daemon/reset` cancellation (which
                // leaves the state as `Rebuilding`). For reset, the
                // runner is responsible for the post-cancel state
                // transition — `record_and_transition_on_err` no-ops
                // on `WorkspaceEvicted`, so without this branch the
                // workspace would stay stuck in `Rebuilding` forever
                // (codex iter-1 review).
                if ws.load_state() == WorkspaceState::Rebuilding {
                    ws.store_state(WorkspaceState::Unloaded);
                }
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            }

            // --- Execute iteration ---
            //
            // `execute_one_rebuild` records success/failure on the
            // workspace before returning, so workspace-level
            // observability is threaded through every iteration even
            // when the drain loop continues past an error.
            last_result = self
                .execute_one_rebuild(key, &ws, current.changes, current.git_state_at_enqueue)
                .await;

            // --- Drain-or-exit decision (under lane lock) ---
            //
            // Releasing `rebuild_in_flight` under the lane is
            // load-bearing: a caller about to park in the lane
            // observes the release atomically with the empty-lane
            // snapshot, so parked pending cannot be stranded.
            let next: Option<PendingRebuild> = {
                let mut lane_guard = ws.rebuild_lane.lock().await;
                match lane_guard.take() {
                    Some(next) => Some(next),
                    None => {
                        ws.rebuild_in_flight.store(false, Ordering::Release);
                        sentinel.armed = false;
                        None
                    }
                }
                // lane_guard dropped at end of scope.
            };

            match next {
                Some(n) => current = n,
                None => return last_result,
            }
        }
    }

    /// Run a single pipeline iteration: decide mode, compute
    /// working-set estimate, reserve admission headroom, execute the
    /// rebuild on a blocking thread, publish atomically. Records
    /// `record_success` on successful publish or `record_failure` on
    /// any error path.
    ///
    /// Called by [`Self::handle_changes`]'s Phase B drain loop for
    /// each coalesced [`PendingRebuild`]. The drain loop may invoke
    /// this multiple times if new pending arrives between iterations;
    /// each invocation is independent from the caller's perspective.
    ///
    /// # Error paths
    ///
    /// All three non-panic error paths call `ws.record_failure` with
    /// a cloned [`DaemonError`] before returning:
    /// - [`WorkspaceManager::reserve_rebuild`] Err (e.g.
    ///   [`DaemonError::MemoryBudgetExceeded`],
    ///   [`DaemonError::WorkspaceEvicted`] from the Phase-1 check).
    /// - [`Self::execute_rebuild`] Err (pipeline failure — plugin
    ///   error, `spawn_blocking` join panic mapped to
    ///   [`DaemonError::WorkspaceBuildFailed`]).
    ///
    /// A panic inside [`WorkspaceManager::publish_and_retain`] — which
    /// is documented as infallible — unwinds past these match arms.
    /// Admission state is restored by `RollbackGuard` + reservation
    /// RAII drop, but `record_failure` is NOT called. That is
    /// acceptable as defense-in-depth only: in practice a
    /// `publish_and_retain` panic means the daemon is in
    /// damage-control territory where missing workspace-level error
    /// bookkeeping is a minor concern.
    ///
    /// On successful publish, calls `ws.record_success` which:
    /// - Stamps `last_good_at = SystemTime::now()`.
    /// - Clears `last_error`.
    /// - Resets `retry_count` to 0.
    /// - Also increments `self.dispatched_count` for the §I
    ///   observability matrix.
    ///
    /// Additionally (Task 7 Phase 7b2): when `git_state_at_enqueue`
    /// is `Some`, writes the snapshot into
    /// [`LoadedWorkspace::last_indexed_git_state`] AFTER the
    /// `publish_and_retain` call so the classifier baseline advances
    /// only with actual publish consumption. `None` entries (direct
    /// non-watcher callers) leave the baseline untouched.
    async fn execute_one_rebuild(
        &self,
        key: &WorkspaceKey,
        ws: &Arc<LoadedWorkspace>,
        changes: ChangeSet,
        git_state_at_enqueue: Option<LastIndexedGitState>,
    ) -> Result<(), DaemonError> {
        let prior_graph: Arc<CodeGraph> = ws.graph.load_full();
        let mode = decide_mode(&self.config, &changes, &prior_graph);
        self.store_last_mode(mode);

        // Task 7 Phase 7c: transition to Rebuilding at iteration entry.
        // Queries keep serving the prior ArcSwap snapshot (A2 §G.5).
        // Placement BEFORE gate_check is intentional: the `Rebuilding`
        // lifecycle covers the synthetic test stall too, so
        // `classify_for_serve_returns_fresh_for_rebuilding_workspace`
        // observes a real state. The store is atomic; a concurrent
        // `execute_eviction` that wins the race overwrites this with
        // `Evicted` under `workspaces.write()` — `classify_for_serve`'s
        // map-missing arm wins, so this is harmless.
        ws.store_state(WorkspaceState::Rebuilding);

        // Task 7 Phase 7b2: record the iteration input for tests
        // BEFORE the optional gate stall, so the test sees the input
        // even when the gate blocks. No-op when `test_capture` is not
        // installed (production).
        if let Some(cap) = self.test_capture.get() {
            cap.iterations.lock().push(CapturedIteration {
                changeset: changes.clone(),
                mode,
                git_state_at_enqueue: git_state_at_enqueue.clone(),
                started_at: Instant::now(),
            });
        }

        // Task 7 Phase 7b2: optional test-only gate. Stalls the
        // iteration until the test driver releases it. Production
        // callers never install a gate — this is a single atomic
        // load + short-circuit.
        self.gate_check().await;

        let estimate = compute_working_set_estimate(&prior_graph, &changes, mode);

        // Admission reservation. `reserve_rebuild` itself performs the
        // Phase-1 membership + cancellation check (Task 7 Phase 7b1)
        // which can surface `WorkspaceEvicted`. Either way, record on
        // the workspace so `last_error` / `retry_count` reflect the
        // failure — EXCEPT on the eviction path (Task 7 Phase 7c: the
        // workspace is gone; polluting telemetry with a "failure"
        // record is misleading).
        let reservation = match self.manager.reserve_rebuild(key, estimate) {
            Ok(r) => r,
            Err(e) => {
                self.record_and_transition_on_err(ws, &e);
                return Err(e);
            }
        };

        // Task 7 Phase 7c: post-reservation hook fires HERE with the
        // reservation alive. Tests use this to snapshot admission
        // state mid-rebuild (e.g., assert `reserved_bytes > 0`) and
        // race eviction. Production is a single atomic load + return.
        self.post_reservation_check().await;

        // Pipeline execution (`spawn_blocking` catches plugin panics
        // internally and maps them to `WorkspaceBuildFailed`).
        //
        // Task 7 Phase 7c: `execute_rebuild` now wires a
        // `CancellationToken` to `ws.rebuild_cancelled` via
        // `spawn_cancellation_forwarder`. A mid-pipeline eviction sets
        // `rebuild_cancelled = true`, the forwarder flips the token,
        // the pipeline returns `GraphBuilderError::Cancelled` →
        // mapped to `DaemonError::WorkspaceEvicted`.
        let durable_rebuild: DurableRebuildOutput = match self
            .execute_rebuild(key, ws, &prior_graph, mode, changes)
            .await
        {
            Ok(g) => g,
            Err(e) => {
                // Reservation refunds via RAII on drop at return.
                drop(reservation);
                // Task 7 Phase 7c feat iter-1 (Codex MAJOR 2):
                // increment the pass-boundary cancellation counter
                // when a `WorkspaceEvicted` surfaces from the
                // pipeline. Lets tests distinguish the two
                // cancellation surfaces (§5e publish-recheck vs
                // sqry-core pass-boundary).
                if matches!(e, DaemonError::WorkspaceEvicted { .. })
                    && let Some(cap) = self.test_capture.get()
                {
                    cap.pass_boundary_cancellations
                        .fetch_add(1, Ordering::AcqRel);
                }
                self.record_and_transition_on_err(ws, &e);
                return Err(e);
            }
        };
        let DurableRebuildOutput {
            graph: new_graph,
            build_result,
        } = durable_rebuild;
        tracing::debug!(
            workspace = %key.source_root.display(),
            nodes = build_result.node_count,
            edges = build_result.edge_count,
            mode = ?mode,
            "daemon rebuild durable persistence committed before publish"
        );

        // Task 7 Phase 7c §5e: hold `workspaces.read()` across the
        // final cancellation/membership re-check AND
        // `publish_and_retain`. `execute_eviction` holds
        // `workspaces.write()` for its entire critical section, so
        // the RwLock makes this publish atomic with respect to
        // eviction: either eviction has fully completed (our
        // re-checks observe cancellation or map-missing) or eviction
        // cannot start until we drop the read guard. Pattern mirrors
        // `WorkspaceManager::get_or_load` (manager.rs:687-761, Codex
        // Task 6 Phase 6b iter-2 MAJOR). Lock order §J.4:
        // `workspaces -> admission`; `publish_and_retain` takes
        // `admission` internally, which nests correctly.
        let publish_result = {
            let workspaces_guard = self.manager.workspaces_read();

            if ws.rebuild_cancelled.load(Ordering::Acquire) {
                drop(workspaces_guard);
                drop(reservation);
                // Task 7 Phase 7c feat iter-1 (Codex MAJOR 2):
                // counter — §5e recheck surface.
                if let Some(cap) = self.test_capture.get() {
                    cap.publish_path_evictions.fetch_add(1, Ordering::AcqRel);
                }
                // NO record_failure / state transition: eviction owns
                // those.
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            }
            if !workspaces_guard.contains_key(key) {
                drop(workspaces_guard);
                drop(reservation);
                if let Some(cap) = self.test_capture.get() {
                    cap.publish_path_evictions.fetch_add(1, Ordering::AcqRel);
                }
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            }

            // `G_daemon_control_plane.md` §3.5 caller-migration —
            // execute_one_rebuild (production caller 2). On
            // post-build oversize, propagate the typed error
            // upstream; the reservation's RAII Drop refunds bytes.
            //
            // Cluster-G iter-2 BLOCKER 2: also transition the
            // workspace to `Failed` so it doesn't stay stuck in
            // `Rebuilding`. The success branch below handles the
            // happy-path `Loaded` transition; the error branch
            // previously only refunded bytes and returned, leaving
            // the workspace observable as `Rebuilding` forever
            // (codex iter-1 review — only `daemon reset` could
            // recover, and the reset path itself was also broken).
            let (_token, published_arc) =
                match self.manager.publish_and_retain(reservation, ws, new_graph) {
                    Ok((token, arc)) => (token, arc),
                    Err(e) => {
                        self.record_and_transition_on_err(ws, &e);
                        return Err(e);
                    }
                };
            published_arc
            // workspaces_guard drops at end of this block; eviction
            // can proceed immediately afterward.
        };

        // Task 7 Phase 7b2: advance the classifier baseline when the
        // consumed PendingRebuild carried a watcher-captured snapshot.
        // This happens AFTER `publish_and_retain` + read-guard drop,
        // which is safe: the write is a per-workspace atomic on an
        // Arc the caller holds. A concurrent eviction stamps its own
        // state transition but does not invalidate this field — the
        // next `classify_for_serve` observes `WorkspaceEvicted` from
        // the map-missing arm.
        if let Some(git_state) = git_state_at_enqueue {
            *ws.last_indexed_git_state.write() = Some(git_state);
        }

        // Success bookkeeping.
        ws.record_success(SystemTime::now());
        // Task 7 Phase 7c: transition Rebuilding -> Loaded. A
        // concurrent eviction that wins the race after our read guard
        // drop overwrites this with Evicted — harmless, see header
        // comment above the read-guard block.
        ws.store_state(WorkspaceState::Loaded);
        self.dispatched_count.fetch_add(1, Ordering::Relaxed);
        let _ = publish_result;

        Ok(())
    }

    /// Task 7 Phase 7c helper: update workspace state + bookkeeping on
    /// a rebuild-failure error path.
    ///
    /// - `WorkspaceEvicted`:
    ///   - **State already `Evicted`** → NO-OP. Eviction wrote
    ///     `Evicted` under `workspaces.write()` BEFORE flipping
    ///     `rebuild_cancelled`; clobbering it with `Failed` would
    ///     destroy that contract.
    ///   - **State still `Rebuilding`** → cluster-G iter-3 fix.
    ///     This is a `daemon reset` cancellation that fired AFTER
    ///     the runner's top-of-loop gate but BEFORE the publish
    ///     recheck (`rebuild.rs:1229`). The runner converts that
    ///     into `WorkspaceEvicted`, but no eviction actually
    ///     happened — `WorkspaceManager::reset` only set
    ///     `rebuild_cancelled = true` and returned
    ///     `ResetCancellationDispatched`. Without the transition
    ///     below, the workspace stays stuck in `Rebuilding` and a
    ///     subsequent `daemon reset` returns
    ///     `ResetCancellationDispatched` again — operator must
    ///     `daemon stop && daemon start` to recover.
    ///
    ///     We transition to `Unloaded` (the same destination
    ///     `WorkspaceManager::reset` would have written if reset
    ///     had won the race against the iteration). The map entry
    ///     and `pinned` bit are preserved by the in-place
    ///     `store_state`; the next `daemon reset` (or the caller's
    ///     retry after the documented `retry_after_ms = 250`) sees
    ///     `Unloaded` and is a no-op, and `daemon load` then
    ///     recovers the workspace cheaply.
    ///
    /// - Any other `DaemonError` → `record_failure` + transition to
    ///   `WorkspaceState::Failed`. This is the entry point to A2
    ///   §G.7's stale-serve flow for that workspace.
    fn record_and_transition_on_err(&self, ws: &LoadedWorkspace, err: &DaemonError) {
        if matches!(err, DaemonError::WorkspaceEvicted { .. }) {
            // Cluster-G iter-3 BLOCKER 3 fix: differentiate
            // eviction-path from reset-path cancellations by reading
            // the state the eviction path would have written. See
            // doc-comment above for the full rationale.
            if ws.load_state() == WorkspaceState::Rebuilding {
                ws.store_state(WorkspaceState::Unloaded);
            }
            return;
        }
        ws.record_failure(clone_err(err));
        ws.store_state(WorkspaceState::Failed);
    }

    /// Drive the actual sqry-core rebuild pipeline on a blocking
    /// thread, with cooperative cancellation via a forwarder task
    /// that mirrors `ws.rebuild_cancelled` into a
    /// [`CancellationToken`].
    ///
    /// Sync-in-async bridge: `build_unified_graph_cancellable` and
    /// `incremental_rebuild` are CPU-bound + use rayon internally, so
    /// they must not block a tokio runtime worker thread. The blocking
    /// closure owns the cloned `Arc<PluginManager>` /
    /// [`BuildConfig`] / `Arc<CodeGraph>` / cancellation token so it
    /// outlives the awaited JoinHandle.
    ///
    /// Task 7 Phase 7c: a tokio task (`spawn_cancellation_forwarder`)
    /// polls `ws.rebuild_cancelled` at `CANCEL_FORWARDER_POLL_MS`
    /// cadence; the first `true` observation calls `token.cancel()`.
    /// The forwarder is `abort()`ed after the rebuild future returns
    /// regardless of outcome — polling stops immediately.
    ///
    /// # Error mapping
    ///
    /// - `GraphBuilderError::Cancelled` → `DaemonError::WorkspaceEvicted`
    ///   (cancellation only fires on eviction per the forwarder
    ///   contract).
    /// - Any other `GraphBuilderError` → `DaemonError::WorkspaceBuildFailed`
    ///   with a human-readable reason.
    /// - `spawn_blocking` join errors (panic inside the closure) →
    ///   `WorkspaceBuildFailed`.
    async fn execute_rebuild(
        &self,
        key: &WorkspaceKey,
        ws: &Arc<LoadedWorkspace>,
        prior: &Arc<CodeGraph>,
        mode: RebuildMode,
        changes: ChangeSet,
    ) -> Result<DurableRebuildOutput, DaemonError> {
        let root = key.source_root.clone();
        let plugins = Arc::clone(&self.plugins);
        let cfg = self.build_config.clone();
        let prior_for_blocking = Arc::clone(prior);
        let root_for_err = root.clone();

        // Task 7 Phase 7c: fresh cancellation token per iteration so
        // a cancelled token from a prior run cannot permanently break
        // subsequent rebuilds on the same workspace.
        let token = CancellationToken::new();
        // Task 7 Phase 7c feat iter-1: optional forwarder suppression
        // for §5e publish-path recheck tests. Production builds and
        // tests without a `TestCapture` always spawn the forwarder;
        // only a test with `suppress_forwarder=true` skips it.
        let forwarder_handle = if self
            .test_capture
            .get()
            .is_some_and(|cap| cap.suppress_forwarder.load(Ordering::Acquire))
        {
            None
        } else {
            Some(spawn_cancellation_forwarder(Arc::clone(ws), token.clone()))
        };
        // Task 7 Phase 7c feat iter-2 (Codex MAJOR 1): optional
        // synchronous pre-cancel for pass-boundary-determinism
        // tests. When armed, the token is already cancelled by the
        // time spawn_blocking dispatches the pipeline, so the very
        // first `cancellation.check()?` inside
        // `build_unified_graph_cancellable` /
        // `incremental_rebuild` fires. Forces the pass-boundary
        // cancellation surface without racing the forwarder.
        if self.test_capture.get().is_some_and(|cap| {
            cap.precancel_token_for_pass_boundary
                .load(Ordering::Acquire)
        }) {
            token.cancel();
        }

        let token_for_blocking = token.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let built = execute_rebuild_blocking(
                &root,
                &prior_for_blocking,
                mode,
                changes,
                &plugins,
                &cfg,
                &token_for_blocking,
            )?;
            persist_rebuild_output_blocking(root, built, &plugins, &cfg, mode)
        })
        .await;

        // Task 7 Phase 7c: stop the forwarder unconditionally once the
        // rebuild future completes. (Iter-1 Option<JoinHandle>: if
        // forwarder suppression is armed in TestCapture, handle is
        // None — nothing to abort.)
        if let Some(handle) = forwarder_handle {
            handle.abort();
        }

        match join_result {
            Ok(Ok(graph)) => Ok(graph),
            Ok(Err(e)) => Err(e),
            Err(join_err) => Err(DaemonError::WorkspaceBuildFailed {
                root: root_for_err,
                reason: format!("spawn_blocking join error: {join_err}"),
            }),
        }
    }

    /// Task 7 Phase 7c: test-only observation + stall point inside
    /// `execute_one_rebuild`, fired AFTER `reserve_rebuild` returns
    /// Ok and BEFORE `execute_rebuild` runs the blocking pipeline.
    ///
    /// Production builds with no `TestCapture` installed see a single
    /// atomic load + return. With a capture installed, each
    /// invocation:
    ///
    /// 1. Fires `post_reservation_reached.notify_waiters()` so tests
    ///    awaiting `wait_until_post_reservation()` return.
    /// 2. If `post_reservation_hold.load > 0`, stalls on
    ///    `post_reservation_release.notified()` — matches the 7b1
    ///    `Notify` handshake pattern (arm `notified()` future BEFORE
    ///    re-checking `hold`) to close the lost-wakeup window.
    async fn post_reservation_check(&self) {
        let Some(cap) = self.test_capture.get() else {
            return;
        };
        // Iter-2 Codex MAJOR 2: set the durable reached-flag BEFORE
        // firing the notify. A test that awaits via
        // `wait_until_post_reservation` observes either the flag
        // (fast path) or the notify (slow path); the flag closes the
        // lost-wakeup hole where the hook fires before the test
        // arms its await.
        cap.post_reservation_reached_flag
            .store(true, Ordering::Release);
        cap.post_reservation_reached.notify_waiters();
        if cap.post_reservation_hold.load(Ordering::Acquire) == 0 {
            return;
        }
        let notified = cap.post_reservation_release.notified();
        if cap.post_reservation_hold.load(Ordering::Acquire) > 0 {
            notified.await;
            // Decrement once per release.
            cap.post_reservation_hold.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Encode the mode into the atomic observability slot.
    fn store_last_mode(&self, mode: RebuildMode) {
        self.last_mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    // -----------------------------------------------------------------
    // Per-workspace watcher bridge (Task 7 Phase 7b2)
    // -----------------------------------------------------------------

    /// Idempotently spawn (if not already active) the per-workspace
    /// watcher + async dispatcher task pair for `(key, ws, root)`.
    ///
    /// # Idempotence
    ///
    /// Looks up `watchers[key]`:
    /// - If present AND the entry's `live` flag is `true`, returns
    ///   `Ok(())` without spawning — an active pair is already
    ///   producing dispatches.
    /// - If present AND `live == false` (the async task has started
    ///   its post-loop cleanup), prunes the stale entry and spawns a
    ///   new pair with a fresh generation.
    /// - If absent, spawns a new pair.
    ///
    /// # Shutdown lifecycle
    ///
    /// Each pair is cooperatively shut down by:
    /// - Eviction (`ws.rebuild_cancelled = true` propagates through
    ///   the cancellable watcher → blocking thread exits → mpsc
    ///   sender drops → async task exits), OR
    /// - The async task observing `DaemonError::WorkspaceEvicted`
    ///   from `handle_changes_with_git_state` → async task exits →
    ///   receiver drops → blocking thread's next send fails → exits.
    ///
    /// The async task's last action is `live.store(false)` followed
    /// by [`Self::reap_watcher`]`(&key, generation)` — removing the
    /// entry from the map via compare-and-remove on the generation.
    ///
    /// # Placement constraint
    ///
    /// This method lives on `RebuildDispatcher`, NOT `WorkspaceManager`
    /// (per the 7b2 RESUME_PROMPT constraint): coupling watcher
    /// lifecycle into `manager.get_or_load` would pollute the
    /// manager's responsibilities and create a dispatcher↔manager
    /// cycle. Test harnesses (and Task 9's future daemon bootstrap)
    /// call this method explicitly after `get_or_load` succeeds.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::Io`] if
    ///   [`sqry_core::watch::SourceTreeWatcher::new`] fails (typical
    ///   cause: `.gitignore` read error or
    ///   `notify::RecommendedWatcher::new` failure).
    pub async fn ensure_watching(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        ws: Arc<LoadedWorkspace>,
        root: PathBuf,
    ) -> Result<(), DaemonError> {
        // Hold `self.watchers` across the ENTIRE operation — check,
        // watcher construction, spawn, and insert. Releasing the
        // lock between the liveness check and the insert would
        // permit two concurrent callers for the same `WorkspaceKey`
        // to both pass the "no live entry" check and both spawn
        // watcher pairs; the later insert would replace the tracked
        // entry without stopping the earlier spawned pair (Tokio
        // `JoinHandle::drop` detaches, per the `WatcherEntry`
        // docstring), producing duplicate rebuild dispatches and
        // leaked watcher resources. This issue was flagged by the
        // 7b2 iter-0 feat review (MAJOR).
        //
        // Holding a `parking_lot::Mutex` across sync operations is
        // the intended usage. The critical section covers:
        //   1. Fast-path liveness check (atomic read).
        //   2. `next_watcher_generation.fetch_add` (atomic).
        //   3. `SourceTreeWatcher::new` — bounded sync I/O
        //      (.gitignore read + notify subscribe). Typically 1–5
        //      ms; does NOT block on any lock held by code that
        //      might reacquire `self.watchers`.
        //   4. Git-state priming — one `RwLock` write on the
        //      workspace's `last_indexed_git_state`. Distinct lock
        //      from `watchers`; no lock-order violation.
        //   5. `tokio::sync::mpsc::channel` — sync allocation.
        //   6. `tokio::spawn` + `tokio::task::spawn_blocking` —
        //      enqueue-only, do not yield. Task bodies may later
        //      call `reap_watcher`, which re-acquires
        //      `self.watchers`; parking_lot blocks the executor
        //      thread briefly until we release here. Acceptable
        //      because the window is sub-ms after spawn.
        //   7. `HashMap::insert` (instant).
        //
        // No `.await` points exist between steps 1–7, so the async
        // function's single poll progresses synchronously while
        // the lock is held; the lock is released at function
        // return (including the error-return path for
        // `SourceTreeWatcher::new` failures via `?`).
        let mut watchers = self.watchers.lock();

        // Step 1 — Fast-path liveness check.
        if let Some(entry) = watchers.get(key)
            && entry.live.load(Ordering::Acquire)
        {
            return Ok(());
        }

        // Step 2 — Allocate a monotonic generation token.
        let generation = self.next_watcher_generation.fetch_add(1, Ordering::Relaxed);

        // Step 3 — Construct the watcher (sync I/O). Errors bubble
        // up via `?`; the locked guard is dropped on return.
        let watcher = SourceTreeWatcher::new(&root).map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!(
                "failed to create watcher for {}: {e:#}",
                root.display()
            )))
        })?;

        // Step 4 — Prime `ws.last_indexed_git_state` with the
        // CURRENT git snapshot. Without this, the classifier has no
        // baseline on the first debounce window and every benign
        // `.git/` event would produce `git_change_class = None`
        // (which the bridge cannot distinguish from a real
        // divergence without a baseline). Priming is a single
        // atomic write — if a real git operation is in flight
        // concurrently, the snapshot captures whichever side wins;
        // subsequent debounce windows compare against it and
        // classify correctly.
        //
        // Only PRIME if no baseline exists yet. A respawn (after
        // evict+reload) should NOT overwrite a baseline that the
        // prior watcher successfully committed, because that
        // baseline reflects the last PUBLISHED graph's git state —
        // overwriting it would lose the classifier's memory.
        {
            let mut baseline = ws.last_indexed_git_state.write();
            if baseline.is_none() {
                *baseline = Some(watcher.git_state().current_state());
            }
        }

        // Step 5 — Bounded tokio mpsc: capacity 16 is generous —
        // the async consumer drains items at dispatch rate and the
        // blocking producer already consolidates many filesystem
        // events into a single ChangeSet before sending.
        let (tx, rx) = tokio::sync::mpsc::channel::<(ChangeSet, LastIndexedGitState)>(16);

        let debounce = Duration::from_millis(self.config.debounce_ms);
        // 100 ms is the design's recommended cancellation-poll cadence:
        // tight enough that an evicted workspace's watcher thread
        // terminates promptly, loose enough not to burn CPU on a
        // quiet repo.
        let cancel_poll_period = Duration::from_millis(100);

        // Liveness flag shared between the stored entry and the async
        // task's post-loop cleanup. Flipped to `false` BEFORE
        // reap_watcher is called so `ensure_watching` re-calls for
        // the same key observe "drained" rather than "live".
        let live = Arc::new(AtomicBool::new(true));

        // Step 6a — Spawn the blocking watcher thread.
        let blocking_handle = {
            let ws = Arc::clone(&ws);
            tokio::task::spawn_blocking(move || {
                watch_loop_blocking(&watcher, &tx, &ws, debounce, cancel_poll_period);
            })
        };

        // Step 6b — Spawn the async dispatcher task.
        let async_handle = {
            let dispatcher = Arc::clone(self);
            let key = key.clone();
            let ws = Arc::clone(&ws);
            let live_for_task = Arc::clone(&live);
            tokio::spawn(async move {
                dispatch_loop_async(&dispatcher, &key, &ws, rx).await;
                // Mark ourselves as draining BEFORE reap_watcher so a
                // concurrent ensure_watching observes the correct
                // liveness state.
                live_for_task.store(false, Ordering::Release);
                dispatcher.reap_watcher(&key, generation);
            })
        };

        // Step 7 — Prune any stale entry + insert the new one.
        // Prune covers the case where a prior entry existed with
        // `live == false` (observed by step 1 falling through).
        // `remove` is idempotent when no entry exists.
        watchers.remove(key);
        watchers.insert(
            key.clone(),
            WatcherEntry {
                generation,
                live,
                async_handle,
                blocking_handle,
            },
        );
        // `watchers` drops here → lock released.
        Ok(())
    }

    /// Remove the watcher entry for `key` if and only if the stored
    /// entry's generation equals `my_generation`. Called by the
    /// per-workspace async task as its LAST action before exit.
    ///
    /// # Why compare-and-remove
    ///
    /// A fast evict+reload sequence can result in:
    /// 1. Old watcher A (gen 0) exits cooperatively.
    /// 2. Before A's closure finishes, `ensure_watching` is called
    ///    again for the same key and observes A's `live == false`,
    ///    prunes A's entry, and inserts new watcher B (gen 1).
    /// 3. A's closure reaches its final statement — `reap_watcher`.
    ///
    /// Without a generation check, A's reap would delete B's entry.
    /// Compare-and-remove guarantees A's reap is a no-op because
    /// `entry.generation == 1 != 0 == my_generation`.
    ///
    /// # Test observability
    ///
    /// Exposed as `pub(crate)` because external callers should never
    /// need to force-reap a watcher — cooperative shutdown via
    /// `rebuild_cancelled` triggers reap automatically. Tests that
    /// assert on the map size use [`Self::watchers_len`].
    pub(crate) fn reap_watcher(&self, key: &WorkspaceKey, my_generation: u64) {
        let mut watchers = self.watchers.lock();
        if let Some(entry) = watchers.get(key)
            && entry.generation == my_generation
        {
            watchers.remove(key);
        }
    }

    /// **Test-only** size observation on the watchers map.
    ///
    /// Used by `rebuild_watcher_shutdown.rs` to assert that the
    /// eviction cascade reaches quiescence (both tasks exit AND the
    /// map entry is reaped). Production callers should consult
    /// workspace-level `status()` rather than the dispatcher's
    /// bookkeeping.
    #[doc(hidden)]
    #[must_use]
    pub fn watchers_len(&self) -> usize {
        self.watchers.lock().len()
    }
}

// ---------------------------------------------------------------------------
// Watcher bridge loops (Task 7 Phase 7b2)
// ---------------------------------------------------------------------------
//
// These are free functions (not `RebuildDispatcher` methods) so the
// closures passed to `tokio::task::spawn_blocking` and `tokio::spawn`
// in `ensure_watching` own only the state they need. The blocking
// loop is `Send + 'static` (captures the `SourceTreeWatcher`, mpsc
// sender, workspace Arc, and two Durations); the async loop is
// `Send + 'static` (captures the dispatcher Arc, workspace key+Arc,
// and the mpsc receiver).

/// Blocking watcher loop — runs on `tokio::task::spawn_blocking`.
///
/// Repeatedly calls
/// [`SourceTreeWatcher::wait_for_changes_cancellable`](sqry_core::watch::SourceTreeWatcher::wait_for_changes_cancellable)
/// using the workspace's last-indexed baseline as the classifier
/// reference. On each non-empty `ChangeSet`, captures the current
/// git state via `watcher.git_state().current_state()` and forwards
/// the `(ChangeSet, LastIndexedGitState)` pair to the async
/// dispatcher task via tokio mpsc.
///
/// # Termination
///
/// Exits on:
/// - `wait_for_changes_cancellable` returns `Ok(None)` — eviction
///   observed cooperatively via `ws.rebuild_cancelled`.
/// - `wait_for_changes_cancellable` returns `Err` — notify channel
///   disconnect (unrecoverable); logged at error level.
/// - `tx.blocking_send` returns `Err` — async receiver dropped
///   (normal shutdown); logged at debug.
fn watch_loop_blocking(
    watcher: &SourceTreeWatcher,
    tx: &tokio::sync::mpsc::Sender<(ChangeSet, LastIndexedGitState)>,
    ws: &LoadedWorkspace,
    debounce: Duration,
    cancel_poll_period: Duration,
) {
    loop {
        let last_git = ws.last_indexed_git_state.read().clone();
        match watcher.wait_for_changes_cancellable(
            debounce,
            last_git.as_ref(),
            &ws.rebuild_cancelled,
            cancel_poll_period,
        ) {
            Ok(None) => {
                tracing::info!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    "watcher cancelled; terminating blocking loop"
                );
                break;
            }
            Err(e) => {
                tracing::error!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    error = %e,
                    "watcher channel disconnected; terminating blocking loop"
                );
                break;
            }
            Ok(Some(cs)) if cs.is_empty() => {
                // Empty ChangeSet — watcher debounced a burst of
                // events that all got filtered (editor temps,
                // gitignored paths, .git/ internals). Do not wake
                // the async side; loop and wait for the next batch.
                continue;
            }
            Ok(Some(cs)) if cs.changed_files.is_empty() && !cs.requires_full_rebuild() => {
                // Git-state-only change whose classifier output does
                // NOT require a full rebuild (Noise or LocalCommit
                // class). A2 §B mandates: these classes are reported
                // for telemetry but do not trigger a rebuild by
                // themselves — a real commit that changed the working
                // tree was already observed as a source-tree event.
                // Skip silently; loop for the next debounce window.
                continue;
            }
            Ok(Some(cs)) if cs.changed_files.is_empty() && cs.requires_full_rebuild() => {
                // Empty-files + full-rebuild classification
                // (BranchSwitch or TreeDiverged) is either a TOCTOU
                // artifact or a graph-neutral git operation. In both
                // cases, skipping is correct:
                //
                // * **TOCTOU artifact.** Immediately after a git
                //   operation, the classifier's
                //   `git rev-parse HEAD HEAD^{tree}` subprocess can
                //   transiently return partial output, causing
                //   `current_state` fields to drop to `None` and the
                //   classifier to fall back to BranchSwitch (see
                //   `GitStateWatcher::classify` at
                //   `sqry-core/src/watch/git_state.rs:240-258`). A
                //   subsequent debounce window will re-observe once
                //   git settles and fire a legitimate dispatch if
                //   state actually diverged.
                //
                // * **Graph-neutral branch/tree move.** Git can
                //   genuinely switch refs without swapping
                //   working-tree content when both refs point at the
                //   same tree (for example,
                //   `git checkout other-branch` where `other-branch`
                //   is already at HEAD's tree). The classifier
                //   reports BranchSwitch because `head_ref` changed,
                //   but no source file events fire because no source
                //   content changed. The published graph is already
                //   consistent with the new ref — a rebuild would be
                //   pure overhead.
                //
                // A "real" tree divergence that our graph does not
                // yet reflect (a pull, a reset, a branch switch that
                // actually rewrites files) emits concrete source
                // file events the source-tree watcher captures; that
                // case falls through to the dispatch arm below and
                // triggers the rebuild as intended.
                tracing::debug!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    git_class = ?cs.git_change_class,
                    "skipping empty-files full-rebuild signal: TOCTOU or graph-neutral git move"
                );
                continue;
            }
            Ok(Some(cs)) => {
                // Capture the git state AS OF now (after debounce
                // completion). The async side will attach this to
                // the PendingRebuild via
                // `handle_changes_with_git_state`; the runner will
                // commit it to `ws.last_indexed_git_state` at
                // publish time.
                let new_git_state = watcher.git_state().current_state();
                if tx.blocking_send((cs, new_git_state)).is_err() {
                    tracing::debug!(
                        target: "sqry_daemon::watch",
                        workspace = %ws.key.source_root.display(),
                        "async dispatcher task dropped receiver; terminating blocking loop"
                    );
                    break;
                }
            }
        }
    }
}

/// Async dispatcher loop — runs on a `tokio::spawn`ed task.
///
/// Consumes `(ChangeSet, LastIndexedGitState)` pairs from `rx` and
/// dispatches each via
/// [`RebuildDispatcher::handle_changes_with_git_state`]. The runner
/// commits `ws.last_indexed_git_state` as part of its publish
/// bookkeeping, keyed off the attached snapshot.
///
/// # Termination
///
/// Exits on:
/// - `rx.recv()` returns `None` — blocking side exited, channel
///   closed; logged at debug.
/// - `handle_changes_with_git_state` returns
///   `Err(WorkspaceEvicted)` — workspace is gone; logged at info.
///
/// Transient errors (`MemoryBudgetExceeded`, `WorkspaceBuildFailed`,
/// `Io`) continue the loop — the baseline is not advanced (the
/// publish did not happen), so the next wait_for_changes_cancellable
/// call re-observes the divergence and retries.
async fn dispatch_loop_async(
    dispatcher: &Arc<RebuildDispatcher>,
    key: &WorkspaceKey,
    ws: &LoadedWorkspace,
    mut rx: tokio::sync::mpsc::Receiver<(ChangeSet, LastIndexedGitState)>,
) {
    loop {
        let Some((cs, new_git_state)) = rx.recv().await else {
            tracing::debug!(
                target: "sqry_daemon::watch",
                workspace = %ws.key.source_root.display(),
                "watcher channel closed; terminating async dispatcher"
            );
            break;
        };
        match dispatcher
            .handle_changes_with_git_state(key, cs, new_git_state)
            .await
        {
            Ok(()) => {
                // Baseline advance (if any) was handled by the
                // runner inside execute_one_rebuild at publish
                // time — nothing for the bridge to do here.
            }
            Err(DaemonError::WorkspaceEvicted { .. }) => {
                tracing::info!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    "workspace evicted; terminating async dispatcher"
                );
                break;
            }
            Err(e) => {
                tracing::warn!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    error = %e,
                    "rebuild failed; baseline unchanged, retrying on next change"
                );
                // loop continues
            }
        }
    }
}

/// Materialize a complete replacement graph on the current (blocking)
/// thread. Factored out of `execute_rebuild` so the blocking closure is a
/// plain free function.
///
/// RPI durable-rebuild correction: even an incremental-triggered rebuild
/// materializes a complete graph before the persistence transaction. The
/// prior incremental graph output is not currently a safe durable snapshot
/// source because CSR compaction can observe inconsistent row/edge counts.
/// Rebuild mode still records the scheduler decision, while durability uses a
/// complete graph artifact for both memory publish and filesystem commit.
///
/// Task 7 Phase 7c: takes a [`CancellationToken`] that's polled at
/// every pass boundary. A cancelled token produces
/// [`GraphBuilderError::Cancelled`] which this helper maps to
/// [`DaemonError::WorkspaceEvicted`] — cancellation only fires when
/// the workspace is evicted (the dispatcher's
/// [`spawn_cancellation_forwarder`] flips the token on observing
/// `ws.rebuild_cancelled = true`).
fn execute_rebuild_blocking(
    root: &std::path::Path,
    _prior: &Arc<CodeGraph>,
    mode: RebuildMode,
    _changes: ChangeSet,
    plugins: &PluginManager,
    cfg: &BuildConfig,
    cancellation: &CancellationToken,
) -> Result<RebuildGraphOutput, DaemonError> {
    match mode {
        RebuildMode::Full | RebuildMode::Incremental => {
            let stage = match mode {
                RebuildMode::Full => "full rebuild",
                RebuildMode::Incremental => "incremental-triggered durable full rebuild",
            };
            match build_unified_graph_with_progress_cancellable(
                root,
                plugins,
                cfg,
                sqry_core::progress::no_op_reporter(),
                cancellation,
            ) {
                Ok((graph, effective_threads)) => Ok(RebuildGraphOutput {
                    graph,
                    effective_threads,
                }),
                Err(e) => Err(map_graph_builder_err(e, root.to_path_buf(), stage)),
            }
        }
    }
}

fn persist_rebuild_output_blocking(
    root: PathBuf,
    built: RebuildGraphOutput,
    plugins: &PluginManager,
    cfg: &BuildConfig,
    mode: RebuildMode,
) -> Result<DurableRebuildOutput, DaemonError> {
    let build_command = match mode {
        RebuildMode::Full => "daemon:rebuild:full",
        RebuildMode::Incremental => "daemon:rebuild:incremental",
    };
    let RebuildGraphOutput {
        graph,
        effective_threads,
    } = built;
    persist_durable_graph_transaction(
        graph,
        DurableGraphPersistenceRequest {
            root: &root,
            plugins,
            config: cfg,
            build_command,
            plugin_selection: inferred_plugin_selection_manifest(plugins),
            progress: sqry_core::progress::no_op_reporter(),
            effective_threads,
        },
    )
    .map(|(graph, build_result)| DurableRebuildOutput {
        graph,
        build_result,
    })
    .map_err(|err| DaemonError::WorkspaceBuildFailed {
        root,
        reason: format!("durable graph persistence transaction failed: {err:#}"),
    })
}

/// Map a sqry-core [`GraphBuilderError`] to the daemon surface type.
///
/// - `Cancelled` → [`DaemonError::WorkspaceEvicted`] (JSON-RPC -32004).
///   Cancellation only fires on eviction in the current design, so the
///   evicted-workspace termination signal is the correct mapping.
/// - Any other variant → [`DaemonError::WorkspaceBuildFailed`] (-32001)
///   with a human-readable reason prefixed by `stage`.
fn map_graph_builder_err(err: GraphBuilderError, root: PathBuf, stage: &str) -> DaemonError {
    match err {
        GraphBuilderError::Cancelled => DaemonError::WorkspaceEvicted { root },
        other => DaemonError::WorkspaceBuildFailed {
            root,
            reason: format!("{stage}: {other}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Cancellation forwarder (Task 7 Phase 7c)
// ---------------------------------------------------------------------------

/// Poll period for the cancellation forwarder. 50 ms is coarse enough
/// to keep the background task's CPU footprint negligible while still
/// bounding cancellation latency at `50ms + next pass boundary`. Tests
/// that need faster propagation can lower via a future hook; the
/// constant is sufficient for production.
const CANCEL_FORWARDER_POLL_MS: u64 = 50;

/// Spawn a tokio task that mirrors `ws.rebuild_cancelled` into
/// `token`. The task polls the atomic on a [`CANCEL_FORWARDER_POLL_MS`]
/// cadence; the first observation of `true` calls `token.cancel()`
/// and exits.
///
/// The returned [`JoinHandle`] MUST be `abort()`ed by the caller after
/// the rebuild future completes — otherwise a quiet workspace (no
/// eviction) leaves the polling task running until the runtime is
/// dropped.
///
/// Task 7 Phase 7c rationale (Codex iter-2 Q2, Q9): a `Notify`-based
/// forwarder is not demonstrably better here — the atomic remains the
/// authoritative source of truth, lock-free, with
/// `Release`/`Acquire` ordering. Polling adds one atomic load every
/// 50 ms, which is negligible against rebuild timescales (seconds).
fn spawn_cancellation_forwarder(
    ws: Arc<LoadedWorkspace>,
    token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if ws.rebuild_cancelled.load(Ordering::Acquire) {
                token.cancel();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(CANCEL_FORWARDER_POLL_MS)).await;
        }
    })
}

// ---------------------------------------------------------------------------
// DrainLoopSentinel — panic-safety for rebuild_in_flight (Task 7 Phase 7b1)
// ---------------------------------------------------------------------------

/// Panic-safety sentinel for the Phase B drain loop in
/// [`RebuildDispatcher::handle_changes`].
///
/// Guarantees that [`LoadedWorkspace::rebuild_in_flight`] is released
/// if `handle_changes` unwinds abnormally. The normal path disarms
/// the sentinel (`armed = false`) after releasing `rebuild_in_flight`
/// under the lane lock inside the drain-loop-exit block; the Drop
/// impl is a no-op on the happy path.
///
/// # Narrow race on the unwind path
///
/// If `handle_changes` unwinds abnormally, the Drop impl stores
/// `rebuild_in_flight = false` WITHOUT re-acquiring the lane. There
/// is a narrow window between the unwind and the Drop store during
/// which a concurrent caller can:
///
/// 1. Lock the lane (freed by the unwinding guard's tokio-Mutex drop).
/// 2. Coalesce incoming with whatever was in the lane.
/// 3. Observe `rebuild_in_flight = true` (we have not stored `false`
///    yet).
/// 4. Park coalesced in the lane and return `Ok(())`.
///
/// After the sentinel's Drop stores `false`, that parked
/// `PendingRebuild` sits without a runner until the NEXT dispatch
/// arrives — which will see `rebuild_in_flight = false`, take the
/// runner role, drain the stranded pending, and process it.
///
/// This is accepted as defense-in-depth: the only realistic trigger
/// for a `handle_changes` unwind is a runtime-level failure (OOM,
/// tokio internal panic) in which case the daemon is already in
/// damage-control territory. Plugin panics during the rebuild
/// pipeline are caught by `spawn_blocking` inside
/// [`RebuildDispatcher::execute_rebuild`] and mapped to
/// [`DaemonError::WorkspaceBuildFailed`], which flows through the
/// drain loop as `last_result` — NOT as an unwind.
struct DrainLoopSentinel {
    /// Shared workspace ref so the `Drop` impl outlives any borrow.
    ws: Arc<LoadedWorkspace>,
    /// Disarmed (`false`) after the normal-path under-lane release
    /// in the drain loop's exit blocks.
    armed: bool,
}

impl Drop for DrainLoopSentinel {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        tracing::error!(
            target: "sqry_daemon::rebuild",
            workspace = %self.ws.key.source_root.display(),
            "handle_changes unwound with armed DrainLoopSentinel — \
             releasing rebuild_in_flight defensively; any parked \
             PendingRebuild will be processed on the next dispatch"
        );
        self.ws.rebuild_in_flight.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Inline unit tests — narrow helpers only.
// ---------------------------------------------------------------------------
//
// The exhaustive decision-fork / coalesce-algebra / integration
// matrices live in the `tests/rebuild_*` binaries. These inline
// tests pin down the private helpers (`RebuildMode` encoding,
// `merge_git_class`, `DrainLoopSentinel` Drop semantics) that the
// external binaries exercise indirectly.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_mode_u8_roundtrip() {
        for mode in [RebuildMode::Full, RebuildMode::Incremental] {
            let encoded = mode.as_u8();
            assert_eq!(RebuildMode::from_u8(encoded), Some(mode));
        }
        // Unset (fresh AtomicU8) → None.
        assert_eq!(RebuildMode::from_u8(0), None);
        // Out-of-range → None.
        assert_eq!(RebuildMode::from_u8(3), None);
        assert_eq!(RebuildMode::from_u8(255), None);
    }

    #[test]
    fn merge_git_class_full_rebuild_dominance_canonicalises_to_tree_diverged() {
        for full_variant in [GitChangeClass::BranchSwitch, GitChangeClass::TreeDiverged] {
            for non_full in [
                None,
                Some(GitChangeClass::LocalCommit),
                Some(GitChangeClass::Noise),
            ] {
                assert_eq!(
                    merge_git_class(Some(full_variant), non_full),
                    Some(GitChangeClass::TreeDiverged),
                );
                assert_eq!(
                    merge_git_class(non_full, Some(full_variant)),
                    Some(GitChangeClass::TreeDiverged),
                );
            }
        }
    }

    #[test]
    fn merge_git_class_non_full_later_wins() {
        assert_eq!(
            merge_git_class(
                Some(GitChangeClass::LocalCommit),
                Some(GitChangeClass::Noise)
            ),
            Some(GitChangeClass::Noise),
        );
        assert_eq!(
            merge_git_class(
                Some(GitChangeClass::Noise),
                Some(GitChangeClass::LocalCommit)
            ),
            Some(GitChangeClass::LocalCommit),
        );
    }

    #[test]
    fn merge_git_class_absorbs_none_symmetrically() {
        assert_eq!(merge_git_class(None, None), None);
        assert_eq!(
            merge_git_class(None, Some(GitChangeClass::Noise)),
            Some(GitChangeClass::Noise),
        );
        assert_eq!(
            merge_git_class(Some(GitChangeClass::LocalCommit), None),
            Some(GitChangeClass::LocalCommit),
        );
    }

    // ---------------------------------------------------------------
    // DrainLoopSentinel (Task 7 Phase 7b1)
    // ---------------------------------------------------------------

    fn make_sentinel_workspace() -> Arc<LoadedWorkspace> {
        use sqry_core::project::ProjectRootMode;
        Arc::new(LoadedWorkspace::new(
            WorkspaceKey::new(
                std::path::PathBuf::from("/repos/sentinel-test"),
                ProjectRootMode::GitRoot,
                0xBEEF,
            ),
            false,
        ))
    }

    #[test]
    fn drain_loop_sentinel_disarmed_is_noop() {
        // Normal path: the drain loop disarms the sentinel after the
        // under-lane release. Dropping a disarmed sentinel must NOT
        // touch `rebuild_in_flight` — the release already happened.
        let ws = make_sentinel_workspace();
        // Simulate the drain loop having already released the flag.
        ws.rebuild_in_flight.store(false, Ordering::Release);
        {
            let sentinel = DrainLoopSentinel {
                ws: Arc::clone(&ws),
                armed: false,
            };
            // sentinel dropped here — disarmed, Drop is a no-op.
            drop(sentinel);
        }
        assert!(
            !ws.rebuild_in_flight.load(Ordering::Acquire),
            "disarmed sentinel must not flip the flag"
        );
    }

    #[test]
    fn drain_loop_sentinel_armed_releases_in_flight_on_drop() {
        // Panic/unwind path: the sentinel is still armed when dropped.
        // Its Drop impl must release `rebuild_in_flight` so a future
        // caller can take the runner role.
        let ws = make_sentinel_workspace();
        ws.rebuild_in_flight.store(true, Ordering::Release);
        {
            let sentinel = DrainLoopSentinel {
                ws: Arc::clone(&ws),
                armed: true,
            };
            drop(sentinel);
        }
        assert!(
            !ws.rebuild_in_flight.load(Ordering::Acquire),
            "armed sentinel Drop must release rebuild_in_flight"
        );
    }

    // ---------------------------------------------------------------
    // gate_check — TestGate plumbing (Task 7 Phase 7b2)
    // ---------------------------------------------------------------
    //
    // These tests stand up a RebuildDispatcher with no workspace and
    // exercise the gate_check helper in isolation. The dispatcher's
    // WorkspaceManager / PluginManager fields are initialised but
    // unused — gate_check only reads `self.test_gate`.

    fn make_dispatcher_for_gate_test() -> Arc<RebuildDispatcher> {
        let config = Arc::new(crate::config::DaemonConfig::default());
        let manager = crate::workspace::WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
        RebuildDispatcher::new(manager, config, plugins)
    }

    #[tokio::test]
    async fn gate_check_is_noop_when_no_gate_installed() {
        // Production fast path: `test_gate.get()` returns `None`, the
        // helper short-circuits before allocating a `notified()`
        // future, and execute_one_rebuild proceeds immediately.
        let dispatcher = make_dispatcher_for_gate_test();
        // Call gate_check; it must return without awaiting anything.
        tokio::time::timeout(Duration::from_millis(50), dispatcher.gate_check())
            .await
            .expect("gate_check with no installed gate must return immediately");
    }

    #[tokio::test]
    async fn gate_check_blocks_then_decrements_hold_on_release() {
        // Install a gate with hold=1. The first gate_check blocks
        // until notify_one is fired; after decrementing, subsequent
        // gate_checks pass through immediately because hold==0.
        let dispatcher = make_dispatcher_for_gate_test();
        let gate = Arc::new(TestGate {
            hold: AtomicUsize::new(1),
            release: tokio::sync::Notify::new(),
        });
        dispatcher
            .install_test_gate(Arc::clone(&gate))
            .expect("first install must succeed");

        // Spawn a task that will block in gate_check.
        let dispatcher_for_task = Arc::clone(&dispatcher);
        let blocked = tokio::spawn(async move { dispatcher_for_task.gate_check().await });

        // Give the task a moment to enter gate_check's await.
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !blocked.is_finished(),
            "gate_check must block while hold > 0"
        );

        // Release. The blocked task wakes and decrements hold.
        gate.release.notify_one();
        tokio::time::timeout(Duration::from_millis(500), blocked)
            .await
            .expect("gate_check must complete promptly after release")
            .expect("task panicked");

        // hold must have been decremented to 0.
        assert_eq!(
            gate.hold.load(Ordering::Acquire),
            0,
            "gate release must decrement hold"
        );

        // Subsequent gate_check is a no-op (hold==0 short-circuit).
        tokio::time::timeout(Duration::from_millis(50), dispatcher.gate_check())
            .await
            .expect("gate_check with hold==0 must return immediately without awaiting");
    }

    // ─────────────────────────────────────────────────────────────────
    // Cluster-G iter-3 BLOCKER 3 — `record_and_transition_on_err`
    // differentiates eviction from in-iteration `daemon reset`
    // cancellation.
    // ─────────────────────────────────────────────────────────────────

    /// Eviction path: state was already written to `Evicted` by
    /// `execute_eviction` under `workspaces.write()` BEFORE the
    /// rebuild_cancelled flag was flipped. The runner observes
    /// `WorkspaceEvicted`, calls `record_and_transition_on_err`, and
    /// the helper must NOT clobber the Evicted state.
    #[test]
    fn record_and_transition_on_err_preserves_evicted_state() {
        let dispatcher = make_dispatcher_for_gate_test();
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(crate::workspace::state::WorkspaceState::Evicted);
        let err = DaemonError::WorkspaceEvicted {
            root: std::path::PathBuf::from("/repo"),
        };
        dispatcher.record_and_transition_on_err(&ws, &err);
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Evicted,
            "eviction-path WorkspaceEvicted must NOT transition state"
        );
    }

    /// `daemon reset` path (cluster-G iter-3 BLOCKER 3 fix): reset
    /// fired AFTER the runner's top-of-loop gate but BEFORE the
    /// publish recheck. The runner converts the cancellation into
    /// `WorkspaceEvicted` while state is still `Rebuilding` (no
    /// eviction wrote `Evicted` because no eviction actually
    /// happened — `WorkspaceManager::reset`'s `Rebuilding` arm only
    /// flipped `rebuild_cancelled` and returned
    /// `ResetCancellationDispatched`). The helper MUST transition to
    /// `Unloaded` so the next `daemon load` recovers the workspace
    /// (without this fix, the workspace stays stuck in `Rebuilding`
    /// until `daemon stop && daemon start`).
    #[test]
    fn record_and_transition_on_err_unloads_reset_in_iteration_cancel() {
        let dispatcher = make_dispatcher_for_gate_test();
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        // Runner is mid-iteration: state is `Rebuilding` (the
        // distinguisher between eviction and reset).
        ws.store_state(crate::workspace::state::WorkspaceState::Rebuilding);
        let err = DaemonError::WorkspaceEvicted {
            root: std::path::PathBuf::from("/repo"),
        };
        dispatcher.record_and_transition_on_err(&ws, &err);
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Unloaded,
            "in-iteration reset cancellation must transition Rebuilding → Unloaded; \
             without this, daemon reset → daemon load cannot recover"
        );
    }

    /// Sanity: any other `DaemonError` (e.g. `WorkspaceOversize`,
    /// `Internal`) → `Failed` regardless of starting state.
    #[test]
    fn record_and_transition_on_err_failed_for_non_eviction_errors() {
        let dispatcher = make_dispatcher_for_gate_test();
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(crate::workspace::state::WorkspaceState::Rebuilding);
        let err = DaemonError::Internal(anyhow::anyhow!("plugin panic"));
        dispatcher.record_and_transition_on_err(&ws, &err);
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Failed,
            "non-eviction errors must transition to Failed"
        );
    }
}
