//! Workspace graph builder abstraction.
//!
//! [`WorkspaceBuilder`] is the dependency-injection seam between the
//! daemon's workspace manager and sqry-core's full graph-build
//! pipeline. Production code wraps
//! [`sqry_core::graph::unified::build::build_unified_graph`] via
//! [`RealWorkspaceBuilder`]; unit tests supply [`EmptyGraphBuilder`],
//! [`FailingGraphBuilder`], or a custom impl.
//!
//! Every build or load returns a [`BuiltGraph`]: the graph together with
//! the [`RosterRecord`] describing the plugin roster it was built with, so
//! the manager publishes the two in one step and the acquirer classifies
//! the served graph against the manifest (surface parity W1, design D3).
//!
//! The trait is `Send + Sync` because the builder is held across a
//! rebuild-dispatcher task boundary. Every concrete implementation
//! must be cheap to clone: callers typically `Arc`-wrap the builder
//! and share it across the reaper + dispatcher + lifecycle tasks.

use std::{path::Path, sync::Arc};

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::MacroOptionsRequest;
use sqry_core::plugin::PluginManager;
use sqry_plugin_registry::{RosterSource, resolve_persisted_selection};

use crate::error::DaemonError;
use crate::workspace::revision::{
    ArtifactKeyInputs, ArtifactPublishResult, DirtySnapshotOptions, DirtySnapshotSource,
    RawGitSource, RawGitSourceOptions, RevisionArtifactStore, VirtualSourceReader,
    materialize_virtual_source,
};
use crate::workspace::roster::{
    ManifestVerdict, ResolvedRoster, RosterRecord, WorkspaceRosterResolver, compare_ids,
    map_selection_error, shared_load_roster,
};
use sqry_daemon_protocol::{ArtifactId, ResolvedRevision};

#[cfg(test)]
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A graph plus the record of the roster it was built (or loaded) with.
#[derive(Debug)]
pub struct BuiltGraph {
    /// The graph.
    pub graph: CodeGraph,
    /// The roster the graph was built with. Published beside the graph.
    pub roster: Arc<RosterRecord>,
}

impl BuiltGraph {
    /// Pair a graph with its record.
    #[must_use]
    pub fn new(graph: CodeGraph, roster: Arc<RosterRecord>) -> Self {
        Self { graph, roster }
    }

    /// Pair a graph with the ids `plugins` registers. For builders that
    /// build with an explicit manager (test fakes, revision harnesses).
    #[must_use]
    pub fn with_manager(graph: CodeGraph, plugins: &PluginManager, source: RosterSource) -> Self {
        Self {
            graph,
            roster: Arc::new(RosterRecord::from_manager(plugins, source)),
        }
    }

    /// An empty graph recorded against the fast-path default roster. For
    /// fakes that return `CodeGraph::new()`.
    #[must_use]
    pub fn empty_fast_path() -> Self {
        Self {
            graph: CodeGraph::new(),
            roster: Arc::new(RosterRecord::fast_path_default()),
        }
    }
}

/// A build or a load whose inputs are resolved and accepted; calling it
/// builds (or loads) the graph. Returned by
/// [`WorkspaceBuilder::prepare_build`],
/// [`WorkspaceBuilder::prepare_build_with_macro_options`],
/// [`WorkspaceBuilder::prepare_durable_build`] and
/// [`WorkspaceBuilder::prepare_load_persisted`], so the manager can refuse
/// a request before it reserves memory and then build with what was
/// resolved, without resolving it again.
pub type PreparedBuild<'a> = Box<dyn FnOnce() -> Result<BuiltGraph, DaemonError> + 'a>;

/// Build a [`CodeGraph`] for the given workspace root.
///
/// Trait-object friendly; [`get_or_load`] and friends accept a
/// `&dyn WorkspaceBuilder` so the caller can choose between the
/// production [`RealWorkspaceBuilder`] and a test-local in-memory
/// variant without the manager caring.
///
/// The manager calls a `prepare_*` method before it reserves admission
/// headroom (whose LRU phase can evict sibling workspaces) and the
/// returned [`PreparedBuild`] after, so a request refused for its own
/// input evicts nothing. The defaults resolve nothing up front and defer to
/// [`Self::build_with_macro_options`] and [`Self::load_persisted`];
/// [`RealWorkspaceBuilder`] resolves the roster and the macro options (or
/// classifies the manifest, for a load) in the `prepare_*` call.
///
/// [`get_or_load`]: super::WorkspaceManager::get_or_load
pub trait WorkspaceBuilder: Send + Sync + std::fmt::Debug {
    /// Build the graph rooted at `workspace_root`. The implementation
    /// is responsible for any required lock-free / rayon parallelism
    /// and for honouring cancellation signals it consumes (e.g.
    /// pass-boundary checks in the rebuild pipeline).
    ///
    /// # Errors
    ///
    /// The daemon converts any returned [`DaemonError`] into a Failed
    /// workspace state + JSON-RPC `-32001 workspace_build_failed`.
    fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError>;

    /// Build the graph rooted at `workspace_root` with an explicit macro
    /// build options request (surface parity W4, design W4-D7, W4-D8): the
    /// daemon-hosted `rebuild_index` passes the `cfg_flags`, `expand_cache`
    /// and `reset_macro_options` its call carried. An empty request is
    /// [`Self::build`].
    ///
    /// The default refuses a non-empty request with
    /// [`DaemonError::InvalidArgument`]: a builder that does not resolve
    /// macro options (the test doubles) must never accept one silently.
    /// [`RealWorkspaceBuilder`] resolves it per root.
    ///
    /// # Errors
    ///
    /// As [`Self::build`], plus [`DaemonError::InvalidArgument`] for the
    /// default over a non-empty request; for a request the request check
    /// refuses (an empty, blank or padded cfg flag, an empty expand cache
    /// directory: [`crate::rebuild::validate_macro_request`]); and when the
    /// resolved expand cache directory's canonical path is not valid UTF-8,
    /// so the manifest could not record it.
    /// [`DaemonError::RebuildMacroOptionsUnavailable`] when the resolved
    /// expand cache directory cannot be used (missing, a file, a dangling
    /// link). Nothing is built or written on any of these.
    fn build_with_macro_options(
        &self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<BuiltGraph, DaemonError> {
        if !macro_request.is_empty() {
            return Err(DaemonError::InvalidArgument {
                reason: format!(
                    "this builder does not honour macro build options for {}",
                    workspace_root.display()
                ),
            });
        }
        self.build(workspace_root)
    }

    /// Read-only, persisted-graph rehydrate.
    ///
    /// SGA04 (shared graph acquisition, daemon provider): reload an
    /// existing valid persisted graph from `<workspace_root>/.sqry/graph/`
    /// **without** running [`Self::build`]: no parse, no plugin
    /// pipeline, no durable publish. Used by
    /// [`super::WorkspaceManager::reload_from_disk_read_only`] to fulfil
    /// the bounded one-shot eviction-reload contract for read-only
    /// queries (see `docs/development/shared-graph-acquisition/02_DESIGN.md`,
    /// "Bounded reload rule").
    ///
    /// The default impl returns [`DaemonError::WorkspaceBuildFailed`]
    /// with a reason of `"persisted graph rehydrate not implemented"`.
    /// Test fakes that don't need the read-only reload path keep that
    /// behaviour; production code uses [`RealWorkspaceBuilder`] which
    /// drives `GraphStorage::load_from_path` against the workspace's
    /// snapshot file with the full compiled roster.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::WorkspaceBuildFailed`] when no persisted graph
    ///   exists, when integrity verification fails, when the snapshot
    ///   format is incompatible, or when this builder does not support
    ///   read-only rehydrate.
    fn load_persisted(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        Err(DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: "persisted graph rehydrate not implemented for this builder".to_string(),
        })
    }

    /// Build from a virtual revision source by materializing regular raw
    /// source entries into a temporary parser input tree.
    ///
    /// `selection_root` is the repository root whose manifest chooses the
    /// roster: the materialised temporary tree has no manifest of its own,
    /// so resolving from it would always fall back to the fast path. The
    /// default impl builds the materialised tree with [`Self::build`] and
    /// does not consult `selection_root` (test fakes have no roster
    /// resolution); [`RealWorkspaceBuilder`] overrides it.
    ///
    /// Symlinks, gitlinks, deletions, too-large files, and unsupported states
    /// stay represented by the virtual source and are not followed or parsed.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if virtual source materialization fails or if
    /// the underlying filesystem graph build fails.
    fn build_virtual_source(
        &self,
        source: &dyn VirtualSourceReader,
        selection_root: &Path,
    ) -> Result<BuiltGraph, DaemonError> {
        let _ = selection_root;
        let materialized = materialize_virtual_source(source)?;
        self.build(materialized.root())
    }

    /// The roster record this builder would build `selection_root` with,
    /// resolved without building. Used to key revision artifacts on the
    /// roster they are actually built with. The default is the fast-path
    /// record (test fakes); [`RealWorkspaceBuilder`] resolves it from the
    /// manifest.
    ///
    /// # Errors
    ///
    /// Returns the resolver's error when the manifest is unreadable or
    /// names an uncompiled plugin id.
    fn roster_for(&self, selection_root: &Path) -> Result<Arc<RosterRecord>, DaemonError> {
        let _ = selection_root;
        Ok(Arc::new(RosterRecord::fast_path_default()))
    }

    /// The full compiled roster used to load snapshots and to classify
    /// manifest ids as loadable or unknown. Shared per process.
    fn load_roster(&self) -> Arc<PluginManager> {
        shared_load_roster()
    }

    /// Resolve every input [`Self::build_with_macro_options`] would refuse
    /// for `workspace_root`, without building, and return the build to run.
    ///
    /// The default resolves nothing and defers to
    /// [`Self::build_with_macro_options`] (a test double refuses, if at all,
    /// when the prepared build runs). [`RealWorkspaceBuilder`] resolves the
    /// roster and the macro options here and builds with them.
    ///
    /// # Errors
    ///
    /// What [`Self::build_with_macro_options`] refuses before building.
    fn prepare_build_with_macro_options<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        let root = workspace_root.to_path_buf();
        let request = macro_request.clone();
        Ok(Box::new(move || {
            self.build_with_macro_options(&root, &request)
        }))
    }

    /// The preparation of [`Self::build`]. The default resolves nothing
    /// and defers to [`Self::build`], so a builder (or a wrapper) that
    /// overrides `build` keeps that behaviour; [`RealWorkspaceBuilder`]
    /// prepares an empty request through
    /// [`Self::prepare_build_with_macro_options`].
    ///
    /// # Errors
    ///
    /// What [`Self::build`] refuses before building.
    fn prepare_build<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        let root = workspace_root.to_path_buf();
        Ok(Box::new(move || self.build(&root)))
    }

    /// Check every input [`Self::load_persisted`] would refuse for
    /// `workspace_root` (no persisted index, an unreadable manifest, an id
    /// this binary did not compile), without reading the snapshot, and
    /// return the load to run.
    ///
    /// The default checks nothing and defers to [`Self::load_persisted`].
    ///
    /// # Errors
    ///
    /// What [`Self::load_persisted`] refuses before reading the snapshot.
    fn prepare_load_persisted<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        let root = workspace_root.to_path_buf();
        Ok(Box::new(move || self.load_persisted(&root)))
    }

    /// [`Self::prepare_build_with_macro_options`] for a build that also
    /// persists the workspace's index: the snapshot and the manifest,
    /// recording the roster and the macro build options the graph was built
    /// with (or dropping the record, for a `reset`), through the durable
    /// persist path `daemon/rebuild` uses. The daemon-hosted `rebuild_index`
    /// loads a workspace that is not resident through this, so it records
    /// what it builds exactly as the standalone `rebuild_index` does.
    ///
    /// The default (test doubles, which have no index to write) is the
    /// in-memory [`Self::prepare_build_with_macro_options`];
    /// [`RealWorkspaceBuilder`] persists.
    ///
    /// # Errors
    ///
    /// What [`Self::prepare_build_with_macro_options`] refuses, plus, for
    /// [`RealWorkspaceBuilder`], [`DaemonError::RebuildWouldNarrowSelection`]
    /// for a roster that drops ids the manifest records.
    fn prepare_durable_build<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        self.prepare_build_with_macro_options(workspace_root, macro_request)
    }

    /// Whether a load through this builder that fails before it publishes
    /// leaves the workspace's index on disk as it found it, so the failure
    /// is its caller's own and the manager leaves no `Failed` slot behind
    /// (B2, round 7 audit). The daemon-hosted `rebuild_index`'s load
    /// route builds through the durable persist, whose refusals write
    /// nothing and whose failures put the old pair back, and answers its
    /// caller with the failure; a later query reads the index the failure
    /// left, as the standalone server does.
    ///
    /// The default (`false`) records the failure on the slot, as a failed
    /// `daemon/load` build does.
    fn failed_load_leaves_no_slot(&self) -> bool {
        false
    }
}

// Allow calling builders through an `Arc` so they can be shared
// between tasks without explicit `.as_ref()` spam at every call site.
impl<T: WorkspaceBuilder + ?Sized> WorkspaceBuilder for Arc<T> {
    fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        (**self).build(workspace_root)
    }

    fn build_with_macro_options(
        &self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<BuiltGraph, DaemonError> {
        (**self).build_with_macro_options(workspace_root, macro_request)
    }

    fn load_persisted(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        (**self).load_persisted(workspace_root)
    }

    fn build_virtual_source(
        &self,
        source: &dyn VirtualSourceReader,
        selection_root: &Path,
    ) -> Result<BuiltGraph, DaemonError> {
        (**self).build_virtual_source(source, selection_root)
    }

    fn roster_for(&self, selection_root: &Path) -> Result<Arc<RosterRecord>, DaemonError> {
        (**self).roster_for(selection_root)
    }

    fn load_roster(&self) -> Arc<PluginManager> {
        (**self).load_roster()
    }

    fn prepare_build_with_macro_options<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        (**self).prepare_build_with_macro_options(workspace_root, macro_request)
    }

    fn prepare_build<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        (**self).prepare_build(workspace_root)
    }

    fn prepare_load_persisted<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        (**self).prepare_load_persisted(workspace_root)
    }

    fn prepare_durable_build<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        (**self).prepare_durable_build(workspace_root, macro_request)
    }

    fn failed_load_leaves_no_slot(&self) -> bool {
        (**self).failed_load_leaves_no_slot()
    }
}

/// Test-only builder that always returns a freshly-built empty
/// [`CodeGraph`]. Useful for admission-accounting tests where the
/// actual graph content does not matter.
#[doc(hidden)]
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyGraphBuilder;

impl WorkspaceBuilder for EmptyGraphBuilder {
    fn build(&self, _workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        Ok(BuiltGraph::empty_fast_path())
    }
}

/// Test builder that always returns a configured error. Used by the
/// Failed-state unit tests in Phase 6c; shipping the type here so
/// every phase after 6b uses the same builder abstraction.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct FailingGraphBuilder {
    /// Reason string surfaced via [`DaemonError::WorkspaceBuildFailed`].
    pub reason: String,
}

impl FailingGraphBuilder {
    /// Construct a failing builder with the given reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl WorkspaceBuilder for FailingGraphBuilder {
    fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        Err(DaemonError::WorkspaceBuildFailed {
            root: workspace_root.to_path_buf(),
            reason: self.reason.clone(),
        })
    }
}

/// Test-only: a graph with `count` function nodes registered in the
/// indices, so one generation can be told from another by content
/// (surface parity W1 round 5). `publish_and_retain` takes the graph by
/// value and wraps it in a fresh `Arc`, so the graph half of a generation
/// has no fixed pointer to compare against; its node count is the
/// identity the round 5 oracles read. Not compiled into release builds.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
#[must_use]
pub fn graph_with_function_nodes(count: u32) -> CodeGraph {
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

/// Test-only builder double that publishes one distinguishable
/// generation: a graph with `nodes` function nodes
/// ([`graph_with_function_nodes`]) beside the record it holds, on `build`
/// and on `load_persisted` alike (surface parity W1 round 5: T47 in
/// `sqry-daemon/tests/rebuild_publish_hook_generation.rs` and T51 in
/// `mcp_host`). Not compiled into release builds.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
#[derive(Debug)]
pub struct FunctionGraphBuilder {
    /// Function nodes in every graph this builder returns.
    pub nodes: u32,
    /// The record published beside every graph this builder returns.
    pub record: Arc<RosterRecord>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl FunctionGraphBuilder {
    /// A builder whose generations carry `nodes` function nodes beside a
    /// fresh fast-path record.
    #[must_use]
    pub fn with_fast_path_record(nodes: u32) -> Self {
        Self {
            nodes,
            record: Arc::new(RosterRecord::fast_path_default()),
        }
    }

    fn generation(&self) -> BuiltGraph {
        BuiltGraph::new(
            graph_with_function_nodes(self.nodes),
            Arc::clone(&self.record),
        )
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl WorkspaceBuilder for FunctionGraphBuilder {
    fn build(&self, _workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        Ok(self.generation())
    }

    fn load_persisted(&self, _workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        Ok(self.generation())
    }
}

/// Production [`WorkspaceBuilder`] that delegates to
/// [`sqry_core::graph::unified::build::build_unified_graph`].
///
/// The daemon bootstrap constructs exactly one [`RealWorkspaceBuilder`]
/// per daemon process, sharing one [`WorkspaceRosterResolver`] with the
/// rebuild dispatcher. Every `build` resolves the roster for its root from
/// the workspace manifest (fast-path fallback for a root with no index);
/// every `load_persisted` loads with the full compiled roster and records
/// the selection the manifest carries. Tests inject [`EmptyGraphBuilder`]
/// or a custom builder.
pub struct RealWorkspaceBuilder {
    roster: Arc<WorkspaceRosterResolver>,
    build_config: sqry_core::graph::unified::build::BuildConfig,
}

impl std::fmt::Debug for RealWorkspaceBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealWorkspaceBuilder")
            .field("roster", &self.roster)
            .field("build_config", &self.build_config)
            .finish()
    }
}

impl RealWorkspaceBuilder {
    /// Construct a real builder with default
    /// [`sqry_core::graph::unified::build::BuildConfig`].
    #[must_use]
    pub fn new(roster: Arc<WorkspaceRosterResolver>) -> Self {
        Self {
            roster,
            build_config: sqry_core::graph::unified::build::BuildConfig::default(),
        }
    }

    /// Construct a real builder with a caller-supplied
    /// [`sqry_core::graph::unified::build::BuildConfig`].
    #[must_use]
    pub fn with_build_config(
        roster: Arc<WorkspaceRosterResolver>,
        build_config: sqry_core::graph::unified::build::BuildConfig,
    ) -> Self {
        Self {
            roster,
            build_config,
        }
    }

    /// The resolver this builder resolves rosters through.
    #[must_use]
    pub fn roster_resolver(&self) -> &Arc<WorkspaceRosterResolver> {
        &self.roster
    }

    /// Run the build pipeline over `build_root` with an already resolved
    /// roster. Shared by [`WorkspaceBuilder::build`] (roster resolved from
    /// `build_root`) and [`WorkspaceBuilder::build_virtual_source`]
    /// (roster resolved from the repository root, tree materialised
    /// elsewhere).
    fn build_with_roster(
        &self,
        build_root: &Path,
        resolved: &ResolvedRoster,
        macro_options: sqry_core::graph::unified::build::MacroBuildOptions,
    ) -> Result<BuiltGraph, DaemonError> {
        let config = sqry_core::graph::unified::build::BuildConfig {
            macro_options,
            ..self.build_config.clone()
        };
        sqry_core::graph::unified::build::build_unified_graph(
            build_root,
            &resolved.plugins,
            &config,
        )
        .map(|graph| BuiltGraph::new(graph, Arc::clone(&resolved.record)))
        .map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: build_root.to_path_buf(),
            reason: e.to_string(),
        })
    }

    /// The macro build options a workspace build at `workspace_root` runs
    /// with (surface parity W4, W4-D7): the manifest's record overlaid by
    /// `macro_request`, which is checked on its own first
    /// ([`crate::rebuild::validate_macro_request`]). The roster resolve
    /// before this refused an unreadable manifest already, so the rule here
    /// is refuse for the record.
    fn macro_options_for(
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<sqry_core::graph::unified::build::MacroBuildOptions, DaemonError> {
        crate::rebuild::validate_macro_request(workspace_root, macro_request)?;
        sqry_core::graph::unified::build::resolve_macro_options(
            workspace_root,
            macro_request,
            sqry_core::graph::unified::build::UnreadableManifestRule::Refuse,
        )
        .map(|resolved| resolved.options)
        .map_err(|err| crate::rebuild::map_macro_options_err(err, workspace_root, macro_request))
    }

    /// Build a graph from an immutable raw Git tree source.
    ///
    /// This path reads Git blob objects directly, materializes only eligible
    /// regular blobs into a temporary parser input tree, and then reuses the
    /// existing graph build pipeline. It never builds from checkout-filtered
    /// worktree bytes. The roster is resolved from `options.repo_root`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionObjectMissing`] for locally missing Git
    /// objects, [`DaemonError::RevisionSourceUnavailable`] for source material
    /// failures, or [`DaemonError::WorkspaceBuildFailed`] for graph build
    /// failures.
    pub fn build_raw_git_tree(
        &self,
        options: RawGitSourceOptions,
    ) -> Result<BuiltGraph, DaemonError> {
        let selection_root = options.repo_root.clone();
        let source = RawGitSource::open(options)?;
        self.build_virtual_source(&source, &selection_root)
    }

    /// Capture and build a resident-only dirty snapshot graph.
    ///
    /// Dirty snapshots are not published to the immutable artifact store by this
    /// method. The caller can use [`DirtySnapshotSource::fingerprint`] to create
    /// a resident revision identity. The roster is resolved from
    /// `options.repo_root`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::DirtySnapshotChanged`] when the worktree mutates
    /// repeatedly during capture, or a build/source error when captured bytes
    /// cannot be materialized or parsed.
    pub fn build_dirty_snapshot(
        &self,
        options: &DirtySnapshotOptions,
    ) -> Result<(BuiltGraph, DirtySnapshotSource), DaemonError> {
        for attempt in 0..=1 {
            let source = DirtySnapshotSource::capture_once(options)?;
            let built = self.build_virtual_source(&source, &options.repo_root)?;
            let validation = DirtySnapshotSource::capture_once(options)?;
            if source.fingerprint().snapshot_digest == validation.fingerprint().snapshot_digest {
                return Ok((built, source));
            }
            if attempt == 1 {
                return Err(DaemonError::DirtySnapshotChanged {
                    root: options.repo_root.clone(),
                });
            }
        }
        unreachable!("dirty snapshot build returns on success or final retry failure")
    }

    /// Build and publish an immutable raw Git revision artifact.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if raw source traversal, graph construction, or
    /// artifact publication fails.
    pub fn build_and_publish_raw_git_artifact(
        &self,
        source_options: RawGitSourceOptions,
        store: &RevisionArtifactStore,
        artifact_id: &ArtifactId,
        resolved_revision: ResolvedRevision,
        key_inputs: ArtifactKeyInputs,
    ) -> Result<ArtifactPublishResult, DaemonError> {
        let built = self.build_raw_git_tree(source_options)?;
        store.publish_graph(
            &built.graph,
            artifact_id,
            resolved_revision,
            key_inputs,
            None,
        )
    }
}

impl WorkspaceBuilder for RealWorkspaceBuilder {
    fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        self.build_with_macro_options(workspace_root, &MacroOptionsRequest::empty())
    }

    fn build_with_macro_options(
        &self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<BuiltGraph, DaemonError> {
        self.prepare_build_with_macro_options(workspace_root, macro_request)?()
    }

    /// Resolve the roster the manifest at `workspace_root` records and the
    /// macro build options `macro_request` asks for over the manifest's
    /// record, and return the build that runs with them. Every refusal
    /// (an unreadable manifest, an id this binary did not compile, an
    /// empty, missing or unrecordable expand cache) is raised here, before
    /// the manager reserves memory; the build resolves nothing again.
    fn prepare_build_with_macro_options<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        let resolved = self.roster.resolve(workspace_root)?;
        let macro_options = Self::macro_options_for(workspace_root, macro_request)?;
        let build_root = workspace_root.to_path_buf();
        Ok(Box::new(move || {
            self.build_with_roster(&build_root, &resolved, macro_options)
        }))
    }

    fn build_virtual_source(
        &self,
        source: &dyn VirtualSourceReader,
        selection_root: &Path,
    ) -> Result<BuiltGraph, DaemonError> {
        // A revision graph is built over a materialised tree that has no
        // manifest of its own; whether it should inherit the workspace's
        // recorded macro options is the same question the in-memory diff
        // builders raise (W4 design section 5, handed to W3), so it builds
        // with the default options as before.
        let resolved = self.roster.resolve(selection_root)?;
        let materialized = materialize_virtual_source(source)?;
        self.build_with_roster(
            materialized.root(),
            &resolved,
            sqry_core::graph::unified::build::MacroBuildOptions::default(),
        )
    }

    fn roster_for(&self, selection_root: &Path) -> Result<Arc<RosterRecord>, DaemonError> {
        Ok(self.roster.resolve(selection_root)?.record)
    }

    fn load_roster(&self) -> Arc<PluginManager> {
        Arc::clone(self.roster.load_roster())
    }

    fn prepare_build<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        self.prepare_build_with_macro_options(workspace_root, &MacroOptionsRequest::empty())
    }

    /// Resolve the roster, the macro build options and the narrowing guard
    /// as `daemon/rebuild` does (`crate::rebuild::resolve_durable_rebuild_inputs`),
    /// before the manager reserves memory, and return the build that builds
    /// and persists with them (`crate::rebuild::build_and_persist_blocking`).
    fn prepare_durable_build<'a>(
        &'a self,
        workspace_root: &Path,
        macro_request: &MacroOptionsRequest,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        // The daemon-hosted `rebuild_index`, a caller's explicit request to
        // replace the index, falls back over a manifest it cannot read
        // (design D9).
        let inputs = crate::rebuild::resolve_durable_rebuild_inputs(
            &self.roster,
            &self.build_config,
            workspace_root,
            macro_request,
            None,
            sqry_plugin_registry::UnreadableManifestPolicy::FallBack,
        )?;
        let root = workspace_root.to_path_buf();
        Ok(Box::new(move || {
            crate::rebuild::build_and_persist_blocking(&root, &inputs, "daemon:rebuild_index")
        }))
    }

    /// SGA04 read-only persisted-graph rehydrate.
    ///
    /// Loads `<workspace_root>/.sqry/graph/snapshot.sqry` via
    /// [`sqry_core::graph::unified::persistence::load_from_path`] using
    /// the full compiled roster, so a snapshot recording a plugin this
    /// binary compiled but does not build with by default (for example
    /// `json`) loads instead of failing as "not installed" (#314). The
    /// record published beside the graph is the selection the manifest
    /// carries. Never invokes the build pipeline; never writes any artifact.
    ///
    /// Surface parity W1 round 2 (design D10): before the snapshot is read,
    /// the manifest's `active_plugin_ids` are classified against the load
    /// roster with [`compare_ids`], and a manifest naming an id this binary
    /// did not compile is refused as [`DaemonError::WorkspaceIncompatibleGraph`]
    /// (`-32005`), so a graph is never published beside a record naming a
    /// plugin the daemon cannot serve. This is where the load roster is
    /// observable: the manager handed to `load_from_path` feeds only the
    /// snapshot header's plugin-version check, and no production writer
    /// stamps that header (`persist_durable_graph_transaction` writes through
    /// `save_to_path`), so that argument is inert on every real index. The
    /// acquirer's per-acquisition check stays, because the manifest can
    /// change under a resident graph.
    ///
    /// A root with no manifest, or with a manifest and no snapshot, is
    /// refused as [`DaemonError::WorkspaceNotIndexed`] (`-32001`) naming the
    /// absent file and the `sqry index` invocation that creates it.
    fn load_persisted(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
        self.prepare_load_persisted(workspace_root)?()
    }

    /// The checks [`WorkspaceBuilder::load_persisted`] makes before it reads
    /// the snapshot (an index and a snapshot exist, the manifest is
    /// readable, it names no id this binary did not compile), made before
    /// the manager reserves memory for the reload; the returned load reads
    /// the snapshot.
    fn prepare_load_persisted<'a>(
        &'a self,
        workspace_root: &Path,
    ) -> Result<PreparedBuild<'a>, DaemonError> {
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
        // An absent index is its own refusal, naming the absent file and
        // the `sqry index` invocation that creates it, so the acquirer can
        // tell "never indexed" from a load that failed (it answered every
        // reload failure as `-32004` "evicted mid-rebuild" before).
        if !storage.exists() {
            return Err(DaemonError::workspace_not_indexed(
                workspace_root,
                storage.manifest_path().to_path_buf(),
            ));
        }
        if !storage.snapshot_exists() {
            return Err(DaemonError::workspace_snapshot_missing(
                workspace_root,
                storage.snapshot_path().to_path_buf(),
            ));
        }
        // `storage.exists()` held, so the manifest is present; `None` here
        // would mean the file vanished between the two reads.
        let record = match resolve_persisted_selection(workspace_root) {
            Ok(Some(selection)) => Arc::new(RosterRecord::from_selection(&selection)),
            Ok(None) => {
                return Err(DaemonError::WorkspaceBuildFailed {
                    root: workspace_root.to_path_buf(),
                    reason: format!(
                        "manifest at {} disappeared during load",
                        storage.manifest_path().display()
                    ),
                });
            }
            // Surface parity W1 round 3 (design D15): the same mapping the
            // resolver gives `build`, so an unreadable manifest reaches the
            // reload's caller as `WorkspaceManifestUnreadable` (`-32001`
            // naming the file and the repair) and an unknown id as
            // `WorkspaceIncompatibleGraph`, instead of the generic build
            // failure whose Display carried the registry's text and whose
            // `error.data` had no `manifest_path` or `repair_command`.
            Err(err) => {
                return Err(map_selection_error(workspace_root, &err));
            }
        };
        let load_roster = self.roster.load_roster();
        // `compare_ids` with the record as both sides yields `Exact` or
        // `UnknownIds` only; the divergence arms cannot fire here.
        if let ManifestVerdict::UnknownIds { unknown_plugin_ids } =
            compare_ids(&record.active_plugin_ids, &record, load_roster)
        {
            return Err(DaemonError::WorkspaceIncompatibleGraph {
                root: workspace_root.to_path_buf(),
                reason: format!(
                    "unknown plugin ids: [{}] (manifest: {})",
                    unknown_plugin_ids.join(", "),
                    storage.manifest_path().display()
                ),
            });
        }
        let workspace_root = workspace_root.to_path_buf();
        Ok(Box::new(move || {
            sqry_core::graph::unified::persistence::load_from_path(
                storage.snapshot_path(),
                Some(load_roster),
            )
            .map(|graph| BuiltGraph::new(graph, record))
            .map_err(|err| snapshot_load_refusal(&workspace_root, storage.snapshot_path(), err))
        }))
    }
}

/// The refusal for a persisted snapshot that did not load.
///
/// - A snapshot format or plugin roster this binary cannot read is
///   [`DaemonError::WorkspaceIncompatibleGraph`] (`-32005`), not a build
///   failure a retry could clear (SGA04 Major #2, codex iter2): the
///   dispatcher surfaces it as an upgrade problem.
/// - Any other failure (corrupt, truncated, failing its integrity check,
///   unreadable) is [`DaemonError::WorkspaceSnapshotUnreadable`] naming the
///   file and the repair, so the acquirer reloads once the snapshot has
///   been rewritten (integration round 7, DAEMON_FOLLOWUP).
fn snapshot_load_refusal(
    workspace_root: &Path,
    snapshot_path: &Path,
    err: sqry_core::graph::unified::persistence::PersistenceError,
) -> DaemonError {
    use sqry_core::graph::unified::persistence::PersistenceError;
    match err {
        PersistenceError::IncompatibleVersion { expected, found } => {
            DaemonError::WorkspaceIncompatibleGraph {
                root: workspace_root.to_path_buf(),
                reason: format!("snapshot version mismatch: expected {expected}, found {found}"),
            }
        }
        // A snapshot header naming a plugin the full compiled roster lacks
        // is the same class: this binary cannot serve it.
        mismatch @ PersistenceError::PluginVersionMismatch { .. } => {
            DaemonError::WorkspaceIncompatibleGraph {
                root: workspace_root.to_path_buf(),
                reason: format!("snapshot plugin mismatch: {mismatch}"),
            }
        }
        other => DaemonError::WorkspaceSnapshotUnreadable {
            root: workspace_root.to_path_buf(),
            snapshot_path: snapshot_path.to_path_buf(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_returns_fresh_graph() {
        let b = EmptyGraphBuilder;
        let g = b.build(Path::new("/repos/example")).expect("always ok");
        assert_eq!(g.graph.node_count(), 0);
        assert_eq!(g.roster.source, RosterSource::Fallback);
        assert!(!g.roster.contains("json"));
    }

    #[test]
    fn failing_builder_surfaces_reason_and_root() {
        let b = FailingGraphBuilder::new("plugin panic");
        let err = b
            .build(Path::new("/repos/example"))
            .expect_err("always fails");
        match err {
            DaemonError::WorkspaceBuildFailed { root, reason } => {
                assert_eq!(root, Path::new("/repos/example"));
                assert_eq!(reason, "plugin panic");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The preparation defaults defer to the methods a builder overrides:
    /// a builder that overrides only `build` runs its own `build` through
    /// `prepare_build`, and one that overrides only `load_persisted` runs
    /// it through `prepare_load_persisted`. A wrapper overriding `build`
    /// and losing its behaviour through the preparation is the hazard this
    /// pins.
    #[test]
    fn the_preparation_defaults_run_the_overridden_methods() {
        #[derive(Debug)]
        struct NodesDouble;
        impl WorkspaceBuilder for NodesDouble {
            fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
                Ok(BuiltGraph::new(
                    graph_with_function_nodes(7),
                    Arc::new(RosterRecord::fast_path_default()),
                ))
            }
            fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
                Ok(BuiltGraph::new(
                    graph_with_function_nodes(11),
                    Arc::new(RosterRecord::fast_path_default()),
                ))
            }
        }
        let root = Path::new("/repos/example");
        let built = NodesDouble.prepare_build(root).expect("prepares")().expect("builds");
        assert_eq!(built.graph.node_count(), 7);
        let loaded = NodesDouble.prepare_load_persisted(root).expect("prepares")().expect("loads");
        assert_eq!(loaded.graph.node_count(), 11);
    }

    /// `RealWorkspaceBuilder` refuses in the preparation itself, before any
    /// build runs (the manager reserves memory in between): a missing
    /// expand cache, an unreadable manifest, and for a load, no index.
    #[test]
    fn the_real_builder_refuses_in_its_preparation() {
        use tempfile::TempDir;
        let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let missing = MacroOptionsRequest {
            expand_cache_dir: Some(root.join("no-such-cache")),
            ..MacroOptionsRequest::empty()
        };
        assert!(matches!(
            builder.prepare_build_with_macro_options(&root, &missing),
            Err(DaemonError::RebuildMacroOptionsUnavailable { .. })
        ));
        assert!(matches!(
            builder.prepare_load_persisted(&root),
            Err(DaemonError::WorkspaceNotIndexed { .. })
        ));
        // S6 (round 7): a request the request check refuses.
        let padded = MacroOptionsRequest {
            cfg_flags: Some(vec![" test".to_string()]),
            ..MacroOptionsRequest::empty()
        };
        assert!(matches!(
            builder.prepare_build_with_macro_options(&root, &padded),
            Err(DaemonError::InvalidArgument { .. })
        ));
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"{ not json").expect("corrupt manifest");
        assert!(matches!(
            builder.prepare_build(&root),
            Err(DaemonError::WorkspaceManifestUnreadable { .. })
        ));
    }

    #[test]
    fn arc_builder_passes_through_to_inner() {
        let inner: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
        let g = inner
            .build(Path::new("/repos/example"))
            .expect("arc-wrapped builder delegates");
        assert_eq!(g.graph.node_count(), 0);
        assert!(inner.load_roster().plugin_by_id("json").is_some());
    }

    /// SGA04 Major #2 (codex iter2): when the persisted snapshot
    /// reports an incompatible format version, `load_persisted` must
    /// return [`DaemonError::WorkspaceIncompatibleGraph`] (which the
    /// dispatcher exposes as JSON-RPC -32005), **not** the generic
    /// transient-build [`DaemonError::WorkspaceBuildFailed`] (-32001).
    /// We hand-craft a snapshot file with the current V10 magic but a
    /// `GraphHeader.version` of `99` to force
    /// `PersistenceError::IncompatibleVersion`.
    #[test]
    fn real_workspace_builder_load_persisted_incompatible_snapshot_returns_incompatible_graph_error()
     {
        use sha2::{Digest, Sha256};
        use sqry_core::graph::unified::persistence::{
            BuildProvenance, GraphHeader, GraphStorage, MAGIC_BYTES_V10, MANIFEST_SCHEMA_VERSION,
            Manifest, PluginSelectionManifest, SNAPSHOT_FORMAT_VERSION,
        };
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().to_path_buf();
        let storage = GraphStorage::new(&workspace);
        fs::create_dir_all(storage.graph_dir()).expect("graph dir");

        // Build V10-magic + bogus-version-99 header bytes.
        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = 99;
        let header_bytes = postcard::to_allocvec(&header).expect("encode header");
        let mut bytes: Vec<u8> = Vec::with_capacity(14 + 4 + header_bytes.len() + 8);
        bytes.extend_from_slice(MAGIC_BYTES_V10);
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        bytes.extend_from_slice(&0u64.to_le_bytes());
        fs::write(storage.snapshot_path(), &bytes).expect("write snapshot");

        // Stub manifest pointing at the bogus snapshot SHA so the
        // GraphStorage `exists()` / `snapshot_exists()` precondition
        // checks pass and `load_persisted` actually reaches
        // `load_from_path`.
        let snapshot_sha256 = hex_lower(&Sha256::digest(&bytes));
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            built_at: "1970-01-01T00:00:00Z".to_string(),
            root_path: workspace.to_string_lossy().into_owned(),
            node_count: 0,
            edge_count: 0,
            raw_edge_count: None,
            snapshot_sha256,
            build_provenance: BuildProvenance {
                sqry_version: env!("CARGO_PKG_VERSION").to_string(),
                build_timestamp: "1970-01-01T00:00:00Z".to_string(),
                build_command: "test:incompatible-version".to_string(),
                plugin_hashes: std::collections::HashMap::new(),
            },
            file_count: std::collections::HashMap::new(),
            languages: Vec::new(),
            config: std::collections::HashMap::new(),
            confidence: Default::default(),
            last_indexed_commit: None,
            plugin_selection: Some(PluginSelectionManifest {
                active_plugin_ids: Vec::new(),
                high_cost_mode: None,
            }),
            macro_options: None,
        };
        manifest
            .save(storage.manifest_path())
            .expect("save manifest");

        let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
        let err = builder
            .load_persisted(&workspace)
            .expect_err("incompatible-version snapshot must fail load_persisted");

        match err {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, workspace);
                assert!(
                    reason.contains("snapshot version mismatch") && reason.contains("found 99"),
                    "expected snapshot version mismatch diagnostic, got {reason:?}"
                );
            }
            other => panic!("expected DaemonError::WorkspaceIncompatibleGraph, got {other:?}"),
        }
    }

    #[derive(Debug)]
    struct RecordingBuilder;

    impl WorkspaceBuilder for RecordingBuilder {
        fn build(&self, workspace_root: &Path) -> Result<BuiltGraph, DaemonError> {
            let source = std::fs::read(workspace_root.join("src/lib.rs")).map_err(|err| {
                DaemonError::WorkspaceBuildFailed {
                    root: workspace_root.to_path_buf(),
                    reason: err.to_string(),
                }
            })?;
            assert_eq!(source, b"fn main() {}\n");
            assert!(!workspace_root.join("link").exists());
            Ok(BuiltGraph::empty_fast_path())
        }
    }

    #[derive(Debug)]
    struct FakeVirtualSource {
        entries: Vec<crate::workspace::revision::VirtualSourceEntry>,
    }

    impl crate::workspace::revision::VirtualSourceReader for FakeVirtualSource {
        fn entries(&self) -> &[crate::workspace::revision::VirtualSourceEntry] {
            &self.entries
        }

        fn read_entry_bytes(
            &self,
            entry: &crate::workspace::revision::VirtualSourceEntry,
        ) -> Result<Vec<u8>, DaemonError> {
            if entry.path.display_lossy() == "src/lib.rs" {
                Ok(b"fn main() {}\n".to_vec())
            } else {
                unreachable!("non-regular virtual entries must not be read")
            }
        }
    }

    #[test]
    fn workspace_builder_materializes_virtual_regular_files_only() {
        use crate::workspace::revision::{VirtualPath, VirtualSourceEntry, VirtualSourceKind};

        let source = FakeVirtualSource {
            entries: vec![
                VirtualSourceEntry::new(
                    VirtualPath::from_git_path_bytes(b"src/lib.rs".to_vec()).unwrap(),
                    VirtualSourceKind::RegularFile,
                    Some("a".repeat(40)),
                    Some(13),
                ),
                VirtualSourceEntry::new(
                    VirtualPath::from_git_path_bytes(b"link".to_vec()).unwrap(),
                    VirtualSourceKind::Symlink,
                    Some("b".repeat(40)),
                    Some(10),
                ),
            ],
        };

        let built = RecordingBuilder
            .build_virtual_source(&source, Path::new("/repos/example"))
            .expect("virtual source build should delegate through temp tree");
        assert_eq!(built.graph.node_count(), 0);
    }

    /// The real builder resolves the roster of a virtual source from the
    /// repository root, not from the materialised temporary tree (which
    /// has no manifest and would always fall back to the fast path).
    #[test]
    fn real_builder_resolves_virtual_source_roster_from_selection_root() {
        use crate::workspace::revision::{VirtualPath, VirtualSourceEntry, VirtualSourceKind};
        use sqry_core::graph::unified::persistence::{
            BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
        };
        use tempfile::TempDir;

        let repo = TempDir::new().expect("tempdir");
        let storage = GraphStorage::new(repo.path());
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        Manifest::new(
            repo.path().to_string_lossy().to_string(),
            0,
            0,
            "fixture-sha256",
            BuildProvenance::new("test", "test"),
        )
        .with_plugin_selection(Some(PluginSelectionManifest {
            active_plugin_ids: vec!["rust".to_string(), "json".to_string()],
            high_cost_mode: Some("include_all".to_string()),
        }))
        .save(storage.manifest_path())
        .expect("manifest saved");

        let source = FakeVirtualSource {
            entries: vec![VirtualSourceEntry::new(
                VirtualPath::from_git_path_bytes(b"src/lib.rs".to_vec()).unwrap(),
                VirtualSourceKind::RegularFile,
                Some("a".repeat(40)),
                Some(13),
            )],
        };
        let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
        let built = builder
            .build_virtual_source(&source, repo.path())
            .expect("virtual build resolves from the repo root");
        assert_eq!(
            built.roster.active_plugin_ids,
            vec!["rust".to_string(), "json".to_string()]
        );
        assert_eq!(built.roster.source, RosterSource::PersistedManifest);
        assert_eq!(
            builder
                .roster_for(repo.path())
                .expect("roster_for resolves")
                .active_plugin_ids,
            vec!["rust".to_string(), "json".to_string()]
        );
    }
}
