//! In-memory resident revision handle registry.
//!
//! Live workspace state remains owned by `WorkspaceManager::workspaces`.
//! This registry owns revision-aware handles that do not participate in
//! live filesystem watching: immutable artifacts, dirty snapshots, and future
//! managed worktree handles. Loads coalesce by [`ArtifactId`] so concurrent
//! callers never build or hydrate the same immutable graph twice.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use parking_lot::{Mutex, RwLock};
use sqry_core::graph::{CodeGraph, unified::GraphMemorySize};
use sqry_daemon_protocol::{
    ArtifactId, ArtifactInputDigest, ResidentHandleKind, ResolvedRevision, RevisionId,
    RevisionLoadState, RevisionStatus,
};

use crate::error::DaemonError;

use super::manifest::hex_sha256;

/// Inputs required to create or load a resident revision handle.
#[derive(Debug, Clone)]
pub struct ResidentRevisionLoad {
    /// Repository root associated with this resident handle.
    pub source_root: PathBuf,
    /// Stable daemon-scoped handle id.
    pub revision_id: RevisionId,
    /// Handle kind.
    pub handle_kind: ResidentHandleKind,
    /// Artifact backing this handle.
    pub artifact_id: ArtifactId,
    /// Manifest input digest for status verification.
    pub artifact_inputs: ArtifactInputDigest,
    /// Fully resolved revision identity.
    pub resolved: ResolvedRevision,
    /// Pin against memory eviction and prune.
    pub pinned: bool,
}

impl ResidentRevisionLoad {
    /// Build a deterministic resident id from a handle kind and artifact id.
    #[must_use]
    pub fn deterministic_revision_id(
        handle_kind: ResidentHandleKind,
        artifact_id: &ArtifactId,
    ) -> RevisionId {
        let digest = hex_sha256(format!("{handle_kind:?}:{}", artifact_id.0).as_bytes());
        RevisionId(format!("rev-{digest}"))
    }
}

/// Query-time guard that pins a resident handle until dropped.
#[derive(Debug)]
pub struct ResidentQueryGuard {
    handle: Arc<ResidentRevisionHandle>,
}

impl ResidentQueryGuard {
    /// Resident handle used by the query.
    #[must_use]
    pub fn handle(&self) -> &Arc<ResidentRevisionHandle> {
        &self.handle
    }

    /// Graph snapshot pinned by this guard.
    #[must_use]
    pub fn graph(&self) -> Option<Arc<CodeGraph>> {
        self.handle.graph()
    }

    /// Status snapshot while the query is active.
    #[must_use]
    pub fn status(&self) -> RevisionStatus {
        self.handle.status()
    }
}

impl Drop for ResidentQueryGuard {
    fn drop(&mut self) {
        self.handle.active_queries.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Resident non-live revision handle.
#[derive(Debug)]
pub struct ResidentRevisionHandle {
    source_root: PathBuf,
    revision_id: RevisionId,
    handle_kind: ResidentHandleKind,
    resolved: ResolvedRevision,
    artifact_id: ArtifactId,
    artifact_inputs: ArtifactInputDigest,
    graph: RwLock<Option<Arc<CodeGraph>>>,
    state: RwLock<RevisionLoadState>,
    pinned: AtomicBool,
    active_queries: AtomicU64,
    memory_bytes: AtomicU64,
    memory_high_water_bytes: AtomicU64,
    last_error: RwLock<Option<String>>,
    last_accessed: RwLock<Instant>,
}

impl ResidentRevisionHandle {
    fn loading(load: ResidentRevisionLoad) -> Self {
        Self {
            source_root: load.source_root,
            revision_id: load.revision_id,
            handle_kind: load.handle_kind,
            resolved: load.resolved,
            artifact_id: load.artifact_id,
            artifact_inputs: load.artifact_inputs,
            graph: RwLock::new(None),
            state: RwLock::new(RevisionLoadState::Loading),
            pinned: AtomicBool::new(load.pinned),
            active_queries: AtomicU64::new(0),
            memory_bytes: AtomicU64::new(0),
            memory_high_water_bytes: AtomicU64::new(0),
            last_error: RwLock::new(None),
            last_accessed: RwLock::new(Instant::now()),
        }
    }

    fn publish_graph(&self, graph: CodeGraph) {
        let memory_bytes = graph.heap_bytes() as u64;
        self.memory_bytes.store(memory_bytes, Ordering::Release);
        self.memory_high_water_bytes
            .fetch_max(memory_bytes, Ordering::AcqRel);
        *self.graph.write() = Some(Arc::new(graph));
        *self.last_error.write() = None;
        *self.state.write() = RevisionLoadState::Loaded;
        self.touch();
    }

    fn record_failure(&self, err: &DaemonError) {
        *self.last_error.write() = Some(err.to_string());
        *self.state.write() = RevisionLoadState::Failed;
        self.touch();
    }

    /// Resident revision id.
    #[must_use]
    pub fn revision_id(&self) -> &RevisionId {
        &self.revision_id
    }

    /// Backing artifact id.
    #[must_use]
    pub fn artifact_id(&self) -> &ArtifactId {
        &self.artifact_id
    }

    /// Handle kind.
    #[must_use]
    pub fn handle_kind(&self) -> ResidentHandleKind {
        self.handle_kind
    }

    /// Repository root associated with this handle.
    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> RevisionLoadState {
        *self.state.read()
    }

    /// True when this handle cannot be evicted or pruned.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.pinned.load(Ordering::Acquire)
    }

    /// Mark this handle pinned.
    pub fn pin(&self) {
        self.pinned.store(true, Ordering::Release);
    }

    /// Number of active queries currently using this handle.
    #[must_use]
    pub fn active_queries(&self) -> u64 {
        self.active_queries.load(Ordering::Acquire)
    }

    /// Current resident graph memory bytes.
    #[must_use]
    pub fn memory_bytes(&self) -> u64 {
        self.memory_bytes.load(Ordering::Acquire)
    }

    /// Monotonic resident graph high-water bytes for this handle.
    #[must_use]
    pub fn memory_high_water_bytes(&self) -> u64 {
        self.memory_high_water_bytes.load(Ordering::Acquire)
    }

    /// Last access time used for inactive LRU eviction.
    #[must_use]
    pub fn last_accessed(&self) -> Instant {
        *self.last_accessed.read()
    }

    /// Graph snapshot when loaded.
    #[must_use]
    pub fn graph(&self) -> Option<Arc<CodeGraph>> {
        self.graph.read().as_ref().map(Arc::clone)
    }

    /// Query guard that increments active query pins until drop.
    #[must_use]
    pub fn pin_query(self: &Arc<Self>) -> ResidentQueryGuard {
        self.active_queries.fetch_add(1, Ordering::AcqRel);
        self.touch();
        ResidentQueryGuard {
            handle: Arc::clone(self),
        }
    }

    /// Status snapshot for wire responses.
    #[must_use]
    pub fn status(&self) -> RevisionStatus {
        RevisionStatus {
            revision_id: self.revision_id.clone(),
            handle_kind: self.handle_kind,
            resolved: self.resolved.clone(),
            artifact_id: self.artifact_id.clone(),
            artifact_inputs: self.artifact_inputs.clone(),
            state: self.state(),
            pinned: self.is_pinned(),
            active_queries: self.active_queries(),
            memory_bytes: self.memory_bytes(),
            last_error: self.last_error.read().clone(),
        }
    }

    fn touch(&self) {
        *self.last_accessed.write() = Instant::now();
    }

    fn mark_unloaded(&self) {
        *self.graph.write() = None;
        self.memory_bytes.store(0, Ordering::Release);
        *self.state.write() = RevisionLoadState::Unloaded;
        self.touch();
    }

    fn can_evict(&self) -> bool {
        !self.is_pinned()
            && self.active_queries() == 0
            && self.handle_kind != ResidentHandleKind::LiveWorkspace
            && matches!(
                self.state(),
                RevisionLoadState::Loaded | RevisionLoadState::Failed
            )
    }
}

#[derive(Debug)]
struct LoadSlot {
    state: StdMutex<LoadSlotState>,
    changed: Condvar,
}

impl LoadSlot {
    fn loading() -> Self {
        Self {
            state: StdMutex::new(LoadSlotState::Loading),
            changed: Condvar::new(),
        }
    }

    fn finish(&self, state: LoadSlotState) {
        *self.state.lock().expect("load slot mutex poisoned") = state;
        self.changed.notify_all();
    }

    fn wait(&self) -> LoadSlotState {
        let mut guard = self.state.lock().expect("load slot mutex poisoned");
        while matches!(*guard, LoadSlotState::Loading) {
            guard = self
                .changed
                .wait(guard)
                .expect("load slot mutex poisoned while waiting");
        }
        guard.clone()
    }
}

#[derive(Debug, Clone)]
enum LoadSlotState {
    Loading,
    Loaded(RevisionId),
    Failed(String),
}

#[derive(Debug, Default)]
struct ResidentRegistryState {
    by_revision: HashMap<RevisionId, Arc<ResidentRevisionHandle>>,
    by_artifact: HashMap<ArtifactId, RevisionId>,
    loading_by_artifact: HashMap<ArtifactId, Arc<LoadSlot>>,
}

/// In-memory resident revision registry.
#[derive(Debug, Default)]
pub struct ResidentRevisionRegistry {
    state: Mutex<ResidentRegistryState>,
    memory_high_water_bytes: AtomicU64,
}

impl ResidentRevisionRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load or reuse a resident handle. Concurrent loads for the same
    /// [`ArtifactId`] wait on the first caller and return its handle.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] when the graph loader fails or when a coalesced
    /// load failed in another caller.
    pub fn load_or_coalesce<F>(
        &self,
        load: ResidentRevisionLoad,
        build_graph: F,
    ) -> Result<Arc<ResidentRevisionHandle>, DaemonError>
    where
        F: FnOnce() -> Result<CodeGraph, DaemonError>,
    {
        let maybe_slot = {
            let mut state = self.state.lock();
            if let Some(existing_id) = state.by_artifact.get(&load.artifact_id)
                && let Some(existing) = state.by_revision.get(existing_id)
            {
                if load.pinned {
                    existing.pin();
                }
                return Ok(Arc::clone(existing));
            }
            if let Some(slot) = state.loading_by_artifact.get(&load.artifact_id) {
                Some(Arc::clone(slot))
            } else {
                let slot = Arc::new(LoadSlot::loading());
                state
                    .loading_by_artifact
                    .insert(load.artifact_id.clone(), Arc::clone(&slot));
                None
            }
        };

        if let Some(slot) = maybe_slot {
            match slot.wait() {
                LoadSlotState::Loaded(revision_id) => {
                    let state = self.state.lock();
                    if let Some(handle) = state.by_revision.get(&revision_id) {
                        if load.pinned {
                            handle.pin();
                        }
                        return Ok(Arc::clone(handle));
                    }
                    return Err(DaemonError::RevisionSourceUnavailable {
                        reason: format!(
                            "coalesced resident revision {} was not present after load",
                            revision_id.0
                        ),
                        path: None,
                    });
                }
                LoadSlotState::Failed(reason) => {
                    return Err(DaemonError::RevisionSourceUnavailable { reason, path: None });
                }
                LoadSlotState::Loading => unreachable!("wait returns only terminal states"),
            }
        }

        let handle = Arc::new(ResidentRevisionHandle::loading(load.clone()));
        let result = build_graph();

        let mut state = self.state.lock();
        let slot = state
            .loading_by_artifact
            .remove(&load.artifact_id)
            .expect("loading slot must exist for first caller");
        match result {
            Ok(graph) => {
                handle.publish_graph(graph);
                let memory = handle.memory_bytes();
                self.memory_high_water_bytes
                    .fetch_max(memory, Ordering::AcqRel);
                state
                    .by_artifact
                    .insert(load.artifact_id.clone(), load.revision_id.clone());
                state
                    .by_revision
                    .insert(load.revision_id.clone(), Arc::clone(&handle));
                slot.finish(LoadSlotState::Loaded(load.revision_id.clone()));
                Ok(handle)
            }
            Err(err) => {
                handle.record_failure(&err);
                state
                    .by_revision
                    .insert(load.revision_id.clone(), Arc::clone(&handle));
                slot.finish(LoadSlotState::Failed(err.to_string()));
                Err(err)
            }
        }
    }

    /// Register an already-loaded graph. Primarily used by tests and future
    /// dirty-snapshot builders that capture graph bytes outside the artifact
    /// store path.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if a concurrent coalesced load failed. When a
    /// valid resident handle already owns the same artifact id, that existing
    /// handle is returned unchanged.
    pub fn register_loaded(
        &self,
        load: ResidentRevisionLoad,
        graph: CodeGraph,
    ) -> Result<Arc<ResidentRevisionHandle>, DaemonError> {
        self.load_or_coalesce(load, || Ok(graph))
    }

    /// Acquire an active query guard by revision id.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] if the handle is not
    /// resident and loaded.
    pub fn acquire_query(
        &self,
        revision_id: &RevisionId,
    ) -> Result<ResidentQueryGuard, DaemonError> {
        let handle = {
            let state = self.state.lock();
            state.by_revision.get(revision_id).cloned()
        }
        .ok_or_else(|| DaemonError::RevisionSourceUnavailable {
            reason: format!("resident revision {} is not loaded", revision_id.0),
            path: None,
        })?;
        if handle.state() != RevisionLoadState::Loaded {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: format!(
                    "resident revision {} is not queryable in state {:?}",
                    revision_id.0,
                    handle.state()
                ),
                path: None,
            });
        }
        Ok(handle.pin_query())
    }

    /// Return a handle by revision id.
    #[must_use]
    pub fn get(&self, revision_id: &RevisionId) -> Option<Arc<ResidentRevisionHandle>> {
        self.state.lock().by_revision.get(revision_id).cloned()
    }

    /// Status snapshots, optionally filtered by source root.
    #[must_use]
    pub fn statuses(&self, root: Option<&Path>, include_unloaded: bool) -> Vec<RevisionStatus> {
        let mut statuses: Vec<_> = self
            .state
            .lock()
            .by_revision
            .values()
            .filter(|handle| root.is_none_or(|root| handle.source_root() == root))
            .filter(|handle| include_unloaded || handle.state() != RevisionLoadState::Unloaded)
            .map(|handle| handle.status())
            .collect();
        statuses.sort_by(|left, right| left.revision_id.cmp(&right.revision_id));
        statuses
    }

    /// Current resident revision memory bytes.
    #[must_use]
    pub fn memory_bytes(&self) -> u64 {
        self.state
            .lock()
            .by_revision
            .values()
            .map(|handle| handle.memory_bytes())
            .sum()
    }

    /// Resident revision memory high-water bytes.
    #[must_use]
    pub fn memory_high_water_bytes(&self) -> u64 {
        let current = self.memory_bytes();
        let previous = self
            .memory_high_water_bytes
            .fetch_max(current, Ordering::AcqRel);
        previous.max(current)
    }

    /// Artifact ids pinned by active or explicitly pinned resident handles.
    #[must_use]
    pub fn pinned_artifact_ids(&self) -> Vec<ArtifactId> {
        let mut ids: Vec<_> = self
            .state
            .lock()
            .by_revision
            .values()
            .filter(|handle| handle.is_pinned() || handle.active_queries() > 0)
            .map(|handle| handle.artifact_id().clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Evict the least-recently-used inactive non-live handle.
    #[must_use]
    pub fn evict_inactive_lru(&self) -> Option<RevisionId> {
        let mut state = self.state.lock();
        let candidate = state
            .by_revision
            .values()
            .filter(|handle| handle.can_evict())
            .min_by_key(|handle| handle.last_accessed())
            .map(|handle| handle.revision_id().clone())?;

        if let Some(handle) = state.by_revision.get(&candidate) {
            handle.mark_unloaded();
            state
                .by_artifact
                .retain(|_, revision_id| revision_id != &candidate);
        }
        Some(candidate)
    }

    /// Unload a resident handle.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] when the handle is
    /// active or pinned and `force` is false.
    pub fn unload(&self, revision_id: &RevisionId, force: bool) -> Result<bool, DaemonError> {
        let mut state = self.state.lock();
        let Some(handle) = state.by_revision.get(revision_id).cloned() else {
            return Ok(false);
        };
        if !force && handle.is_pinned() {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: format!("resident revision {} is pinned", revision_id.0),
                path: None,
            });
        }
        if !force && handle.active_queries() > 0 {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: format!("resident revision {} has active queries", revision_id.0),
                path: None,
            });
        }
        handle.mark_unloaded();
        state.by_revision.remove(revision_id);
        state
            .by_artifact
            .retain(|_, current| current != revision_id);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use sqry_daemon_protocol::{
        ArtifactInputDigest, ObjectFormat, RepositoryIdentity, RevisionSelector, SourceByteMode,
    };

    use super::*;

    fn load_request(artifact: &str, revision: &str) -> ResidentRevisionLoad {
        let resolved = ResolvedRevision {
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
        };
        ResidentRevisionLoad {
            source_root: PathBuf::from("/repo"),
            revision_id: RevisionId(revision.to_owned()),
            handle_kind: ResidentHandleKind::ImmutableRevision,
            artifact_id: ArtifactId(artifact.to_owned()),
            artifact_inputs: ArtifactInputDigest {
                schema_version: 1,
                digest: "digest".to_owned(),
            },
            resolved,
            pinned: false,
        }
    }

    #[test]
    fn deterministic_revision_id_is_stable_and_kind_scoped() {
        let artifact = ArtifactId("artifact".to_owned());
        let immutable = ResidentRevisionLoad::deterministic_revision_id(
            ResidentHandleKind::ImmutableRevision,
            &artifact,
        );
        let dirty = ResidentRevisionLoad::deterministic_revision_id(
            ResidentHandleKind::DirtySnapshot,
            &artifact,
        );

        assert_eq!(
            immutable,
            ResidentRevisionLoad::deterministic_revision_id(
                ResidentHandleKind::ImmutableRevision,
                &artifact,
            )
        );
        assert_ne!(immutable, dirty);
    }

    #[test]
    fn coalesces_concurrent_loads_for_same_artifact() {
        let registry = Arc::new(ResidentRevisionRegistry::new());
        let builds = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for idx in 0..4 {
            let registry = Arc::clone(&registry);
            let builds = Arc::clone(&builds);
            threads.push(thread::spawn(move || {
                registry
                    .load_or_coalesce(load_request("artifact-a", &format!("rev-{idx}")), || {
                        builds.fetch_add(1, Ordering::AcqRel);
                        thread::sleep(Duration::from_millis(25));
                        Ok(CodeGraph::new())
                    })
                    .unwrap()
                    .artifact_id()
                    .clone()
            }));
        }

        let artifact_ids: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();

        assert_eq!(builds.load(Ordering::Acquire), 1);
        assert!(artifact_ids.iter().all(|id| id.0 == "artifact-a"));
        assert_eq!(registry.statuses(None, false).len(), 1);
    }

    #[test]
    fn active_query_guard_pins_until_drop() {
        let registry = ResidentRevisionRegistry::new();
        let load = load_request("artifact-a", "rev-a");
        registry
            .register_loaded(load.clone(), CodeGraph::new())
            .unwrap();

        let guard = registry.acquire_query(&load.revision_id).unwrap();
        assert_eq!(guard.status().active_queries, 1);
        assert_eq!(
            registry.pinned_artifact_ids(),
            vec![ArtifactId("artifact-a".to_owned())]
        );
        assert_eq!(registry.evict_inactive_lru(), None);

        drop(guard);

        assert!(registry.pinned_artifact_ids().is_empty());
        assert_eq!(registry.evict_inactive_lru(), Some(load.revision_id));
    }

    #[test]
    fn pinned_handle_refuses_unload_without_force() {
        let registry = ResidentRevisionRegistry::new();
        let mut load = load_request("artifact-a", "rev-a");
        load.pinned = true;
        registry
            .register_loaded(load.clone(), CodeGraph::new())
            .unwrap();

        let err = registry.unload(&load.revision_id, false).unwrap_err();
        assert!(matches!(err, DaemonError::RevisionSourceUnavailable { .. }));
        assert!(registry.unload(&load.revision_id, true).unwrap());
    }

    #[test]
    fn statuses_filter_by_root_and_loaded_state() {
        let registry = ResidentRevisionRegistry::new();
        let first = load_request("artifact-a", "rev-a");
        let mut second = load_request("artifact-b", "rev-b");
        second.source_root = PathBuf::from("/other");
        registry
            .register_loaded(first.clone(), CodeGraph::new())
            .unwrap();
        registry
            .register_loaded(second.clone(), CodeGraph::new())
            .unwrap();
        registry.unload(&second.revision_id, true).unwrap();

        assert_eq!(registry.statuses(Some(Path::new("/repo")), false).len(), 1);
        assert_eq!(registry.statuses(Some(Path::new("/other")), false).len(), 0);
    }
}
