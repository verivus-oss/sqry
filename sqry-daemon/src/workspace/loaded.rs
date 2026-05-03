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
//! [`ArcSwap<CodeGraph>`] owns the published graph. Queries take
//! `graph.load_full()` to get a stable `Arc<CodeGraph>` that survives
//! a concurrent `publish_and_retain` swap; the retention reaper is
//! responsible for eventually dropping the superseded `Arc`.
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
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use sqry_core::graph::CodeGraph;
use sqry_core::watch::{ChangeSet, LastIndexedGitState};
use sqry_nl::Translator;

use crate::error::DaemonError;

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
}

/// Per-workspace runtime state owned by the
/// [`super::WorkspaceManager::workspaces`] map.
///
/// Mutating state:
///
/// - `graph`, `state`, `memory_bytes`, `memory_high_water_bytes`,
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

    /// Published graph. Readers call `graph.load_full()`.
    ///
    /// Only [`crate::workspace::publish::publish_and_retain`] swaps a
    /// new graph in; eviction stores a fresh empty graph so the
    /// ArcSwap remains non-null (simpler than Option-wrapping).
    pub graph: ArcSwap<CodeGraph>,

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
    /// dispatcher (Task 7).
    pub rebuild_lane: tokio::sync::Mutex<Option<PendingRebuild>>,

    /// Lock-free cancellation signal for in-flight rebuilds. Set by
    /// `evict_lru` / explicit `unload` and polled by the rebuild
    /// pipeline at each pass boundary. Once set, the running rebuild
    /// aborts, drops its `RebuildReservation`, and never publishes.
    pub rebuild_cancelled: AtomicBool,

    /// Runner-role gate for the per-workspace rebuild serial consumer
    /// (A2 §J.2, Task 7 Phase 7b1). When `true`, exactly one
    /// [`crate::RebuildDispatcher::handle_changes`] call is actively
    /// running the full rebuild pipeline (the Phase B drain loop in
    /// `rebuild.rs`). A concurrent caller observing `true` under the
    /// [`Self::rebuild_lane`] lock MUST park its coalesced
    /// [`PendingRebuild`] in the lane and return `Ok(())` without
    /// executing — the active runner will drain the lane at its next
    /// drain-loop iteration.
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
    /// - `RebuildDispatcher::handle_changes` Phase A (`false → true`
    ///   under lane).
    /// - `RebuildDispatcher::handle_changes` Phase B drain-loop exit
    ///   (`true → false` under lane, when the lane is empty or the
    ///   top-of-loop eviction gate fires).
    /// - `DrainLoopSentinel::drop` panic-safety path (`true → false`,
    ///   bare atomic store, narrow race).
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

    /// NL07 — lazily-initialised, per-workspace
    /// [`sqry_nl::Translator`] backing the daemon-hosted `sqry_ask`
    /// MCP tool.
    ///
    /// Init cost is heavy (O(seconds) — N parallel ONNX session
    /// loads from the [`sqry_nl::classifier::ClassifierPool`]) so the
    /// daemon defers it to the first `sqry_ask` call against the
    /// workspace via [`OnceCell::get_or_try_init`]. Subsequent calls
    /// receive a cheap `Arc<Translator>` clone.
    ///
    /// Lifetime: tied to [`LoadedWorkspace`] itself. Workspace
    /// eviction (LRU, explicit unload, daemon shutdown) drops the
    /// `OnceCell`, which drops the `Arc<Translator>`, which drops
    /// the pool's `N` classifier sessions and frees their model
    /// weights. No explicit cleanup required.
    ///
    /// Concurrency: the cell itself is `Send + Sync`. The
    /// `Translator` inside uses an internal sync pool that the
    /// daemon's MCP host calls under `tokio::task::spawn_blocking`
    /// so the daemon's tokio runtime never blocks on `recv()`.
    pub nl_translator: OnceCell<Arc<Translator>>,
}

impl LoadedWorkspace {
    /// Construct a fresh workspace entry with an empty initial graph.
    ///
    /// The empty graph is `Arc`-cheap (sub-kilobyte) so keeping the
    /// `ArcSwap` non-null for the entire workspace lifetime avoids an
    /// `Option` layer on the query path. Eviction stores another
    /// empty graph through the same ArcSwap; re-load overwrites it.
    #[must_use]
    pub fn new(key: WorkspaceKey, pinned: bool) -> Self {
        Self {
            key,
            graph: ArcSwap::from_pointee(CodeGraph::new()),
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
            rebuild_in_flight: AtomicBool::new(false),
            last_indexed_git_state: RwLock::new(None),
            nl_translator: OnceCell::new(),
        }
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
    pub fn store_state(&self, new_state: WorkspaceState) {
        self.state.store(new_state.as_u8(), Ordering::Release);
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
