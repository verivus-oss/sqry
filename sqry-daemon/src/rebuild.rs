//! Rebuild dispatcher for the sqryd daemon (Task 7 Phase 7a + 7b1).
//!
//! The [`RebuildDispatcher`] owns the per-workspace call path that
//! maps a debounced [`ChangeSet`] from the [`sqry_core::watch`] layer
//! through to a rebuild, and publishes the resulting [`CodeGraph`] via
//! [`WorkspaceManager::publish_and_retain`](crate::workspace::WorkspaceManager::publish_and_retain).
//!
//! Phase 7a shipped the synchronous skeleton (coalesce-then-execute
//! single-caller driver). Phase 7b1 adds the A2 §J.2 runner-role gate
//! so concurrent callers serialise cleanly:
//!
//! - [`PendingRebuild::coalesce_with`]: A2 §J.2 lane-merge algebra
//!   (union of files, OR of `git_state_changed`, max of `enqueued_at`,
//!   full-rebuild-dominance merge on `git_change_class`), applied to the
//!   pairs [`PendingRebuild::merges_with`] accepts.
//! - [`RebuildDispatcher::handle_changes`]: Phase A (acquire-or-park
//!   runner role via [`LoadedWorkspace::rebuild_in_flight`] CAS under
//!   the [`LoadedWorkspace::rebuild_lane`] mutex, refusing a request
//!   that does not merge with the parked one) + Phase B (drain loop:
//!   cancellation gate → pipeline → re-lock-and-drain → loop-or-exit).
//! - [`RebuildDispatcher::execute_one_rebuild`]: one iteration of
//!   the pipeline: decide / resolve inputs / estimate / reserve /
//!   execute (`spawn_blocking`) / publish / record success, refusal or
//!   failure.
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
//! Every iteration, the incremental-triggered ones included, builds the
//! whole graph (`build_unified_graph_with_progress_cancellable`) with the
//! inputs resolved from the manifest, the recorded macro build options
//! among them, before the durable persist; the mode records the
//! scheduler's decision and sizes the working-set estimate
//! (`execute_rebuild_blocking`). The core's `incremental_rebuild` is not
//! called. The [`CancellationToken`] mirroring
//! [`LoadedWorkspace::rebuild_cancelled`] is polled at the build's pass
//! boundaries.
//!
//! # §J.2 runner-role invariant (Phase 7b1)
//!
//! At most one [`execute_one_rebuild`](RebuildDispatcher::execute_one_rebuild)
//! executes at a time per workspace, and at most one additional
//! [`PendingRebuild`] is parked in the lane awaiting the runner. A
//! caller arriving while the runner is active merges its incoming
//! request into the lane (A2 §J.2 merge rules) and returns `Ok(())`
//! without running the pipeline; the active runner will drain the lane
//! at its next drain-loop iteration. A request whose macro options the
//! parked entry does not share ([`PendingRebuild::merges_with`]) is
//! refused instead (`InvalidArgument`), and the parked entry is left as
//! it was.
//!
//! All normal-path transitions of [`LoadedWorkspace::rebuild_in_flight`]
//! happen while [`LoadedWorkspace::rebuild_lane`] is held.
//! [`DrainLoopSentinel::drop`] is the sole recovery exception.
//!
//! # Eviction and cancellation cooperation
//!
//! Every drain-loop iteration (including the first), and the drain loop
//! once more after its last iteration, checks `ws.rebuild_cancelled` at
//! its cancellation gate. If set, the runner consumes it (leaving a
//! completed eviction's flag for the next load), abandons any parked
//! pending, releases `rebuild_in_flight` under the lane, and returns
//! [`DaemonError::WorkspaceEvicted`]. The same
//! [`DaemonError::WorkspaceEvicted`] is surfaced by
//! [`WorkspaceManager::reserve_rebuild`]'s Phase-1 membership +
//! cancellation check — so a gate-check → `reserve_rebuild` race that
//! eviction wins cannot publish into an orphaned workspace.
//!
//! # Lock order (§J.4)
//!
//! [`RebuildDispatcher`] is the only code that waits for
//! [`LoadedWorkspace::rebuild_lane`](crate::workspace::LoadedWorkspace::rebuild_lane).
//! [`WorkspaceManager::reset`] also takes it, without waiting
//! (`try_lock` under `workspaces.write()`, retried with the guard
//! released), so a reset and the runner role cannot interleave (decision
//! D-i7-3). The canonical call path honours the A2 §J.4 total order:
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
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
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
            MacroOptionsError, MacroOptionsRequest, UnreadableManifestRule,
            build_unified_graph_with_progress_cancellable, compute_reverse_dep_closure,
            count_buildable_files, persist_durable_graph_transaction, resolve_macro_options,
        },
        memory::GraphMemorySize,
        persistence::{
            GraphStorage, IndexRemovedDuringPersist, IndexWriteLock, LockWait, PersistGate,
            holds_committed_index,
        },
    },
};
use sqry_core::plugin::PluginManager;
use sqry_core::watch::{ChangeSet, GitChangeClass, LastIndexedGitState, SourceTreeWatcher};
use sqry_plugin_registry::{
    PluginSelectionError, RosterSource, UnreadableManifestPolicy, resolve_persisted_selection,
};
use tokio::task::JoinHandle;

use crate::{
    config::{DaemonConfig, ESTIMATE_FINAL_PER_FILE_BYTES, ESTIMATE_STAGING_PER_FILE_BYTES},
    error::DaemonError,
    workspace::{
        BuiltGraph, LoadedWorkspace, PendingRebuild, RebuildRequester, RebuildReservation,
        RebuildWaiters, WorkingSetInputs, WorkspaceKey, WorkspaceManager, WorkspaceState,
        clone_err,
        loaded::PublishedGraph,
        roster::{ResolvedRoster, RosterRecord, WorkspaceRosterResolver, restore_command},
        working_set_estimate,
    },
};

// ---------------------------------------------------------------------------
// RebuildMode
// ---------------------------------------------------------------------------

/// Outcome of the hybrid decision function: does the change need a full
/// rebuild, or would its reverse-dependency closure do? Both modes build
/// the whole graph (`execute_rebuild_blocking`: an incremental graph is
/// not a safe durable snapshot source); the mode sizes the working-set
/// estimate and is reported (`was_full`, [`RebuildDispatcher::last_mode`]).
///
/// Encoded as `u8` for the [`RebuildDispatcher::last_mode`] atomic
/// observability surface. The encoding is stable across the
/// dispatcher's lifetime but is not part of any on-wire contract —
/// Task 8's IPC layer surfaces the mode only through structured
/// tracing, not a raw byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildMode {
    /// The change needs a full rebuild.
    Full,
    /// The change's reverse-dependency closure is small: the scheduler's
    /// incremental mode, which still builds and persists the whole graph
    /// (the core's `incremental_rebuild` is not called).
    Incremental,
}

struct RebuildGraphOutput {
    graph: CodeGraph,
    effective_threads: usize,
}

struct DurableRebuildOutput {
    graph: CodeGraph,
    build_result: BuildResult,
    /// The roster the graph was built with and the manifest now records.
    roster: Arc<RosterRecord>,
}

/// The inputs one durable rebuild builds and persists with, resolved and
/// accepted before any memory is reserved ([`resolve_durable_rebuild_inputs`]).
pub(crate) struct RebuildInputs {
    /// The roster the manifest records: the manager to build with and the
    /// record the persist writes.
    pub(crate) roster: ResolvedRoster,
    /// The build configuration, carrying the resolved macro build options.
    pub(crate) cfg: BuildConfig,
    /// The record of the generation resident when the inputs were resolved
    /// (`None` when nothing is resident). The narrowing guard compares
    /// against it when the manifest is gone ([`refuse_if_rebuild_narrows`]).
    pub(crate) resident: Option<Arc<RosterRecord>>,
    /// Whether this rebuild may replace a manifest it cannot read: `FallBack`
    /// for the daemon-hosted `rebuild_index` (design D9), `Refuse` for every
    /// other rebuild. The narrowing guard before the persist applies it.
    pub(crate) unreadable_policy: UnreadableManifestPolicy,
    /// What the inputs were resolved from, so the persist resolves them
    /// again under the index's persist lock and publishes the record current
    /// then (decision D-i8-5): the roster resolver, the build configuration
    /// before the macro options were applied, and the caller's macro
    /// options request.
    pub(crate) resolved_from: RebuildInputSources,
    /// `holds_committed_index` of the graph directory when these inputs
    /// were resolved. True selects the persist's existing-index wait (false
    /// the creating wait) and arms the content clause of its removal check
    /// (decision D-i8-6).
    pub(crate) index_present: bool,
}

/// The sources [`resolve_durable_rebuild_inputs`] resolved one rebuild's
/// inputs from, kept so the persist can resolve them again under the lock.
#[derive(Clone)]
pub(crate) struct RebuildInputSources {
    pub(crate) resolver: Arc<WorkspaceRosterResolver>,
    pub(crate) build_config: BuildConfig,
    pub(crate) macro_request: MacroOptionsRequest,
}

/// How one iteration's pipeline ended without a graph to publish. The
/// variant decides what the iteration does to the workspace state.
#[derive(Debug)]
enum PipelineError {
    /// Refused before anything was written (the narrowing guard ahead of
    /// the durable persist): the workspace returns to the state the
    /// iteration entered from ([`refused_state`]).
    Refused(DaemonError),
    /// The build failed or was cancelled, or the durable persist failed:
    /// [`RebuildDispatcher::record_and_transition_on_err`] decides.
    Failed(DaemonError),
}

/// The state a refused iteration returns the workspace to: the state it
/// entered from, because a refusal wrote nothing and the workspace is
/// exactly as the iteration found it. `Rebuilding` (an iteration that found
/// the workspace `Rebuilding` with no runner behind it) returns to `Loaded`,
/// since a refusal keeps the graph the slot holds.
const fn refused_state(entered: WorkspaceState) -> WorkspaceState {
    match entered {
        WorkspaceState::Rebuilding => WorkspaceState::Loaded,
        other => other,
    }
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
    /// 6. **`macro_request`: an empty later request keeps the earlier
    ///    one; a non-empty later request replaces it.** The dispatcher
    ///    calls this only for the pairs [`Self::merges_with`] accepts
    ///    (decision D-i7-1 in
    ///    `docs/development/surface-parity/04_PROGRESS-surface-parity.md`,
    ///    replacing D-w4r1-8): two requests that mean the same, or a
    ///    watcher-driven enqueue (no waiters, empty request) on either
    ///    side. For those pairs the rule is exact: a watcher enqueue never
    ///    erases a parked explicit request, an explicit request merged over
    ///    a parked watcher enqueue runs with its own options, and two
    ///    requests that mean the same run with either. Any other pair is
    ///    refused by the dispatcher (`-32602`) before it gets here, because
    ///    merging it would answer a caller for options it did not ask for.
    /// 7. **`waiters`: union.** Every caller merged into the entry
    ///    receives the outcome of the one iteration that consumes it.
    /// 8. **`requester`: [`RebuildRequester::merge`]**, the later in
    ///    [`RebuildRequester`]'s order, so the provenance the iteration
    ///    records names an explicit caller over the watcher, and
    ///    `rebuild_index` over `daemon/rebuild`; but `daemon/rebuild` and
    ///    `rebuild_index` together run with `daemon/rebuild`'s refusal of
    ///    an unreadable manifest (decision D-i8-40, which reconciles the
    ///    merge with D-i7-1; the fall-back itself is D-i7-8's): the
    ///    fall-back is only for a caller who asked for nothing else.
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

        // 6. macro_request: a later non-empty request wins, an empty later
        // request keeps the earlier one.
        let macro_request = if later.macro_request.is_empty() {
            self.macro_request
        } else {
            later.macro_request
        };

        // 7. waiters: union, earlier first.
        let mut waiters = self.waiters;
        waiters.absorb(later.waiters);

        // 8. requester: the later in precedence order, except that
        // `daemon/rebuild` and `rebuild_index` merge to the requester that
        // keeps `daemon/rebuild`'s refusal policy (audit S3, D-i8-40).
        let requester = self.requester.merge(later.requester);

        PendingRebuild {
            changes: ChangeSet {
                changed_files,
                git_state_changed,
                git_change_class,
            },
            enqueued_at,
            git_state_at_enqueue,
            macro_request,
            waiters,
            requester,
        }
    }

    /// Whether the dispatcher may merge `later` into this parked entry
    /// (decision D-i7-1 in
    /// `docs/development/surface-parity/04_PROGRESS-surface-parity.md`,
    /// replacing D-w4r1-8).
    ///
    /// Two entries merge when their macro requests mean the same
    /// ([`macro_requests_agree`]), or when either is a watcher-driven
    /// enqueue: no waiters and an empty request. Every other pair would run
    /// one iteration with options one side did not ask for and answer that
    /// side's callers with its outcome (a plain `daemon/rebuild` told
    /// `-32022` for an expand cache it never named, or an explicit request
    /// run without its options), so the dispatcher refuses the later
    /// request instead and the parked one keeps its options.
    #[must_use]
    pub fn merges_with(&self, later: &PendingRebuild) -> bool {
        macro_requests_agree(&self.macro_request, &later.macro_request)
            || self.is_watcher_enqueue()
            || later.is_watcher_enqueue()
    }

    /// A watcher-driven enqueue: nobody waits on it and it reuses the
    /// manifest's record.
    fn is_watcher_enqueue(&self) -> bool {
        self.waiters.is_empty() && self.macro_request.is_empty()
    }
}

/// Whether two macro build options requests mean the same: the same
/// `reset`, the same expand cache directory, and the same cfg flags as a
/// set (an absent component and an explicit empty list differ: the first
/// keeps the record, the second clears it). The directories are compared
/// as given, so a request is normalised first
/// ([`normalized_macro_request`]): `RebuildDispatcher::handle_changes_with_macro_options`
/// does that before the request parks.
#[must_use]
pub fn macro_requests_agree(a: &MacroOptionsRequest, b: &MacroOptionsRequest) -> bool {
    fn flag_set(request: &MacroOptionsRequest) -> Option<std::collections::BTreeSet<&String>> {
        request
            .cfg_flags
            .as_ref()
            .map(|flags| flags.iter().collect())
    }
    a.reset == b.reset && a.expand_cache_dir == b.expand_cache_dir && flag_set(a) == flag_set(b)
}

/// `request` with its expand cache directory in the form the build will
/// resolve it, so two spellings of one directory compare equal: a plain
/// relative directory (no root, no prefix) is anchored to `root`, as
/// `resolve_macro_options` anchors it, and the directory is then
/// canonicalised when it exists; when it does not, its longest existing
/// prefix is canonicalised and the rest is applied lexically (`.` dropped,
/// `..` taking the parent, the trailing separator gone), so two spellings
/// of a directory not yet created are one request too. An empty directory, and a relative
/// one with a root or a prefix (Windows `\dir`, `C:dir`), are left as given
/// for the resolver to decide. The cfg flags are left in the order given;
/// [`macro_requests_agree`] compares them as a set.
#[must_use]
pub fn normalized_macro_request(root: &Path, request: MacroOptionsRequest) -> MacroOptionsRequest {
    let MacroOptionsRequest {
        cfg_flags,
        expand_cache_dir,
        reset,
    } = request;
    MacroOptionsRequest {
        cfg_flags,
        expand_cache_dir: expand_cache_dir.map(|dir| normalized_expand_cache_dir(root, dir)),
        reset,
    }
}

fn normalized_expand_cache_dir(root: &Path, dir: PathBuf) -> PathBuf {
    use std::path::Component;

    if dir.as_os_str().is_empty() {
        return dir;
    }
    let plain_relative = dir.components().all(|component| {
        matches!(
            component,
            Component::Normal(_) | Component::CurDir | Component::ParentDir
        )
    });
    let anchored = if plain_relative {
        root.join(&dir)
    } else if dir.is_absolute() {
        dir
    } else {
        return dir;
    };
    if let Ok(canonical) = anchored.canonicalize() {
        return canonical;
    }
    // The directory does not exist yet. Its longest prefix that does exist
    // is canonicalised, so a symlink in it resolves as the filesystem will
    // resolve it; the rest is applied lexically, `.` dropped and `..` taking
    // the parent of what precedes it. No component of the rest exists, so
    // none of it is a symlink and the lexical answer is the filesystem's: so
    // `new/../cache` and `cache` name one directory, and `link/../cache`
    // names the link target's sibling, not the root's `cache`.
    let components: Vec<Component<'_>> = anchored.components().collect();
    let Some((existing, mut out)) = (1..=components.len()).rev().find_map(|end| {
        components[..end]
            .iter()
            .collect::<PathBuf>()
            .canonicalize()
            .ok()
            .map(|canonical| (end, canonical))
    }) else {
        return anchored.components().collect();
    };
    for component in &components[existing..] {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// How long a caller waits for its own rebuild outcome before it is
/// answered `RebuildOutcomeTimeout` (`-32000`, not retryable: the rebuild
/// continues): the `daemon/rebuild` handler and the
/// daemon-hosted `rebuild_index` resident path. The bound is on the
/// caller's wait only; the drain loop runs on its own task and is never
/// abandoned. [`RebuildDispatcher::outcome_wait`] is this value unless a
/// test shortened it.
pub const REBUILD_OUTCOME_WAIT: Duration = Duration::from_secs(600);

/// What one rebuild iteration published, delivered to every caller merged
/// into the entry it consumed: the generation (the graph and the roster
/// record as one value, design D20) and the mode the iteration ran. A
/// caller answers from this value, never from a later read of the slot,
/// which a later iteration may already have advanced.
#[derive(Debug, Clone)]
pub struct RebuildReport {
    /// The generation the iteration published.
    pub published: Arc<PublishedGraph>,
    /// The mode the iteration ran: `Full` for a forced or full-class
    /// change set, `Incremental` otherwise (an incremental-triggered
    /// rebuild still persists a complete graph).
    pub mode: RebuildMode,
}

/// One `daemon/rebuild` request's own outcome (integration of W1 and W4),
/// returned by [`RebuildDispatcher::handle_changes_with_macro_options`].
///
/// [`Self::wait`] resolves to the result of the drain-loop iteration that
/// consumed this request, as soon as that iteration ends, not after the
/// drain loop's later iterations. Dropping the outcome, or abandoning
/// [`Self::wait`] on a timeout, affects no rebuild: the drain loop runs on
/// its own task and the runner's delivery to a closed receiver is skipped.
#[derive(Debug)]
#[must_use = "a rebuild outcome must be waited on to learn whether the request was refused"]
pub struct RebuildOutcome {
    receiver: tokio::sync::oneshot::Receiver<Result<RebuildReport, DaemonError>>,
    /// What the dispatch call itself returned: the error that kept the
    /// request out of the lane when it never got there.
    call: Result<(), DaemonError>,
}

impl RebuildOutcome {
    /// This request's own result: the report of the iteration that
    /// consumed it.
    ///
    /// # Errors
    ///
    /// The error of the iteration that consumed this request (a refusal,
    /// a build or persist failure); [`DaemonError::WorkspaceEvicted`]
    /// (`-32004`) when the workspace was evicted or reset before or while
    /// the request ran, or when a `daemon/cancel_rebuild` cancelled it; the
    /// dispatch call's own error when the request never reached the lane
    /// (an unknown workspace, a queued request whose macro options it does
    /// not share); [`DaemonError::Internal`] when the runner unwound, or
    /// otherwise dropped this request, without reporting it. Never `Ok`
    /// without a report of a published generation.
    pub async fn wait(self) -> Result<RebuildReport, DaemonError> {
        match self.receiver.await {
            Ok(own) => own,
            Err(_) => match self.call {
                Err(err) => Err(err),
                Ok(()) => Err(DaemonError::Internal(anyhow::anyhow!(
                    "the rebuild runner exited before reporting this request's outcome"
                ))),
            },
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
/// 2. `changed_files.len() > config.incremental_threshold`: too many
///    changed files for the incremental mode's estimate.
/// 3. `closure.len() > graph.file_count() * closure_limit_percent /
///    100` — the reverse-dep closure would touch more files than the
///    full-rebuild cost; take the full path instead.
///
/// Otherwise, Incremental. Empty `ChangeSet` ([`ChangeSet::is_empty`])
/// returns Incremental as a legitimate no-op rebuild.
///
/// Closure math resolves only paths already present in the graph's
/// file registry. Paths not yet registered (new files) contribute
/// zero to the closure; the iteration builds the whole graph, so a new
/// file is parsed either way.
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

    /// Test-only switch (round 8, decision D-i8-5): when `true`, the
    /// durable persist cancels the iteration's token just before it builds
    /// again under the persist lock (the record changed during the build),
    /// as the forwarder would if a cancel landed then. With the forwarder
    /// suppressed, that is the one point the token is cancelled, so the
    /// rebuild under the lock is the stage that must observe it.
    ///
    /// Production leaves this `false`.
    pub cancel_token_at_rebuild_under_lock: AtomicBool,

    /// Set by the durable persist when its first attempt at the index's
    /// persist lock finds another holder (round 8, decision D-i8-6), so a
    /// test can act while the persist waits for that holder.
    pub persist_lock_contended: AtomicBool,

    /// Test-only switch (audit N6): when `true`, the durable persist
    /// cancels the iteration's token right after the persist lock is
    /// handed to it, past the wait's own last check, as a cancel landing
    /// during the hand-over would. Production leaves this `false`.
    pub cancel_token_after_lock_acquired: AtomicBool,

    /// Test-only switch (fourth audit, item 2): when `true`, the durable
    /// persist removes the workspace's `.sqry` directory after its own
    /// last check and just before the transaction, as a `sqry workspace
    /// clean` landing in that gap would. Production leaves this `false`.
    pub remove_index_before_transaction: AtomicBool,

    /// How many builds the durable persist started under the persist lock
    /// (fourth audit, item 4).
    pub rebuilds_under_lock: AtomicUsize,

    /// Test-only hold (round 8, audit item E): when armed, the durable
    /// persist stops just before it builds again under the persist lock,
    /// after any reservation that build takes, and waits for
    /// [`Self::release_rebuild_under_lock`]. `(reached, released)`.
    pub rebuild_under_lock_hold: AtomicBool,
    rebuild_under_lock_gate: (parking_lot::Mutex<(bool, bool)>, parking_lot::Condvar),

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

    /// Surface parity W1 round 5 (design D27; its stated position
    /// narrowed in round 6 by D31): counter of iterations that must stall
    /// at the post-publish hook ([`RebuildDispatcher::post_publish_check`]),
    /// which fires AFTER the publish block's read guard is released and
    /// BEFORE the publish hook is dispatched with the graph half of the
    /// pair `publish_and_retain` swapped in. The state store, the success
    /// bookkeeping and the `published_generations` push sit before it in
    /// the source; nothing in production observes their order relative to
    /// the seam, and T47 pins the seam's position relative to the guard
    /// release and the dispatch only. Armed via
    /// [`Self::arm_post_publish_hold`], released via
    /// [`Self::release_post_publish`]. The one place a second publication
    /// can run between the publish and the hook dispatch, so a test can
    /// observe which generation's graph the hook receives (T47).
    pub post_publish_hold: AtomicUsize,

    /// Fired when [`RebuildDispatcher::post_publish_check`] is entered.
    /// The test awaits [`Self::wait_until_post_publish`].
    pub post_publish_reached: tokio::sync::Notify,

    /// Durable flag set by [`RebuildDispatcher::post_publish_check`]
    /// before it fires `post_publish_reached`, the same lost-wakeup
    /// handshake as `post_reservation_reached_flag`.
    pub post_publish_reached_flag: AtomicBool,

    /// Fired by the test driver to release one iteration stalled at the
    /// post-publish hook.
    pub post_publish_release: tokio::sync::Notify,

    /// Every generation `execute_one_rebuild` published, in order, pushed
    /// before the post-publish hook fires: the pair the iteration swapped
    /// in, so a test can compare the graph the publish hook received with
    /// the graph half of the generation this iteration published (T47).
    pub published_generations: parking_lot::Mutex<Vec<Arc<PublishedGraph>>>,

    /// When `true`, the next `execute_one_rebuild` iteration to pass the
    /// test gate panics there (the flag is cleared as it fires), with the
    /// workspace `Rebuilding` and the runner role held: the runner unwind
    /// a test needs to observe what [`DrainLoopSentinel`] leaves behind.
    /// Production leaves this `false`.
    pub panic_in_next_iteration: AtomicBool,

    /// Counter of drain loops that must stall at the release hook
    /// ([`RebuildDispatcher::pre_release_check`]): after the loop found the
    /// lane empty and before it takes the lane again to release the runner
    /// role. A request that parks there is the one the release path must
    /// still take (integration round 7, plant P23). Armed via
    /// [`Self::arm_pre_release_hold`], released via
    /// [`Self::release_pre_release`].
    pub pre_release_hold: AtomicUsize,

    /// Fired when [`RebuildDispatcher::pre_release_check`] is entered.
    pub pre_release_reached: tokio::sync::Notify,

    /// Durable flag set before `pre_release_reached` fires, the same
    /// lost-wakeup handshake as `post_publish_reached_flag`.
    pub pre_release_reached_flag: AtomicBool,

    /// Fired by the test driver to release one stalled drain loop.
    pub pre_release_release: tokio::sync::Notify,
}

impl TestCapture {
    /// Construct a zero-initialised capture. Same as
    /// [`Default::default`]; named constructor for clarity in tests.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Block until the armed persist reached its hold before the build
    /// under the persist lock ([`Self::rebuild_under_lock_hold`]), at most
    /// `timeout`; `true` once it has.
    pub fn wait_until_rebuild_under_lock(&self, timeout: std::time::Duration) -> bool {
        let (gate, changed) = &self.rebuild_under_lock_gate;
        let mut state = gate.lock();
        let deadline = std::time::Instant::now() + timeout;
        while !state.0 {
            if changed.wait_until(&mut state, deadline).timed_out() {
                return state.0;
            }
        }
        true
    }

    /// Release the persist held before its build under the persist lock.
    pub fn release_rebuild_under_lock(&self) {
        let (gate, changed) = &self.rebuild_under_lock_gate;
        gate.lock().1 = true;
        changed.notify_all();
    }

    fn hold_before_rebuild_under_lock(&self) {
        if !self.rebuild_under_lock_hold.swap(false, Ordering::AcqRel) {
            return;
        }
        let (gate, changed) = &self.rebuild_under_lock_gate;
        let mut state = gate.lock();
        state.0 = true;
        changed.notify_all();
        while !state.1 {
            changed.wait(&mut state);
        }
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

    /// Arm a single post-publish stall (surface parity W1 round 5, design
    /// D27; round 6, D31). The next `execute_one_rebuild` iteration that
    /// reaches [`RebuildDispatcher::post_publish_check`] (after the publish
    /// block's read guard is released, before it dispatches the publish
    /// hook) blocks until [`Self::release_post_publish`] is called. Stacks
    /// like [`Self::arm_post_reservation_hold`].
    pub fn arm_post_publish_hold(&self) {
        self.post_publish_hold.fetch_add(1, Ordering::AcqRel);
    }

    /// Arm a single stall at the release hook: the next drain loop that
    /// finds its lane empty blocks before it releases the runner role
    /// until [`Self::release_pre_release`] is called.
    pub fn arm_pre_release_hold(&self) {
        self.pre_release_hold.fetch_add(1, Ordering::AcqRel);
    }

    /// Release exactly one drain loop stalled at the release hook.
    pub fn release_pre_release(&self) {
        self.pre_release_release.notify_one();
    }

    /// Await a drain loop reaching the release hook (at once if one
    /// already has).
    pub async fn wait_until_pre_release(&self) {
        if self.pre_release_reached_flag.load(Ordering::Acquire) {
            return;
        }
        let notified = self.pre_release_reached.notified();
        if self.pre_release_reached_flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    /// Release exactly one iteration stalled at the post-publish hook.
    /// Lost-wakeup-safe through the handshake in
    /// [`RebuildDispatcher::post_publish_check`], as
    /// [`Self::release_post_reservation`] is.
    pub fn release_post_publish(&self) {
        self.post_publish_release.notify_one();
    }

    /// Await the next `execute_one_rebuild` iteration reaching the
    /// post-publish hook. Returns at once if the hook has already fired
    /// (`post_publish_reached_flag`); otherwise arms the `notified()`
    /// future BEFORE re-checking the flag, the same handshake as
    /// [`Self::wait_until_post_reservation`].
    pub async fn wait_until_post_publish(&self) {
        if self.post_publish_reached_flag.load(Ordering::Acquire) {
            return;
        }
        let notified = self.post_publish_reached.notified();
        if self.post_publish_reached_flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

/// One captured `execute_one_rebuild` iteration (test-only).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct CapturedIteration {
    /// The `ChangeSet` as-consumed by this iteration (post-coalesce).
    pub changeset: ChangeSet,
    /// Mode decided for this iteration.
    pub mode: RebuildMode,
    /// Git-state snapshot attached to the consumed `PendingRebuild`.
    /// `None` for direct (non-bridge) callers.
    pub git_state_at_enqueue: Option<LastIndexedGitState>,
    /// The macro build options request the iteration ran with (the merged
    /// entry's, after normalisation).
    pub macro_request: MacroOptionsRequest,
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
/// (Tokio's `JoinHandle::drop` is detach, not cancel, see
/// `tokio::task::JoinHandle` docs).
///
/// Shutdown is cooperative through [`Self::stop`], this watcher's own
/// stop signal (armed per watcher by [`LoadedWorkspace::arm_watcher_stop`]):
/// the blocking loop polls it, returns, and drops the mpsc sender, so the
/// async task's `rx.recv()` returns `None` and the async task exits too.
/// Three writers set it: the tombstone writers (eviction, `unload`,
/// `reset`, through [`LoadedWorkspace::stop_watcher`]), the next watcher
/// armed for the same workspace (through `arm_watcher_stop`), and
/// [`RebuildDispatcher::shutdown`]. A cancelled rebuild does not: the
/// async task keeps dispatching after a `WorkspaceEvicted` whose stop is
/// clear (a `daemon/cancel_rebuild`), and exits on one whose stop is set.
///
/// Before exiting, the async task flips [`Self::live`] to `false` and
/// then calls
/// [`RebuildDispatcher::reap_watcher`](crate::RebuildDispatcher::reap_watcher)
/// with its [`Self::generation`] to remove the entry from the map. The
/// generation token ensures a late old task cannot erase a newer
/// replacement entry after a fast evict+reload.
struct WatcherEntry {
    /// Monotonic generation token. Assigned at construction time from
    /// `RebuildDispatcher::next_watcher_generation`; used by
    /// `reap_watcher` to distinguish "my entry" from "a newer entry
    /// for the same `WorkspaceKey`".
    generation: u64,
    /// `true` while the async task is processing dispatches. Flipped
    /// to `false` as the first action of the post-loop cleanup
    /// sequence, BEFORE `reap_watcher` is called.
    live: Arc<AtomicBool>,
    /// This watcher's stop signal. Once set the watcher is draining even
    /// while `live` is still `true` (its blocking loop observes the signal
    /// on its next poll, up to 100 ms later), so [`Self::is_watching`] reads
    /// both: an entry whose stop is set is not watching, and
    /// `ensure_watching` replaces it rather than keep it.
    stop: Arc<AtomicBool>,
    /// Handle to the async dispatcher task. Stored only to keep the
    /// task attached for the entry's lifetime: never awaited or
    /// aborted. Shutdown is cooperative (see struct-level docs).
    #[allow(dead_code)]
    async_handle: JoinHandle<()>,
    /// Handle to the blocking watcher thread. Stored only for
    /// attachment: dropping it detaches the task, which continues to
    /// completion via cooperative cancellation.
    #[allow(dead_code)]
    blocking_handle: JoinHandle<()>,
}

impl WatcherEntry {
    /// Whether this watcher is watching: its async task is live and its
    /// stop signal is clear. A stopped watcher that has not exited yet is
    /// draining, not watching.
    fn is_watching(&self) -> bool {
        self.live.load(Ordering::Acquire) && !self.stop.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------
// RebuildDispatcher
// ---------------------------------------------------------------------------

/// Sole acquirer of [`LoadedWorkspace::rebuild_lane`] (A2 §J).
///
/// Constructed once at daemon startup with the daemon's shared
/// [`WorkspaceRosterResolver`], so every rebuild builds and persists the
/// roster the workspace manifest records (surface parity W1). Every
/// [`Self::handle_changes`] call honours
/// the canonical 7-step reservation call path (plan line 1495) and
/// the §J.4 lock-order contract documented on the module.
pub struct RebuildDispatcher {
    manager: Arc<WorkspaceManager>,
    config: Arc<DaemonConfig>,
    roster: Arc<WorkspaceRosterResolver>,
    build_config: BuildConfig,
    dispatched_count: AtomicU64,
    last_mode: AtomicU8,
    /// How long a caller waits for its own outcome, in milliseconds:
    /// [`REBUILD_OUTCOME_WAIT`] unless a test shortened it
    /// ([`Self::set_outcome_wait_for_test`]).
    outcome_wait_ms: AtomicU64,

    /// Per-workspace watcher+dispatcher task pairs (Task 7 Phase 7b2).
    /// Populated by [`Self::ensure_watching`]; pruned by
    /// [`Self::reap_watcher`] as each async task exits.
    watchers: parking_lot::Mutex<HashMap<WorkspaceKey, WatcherEntry>>,
    /// Monotonic counter used to tag each `WatcherEntry` with a unique
    /// generation, enabling `reap_watcher`'s compare-and-remove.
    next_watcher_generation: AtomicU64,
    /// Set by [`Self::shutdown`]: no watcher starts after it.
    shutting_down: AtomicBool,

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
    /// config, and roster resolver.
    ///
    /// `build_config` defaults to [`BuildConfig::default`]; Task 14
    /// calibration may override this from [`DaemonConfig`] knobs.
    #[must_use]
    pub fn new(
        manager: Arc<WorkspaceManager>,
        config: Arc<DaemonConfig>,
        roster: Arc<WorkspaceRosterResolver>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            config,
            roster,
            build_config: BuildConfig::default(),
            dispatched_count: AtomicU64::new(0),
            last_mode: AtomicU8::new(0),
            outcome_wait_ms: AtomicU64::new(
                u64::try_from(REBUILD_OUTCOME_WAIT.as_millis()).unwrap_or(u64::MAX),
            ),
            watchers: parking_lot::Mutex::new(HashMap::new()),
            next_watcher_generation: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
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
    ///
    /// Compiled only for tests and the `test-hooks` feature (fifth audit,
    /// item 2): some capture switches are destructive (one removes the
    /// workspace's `.sqry` directory), so a release build has no way to
    /// install one, and every switch stays inert there.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-hooks"))]
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

    /// How long a caller waits for its own rebuild outcome before it is
    /// answered `RebuildOutcomeTimeout`: [`REBUILD_OUTCOME_WAIT`] (600 s) in
    /// production.
    #[must_use]
    pub fn outcome_wait(&self) -> Duration {
        Duration::from_millis(self.outcome_wait_ms.load(Ordering::Relaxed))
    }

    /// **Test-only.** Shorten the outcome wait, so a test can observe the
    /// bound without waiting 600 s. Production never calls it.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn set_outcome_wait_for_test(&self, wait: Duration) {
        self.outcome_wait_ms.store(
            u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
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
    /// Callers (the Phase 7b2 watcher task, direct test drivers) invoke
    /// this for each debounced [`ChangeSet`]. Exactly one concurrent
    /// invocation per workspace runs the full pipeline; others coalesce
    /// their [`ChangeSet`] into the lane and return `Ok(())` promptly.
    ///
    /// Preconditions (not part of the §J.4 ordered sequence):
    /// - The workspace must already be registered with the manager
    ///   (i.e. [`WorkspaceManager::get_or_load`] succeeded earlier).
    ///   A caller whose [`WorkspaceManager::lookup`] returns `None`
    ///   sees [`DaemonError::WorkspaceEvicted`].
    ///
    /// # Phase A (acquire-or-park): single lane lock scope
    ///
    /// `Self::acquire_or_park`:
    ///
    /// 1. Lock [`LoadedWorkspace::rebuild_lane`].
    /// 2. Merge incoming with any prior [`PendingRebuild`] parked in the
    ///    lane (A2 §J.2 merge rules, [`PendingRebuild::coalesce_with`]),
    ///    when [`PendingRebuild::merges_with`] accepts the pair; otherwise
    ///    refuse incoming with [`DaemonError::InvalidArgument`] and leave
    ///    the parked entry as it was. A request from this method is a
    ///    watcher-shaped enqueue (no waiters, empty macro request), which
    ///    merges with any parked entry.
    /// 3. CAS [`LoadedWorkspace::rebuild_in_flight`] `false → true`.
    ///    - On CAS success: we own the runner role; keep the merged
    ///      pending as `current`, drop lane.
    ///    - On CAS failure: another runner is active; park the merged
    ///      pending in the lane, drop lane, return `Ok(())`.
    ///
    /// Holding the lane across the in-flight CAS makes the acquire
    /// race-free: every in-flight transition happens under the lane,
    /// so no two runners ever claim the role simultaneously.
    ///
    /// # Phase B (drain loop): sentinel-protected
    ///
    /// `Self::drain`, armed with `DrainLoopSentinel` for
    /// panic-safety:
    ///
    /// 1. **Cancellation gate.** `Self::take_cancellation`. If a
    ///    cancellation is pending, take and drop any parked pending
    ///    (telling its waiters `WorkspaceEvicted`), release
    ///    `rebuild_in_flight` under the lane, disarm sentinel, and return
    ///    [`DaemonError::WorkspaceEvicted`] (or, after the last
    ///    iteration, that iteration's result).
    /// 2. Call `Self::execute_one_rebuild` on `current`. Records
    ///    success, a refusal or a failure on the workspace; the result
    ///    flows into `last_result` and to the entry's waiters.
    /// 3. Re-lock lane. If a new `PendingRebuild` is parked: take it
    ///    as the next `current`, loop to step 1 (in-flight stays
    ///    true). If the lane is empty: loop to step 1 once more, then
    ///    release `rebuild_in_flight` under the lane (unless a request
    ///    parked or a cancellation arrived meanwhile), disarm sentinel,
    ///    return `last_result`.
    ///
    /// # §J.4 lock order
    ///
    /// `manager.lookup` takes `workspaces.read()` and drops before
    /// `rebuild_lane`. `rebuild_lane` is dropped before
    /// [`WorkspaceManager::reserve_rebuild`] (which re-takes
    /// `workspaces.read() → admission.lock()` internally) and before
    /// `Self::take_cancellation` (which takes `workspaces.read()`). No
    /// `rebuild_lane` ↔ `admission` nesting ever occurs.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspaceEvicted`] when the workspace is not
    ///   registered, when the cancellation gate consumes a cancellation
    ///   before an entry runs (an eviction, `daemon/cancel_rebuild`,
    ///   `daemon/reset` of the rebuilding workspace), or when an
    ///   iteration observes one (the reservation's Phase-1 check, a pass
    ///   boundary, the publish recheck).
    /// - The refusals `Self::resolve_rebuild_inputs` raises (with an
    ///   empty request: [`DaemonError::WorkspaceIncompatibleGraph`],
    ///   [`DaemonError::WorkspaceManifestUnreadable`],
    ///   [`DaemonError::RebuildMacroOptionsUnavailable`] for a recorded
    ///   expand cache that no longer exists,
    ///   [`DaemonError::RebuildWouldNarrowSelection`]), and
    ///   [`DaemonError::MemoryBudgetExceeded`] when admission cannot
    ///   satisfy a reservation after eviction. Each leaves the workspace
    ///   `Loaded` with no recorded failure.
    /// - [`DaemonError::WorkspaceBuildFailed`] when the build or the
    ///   durable persist fails (including `spawn_blocking` join
    ///   failures), and [`DaemonError::WorkspaceOversize`] when the built
    ///   graph does not fit at publish.
    ///
    /// Only the FINAL drain-loop iteration's result is surfaced to
    /// the caller. Per-iteration success/failure is recorded on the
    /// workspace (`ws.last_error`, `ws.last_good_at`,
    /// `ws.retry_count`) via `Self::execute_one_rebuild`, and delivered
    /// to the waiters of the entry each iteration consumed.
    pub async fn handle_changes(
        &self,
        key: &WorkspaceKey,
        changes: ChangeSet,
    ) -> Result<(), DaemonError> {
        // Plain handle_changes: no git-state snapshot attached. The
        // runner will consume this PendingRebuild but will NOT advance
        // `ws.last_indexed_git_state` because `git_state_at_enqueue`
        // is `None`. Used by direct callers (tests) that don't own a
        // watcher.
        self.handle_changes_inner(
            key,
            PendingRebuild {
                changes,
                enqueued_at: Instant::now(),
                git_state_at_enqueue: None,
                macro_request: MacroOptionsRequest::empty(),
                waiters: RebuildWaiters::default(),
                requester: RebuildRequester::Watcher,
            },
        )
        .await
    }

    /// One caller's rebuild with an explicit macro build options request
    /// (surface parity W4, design W4-D7, W4-D8): the `daemon/rebuild`
    /// handler passes the `cfg_flags`, `expand_cache` and
    /// `reset_macro_options` fields its call carried, and the daemon-hosted
    /// `rebuild_index` does the same for a resident workspace; an empty
    /// request reuses the manifest's record, exactly as
    /// [`Self::handle_changes`].
    ///
    /// The request is normalised first ([`normalized_macro_request`], so
    /// two spellings of one directory compare equal), then Phase A runs
    /// on this task. The call returns at once with a [`RebuildOutcome`]
    /// that resolves to THIS request's own iteration (integration of W1
    /// and W4): when it parks behind a running rebuild, the runner
    /// delivers the outcome when it drains it; when it takes the runner
    /// role, the drain loop runs on a spawned task and delivers this
    /// request's outcome as soon as its own iteration ends, so the caller
    /// never waits for the iterations parked behind it, and a caller that
    /// stops waiting never abandons the drain loop mid-pipeline. The caller
    /// bounds its wait on the outcome ([`Self::outcome_wait`]), never the
    /// runner.
    ///
    /// The outcome's errors are those of [`Self::handle_changes`], plus,
    /// from this request's own options: [`DaemonError::InvalidArgument`]
    /// for a request the request check refuses ([`validate_macro_request`];
    /// the handlers refuse it before calling this), for an empty expand
    /// cache, for one whose canonical path is not valid UTF-8 (the manifest
    /// could not record it) and for a recorded cfg flag that names no
    /// predicate; [`DaemonError::RebuildMacroOptionsUnavailable`] when the
    /// resolved expand cache directory cannot be used (in each case
    /// nothing is written and the workspace keeps the state it was in);
    /// and [`DaemonError::InvalidArgument`] when a rebuild whose
    /// macro options this request does not share is already queued
    /// ([`PendingRebuild::merges_with`]: nothing is queued for this
    /// request; the parked one keeps its options).
    pub async fn handle_changes_with_macro_options(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        changes: ChangeSet,
        macro_request: MacroOptionsRequest,
    ) -> RebuildOutcome {
        self.handle_changes_for(key, changes, macro_request, RebuildRequester::DaemonRebuild)
            .await
    }

    /// [`Self::handle_changes_with_macro_options`] for the daemon-hosted
    /// `rebuild_index` over a resident workspace: the same rebuild, whose
    /// durable persist records `daemon:rebuild_index` as the index's
    /// provenance, as the `rebuild_index` load route records it.
    ///
    /// # Errors
    ///
    /// As [`Self::handle_changes_with_macro_options`].
    pub async fn handle_changes_for_rebuild_index(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        changes: ChangeSet,
        macro_request: MacroOptionsRequest,
    ) -> RebuildOutcome {
        self.handle_changes_for(key, changes, macro_request, RebuildRequester::RebuildIndex)
            .await
    }

    /// One caller's rebuild request from `requester`, answered with the
    /// outcome of the iteration that consumes it.
    async fn handle_changes_for(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        changes: ChangeSet,
        macro_request: MacroOptionsRequest,
        requester: RebuildRequester,
    ) -> RebuildOutcome {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let incoming = PendingRebuild {
            changes,
            enqueued_at: Instant::now(),
            git_state_at_enqueue: None,
            macro_request: normalized_macro_request(&key.source_root, macro_request),
            waiters: RebuildWaiters::one(sender),
            requester,
        };
        let call = match self.acquire_or_park(key, incoming).await {
            Ok(Some((ws, current))) => {
                // This call holds the runner role. The drain loop runs on
                // its own task: this request's outcome is delivered when
                // its own iteration ends, and the loop finishes the
                // requests parked behind it whether or not anyone still
                // waits.
                let dispatcher = Arc::clone(self);
                tokio::spawn(async move {
                    let _ = dispatcher.drain(&ws, current).await;
                });
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(refused) => Err(refused),
        };
        RebuildOutcome { receiver, call }
    }

    /// Like [`Self::handle_changes`] but attaches a
    /// [`LastIndexedGitState`] snapshot to the enqueued
    /// [`PendingRebuild`]. When the runner (either us directly or a
    /// concurrent runner we parked against) successfully publishes
    /// the graph derived from this `PendingRebuild`, it commits the
    /// snapshot into [`LoadedWorkspace::last_indexed_git_state`]
    /// as the new classifier baseline.
    ///
    /// Task 7 Phase 7b2: used exclusively by the per-workspace
    /// watcher bridge spawned by [`Self::ensure_watching`]. Plain
    /// [`Self::handle_changes`] remains the API for callers without a
    /// watcher-owned git-state snapshot.
    ///
    /// `watcher_stop` is the dispatching watcher's own stop signal. Phase A
    /// reads it under the rebuild lane and refuses the enqueue with
    /// [`DaemonError::WorkspaceEvicted`] once it is set, so a change set the
    /// watcher received before a tombstone writer stopped it (an eviction,
    /// `daemon/unload`, a `daemon/reset` that took the lane) never starts a
    /// rebuild after that writer: a reset workspace is not rebuilt back to
    /// `Loaded` by its own stopped watcher.
    ///
    /// # §J.4 lock order, error taxonomy, gate, sentinel
    ///
    /// Identical to [`Self::handle_changes`]: Phase A
    /// (acquire-or-park under lane + CAS) and Phase B (drain loop)
    /// behave identically. The differences are the
    /// `git_state_at_enqueue: Some(git_state)` construction and the
    /// stop-signal refusal above.
    ///
    /// # Errors
    ///
    /// Same as [`Self::handle_changes`], plus
    /// [`DaemonError::WorkspaceEvicted`] when `watcher_stop` is set.
    pub async fn handle_changes_with_git_state(
        &self,
        key: &WorkspaceKey,
        changes: ChangeSet,
        git_state: LastIndexedGitState,
        watcher_stop: &AtomicBool,
    ) -> Result<(), DaemonError> {
        let incoming = PendingRebuild {
            changes,
            enqueued_at: Instant::now(),
            git_state_at_enqueue: Some(git_state),
            macro_request: MacroOptionsRequest::empty(),
            waiters: RebuildWaiters::default(),
            requester: RebuildRequester::Watcher,
        };
        let Some((ws, current)) = self
            .acquire_or_park_unless_stopped(key, incoming, Some(watcher_stop))
            .await?
        else {
            return Ok(());
        };
        self.drain(&ws, current).await.map(|_| ())
    }

    /// Phase A (acquire-or-park) + Phase B (drain loop, on this task) for
    /// [`Self::handle_changes`], which constructs the incoming
    /// [`PendingRebuild`] and threads it here.
    async fn handle_changes_inner(
        &self,
        key: &WorkspaceKey,
        incoming: PendingRebuild,
    ) -> Result<(), DaemonError> {
        let Some((ws, current)) = self.acquire_or_park(key, incoming).await? else {
            return Ok(());
        };
        self.drain(&ws, current).await.map(|_| ())
    }

    /// Phase A: take the runner role, or park in the lane behind the
    /// runner that holds it (single lane lock scope).
    ///
    /// Holding `rebuild_lane` across the `rebuild_in_flight` CAS is the
    /// load-bearing invariant: every in-flight transition happens under
    /// the lane, so no two runners ever claim the role simultaneously.
    ///
    /// Returns the workspace and the entry to run when this call took the
    /// runner role, `None` when it parked. From here on the dispatcher
    /// names the workspace by the key it is registered under
    /// ([`LoadedWorkspace::key`]), never by `key`: an anonymous key
    /// resolves by source root ([`WorkspaceManager::lookup`]), so a caller's
    /// key may differ from the registered one in its root mode or
    /// fingerprint (a pinned workspace is registered under
    /// `ProjectRootMode::WorkspaceFolder`, the CLI and the MCP host send
    /// `GitRoot`), and every exact-key step after the lookup would then miss
    /// the workspace it is rebuilding.
    ///
    /// # Errors
    ///
    /// [`DaemonError::WorkspaceEvicted`] when the workspace is not
    /// registered; [`DaemonError::InvalidArgument`] when a parked entry
    /// does not merge with `incoming` ([`PendingRebuild::merges_with`]).
    async fn acquire_or_park(
        &self,
        key: &WorkspaceKey,
        incoming: PendingRebuild,
    ) -> Result<Option<(Arc<LoadedWorkspace>, PendingRebuild)>, DaemonError> {
        self.acquire_or_park_unless_stopped(key, incoming, None)
            .await
    }

    /// [`Self::acquire_or_park`] for a watcher's enqueue: `watcher_stop`,
    /// when given, is read under the lane and a set signal refuses the
    /// enqueue ([`DaemonError::WorkspaceEvicted`]) before anything parks.
    async fn acquire_or_park_unless_stopped(
        &self,
        key: &WorkspaceKey,
        incoming: PendingRebuild,
        watcher_stop: Option<&AtomicBool>,
    ) -> Result<Option<(Arc<LoadedWorkspace>, PendingRebuild)>, DaemonError> {
        // --- Precondition: lookup Arc<LoadedWorkspace>. ---
        let ws: Arc<LoadedWorkspace> =
            self.manager
                .lookup(key)
                .ok_or_else(|| DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                })?;

        let mut lane_guard = ws.rebuild_lane.lock().await;

        // A stopped watcher's change set starts nothing. The tombstone
        // writers set the signal, and `daemon/reset` sets it while it holds
        // this lane, so an enqueue that takes the lane after a reset sees it.
        if watcher_stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(DaemonError::WorkspaceEvicted {
                root: key.source_root.clone(),
            });
        }

        // An explicit caller's serving check, made under the lane so it is
        // one step with the enqueue (audit S2). `daemon/rebuild` and the
        // daemon-hosted `rebuild_index` check the state before they call
        // here; a `daemon/reset` (which stores `Unloaded` holding this
        // lane) or an eviction landing between that check and this lane
        // would otherwise have the reset workspace rebuilt back to
        // `Loaded`. A reset that comes after this lane sees the runner
        // role or the parked entry and cancels it (decision D-i7-3).
        if incoming.requester != RebuildRequester::Watcher && !ws.load_state().is_serving() {
            let root = key.source_root.clone();
            return Err(if incoming.requester == RebuildRequester::DaemonRebuild {
                DaemonError::WorkspaceNotLoaded { root }
            } else {
                DaemonError::WorkspaceEvicted { root }
            });
        }

        // Merge incoming into any prior parked pending, when the two may
        // share one iteration (decision D-i7-1): requests that mean the
        // same, or a watcher-driven enqueue on either side. Otherwise the
        // later request is refused and the parked one keeps its options,
        // so no caller is told the outcome of a rebuild that ran with
        // options it did not ask for (integration of W1 and W4).
        let coalesced = match lane_guard.take() {
            Some(prior) if !prior.merges_with(&incoming) => {
                *lane_guard = Some(prior);
                return Err(DaemonError::InvalidArgument {
                    reason: format!(
                        "a rebuild of {} with different macro build options is already \
                         queued; it runs first and this request was not queued. Retry \
                         once it completes",
                        key.source_root.display()
                    ),
                });
            }
            Some(prior) => prior.coalesce_with(incoming),
            None => incoming,
        };

        // Try to acquire the runner role.
        let acquired_runner =
            ws.rebuild_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
        if acquired_runner.is_ok() {
            drop(lane_guard);
            Ok(Some((ws, coalesced)))
        } else {
            // Another runner is active: park the merged pending in the
            // lane; that runner will drain it at its next drain-loop
            // iteration.
            *lane_guard = Some(coalesced);
            Ok(None)
        }
    }

    /// Phase B of [`Self::handle_changes`]: the drain loop the runner
    /// role runs, from the entry it took in Phase A until the lane is
    /// empty or a cancellation is consumed.
    ///
    /// A [`DrainLoopSentinel`] releases `rebuild_in_flight` if the loop
    /// unwinds abnormally (plugin panics inside `spawn_blocking` are
    /// caught by `execute_rebuild` and mapped to `Err`, so the realistic
    /// unwind triggers are a runtime-level failure or a panic in the
    /// dispatcher itself).
    ///
    /// The cancellation gate runs before every iteration and once more
    /// before the runner role is released, so a cancellation that lands
    /// during an iteration (a `daemon/cancel_rebuild`, a `daemon/reset` of
    /// the rebuilding workspace, an eviction) is consumed by the runner it
    /// was aimed at and never outlives it.
    async fn drain(
        &self,
        ws: &Arc<LoadedWorkspace>,
        first: PendingRebuild,
    ) -> Result<RebuildReport, DaemonError> {
        let key = &ws.key;
        let mut sentinel = DrainLoopSentinel {
            ws: Arc::clone(ws),
            armed: true,
        };

        // `None` once the entry has run and the lane was empty when the
        // iteration ended: the loop then runs the gate once more and
        // releases the runner role under the lane.
        let mut current: Option<PendingRebuild> = Some(first);
        // Assigned by every iteration and read only after one has run:
        // before the first iteration `current` holds the first entry, so
        // the gate answers `WorkspaceEvicted` and the release path is not
        // reached. The initial value is never returned.
        let mut last_result: Result<RebuildReport, DaemonError> = Err(DaemonError::Internal(
            anyhow::anyhow!("the rebuild drain loop ended before any iteration ran"),
        ));
        loop {
            // --- Cancellation gate ---
            //
            // `rebuild_cancelled` is set by eviction (under
            // `workspaces.write()`, together with the `Evicted` store), by
            // `daemon/cancel_rebuild` (under the lane, while a rebuild is in
            // flight) and by `daemon/reset` of a rebuilding workspace.
            // `take_cancellation` consumes it, except after a completed
            // eviction: LRU eviction keeps the tombstone in the map and the
            // next load reuses this `LoadedWorkspace`, so the flag is left
            // for that load to consume, and a load racing the eviction
            // still observes it at its publish recheck.
            if self.take_cancellation(ws) {
                let mut lane_guard = ws.rebuild_lane.lock().await;
                // Abandon any parked pending. Its waiters, and the waiters
                // of the entry this iteration was about to run, are told so
                // rather than left to time out.
                let dropped: Option<PendingRebuild> = lane_guard.take();
                let evicted: Result<RebuildReport, DaemonError> =
                    Err(DaemonError::WorkspaceEvicted {
                        root: key.source_root.clone(),
                    });
                if let Some(current) = &current {
                    current.waiters.deliver(&evicted);
                }
                if let Some(dropped) = dropped {
                    dropped.waiters.deliver(&evicted);
                }
                ws.rebuild_in_flight.store(false, Ordering::Release);
                sentinel.armed = false;
                // Cluster-G iter-2 BLOCKER 3: distinguish an eviction (it
                // stored `Evicted` under `workspaces.write()` before it
                // set the flag, so the transition below writes nothing
                // for it) from a `daemon/cancel_rebuild` or `daemon/reset`
                // that landed during an iteration. A pass boundary or the
                // reservation turned that cancellation into `Unloaded`
                // already (`record_and_transition_on_err`); the publish
                // recheck returns with the state still `Rebuilding`, and
                // this transition is what leaves it. The runner, not a
                // later `daemon/reset`, is responsible for that state.
                //
                // Surface parity W1 round 7 (design D37): the read and
                // the store were two operations, so an eviction that
                // completed between them replaced the `Evicted`
                // tombstone with `Unloaded` over the placeholder, which `is_serving` refuses
                // and `evict_lru` skips, but which the partial-eviction
                // reporting depends on. The compare-exchange carries
                // the guard the `load_state()` read used to carry, and
                // writes nothing when it loses.
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Unloaded)
                {
                    tracing::debug!(
                        workspace = %key.source_root.display(),
                        observed = %observed,
                        "top-of-loop cancellation left the state to its writer"
                    );
                }
                // An entry about to run was cancelled. With none (the gate
                // after the last iteration), that iteration ran to its own
                // end and its result stands: the gate only consumed the
                // flag and told whatever parked meanwhile.
                return match current {
                    Some(_) => Err(DaemonError::WorkspaceEvicted {
                        root: key.source_root.clone(),
                    }),
                    None => last_result,
                };
            }

            // --- Release the runner role, or take what parked meanwhile ---
            //
            // Releasing `rebuild_in_flight` under the lane is load-bearing:
            // a caller about to park in the lane observes the release
            // atomically with the empty-lane snapshot, so parked pending
            // cannot be stranded. A cancellation set under the lane
            // (`cancel_rebuild`) after the gate above is seen here and sent
            // back to the gate, so it cannot outlive the runner either.
            let Some(entry) = current.take() else {
                self.pre_release_check().await;
                let mut lane_guard = ws.rebuild_lane.lock().await;
                if let Some(next) = lane_guard.take() {
                    current = Some(next);
                    continue;
                }
                if ws.rebuild_cancelled.load(Ordering::Acquire) {
                    continue;
                }
                ws.rebuild_in_flight.store(false, Ordering::Release);
                sentinel.armed = false;
                return last_result;
            };

            // --- Execute iteration ---
            //
            // `execute_one_rebuild` records success/failure on the
            // workspace before returning, so workspace-level
            // observability is threaded through every iteration even
            // when the drain loop continues past an error.
            let PendingRebuild {
                changes,
                git_state_at_enqueue,
                macro_request,
                waiters,
                requester,
                ..
            } = entry;
            last_result = self
                .execute_one_rebuild(ws, changes, git_state_at_enqueue, macro_request, requester)
                .await;
            // Each caller merged into this entry receives this iteration's
            // result, not the drain loop's last one.
            waiters.deliver(&last_result);

            // --- Take the next parked entry, if any ---
            current = ws.rebuild_lane.lock().await.take();
        }
    }

    /// Consume a pending cancellation of this workspace's rebuilds, and
    /// say whether there was one.
    ///
    /// Read under `workspaces.read()` so it is atomic with respect to the
    /// tombstone writers. After a completed eviction (`Evicted`) the flag is
    /// reported but left set: the tombstone stays in the map, the next load
    /// reuses this `LoadedWorkspace` and consumes the flag in its gate, and
    /// a load that raced the eviction must still see it at its publish
    /// recheck. Otherwise (a `daemon/cancel_rebuild`, a `daemon/reset` of
    /// the rebuilding workspace) the flag is cleared, so it cannot fail the
    /// next load or abort the next rebuild.
    ///
    /// Lock order: called with no lane held; takes `workspaces` alone.
    fn take_cancellation(&self, ws: &LoadedWorkspace) -> bool {
        let _workspaces = self.manager.workspaces_read();
        if ws.load_state() == WorkspaceState::Evicted {
            return ws.rebuild_cancelled.load(Ordering::Acquire);
        }
        ws.rebuild_cancelled.swap(false, Ordering::AcqRel)
    }

    /// Cancel the rebuild in flight for `ws`, if one is: set
    /// [`LoadedWorkspace::rebuild_cancelled`] and say whether a rebuild was
    /// in flight (`daemon/cancel_rebuild`).
    ///
    /// The flag is set under the rebuild lane, the lock every runner-role
    /// transition takes, so a cancellation either finds the runner role
    /// released (and sets nothing, which would otherwise abort the next
    /// rebuild or fail the next load) or is seen by the runner at its
    /// cancellation gate before the role is released. The file watcher keeps
    /// watching: it stops only on its own signal
    /// ([`LoadedWorkspace::stop_watcher`]).
    pub async fn cancel_rebuild(&self, ws: &LoadedWorkspace) -> bool {
        let _lane = ws.rebuild_lane.lock().await;
        let in_flight = ws.rebuild_in_flight.load(Ordering::Acquire);
        if in_flight {
            ws.rebuild_cancelled.store(true, Ordering::Release);
        }
        in_flight
    }

    /// Run a single pipeline iteration: decide mode, resolve the inputs
    /// the build would refuse, compute the working-set estimate, reserve
    /// admission headroom, execute the rebuild on a blocking thread,
    /// publish atomically. Records `record_success` on a successful
    /// publish; what an error does to the workspace depends on where it
    /// arose (below).
    ///
    /// Called by [`Self::handle_changes`]'s Phase B drain loop for
    /// each coalesced [`PendingRebuild`]. The drain loop may invoke
    /// this multiple times if new pending arrives between iterations;
    /// each invocation is independent from the caller's perspective.
    ///
    /// # Error paths
    ///
    /// - The entry refusal (the workspace is `Evicted` or `Loading`):
    ///   nothing is written, not even the state.
    /// - Refusals, raised before anything is written: the input
    ///   resolution ([`Self::resolve_rebuild_inputs`]: the roster, the
    ///   macro build options, the narrowing guard), a reservation the
    ///   budget cannot admit ([`DaemonError::MemoryBudgetExceeded`]), and
    ///   the narrowing guard ahead of the durable persist. The workspace
    ///   returns to the state the iteration entered from (`Failed` stays
    ///   `Failed`), with no `record_failure` and no backoff
    ///   ([`Self::record_refusal`]).
    /// - Cancellation ([`DaemonError::WorkspaceEvicted`] from the
    ///   reservation's membership check, a pass boundary or the publish
    ///   recheck): an eviction owns the state; a `daemon/cancel_rebuild` or
    ///   a `daemon/reset` of the rebuilding workspace leaves it `Unloaded`
    ///   ([`Self::record_and_transition_on_err`] at the first two; the
    ///   drain loop's cancellation gate after the publish recheck, which
    ///   returns with the state still `Rebuilding`).
    /// - The publish recheck finding the workspace no longer registered
    ///   under its key ([`WorkspaceManager::registers`]; an unload removed
    ///   it): [`DaemonError::WorkspaceEvicted`], the state moved from
    ///   `Rebuilding` to `Unloaded` and the cancellation flag cleared, so no
    ///   exit leaves `Rebuilding` with no runner. The workspace is looked
    ///   up, reserved, rechecked and watched by the key it is registered
    ///   under ([`LoadedWorkspace::key`]), never by a caller's key.
    /// - Failures after work began (a build error, a `spawn_blocking`
    ///   join panic mapped to [`DaemonError::WorkspaceBuildFailed`], a
    ///   persist failure, a post-build [`DaemonError::WorkspaceOversize`]):
    ///   `record_failure` and `Failed`.
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
    ///
    /// Surface parity W1 round 7 (design D36): that write,
    /// `record_success` and the `Loaded` transition all run inside the
    /// publish block, so the `WorkspaceManager::workspaces_read` guard
    /// that made the publish atomic with respect to eviction is still
    /// held across them. `dispatched_count`, the capture push,
    /// `post_publish_check` and the hook dispatch stay outside it.
    async fn execute_one_rebuild(
        &self,
        ws: &Arc<LoadedWorkspace>,
        changes: ChangeSet,
        git_state_at_enqueue: Option<LastIndexedGitState>,
        macro_request: MacroOptionsRequest,
        requester: RebuildRequester,
    ) -> Result<RebuildReport, DaemonError> {
        let key = &ws.key;
        let prior_graph: Arc<CodeGraph> = ws.graph();
        let mode = decide_mode(&self.config, &changes, &prior_graph);
        self.store_last_mode(mode);

        // Task 7 Phase 7c: transition to Rebuilding at iteration entry.
        // Queries keep serving the prior ArcSwap snapshot (A2 §G.5).
        // Placement BEFORE gate_check is intentional: the `Rebuilding`
        // lifecycle covers the synthetic test stall too, so
        // `classify_for_serve_returns_fresh_for_rebuilding_workspace`
        // observes a real state.
        //
        // Surface parity W1 round 7 (design D37). The entry has no
        // single predecessor state: an iteration may legitimately begin
        // on a workspace that is `Loaded`, `Failed`, `Unloaded` or
        // already `Rebuilding`. It must not begin on `Evicted`, which
        // is a completed tombstone, nor on `Loading`, which the load
        // gate owns. The unconditional store this replaces made the
        // slot `(Rebuilding, placeholder)` whenever an eviction
        // completed just before it, which `classify_for_serve` answers
        // with an internal error until `reserve_rebuild`'s membership
        // and cancellation check ends the iteration. Nothing is written
        // on a refusal and `record_and_transition_on_err` is not called
        // on this path, for the reason the publish-path rechecks
        // already give in their own comment: eviction owns the state
        // and the failure record.
        // The state the iteration entered from is what a refusal restores
        // (`Self::record_refusal`): a refusal wrote nothing, so a `Failed`
        // workspace must not come out of it `Loaded`.
        let entered = match ws.transition_state_from_any(
            &[
                WorkspaceState::Loaded,
                WorkspaceState::Failed,
                WorkspaceState::Unloaded,
                WorkspaceState::Rebuilding,
            ],
            WorkspaceState::Rebuilding,
        ) {
            // `Unloaded` is an entry state only for a workspace that still
            // holds its graph (a cancelled rebuild leaves it so, and its
            // watcher rebuilds it). `Unloaded` over the placeholder is a
            // reset workspace, which only a load makes resident again: a
            // request that reaches it (one enqueued before the reset
            // through a path with no serving check) must not rebuild it
            // back to `Loaded` (audit S2, D2F). Nothing else writes the
            // published slot while the state is `Rebuilding`, so the read
            // after the transition is stable; the state goes back to
            // `Unloaded` and nothing is written.
            Ok(WorkspaceState::Unloaded) if ws.roster().is_none() => {
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Unloaded)
                {
                    tracing::debug!(
                        workspace = %key.source_root.display(),
                        observed = %observed,
                        "a refused entry from a reset workspace left the state to its writer"
                    );
                }
                return Err(DaemonError::WorkspaceNotLoaded {
                    root: key.source_root.clone(),
                });
            }
            Ok(entered) => entered,
            Err(observed) => {
                tracing::debug!(
                    workspace = %key.source_root.display(),
                    observed = %observed,
                    "rebuild iteration refused to enter Rebuilding: the workspace is not rebuildable"
                );
                if observed == WorkspaceState::Evicted {
                    return Err(DaemonError::WorkspaceEvicted {
                        root: key.source_root.clone(),
                    });
                }
                return Err(DaemonError::WorkspaceBuildFailed {
                    root: key.source_root.clone(),
                    reason: format!("workspace is {observed}, not rebuildable"),
                });
            }
        };

        // Task 7 Phase 7b2: record the iteration input for tests
        // BEFORE the optional gate stall, so the test sees the input
        // even when the gate blocks. No-op when `test_capture` is not
        // installed (production).
        if let Some(cap) = self.test_capture.get() {
            cap.iterations.lock().push(CapturedIteration {
                changeset: changes.clone(),
                mode,
                git_state_at_enqueue: git_state_at_enqueue.clone(),
                macro_request: macro_request.clone(),
                started_at: Instant::now(),
            });
        }

        // Task 7 Phase 7b2: optional test-only gate. Stalls the
        // iteration until the test driver releases it. Production
        // callers never install a gate — this is a single atomic
        // load + short-circuit.
        self.gate_check().await;

        // Test-only: unwind the runner here, after the gate, with the
        // workspace `Rebuilding` and the runner role held, so a test can
        // park a request behind it and observe what an unwind leaves.
        // Production with no capture installed sees one `OnceLock::get`.
        if let Some(cap) = self.test_capture.get() {
            assert!(
                !cap.panic_in_next_iteration.swap(false, Ordering::AcqRel),
                "test hook: the rebuild runner unwinds mid-iteration"
            );
        }

        // Refuse before reserving. The reservation below can evict sibling
        // workspaces (its LRU phase runs before it commits), so every input
        // the build would refuse is resolved here first: the roster the
        // manifest records (an unreadable manifest, an id this binary did
        // not compile), the macro build options the request asks for over
        // the manifest's record (an empty, missing or unrecordable expand
        // cache), and the narrowing guard. A refused request evicts nothing
        // and returns the workspace to the state it entered from. The
        // resolved roster and options are what the pipeline builds and
        // persists with; nothing resolves them again.
        let inputs = match self.resolve_rebuild_inputs(
            &key.source_root,
            &macro_request,
            ws.roster(),
            requester,
        ) {
            Ok(inputs) => inputs,
            Err(refusal) => {
                Self::record_refusal(ws, &refusal, entered);
                return Err(refusal);
            }
        };

        let estimate = compute_working_set_estimate(&prior_graph, &changes, mode);

        // Admission reservation. `reserve_rebuild` itself performs the
        // Phase-1 membership + cancellation check (Task 7 Phase 7b1)
        // which can surface `WorkspaceEvicted`, handled by the
        // cancellation arm of `record_and_transition_on_err`. A budget
        // that cannot admit the working set (`MemoryBudgetExceeded`) is a
        // refusal: nothing has been written, so the workspace returns to
        // the state it entered from with no recorded failure.
        let reservation = match self.manager.reserve_rebuild(key, estimate) {
            Ok(r) => r,
            Err(cancelled @ DaemonError::WorkspaceEvicted { .. }) => {
                Self::record_and_transition_on_err(ws, &cancelled);
                return Err(cancelled);
            }
            Err(refusal) => {
                Self::record_refusal(ws, &refusal, entered);
                return Err(refusal);
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
            .execute_rebuild(ws, &prior_graph, mode, changes, inputs, requester)
            .await
        {
            Ok(g) => g,
            Err(PipelineError::Refused(refusal)) => {
                // The narrowing guard refused before the durable persist
                // wrote anything (a manifest rewritten during the build).
                drop(reservation);
                Self::record_refusal(ws, &refusal, entered);
                return Err(refusal);
            }
            Err(PipelineError::Failed(e)) => {
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
                Self::record_and_transition_on_err(ws, &e);
                return Err(e);
            }
        };
        let DurableRebuildOutput {
            graph: new_graph,
            build_result,
            roster: new_roster,
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
        // the re-check and publish of `WorkspaceManager::get_or_load_published`
        // (Codex Task 6 Phase 6b iter-2 MAJOR). Lock order §J.4:
        // `workspaces -> admission`; `publish_and_retain` takes
        // `admission` internally, which nests correctly.
        let published = {
            let workspaces_guard = self.manager.workspaces_read();

            if ws.rebuild_cancelled.load(Ordering::Acquire) {
                // Task 7 Phase 7c feat iter-1 (Codex MAJOR 2):
                // counter — §5e recheck surface.
                if let Some(cap) = self.test_capture.get() {
                    cap.publish_path_evictions.fetch_add(1, Ordering::AcqRel);
                }
                // A cancellation the pipeline did not observe (it landed
                // after the last pass boundary, during the durable
                // persist). No record_failure and no transition here: an
                // eviction owns the state and the failure record; for a
                // `daemon/cancel_rebuild` or a `daemon/reset`, the drain
                // loop's cancellation gate, which runs next, consumes the
                // flag and moves `Rebuilding` to `Unloaded`. (Before the
                // gate ran after every iteration, this return left the
                // workspace `Rebuilding` with no runner, which
                // `daemon/reset` answered with `ResetCancellationDispatched`
                // on every retry.)
                drop(workspaces_guard);
                drop(reservation);
                return Err(DaemonError::WorkspaceEvicted {
                    root: key.source_root.clone(),
                });
            }
            if !WorkspaceManager::registers(&workspaces_guard, ws) {
                // The workspace this iteration rebuilt is no longer the one
                // registered under its key: `daemon/unload` removed it, and a
                // later load may have registered another in its place. Nothing
                // is published into it. Every way out of the map passes
                // through the tombstone writer, which sets the cancellation
                // flag, so the check above normally answers first; this arm
                // is the one that does not rely on that. It leaves no
                // `Rebuilding` behind and no flag for a runner to consume:
                // the state moves to `Unloaded`, as a consumed cancellation
                // leaves it, and the flag is cleared.
                drop(workspaces_guard);
                drop(reservation);
                if let Some(cap) = self.test_capture.get() {
                    cap.publish_path_evictions.fetch_add(1, Ordering::AcqRel);
                }
                ws.rebuild_cancelled.store(false, Ordering::Release);
                if let Err(observed) =
                    ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Unloaded)
                {
                    tracing::debug!(
                        workspace = %key.source_root.display(),
                        observed = %observed,
                        "an unregistered workspace's rebuild left the state to its writer"
                    );
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
            // The pair this publish swapped in (design D20); the hook below
            // receives its graph half, the only half the hook contract
            // carries.
            let (_token, published) = match self.manager.publish_and_retain(
                reservation,
                ws,
                BuiltGraph::new(new_graph, new_roster),
            ) {
                Ok((token, published)) => (token, published),
                Err(e) => {
                    Self::record_and_transition_on_err(ws, &e);
                    return Err(e);
                }
            };

            // Surface parity W1 round 7 (design D36). The three
            // operations that report the generation this iteration
            // published run HERE, under the guard that justifies
            // them, in the order they had when they ran after it.
            // `WorkspaceManager::evict_to_tombstone_locked` is reached
            // only under `workspaces.write()`, so no eviction can
            // complete between the publish and this bookkeeping, and
            // the state stored below cannot describe a generation that
            // is gone. Ahead of round 7 these ran after the guard
            // dropped and the `Loaded` store compared nothing, so an
            // eviction that completed in that window was overwritten:
            // the slot became `(Loaded, placeholder)` and every
            // read-only surface that classifies through
            // `WorkspaceManager::classify_for_serve` received an
            // internal error instead of the `WorkspaceEvicted` that
            // makes the shared acquirer reload from the snapshot.
            // `WorkspaceManager::get_or_load_published` and
            // `WorkspaceManager::reload_from_disk_read_only` already
            // do exactly these three operations under their own
            // publish guard; this is that pattern, not a new one.
            //
            // Lock order (design section 2.1.7): `workspaces ->
            // last_indexed_git_state` is new and has no inverse, and
            // `workspaces -> last_good_at` / `workspaces ->
            // last_error`, which `record_success` takes, are the order
            // the two loaders and `classify_for_serve` already take.
            // Every critical section on the far side is one assignment
            // or a clone, and no `.await` runs under the guard:
            // `post_publish_check().await` and the hook dispatch stay
            // outside the block, where the seam's sentence (design
            // D31) and the hook's re-entrancy note require them.
            //
            // Task 7 Phase 7b2: advance the classifier baseline when
            // the consumed PendingRebuild carried a watcher-captured
            // snapshot. `None` entries (direct non-watcher callers)
            // leave the baseline untouched.
            if let Some(git_state) = git_state_at_enqueue {
                *ws.last_indexed_git_state.write() = Some(git_state);
            }

            // Success bookkeeping.
            ws.record_success(SystemTime::now());
            // Task 7 Phase 7c: transition Rebuilding -> Loaded, as a
            // compare-exchange from the state this iteration installed
            // at its entry (design D37). Under the guard above the
            // exchange cannot fail, so D36 and D37 are deliberately
            // redundant at this one site: the guard closes the window,
            // and the compare-exchange makes a lost guard harmless.
            // Battery rows S29 and S32 declare each half equivalent
            // with the other named as the reason; C64 is the row that
            // reverts both.
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Loaded)
            {
                tracing::debug!(
                    workspace = %key.source_root.display(),
                    observed = %observed,
                    "rebuild publish bookkeeping observed a state this iteration did not install"
                );
            }
            published
            // workspaces_guard drops at end of this block; eviction
            // can proceed immediately afterward.
        };

        self.dispatched_count.fetch_add(1, Ordering::Relaxed);

        // Surface parity W1 round 5 (design D27): record the generation
        // this iteration published and give a test the one point at which
        // a second publication can run between the publish and the hook
        // dispatch below. The argument the hook receives is
        // `published.graph`, the graph half of the pair `publish_and_retain`
        // swapped in, never a re-read of the slot, which that second
        // publication could have advanced (T47, battery row K38).
        // Production with no capture installed sees one `OnceLock::get`.
        if let Some(cap) = self.test_capture.get() {
            cap.published_generations
                .lock()
                .push(Arc::clone(&published));
        }
        self.post_publish_check().await;

        // Dispatch the post-publish hook now that `workspaces_guard` has
        // dropped (end of the publish block above), mirroring the loader
        // in `get_or_load`. Without this, the rebuild path published a new
        // graph but never fired `QueryDbHook`, so the derived-cache save
        // ran only on load: after every `sqry daemon rebuild` or
        // watcher-driven rebuild the snapshot SHA changed while `derived.sqry`
        // stayed at the old SHA, was discarded as stale on the next query,
        // and was never rewritten until the next load (verivus-oss/sqry#358).
        self.manager
            .dispatch_publish_hook(&key.source_root, Arc::clone(&published.graph));

        Ok(RebuildReport { published, mode })
    }

    /// Task 7 Phase 7c helper: update workspace state + bookkeeping on
    /// a rebuild error raised after work began (a build, a durable
    /// persist, a publish) or by a cancellation. A refusal, raised before
    /// anything was written, is never handed here: it is recorded by
    /// [`Self::record_refusal`], which decides by where the error arose.
    ///
    /// - `WorkspaceEvicted` (a cancellation observed at the reservation or
    ///   at a pass boundary):
    ///   - **State already `Evicted`**: no-op. Eviction wrote `Evicted`
    ///     under `workspaces.write()` before setting `rebuild_cancelled`;
    ///     clobbering it would destroy that contract.
    ///   - **State still `Rebuilding`**: cluster-G iter-3 fix. No eviction
    ///     happened: a `daemon/cancel_rebuild`, a `daemon/reset` of a
    ///     workspace whose runner held the role (which only set
    ///     `rebuild_cancelled` and answered `ResetCancellationDispatched`),
    ///     or daemon shutdown cancelled the iteration. It moves to
    ///     `Unloaded`, the destination `WorkspaceManager::reset` writes; the
    ///     map entry and the `pinned` bit are preserved, the drain loop's
    ///     cancellation gate then consumes the flag, the caller's retried
    ///     `daemon/reset` resets it, and `daemon/load` brings the workspace
    ///     back.
    /// - Any other `DaemonError` (a build error, a persist failure, a
    ///   post-build oversize): `record_failure` and `Failed`, the entry
    ///   point to A2 §G.7's stale-serve flow for that workspace. A refusal
    ///   variant reaching here was raised by work that wrote something, so
    ///   it is a failure too.
    ///
    /// Surface parity W1 round 7 (design D37): every transition in this
    /// function is a compare-exchange from `Rebuilding`, the state the
    /// iteration installed at its entry, so a state a writer that owns
    /// it has since installed is left exactly as that writer left it.
    /// `record_failure` stays unconditional on the `Failed` path.
    fn record_and_transition_on_err(ws: &LoadedWorkspace, err: &DaemonError) {
        if matches!(err, DaemonError::WorkspaceEvicted { .. }) {
            // Cluster-G iter-3 BLOCKER 3 fix: differentiate
            // eviction-path from reset-path cancellations by reading
            // the state the eviction path would have written. See
            // doc-comment above for the full rationale.
            if let Err(observed) =
                ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Unloaded)
            {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "an in-iteration cancellation left the state to its writer"
                );
            }
            return;
        }
        ws.record_failure(clone_err(err));
        // Surface parity W1 round 7 (design D37). This store had no
        // observation at all in front of it, and `Failed` is in
        // `WorkspaceState::is_serving`, so an eviction that completed
        // before it turned the tombstone into a stale-servable slot
        // carrying the placeholder, which `classify_for_serve` answers
        // with an internal error for any workspace whose `last_good_at`
        // is set (eviction does not clear it). `record_failure` above
        // stays unconditional: `last_error` is a diagnostic surface
        // that `classify_for_serve` reads only in a state the
        // compare-exchange refuses to create. T57 is the oracle.
        if let Err(observed) =
            ws.transition_state(WorkspaceState::Rebuilding, WorkspaceState::Failed)
        {
            tracing::debug!(
                workspace = %ws.key.source_root.display(),
                observed = %observed,
                "a failed rebuild left the state to its writer"
            );
        }
    }

    /// Record an iteration refused before anything was written: by
    /// [`Self::resolve_rebuild_inputs`] before the reservation, by a
    /// reservation the budget cannot admit
    /// ([`DaemonError::MemoryBudgetExceeded`]), or by the narrowing guard
    /// before the durable persist.
    ///
    /// The refusal is decided by where the error arose, not by its variant:
    /// whatever the variant, the prior graph is intact and still the one
    /// the manifest describes, so the workspace returns to the state the
    /// iteration entered from (`entered`, from
    /// [`LoadedWorkspace::transition_state_from_any`]), with no recorded
    /// failure and no backoff, on the `daemon/rebuild` path and on the
    /// watcher path alike. A `Failed` workspace stays `Failed`, its
    /// previous failure still recorded and its stale-serve clock still
    /// running; a `Loaded` one stays `Loaded`; an `Unloaded` one (a watcher
    /// rebuild after a cancellation) stays `Unloaded`. An iteration that
    /// entered `Rebuilding` (no runner had left it) returns to `Loaded`.
    /// Before round 7 every refusal moved the workspace to `Loaded`, so a
    /// refused rebuild of a `Failed` workspace served its stale graph as
    /// fresh with no expiry.
    ///
    /// A variant with an arm below gets a log line naming the repair for
    /// the watcher-driven path that has no caller; any other (a plugin
    /// selection error kind the mapping renders as `WorkspaceBuildFailed`)
    /// takes the last arm.
    ///
    /// Surface parity W1 round 7 (design D37): each transition is a
    /// compare-exchange from `Rebuilding`, so a completed eviction's
    /// tombstone is left alone.
    fn record_refusal(ws: &LoadedWorkspace, err: &DaemonError, entered: WorkspaceState) {
        let target = refused_state(entered);
        if let DaemonError::WorkspaceIncompatibleGraph { root, reason } = err {
            // The roster resolver refused a manifest naming a plugin id this
            // binary did not compile, before the iteration reserved memory.
            // Nothing was written and the resident graph is intact, so this
            // is a refusal like the arms below: back to the state the
            // iteration entered from, no recorded failure, no backoff.
            // `daemon/rebuild` used to record a failure here and leave the
            // workspace `Failed`, where the daemon-hosted `rebuild_index`
            // kept it for the same refusal.
            tracing::warn!(
                workspace = %root.display(),
                reason = %reason,
                "rebuild refused: the manifest names a plugin this binary cannot build with"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        if let DaemonError::MemoryBudgetExceeded {
            limit_bytes,
            requested_bytes,
            ..
        } = err
        {
            // Admission could not reserve the iteration's working set even
            // after eviction. The reservation is the last step before the
            // build, so nothing was written and the resident graph is intact:
            // a refusal, back to the entered state, no recorded failure, no
            // backoff.
            tracing::warn!(
                workspace = %ws.key.source_root.display(),
                limit_bytes,
                requested_bytes,
                "rebuild refused: the memory budget cannot admit the rebuild's working set"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        if let DaemonError::RebuildWouldNarrowSelection {
            root,
            missing_plugin_ids,
            restore_command,
        } = err
        {
            // Surface parity W1 (D5): the rebuild was refused before
            // anything was written, so the prior graph is intact and
            // still the one the manifest describes. This is not a build
            // failure: the workspace returns to the state the iteration
            // entered from (`refused_state`; no `record_failure`, no
            // backoff), the caller receives the
            // typed error, and the log names the restore command for the
            // watcher-driven path that has no caller.
            tracing::warn!(
                workspace = %root.display(),
                missing_plugin_ids = ?missing_plugin_ids,
                restore_command = %restore_command,
                "rebuild refused: it would narrow the plugin selection the manifest records"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        if let DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } = err
        {
            // Surface parity W4 (W4-D7): the macro options were refused
            // before the build, so the prior graph is intact and still the
            // one the manifest describes. Same class as the narrowing
            // refusal: back to the entered state, no recorded failure, no
            // backoff, and the log carries the refusal's own text, whose
            // remedy fits where the directory came from (for the
            // watcher-driven path, always the record, so it names the
            // reset).
            tracing::warn!(
                workspace = %root.display(),
                expand_cache_dir = %expand_cache_dir.display(),
                origin = origin.as_str(),
                refusal = %err,
                "rebuild refused: the expand cache directory cannot be used"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        if let DaemonError::WorkspaceManifestUnreadable {
            root,
            manifest_path,
            reason,
        } = err
        {
            // Surface parity W1 round 2 (D9): the resolver (before the
            // build) or the guard (before the persist) refused because the
            // manifest cannot be read. Nothing was written and the resident
            // graph is intact, so this is the same class as the narrowing
            // refusal: back to the entered state, no recorded failure, no
            // backoff, and the log names the repair for the watcher-driven
            // path.
            tracing::warn!(
                workspace = %root.display(),
                manifest = %manifest_path.display(),
                reason = %reason,
                repair_command = %format!("sqry index --force {}", root.display()),
                "rebuild refused: the manifest cannot be read, so the recorded selection is unknown"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        if let DaemonError::InvalidArgument { reason } = err {
            // Surface parity W4 round 2 (W4-D11) and the integration of W1
            // and W4: the request named an input the daemon cannot honour
            // (`map_macro_options_err`: an empty expand cache directory, one
            // whose canonical path the manifest could not record, a recorded
            // cfg flag that names no predicate; the request check
            // `MacroOptionsRequest::validate`). It was refused before the
            // iteration reserved memory, so nothing was written and the
            // prior graph is intact: the same class as the refusals above,
            // back to the entered state, no recorded failure, no backoff. (A
            // queued request whose macro options conflict with the parked
            // one is refused before it reaches the lane, so it never reaches
            // this function.)
            tracing::warn!(
                workspace = %ws.key.source_root.display(),
                reason = %reason,
                "rebuild refused: the request names an input the daemon cannot honour"
            );
            if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
                tracing::debug!(
                    workspace = %ws.key.source_root.display(),
                    observed = %observed,
                    "a refused rebuild left the state to its writer"
                );
            }
            return;
        }
        tracing::warn!(
            workspace = %ws.key.source_root.display(),
            error = %err,
            "rebuild refused before anything was written"
        );
        if let Err(observed) = ws.transition_state(WorkspaceState::Rebuilding, target) {
            tracing::debug!(
                workspace = %ws.key.source_root.display(),
                observed = %observed,
                "a refusal left the state to its writer"
            );
        }
    }

    /// Drive the actual sqry-core rebuild pipeline on a blocking
    /// thread, with cooperative cancellation via a forwarder task
    /// that mirrors `ws.rebuild_cancelled` into a
    /// [`CancellationToken`].
    ///
    /// Sync-in-async bridge: the graph build is CPU-bound and uses rayon
    /// internally, so it must not block a tokio runtime worker thread. The blocking
    /// closure owns the cloned `Arc<PluginManager>` /
    /// [`BuildConfig`] / `Arc<CodeGraph>` / cancellation token so it
    /// outlives the awaited `JoinHandle`.
    ///
    /// Task 7 Phase 7c: a tokio task (`spawn_cancellation_forwarder`)
    /// polls `ws.rebuild_cancelled` at `CANCEL_FORWARDER_POLL_MS`
    /// cadence; the first `true` observation calls `token.cancel()`.
    /// The forwarder is `abort()`ed after the rebuild future returns
    /// regardless of outcome — polling stops immediately.
    ///
    /// The pipeline builds and persists with `inputs`, which
    /// [`Self::resolve_rebuild_inputs`] resolved before the reservation; it
    /// resolves nothing itself.
    ///
    /// # Error mapping
    ///
    /// - `GraphBuilderError::Cancelled` → `DaemonError::WorkspaceEvicted`
    ///   (the forwarder cancels on an eviction, a `daemon/cancel_rebuild`
    ///   or a `daemon/reset` of a rebuilding workspace), as
    ///   [`PipelineError::Failed`].
    /// - Any other `GraphBuilderError` → `DaemonError::WorkspaceBuildFailed`
    ///   with a human-readable reason, as [`PipelineError::Failed`].
    /// - The narrowing guard that runs before the durable persist →
    ///   [`PipelineError::Refused`]; the persist's own failure →
    ///   [`PipelineError::Failed`].
    /// - `spawn_blocking` join errors (panic inside the closure) →
    ///   `WorkspaceBuildFailed`, as [`PipelineError::Failed`].
    async fn execute_rebuild(
        &self,
        ws: &Arc<LoadedWorkspace>,
        prior: &Arc<CodeGraph>,
        mode: RebuildMode,
        changes: ChangeSet,
        inputs: RebuildInputs,
        requester: RebuildRequester,
    ) -> Result<DurableRebuildOutput, PipelineError> {
        let root = ws.key.source_root.clone();
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
        // first `cancellation.check()?` inside the graph build fires. Forces the pass-boundary
        // cancellation surface without racing the forwarder.
        if self.test_capture.get().is_some_and(|cap| {
            cap.precancel_token_for_pass_boundary
                .load(Ordering::Acquire)
        }) {
            token.cancel();
        }

        let token_for_blocking = token.clone();
        let capture_for_blocking = self.test_capture.get().cloned();
        // Audit item E: the build again under the persist lock (D-i8-5) is
        // a full build with the inputs another writer recorded, which can
        // outgrow the iteration's reservation (an incremental one reserved
        // staging for its changed files only; a widened roster parses more
        // files). The persist reserves the difference through the manager,
        // with eviction under pressure as for any reservation, sized from
        // the files the re-resolved roster will parse.
        let top_up: FullBuildTopUp = {
            let reserved = compute_working_set_estimate(prior, &changes, mode);
            let prior = Arc::clone(prior);
            let manager = Arc::clone(&self.manager);
            let key = ws.key.clone();
            Box::new(move |plugins: &PluginManager, config: &BuildConfig| {
                let files = count_buildable_files(&key.source_root, plugins, config);
                let needed = rebuild_under_lock_estimate(&prior, files);
                manager.reserve_rebuild(&key, needed.saturating_sub(reserved))
            })
        };
        let join_result = tokio::task::spawn_blocking(move || {
            let built = execute_rebuild_blocking(
                &root,
                &prior_for_blocking,
                mode,
                changes,
                &inputs.roster.plugins,
                &inputs.cfg,
                &token_for_blocking,
            )
            .map_err(PipelineError::Failed)?;
            persist_rebuild_output_blocking(
                root,
                built,
                &inputs,
                requester.build_command(mode),
                &token_for_blocking,
                capture_for_blocking.as_deref(),
                Some(top_up),
            )
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
            Err(join_err) => Err(PipelineError::Failed(DaemonError::WorkspaceBuildFailed {
                root: root_for_err,
                reason: format!("spawn_blocking join error: {join_err}"),
            })),
        }
    }

    /// Resolve the inputs one iteration builds with, refusing every input
    /// the build would refuse, before the iteration reserves memory (F1:
    /// refuse before you evict).
    ///
    /// - The roster the manifest at `root` records (surface parity W1,
    ///   D1/D2): a workspace indexed with `--include-high-cost` rebuilds
    ///   with the plugins its manifest records, not a process-wide default.
    /// - The macro build options (surface parity W4, W4-D7): the manifest's
    ///   record overlaid by `macro_request`. The roster resolve already
    ///   refused an unreadable manifest, so the rule here is refuse.
    /// - The narrowing guard ([`refuse_if_rebuild_narrows`]): the roster
    ///   about to be recorded against the selection the manifest records.
    ///   It runs again before the durable persist, for a manifest rewritten
    ///   during the build.
    ///
    /// # Errors
    ///
    /// Every error is a refusal: nothing was reserved, built or written.
    /// [`DaemonError::WorkspaceIncompatibleGraph`] for an id this binary did
    /// not compile; [`DaemonError::WorkspaceManifestUnreadable`] for an
    /// unreadable manifest; [`DaemonError::RebuildMacroOptionsUnavailable`]
    /// for an expand cache directory that does not exist;
    /// [`DaemonError::InvalidArgument`] for an empty expand cache directory
    /// or one whose canonical path is not valid UTF-8;
    /// [`DaemonError::RebuildWouldNarrowSelection`] for a roster that drops
    /// recorded ids; [`DaemonError::WorkspaceBuildFailed`] for a selection
    /// error kind the mapping does not name.
    fn resolve_rebuild_inputs(
        &self,
        root: &Path,
        macro_request: &MacroOptionsRequest,
        resident: Option<Arc<RosterRecord>>,
        requester: RebuildRequester,
    ) -> Result<RebuildInputs, DaemonError> {
        // Only a caller's explicit request to replace the index falls back
        // over a manifest it cannot read (design D9): the daemon-hosted
        // `rebuild_index`. The watcher and `daemon/rebuild` refuse.
        let unreadable_policy = if requester.falls_back_over_an_unreadable_manifest() {
            UnreadableManifestPolicy::FallBack
        } else {
            UnreadableManifestPolicy::Refuse
        };
        resolve_durable_rebuild_inputs(
            &self.roster,
            &self.build_config,
            root,
            macro_request,
            resident,
            unreadable_policy,
        )
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

    /// Surface parity W1 round 5 (design D27; its stated position
    /// narrowed in round 6 by D31): test-only observation + stall point
    /// inside `execute_one_rebuild`, fired AFTER the publish block's read
    /// guard is released (the publish is complete) and BEFORE the publish
    /// hook is dispatched with the graph half of the pair
    /// `publish_and_retain` swapped in. The `Loaded` state store, the
    /// success bookkeeping and the `published_generations` push precede
    /// it in the source; that is a source order, not a declared contract:
    /// no production reader observes the seam's position relative to the
    /// state store (`classify_for_serve` and `WorkspaceState::is_serving`
    /// decide the same for `Rebuilding` as for `Loaded`, and the hook
    /// reads no state), and T47 pins the position relative to the guard
    /// release and the dispatch only (battery rows K38 and S25). Mirrors
    /// [`Self::post_reservation_check`]: production builds with no
    /// `TestCapture` installed see a single atomic load + return; with a
    /// capture installed, the durable reached-flag is set, then
    /// `post_publish_reached.notify_waiters()` fires, then, if
    /// `post_publish_hold > 0`, the iteration stalls on
    /// `post_publish_release.notified()` (the `notified()` future armed
    /// BEFORE the re-check, closing the lost-wakeup window).
    async fn post_publish_check(&self) {
        let Some(cap) = self.test_capture.get() else {
            return;
        };
        cap.post_publish_reached_flag.store(true, Ordering::Release);
        cap.post_publish_reached.notify_waiters();
        if cap.post_publish_hold.load(Ordering::Acquire) == 0 {
            return;
        }
        let notified = cap.post_publish_release.notified();
        if cap.post_publish_hold.load(Ordering::Acquire) > 0 {
            notified.await;
            cap.post_publish_hold.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Test-only seam on the release path of the drain loop: after the
    /// loop found the lane empty, before it takes the lane again to release
    /// the runner role. Production builds with no `TestCapture` installed
    /// see a single atomic load + return; with one installed the reached
    /// flag is set, `pre_release_reached` fires, and with
    /// `pre_release_hold > 0` the loop stalls on `pre_release_release`
    /// (the `notified()` future armed before the re-check).
    async fn pre_release_check(&self) {
        let Some(cap) = self.test_capture.get() else {
            return;
        };
        cap.pre_release_reached_flag.store(true, Ordering::Release);
        cap.pre_release_reached.notify_waiters();
        if cap.pre_release_hold.load(Ordering::Acquire) == 0 {
            return;
        }
        let notified = cap.pre_release_release.notified();
        if cap.pre_release_hold.load(Ordering::Acquire) > 0 {
            notified.await;
            cap.pre_release_hold.fetch_sub(1, Ordering::AcqRel);
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
    /// Refused once [`Self::shutdown`] has run. Otherwise looks up
    /// `watchers[key]`:
    /// - If present and watching (`WatcherEntry::is_watching`: its task is
    ///   live and its stop signal clear), returns `Ok(())` without
    ///   spawning: an active pair is already producing dispatches.
    /// - If present and stopped or exiting (its stop signal set by a
    ///   tombstone writer, or its async task past its loop), prunes the
    ///   entry and spawns a new pair with a fresh generation and a fresh
    ///   stop signal ([`LoadedWorkspace::arm_watcher_stop`], which also
    ///   sets the old one, so a stopped pair still draining exits rather
    ///   than run beside the new one).
    /// - If absent, spawns a new pair.
    ///
    /// # Shutdown lifecycle
    ///
    /// Each pair stops on its own stop signal, which the tombstone writers
    /// (eviction, `unload`, `reset`), the next `arm_watcher_stop` for the
    /// workspace and [`Self::shutdown`] set: the blocking loop polls it
    /// every 100 ms, returns and drops the mpsc sender, so the async
    /// task's receive returns `None` and it exits too. A change set the
    /// async task receives after the signal is set is refused under the
    /// rebuild lane (`handle_changes_with_git_state`), and a
    /// `WorkspaceEvicted` answer ends the async task only when the signal
    /// is set (a `daemon/cancel_rebuild` leaves the watcher watching).
    ///
    /// The async task's last action is `live.store(false)` followed by
    /// [`Self::reap_watcher`]`(&key, generation)`, removing the entry from
    /// the map by compare-and-remove on the generation.
    ///
    /// # Placement constraint
    ///
    /// This method lives on `RebuildDispatcher`, NOT `WorkspaceManager`
    /// (per the 7b2 `RESUME_PROMPT` constraint): coupling watcher
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
    /// - [`DaemonError::WorkspaceEvicted`] when the workspace is a
    ///   tombstone (`Evicted`, or `Unloaded` after a reset): no watcher is
    ///   started for it.
    pub fn ensure_watching(
        self: &Arc<Self>,
        key: &WorkspaceKey,
        ws: &Arc<LoadedWorkspace>,
        root: &Path,
    ) -> Result<(), DaemonError> {
        // The watcher is registered, and dispatches, under the key the
        // workspace is registered under, not the caller's: an anonymous
        // caller key resolves to the workspace by source root, so it can
        // differ from the registered key in its root mode or fingerprint
        // (`daemon/load` from the CLI sends `GitRoot`, the pinned preload
        // registers `WorkspaceFolder`). Keyed by the caller's key, a second
        // caller would spawn a second watcher beside the first, and
        // `daemon/status` (which asks by the registered key) would report
        // the workspace unwatched.
        debug_assert_eq!(
            key.source_root, ws.key.source_root,
            "ensure_watching: the key and the workspace must name one root"
        );
        let key = &ws.key;
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

        // Step 0: no watcher starts once the dispatcher is shutting down
        // (`Self::shutdown`), or the runtime would wait on its blocking
        // loop at exit.
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(DaemonError::Io(std::io::Error::other(format!(
                "not watching {}: the daemon is shutting down",
                root.display()
            ))));
        }

        // Step 1: fast-path liveness check. An entry whose stop signal is
        // set is draining (an eviction or a reset stopped it, and its
        // blocking loop has not observed the signal yet), not watching: a
        // reload within that window must start a new watcher, or the old
        // one exits and the reloaded workspace is left unwatched.
        // An entry armed for another `LoadedWorkspace` object of this key
        // (a workspace unloaded and loaded again) does not serve `ws`: the
        // tombstone writers of `ws` cannot reach its signal, so it is
        // replaced (step 7 sets its signal) rather than kept.
        if let Some(entry) = watchers.get(key)
            && entry.is_watching()
            && ws.holds_watcher_stop(&entry.stop)
        {
            return Ok(());
        }

        // Step 2 — Allocate a monotonic generation token.
        let generation = self.next_watcher_generation.fetch_add(1, Ordering::Relaxed);

        // Step 3 — Construct the watcher (sync I/O). Errors bubble
        // up via `?`; the locked guard is dropped on return.
        let watcher = SourceTreeWatcher::new(root).map_err(|e| {
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

        // This watcher's own stop signal. Only the tombstone writers set
        // it (`LoadedWorkspace::stop_watcher`); a cancelled rebuild does
        // not, so the watcher survives `daemon/cancel_rebuild` and a reset
        // of a rebuilding workspace.
        //
        // A tombstone (`Evicted`, or `Unloaded` after a reset) is never
        // armed: the state is read under the signal's lock, which the
        // tombstone writers hold while they store it, so a watcher started
        // as an eviction or a reset lands is either stopped by it or
        // refused here, never left running on the tombstone with a clear
        // signal nothing sets again (audit S1). The intent
        // (`watch_wanted`) is kept, so the reload of an evicted workspace
        // starts the watcher once the workspace is resident again.
        let Some(stop) = ws.arm_watcher_stop() else {
            return Err(DaemonError::WorkspaceEvicted {
                root: root.to_path_buf(),
            });
        };
        let stop_for_entry = Arc::clone(&stop);

        // Step 6a: spawn the blocking watcher thread.
        let blocking_handle = {
            let ws = Arc::clone(ws);
            let stop = Arc::clone(&stop);
            tokio::task::spawn_blocking(move || {
                watch_loop_blocking(&watcher, &tx, &ws, &stop, debounce, cancel_poll_period);
            })
        };

        // Step 6b — Spawn the async dispatcher task.
        let async_handle = {
            let dispatcher = Arc::clone(self);
            let key = key.clone();
            let ws = Arc::clone(ws);
            let live_for_task = Arc::clone(&live);
            tokio::spawn(async move {
                dispatch_loop_async(&dispatcher, &key, &ws, &stop, rx).await;
                // Mark ourselves as draining BEFORE reap_watcher so a
                // concurrent ensure_watching observes the correct
                // liveness state.
                live_for_task.store(false, Ordering::Release);
                dispatcher.reap_watcher(&key, generation);
            })
        };

        // Step 7: prune any stale entry and insert the new one.
        // Prune covers the case where a prior entry existed and was not
        // watching (observed by step 1 falling through): its stop signal
        // is set, by its tombstone writer or by `arm_watcher_stop` above,
        // so it exits on its own and its late `reap_watcher` call, keyed
        // by its older generation, leaves this entry alone. `remove` is
        // idempotent when no entry exists.
        if let Some(replaced) = watchers.remove(key) {
            replaced.stop.store(true, Ordering::Release);
        }
        watchers.insert(
            key.clone(),
            WatcherEntry {
                generation,
                live,
                stop: stop_for_entry,
                async_handle,
                blocking_handle,
            },
        );
        // `watchers` drops here → lock released.
        Ok(())
    }

    /// Production bootstrap hook: start watching `key` after a
    /// successful [`crate::workspace::WorkspaceManager::get_or_load`].
    ///
    /// This is the "Task 9 daemon bootstrap" the [`Self::ensure_watching`]
    /// placement comment defers to. Before this method existed,
    /// `ensure_watching` was reachable only from test harnesses, so loaded
    /// graphs silently drifted from disk and refreshed only on an explicit
    /// `sqry daemon rebuild` (see verivus-oss/sqry#461). Every production
    /// path that makes a workspace resident calls this so edits trigger a
    /// debounced rebuild: the `daemon/load` IPC handler, the pinned pre-load
    /// at startup, the daemon-hosted `rebuild_index` load route, and the
    /// read-only reload of an evicted workspace that wanted watching
    /// ([`crate::workspace::LoadedWorkspace::watch_wanted`], which this
    /// sets).
    ///
    /// It resolves the freshly-loaded `Arc<LoadedWorkspace>` from the
    /// manager it already holds and delegates to [`Self::ensure_watching`],
    /// keeping the watcher lifecycle on `RebuildDispatcher` (the placement
    /// constraint) rather than leaking it into `WorkspaceManager`.
    ///
    /// # Non-fatal by contract
    ///
    /// Watching is best-effort and MUST NOT fail the load. A failure to
    /// start the watcher (a non-git workspace, since `SourceTreeWatcher::new`
    /// requires a `.git` directory; inotify-instance exhaustion; or a
    /// `.gitignore` read error) leaves the graph resident and queryable;
    /// the workspace merely behaves as it did before this wiring existed
    /// (refreshed only by explicit `sqry daemon rebuild`). The failure is
    /// logged at WARN so a missing watcher is observable rather than
    /// silent.
    ///
    /// # Idempotence
    ///
    /// Safe to call after every `get_or_load`, including reloads: an
    /// already-live watcher is a no-op via `ensure_watching`'s liveness
    /// fast-path.
    pub fn start_watching(self: &Arc<Self>, key: &WorkspaceKey) {
        let Some(ws) = self.manager.lookup(key) else {
            // The workspace is not resident (evicted under memory
            // pressure, or a racing unload between load and this call).
            // Nothing to watch; not an error.
            tracing::debug!(
                root = %key.source_root.display(),
                "start_watching: workspace not resident, skipping watcher setup"
            );
            return;
        };

        ws.watch_wanted.store(true, Ordering::Release);
        let root = ws.key.source_root.clone();
        match self.ensure_watching(&ws.key, &ws, &root) {
            Ok(()) => {
                tracing::info!(
                    root = %root.display(),
                    debounce_ms = self.config.debounce_ms,
                    "file watcher active; edits trigger a debounced rebuild"
                );
            }
            Err(DaemonError::WorkspaceEvicted { .. }) => {
                tracing::debug!(
                    root = %root.display(),
                    "start_watching: the workspace was evicted or reset before its watcher \
                     started; the next load watches it"
                );
            }
            Err(e) => {
                tracing::warn!(
                    root = %root.display(),
                    err = %e,
                    "failed to start file watcher; workspace will not auto-rebuild on \
                     edits (refresh manually with `sqry daemon rebuild`)"
                );
            }
        }
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
    /// need to force-reap a watcher: a pair whose stop signal is set reaps
    /// itself as its async task exits. Tests that
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

    /// Snapshot the workspace keys whose watcher is watching: its task is
    /// live and its stop signal is clear. A watcher an eviction, an unload
    /// or a reset has stopped is not listed, even in the poll before its
    /// loop exits.
    ///
    /// Used by `daemon/status` to expose watcher observability without
    /// changing watcher bootstrap or moving watcher ownership into
    /// [`WorkspaceManager`].
    #[must_use]
    pub fn live_watcher_keys(&self) -> HashSet<WorkspaceKey> {
        self.watchers
            .lock()
            .iter()
            .filter(|(_, entry)| entry.is_watching())
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Stop the daemon's rebuild work for shutdown (`daemon/stop`, SIGTERM,
    /// SIGINT; issue #902): no watcher starts after this call, every
    /// watcher's stop signal is set, and every rebuild in flight is
    /// cancelled as `daemon/cancel_rebuild` cancels it (under the rebuild
    /// lane, only while a runner holds the role), so its callers and the
    /// requests parked behind it are answered `-32004`.
    ///
    /// The watchers' blocking loops observe their signal within one poll
    /// (100 ms) and exit, so the runtime is not left waiting on them at
    /// exit: before this existed nothing set the signal on shutdown, and a
    /// stopped daemon stayed alive until the next file event in a watched
    /// tree woke its watcher. A rebuild cancelled here stops at its next
    /// pass boundary; one already in its durable persist finishes the
    /// write: the process waits, without a bound, for every persist in
    /// flight before its bounded runtime shutdown
    /// (`entrypoint::finish_runtime`, sqry-core's `PersistGate`), and a
    /// persist that has not begun by then is refused before it writes.
    ///
    /// Idempotent. Lock order: the watcher map alone, then `workspaces`
    /// alone for the snapshot, then each rebuild lane alone (through
    /// [`Self::cancel_rebuild`]); never nested.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let stops: Vec<Arc<AtomicBool>> = self
            .watchers
            .lock()
            .values()
            .map(|entry| Arc::clone(&entry.stop))
            .collect();
        for stop in &stops {
            stop.store(true, Ordering::Release);
        }
        for ws in self.manager.workspaces_snapshot() {
            // The watcher signal the workspace holds too: a watcher attached
            // by another dispatcher over the same manager (tests) stops with
            // this one.
            ws.stop_watcher();
            let _ = self.cancel_rebuild(&ws).await;
        }
        tracing::info!(
            watchers = stops.len(),
            "rebuild dispatcher shutting down: watchers stopped, rebuilds in flight cancelled"
        );
    }

    /// Wait until every watcher's tasks have exited and been reaped, or
    /// `within` elapses; answers whether they all did. Used after
    /// [`Self::shutdown`] so the IPC server returns only once no watcher
    /// thread is left for the runtime to wait on.
    pub async fn wait_for_watchers_to_exit(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.watchers.lock().is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
/// - `wait_for_changes_cancellable` returns `Ok(None)`: `stop`, this
///   watcher's own signal, was set by a tombstone writer (eviction,
///   `unload`, `reset`). A cancelled rebuild does not set it.
/// - `wait_for_changes_cancellable` returns `Err` — notify channel
///   disconnect (unrecoverable); logged at error level.
/// - `tx.blocking_send` returns `Err` — async receiver dropped
///   (normal shutdown); logged at debug.
fn watch_loop_blocking(
    watcher: &SourceTreeWatcher,
    tx: &tokio::sync::mpsc::Sender<(ChangeSet, LastIndexedGitState)>,
    ws: &LoadedWorkspace,
    stop: &AtomicBool,
    debounce: Duration,
    cancel_poll_period: Duration,
) {
    loop {
        let last_git = ws.last_indexed_git_state.read().clone();
        match watcher.wait_for_changes_cancellable(
            debounce,
            last_git.as_ref(),
            stop,
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
            }
            Ok(Some(cs)) if cs.changed_files.is_empty() && !cs.requires_full_rebuild() => {
                // Git-state-only change whose classifier output does
                // NOT require a full rebuild (Noise or LocalCommit
                // class). A2 §B mandates: these classes are reported
                // for telemetry but do not trigger a rebuild by
                // themselves — a real commit that changed the working
                // tree was already observed as a source-tree event.
                // Skip silently; loop for the next debounce window.
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
                //   `GitStateWatcher::classify` in
                //   `sqry-core/src/watch/git_state.rs`). A
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
/// - `rx.recv()` returns `None`: the blocking side exited, channel
///   closed; logged at debug.
/// - `handle_changes_with_git_state` returns `Err(WorkspaceEvicted)`
///   while `stop` is set: the workspace was evicted, unloaded or reset;
///   logged at info.
///
/// `WorkspaceEvicted` with `stop` clear is a cancelled rebuild
/// (`daemon/cancel_rebuild`, a reset of the rebuilding workspace): the
/// loop continues, so cancelling a rebuild never stops the watcher. Other
/// errors (refusals, `WorkspaceBuildFailed`, `Io`) continue the loop too:
/// the baseline is not advanced (the publish did not happen), so the next
/// `wait_for_changes_cancellable` call re-observes the divergence and
/// retries.
async fn dispatch_loop_async(
    dispatcher: &Arc<RebuildDispatcher>,
    key: &WorkspaceKey,
    ws: &LoadedWorkspace,
    stop: &AtomicBool,
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
            .handle_changes_with_git_state(key, cs, new_git_state, stop)
            .await
        {
            Ok(()) => {
                // Baseline advance (if any) was handled by the
                // runner inside execute_one_rebuild at publish
                // time — nothing for the bridge to do here.
            }
            Err(DaemonError::WorkspaceEvicted { .. }) if stop.load(Ordering::Acquire) => {
                tracing::info!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    "workspace evicted; terminating async dispatcher"
                );
                break;
            }
            Err(DaemonError::WorkspaceEvicted { .. }) => {
                tracing::info!(
                    target: "sqry_daemon::watch",
                    workspace = %ws.key.source_root.display(),
                    "rebuild cancelled; the watcher keeps watching"
                );
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

/// Reserves, for the build again under the persist lock, what that build
/// needs beyond the iteration's own reservation, sized from the roster and
/// configuration it builds with (audit item E).
type FullBuildTopUp =
    Box<dyn FnOnce(&PluginManager, &BuildConfig) -> Result<RebuildReservation, DaemonError> + Send>;

/// The refusal when the persist's wait for the lock answers
/// `LockWait::IndexRemoved` (decision D-i8-6).
pub(crate) const INDEX_REMOVED_DURING_PERSIST_WAIT: &str = "the index directory was removed while the rebuild waited for its persist lock; no index was written";

/// The refusal when a removal check under the persist lock fails, or the
/// transaction refuses its caller's hold at entry (decision D-i8-6).
pub(crate) const INDEX_REMOVED_DURING_PERSIST: &str =
    "the index directory was removed during the persist; no index was written";

/// The working set of a full build of `files` files over `prior` (the
/// graph the iteration started from): `compute_working_set_estimate`'s
/// full-mode figure with the file count of the inputs actually built, so
/// a roster widened during the build (`json` added) is counted.
fn rebuild_under_lock_estimate(prior: &CodeGraph, files: usize) -> u64 {
    let prior_bytes = prior.heap_bytes() as u64;
    let prior_files = prior.files().len() as u64;
    let files = files as u64;
    working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: prior_bytes.saturating_add(
            files
                .saturating_sub(prior_files)
                .saturating_mul(ESTIMATE_FINAL_PER_FILE_BYTES),
        ),
        staging_overhead: files.saturating_mul(ESTIMATE_STAGING_PER_FILE_BYTES),
        interner_snapshot_bytes: prior.strings().heap_bytes() as u64,
    })
}

/// Persist a rebuilt graph durably, recording the roster it was built
/// with (surface parity W1, S9) and the macro build options `inputs.cfg`
/// carries.
///
/// Holds the root's persist lock ([`IndexWriteLock`], the one every
/// persist path in this tree takes) across the narrowing guard and the
/// transaction, which re-enters it; the daemon-hosted `rebuild_index` can
/// persist the same root while an iteration's persist runs.
///
/// Under the lock it resolves the inputs again
/// ([`resolve_durable_rebuild_inputs`], which runs
/// [`refuse_if_rebuild_narrows`]), for `RebuildMode::Full` and
/// `RebuildMode::Incremental` alike because the persist runs for both; the
/// preflight resolved them before the reservation. If the roster or the
/// macro options the manifest records now differ from the ones the graph
/// was built with (another writer published during the build), the graph
/// is built again under the lock with the inputs current now and that
/// graph is persisted and returned (decision D-i8-5), so the manifest
/// records the record current at its publication. A refusal of that
/// resolution is [`PipelineError::Refused`] (nothing written); a failure
/// of the rebuild under the lock or of the persist itself is
/// [`PipelineError::Failed`], and the transaction has put the old manifest
/// and snapshot back by then (`persist_durable_graph_transaction`). The
/// manifest's `plugin_selection` is the resolved record, so the recorded
/// `high_cost_mode` is carried through instead of `None`.
///
/// The wait for the lock checks `cancellation` and the persist gate at
/// each step ([`PipelineError::Failed`], `WorkspaceEvicted`) and refuses
/// on `LockWait::IndexRemoved` ([`INDEX_REMOVED_DURING_PERSIST_WAIT`]).
/// Under the hold, `index_still_there` is checked where a refusal of the
/// resolution above is mapped, after that resolution, before a rebuild
/// under the lock and before the transaction: the hold is current and,
/// when armed, `holds_committed_index` is true. It is armed by
/// `inputs.index_present` or the hold's `saw_committed_index`. A failure
/// refuses with [`INDEX_REMOVED_DURING_PERSIST`], as does the
/// transaction's `IndexRemovedDuringPersist`. What these checks miss is
/// recorded in decision D-i8-6. A build again under the lock first
/// reserves what it needs (`top_up`).
fn persist_rebuild_output_blocking(
    root: PathBuf,
    built: RebuildGraphOutput,
    inputs: &RebuildInputs,
    build_command: &'static str,
    cancellation: &CancellationToken,
    capture: Option<&TestCapture>,
    top_up: Option<FullBuildTopUp>,
) -> Result<DurableRebuildOutput, PipelineError> {
    #[cfg(test)]
    persist_plant::run(&root, persist_plant::Phase::Arrived);
    // Decision D-i8-6: the wait checks the iteration's cancellation and the
    // persist gate's closure; either ends it holding nothing and writing no
    // index (the creating wait may already have created the directory and
    // the lock file).
    let cancelled = || cancellation.is_cancelled() || PersistGate::global().is_closed();
    let cancelled_err =
        || PipelineError::Failed(DaemonError::WorkspaceEvicted { root: root.clone() });
    let removed_err = || {
        PipelineError::Refused(DaemonError::WorkspaceBuildFailed {
            root: root.clone(),
            reason: INDEX_REMOVED_DURING_PERSIST.to_string(),
        })
    };
    // A committed index at input resolution selects the existing-index wait
    // (it does not create the directory); none selects the creating wait.
    let wait_for_lock = if inputs.index_present {
        IndexWriteLock::acquire_existing_unless_cancelled
    } else {
        IndexWriteLock::acquire_unless_cancelled
    };
    let wait = wait_for_lock(
        GraphStorage::new(&root).graph_dir(),
        &cancelled,
        &mut || {
            if let Some(cap) = capture {
                cap.persist_lock_contended.store(true, Ordering::Release);
            }
        },
    )
    .map_err(|err| {
        PipelineError::Failed(DaemonError::WorkspaceBuildFailed {
            root: root.clone(),
            reason: format!("could not take the index's persist lock: {err:#}"),
        })
    })?;
    let serialised = match wait {
        LockWait::Held(held) => held,
        LockWait::Cancelled => return Err(cancelled_err()),
        LockWait::IndexRemoved => {
            return Err(PipelineError::Refused(DaemonError::WorkspaceBuildFailed {
                root: root.clone(),
                reason: INDEX_REMOVED_DURING_PERSIST_WAIT.to_string(),
            }));
        }
    };
    // The removal check under the hold: the hold is on the lock file the
    // path names and, when armed, a committed index is still there. Armed
    // by a committed index at input resolution or one the hold saw (when
    // the wait opened the lock file, committed while it waited, or when
    // the hold arrived).
    let persist_graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let armed = inputs.index_present || serialised.saw_committed_index();
    let index_still_there = |held: &IndexWriteLock| {
        held.is_current() && (!armed || holds_committed_index(&persist_graph_dir))
    };
    if capture.is_some_and(|cap| cap.cancel_token_after_lock_acquired.load(Ordering::Acquire)) {
        cancellation.cancel();
    }
    // A cancel that landed while the lock was being handed over (audit N6).
    if cancelled() {
        return Err(cancelled_err());
    }
    #[cfg(test)]
    persist_plant::run(&root, persist_plant::Phase::Inside);
    // Decision D-i8-5: resolve the inputs again under the lock, which runs
    // the narrowing guard against the manifest as it is now. If the record
    // changed since the build's inputs were resolved (another writer
    // published), the built graph carries stale inputs: build again, under
    // the lock, with the inputs current now, so the manifest records the
    // record current under the lock when it is published.
    let current = resolve_durable_rebuild_inputs(
        &inputs.resolved_from.resolver,
        &inputs.resolved_from.build_config,
        &root,
        &inputs.resolved_from.macro_request,
        inputs.resident.clone(),
        inputs.unreadable_policy,
    )
    .map_err(|refusal| {
        // A refusal with the index no longer there is reported as the
        // removal, which can cause it (a narrowing against the resident
        // record, a missing expand cache).
        if index_still_there(&serialised) {
            PipelineError::Refused(refusal)
        } else {
            removed_err()
        }
    })?;
    // Checked before the work below too (fourth audit, item 4).
    if !index_still_there(&serialised) {
        return Err(removed_err());
    }
    let unchanged = current.roster.record.selection_manifest()
        == inputs.roster.record.selection_manifest()
        && current.cfg.macro_options == inputs.cfg.macro_options;
    // Held to the end of the persist, beside the iteration's own.
    let mut _top_up_reservation: Option<RebuildReservation> = None;
    let (inputs, built) = if unchanged {
        (inputs, built)
    } else {
        // The stale graph goes first, so the rebuild under the lock holds
        // one graph at a time; the reservation is topped up to what this
        // build needs just below.
        drop(built);
        // A full build, so a full build's reservation (audit item E): a
        // budget refusal is a refusal, an eviction or cancel seen there is
        // that cancellation.
        if let Some(top_up) = top_up {
            _top_up_reservation = Some(top_up(&current.roster.plugins, &current.cfg).map_err(
                |err| match err {
                    DaemonError::WorkspaceEvicted { .. } => PipelineError::Failed(err),
                    other => PipelineError::Refused(other),
                },
            )?);
        }
        if let Some(cap) = capture {
            cap.hold_before_rebuild_under_lock();
        }
        if !index_still_there(&serialised) {
            return Err(removed_err());
        }
        if let Some(cap) = capture {
            cap.rebuilds_under_lock.fetch_add(1, Ordering::AcqRel);
        }
        tracing::info!(
            workspace = %root.display(),
            "the index's recorded build inputs changed during the rebuild; building again with the inputs recorded now"
        );
        if capture.is_some_and(|cap| {
            cap.cancel_token_at_rebuild_under_lock
                .load(Ordering::Acquire)
        }) {
            cancellation.cancel();
        }
        // The iteration's own token, checked at this build's pass
        // boundaries, as for the first build.
        let rebuilt = build_unified_graph_with_progress_cancellable(
            &root,
            &current.roster.plugins,
            &current.cfg,
            sqry_core::progress::no_op_reporter(),
            cancellation,
        )
        .map_err(|err| {
            PipelineError::Failed(map_graph_builder_err(
                err,
                root.clone(),
                "rebuild with the recorded inputs current at publication",
            ))
        })?;
        (
            &current,
            RebuildGraphOutput {
                graph: rebuilt.0,
                effective_threads: rebuilt.1,
            },
        )
    };
    let RebuildGraphOutput {
        graph,
        effective_threads,
    } = built;
    // The last cancellation check before the transaction (audit N6).
    if cancelled() {
        return Err(cancelled_err());
    }
    #[cfg(test)]
    persist_plant::run(&root, persist_plant::Phase::BeforeTransaction);
    // The last removal check before the transaction (third audit, item 2).
    if !index_still_there(&serialised) {
        return Err(removed_err());
    }
    if capture.is_some_and(|cap| cap.remove_index_before_transaction.load(Ordering::Acquire)) {
        let _ = std::fs::remove_dir_all(root.join(".sqry"));
    }
    persist_durable_graph_transaction(
        graph,
        DurableGraphPersistenceRequest {
            root: &root,
            plugins: &inputs.roster.plugins,
            config: &inputs.cfg,
            build_command,
            plugin_selection: Some(inputs.roster.record.selection_manifest()),
            progress: sqry_core::progress::no_op_reporter(),
            effective_threads,
        },
    )
    .map(|(graph, build_result)| DurableRebuildOutput {
        graph,
        build_result,
        roster: Arc::clone(&inputs.roster.record),
    })
    .map_err(|err| {
        // The transaction's refusal of a stale hold is a refusal here too.
        if err
            .chain()
            .any(|cause| cause.is::<IndexRemovedDuringPersist>())
        {
            return PipelineError::Refused(DaemonError::WorkspaceBuildFailed {
                root,
                reason: INDEX_REMOVED_DURING_PERSIST.to_string(),
            });
        }
        PipelineError::Failed(DaemonError::WorkspaceBuildFailed {
            root,
            reason: format!("durable graph persistence transaction failed: {err:#}"),
        })
    })
}

/// Test-only: a closure the persist runs as it arrives at the root's
/// persist lock and again once it holds it, so a unit test can hold one
/// persist inside and observe whether a second persist of the same root,
/// already arrived, gets in.
#[cfg(test)]
mod persist_plant {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// Where the persist is when the plant runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Phase {
        /// Before it takes the root's persist lock.
        Arrived,
        /// Holding the lock, before the narrowing guard and the transaction.
        Inside,
        /// Holding the lock, after the last cancellation check and before
        /// the last removal check ahead of the transaction.
        BeforeTransaction,
    }

    type Plant = Arc<dyn Fn(Phase) + Send + Sync>;

    static PLANT: parking_lot::Mutex<Option<(PathBuf, Plant)>> = parking_lot::Mutex::new(None);

    /// Run `plant` at each phase of every persist of `root` until [`clear`].
    pub(super) fn install(root: &Path, plant: Plant) {
        *PLANT.lock() = Some((root.to_path_buf(), plant));
    }

    pub(super) fn clear() {
        *PLANT.lock() = None;
    }

    pub(super) fn run(root: &Path, phase: Phase) {
        let plant = PLANT
            .lock()
            .as_ref()
            .filter(|(planted, _)| planted == root)
            .map(|(_, plant)| Arc::clone(plant));
        if let Some(plant) = plant {
            plant(phase);
        }
    }
}

/// Resolve the inputs a durable rebuild at `root` builds with, refusing
/// every input the build would refuse, before any memory is reserved:
///
/// - the roster the manifest records (surface parity W1, D1/D2): a
///   workspace indexed with `--include-high-cost` rebuilds with the plugins
///   its manifest records, not with a process-wide default;
/// - the macro build options (surface parity W4, W4-D7): the manifest's
///   record overlaid by `macro_request` (the roster resolve already refused
///   an unreadable manifest, so the rule here is refuse);
/// - the narrowing guard ([`refuse_if_rebuild_narrows`]), which runs again
///   before the durable persist.
///
/// Used by [`RebuildDispatcher::resolve_rebuild_inputs`] and by
/// `RealWorkspaceBuilder`'s durable preparation (the daemon-hosted
/// `rebuild_index` over a workspace that is not resident), so both
/// persisting paths refuse the same inputs.
///
/// # Errors
///
/// Every error is a refusal: nothing was reserved, built or written.
/// [`DaemonError::WorkspaceIncompatibleGraph`] for an id this binary did
/// not compile; [`DaemonError::WorkspaceManifestUnreadable`] for an
/// unreadable manifest; [`DaemonError::InvalidArgument`] for a request
/// the request check refuses ([`validate_macro_request`]);
/// [`DaemonError::RebuildMacroOptionsUnavailable`] for an expand cache
/// directory that does not exist; [`DaemonError::InvalidArgument`] for an
/// empty expand cache directory or one whose canonical path is not valid
/// UTF-8;
/// [`DaemonError::RebuildWouldNarrowSelection`] for a roster that drops
/// recorded ids; [`DaemonError::WorkspaceBuildFailed`] for a selection
/// error kind the mapping does not name.
pub(crate) fn resolve_durable_rebuild_inputs(
    roster: &Arc<WorkspaceRosterResolver>,
    build_config: &BuildConfig,
    root: &Path,
    macro_request: &MacroOptionsRequest,
    resident: Option<Arc<RosterRecord>>,
    unreadable_policy: UnreadableManifestPolicy,
) -> Result<RebuildInputs, DaemonError> {
    let (resolved, unreadable_manifest) = match unreadable_policy {
        UnreadableManifestPolicy::Refuse => (roster.resolve(root)?, None),
        UnreadableManifestPolicy::FallBack => roster.resolve_for_rebuild(root)?,
    };
    validate_macro_request(root, macro_request)?;
    // Over a manifest that cannot be read there is no record to overlay:
    // the request alone decides (`TreatAsNoRecord`, as the standalone
    // rebuild resolves it). Only a `FallBack` resolve gets here with one.
    let record_rule = if unreadable_manifest.is_some() {
        UnreadableManifestRule::TreatAsNoRecord
    } else {
        UnreadableManifestRule::Refuse
    };
    let macro_options = resolve_macro_options(root, macro_request, record_rule)
        .map_err(|err| map_macro_options_err(err, root, macro_request))?;
    refuse_if_rebuild_narrows(
        root,
        &resolved.record,
        resident.as_deref(),
        unreadable_policy,
    )?;
    if let Some(unreadable) = &unreadable_manifest {
        tracing::warn!(
            workspace = %root.display(),
            manifest = %unreadable.manifest_path.display(),
            reason = %unreadable.reason,
            "manifest unreadable; rebuilding with the fallback roster, which the persist records"
        );
    }
    Ok(RebuildInputs {
        roster: resolved,
        cfg: BuildConfig {
            macro_options: macro_options.options,
            ..build_config.clone()
        },
        resident,
        unreadable_policy,
        resolved_from: RebuildInputSources {
            resolver: Arc::clone(roster),
            build_config: build_config.clone(),
            macro_request: macro_request.clone(),
        },
        index_present: index_present_at(GraphStorage::new(root).graph_dir()),
    })
}

/// `RebuildInputs::index_present` for the graph directory `graph_dir`.
fn index_present_at(graph_dir: &Path) -> bool {
    holds_committed_index(graph_dir)
}

/// Build the graph at `root` with `inputs` and persist it durably as the
/// workspace's index (the snapshot, the analyses and the manifest, which
/// records the roster and the macro build options the graph was built
/// with): the pipeline a `daemon/rebuild` iteration runs, without the
/// dispatcher's cancellation and scheduling. Used by the daemon-hosted
/// `rebuild_index` over a workspace that is not resident, so it records its
/// options exactly as `daemon/rebuild` and the standalone `rebuild_index`
/// do. Runs on the calling (blocking) thread.
///
/// # Errors
///
/// The narrowing guard's refusal (a manifest rewritten during the build),
/// [`DaemonError::WorkspaceBuildFailed`] for a build or a persist failure.
pub(crate) fn build_and_persist_blocking(
    root: &Path,
    inputs: &RebuildInputs,
    build_command: &'static str,
) -> Result<BuiltGraph, DaemonError> {
    // The daemon-hosted `rebuild_index` has no cancellation of its own (it
    // is not a rebuild iteration, so no forwarder watches it): one token
    // for both its builds.
    let cancellation = CancellationToken::new();
    let built = build_unified_graph_with_progress_cancellable(
        root,
        &inputs.roster.plugins,
        &inputs.cfg,
        sqry_core::progress::no_op_reporter(),
        &cancellation,
    )
    .map(|(graph, effective_threads)| RebuildGraphOutput {
        graph,
        effective_threads,
    })
    .map_err(|err| map_graph_builder_err(err, root.to_path_buf(), "full rebuild"))?;
    let durable = persist_rebuild_output_blocking(
        root.to_path_buf(),
        built,
        inputs,
        build_command,
        &cancellation,
        None,
        None,
    )
    .map_err(|err| match err {
        PipelineError::Refused(err) | PipelineError::Failed(err) => err,
    })?;
    Ok(BuiltGraph::new(durable.graph, durable.roster))
}

/// Refuse to persist a rebuild whose roster drops ids the manifest at
/// `root` already records (surface parity W1, design D5), or whose
/// manifest cannot be read (round 2, design D9).
///
/// Runs twice per rebuild iteration: in
/// [`RebuildDispatcher::resolve_rebuild_inputs`], before the reservation,
/// and again before the durable persist. Each run reads the manifest fresh
/// (a concurrent `sqry index` may have rewritten it during the build).
/// When the prior
/// selection names an id `about_to_record` lacks, returns
/// [`DaemonError::RebuildWouldNarrowSelection`] naming the ids and the
/// `sqry index` invocation that restores them. When the manifest exists
/// but cannot be read, returns [`DaemonError::WorkspaceManifestUnreadable`]
/// naming the file and `sqry index --force <root>`: an unreadable manifest
/// carries no recorded selection, so persisting over it would replace an
/// unknown selection with whatever was built, which is the narrowing this
/// guard exists to refuse. The resolver already refuses the same file
/// before the build; this arm is the defence for a manifest that becomes
/// unreadable between the resolve and the persist. Nothing is written on
/// either refusal path. No manifest at all is not a refusal; the guard then
/// compares against the resident generation's record when that came from
/// a manifest (decision D-i7-4).
///
/// `unreadable_policy` is `Refuse` for every rebuild but the daemon-hosted
/// `rebuild_index`, a caller's explicit request to replace the index,
/// which passes `FallBack` (design D9): for it an unreadable manifest
/// records nothing, exactly as a missing one.
///
/// # Errors
///
/// - [`DaemonError::RebuildWouldNarrowSelection`] (JSON-RPC `-32021`).
/// - [`DaemonError::WorkspaceManifestUnreadable`] (JSON-RPC `-32001`),
///   under `Refuse` only.
/// - [`DaemonError::WorkspaceBuildFailed`] (JSON-RPC `-32001`) for any
///   other resolver error, so the persist never proceeds on an unread
///   selection.
pub(crate) fn refuse_if_rebuild_narrows(
    root: &std::path::Path,
    about_to_record: &RosterRecord,
    resident: Option<&RosterRecord>,
    unreadable_policy: UnreadableManifestPolicy,
) -> Result<(), DaemonError> {
    let recorded = match resolve_persisted_selection(root) {
        Err(PluginSelectionError::ManifestUnreadable { .. })
            if unreadable_policy == UnreadableManifestPolicy::FallBack =>
        {
            Ok(None)
        }
        other => other,
    };
    let prior_ids: Vec<String> = match recorded {
        Ok(Some(prior)) => prior.active_plugin_ids,
        // No manifest. A resident graph built from a manifest still records
        // the selection that manifest carried (it was set aside by a crash
        // in the middle of a persist, or removed by hand): the next rebuild
        // would otherwise record the fallback roster in its place, silently
        // narrowing the selection. Compare against that record instead; a
        // resident graph built from no manifest (`Fallback`) has nothing to
        // protect.
        Ok(None) => match resident.filter(|record| record.source != RosterSource::Fallback) {
            Some(record) => record.active_plugin_ids.clone(),
            None => return Ok(()),
        },
        Err(PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        }) => {
            return Err(DaemonError::WorkspaceManifestUnreadable {
                root: root.to_path_buf(),
                manifest_path,
                reason,
            });
        }
        Err(other) => {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!(
                    "recorded plugin selection could not be read before persisting: {other}"
                ),
            });
        }
    };
    let missing_plugin_ids: Vec<String> = prior_ids
        .iter()
        .filter(|id| !about_to_record.contains(id))
        .cloned()
        .collect();
    if missing_plugin_ids.is_empty() {
        return Ok(());
    }
    Err(DaemonError::RebuildWouldNarrowSelection {
        root: root.to_path_buf(),
        restore_command: restore_command(root, &missing_plugin_ids),
        missing_plugin_ids,
    })
}

/// Check a macro build options request on its own, before anything is
/// read, reserved or written (S6, round 7): an empty, blank or padded cfg
/// flag, an empty expand cache directory, or one that cannot be anchored
/// to the root ([`MacroOptionsRequest::validate`]). Refused as
/// [`DaemonError::InvalidArgument`] (`-32602`) in the words the standalone
/// `rebuild_index` uses, "rebuild of <root> refused: <reason>".
///
/// Every daemon site that resolves a request runs it: `daemon/rebuild` and
/// the daemon-hosted `rebuild_index` before they load or enqueue anything,
/// and [`resolve_durable_rebuild_inputs`] and the workspace builder's
/// macro options resolution, so no path reaches a build with a request the
/// check refuses.
///
/// # Errors
///
/// [`DaemonError::InvalidArgument`] naming the root and the check's reason.
pub(crate) fn validate_macro_request(
    root: &Path,
    request: &MacroOptionsRequest,
) -> Result<(), DaemonError> {
    request
        .validate()
        .map_err(|err| DaemonError::InvalidArgument {
            reason: format!("rebuild of {} refused: {err}", root.display()),
        })
}

/// Map a macro-options refusal to the daemon surface type (surface parity
/// W4, W4-D7): a missing expand cache directory is
/// [`DaemonError::RebuildMacroOptionsUnavailable`] (`-32022`); an unreadable
/// manifest is [`DaemonError::WorkspaceManifestUnreadable`] (`-32001`), the
/// same refusal the roster resolver gives for the same file; an expand cache
/// directory whose canonical path is not valid UTF-8 (W4-D11), which the
/// manifest could not record, is [`DaemonError::InvalidArgument`]
/// (`-32602`) carrying the resolver's message, which names the directory and
/// both ways out; and an empty expand cache directory, which names no
/// directory (joined to the root it would name the root itself), is
/// [`DaemonError::InvalidArgument`] (`-32602`) carrying the resolver's
/// message; so is a recorded cfg flag that names no predicate (a hand-edited
/// manifest), whose message names the flag and both ways out. No new wire
/// code: the request (or the record it keeps) names an input the daemon
/// cannot honour, and nothing was written. `request` decides where a
/// refused expand cache directory came from (`ExpandCacheOrigin::of`).
pub(crate) fn map_macro_options_err(
    err: MacroOptionsError,
    root: &Path,
    request: &MacroOptionsRequest,
) -> DaemonError {
    match err {
        MacroOptionsError::ExpandCacheMissing { dir } => {
            DaemonError::RebuildMacroOptionsUnavailable {
                root: root.to_path_buf(),
                expand_cache_dir: dir,
                origin: sqry_mcp::error::ExpandCacheOrigin::of(request),
            }
        }
        unrecordable @ MacroOptionsError::ExpandCachePathNotUtf8 { .. } => {
            DaemonError::InvalidArgument {
                reason: format!("rebuild of {} refused: {unrecordable}", root.display()),
            }
        }
        empty @ MacroOptionsError::ExpandCacheEmpty => DaemonError::InvalidArgument {
            reason: format!("rebuild of {} refused: {empty}", root.display()),
        },
        // A hand-edited record naming a cfg flag no surface would record:
        // the standalone `rebuild_index`'s refusal, in its words.
        recorded @ MacroOptionsError::RecordedCfgFlagInvalid { .. } => {
            DaemonError::InvalidArgument {
                reason: format!("rebuild of {} refused: {recorded}", root.display()),
            }
        }
        MacroOptionsError::ManifestUnreadable {
            manifest_path,
            reason,
        } => DaemonError::WorkspaceManifestUnreadable {
            root: root.to_path_buf(),
            manifest_path,
            reason,
        },
    }
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

/// Panic-safety sentinel for the Phase B drain loop
/// ([`RebuildDispatcher::drain`]).
///
/// Guarantees that [`LoadedWorkspace::rebuild_in_flight`] is released if
/// the drain loop unwinds abnormally. The normal path disarms the
/// sentinel (`armed = false`) after releasing `rebuild_in_flight` under
/// the lane lock; the Drop impl is a no-op on the happy path.
///
/// # On an unwind
///
/// The callers waiting on the entry the unwinding iteration was running
/// hold the receiving halves of senders dropped by the unwind, so each is
/// answered [`DaemonError::Internal`] by [`RebuildOutcome::wait`]. The
/// Drop impl then:
///
/// 1. Takes the lane with `try_lock` (Drop cannot await). When it gets
///    it, it takes the parked entry, if any, and answers its waiters
///    [`DaemonError::Internal`] rather than leave them to their 600 s
///    bound, then releases `rebuild_in_flight` under the lane. The entry
///    is dropped, not left for the next dispatch: its explicit macro
///    options would otherwise run later for callers already told the
///    request failed, and every rebuild builds the complete graph, so
///    a watcher's dropped file list loses nothing the next rebuild does
///    not rebuild.
/// 2. Leaves `Rebuilding` for `Failed` with a recorded failure, so a
///    workspace whose runner unwound mid-iteration is not left
///    `Rebuilding` with no runner (where `daemon/reset` would answer
///    `ResetCancellationDispatched` on every retry).
///
/// # Narrow race on the unwind path
///
/// When `try_lock` fails (a caller holds the lane in Phase A at that
/// instant), the Drop impl stores `rebuild_in_flight = false` without the
/// lane. That caller can then park its request after observing
/// `rebuild_in_flight = true`, and the parked entry sits without a runner
/// until the NEXT dispatch arrives and takes the runner role (or is
/// refused for macro options the parked entry does not share, in which
/// case the parked callers are answered by their own 600 s bound).
///
/// This is accepted as defense-in-depth: the only realistic trigger for
/// a drain-loop unwind is a runtime-level failure (OOM, tokio internal
/// panic) in which case the daemon is already in damage-control
/// territory. Plugin panics during the rebuild pipeline are caught by
/// `spawn_blocking` inside [`RebuildDispatcher::execute_rebuild`] and
/// mapped to [`DaemonError::WorkspaceBuildFailed`], which flows through
/// the drain loop as `last_result`, NOT as an unwind.
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
            "the rebuild drain loop unwound with an armed DrainLoopSentinel: \
             releasing rebuild_in_flight, answering parked requests, leaving Rebuilding"
        );
        let unwound = || {
            DaemonError::Internal(anyhow::anyhow!(
                "the rebuild runner unwound before running this request"
            ))
        };
        if let Ok(mut lane) = self.ws.rebuild_lane.try_lock() {
            if let Some(parked) = lane.take() {
                parked.waiters.deliver(&Err(unwound()));
            }
            self.ws.rebuild_in_flight.store(false, Ordering::Release);
        } else {
            self.ws.rebuild_in_flight.store(false, Ordering::Release);
        }
        if self
            .ws
            .transition_state(WorkspaceState::Rebuilding, WorkspaceState::Failed)
            .is_ok()
        {
            self.ws
                .record_failure(DaemonError::Internal(anyhow::anyhow!(
                    "the rebuild runner unwound mid-iteration"
                )));
        }
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
    // RebuildOutcome, the merge rule, and request normalisation
    // ---------------------------------------------------------------

    /// F5 (P1's class): a sender dropped without a report (the runner
    /// unwound, or dropped the entry) is an error, never `Ok`; a call that
    /// never reached the lane answers with its own error.
    #[tokio::test]
    async fn an_outcome_whose_sender_was_dropped_is_an_error() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        drop(sender);
        let outcome = RebuildOutcome {
            receiver,
            call: Ok(()),
        };
        match outcome.wait().await {
            Err(DaemonError::Internal(reason)) => assert!(
                reason
                    .to_string()
                    .contains("exited before reporting this request's outcome"),
                "{reason}"
            ),
            other => panic!("a dropped sender must be an error, got {other:?}"),
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        drop(sender);
        let refused = RebuildOutcome {
            receiver,
            call: Err(DaemonError::InvalidArgument {
                reason: "a conflicting request is queued".to_string(),
            }),
        };
        assert!(matches!(
            refused.wait().await,
            Err(DaemonError::InvalidArgument { .. })
        ));
    }

    fn entry(macro_request: MacroOptionsRequest, waiter: bool) -> PendingRebuild {
        PendingRebuild {
            changes: ChangeSet {
                changed_files: Vec::new(),
                git_state_changed: false,
                git_change_class: None,
            },
            enqueued_at: Instant::now(),
            git_state_at_enqueue: None,
            macro_request,
            waiters: if waiter {
                RebuildWaiters::one(tokio::sync::oneshot::channel().0)
            } else {
                RebuildWaiters::default()
            },
            requester: RebuildRequester::Watcher,
        }
    }

    fn cfg(flags: &[&str]) -> MacroOptionsRequest {
        MacroOptionsRequest {
            cfg_flags: Some(flags.iter().map(|flag| (*flag).to_string()).collect()),
            ..MacroOptionsRequest::empty()
        }
    }

    /// Rule 8 of `coalesce_with`: the merged requester is the later in
    /// precedence order whichever parked first, so an iteration a watcher
    /// enqueue merged into records the explicit caller's provenance.
    #[test]
    fn coalesce_with_keeps_the_explicit_requester_in_both_orders() {
        let with = |requester| PendingRebuild {
            requester,
            ..entry(MacroOptionsRequest::empty(), false)
        };
        for (earlier, later, merged) in [
            (
                RebuildRequester::Watcher,
                RebuildRequester::RebuildIndex,
                RebuildRequester::RebuildIndex,
            ),
            (
                RebuildRequester::RebuildIndex,
                RebuildRequester::Watcher,
                RebuildRequester::RebuildIndex,
            ),
            (
                RebuildRequester::Watcher,
                RebuildRequester::DaemonRebuild,
                RebuildRequester::DaemonRebuild,
            ),
            (
                RebuildRequester::DaemonRebuild,
                RebuildRequester::RebuildIndex,
                RebuildRequester::RebuildIndexWithDaemonRebuild,
            ),
            (
                RebuildRequester::RebuildIndex,
                RebuildRequester::DaemonRebuild,
                RebuildRequester::RebuildIndexWithDaemonRebuild,
            ),
            (
                RebuildRequester::RebuildIndexWithDaemonRebuild,
                RebuildRequester::RebuildIndex,
                RebuildRequester::RebuildIndexWithDaemonRebuild,
            ),
            (
                RebuildRequester::Watcher,
                RebuildRequester::RebuildIndexWithDaemonRebuild,
                RebuildRequester::RebuildIndexWithDaemonRebuild,
            ),
        ] {
            assert_eq!(
                with(earlier).coalesce_with(with(later)).requester,
                merged,
                "{earlier:?} then {later:?}"
            );
        }
        assert_eq!(
            RebuildRequester::RebuildIndex.build_command(RebuildMode::Incremental),
            "daemon:rebuild_index"
        );
        assert_eq!(
            RebuildRequester::RebuildIndexWithDaemonRebuild.build_command(RebuildMode::Full),
            "daemon:rebuild_index"
        );
        assert!(RebuildRequester::RebuildIndex.falls_back_over_an_unreadable_manifest());
        for refuses in [
            RebuildRequester::Watcher,
            RebuildRequester::DaemonRebuild,
            RebuildRequester::RebuildIndexWithDaemonRebuild,
        ] {
            assert!(
                !refuses.falls_back_over_an_unreadable_manifest(),
                "{refuses:?} refuses an unreadable manifest"
            );
        }
        assert_eq!(
            RebuildRequester::Watcher.build_command(RebuildMode::Incremental),
            "daemon:rebuild:incremental"
        );
    }

    /// Rule 6 of `coalesce_with`, in both orders: an empty later request
    /// keeps the earlier explicit one (a watcher enqueue never erases a
    /// parked caller's options), and an explicit later request replaces an
    /// earlier empty one (it runs with its own options).
    #[test]
    fn coalesce_with_keeps_the_explicit_request_in_both_orders() {
        let explicit = cfg(&["unix"]);
        let merged =
            entry(explicit.clone(), true).coalesce_with(entry(MacroOptionsRequest::empty(), false));
        assert_eq!(
            merged.macro_request, explicit,
            "an empty later request keeps the earlier"
        );
        assert_eq!(merged.waiters.pending(), 1, "the waiter is kept");
        let merged =
            entry(MacroOptionsRequest::empty(), false).coalesce_with(entry(explicit.clone(), true));
        assert_eq!(
            merged.macro_request, explicit,
            "an explicit later request replaces an empty one"
        );
        assert_eq!(merged.waiters.pending(), 1);
    }

    /// The merge matrix (decision D-i7-1), each pair in both orders.
    #[test]
    fn merges_with_accepts_equal_requests_and_watcher_enqueues_only() {
        let watcher = || entry(MacroOptionsRequest::empty(), false);
        let plain = || entry(MacroOptionsRequest::empty(), true);
        let unix = || entry(cfg(&["unix"]), true);
        let unix_without_waiter = || entry(cfg(&["unix"]), false);
        let windows = || entry(cfg(&["windows"]), true);
        let cases: [(&str, PendingRebuild, PendingRebuild, bool); 7] = [
            ("watcher, explicit", watcher(), unix(), true),
            ("watcher, plain", watcher(), plain(), true),
            ("plain, plain", plain(), plain(), true),
            ("explicit, same explicit", unix(), unix(), true),
            ("plain, explicit", plain(), unix(), false),
            ("explicit, other explicit", unix(), windows(), false),
            (
                "explicit without waiters, plain",
                unix_without_waiter(),
                plain(),
                false,
            ),
        ];
        for (label, a, b, expected) in cases {
            assert_eq!(a.merges_with(&b), expected, "{label}");
            assert_eq!(b.merges_with(&a), expected, "{label}, reversed");
        }
    }

    /// Requests are compared by meaning: cfg flags as a set, an absent
    /// component apart from an explicit empty one, `reset` apart.
    #[test]
    fn macro_requests_agree_compares_meaning() {
        assert!(macro_requests_agree(
            &cfg(&["a", "b"]),
            &cfg(&["b", "a", "a"])
        ));
        assert!(!macro_requests_agree(&cfg(&["a"]), &cfg(&["a", "c"])));
        assert!(!macro_requests_agree(
            &cfg(&[]),
            &MacroOptionsRequest::empty()
        ));
        let reset = MacroOptionsRequest {
            reset: true,
            ..MacroOptionsRequest::empty()
        };
        assert!(!macro_requests_agree(&reset, &MacroOptionsRequest::empty()));
        assert!(macro_requests_agree(&reset, &reset.clone()));
    }

    /// Normalisation anchors a plain relative directory to the root and
    /// canonicalises it; it removes `.` components and a trailing separator
    /// from a directory that does not exist; it leaves an empty directory
    /// for the resolver to refuse.
    #[test]
    fn normalized_macro_request_names_one_directory_one_way() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("cachedir")).expect("cache dir");
        let dir = |request: MacroOptionsRequest| {
            normalized_macro_request(&root, request).expand_cache_dir
        };
        let with = |path: &str| MacroOptionsRequest {
            expand_cache_dir: Some(PathBuf::from(path)),
            ..MacroOptionsRequest::empty()
        };
        let canonical = Some(root.join("cachedir"));
        assert_eq!(dir(with("cachedir")), canonical);
        assert_eq!(dir(with("./cachedir/")), canonical);
        assert_eq!(
            dir(with(&root.join("cachedir").to_string_lossy())),
            canonical
        );
        // A symlinked spelling of the directory is the directory (round 7,
        // plant P04: compared unresolved, two requests naming one cache
        // through a link and directly would be refused as differing).
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("cachedir"), root.join("cachelink"))
                .expect("symlink");
            assert_eq!(dir(with("cachelink")), canonical);
        }
        assert_eq!(dir(with("missing/./dir/")), Some(root.join("missing/dir")));
        // `..` is resolved too, for a directory that exists and for one not
        // yet created (round 8, the round 7 review's note: `..` was kept for a
        // missing directory, so two spellings of it were refused `-32602`).
        std::fs::create_dir_all(root.join("sub")).expect("sub dir");
        assert_eq!(dir(with("sub/../cachedir")), canonical);
        assert_eq!(dir(with("missing/../cachedir")), canonical);
        let fresh = Some(root.join("fresh"));
        for spelling in [
            "fresh",
            "./fresh/",
            "sub/../fresh",
            "missing/../fresh",
            "missing/deeper/../../fresh/.",
        ] {
            assert_eq!(dir(with(spelling)), fresh, "{spelling}");
        }
        assert_eq!(
            dir(with(&root.join("missing/../fresh").to_string_lossy())),
            fresh
        );
        assert_ne!(dir(with("missing/../other")), fresh, "another directory");
        // A symlink in the existing prefix resolves as the filesystem will:
        // `elsewhere/../fresh` through a link to `sub/inner` is `sub/fresh`,
        // not the root's `fresh`.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(root.join("sub/inner")).expect("inner dir");
            std::os::unix::fs::symlink(root.join("sub/inner"), root.join("elsewhere"))
                .expect("symlink");
            assert_eq!(
                dir(with("elsewhere/../fresh")),
                Some(root.join("sub/fresh"))
            );
        }
        assert_eq!(dir(with("")), Some(PathBuf::new()));
        assert_eq!(dir(MacroOptionsRequest::empty()), None);
        let flags = normalized_macro_request(&root, cfg(&["b", "a"])).cfg_flags;
        assert_eq!(
            flags,
            Some(vec!["b".to_string(), "a".to_string()]),
            "the flags keep the order given"
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
        let _env = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config = Arc::new(crate::config::DaemonConfig::default());
        let manager = crate::workspace::WorkspaceManager::new_without_reaper(Arc::clone(&config));
        RebuildDispatcher::new(manager, config, Arc::new(WorkspaceRosterResolver::new()))
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
        RebuildDispatcher::record_and_transition_on_err(&ws, &err);
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
        RebuildDispatcher::record_and_transition_on_err(&ws, &err);
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Unloaded,
            "in-iteration reset cancellation must transition Rebuilding → Unloaded; \
             without this, daemon reset → daemon load cannot recover"
        );
    }

    /// Surface parity W1 (D5): a refused narrowing rebuild returns the
    /// workspace to `Loaded` (the prior graph is intact) and records no
    /// failure, so the watcher does not back off and status shows no
    /// error for a rebuild that wrote nothing.
    #[test]
    fn record_and_transition_on_err_restores_loaded_for_narrowing_refusal() {
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(crate::workspace::state::WorkspaceState::Rebuilding);
        let err = DaemonError::RebuildWouldNarrowSelection {
            root: std::path::PathBuf::from("/repo"),
            missing_plugin_ids: vec!["json".to_string()],
            restore_command: "sqry index --force --include-high-cost /repo".to_string(),
        };
        RebuildDispatcher::record_refusal(
            &ws,
            &err,
            crate::workspace::state::WorkspaceState::Loaded,
        );
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Loaded,
            "a refused narrowing rebuild must return the workspace to Loaded"
        );
        assert!(
            ws.last_error.read().is_none(),
            "a refusal that wrote nothing must not be recorded as a failure"
        );
        assert_eq!(ws.retry_count.load(Ordering::Acquire), 0);
    }

    /// Surface parity W4 round 2 (W4-D11): an expand cache directory the
    /// manifest cannot record maps to `InvalidArgument` (`-32602`) naming
    /// the root and the directory, and the refusal returns the workspace to
    /// `Loaded` with no recorded failure, because nothing was written.
    #[cfg(unix)]
    #[test]
    fn an_unrecordable_expand_cache_is_an_invalid_argument_refusal() {
        use std::os::unix::ffi::OsStringExt;

        let mut name = b"/cache/expand-".to_vec();
        name.push(0xff);
        let dir = std::path::PathBuf::from(std::ffi::OsString::from_vec(name));
        let root = std::path::PathBuf::from("/repo");
        let mapped = map_macro_options_err(
            MacroOptionsError::ExpandCachePathNotUtf8 { dir: dir.clone() },
            &root,
            &MacroOptionsRequest::empty(),
        );
        let reason = match &mapped {
            DaemonError::InvalidArgument { reason } => reason.clone(),
            other => panic!("expected InvalidArgument, got {other:?}"),
        };
        println!("{reason}");
        assert_eq!(mapped.jsonrpc_code(), Some(-32602));
        assert!(reason.contains("/repo"), "names the root: {reason}");
        assert!(
            reason.contains(&dir.display().to_string()),
            "names the directory: {reason}"
        );
        assert!(reason.contains("not valid UTF-8"), "{reason}");
        assert!(reason.contains("--no-macro-options"), "{reason}");

        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                root,
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(crate::workspace::state::WorkspaceState::Rebuilding);
        RebuildDispatcher::record_refusal(
            &ws,
            &mapped,
            crate::workspace::state::WorkspaceState::Loaded,
        );
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Loaded,
            "a refusal that wrote nothing returns the workspace to Loaded"
        );
        assert!(
            ws.last_error.read().is_none(),
            "a refusal that wrote nothing must not be recorded as a failure"
        );
        assert_eq!(ws.retry_count.load(Ordering::Acquire), 0);
    }

    /// `refuse_if_rebuild_narrows` is a pure comparison against the
    /// manifest: no manifest or an equal/wider record passes, a narrower
    /// record is refused with the restore command, and nothing is written.
    #[test]
    fn refuse_if_rebuild_narrows_compares_against_the_manifest() {
        use sqry_core::graph::unified::persistence::{
            BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
        };
        use sqry_plugin_registry::RosterSource;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let fast = RosterRecord::fast_path_default();
        assert!(
            refuse_if_rebuild_narrows(tmp.path(), &fast, None, UnreadableManifestPolicy::Refuse)
                .is_ok(),
            "no manifest, nothing to narrow"
        );

        let storage = GraphStorage::new(tmp.path());
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        let mut include_all = fast.active_plugin_ids.clone();
        include_all.push("json".to_string());
        Manifest::new(
            tmp.path().to_string_lossy().to_string(),
            1,
            1,
            "fixture-sha256",
            BuildProvenance::new("test", "test"),
        )
        .with_plugin_selection(Some(PluginSelectionManifest {
            active_plugin_ids: include_all.clone(),
            high_cost_mode: Some("include_all".to_string()),
        }))
        .save(storage.manifest_path())
        .expect("manifest saved");
        let manifest_bytes_before = std::fs::read(storage.manifest_path()).expect("read");

        let err =
            refuse_if_rebuild_narrows(tmp.path(), &fast, None, UnreadableManifestPolicy::Refuse)
                .expect_err("narrower is refused");
        match &err {
            DaemonError::RebuildWouldNarrowSelection {
                root,
                missing_plugin_ids,
                restore_command,
            } => {
                assert_eq!(root, tmp.path());
                assert_eq!(missing_plugin_ids, &vec!["json".to_string()]);
                assert_eq!(
                    restore_command,
                    &format!(
                        "sqry index --force --include-high-cost {}",
                        tmp.path().display()
                    )
                );
            }
            other => panic!("expected RebuildWouldNarrowSelection, got {other:?}"),
        }
        assert_eq!(err.jsonrpc_code(), Some(-32021));
        assert_eq!(
            std::fs::read(storage.manifest_path()).expect("read"),
            manifest_bytes_before,
            "the refusal must write nothing"
        );

        let wide = RosterRecord {
            active_plugin_ids: include_all,
            high_cost_mode: Some("include_all".to_string()),
            source: RosterSource::PersistedManifest,
        };
        assert!(
            refuse_if_rebuild_narrows(tmp.path(), &wide, None, UnreadableManifestPolicy::Refuse)
                .is_ok(),
            "an equal record passes"
        );
    }

    /// T23b (round 2, D9): an unreadable manifest is a refusal, not "no
    /// prior selection". On the pre-change head this returned `Ok(())` and
    /// the persist overwrote the unreadable file with the built roster.
    #[test]
    fn refuse_if_rebuild_narrows_refuses_an_unreadable_manifest() {
        use sqry_core::graph::unified::persistence::GraphStorage;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let storage = GraphStorage::new(tmp.path());
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
        let bytes_before = std::fs::read(storage.manifest_path()).expect("read");

        let fast = RosterRecord::fast_path_default();
        let err =
            refuse_if_rebuild_narrows(tmp.path(), &fast, None, UnreadableManifestPolicy::Refuse)
                .expect_err("an unreadable manifest must be refused, not treated as no prior");
        match &err {
            DaemonError::WorkspaceManifestUnreadable {
                root,
                manifest_path,
                reason,
            } => {
                assert_eq!(root, tmp.path());
                assert_eq!(manifest_path, storage.manifest_path());
                assert!(!reason.is_empty(), "reason carries the parse error");
            }
            other => panic!("expected WorkspaceManifestUnreadable, got {other:?}"),
        }
        assert_eq!(err.jsonrpc_code(), Some(-32001));
        let rendered = err.to_string();
        assert!(
            rendered.contains(&storage.manifest_path().display().to_string())
                && rendered.contains(&format!("sqry index --force {}", tmp.path().display())),
            "the refusal must name the manifest and the repair: {rendered}"
        );
        assert_eq!(
            std::fs::read(storage.manifest_path()).expect("read"),
            bytes_before,
            "the refusal must write nothing"
        );

        // The dispatcher returns the workspace to the state it entered
        // from for this refusal, the same as for a narrowing refusal.
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                tmp.path().to_path_buf(),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(crate::workspace::state::WorkspaceState::Rebuilding);
        RebuildDispatcher::record_refusal(
            &ws,
            &err,
            crate::workspace::state::WorkspaceState::Loaded,
        );
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Loaded,
            "a refusal that wrote nothing returns the workspace to Loaded"
        );
        assert!(ws.last_error.read().is_none());
        assert_eq!(ws.retry_count.load(Ordering::Acquire), 0);
    }

    /// Sanity: any other `DaemonError` (e.g. `WorkspaceOversize`,
    /// `Internal`) → `Failed` regardless of starting state.
    #[test]
    fn record_and_transition_on_err_failed_for_non_eviction_errors() {
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
        RebuildDispatcher::record_and_transition_on_err(&ws, &err);
        assert_eq!(
            ws.load_state(),
            crate::workspace::state::WorkspaceState::Failed,
            "non-eviction errors must transition to Failed"
        );
    }

    /// T57 (surface parity W1 round 7, design D37; R7-0's class at the
    /// one site in this function that had no observation in front of it
    /// at all). A completed eviction owns the state, so the final arm
    /// leaves the tombstone alone.
    ///
    /// This is the worst site of the class before the repair, and the
    /// reason is `WorkspaceState::Failed`: `is_serving` accepts it, so
    /// an unconditional store over a tombstone produced a
    /// stale-servable slot carrying the placeholder, which
    /// `classify_for_serve` answers with an internal error for any
    /// workspace whose `last_good_at` is set, and eviction does not
    /// clear `last_good_at`.
    ///
    /// The diagnostic write stays unconditional by decision (design
    /// D37), so `last_error` is asserted set, not absent.
    ///
    /// Row: C69, the final store reverted to the unconditional
    /// `store_state(WorkspaceState::Failed)`.
    #[test]
    fn record_and_transition_on_err_leaves_a_completed_eviction_evicted() {
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        // The shape eviction leaves: `Evicted` over the placeholder
        // generation, which carries no roster record.
        ws.store_state(crate::workspace::state::WorkspaceState::Evicted);
        assert!(
            ws.roster().is_none(),
            "the placeholder generation carries no roster record"
        );

        let err = DaemonError::WorkspaceBuildFailed {
            root: std::path::PathBuf::from("/repo"),
            reason: "a build that failed after the eviction completed".to_string(),
        };
        RebuildDispatcher::record_and_transition_on_err(&ws, &err);

        let state_after = ws.load_state();
        let roster_after = ws.roster().is_some();
        let error_recorded = ws.last_error.read().is_some();
        let retries = ws.retry_count.load(Ordering::Acquire);
        println!(
            "R7-0 error-path store: state_after={state_after} roster_after={roster_after} \
             error_recorded={error_recorded} retries={retries}"
        );
        assert_eq!(
            state_after,
            crate::workspace::state::WorkspaceState::Evicted,
            "a non-eviction failure must not relabel a completed tombstone as Failed"
        );
        assert!(
            !roster_after,
            "nothing on this path publishes a roster record"
        );
        assert!(
            error_recorded,
            "the diagnostic write is unconditional by decision (design D37)"
        );
        assert_eq!(retries, 1, "record_failure counted exactly one attempt");
    }

    /// T59 (surface parity W1 round 7, design D37; R7-0's class at the
    /// rebuild iteration's entry). A completed tombstone is not a
    /// rebuildable state: the iteration refuses to enter `Rebuilding`
    /// on it, answers `WorkspaceEvicted`, and writes nothing.
    ///
    /// The eviction runs through the production path
    /// ([`WorkspaceManager::evict_for_test`] calls `execute_eviction`,
    /// which holds `workspaces.write()` across
    /// `evict_to_tombstone_locked`), so the slot carries the genuine
    /// post-eviction shape and not a seeded imitation of it: `Evicted`,
    /// the placeholder generation with no roster record, and
    /// `rebuild_cancelled` true. The first `handle_changes` stops at the
    /// cancellation gate and returns. The gate leaves a completed
    /// eviction's flag set (the next load consumes it, and a load racing
    /// the eviction must still see it), so the test clears it the way that
    /// load's gate does; the second call is the measurement, because by
    /// then nothing but the entry store itself stands between the
    /// iteration and a published generation over a tombstone.
    ///
    /// Row: C72, the entry store reverted to the unconditional
    /// `store_state(WorkspaceState::Rebuilding)`.
    #[tokio::test]
    async fn a_rebuild_iteration_refuses_to_enter_rebuilding_on_a_tombstone() {
        use sqry_core::project::ProjectRootMode;

        // A real source root, so that on the pre-change shape the second
        // iteration has something to build and publish rather than
        // failing for an unrelated reason.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("seed.rs"), "pub fn seed() {}\n").expect("seed file");

        let dispatcher = make_dispatcher_for_gate_test();
        let key = WorkspaceKey::new(tmp.path().to_path_buf(), ProjectRootMode::GitRoot, 0x7);
        dispatcher
            .manager
            .insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
        let ws = dispatcher
            .manager
            .lookup(&key)
            .expect("the seeded workspace is resident");
        assert_eq!(
            ws.load_state(),
            WorkspaceState::Loaded,
            "the seeded slot starts Loaded"
        );
        assert!(
            ws.roster().is_some(),
            "the seeded slot starts with a roster record"
        );

        assert!(
            dispatcher.manager.evict_for_test(&key),
            "the resident workspace evicts through the production path"
        );
        assert_eq!(
            ws.load_state(),
            WorkspaceState::Evicted,
            "eviction leaves the tombstone state"
        );
        assert!(
            ws.roster().is_none(),
            "eviction swaps in the placeholder generation, which carries no roster record"
        );
        assert!(
            ws.rebuild_cancelled.load(Ordering::Acquire),
            "eviction flips the cancellation flag"
        );

        let changes = || ChangeSet {
            changed_files: vec![std::path::PathBuf::from("seed.rs")],
            git_state_changed: false,
            git_change_class: None,
        };

        // Iteration one: the top-of-loop gate consumes the flag and
        // returns. This is not the measurement; it is what makes the
        // second call a measurement of the entry store alone.
        let first = dispatcher.handle_changes(&key, changes()).await;
        assert!(
            matches!(first, Err(DaemonError::WorkspaceEvicted { .. })),
            "the top-of-loop gate must answer WorkspaceEvicted: {first:?}"
        );
        let state_between = ws.load_state();
        let flag_between = ws.rebuild_cancelled.load(Ordering::Acquire);
        // What the next load's gate does (`honor_preexisting_cancel` from
        // `Evicted`), so the second call reaches the entry store.
        ws.rebuild_cancelled.store(false, Ordering::Release);

        // Iteration two: the flag is clear and the workspace is still in
        // the map, so the entry store is the only thing left.
        let second = dispatcher.handle_changes(&key, changes()).await;
        let state_after = ws.load_state();
        let roster_after = ws.roster().is_some();
        let nodes_after = ws.graph().node_count();
        println!(
            "R7-0 iteration entry: first={first:?} state_between={state_between} \
             flag_between={flag_between} second={second:?} state_after={state_after} \
             roster_after={roster_after} nodes_after={nodes_after}"
        );
        assert_eq!(
            state_between,
            WorkspaceState::Evicted,
            "the gate must not rewrite the tombstone"
        );
        assert!(
            flag_between,
            "the gate must leave a completed eviction's flag for the next load"
        );
        assert_eq!(
            state_after,
            WorkspaceState::Evicted,
            "a refused iteration must leave the tombstone exactly as eviction left it"
        );
        assert!(
            matches!(second, Err(DaemonError::WorkspaceEvicted { .. })),
            "a rebuild iteration that begins on a tombstone must answer WorkspaceEvicted: {second:?}"
        );
        assert!(
            !roster_after,
            "a refused iteration must not publish a roster record over a tombstone"
        );
        assert_eq!(
            nodes_after, 0,
            "a refused iteration must leave the placeholder generation"
        );
    }

    /// The shape T70 and T71 share (surface parity W1 round 8, design D41):
    /// a workspace stored `Evicted` over the placeholder generation, the one
    /// shape a completed eviction leaves, handed to
    /// `record_and_transition_on_err` with a refusal. Answers whether the
    /// placeholder carried a record before the call, then the state, the
    /// record, whether `last_error` was set and the retry count after it.
    fn refusal_over_a_completed_eviction(
        err: &DaemonError,
    ) -> (bool, WorkspaceState, bool, bool, u32) {
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(WorkspaceState::Evicted);
        let record_before = ws.roster().is_some();
        RebuildDispatcher::record_refusal(&ws, err, WorkspaceState::Loaded);
        (
            record_before,
            ws.load_state(),
            ws.roster().is_some(),
            ws.last_error.read().is_some(),
            ws.retry_count.load(Ordering::Acquire),
        )
    }

    /// T70 (surface parity W1 round 8, design D41; section 2.1.8 row 4).
    /// T57's shape at the narrowing-refusal arm of
    /// `record_and_transition_on_err`. A refused iteration whose eviction
    /// completed before the refusal was recorded finds `Evicted`, not the
    /// `Rebuilding` it installed, and must leave it. The refusal records no
    /// failure, which `record_and_transition_on_err_restores_loaded_for_narrowing_refusal`
    /// already pins for the `Rebuilding` case, so `last_error` stays unset
    /// and the retry count stays 0 here too.
    ///
    /// Row: C84, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Loaded)`. Under it the state
    /// assertion fails with left Loaded right Evicted.
    #[test]
    fn record_and_transition_on_err_narrowing_refusal_leaves_a_completed_eviction_evicted() {
        let err = DaemonError::RebuildWouldNarrowSelection {
            root: std::path::PathBuf::from("/repo"),
            missing_plugin_ids: vec!["json".to_string()],
            restore_command: "sqry index --force --include-high-cost /repo".to_string(),
        };
        let (record_before, state_after, record_after, error_recorded, retries) =
            refusal_over_a_completed_eviction(&err);
        println!(
            "R8-1 narrowing refusal over a tombstone: record_before={record_before} \
             state_after={state_after} record_after={record_after} \
             error_recorded={error_recorded} retries={retries}"
        );
        assert!(
            !record_before,
            "the placeholder generation carries no roster record"
        );
        assert_eq!(
            state_after,
            WorkspaceState::Evicted,
            "a narrowing refusal must not relabel a completed tombstone as Loaded"
        );
        assert!(
            !record_after,
            "nothing on this path publishes a roster record"
        );
        assert!(
            !error_recorded,
            "a refusal that wrote nothing records no failure"
        );
        assert_eq!(retries, 0, "a refusal counts no failed attempt");
    }

    /// T71 (surface parity W1 round 8, design D41; section 2.1.8 row 5).
    /// T70 at the unreadable-manifest arm.
    ///
    /// Row: C85, this arm alone reverted to
    /// `ws.store_state(WorkspaceState::Loaded)`. Under it the state
    /// assertion fails with left Loaded right Evicted.
    #[test]
    fn record_and_transition_on_err_unreadable_manifest_leaves_a_completed_eviction_evicted() {
        let err = DaemonError::WorkspaceManifestUnreadable {
            root: std::path::PathBuf::from("/repo"),
            manifest_path: std::path::PathBuf::from("/repo/.sqry/graph/manifest.json"),
            reason: "expected value at line 1 column 1".to_string(),
        };
        let (record_before, state_after, record_after, error_recorded, retries) =
            refusal_over_a_completed_eviction(&err);
        println!(
            "R8-1 unreadable-manifest refusal over a tombstone: record_before={record_before} \
             state_after={state_after} record_after={record_after} \
             error_recorded={error_recorded} retries={retries}"
        );
        assert!(
            !record_before,
            "the placeholder generation carries no roster record"
        );
        assert_eq!(
            state_after,
            WorkspaceState::Evicted,
            "an unreadable-manifest refusal must not relabel a completed tombstone as Loaded"
        );
        assert!(
            !record_after,
            "nothing on this path publishes a roster record"
        );
        assert!(
            !error_recorded,
            "a refusal that wrote nothing records no failure"
        );
        assert_eq!(retries, 0, "a refusal counts no failed attempt");
    }

    /// T70's shape at W4's macro-options refusal arm (W4-D7). The release
    /// batch of 2026-09-30 merged W4's arm, written with the pre-D37
    /// guard-then-store, beside W1's compare-exchange; the arm now uses
    /// `transition_state` like its siblings.
    ///
    /// A single-threaded test cannot observe the window between a guard and
    /// a store; the compare-exchange closes it by construction (design D37).
    /// What this pins is the tombstone left alone and no failure recorded,
    /// which an unconditional `ws.store_state(WorkspaceState::Loaded)` in
    /// this arm fails with left Loaded right Evicted.
    #[test]
    fn record_and_transition_on_err_macro_options_refusal_leaves_a_completed_eviction_evicted() {
        let err = DaemonError::RebuildMacroOptionsUnavailable {
            root: std::path::PathBuf::from("/repo"),
            expand_cache_dir: std::path::PathBuf::from("/repo/.sqry/expand-cache"),
            origin: sqry_mcp::error::ExpandCacheOrigin::Recorded,
        };
        let (record_before, state_after, record_after, error_recorded, retries) =
            refusal_over_a_completed_eviction(&err);
        assert!(
            !record_before,
            "the placeholder generation carries no roster record"
        );
        assert_eq!(
            state_after,
            WorkspaceState::Evicted,
            "a macro-options refusal must not relabel a completed tombstone as Loaded"
        );
        assert!(
            !record_after,
            "nothing on this path publishes a roster record"
        );
        assert!(
            !error_recorded,
            "a refusal that wrote nothing records no failure"
        );
        assert_eq!(retries, 0, "a refusal counts no failed attempt");
    }

    /// A workspace an iteration put in `Rebuilding`, handed to `record` with
    /// `err`; answers the state, whether `last_error` was set, and the
    /// retry count after it.
    fn refusal_over_a_rebuilding_workspace(
        err: &DaemonError,
        record: impl Fn(&LoadedWorkspace, &DaemonError),
    ) -> (WorkspaceState, bool, u32) {
        let ws = Arc::new(LoadedWorkspace::new(
            crate::workspace::state::WorkspaceKey::new(
                std::path::PathBuf::from("/repo"),
                sqry_core::project::ProjectRootMode::GitRoot,
                0x1,
            ),
            false,
        ));
        ws.store_state(WorkspaceState::Rebuilding);
        record(&ws, err);
        (
            ws.load_state(),
            ws.last_error.read().is_some(),
            ws.retry_count.load(Ordering::Acquire),
        )
    }

    /// The refusal arms the rebuild path's pre-write errors derive (F2): an
    /// uncompiled plugin id the roster resolver refuses, and a budget the
    /// reservation cannot satisfy. Each returns the workspace to `Loaded`
    /// with no recorded failure; and, T57's shape, each leaves a completed
    /// eviction's tombstone alone.
    #[test]
    fn record_and_transition_on_err_incompatible_graph_and_memory_budget_are_refusals() {
        let incompatible = DaemonError::WorkspaceIncompatibleGraph {
            root: std::path::PathBuf::from("/repo"),
            reason: "unknown plugin ids: planted".to_string(),
        };
        let budget = DaemonError::MemoryBudgetExceeded {
            limit_bytes: 1,
            current_bytes: 1,
            reserved_bytes: 0,
            retained_bytes: 0,
            requested_bytes: 2,
        };
        for err in [&incompatible, &budget] {
            let (state, error_recorded, retries) =
                refusal_over_a_rebuilding_workspace(err, |ws, err| {
                    RebuildDispatcher::record_refusal(ws, err, WorkspaceState::Loaded);
                });
            assert_eq!(state, WorkspaceState::Loaded, "{err:?}");
            assert!(!error_recorded, "a refusal records no failure: {err:?}");
            assert_eq!(retries, 0, "a refusal counts no failed attempt: {err:?}");
            let (record_before, state_after, record_after, error_recorded, retries) =
                refusal_over_a_completed_eviction(err);
            assert!(!record_before && !record_after);
            assert_eq!(state_after, WorkspaceState::Evicted, "{err:?}");
            assert!(!error_recorded);
            assert_eq!(retries, 0);
        }
    }

    /// `record_refusal` decides by origin: an error the preflight or the
    /// narrowing guard raised is a refusal whatever its variant. A variant
    /// with no refusal arm (a selection error kind the mapping renders as
    /// `WorkspaceBuildFailed`) still returns the workspace to `Loaded` with
    /// no recorded failure, where `record_and_transition_on_err` would
    /// record it and leave the workspace `Failed`.
    #[test]
    fn record_refusal_returns_loaded_whatever_the_variant() {
        let unnamed = DaemonError::WorkspaceBuildFailed {
            root: std::path::PathBuf::from("/repo"),
            reason: "plugin selection failed: a kind this daemon does not name".to_string(),
        };
        let refuse_from = |entered: WorkspaceState| {
            move |ws: &LoadedWorkspace, err: &DaemonError| {
                RebuildDispatcher::record_refusal(ws, err, entered);
            }
        };
        let (state, error_recorded, retries) =
            refusal_over_a_rebuilding_workspace(&unnamed, refuse_from(WorkspaceState::Loaded));
        assert_eq!(state, WorkspaceState::Loaded);
        assert!(!error_recorded);
        assert_eq!(retries, 0);
        // The control: the same error raised by the build is a failure.
        let (state, error_recorded, retries) = refusal_over_a_rebuilding_workspace(
            &unnamed,
            RebuildDispatcher::record_and_transition_on_err,
        );
        assert_eq!(state, WorkspaceState::Failed);
        assert!(error_recorded);
        assert_eq!(retries, 1);
        // A variant with its own arm takes it through `record_refusal` too.
        let narrowing = DaemonError::RebuildWouldNarrowSelection {
            root: std::path::PathBuf::from("/repo"),
            missing_plugin_ids: vec!["json".to_string()],
            restore_command: "sqry index --force --include-high-cost /repo".to_string(),
        };
        let (state, error_recorded, retries) =
            refusal_over_a_rebuilding_workspace(&narrowing, refuse_from(WorkspaceState::Loaded));
        assert_eq!(state, WorkspaceState::Loaded);
        assert!(!error_recorded);
        assert_eq!(retries, 0);
    }

    /// S3 (integration round 7): a refusal restores the state the iteration
    /// entered from, for every variant. A `Failed` workspace stays `Failed`
    /// with its previous failure and retry count untouched (it must keep
    /// its stale-serve clock), an `Unloaded` one stays `Unloaded`, and only
    /// an iteration that entered `Rebuilding` (no runner had left it) goes
    /// to `Loaded`. Before round 7 every refusal went to `Loaded`.
    #[test]
    fn a_refusal_restores_the_state_the_iteration_entered_from() {
        let refusals = [
            DaemonError::RebuildMacroOptionsUnavailable {
                root: std::path::PathBuf::from("/repo"),
                expand_cache_dir: std::path::PathBuf::from("/repo/cache"),
                origin: sqry_mcp::error::ExpandCacheOrigin::Recorded,
            },
            DaemonError::MemoryBudgetExceeded {
                limit_bytes: 1,
                current_bytes: 1,
                reserved_bytes: 0,
                retained_bytes: 0,
                requested_bytes: 2,
            },
            DaemonError::WorkspaceIncompatibleGraph {
                root: std::path::PathBuf::from("/repo"),
                reason: "unknown plugin ids: planted".to_string(),
            },
            DaemonError::RebuildWouldNarrowSelection {
                root: std::path::PathBuf::from("/repo"),
                missing_plugin_ids: vec!["json".to_string()],
                restore_command: "sqry index --force --include-high-cost /repo".to_string(),
            },
            DaemonError::WorkspaceManifestUnreadable {
                root: std::path::PathBuf::from("/repo"),
                manifest_path: std::path::PathBuf::from("/repo/.sqry/graph/manifest.json"),
                reason: "expected value".to_string(),
            },
            DaemonError::InvalidArgument {
                reason: "an empty expand cache".to_string(),
            },
            DaemonError::WorkspaceBuildFailed {
                root: std::path::PathBuf::from("/repo"),
                reason: "a selection error kind with no arm".to_string(),
            },
        ];
        for err in &refusals {
            for (entered, expected) in [
                (WorkspaceState::Loaded, WorkspaceState::Loaded),
                (WorkspaceState::Failed, WorkspaceState::Failed),
                (WorkspaceState::Unloaded, WorkspaceState::Unloaded),
                (WorkspaceState::Rebuilding, WorkspaceState::Loaded),
            ] {
                let ws = make_sentinel_workspace();
                let earlier = DaemonError::WorkspaceBuildFailed {
                    root: std::path::PathBuf::from("/repo"),
                    reason: "the failure that left it Failed".to_string(),
                };
                if entered == WorkspaceState::Failed {
                    ws.record_failure(earlier);
                }
                let retries_before = ws.retry_count.load(Ordering::Acquire);
                ws.store_state(WorkspaceState::Rebuilding);
                RebuildDispatcher::record_refusal(&ws, err, entered);
                assert_eq!(ws.load_state(), expected, "{err:?} entered {entered:?}");
                assert_eq!(
                    ws.retry_count.load(Ordering::Acquire),
                    retries_before,
                    "a refusal counts no attempt: {err:?} entered {entered:?}"
                );
                assert_eq!(
                    ws.last_error.read().is_some(),
                    entered == WorkspaceState::Failed,
                    "a refusal records nothing and keeps an earlier failure: {err:?}"
                );
            }
        }
    }

    /// P22 (integration round 7): only `record_refusal` treats an error as
    /// a refusal. `record_and_transition_on_err` records every error it is
    /// given as a failure, whatever its variant, so a refusal routed to it
    /// by mistake (the persist-time narrowing refusal) shows as `Failed`
    /// with a recorded failure; the cancellation is its one other arm.
    #[test]
    fn record_and_transition_on_err_records_a_refusal_variant_as_a_failure() {
        let narrowing = DaemonError::RebuildWouldNarrowSelection {
            root: std::path::PathBuf::from("/repo"),
            missing_plugin_ids: vec!["json".to_string()],
            restore_command: "sqry index --force --include-high-cost /repo".to_string(),
        };
        let (state, error_recorded, retries) = refusal_over_a_rebuilding_workspace(
            &narrowing,
            RebuildDispatcher::record_and_transition_on_err,
        );
        assert_eq!(state, WorkspaceState::Failed);
        assert!(error_recorded);
        assert_eq!(retries, 1);
    }

    /// The same at W4's invalid-argument refusal arm (W4-D11).
    #[test]
    fn record_and_transition_on_err_invalid_argument_refusal_leaves_a_completed_eviction_evicted() {
        let err = DaemonError::InvalidArgument {
            reason: "the expand cache directory cannot be recorded".to_string(),
        };
        let (record_before, state_after, record_after, error_recorded, retries) =
            refusal_over_a_completed_eviction(&err);
        assert!(
            !record_before,
            "the placeholder generation carries no roster record"
        );
        assert_eq!(
            state_after,
            WorkspaceState::Evicted,
            "an invalid-argument refusal must not relabel a completed tombstone as Loaded"
        );
        assert!(
            !record_after,
            "nothing on this path publishes a roster record"
        );
        assert!(
            !error_recorded,
            "a refusal that wrote nothing records no failure"
        );
        assert_eq!(retries, 0, "a refusal counts no failed attempt");
    }

    /// The "second persist writer" (audit R7 notes): two daemon persists of
    /// one root do not interleave. The first is held inside the root's
    /// persist lock; the second, which has arrived at the lock, does not get
    /// in until the first is released. Without the lock the second is
    /// inside at once, beside the first.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn two_persists_of_one_root_do_not_interleave() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let inputs = || {
            resolve_durable_rebuild_inputs(
                &roster,
                &BuildConfig::default(),
                &root,
                &MacroOptionsRequest::empty(),
                None,
                UnreadableManifestPolicy::Refuse,
            )
            .expect("inputs resolve")
        };
        let (first_inputs, second_inputs) = (inputs(), inputs());

        let arrived = Arc::new(AtomicUsize::new(0));
        let inside = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(parking_lot::Mutex::new(release_rx));
        {
            let arrived = Arc::clone(&arrived);
            let inside = Arc::clone(&inside);
            let release_rx = Arc::clone(&release_rx);
            persist_plant::install(
                &root,
                Arc::new(move |phase| match phase {
                    persist_plant::Phase::Arrived => {
                        arrived.fetch_add(1, Ordering::AcqRel);
                    }
                    persist_plant::Phase::Inside => {
                        // The first persist inside waits for the release.
                        if inside.fetch_add(1, Ordering::AcqRel) == 0 {
                            let _ = release_rx.lock().recv();
                        }
                    }
                    persist_plant::Phase::BeforeTransaction => {}
                }),
            );
        }
        let first = {
            let root = root.clone();
            std::thread::spawn(move || build_and_persist_blocking(&root, &first_inputs, "t:first"))
        };
        let wait = |what: &AtomicUsize, at_least: usize, within: Duration| {
            let deadline = Instant::now() + within;
            while what.load(Ordering::Acquire) < at_least {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            true
        };
        assert!(
            wait(&inside, 1, Duration::from_secs(60)),
            "the first persist gets in"
        );
        let second = {
            let root = root.clone();
            std::thread::spawn(move || {
                build_and_persist_blocking(&root, &second_inputs, "t:second")
            })
        };
        assert!(
            wait(&arrived, 2, Duration::from_secs(60)),
            "the second persist arrives"
        );
        let second_got_in = wait(&inside, 2, Duration::from_secs(1));
        release_tx.send(()).expect("release the first");
        let first = first.join().expect("first joins");
        let second = second.join().expect("second joins");
        persist_plant::clear();
        println!(
            "second persist writer: the second got in while the first was inside={second_got_in}; \
             first ok={} second ok={}",
            first.is_ok(),
            second.is_ok()
        );
        assert!(
            !second_got_in,
            "a second persist of the same root must wait for the first"
        );
        assert!(first.is_ok() && second.is_ok());
        assert_eq!(
            inside.load(Ordering::Acquire),
            2,
            "both persists ran, in turn"
        );
    }

    /// D-i8-5: a record another writer publishes after this rebuild
    /// resolved its inputs and before its persist takes the lock is the
    /// record the persist publishes. The rebuild resolved no macro options;
    /// the other writer records `cfg=test` while the rebuild arrives at the
    /// lock; the persist resolves again under the lock, builds again with
    /// `cfg=test` and records it. Before the fix it recorded its stale
    /// inputs over the other writer's record, silently.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn a_record_published_during_the_build_is_the_one_the_persist_records() {
        use sqry_core::graph::unified::build::{
            MacroBuildOptions, build_and_persist_graph_with_progress,
        };
        use sqry_core::graph::unified::persistence::GraphStorage;
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(
            root.join("lib.rs"),
            b"pub fn a() -> u32 { b() }\npub fn b() -> u32 { 1 }\n",
        )
        .expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        assert!(
            inputs.cfg.macro_options.cfg_flags.is_empty(),
            "precondition: no record, no macro options"
        );
        let published = Arc::new(AtomicBool::new(false));
        {
            let published = Arc::clone(&published);
            let other_root = root.clone();
            let plugins = Arc::clone(&inputs.roster.plugins);
            let selection = inputs.roster.record.selection_manifest();
            persist_plant::install(
                &root,
                Arc::new(move |phase| {
                    if phase == persist_plant::Phase::Arrived
                        && !published.swap(true, Ordering::AcqRel)
                    {
                        // Another writer (a `sqry index --cfg test`) publishes
                        // while this persist arrives at the lock.
                        let config = BuildConfig {
                            macro_options: MacroBuildOptions {
                                cfg_flags: vec!["test".to_string()],
                                ..MacroBuildOptions::default()
                            },
                            ..BuildConfig::default()
                        };
                        build_and_persist_graph_with_progress(
                            &other_root,
                            &plugins,
                            &config,
                            "t:other-writer",
                            Some(selection.clone()),
                            sqry_core::progress::no_op_reporter(),
                        )
                        .expect("the other writer publishes");
                    }
                }),
            );
        }
        let built = build_and_persist_blocking(&root, &inputs, "t:stale-inputs");
        persist_plant::clear();
        built.expect("the rebuild persists");
        assert!(published.load(Ordering::Acquire), "the other writer ran");
        let manifest = GraphStorage::new(&root).load_manifest().expect("manifest");
        assert_eq!(manifest.build_provenance.build_command, "t:stale-inputs");
        assert_eq!(
            manifest.macro_options.map(|recorded| recorded.cfg_flags),
            Some(vec!["test".to_string()]),
            "the persist recorded its stale inputs over the record another writer published"
        );
    }

    /// D-i8-5, the roster half: a roster another writer records while this
    /// rebuild arrives at the lock (`json`, from an include-all index) is
    /// the roster the persist records. The rebuild resolved the fast-path
    /// default; under the lock it resolves the record again and builds with
    /// it. Without the rebuild under the lock it recorded the stale roster
    /// over the other writer's, silently.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn a_roster_published_during_the_build_is_the_one_the_persist_records() {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_plugin_registry::{
            HighCostMode, PluginSelectionConfig, build_and_persist_with_workspace_roster,
        };
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        std::fs::write(root.join("config.json"), br#"{"name": "fixture"}"#).expect("json");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        let records_json = |ids: &[String]| ids.iter().any(|id| id == "json");
        assert!(
            !records_json(&inputs.roster.record.selection_manifest().active_plugin_ids),
            "precondition: the default roster has no json"
        );
        let published = Arc::new(AtomicBool::new(false));
        {
            let published = Arc::clone(&published);
            let other_root = root.clone();
            persist_plant::install(
                &root,
                Arc::new(move |phase| {
                    if phase == persist_plant::Phase::Arrived
                        && !published.swap(true, Ordering::AcqRel)
                    {
                        // Another writer (a `sqry index --include-high-cost`)
                        // publishes while this persist arrives at the lock.
                        build_and_persist_with_workspace_roster(
                            &other_root,
                            &PluginSelectionConfig {
                                high_cost_mode: HighCostMode::IncludeAll,
                                ..PluginSelectionConfig::default()
                            },
                            UnreadableManifestPolicy::Refuse,
                            "t:other-writer",
                            &BuildConfig::default(),
                            &MacroOptionsRequest::empty(),
                            sqry_core::progress::no_op_reporter(),
                        )
                        .expect("the other writer publishes");
                    }
                }),
            );
        }
        let built = build_and_persist_blocking(&root, &inputs, "t:stale-roster");
        persist_plant::clear();
        assert!(published.load(Ordering::Acquire), "the other writer ran");
        built.expect("the rebuild persists with the roster recorded now");
        let manifest = GraphStorage::new(&root).load_manifest().expect("manifest");
        assert_eq!(manifest.build_provenance.build_command, "t:stale-roster");
        assert!(
            manifest
                .plugin_selection
                .is_some_and(|selection| records_json(&selection.active_plugin_ids)),
            "the persist dropped the json plugin another writer recorded"
        );
    }

    /// Round-nine verification: an index removed after the persist took
    /// the lock and before it resolves its inputs again under it is
    /// reported as removed, not as the refusal the vanished record causes.
    /// The workspace is indexed with `--include-high-cost` (`json`
    /// recorded) and the rebuild's resident record carries it; the plant
    /// removes `.sqry` at the `Inside` point. Re-resolution then finds no
    /// manifest, falls back to the default roster without `json`, and the
    /// narrowing guard would refuse it as `RebuildWouldNarrowSelection`
    /// (`-32021`); the persist must answer `INDEX_REMOVED_DURING_PERSIST`
    /// (`-32001`, only ever a refusal) and write nothing.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn an_index_removed_before_re_resolution_is_reported_as_removed() {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_plugin_registry::{
            HighCostMode, PluginSelectionConfig, build_and_persist_with_workspace_roster,
        };

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        std::fs::write(root.join("config.json"), br#"{"name": "fixture"}"#).expect("json");
        build_and_persist_with_workspace_roster(
            &root,
            &PluginSelectionConfig {
                high_cost_mode: HighCostMode::IncludeAll,
                ..PluginSelectionConfig::default()
            },
            UnreadableManifestPolicy::Refuse,
            "t:indexed",
            &BuildConfig::default(),
            &MacroOptionsRequest::empty(),
            sqry_core::progress::no_op_reporter(),
        )
        .expect("the high-cost index publishes");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let recorded = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        assert!(
            recorded
                .roster
                .record
                .selection_manifest()
                .active_plugin_ids
                .iter()
                .any(|id| id == "json"),
            "precondition: the record carries json"
        );
        // The resident record is the recorded one, as for a loaded
        // workspace whose roster came from its manifest.
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            Some(Arc::clone(&recorded.roster.record)),
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve with the resident record");
        let sqry_dir = root.join(".sqry");
        {
            let sqry_dir = sqry_dir.clone();
            persist_plant::install(
                &root,
                Arc::new(move |phase| {
                    if phase == persist_plant::Phase::Inside {
                        let _ = std::fs::remove_dir_all(&sqry_dir);
                    }
                }),
            );
        }
        let built = build_and_persist_blocking(&root, &inputs, "t:removed-inside");
        persist_plant::clear();
        let storage = GraphStorage::new(&root);
        println!(
            "index removed before re-resolution: result={:?} sqry_dir={}",
            built.as_ref().map(|_| ()),
            sqry_dir.exists()
        );
        match &built {
            Err(err @ DaemonError::WorkspaceBuildFailed { reason, .. }) => {
                assert_eq!(reason, INDEX_REMOVED_DURING_PERSIST);
                assert_eq!(err.jsonrpc_code(), Some(-32001));
            }
            Err(other) => panic!("expected the removed-index refusal, got {other:?}"),
            Ok(_) => panic!("the persist over a removed index succeeded"),
        }
        assert!(
            !sqry_dir.exists(),
            "the persist recreated the removed index"
        );
        assert!(!storage.manifest_path().exists(), "a manifest was written");
    }

    /// Remove every entry of `graph_dir` but the persist lock file, as an
    /// `rm -rf .sqry` that has unlinked the index content and not yet the
    /// lock file does.
    fn remove_all_but_the_lock_file(graph_dir: &std::path::Path) {
        use sqry_core::graph::unified::persistence::PERSIST_LOCK_FILE_NAME;
        for entry in std::fs::read_dir(graph_dir).expect("graph dir").flatten() {
            if entry.file_name() == PERSIST_LOCK_FILE_NAME {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Assert `built` is the removed-index refusal and no manifest or
    /// snapshot was written.
    fn assert_removed_under_the_lock(
        built: &Result<BuiltGraph, DaemonError>,
        root: &std::path::Path,
        case: &str,
    ) {
        use sqry_core::graph::unified::persistence::GraphStorage;
        let storage = GraphStorage::new(root);
        println!(
            "{case}: result={:?} manifest={} snapshot={}",
            built.as_ref().map(|_| ()),
            storage.manifest_path().exists(),
            storage.snapshot_path().exists()
        );
        match built {
            Err(err @ DaemonError::WorkspaceBuildFailed { reason, .. }) => {
                assert_eq!(reason, INDEX_REMOVED_DURING_PERSIST, "{case}");
                assert_eq!(err.jsonrpc_code(), Some(-32001), "{case}");
            }
            Err(other) => panic!("{case}: expected the removed-index refusal, got {other:?}"),
            Ok(_) => panic!("{case}: the persist over a removed index succeeded"),
        }
        assert!(
            !storage.manifest_path().exists(),
            "{case}: a manifest was written"
        );
        assert!(
            !storage.snapshot_path().exists(),
            "{case}: a snapshot was written"
        );
    }

    /// Round-nine verification (the verifier's probe): an index whose
    /// content (manifest, snapshot, everything but the lock file) is
    /// removed after the persist took the lock, as an `rm -rf .sqry` in
    /// progress leaves it, is reported as removed even though the lock
    /// file, and so the hold, is still current. A high-cost record and a
    /// resident record carrying `json`: re-resolution would refuse as
    /// `RebuildWouldNarrowSelection` (`-32021`).
    #[test]
    #[serial_test::serial(persist_plant)]
    fn index_content_removed_under_the_lock_is_reported_as_removed() {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_plugin_registry::{
            HighCostMode, PluginSelectionConfig, build_and_persist_with_workspace_roster,
        };

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        std::fs::write(root.join("config.json"), br#"{"name": "fixture"}"#).expect("json");
        build_and_persist_with_workspace_roster(
            &root,
            &PluginSelectionConfig {
                high_cost_mode: HighCostMode::IncludeAll,
                ..PluginSelectionConfig::default()
            },
            UnreadableManifestPolicy::Refuse,
            "t:indexed",
            &BuildConfig::default(),
            &MacroOptionsRequest::empty(),
            sqry_core::progress::no_op_reporter(),
        )
        .expect("the high-cost index publishes");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let recorded = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            Some(Arc::clone(&recorded.roster.record)),
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve with the resident record");
        let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
        persist_plant::install(
            &root,
            Arc::new(move |phase| {
                if phase == persist_plant::Phase::Inside {
                    remove_all_but_the_lock_file(&graph_dir);
                }
            }),
        );
        let built = build_and_persist_blocking(&root, &inputs, "t:content-removed-inside");
        persist_plant::clear();
        assert_removed_under_the_lock(&built, &root, "content removed at Inside");
    }

    /// Round-nine verification: the last check before the transaction
    /// catches index content removed with the lock file kept, on a
    /// rebuild that would not otherwise refuse (default roster, no
    /// resident record). Without it the transaction re-enters the current
    /// hold and writes a manifest and a snapshot into the directory being
    /// removed.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn index_content_removed_before_the_transaction_is_reported_as_removed() {
        use sqry_core::graph::unified::persistence::GraphStorage;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let first = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        build_and_persist_blocking(&root, &first, "t:indexed").expect("the first index");
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve over the index");
        assert!(inputs.index_present, "precondition: the index has content");
        let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
        persist_plant::install(
            &root,
            Arc::new(move |phase| {
                if phase == persist_plant::Phase::BeforeTransaction {
                    remove_all_but_the_lock_file(&graph_dir);
                }
            }),
        );
        let built = build_and_persist_blocking(&root, &inputs, "t:content-removed-late");
        persist_plant::clear();
        assert_removed_under_the_lock(&built, &root, "content removed before the transaction");
    }

    /// Round-nine verification (the verifier's probe for the content
    /// clause): a first persist of an unindexed root that was killed after
    /// writing its begun marker and a partial snapshot leaves a rollback
    /// set. Its hold's recovery discards the set (the marker says no
    /// snapshot existed) and nothing is left. That is a first persist, not
    /// a removal: it must persist the index. When the set counted as an
    /// index at resolution (content, round nine), the check under the lock
    /// read the cleared set as a removed index and refused; it is no
    /// committed index now (`holds_committed_index`, round ten).
    #[test]
    fn a_first_persist_after_a_killed_first_persist_is_not_a_removal() {
        use sqry_core::graph::unified::persistence::{GraphStorage, PERSIST_LOCK_FILE_NAME};

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let mut inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        // Another writer's first persist starts after `roster.resolve` read
        // no manifest and before `index_present` is taken: its begun marker
        // and a partial snapshot are what the resolution sees. It is then
        // killed, leaving them and its lock file. (A reader's recovery
        // during the resolution would otherwise clear them first, so the
        // files are placed after it and `index_present` set as that
        // resolution computes it over them.)
        let storage = GraphStorage::new(&root);
        let graph_dir = storage.graph_dir().to_path_buf();
        std::fs::create_dir_all(&graph_dir).expect("graph dir");
        std::fs::write(graph_dir.join(PERSIST_LOCK_FILE_NAME), b"").expect("lock file");
        std::fs::write(
            graph_dir.join(".txn-begun.rollback-1-1-1"),
            b"snapshot_existed=0\n",
        )
        .expect("begun marker");
        std::fs::write(storage.snapshot_path(), b"partial").expect("partial snapshot");
        inputs.index_present = index_present_at(&graph_dir);
        assert!(
            !inputs.index_present,
            "precondition: a killed first persist's set is no committed index"
        );
        let built = build_and_persist_blocking(&root, &inputs, "t:after-killed-first");
        println!(
            "after a killed first persist: result={:?} manifest={}",
            built.as_ref().map(|_| ()),
            storage.manifest_path().exists()
        );
        built.expect("the persist is a first index, not a removal");
        assert!(storage.manifest_path().exists(), "no manifest was written");
    }

    /// Round-nine verification: the content clause is armed by the index
    /// the hold saw, not only by the input resolution. The root has no
    /// index at resolution (the creating wait); another writer publishes
    /// one as this persist arrives at the lock, so the hold sees a
    /// committed index (`saw_committed_index`); its content is then
    /// removed, lock file kept, at `Inside`. The persist reports the
    /// removal. Armed from `index_present` (false here) alone, it wrote
    /// into the directory being removed.
    #[test]
    #[serial_test::serial(persist_plant)]
    fn content_gained_before_the_hold_and_removed_under_it_is_reported_as_removed() {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_plugin_registry::{
            PluginSelectionConfig, build_and_persist_with_workspace_roster,
        };
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        assert!(
            !inputs.index_present,
            "precondition: no index at resolution"
        );
        let published = Arc::new(AtomicBool::new(false));
        {
            let published = Arc::clone(&published);
            let other_root = root.clone();
            let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
            persist_plant::install(
                &root,
                Arc::new(move |phase| match phase {
                    persist_plant::Phase::Arrived if !published.swap(true, Ordering::AcqRel) => {
                        build_and_persist_with_workspace_roster(
                            &other_root,
                            &PluginSelectionConfig::default(),
                            UnreadableManifestPolicy::Refuse,
                            "t:other-writer",
                            &BuildConfig::default(),
                            &MacroOptionsRequest::empty(),
                            sqry_core::progress::no_op_reporter(),
                        )
                        .expect("the other writer publishes");
                    }
                    persist_plant::Phase::Inside => remove_all_but_the_lock_file(&graph_dir),
                    _ => {}
                }),
            );
        }
        let built = build_and_persist_blocking(&root, &inputs, "t:gained-then-removed");
        persist_plant::clear();
        assert!(published.load(Ordering::Acquire), "the other writer ran");
        assert_removed_under_the_lock(&built, &root, "content gained, then removed");
    }

    /// Whether some thread is blocked on the persist lock of `graph_dir`,
    /// by the kernel's own record (`/proc/locks`, Linux only): a `->`
    /// waiter line naming the lock file's device (major and minor, hex)
    /// and inode. The device is the one `stat` reports; on a filesystem
    /// whose `stat` device differs from the superblock's (btrfs
    /// subvolumes) nothing matches and the caller's deadline fails.
    #[cfg(target_os = "linux")]
    fn someone_waits_on_the_lock(graph_dir: &std::path::Path) -> bool {
        use sqry_core::graph::unified::persistence::PERSIST_LOCK_FILE_NAME;
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = std::fs::metadata(graph_dir.join(PERSIST_LOCK_FILE_NAME)) else {
            return false;
        };
        let dev = metadata.dev();
        let wanted = (
            ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000),
            (dev & 0xff) | ((dev >> 12) & 0xffff_ff00),
            metadata.ino(),
        );
        let locks = std::fs::read_to_string("/proc/locks").unwrap_or_default();
        locks.lines().any(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            fields
                .iter()
                .position(|field| *field == "->")
                .and_then(|at| fields.get(at + 5))
                .and_then(|id| {
                    let mut parts = id.split(':');
                    let major = u64::from_str_radix(parts.next()?, 16).ok()?;
                    let minor = u64::from_str_radix(parts.next()?, 16).ok()?;
                    let ino = parts.next()?.parse::<u64>().ok()?;
                    parts.next().is_none().then_some((major, minor, ino))
                })
                .is_some_and(|waiter| waiter == wanted)
        })
    }

    /// The other holders a contended-wait test started. `join` (after the
    /// persist returned) tells each to stop waiting for the persist to
    /// block, waits for each and re-raises its panic, so an assertion that
    /// fails on one of them (its deadline) fails the test.
    #[cfg(target_os = "linux")]
    #[derive(Clone, Default)]
    struct Contenders(Arc<ContendersInner>);

    #[cfg(target_os = "linux")]
    #[derive(Default)]
    struct ContendersInner {
        handles: parking_lot::Mutex<Vec<std::thread::JoinHandle<()>>>,
        /// The persist returned: a holder still waiting for it to block
        /// stops (it answered without waiting).
        finished: AtomicBool,
        /// Holders that saw the persist blocked on the lock.
        waited: AtomicUsize,
    }

    #[cfg(target_os = "linux")]
    impl Contenders {
        fn push(&self, handle: std::thread::JoinHandle<()>) {
            self.0.handles.lock().push(handle);
        }

        /// Wait until a thread is blocked on the persist lock of
        /// `graph_dir` (`true`), or until the persist returned (`false`).
        /// Panics after 60 s.
        fn wait_for_the_persist(&self, graph_dir: &std::path::Path) -> bool {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                if someone_waits_on_the_lock(graph_dir) {
                    self.0.waited.fetch_add(1, Ordering::AcqRel);
                    return true;
                }
                if self.0.finished.load(Ordering::Acquire) {
                    return false;
                }
                assert!(Instant::now() < deadline, "the persist never waited");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Join every holder started; `expected` is how many must have run.
        /// Answers how many saw the persist blocked on the lock.
        fn join(&self, expected: usize) -> usize {
            self.0.finished.store(true, Ordering::Release);
            let handles = std::mem::take(&mut *self.0.handles.lock());
            let started = handles.len();
            for handle in handles {
                if let Err(panic) = handle.join() {
                    std::panic::resume_unwind(panic);
                }
            }
            assert_eq!(started, expected, "other holders started");
            self.0.waited.load(Ordering::Acquire)
        }
    }

    /// Start another holder of the persist lock of `graph_dir`: it takes
    /// the lock (`IndexWriteLock::acquire`, whose hold recovers), runs
    /// `on_hold`, and once this returns it waits until a thread is blocked
    /// on the lock and runs `while_held` (skipped when the persist
    /// returned without blocking), and releases.
    #[cfg(target_os = "linux")]
    fn spawn_contender(
        graph_dir: std::path::PathBuf,
        on_hold: impl FnOnce(&std::path::Path) + Send + 'static,
        while_held: impl FnOnce(&std::path::Path) + Send + 'static,
        contenders: &Contenders,
    ) {
        use sqry_core::graph::unified::persistence::IndexWriteLock;
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let waiter = contenders.clone();
        contenders.push(std::thread::spawn(move || {
            let held = IndexWriteLock::acquire(&graph_dir).expect("the other holder");
            on_hold(&graph_dir);
            held_tx.send(()).expect("report the hold");
            if waiter.wait_for_the_persist(&graph_dir) {
                while_held(&graph_dir);
            }
            drop(held);
        }));
        // An `Err` here means the holder panicked first; `join` reports it.
        let _ = held_rx.recv();
    }

    /// Install a plant that, as the persist arrives at the lock, starts
    /// another holder of the lock ([`spawn_contender`]). The persist's
    /// wait is thus contended, and `while_held` runs after the wait opened
    /// its file. The caller joins the returned holders after the persist.
    #[cfg(target_os = "linux")]
    fn contend_the_wait(
        root: &std::path::Path,
        on_hold: impl Fn(&std::path::Path) + Send + Sync + 'static,
        while_held: impl Fn(&std::path::Path) + Send + Sync + 'static,
    ) -> Contenders {
        use sqry_core::graph::unified::persistence::GraphStorage;
        let graph_dir = GraphStorage::new(root).graph_dir().to_path_buf();
        let on_hold = Arc::new(on_hold);
        let while_held = Arc::new(while_held);
        let contenders = Contenders::default();
        {
            let contenders = contenders.clone();
            persist_plant::install(
                root,
                Arc::new(move |phase| {
                    if phase != persist_plant::Phase::Arrived {
                        return;
                    }
                    let on_hold = Arc::clone(&on_hold);
                    let while_held = Arc::clone(&while_held);
                    spawn_contender(
                        graph_dir.clone(),
                        move |dir| on_hold(dir),
                        move |dir| while_held(dir),
                        &contenders,
                    );
                }),
            );
        }
        contenders
    }

    /// Round-nine verification (the verifier's regression probe): an
    /// index whose content is removed, lock file kept, while this persist
    /// waits for another holder of the lock is reported as removed when
    /// the lock arrives. Armed from the content seen under the hold alone,
    /// the persist found none, checked only the hold, and published.
    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial(persist_plant)]
    fn content_removed_during_a_contended_wait_is_reported_as_removed() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let resolve = || {
            resolve_durable_rebuild_inputs(
                &roster,
                &BuildConfig::default(),
                &root,
                &MacroOptionsRequest::empty(),
                None,
                UnreadableManifestPolicy::Refuse,
            )
            .expect("inputs resolve")
        };
        build_and_persist_blocking(&root, &resolve(), "t:indexed").expect("the first index");
        let inputs = resolve();
        assert!(inputs.index_present, "precondition: the index has content");
        let contenders = contend_the_wait(&root, |_| {}, remove_all_but_the_lock_file);
        let built = build_and_persist_blocking(&root, &inputs, "t:contended-removal");
        persist_plant::clear();
        assert_eq!(contenders.join(1), 1, "the persist's wait was contended");
        assert_removed_under_the_lock(&built, &root, "content removed during a contended wait");
    }

    /// Round-nine verification: the contended form of the killed first
    /// persist. A leftover set is there at resolution and another holder
    /// keeps the lock while this persist waits, then is killed with a new
    /// one left; this hold's recovery clears it, which is not a removal:
    /// the persist publishes a first index.
    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial(persist_plant)]
    fn a_contended_first_persist_after_a_killed_first_persist_is_not_a_removal() {
        use sqry_core::graph::unified::persistence::GraphStorage;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let mut inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        // The other writer's first persist was under way when this rebuild
        // resolved its inputs: `index_present` is computed over its set as
        // the resolution computes it (the other holder's hold recovers this
        // set, and it leaves another).
        let storage = GraphStorage::new(&root);
        let graph_dir = storage.graph_dir().to_path_buf();
        std::fs::create_dir_all(&graph_dir).expect("graph dir");
        std::fs::write(
            graph_dir.join(".txn-begun.rollback-1-1-1"),
            b"begun snapshot_existed=0 manifest_existed=0\n",
        )
        .expect("begun marker");
        std::fs::write(storage.snapshot_path(), b"partial").expect("partial snapshot");
        inputs.index_present = index_present_at(&graph_dir);
        assert!(
            !inputs.index_present,
            "precondition: a first persist's set is no committed index"
        );
        // The other holder begins a first persist (its begun marker and a
        // partial snapshot) and is then killed: it releases with the set
        // left behind.
        let contenders = contend_the_wait(
            &root,
            |graph_dir| {
                std::fs::write(
                    graph_dir.join(".txn-begun.rollback-1-1-1"),
                    b"begun snapshot_existed=0 manifest_existed=0\n",
                )
                .expect("begun marker");
                std::fs::write(graph_dir.join("snapshot.sqry"), b"partial")
                    .expect("partial snapshot");
            },
            |_| {},
        );
        let built = build_and_persist_blocking(&root, &inputs, "t:contended-after-killed");
        persist_plant::clear();
        assert_eq!(contenders.join(1), 1, "the persist's wait was contended");
        println!(
            "contended after a killed first persist: result={:?} manifest={}",
            built.as_ref().map(|_| ()),
            storage.manifest_path().exists()
        );
        built.expect("the persist is a first index, not a removal");
        assert!(storage.manifest_path().exists(), "no manifest was written");
    }

    /// The persist's removal handling as one case matrix (round ten).
    ///
    /// Rows are an initial state of `.sqry/graph` when the inputs are
    /// resolved, one event, and when the event happens. The expected
    /// outcome comes from the goal, not from the code:
    ///
    /// - the persist protects a committed index (one a recovery run then
    ///   would leave with a manifest or a snapshot): the one seen at input
    ///   resolution, and, from the moment its wait opens the lock file,
    ///   any committed while it waits or holds the lock;
    /// - a user removal of a protected index before the last check is
    ///   refused with `-32001`: `INDEX_REMOVED_DURING_PERSIST_WAIT` when
    ///   the refusal comes before the persist holds the lock (the wait saw
    ///   it), `INDEX_REMOVED_DURING_PERSIST` once it holds it, and nothing
    ///   of the persist's is written;
    /// - removing the whole directory (the lock file included) while the
    ///   persist waits or holds is a removal whatever it held, since the
    ///   persist would recreate the directory the user removed;
    /// - everything else (no event, removing content where no committed
    ///   index was, another holder's recovery, another writer's commit)
    ///   publishes.
    ///
    /// The state is set up after the inputs are resolved and
    /// `index_present` is recomputed over it with the resolution's own
    /// predicate (`index_present_at`): the resolution's readers would
    /// otherwise recover a leftover before the persist starts (decision
    /// D-i8-1), which is the "recovery clears" event, not the state.
    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial(persist_plant)]
    fn the_persist_removal_matrix() {
        let mut rows = Vec::new();
        for state in MState::ALL {
            for event in MEvent::ALL {
                for timing in MTiming::ALL {
                    if let Some(expect) = matrix_expected(state, event, timing) {
                        rows.push((state, event, timing, expect));
                    }
                }
            }
        }
        let mut failures = Vec::new();
        for (state, event, timing, expect) in &rows {
            let actual = run_matrix_row(*state, *event, *timing);
            let pass = actual == expect.label();
            println!(
                "matrix | {state:?} | {event:?} | {timing:?} | expected {} | actual {actual} | {}",
                expect.label(),
                if pass { "pass" } else { "FAIL" }
            );
            if !pass {
                failures.push(format!(
                    "{state:?} {event:?} {timing:?}: expected {}, got {actual}",
                    expect.label()
                ));
            }
        }
        // Two or three events in sequence, for what one event cannot show:
        // which index the persist protects when another writer commits
        // around a removal. Expected from the goal as above.
        type Sequence = (
            &'static str,
            MState,
            &'static [MEvent],
            Option<&'static [MEvent]>,
            MExpect,
        );
        let sequences: [Sequence; 5] = [
            // Committed after resolution, before the wait opens the lock
            // file; removed while the persist waits: protected.
            (
                "commit before the wait, content removed during it",
                MState::NoDir,
                &[MEvent::OtherCommits],
                Some(&[MEvent::RemoveContent]),
                MExpect::Refuse,
            ),
            (
                "commit and content removal during the wait",
                MState::NoDir,
                &[],
                Some(&[MEvent::OtherCommits, MEvent::RemoveContent]),
                MExpect::Refuse,
            ),
            // The index there at the hold is the recommitted one.
            (
                "content removed, then a commit, during the wait",
                MState::Committed,
                &[],
                Some(&[MEvent::RemoveContent, MEvent::OtherCommits]),
                MExpect::Publish,
            ),
            // The directory the wait opened was removed.
            (
                "rm -rf, then a new index, during the wait",
                MState::Committed,
                &[],
                Some(&[MEvent::RemoveAll, MEvent::OtherCommits]),
                MExpect::RefuseWait,
            ),
            (
                "commit, content removed, commit, during the wait",
                MState::NoDir,
                &[],
                Some(&[
                    MEvent::OtherCommits,
                    MEvent::RemoveContent,
                    MEvent::OtherCommits,
                ]),
                MExpect::Publish,
            ),
        ];
        for (name, state, before, during, expect) in sequences {
            let actual = run_matrix_case(
                state,
                before.to_vec(),
                during.map(<[MEvent]>::to_vec),
                vec![],
            );
            let pass = actual == expect.label();
            println!(
                "matrix | {state:?} | {name} | expected {} | actual {actual} | {}",
                expect.label(),
                if pass { "pass" } else { "FAIL" }
            );
            if !pass {
                failures.push(format!(
                    "{state:?} {name}: expected {}, got {actual}",
                    expect.label()
                ));
            }
        }
        println!(
            "matrix: {} rows and {} sequences, {} failed",
            rows.len(),
            sequences.len(),
            failures.len()
        );
        assert_eq!(rows.len(), 99, "matrix rows");
        assert!(
            failures.is_empty(),
            "{} matrix rows failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MState {
        /// No `.sqry/graph`.
        NoDir,
        /// An empty `.sqry/graph`.
        EmptyDir,
        /// Only the lock file.
        LockOnly,
        /// A committed index (manifest and snapshot).
        Committed,
        /// A committed index whose manifest was deleted (snapshot only).
        SnapshotOnly,
        /// A first persist killed after its begun marker
        /// (`snapshot_existed=0 manifest_existed=0`) and a partial snapshot.
        KilledFirst,
        /// An update of a committed index killed after setting the old
        /// pair aside and writing a partial new snapshot.
        KilledUpdate,
        /// The same over a snapshot-only index (`manifest_existed=0`).
        KilledUpdateSnapshotOnly,
        /// Another writer's first persist in flight (holding the lock,
        /// paused after its begun marker); it commits when released.
        InFlightFirst,
        /// Another writer's update in flight, as above.
        InFlightUpdate,
    }

    #[cfg(target_os = "linux")]
    impl MState {
        const ALL: [Self; 10] = [
            Self::NoDir,
            Self::EmptyDir,
            Self::LockOnly,
            Self::Committed,
            Self::SnapshotOnly,
            Self::KilledFirst,
            Self::KilledUpdate,
            Self::KilledUpdateSnapshotOnly,
            Self::InFlightFirst,
            Self::InFlightUpdate,
        ];

        /// A committed index at input resolution (a killed or in-flight
        /// update keeps the previous pair aside, which counts).
        fn committed_at_resolution(self) -> bool {
            matches!(
                self,
                Self::Committed
                    | Self::SnapshotOnly
                    | Self::KilledUpdate
                    | Self::KilledUpdateSnapshotOnly
                    | Self::InFlightUpdate
            )
        }

        /// A committed index once the persist's wait has begun: the
        /// leftover of a killed update is put back by the first hold, an
        /// in-flight writer has committed by the time it releases.
        fn committed_during_the_wait(self) -> bool {
            !matches!(
                self,
                Self::NoDir | Self::EmptyDir | Self::LockOnly | Self::KilledFirst
            )
        }

        fn leftover(self) -> bool {
            matches!(
                self,
                Self::KilledFirst | Self::KilledUpdate | Self::KilledUpdateSnapshotOnly
            )
        }

        fn in_flight(self) -> bool {
            matches!(self, Self::InFlightFirst | Self::InFlightUpdate)
        }
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MEvent {
        None,
        /// The user removes `.sqry` (the lock file included).
        RemoveAll,
        /// The user removes everything in `.sqry/graph` but the lock file.
        RemoveContent,
        /// Another holder's recovery clears (or puts back) a leftover.
        RecoveryClears,
        /// Another writer commits a new index.
        OtherCommits,
    }

    #[cfg(target_os = "linux")]
    impl MEvent {
        const ALL: [Self; 5] = [
            Self::None,
            Self::RemoveAll,
            Self::RemoveContent,
            Self::RecoveryClears,
            Self::OtherCommits,
        ];
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MTiming {
        /// After the inputs are resolved, before the wait opens the lock
        /// file (the `Arrived` plant point); the wait is not contended.
        BeforeWait,
        /// While another holder keeps the lock and the persist waits.
        ContendedWait,
        /// Holding the lock, before the last check (`BeforeTransaction`).
        UnderHold,
    }

    #[cfg(target_os = "linux")]
    impl MTiming {
        const ALL: [Self; 3] = [Self::BeforeWait, Self::ContendedWait, Self::UnderHold];
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MExpect {
        Publish,
        RefuseWait,
        Refuse,
    }

    #[cfg(target_os = "linux")]
    impl MExpect {
        fn label(self) -> &'static str {
            match self {
                Self::Publish => "publish",
                Self::RefuseWait => "refuse:wait",
                Self::Refuse => "refuse:hold",
            }
        }
    }

    #[cfg(target_os = "linux")]
    /// The expected outcome of one row, or `None` for a combination that
    /// is impossible or the same run as another row.
    fn matrix_expected(state: MState, event: MEvent, timing: MTiming) -> Option<MExpect> {
        use MEvent as E;
        use MTiming as T;
        match (event, timing) {
            // No event under the hold is the uncontended no-event row.
            (E::None, T::UnderHold) => return None,
            // The persist holds the lock: no other holder can recover or
            // commit before its last check.
            (E::RecoveryClears | E::OtherCommits, T::UnderHold) => return None,
            // The contender's own hold recovers the leftover: that run is
            // the `RecoveryClears` contended row.
            (E::None, T::ContendedWait) if state.leftover() => return None,
            _ => {}
        }
        // No leftover to clear.
        if event == E::RecoveryClears && !state.leftover() {
            return None;
        }
        // The in-flight writer's own commit is the other writer's commit,
        // and it leaves no leftover.
        if state.in_flight() && matches!(event, E::RecoveryClears | E::OtherCommits) {
            return None;
        }
        Some(match (event, timing) {
            (E::None | E::RecoveryClears | E::OtherCommits, _) => MExpect::Publish,
            (E::RemoveAll | E::RemoveContent, T::BeforeWait) => {
                if state.committed_at_resolution() {
                    MExpect::RefuseWait
                } else {
                    MExpect::Publish
                }
            }
            (E::RemoveAll, T::ContendedWait) => MExpect::RefuseWait,
            (E::RemoveAll, T::UnderHold) => MExpect::Refuse,
            (E::RemoveContent, T::ContendedWait | T::UnderHold) => {
                if state.committed_during_the_wait() {
                    MExpect::Refuse
                } else {
                    MExpect::Publish
                }
            }
        })
    }

    /// Mid-persist pauses for the in-flight writer, by graph directory,
    /// through core's process-wide mid-persist hook (set once).
    #[cfg(target_os = "linux")]
    mod in_flight_pause {
        use std::path::{Path, PathBuf};
        use std::sync::mpsc::{Receiver, Sender};

        type Pause = (PathBuf, Sender<()>, Receiver<()>);
        static PAUSES: parking_lot::Mutex<Vec<Pause>> = parking_lot::Mutex::new(Vec::new());

        fn hook(graph_dir: &Path) {
            let pause = {
                let mut pauses = PAUSES.lock();
                pauses
                    .iter()
                    .position(|(dir, _, _)| dir == graph_dir)
                    .map(|at| pauses.remove(at))
            };
            if let Some((_, in_flight, go)) = pause {
                let _ = in_flight.send(());
                let _ = go.recv();
            }
        }

        /// Pause the next persist of `graph_dir` once its old pair is
        /// aside: the first receiver hears it is in flight, the sender
        /// lets it go on.
        pub(super) fn pause_next(graph_dir: &Path) -> (Receiver<()>, Sender<()>) {
            sqry_core::graph::unified::persistence::write_guard::set_mid_persist_hook(hook);
            let (in_flight_tx, in_flight_rx) = std::sync::mpsc::channel();
            let (go_tx, go_rx) = std::sync::mpsc::channel();
            PAUSES
                .lock()
                .push((graph_dir.to_path_buf(), in_flight_tx, go_rx));
            (in_flight_rx, go_tx)
        }
    }

    /// Another writer holding the lock with a persist in flight: it holds
    /// the lock around its own persist (which re-enters that hold), commits
    /// when let go, runs `after_commit` still holding, then releases.
    /// What the in-flight writer runs after its commit, still holding.
    #[cfg(target_os = "linux")]
    type AfterCommit = Arc<parking_lot::Mutex<Option<Box<dyn FnOnce(&std::path::Path) + Send>>>>;

    #[cfg(target_os = "linux")]
    struct InFlightWriter {
        go: std::sync::mpsc::Sender<()>,
        after_commit: AfterCommit,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(target_os = "linux")]
    impl InFlightWriter {
        fn start(root: &std::path::Path) -> Self {
            use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
            use sqry_plugin_registry::{
                PluginSelectionConfig, build_and_persist_with_workspace_roster,
            };
            let graph_dir = GraphStorage::new(root).graph_dir().to_path_buf();
            let (in_flight, go) = in_flight_pause::pause_next(&graph_dir);
            let after_commit: AfterCommit = Arc::default();
            let handle = {
                let root = root.to_path_buf();
                let after_commit = Arc::clone(&after_commit);
                std::thread::spawn(move || {
                    let held = IndexWriteLock::acquire(&graph_dir).expect("the writer's hold");
                    build_and_persist_with_workspace_roster(
                        &root,
                        &PluginSelectionConfig::default(),
                        UnreadableManifestPolicy::Refuse,
                        "t:in-flight-writer",
                        &BuildConfig::default(),
                        &MacroOptionsRequest::empty(),
                        sqry_core::progress::no_op_reporter(),
                    )
                    .expect("the in-flight writer commits");
                    let event = after_commit.lock().take();
                    if let Some(event) = event {
                        event(&graph_dir);
                    }
                    drop(held);
                })
            };
            in_flight
                .recv_timeout(Duration::from_secs(120))
                .expect("the writer's persist is in flight");
            Self {
                go,
                after_commit,
                handle: Some(handle),
            }
        }

        /// Let it commit and release, and wait for it.
        fn finish(&mut self) {
            let _ = self.go.send(());
            if let Some(handle) = self.handle.take()
                && let Err(panic) = handle.join()
            {
                std::panic::resume_unwind(panic);
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for InFlightWriter {
        fn drop(&mut self) {
            let _ = self.go.send(());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[cfg(target_os = "linux")]
    /// Plant a rollback set as a killed persist leaves it: the begun
    /// marker, the old pair aside where it existed, and a partial new
    /// snapshot.
    fn plant_killed_persist(graph_dir: &std::path::Path, snapshot_existed: bool) {
        use sqry_core::graph::unified::persistence::PERSIST_LOCK_FILE_NAME;
        std::fs::create_dir_all(graph_dir).expect("graph dir");
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(graph_dir.join(PERSIST_LOCK_FILE_NAME))
            .expect("lock file");
        let manifest = graph_dir.join("manifest.json");
        let snapshot = graph_dir.join("snapshot.sqry");
        let manifest_existed = manifest.exists();
        std::fs::write(
            graph_dir.join(".txn-begun.rollback-1-1-1"),
            format!(
                "begun snapshot_existed={} manifest_existed={}\n",
                u8::from(snapshot_existed),
                u8::from(manifest_existed)
            ),
        )
        .expect("begun marker");
        if manifest_existed {
            std::fs::rename(&manifest, graph_dir.join(".manifest.json.rollback-1-1-1"))
                .expect("manifest aside");
        }
        if snapshot_existed {
            std::fs::hard_link(&snapshot, graph_dir.join(".snapshot.sqry.rollback-1-1-1"))
                .expect("snapshot aside");
        }
        // The new snapshot, written as the transaction writes it (a new
        // file renamed into place), so the one aside is not touched.
        let partial = graph_dir.join("snapshot.sqry.partial-new");
        std::fs::write(&partial, b"partial").expect("partial snapshot");
        std::fs::rename(&partial, &snapshot).expect("partial snapshot in place");
    }

    #[cfg(target_os = "linux")]
    fn has_rollback_names(graph_dir: &std::path::Path) -> bool {
        std::fs::read_dir(graph_dir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".rollback-"))
        })
    }

    #[cfg(target_os = "linux")]
    /// Run one event now. A recovery or a commit runs on another thread
    /// (another holder) when `elsewhere`, otherwise on this one, which then
    /// holds the lock (a contender: its persist re-enters that hold).
    fn run_matrix_event(event: MEvent, root: &std::path::Path, elsewhere: bool) {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_plugin_registry::{
            PluginSelectionConfig, build_and_persist_with_workspace_roster,
        };
        let graph_dir = GraphStorage::new(root).graph_dir().to_path_buf();
        match event {
            MEvent::None => {}
            MEvent::RemoveAll => {
                let _ = std::fs::remove_dir_all(root.join(".sqry"));
            }
            MEvent::RemoveContent => {
                if graph_dir.is_dir() {
                    remove_all_but_the_lock_file(&graph_dir);
                }
            }
            MEvent::RecoveryClears if elsewhere => {
                // A reader: the manifest is missing beside rollback names,
                // so it takes the lock and its hold recovers.
                let root = root.to_path_buf();
                std::thread::spawn(move || {
                    let _ = GraphStorage::new(&root).exists();
                })
                .join()
                .expect("the reader");
                assert!(!has_rollback_names(&graph_dir), "the reader recovered");
            }
            // The contender's own hold has recovered.
            MEvent::RecoveryClears => {
                assert!(!has_rollback_names(&graph_dir), "the contender recovered");
            }
            MEvent::OtherCommits => {
                let commit = move |root: &std::path::Path| {
                    build_and_persist_with_workspace_roster(
                        root,
                        &PluginSelectionConfig::default(),
                        UnreadableManifestPolicy::Refuse,
                        "t:other-writer",
                        &BuildConfig::default(),
                        &MacroOptionsRequest::empty(),
                        sqry_core::progress::no_op_reporter(),
                    )
                    .expect("the other writer commits");
                };
                // A contender whose lock file was removed holds nothing
                // current: the writer then runs as another thread would.
                if elsewhere
                    || !sqry_core::graph::unified::persistence::IndexWriteLock::held_by_this_thread(
                        &graph_dir,
                    )
                {
                    let root = root.to_path_buf();
                    std::thread::spawn(move || commit(&root))
                        .join()
                        .expect("the other writer");
                } else {
                    commit(root);
                }
            }
        }
    }

    /// Run one matrix row; its outcome as `MExpect::label` spells it, or a
    /// description of anything else.
    #[cfg(target_os = "linux")]
    fn run_matrix_row(state: MState, event: MEvent, timing: MTiming) -> String {
        match timing {
            MTiming::BeforeWait => run_matrix_case(state, vec![event], None, vec![]),
            MTiming::ContendedWait => run_matrix_case(state, vec![], Some(vec![event]), vec![]),
            MTiming::UnderHold => run_matrix_case(state, vec![], None, vec![event]),
        }
    }

    /// Run one case: `before` in order before the wait, `during` in order
    /// while another holder keeps the lock and the persist waits (`None`:
    /// the wait is not contended), `under` in order under the hold before
    /// the last check.
    #[cfg(target_os = "linux")]
    fn run_matrix_case(
        state: MState,
        before: Vec<MEvent>,
        during: Option<Vec<MEvent>>,
        under: Vec<MEvent>,
    ) -> String {
        assert!(
            !(state.in_flight() && during.is_some() && !before.is_empty()),
            "an in-flight writer holds the lock before a contended wait"
        );
        let run_all = |events: &[MEvent], root: &std::path::Path, elsewhere: bool| {
            for event in events {
                run_matrix_event(*event, root, elsewhere);
            }
        };
        use sqry_core::graph::unified::persistence::{GraphStorage, PERSIST_LOCK_FILE_NAME};
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let storage = GraphStorage::new(&root);
        let graph_dir = storage.graph_dir().to_path_buf();
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let resolve = || {
            resolve_durable_rebuild_inputs(
                &roster,
                &BuildConfig::default(),
                &root,
                &MacroOptionsRequest::empty(),
                None,
                UnreadableManifestPolicy::Refuse,
            )
            .expect("inputs resolve")
        };
        match state {
            MState::NoDir | MState::KilledFirst | MState::InFlightFirst => {}
            MState::EmptyDir => std::fs::create_dir_all(&graph_dir).expect("graph dir"),
            MState::LockOnly => {
                std::fs::create_dir_all(&graph_dir).expect("graph dir");
                std::fs::write(graph_dir.join(PERSIST_LOCK_FILE_NAME), b"").expect("lock file");
            }
            MState::Committed | MState::KilledUpdate | MState::InFlightUpdate => {
                build_and_persist_blocking(&root, &resolve(), "t:matrix-index")
                    .expect("the first index");
            }
            MState::SnapshotOnly | MState::KilledUpdateSnapshotOnly => {
                build_and_persist_blocking(&root, &resolve(), "t:matrix-index")
                    .expect("the first index");
                std::fs::remove_file(storage.manifest_path()).expect("manifest deleted");
            }
        }
        let mut inputs = resolve();
        let mut writer = None;
        match state {
            MState::KilledFirst => plant_killed_persist(&graph_dir, false),
            MState::KilledUpdate | MState::KilledUpdateSnapshotOnly => {
                plant_killed_persist(&graph_dir, true);
            }
            MState::InFlightFirst | MState::InFlightUpdate => {
                writer = Some(InFlightWriter::start(&root));
            }
            _ => {}
        }
        inputs.index_present = index_present_at(&graph_dir);
        let contended = during.is_some();
        let leftover_at_arrival = Arc::new(AtomicBool::new(false));
        let contenders = Contenders::default();
        let watcher = Contenders::default();
        let writer = Arc::new(parking_lot::Mutex::new(writer));
        {
            let event_root = root.clone();
            let graph_dir = graph_dir.clone();
            let leftover_at_arrival = Arc::clone(&leftover_at_arrival);
            let contenders = contenders.clone();
            let watcher = watcher.clone();
            let writer = Arc::clone(&writer);
            let arrived_once = Arc::new(AtomicBool::new(false));
            persist_plant::install(
                &root,
                Arc::new(move |phase| match phase {
                    persist_plant::Phase::Arrived if !arrived_once.swap(true, Ordering::AcqRel) => {
                        leftover_at_arrival
                            .store(has_rollback_names(&graph_dir), Ordering::Release);
                        let contended = during.is_some();
                        if state.in_flight() {
                            let mut writer = writer.lock();
                            let writer = writer.as_mut().expect("the in-flight writer");
                            if contended {
                                // It commits once the persist waits, then
                                // the event runs while it still holds.
                                let event_root = event_root.clone();
                                let events = during.clone().unwrap_or_default();
                                *writer.after_commit.lock() = Some(Box::new(move |_| {
                                    run_all(&events, &event_root, false);
                                }));
                                let go = writer.go.clone();
                                let graph_dir = graph_dir.clone();
                                let waiter = watcher.clone();
                                watcher.push(std::thread::spawn(move || {
                                    // Lets the writer go even when the wait
                                    // check panics, so the persist is not
                                    // left waiting.
                                    struct Go(std::sync::mpsc::Sender<()>);
                                    impl Drop for Go {
                                        fn drop(&mut self) {
                                            let _ = self.0.send(());
                                        }
                                    }
                                    let _go = Go(go);
                                    waiter.wait_for_the_persist(&graph_dir);
                                }));
                                return;
                            }
                            writer.finish();
                        }
                        run_all(&before, &event_root, true);
                        if let Some(events) = during.clone() {
                            let event_root = event_root.clone();
                            spawn_contender(
                                graph_dir.clone(),
                                |_| {},
                                move |_| run_all(&events, &event_root, false),
                                &contenders,
                            );
                        }
                    }
                    persist_plant::Phase::BeforeTransaction => {
                        run_all(&under, &event_root, true);
                    }
                    _ => {}
                }),
            );
        }
        let built = build_and_persist_blocking(&root, &inputs, "t:matrix");
        persist_plant::clear();
        let waited = contenders.join(usize::from(contended && !state.in_flight()))
            + watcher.join(usize::from(contended && state.in_flight()));
        if let Some(mut writer) = writer.lock().take() {
            writer.finish();
        }
        if state.leftover() {
            assert!(
                leftover_at_arrival.load(Ordering::Acquire),
                "{state:?}: the leftover was gone before the persist arrived"
            );
        }
        let manifest = storage.manifest_path().exists();
        let snapshot = storage.snapshot_path().exists();
        // The persist's own manifest records its build command; one
        // another writer committed (a case that recreates the index) does
        // not, and its snapshot is that writer's too.
        let ours = std::fs::read_to_string(storage.manifest_path())
            .is_ok_and(|text| text.contains("\"t:matrix\""));
        let uncontended = if contended && waited == 0 {
            "uncontended "
        } else {
            ""
        };
        let outcome = match &built {
            Ok(_) if ours => "publish".to_string(),
            Ok(_) => format!("published without its manifest (manifest={manifest})"),
            Err(err @ DaemonError::WorkspaceBuildFailed { reason, .. })
                if err.jsonrpc_code() == Some(-32001)
                    && (reason == INDEX_REMOVED_DURING_PERSIST_WAIT
                        || reason == INDEX_REMOVED_DURING_PERSIST) =>
            {
                if ours || (snapshot && !manifest) {
                    format!("refused but ours={ours} manifest={manifest} snapshot={snapshot}")
                } else if reason == INDEX_REMOVED_DURING_PERSIST_WAIT {
                    "refuse:wait".to_string()
                } else {
                    "refuse:hold".to_string()
                }
            }
            Err(other) => format!("error {other:?}"),
        };
        format!("{uncontended}{outcome}")
    }

    /// Round ten (Codex P2): another writer's first persist is in flight
    /// when this persist's wait opens the lock file and rolls back
    /// (snapshot, then marker) right after that read's last look at the
    /// snapshot. The read must not report a committed index: armed from
    /// it, the check under the hold found none and refused this valid
    /// first persist as `INDEX_REMOVED_DURING_PERSIST`.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_first_persist_rolled_back_during_the_waits_read_is_not_a_removal() {
        use sqry_core::graph::unified::persistence::GraphStorage;
        use sqry_core::graph::unified::persistence::write_guard::committed_read_seam;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("lib.rs"), b"pub fn a() -> u32 { 1 }\n").expect("source");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        let mut inputs = resolve_durable_rebuild_inputs(
            &roster,
            &BuildConfig::default(),
            &root,
            &MacroOptionsRequest::empty(),
            None,
            UnreadableManifestPolicy::Refuse,
        )
        .expect("inputs resolve");
        let storage = GraphStorage::new(&root);
        let graph_dir = storage.graph_dir().to_path_buf();
        plant_killed_persist(&graph_dir, false);
        inputs.index_present = index_present_at(&graph_dir);
        assert!(!inputs.index_present, "precondition: no committed index");
        let rolled_back = graph_dir.clone();
        committed_read_seam::arm(
            &graph_dir,
            committed_read_seam::Point::Third,
            Box::new(move || {
                std::fs::remove_file(rolled_back.join("snapshot.sqry")).expect("snapshot");
                std::fs::remove_file(rolled_back.join(".txn-begun.rollback-1-1-1"))
                    .expect("marker");
            }),
        );
        let built = build_and_persist_blocking(&root, &inputs, "t:rolled-back-in-read");
        println!(
            "rolled back during the wait's read: result={:?} manifest={}",
            built.as_ref().map(|_| ()),
            storage.manifest_path().exists()
        );
        built.expect("a first persist, not a removal");
        assert!(storage.manifest_path().exists(), "no manifest was written");
    }

    /// S6 (round 7): the input resolution every durable rebuild runs
    /// refuses a request the request check refuses, naming the root, before
    /// it reads the record: the defence behind the handlers' own check.
    #[test]
    fn durable_rebuild_inputs_refuse_a_request_the_check_refuses() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let roster = Arc::new(WorkspaceRosterResolver::new());
        for flags in [
            vec![String::new()],
            vec!["  ".to_string()],
            vec![" test".to_string()],
        ] {
            let request = MacroOptionsRequest {
                cfg_flags: Some(flags.clone()),
                ..MacroOptionsRequest::empty()
            };
            let refused = resolve_durable_rebuild_inputs(
                &roster,
                &BuildConfig::default(),
                &root,
                &request,
                None,
                UnreadableManifestPolicy::Refuse,
            );
            match refused {
                Err(DaemonError::InvalidArgument { reason }) => assert!(
                    reason.starts_with(&format!("rebuild of {} refused: ", root.display())),
                    "{flags:?}: {reason}"
                ),
                Err(other) => panic!("{flags:?}: expected InvalidArgument, got {other:?}"),
                Ok(_) => panic!("{flags:?}: expected InvalidArgument, the inputs resolved"),
            }
        }
        assert!(
            resolve_durable_rebuild_inputs(
                &roster,
                &BuildConfig::default(),
                &root,
                &MacroOptionsRequest {
                    cfg_flags: Some(vec!["test".to_string()]),
                    ..MacroOptionsRequest::empty()
                },
                None,
                UnreadableManifestPolicy::Refuse,
            )
            .is_ok(),
            "a flag the check accepts resolves"
        );
    }

    /// S4: with no manifest, the narrowing guard compares against the
    /// resident generation's record when that record came from a manifest,
    /// so a rebuild after the manifest is gone (a crash in the middle of a
    /// persist, a hand removal) cannot silently record a narrower roster.
    /// A resident record from no manifest, or no resident generation, has
    /// nothing to protect.
    #[test]
    fn with_no_manifest_the_narrowing_guard_compares_against_the_resident_record() {
        use sqry_plugin_registry::RosterSource;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let fast = RosterRecord::fast_path_default();
        let mut wide_ids = fast.active_plugin_ids.clone();
        wide_ids.push("json".to_string());
        let from_a_manifest = RosterRecord {
            active_plugin_ids: wide_ids.clone(),
            high_cost_mode: Some("include_all".to_string()),
            source: RosterSource::PersistedManifest,
        };
        let refused = refuse_if_rebuild_narrows(
            tmp.path(),
            &fast,
            Some(&from_a_manifest),
            UnreadableManifestPolicy::Refuse,
        )
        .expect_err("narrower than the resident record from a manifest");
        match &refused {
            DaemonError::RebuildWouldNarrowSelection {
                missing_plugin_ids,
                restore_command,
                ..
            } => {
                assert_eq!(missing_plugin_ids, &vec!["json".to_string()]);
                assert!(
                    restore_command.contains("--include-high-cost"),
                    "{restore_command}"
                );
            }
            other => panic!("expected RebuildWouldNarrowSelection, got {other:?}"),
        }
        assert_eq!(refused.jsonrpc_code(), Some(-32021));
        let from_no_manifest = RosterRecord {
            source: RosterSource::Fallback,
            ..from_a_manifest.clone()
        };
        assert!(
            refuse_if_rebuild_narrows(
                tmp.path(),
                &fast,
                Some(&from_no_manifest),
                UnreadableManifestPolicy::Refuse
            )
            .is_ok()
        );
        assert!(
            refuse_if_rebuild_narrows(tmp.path(), &fast, None, UnreadableManifestPolicy::Refuse)
                .is_ok()
        );
        let as_wide = RosterRecord {
            active_plugin_ids: wide_ids,
            ..fast.clone()
        };
        assert!(
            refuse_if_rebuild_narrows(
                tmp.path(),
                &as_wide,
                Some(&from_a_manifest),
                UnreadableManifestPolicy::Refuse
            )
            .is_ok(),
            "an equal roster is not narrower"
        );
    }
}
