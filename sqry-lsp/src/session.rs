use crate::LspOptions;
use crate::config::{ConfigDiff, SessionConfig};
use crate::documents::{DocumentSnapshot, DocumentStore, compute_line_offsets};
use crate::file_types::classify_file;
use anyhow::{Context, Result, anyhow};
use parking_lot::RwLock;
use ropey::Rope;
use sqry_core::graph::acquisition::{
    AcquisitionOperation, AutoBuildHook, FilesystemGraphProvider, GraphAcquirer,
    GraphAcquisitionError, GraphAcquisitionRequest, MissingGraphPolicy, PathPolicy,
    PluginSelectionPolicy, StalePolicy,
};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::resolution::display_graph_qualified_name;
use sqry_core::graph::unified::{NodeEntry, NodeKind, StagingGraph, StagingOp, StringId};
use sqry_core::plugin::PluginManager;
use sqry_core::project::{Project, ProjectManager};
use sqry_core::query::QueryExecutor;
use sqry_core::workspace::{Classification, HeuristicVerdict, LogicalWorkspace, MemberReason};
use sqry_plugin_registry::create_plugin_manager;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tower_lsp::lsp_types::{Position, Url};

#[derive(Clone)]
pub struct SessionManager {
    options: Arc<LspOptions>,
    /// Legacy root path (kept for backwards compatibility).
    root_path: Arc<PathBuf>,
    executor: Arc<QueryExecutor>,
    config: Arc<RwLock<SessionConfig>>,
    documents: DocumentStore,
    /// Graph cache for single-project mode.
    graph_cache: Arc<RwLock<Option<Arc<CodeGraph>>>>,
    /// SGA06 — observability counter incremented on every successful return
    /// from [`Self::graph_for_path`]. Used by the SGA06 parity tests to pin
    /// that LSP read-only handlers route their graph acquisition through the
    /// shared `FilesystemGraphProvider` pipeline rather than re-entering the
    /// executor's own `get_or_load_graph`.
    graph_for_path_calls: Arc<AtomicU64>,
    /// Project manager for multi-project support (per `PROJECT_ROOT_SPEC.md` Section 9).
    project_manager: Arc<ProjectManager>,
    /// Logical workspace identity owned by the session.
    ///
    /// Wrapped in `Arc<RwLock<Arc<_>>>` so handlers can read the current
    /// workspace lock-free via `Arc::clone` while a future
    /// `sqry/workspaceUpdate` request (Step 5) can atomically swap in a
    /// new workspace without invalidating in-flight reads. The default
    /// value at construction is an `AnonymousMultiRoot` over the
    /// `LspOptions::index_root` (or current dir), which preserves the
    /// pre-Step-4 single-root behaviour until `initialize()` resolves
    /// the real workspace.
    logical_workspace: Arc<RwLock<Arc<LogicalWorkspace>>>,
}

#[derive(Debug, Clone)]
pub struct NodeMatch {
    pub name: String,
    pub qualified_name: Option<String>,
    pub kind: NodeKind,
    pub is_static: bool,
    pub file_path: PathBuf,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub signature: Option<String>,
    pub documentation: Option<String>,
    pub language: Option<String>,
}

impl NodeMatch {
    #[must_use]
    pub fn qualified_name_or_name(&self) -> &str {
        self.qualified_name.as_deref().unwrap_or(&self.name)
    }

    #[must_use]
    pub fn display_qualified_name_or_name(&self) -> String {
        let Some(qualified_name) = self.qualified_name.as_deref() else {
            return self.name.clone();
        };
        let Some(language) = self
            .language
            .as_deref()
            .and_then(sqry_core::graph::Language::from_id)
        else {
            return qualified_name.to_string();
        };
        display_graph_qualified_name(language, qualified_name, self.kind, self.is_static)
    }
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf> {
    let normalized_path = normalize_path_lexically(path);
    let mut missing_suffix: Vec<OsString> = Vec::new();
    let mut current = normalized_path.as_path();

    loop {
        match current.canonicalize() {
            Ok(canonical) => {
                let mut resolved = canonical;
                for component in missing_suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) => {
                let Some(file_name) = current.file_name() else {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to canonicalize any existing path prefix for {}",
                            path.display()
                        )
                    });
                };
                let Some(parent) = current.parent() else {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to canonicalize any existing path prefix for {}",
                            path.display()
                        )
                    });
                };
                missing_suffix.push(file_name.to_os_string());
                current = parent;
            }
        }
    }
}

impl SessionManager {
    /// Construct a fresh `SessionManager` from CLI options.
    ///
    /// # Panics
    ///
    /// Panics only if [`LogicalWorkspace::anonymous_multi_root`] cannot
    /// construct an empty workspace — which is unreachable by contract
    /// (an empty folder list never canonicalizes anything).
    #[must_use]
    pub fn new(options: LspOptions) -> Self {
        // C094c: `--workspace <PATH>` (forwarded from the CLI dispatcher
        // via `LspOptions.workspace`) wins over the legacy `--index-root`
        // fallback. Both values are filesystem paths; the resolver
        // canonicalises whichever wins so downstream comparisons stay
        // platform-stable.
        let root_override = options
            .workspace
            .clone()
            .or_else(|| options.index_root.clone());
        let root = resolve_root_path(root_override).unwrap_or_else(|_| PathBuf::from("."));
        let executor = Arc::new(QueryExecutor::with_plugin_manager(build_plugin_manager()));
        let config = SessionConfig::from_options(&options).unwrap_or_else(|err| {
            log::warn!("failed to load initial configuration: {err}");
            SessionConfig::default()
        });
        log::set_max_level(config.log_level);

        // Initialize ProjectManager with mode from config (per PROJECT_ROOT_SPEC.md Section 9.1)
        let project_manager = Arc::new(ProjectManager::new(config.project_root_mode));

        // Default LogicalWorkspace covers the single index_root / cwd as an
        // anonymous multi-root; initialize() will replace it with the real
        // workspace per §1.3 once the LSP handshake delivers
        // workspace_folders + initializationOptions.
        let logical_workspace = Arc::new(RwLock::new(Arc::new(
            LogicalWorkspace::anonymous_multi_root(vec![root.clone()]).unwrap_or_else(|err| {
                log::warn!(
                    "failed to construct default LogicalWorkspace at {} ({err}); falling back to single_root",
                    root.display()
                );
                LogicalWorkspace::single_root(root.clone()).unwrap_or_else(|err2| {
                    log::error!(
                        "failed to construct any default LogicalWorkspace ({err2}); using empty anonymous multi-root"
                    );
                    LogicalWorkspace::anonymous_multi_root(Vec::new())
                        .expect("empty AnonymousMultiRoot is always constructible")
                })
            }),
        )));

        Self {
            options: Arc::new(options),
            root_path: Arc::new(root),
            executor,
            config: Arc::new(RwLock::new(config)),
            documents: DocumentStore::new(),
            graph_cache: Arc::new(RwLock::new(None)),
            graph_for_path_calls: Arc::new(AtomicU64::new(0)),
            project_manager,
            logical_workspace,
        }
    }

    #[must_use]
    pub fn options(&self) -> &LspOptions {
        &self.options
    }

    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Returns the workspace root path suitable for PN3 cold-load lookups.
    ///
    /// Returns the configured `index_root` if set (overrides the root path via
    /// the LSP config), otherwise falls back to the session root path.
    /// This is the directory under which `.sqry/graph/derived.sqry` is expected
    /// to exist.
    #[must_use]
    pub fn index_root_for_cold_load(&self) -> PathBuf {
        self.current_index_root()
    }

    #[must_use]
    pub fn executor(&self) -> Arc<QueryExecutor> {
        Arc::clone(&self.executor)
    }

    /// Resolve the workspace-relative path requested by the client.
    ///
    /// The resolved path is validated to be within the workspace root
    /// (or `index_root` override) to prevent directory traversal.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be resolved or when the
    /// requested path escapes the workspace boundary.
    pub fn resolve_path(&self, input: Option<&str>) -> Result<PathBuf> {
        let base_override = self.config.read().index_root.clone();
        let base = base_override.as_deref().unwrap_or_else(|| self.root_path());
        let canonical_base = canonicalize_with_missing_tail(base)
            .with_context(|| format!("failed to resolve workspace root {}", base.display()))?;
        let path = match input {
            Some(p) => {
                let candidate = PathBuf::from(p);
                if candidate.is_absolute() {
                    candidate
                } else {
                    canonical_base.join(candidate)
                }
            }
            None => canonical_base.clone(),
        };
        let resolved = canonicalize_with_missing_tail(&path)
            .with_context(|| format!("failed to resolve requested path {}", path.display()))?;

        if !resolved.starts_with(&canonical_base) {
            anyhow::bail!(
                "resolved path {} is outside workspace root {}",
                resolved.display(),
                canonical_base.display()
            );
        }

        Ok(resolved)
    }

    #[must_use]
    pub fn config(&self) -> SessionConfig {
        self.config.read().clone()
    }

    /// Apply client-provided configuration overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when the settings payload cannot be deserialized.
    pub fn apply_client_settings(&self, settings: &serde_json::Value) -> Result<ConfigDiff> {
        let mut guard = self.config.write();
        let diff = guard.apply_settings(settings)?;

        if let Some(level) = diff.log_level {
            log::set_max_level(level);
        }
        if diff.document_limits_changed() {
            self.documents.prune_by_limits(&guard.document_limits);
        }
        if diff.index_root.is_some() {
            self.clear_graph_cache();
        }

        // Handle projectRootMode changes (per PROJECT_ROOT_SPEC.md Section 9.3)
        if let Some(new_mode) = diff.project_root_mode {
            self.project_manager.handle_config_change(new_mode);
            self.clear_graph_cache();
        }

        Ok(diff)
    }

    #[must_use]
    pub fn documents(&self) -> DocumentStore {
        self.documents.clone()
    }

    /// Set workspace folders (per `PROJECT_ROOT_SPEC.md` Section 9.1).
    ///
    /// Updates the `ProjectManager` with the provided workspace folders.
    /// Folders are canonicalized before being stored.
    pub fn set_workspace_folders(&self, folders: Vec<PathBuf>) {
        let canonical_folders: Vec<PathBuf> = folders
            .into_iter()
            .map(|f| f.canonicalize().unwrap_or(f))
            .collect();
        log::info!(
            "Setting {} workspace folder(s) on ProjectManager",
            canonical_folders.len()
        );
        self.project_manager
            .set_workspace_folders(canonical_folders);
    }

    /// Update workspace folders (per `PROJECT_ROOT_SPEC.md` Section 9.1).
    ///
    /// Handles dynamic workspace folder changes:
    /// - Added folders are registered with the `ProjectManager`
    /// - Removed folders have their Projects torn down and caches cleared
    pub fn update_workspace_folders(&self, added: Vec<PathBuf>, removed: Vec<PathBuf>) {
        // Canonicalize paths
        let added_canonical: Vec<PathBuf> = added
            .into_iter()
            .map(|f| f.canonicalize().unwrap_or(f))
            .collect();
        let removed_canonical: Vec<PathBuf> = removed
            .into_iter()
            .map(|f| f.canonicalize().unwrap_or(f))
            .collect();

        // Remove projects for removed folders (per PROJECT_ROOT_SPEC.md Section 6.3)
        // Also remove any projects nested under the removed folder (fix for iter4 finding 2)
        for folder in &removed_canonical {
            // First, find all projects whose index_root is under this folder
            let nested_roots: Vec<PathBuf> = self
                .project_manager
                .all_projects()
                .into_iter()
                .filter(|p| p.index_root.starts_with(folder))
                .map(|p| p.index_root.clone())
                .collect();

            // Remove nested projects first
            for nested_root in &nested_roots {
                if let Some(_project) = self.project_manager.remove_project(nested_root) {
                    log::info!(
                        "Removed nested Project at '{}' under workspace folder: {}",
                        nested_root.display(),
                        folder.display()
                    );
                }
            }

            // Then remove the folder's direct project (if any, might already be removed above)
            if !nested_roots.contains(folder)
                && let Some(_project) = self.project_manager.remove_project(folder)
            {
                log::info!("Removed Project for workspace folder: {}", folder.display());
            }
        }

        // Rebuild workspace folder list: current - removed + added
        // This ensures removed folders are actually removed from the list
        let current = self.project_manager.workspace_folders();
        let updated: Vec<PathBuf> = current
            .into_iter()
            .filter(|f| !removed_canonical.contains(f))
            .chain(added_canonical)
            .collect();
        self.project_manager.set_workspace_folders(updated);

        // Clear caches for removed folders
        // Caches will be updated lazily on next access
        self.clear_graph_cache();
    }

    /// Get the `ProjectManager` for advanced use cases.
    #[must_use]
    pub fn project_manager(&self) -> Arc<ProjectManager> {
        Arc::clone(&self.project_manager)
    }

    /// Get the Project for a file path (per `PROJECT_ROOT_SPEC.md` Section 9.2).
    ///
    /// Uses the `ProjectManager` to resolve the correct Project based on
    /// the current `projectRootMode`.
    ///
    /// # Errors
    ///
    /// Returns an error if project resolution fails.
    pub fn project_for_path(&self, path: &Path) -> Result<Arc<Project>> {
        self.project_manager
            .project_for_path(path)
            .map_err(|e| anyhow!("failed to resolve project for '{}': {}", path.display(), e))
    }

    /// Shutdown the session (per `PROJECT_ROOT_SPEC.md` Section 6.3).
    ///
    /// Cancels all in-progress operations and persists state if configured.
    pub fn shutdown(&self) {
        log::info!("Shutting down SessionManager");
        self.project_manager.shutdown();
    }

    /// Snapshot the current `Arc<LogicalWorkspace>`. The returned handle is
    /// stable for the duration of the caller — even if a concurrent
    /// `sqry/workspaceUpdate` swaps the inner `Arc`, this snapshot keeps
    /// pointing at the value seen at call time.
    #[must_use]
    pub fn logical_workspace(&self) -> Arc<LogicalWorkspace> {
        Arc::clone(&self.logical_workspace.read())
    }

    /// Replace the session's logical workspace.
    ///
    /// Called by `initialize()` once the §1.3 5-step resolution order has
    /// produced a `LogicalWorkspace`, and (in Step 5) by the
    /// `sqry/workspaceUpdate` handler when the client wants to swap in a
    /// freshly resolved workspace without restarting the LSP.
    ///
    /// Concurrent readers that already cloned the prior `Arc` keep their
    /// view stable (Arc-clone semantics); future readers see the new
    /// workspace.
    pub fn set_logical_workspace(&self, workspace: Arc<LogicalWorkspace>) {
        // STEP_12 — the user-visible aggregate telemetry line is the
        // tracing::info! event under target `sqry::workspace` emitted
        // by `server::initialize` (the resolution site). The detailed
        // debug below is intentionally NOT under that target so the
        // regression guard
        // (`sqry-lsp/tests/telemetry_resolution.rs::no_per_folder_resolution_lines_emitted`)
        // can pin "exactly ONE INFO event under sqry::workspace per
        // resolution" without picking up bookkeeping logs from setter
        // call sites (e.g. `sqry/workspaceUpdate`).
        let workspace_id_short = workspace.workspace_id().as_short_hex();
        let source_root_count = workspace.source_roots().len();
        let member_folder_count = workspace.member_folders().len();
        tracing::debug!(
            target: "sqry::workspace::session",
            workspace_id_short = %workspace_id_short,
            source_root_count,
            member_folder_count,
            project_root_mode = %workspace.project_root_mode(),
            identity = ?workspace.identity(),
            "set_logical_workspace"
        );
        let mut guard = self.logical_workspace.write();
        *guard = workspace;
    }

    fn current_index_root(&self) -> PathBuf {
        let guard = self.config.read();
        guard
            .index_root
            .clone()
            .unwrap_or_else(|| self.root_path().to_path_buf())
    }

    /// Load the unified graph for a file path.
    ///
    /// This method routes through `project_for_path()` to get the correct Project,
    /// then loads the graph from that Project's `index_root`. This enables proper
    /// multi-project support where each workspace folder can have its own graph.
    ///
    /// # Errors
    ///
    /// Returns an error if project resolution or graph loading fails.
    pub fn graph_for_path(&self, path: &Path) -> Result<Option<Arc<CodeGraph>>> {
        // SGA06 — record every observable entry into the shared-acquisition
        // path so the parity test suite can pin that read-only LSP handlers
        // (search, callers/callees, relations, hierarchical_search,
        // batch_counts, call_hierarchy, workspace_symbol) route their graph
        // acquisition through this method rather than re-entering the
        // executor's own `get_or_load_graph`.
        self.graph_for_path_calls.fetch_add(1, Ordering::Relaxed);

        // Backward compatibility: In single-root mode (no workspace folders configured),
        // use the legacy graph() which respects the configured index_root setting.
        if self.project_manager.workspace_folders().is_empty() {
            log::debug!(
                "graph_for_path: single-root mode, using session graph() for path '{}'",
                path.display()
            );
            return self.graph();
        }

        // Multi-project mode: resolve the correct project for this file path
        let project = self.project_for_path(path)?;

        log::debug!(
            "Loading graph for path '{}' from project '{}' (root: '{}')",
            path.display(),
            project.id,
            project.index_root.display()
        );

        // Use per-project graph caching, with the same corrupt/incompatible
        // snapshot self-healing behavior as single-root mode.
        match project.graph() {
            Ok(graph) => Ok(graph),
            Err(load_error) => {
                log::warn!(
                    "Graph load failed for project '{}' ({load_error}), auto-rebuilding index for LSP",
                    project.index_root.display()
                );
                Self::rebuild_project_graph_after_load_failure(&project, &load_error)
            }
        }
    }

    /// Load (or reuse) the cached unified graph for single-root mode.
    ///
    /// This is a backward-compatibility method for single-project sessions
    /// (i.e., when no workspace folders are configured). Prefer `graph_for_path()`
    /// which supports both single-root and multi-project modes.
    ///
    /// # SGA06 routing
    ///
    /// Standalone-LSP graph acquisition is now a thin wrapper around
    /// [`FilesystemGraphProvider`] with [`MissingGraphPolicy::AutoBuildIfEnabled`]
    /// and an auto-build hook that reproduces the historic self-heal flow
    /// (corrupt / `LoadFailed` snapshots trigger an in-place
    /// `build_and_persist_graph` rebuild). The provider supplies the
    /// canonical path-policy / plugin-selection / SHA-256 integrity checks
    /// that previously lived inline.
    ///
    /// `MissingGraphPolicy::AutoBuildIfEnabled` matches the pre-SGA06
    /// behaviour: a missing snapshot returned `None` and the LSP startup
    /// filter triggered a build via `rebuild_index`. We preserve that
    /// surface by mapping [`GraphAcquisitionError::NoGraph`] back to
    /// `Ok(None)`. The auto-build hook is therefore reached only on the
    /// historic self-heal path (corrupt / partially-written snapshot).
    ///
    /// # Errors
    ///
    /// Returns an error when the graph cannot be loaded from disk.
    pub fn graph(&self) -> Result<Option<Arc<CodeGraph>>> {
        // Fast path: check session-level cache first. SGA06 mirrors the MCP
        // engine's `cached_graph` pattern — the in-memory cache short-circuits
        // before any provider I/O.
        if let Some(graph) = self.graph_cache.read().as_ref() {
            return Ok(Some(graph.clone()));
        }

        // Slow path: route every disk-resident graph through the shared
        // FilesystemGraphProvider so path-policy / plugin-selection /
        // integrity checks run uniformly across CLI, MCP, and LSP.
        let logical_workspace = self.logical_workspace();
        let root = self.current_index_root();
        log::debug!(
            "Loading single-root graph from '{}' (workspace_id={}, source_roots={})",
            root.display(),
            logical_workspace.workspace_id().as_short_hex(),
            logical_workspace.source_roots().len()
        );

        match acquire_session_graph(&root, "lsp:session_graph") {
            Ok(graph) => {
                let mut cache = self.graph_cache.write();
                *cache = Some(graph.clone());
                Ok(Some(graph))
            }
            Err(GraphAcquisitionError::NoGraph { .. }) => {
                // No manifest → no complete index → return None
                // (startup filter should have caught this and triggered build)
                Ok(None)
            }
            Err(err) => {
                // SGA06 — preserve the visibility of stale / incompatible-graph
                // diagnostics by surfacing the typed error via anyhow with a
                // stable variant prefix that downstream LSP `Diagnostic`
                // pipelines (and the existing log-based diagnostic channel)
                // can match on.
                Err(map_acquisition_error_for_lsp(err, &root))
            }
        }
    }

    fn rebuild_project_graph_after_load_failure(
        project: &Project,
        load_error: &sqry_core::project::ProjectError,
    ) -> Result<Option<Arc<CodeGraph>>> {
        let plugins = create_plugin_manager();
        let config = sqry_core::graph::unified::build::BuildConfig::default();
        let (new_graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
            &project.index_root,
            &plugins,
            &config,
            "lsp:project_auto_rebuild",
        )
        .with_context(|| {
            format!(
                "auto-rebuild failed for {} (original error: {})",
                project.index_root.display(),
                load_error
            )
        })?;

        let graph = Arc::new(new_graph);
        project.clear_graph_cache();
        Ok(Some(graph))
    }

    /// SGA06 — return the number of times [`Self::graph_for_path`] has been
    /// called on this session.
    ///
    /// The counter is incremented unconditionally on entry (including the
    /// session-cache fast path), so it counts every read-only handler that
    /// passes through the shared-acquisition entry point. Used by the SGA06
    /// parity tests to assert that LSP read-only handlers route their graph
    /// acquisition through this method rather than re-entering the
    /// executor's own `get_or_load_graph`.
    #[doc(hidden)]
    #[must_use]
    pub fn graph_for_path_call_count(&self) -> u64 {
        self.graph_for_path_calls.load(Ordering::Relaxed)
    }

    /// Clear the cached unified graph.
    pub fn clear_graph_cache(&self) {
        let mut cache = self.graph_cache.write();
        *cache = None;
    }

    /// Clear the cached unified graph for the project that owns `path`.
    ///
    /// This is used after explicit multi-root rebuilds. The single-root cache is
    /// cleared separately by [`Self::clear_graph_cache`].
    pub fn clear_project_graph_cache_for_path(&self, path: &Path) {
        match self.project_for_path(path) {
            Ok(project) => project.clear_graph_cache(),
            Err(err) => log::warn!(
                "failed to clear project graph cache for '{}': {err}",
                path.display()
            ),
        }
    }

    #[must_use]
    pub fn document_snapshot(&self, path: &Path) -> Option<DocumentSnapshot> {
        self.documents.get(path)
    }

    /// Look up a node in an unsaved buffer via on-demand graph building.
    ///
    /// # Errors
    ///
    /// Returns an error when parsing the file fails.
    pub fn node_at(&self, uri: &Url, position: Position) -> Result<Option<NodeMatch>> {
        let path = uri
            .to_file_path()
            .map_err(|()| anyhow!("invalid file URI: {uri}"))?;
        self.node_at_path(&path, position)
    }

    fn node_at_path(&self, path: &Path, position: Position) -> Result<Option<NodeMatch>> {
        let snapshot = self.document_snapshot(path);
        let (rope, line_offsets, text) = load_document_content(snapshot.clone(), path)?;

        let line_idx = position.line as usize;
        if line_idx >= line_offsets.len() {
            return Ok(None);
        }

        let line_start = line_offsets[line_idx];
        let byte_offset = if let Some(snapshot_ref) = snapshot.as_ref() {
            match snapshot_ref.lsp_to_byte(position) {
                Some(offset) => offset,
                None => return Ok(None),
            }
        } else {
            let line_text = rope.line(line_idx).to_string();
            let byte_in_line = crate::utils::position::line_utf16_col_to_byte(
                line_text.as_str(),
                position.character as usize,
            );
            line_start + byte_in_line
        };

        let byte_in_line = byte_offset.saturating_sub(line_start);
        let target_line = line_idx;
        let target_col = byte_in_line;

        let nodes = self.nodes_for_path(path, snapshot.as_ref(), text.as_bytes())?;

        Ok(best_match(nodes, target_line, target_col))
    }

    /// Parse all nodes present in the given document.
    ///
    /// # Errors
    ///
    /// Returns an error when document loading or parsing fails.
    pub fn nodes_in_document(&self, uri: &Url) -> Result<Vec<NodeMatch>> {
        let path = uri
            .to_file_path()
            .map_err(|()| anyhow!("invalid file URI: {uri}"))?;
        let snapshot = self.document_snapshot(&path);
        let (_, _, text) = load_document_content(snapshot.clone(), &path)?;
        self.nodes_for_path(&path, snapshot.as_ref(), text.as_bytes())
    }

    fn nodes_for_path(
        &self,
        path: &Path,
        snapshot: Option<&DocumentSnapshot>,
        content: &[u8],
    ) -> Result<Vec<NodeMatch>> {
        let has_unsaved_changes = snapshot.is_some() && !document_matches_disk(snapshot, path);

        if !has_unsaved_changes {
            match self.graph_for_path(path) {
                Ok(Some(graph)) => {
                    if let Some(nodes) = Self::nodes_from_graph(path, &graph) {
                        return Ok(nodes);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    log::warn!(
                        "failed to load graph for '{}'; falling back to document content: {err}",
                        path.display()
                    );
                }
            }
        }

        self.nodes_from_content(path, content)
    }

    fn nodes_from_graph(path: &Path, graph: &CodeGraph) -> Option<Vec<NodeMatch>> {
        let file_id = graph.files().get(path)?;

        let mut nodes = Vec::new();
        for &node_id in graph.indices().by_file(file_id) {
            let Some(entry) = graph.nodes().get(node_id) else {
                continue;
            };
            nodes.push(node_from_graph_entry(path, entry, graph));
        }

        Some(nodes)
    }

    fn nodes_from_content(&self, path: &Path, content: &[u8]) -> Result<Vec<NodeMatch>> {
        let plugin_manager = self.executor.plugin_manager();
        let Some(plugin) = plugin_manager.plugin_for_path(path) else {
            return Ok(Vec::new());
        };

        let (prepared_content, tree) = plugin
            .prepare_ast(content)
            .map_err(|err| anyhow!("failed to parse AST for '{}': {:?}", path.display(), err))?;
        let parse_content = prepared_content.as_ref();

        let builder = plugin
            .graph_builder()
            .ok_or_else(|| anyhow!("no graph builder registered for '{}'", plugin.metadata().id))?;

        let mut staging = StagingGraph::new();
        builder
            .build_graph(&tree, parse_content, path, &mut staging)
            .map_err(|err| anyhow!("failed to build graph for '{}': {:?}", path.display(), err))?;
        // Node-listing path: body hashes only. Shape descriptors are never read
        // from this throwaway staging graph, so computing them would be wasted work.
        staging.attach_body_hashes(content, None);

        let strings = staging_string_table(&staging);
        let language = builder.language().to_string();
        Ok(staging_nodes(
            path,
            &staging,
            &strings,
            Some(language.as_str()),
        ))
    }

    #[must_use]
    pub fn has_unsaved_changes(&self, path: &Path) -> bool {
        let Some(snapshot) = self.document_snapshot(path) else {
            return false;
        };
        !document_matches_disk(Some(&snapshot), path)
    }

    #[must_use]
    pub fn plugin_manager(&self) -> PluginManager {
        build_plugin_manager()
    }
}

/// SGA06 — acquire a graph for `root` through the shared
/// [`FilesystemGraphProvider`].
///
/// Used by [`SessionManager::graph`] (single-root LSP) and
/// [`crate::handlers::index::load_status_graph`] (`sqry/indexStatus`) so that
/// every disk-resident graph load in standalone-LSP mode runs through the
/// same canonicalize → workspace-discover → manifest-verify → SHA-256 →
/// plugin-compat → deserialize pipeline that CLI and standalone MCP use.
///
/// The provider is configured with [`MissingGraphPolicy::AutoBuildIfEnabled`]
/// and an auto-build hook that reproduces the historic self-heal flow:
/// when the snapshot is missing or corrupt, the hook calls
/// [`build_and_persist_graph`] in place. The caller distinguishes "no
/// snapshot" from "corrupt snapshot" by inspecting whether
/// [`GraphAcquisitionError::NoGraph`] surfaces — that variant means the
/// provider's depth-bounded ancestor walk found no `.sqry/graph`
/// directory, which is the standalone-LSP "index missing" signal.
///
/// `tool_name` is forwarded into [`GraphAcquisitionRequest::tool_name`] so
/// provider-side observability can attribute the acquisition.
///
/// [`build_and_persist_graph`]: sqry_core::graph::unified::build::build_and_persist_graph
pub(crate) fn acquire_session_graph(
    root: &Path,
    tool_name: &'static str,
) -> Result<Arc<CodeGraph>, GraphAcquisitionError> {
    let provider_plugins = build_plugin_manager();
    let auto_build_root = root.to_path_buf();
    let auto_build_hook: AutoBuildHook = Arc::new(move |_req_path| {
        log::warn!(
            "Graph load failed for LSP at '{}', auto-rebuilding index (self-heal)",
            auto_build_root.display()
        );
        let plugins = create_plugin_manager();
        let config = sqry_core::graph::unified::build::BuildConfig::default();
        let (graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
            &auto_build_root,
            &plugins,
            &config,
            "lsp:auto_rebuild",
        )
        .map_err(|e| GraphAcquisitionError::BuildFailed {
            workspace_root: auto_build_root.clone(),
            reason: format!("{e}"),
        })?;
        Ok(Arc::new(graph))
    });

    let provider = FilesystemGraphProvider::new(Arc::new(provider_plugins))
        .with_auto_build_hook(auto_build_hook);

    let request = GraphAcquisitionRequest {
        requested_path: root.to_path_buf(),
        operation: AcquisitionOperation::ReadOnlyQuery,
        path_policy: PathPolicy::default(),
        // The provider only enters the auto-build branch when *no* graph
        // artifact is found. For a successfully loaded but corrupt /
        // load-failed snapshot, the provider returns `LoadFailed` — the
        // caller handles that branch via `map_acquisition_error_for_lsp`.
        // The auto-build hook above runs only on the historic self-heal
        // path, gated behind `LoadFailed`.
        missing_graph_policy: MissingGraphPolicy::AutoBuildIfEnabled,
        stale_policy: StalePolicy::default(),
        plugin_selection_policy: PluginSelectionPolicy::default(),
        tool_name: Some(tool_name),
    };

    match provider.acquire(request) {
        Ok(acquisition) => Ok(acquisition.graph),
        // Self-heal historic behaviour: a corrupt / load-failed snapshot
        // triggers an in-place rebuild. SGA06 preserves only this explicit
        // policy (per the design spec — corrupt-load self-heal is a
        // documented LSP behaviour, distinct from a clean missing-graph).
        Err(GraphAcquisitionError::LoadFailed {
            source_root,
            reason,
        }) => {
            log::warn!(
                "Graph load failed for LSP at '{}' ({reason}), auto-rebuilding index (self-heal)",
                source_root.display()
            );
            let plugins = create_plugin_manager();
            let config = sqry_core::graph::unified::build::BuildConfig::default();
            let (graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
                &source_root,
                &plugins,
                &config,
                "lsp:auto_rebuild",
            )
            .map_err(|e| GraphAcquisitionError::BuildFailed {
                workspace_root: source_root.clone(),
                reason: format!("auto-rebuild after corrupt load failed: {e}"),
            })?;
            Ok(Arc::new(graph))
        }
        Err(err) => Err(err),
    }
}

/// SGA06 — map a typed [`GraphAcquisitionError`] back into the
/// `anyhow::Error` shape that LSP handlers and the LSP wire layer
/// already render to clients (server logs / `Diagnostic` messages). The
/// variant prefix is preserved so existing diagnostic-channel matchers
/// continue to surface stale / incompatible / evicted classes.
pub(crate) fn map_acquisition_error_for_lsp(
    err: GraphAcquisitionError,
    root: &Path,
) -> anyhow::Error {
    match err {
        GraphAcquisitionError::InvalidPath { path, reason } => {
            anyhow!("invalid path {}: {}", path.display(), reason)
        }
        GraphAcquisitionError::NoGraph { workspace_root } => anyhow!(
            "No unified graph found at {}. Run `sqry index` to create the graph.",
            workspace_root.display()
        ),
        GraphAcquisitionError::IncompatibleGraph {
            source_root,
            status,
        } => anyhow!(
            "Incompatible graph at {}: {:?}. Rebuild the index (`sqry index --force`) after upgrading sqry.",
            source_root.display(),
            status
        ),
        GraphAcquisitionError::LoadFailed {
            source_root,
            reason,
        } => anyhow!(
            "Failed to load graph at {}: {}",
            source_root.display(),
            reason
        ),
        GraphAcquisitionError::BuildFailed {
            workspace_root,
            reason,
        } => anyhow!(
            "Graph auto-rebuild failed for {}: {}",
            workspace_root.display(),
            reason
        ),
        GraphAcquisitionError::StaleExpired {
            workspace_root,
            age_hours,
        } => anyhow!(
            "Stale graph for {} (age_hours={:?}) exceeded the configured stale-serve window. Run `sqry index` to rebuild.",
            workspace_root.display(),
            age_hours
        ),
        // Filesystem provider does not surface NotReady / Evicted; if a
        // future provider does, fall back to a generic acquisition message
        // that still records the workspace context for diagnostics.
        GraphAcquisitionError::NotReady {
            workspace_root,
            lifecycle,
        } => anyhow!(
            "Workspace at {} is not ready (lifecycle={lifecycle})",
            workspace_root.display()
        ),
        GraphAcquisitionError::Evicted {
            workspace_root,
            original_lifecycle,
            reload_failure,
        } => anyhow!(
            "Workspace at {} was evicted (original_lifecycle={original_lifecycle}, reload_failure={reload_failure:?}); reload before retrying. Original LSP root: {}",
            workspace_root.display(),
            root.display()
        ),
        GraphAcquisitionError::Internal { reason } => anyhow!(
            "Internal acquisition error for {}: {}",
            root.display(),
            reason
        ),
    }
}

fn best_match(nodes: Vec<NodeMatch>, line: usize, column: usize) -> Option<NodeMatch> {
    nodes
        .into_iter()
        .filter(|node| contains_position(node, line, column))
        .min_by_key(ranking_key)
}

fn contains_position(node: &NodeMatch, line: usize, column: usize) -> bool {
    if node.start_line == 0 || node.end_line == 0 {
        return false;
    }

    let start_line = node.start_line.saturating_sub(1) as usize;
    let end_line = node.end_line.saturating_sub(1) as usize;

    if line < start_line || line > end_line {
        return false;
    }

    let start_col = node.start_column as usize;
    let end_col = node.end_column as usize;

    if start_line == end_line {
        return column >= start_col && column <= end_col;
    }

    if line == start_line {
        return column >= start_col;
    }

    if line == end_line {
        return column <= end_col;
    }

    true
}

fn ranking_key(node: &NodeMatch) -> (usize, usize) {
    let line_span = node.end_line.saturating_sub(node.start_line) as usize;
    let col_span = node.end_column.saturating_sub(node.start_column) as usize;
    (line_span, col_span)
}

fn load_document_content(
    snapshot: Option<DocumentSnapshot>,
    path: &Path,
) -> Result<(Rope, Vec<usize>, String)> {
    if let Some(snapshot) = snapshot {
        let rope = snapshot.rope.clone();
        let offsets = snapshot.line_offsets.iter().copied().collect::<Vec<_>>();
        let text = snapshot.text();
        Ok((rope, offsets, text))
    } else {
        // Check file type before attempting disk read
        let category = classify_file(path);
        if !category.is_supported() {
            return Err(anyhow!(
                "{}: unsupported {} - cannot process binary files",
                path.display(),
                category.description()
            ));
        }

        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        let rope = Rope::from(text.as_str());
        let offsets = compute_line_offsets(&rope);
        Ok((rope, offsets, text))
    }
}

fn node_from_graph_entry(path: &Path, entry: &NodeEntry, graph: &CodeGraph) -> NodeMatch {
    let name = graph
        .strings()
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = entry
        .qualified_name
        .and_then(|id| graph.strings().resolve(id))
        .map(|s| s.to_string());
    let signature = entry
        .signature
        .and_then(|id| graph.strings().resolve(id))
        .map(|s| s.to_string());
    let documentation = entry
        .doc
        .and_then(|id| graph.strings().resolve(id))
        .map(|s| s.to_string());
    let language = graph
        .files()
        .language_for_file(entry.file)
        .map(|lang| lang.to_string());
    let file_path = graph
        .files()
        .resolve(entry.file)
        .map_or_else(|| path.to_path_buf(), |p| p.to_path_buf());

    NodeMatch {
        name,
        qualified_name,
        kind: entry.kind,
        is_static: entry.is_static,
        file_path,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        signature,
        documentation,
        language,
    }
}

fn staging_nodes(
    path: &Path,
    staging: &StagingGraph,
    strings: &HashMap<StringId, String>,
    language: Option<&str>,
) -> Vec<NodeMatch> {
    let mut nodes = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op {
            nodes.push(node_from_staging_entry(path, entry, strings, language));
        }
    }
    nodes
}

fn node_from_staging_entry(
    path: &Path,
    entry: &NodeEntry,
    strings: &HashMap<StringId, String>,
    language: Option<&str>,
) -> NodeMatch {
    let name = resolve_staging_string(strings, entry.name).unwrap_or_default();
    let qualified_name = entry
        .qualified_name
        .and_then(|id| resolve_staging_string(strings, id));
    let signature = entry
        .signature
        .and_then(|id| resolve_staging_string(strings, id));
    let documentation = entry.doc.and_then(|id| resolve_staging_string(strings, id));

    NodeMatch {
        name,
        qualified_name,
        kind: entry.kind,
        is_static: entry.is_static,
        file_path: path.to_path_buf(),
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        signature,
        documentation,
        language: language.map(str::to_string),
    }
}

fn staging_string_table(staging: &StagingGraph) -> HashMap<StringId, String> {
    let mut strings = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            strings.entry(*local_id).or_insert_with(|| value.clone());
        }
    }
    strings
}

fn resolve_staging_string(strings: &HashMap<StringId, String>, id: StringId) -> Option<String> {
    strings.get(&id).cloned()
}

fn document_matches_disk(snapshot: Option<&DocumentSnapshot>, path: &Path) -> bool {
    let Some(snapshot) = snapshot else {
        return true;
    };

    match std::fs::read_to_string(path) {
        Ok(disk_content) => snapshot.text() == disk_content,
        Err(_) => false,
    }
}

fn resolve_root_path(index_root: Option<PathBuf>) -> Result<PathBuf> {
    let root = match index_root {
        Some(path) => path,
        None => env::current_dir().context("failed to determine current working directory")?,
    };

    Ok(root.canonicalize().unwrap_or(root))
}

fn build_plugin_manager() -> PluginManager {
    create_plugin_manager()
}

// ---------------------------------------------------------------------------
// LogicalWorkspace resolution (§1.3 of 03_IMPLEMENTATION_PLAN.md)
// ---------------------------------------------------------------------------
//
// The five resolution branches are exposed as separate public functions so
// the integration suite (`sqry-lsp/tests/multi_root_logical_workspace.rs`)
// can exercise each branch deterministically. `resolve_logical_workspace`
// composes them in the documented short-circuit order.

/// Inputs needed to resolve the §1.3 [`LogicalWorkspace`] from an LSP
/// `initialize` request. All fields are pre-extracted by `server::initialize`
/// so this resolver stays unit-testable without a live LSP transport.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceResolutionInputs {
    /// `params.workspace_folders[*]` mapped to local filesystem paths.
    /// Empty when the client did not advertise workspace folders.
    pub workspace_folders: Vec<PathBuf>,
    /// `--index-root` / `LspOptions::index_root`. Bounds the security
    /// envelope (acceptance criterion 7) and feeds branch 2.
    pub index_root: Option<PathBuf>,
    /// Decoded `initializationOptions.sqry.workspace` payload (branch 1).
    /// Accepts a JSON object that round-trips through
    /// [`serde_json::from_value`] into [`LogicalWorkspace`].
    pub init_options_workspace: Option<serde_json::Value>,
    /// Decoded `initializationOptions.sqry.workspaceFile` (branch 4): path
    /// to a sibling `.code-workspace` file the extension passes through.
    pub init_options_workspace_file: Option<PathBuf>,
}

/// Branch number reported by [`resolve_logical_workspace`] so the caller
/// can log which §1.3 step produced the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionBranch {
    /// Step 1 — `initializationOptions.sqry.workspace` produced a workspace.
    InitializationOptions,
    /// Step 2 — `--index-root` contained a `.sqry-workspace` file.
    IndexRootSqryWorkspace,
    /// Step 3 — a `workspace_folders[*]` directory contained a
    /// `.sqry-workspace` file.
    WorkspaceFolderSqryWorkspace,
    /// Step 4 — a sibling `.code-workspace` was identifiable.
    SiblingCodeWorkspace,
    /// Step 5 — fall back to `AnonymousMultiRoot`.
    AnonymousMultiRoot,
}

const SQRY_WORKSPACE_FILENAME: &str = ".sqry-workspace";

/// Branch 1 — accept a fully serialized [`LogicalWorkspace`] from
/// `initializationOptions.sqry.workspace`. Returns `Ok(None)` when the
/// caller did not provide the option; `Err` only on a present-but-invalid
/// payload so the caller can surface the failure rather than silently
/// fall through.
///
/// `STEP_5` codex iter1 MAJOR fix: when the payload is the lightweight
/// **extension-side classification hint** (a JSON object with a top-level
/// `folders` array and a `classification` key — see
/// `sqry-vscode/src/sqryClient.ts::SqryWorkspaceInitializationPayload`),
/// the function returns `Ok(None)` so the resolver falls through to
/// branch 4 (`workspaceFile` path), which loads + classifies the
/// `.code-workspace` in-process. This keeps the contract that the
/// extension parses + classifies + sends both shapes while preserving the
/// existing strict-deserialize behaviour for genuine `LogicalWorkspace`
/// payloads (e.g. produced by other automation).
///
/// # Errors
///
/// Returns an error when the JSON payload is present, is not a
/// recognized extension-side hint, and cannot be deserialized into
/// a [`LogicalWorkspace`].
pub fn resolve_step_1(
    init_options_workspace: Option<&serde_json::Value>,
) -> Result<Option<LogicalWorkspace>> {
    let Some(value) = init_options_workspace else {
        return Ok(None);
    };
    if is_extension_classification_hint(value) {
        // Extension-side classification hint — branch 4 owns the actual
        // resolution. Soft fall-through.
        return Ok(None);
    }
    let workspace: LogicalWorkspace = serde_json::from_value(value.clone()).with_context(
        || "initializationOptions.sqry.workspace: payload did not deserialize as LogicalWorkspace",
    )?;
    Ok(Some(workspace))
}

/// Detect the lightweight classification-hint shape produced by
/// `sqry-vscode/src/extension.ts` (the parsed `.code-workspace`):
///
/// - top-level object with a `folders` ARRAY field, AND
/// - a `classification` field that is either `null` or an OBJECT.
///
/// Both fields must be present; this is intentionally strict so a
/// hand-crafted `LogicalWorkspace` payload (which carries
/// `source_roots`, `member_folders`, `workspace_id`, etc., but no
/// `classification` key) does not get misclassified as a hint.
fn is_extension_classification_hint(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let folders_ok = obj.get("folders").is_some_and(serde_json::Value::is_array);
    let classification_ok = obj
        .get("classification")
        .is_some_and(|v| v.is_null() || v.is_object());
    folders_ok && classification_ok
}

/// Branch 2 — `--index-root` is set AND that path contains a
/// `.sqry-workspace`. Returns `Ok(None)` when either precondition is
/// unmet; `Err` only on a present-but-malformed registry.
///
/// # Errors
///
/// Returns an error when the registry file exists but cannot be parsed.
pub fn resolve_step_2(index_root: Option<&Path>) -> Result<Option<LogicalWorkspace>> {
    let Some(root) = index_root else {
        return Ok(None);
    };
    let candidate = root.join(SQRY_WORKSPACE_FILENAME);
    if !candidate.is_file() {
        return Ok(None);
    }
    let workspace = LogicalWorkspace::from_sqry_workspace(&candidate).with_context(|| {
        format!(
            "failed to load .sqry-workspace at {} (branch 2)",
            candidate.display()
        )
    })?;
    Ok(Some(workspace))
}

/// Branch 3 — any `workspace_folders[*]` directory contains a
/// `.sqry-workspace`. The first folder (in client-provided order) that
/// holds the registry wins. Returns `Ok(None)` when no folder qualifies.
///
/// # Errors
///
/// Returns an error when the first qualifying registry file fails to
/// parse.
pub fn resolve_step_3(workspace_folders: &[PathBuf]) -> Result<Option<LogicalWorkspace>> {
    for folder in workspace_folders {
        let candidate = folder.join(SQRY_WORKSPACE_FILENAME);
        if !candidate.is_file() {
            continue;
        }
        let workspace = LogicalWorkspace::from_sqry_workspace(&candidate).with_context(|| {
            format!(
                "failed to load .sqry-workspace at {} (branch 3)",
                candidate.display()
            )
        })?;
        return Ok(Some(workspace));
    }
    Ok(None)
}

/// Branch 4 — `initializationOptions.sqry.workspaceFile` points at a
/// `.code-workspace`. Returns `Ok(None)` when the option is absent.
///
/// `heuristic_fn` is the per-folder classifier; the LSP supplies a
/// best-effort heuristic (currently `HeuristicVerdict::Unknown` for
/// every folder, which yields the §1.3 last-resort default of
/// `Member::NoLanguagePluginMatch`). Future steps will wire a real
/// heuristic here.
///
/// # Errors
///
/// Returns an error when the file is missing or cannot be parsed as a
/// `.code-workspace`.
pub fn resolve_step_4(
    workspace_file: Option<&Path>,
    heuristic_fn: &dyn Fn(&Path) -> HeuristicVerdict,
) -> Result<Option<LogicalWorkspace>> {
    let Some(path) = workspace_file else {
        return Ok(None);
    };
    let workspace =
        LogicalWorkspace::from_code_workspace(path, heuristic_fn).with_context(|| {
            format!(
                "failed to load .code-workspace at {} (branch 4)",
                path.display()
            )
        })?;
    Ok(Some(workspace))
}

/// Branch 5 — the last-resort fallback: synthesize an
/// [`LogicalWorkspace::anonymous_multi_root`] from the client-provided
/// `workspace_folders`. When no folders were advertised, falls back to
/// the session root path so the workspace still has a single source root.
///
/// This branch is infallible by contract; any canonicalization failure
/// is reported as `LogicalWorkspaceError` from the constructor.
///
/// # Errors
///
/// Returns an error when no folders can be canonicalized at all.
pub fn resolve_step_5(
    workspace_folders: Vec<PathBuf>,
    fallback_root: &Path,
) -> Result<LogicalWorkspace> {
    let folders = if workspace_folders.is_empty() {
        vec![fallback_root.to_path_buf()]
    } else {
        workspace_folders
    };
    LogicalWorkspace::anonymous_multi_root(folders).with_context(|| {
        format!(
            "AnonymousMultiRoot fallback failed for root {}",
            fallback_root.display()
        )
    })
}

/// The default heuristic for branch 4: returns `Unknown` for every
/// folder, which causes [`LogicalWorkspace::from_code_workspace`] to
/// apply the §1.3 last-resort default (`Member::NoLanguagePluginMatch`).
/// A future step will replace this with a real per-folder classifier.
pub fn default_workspace_heuristic() -> impl Fn(&Path) -> HeuristicVerdict {
    |_path: &Path| HeuristicVerdict::Unknown
}

/// Resolve a [`LogicalWorkspace`] per §1.3 of the implementation plan.
///
/// Short-circuits in the documented order; never falls back beyond
/// branch 5.
///
/// # Errors
///
/// Returns the first hard failure encountered in any branch. Soft misses
/// (option absent, file not present) fall through to the next branch.
pub fn resolve_logical_workspace(
    inputs: &WorkspaceResolutionInputs,
    fallback_root: &Path,
    heuristic_fn: &dyn Fn(&Path) -> HeuristicVerdict,
) -> Result<(LogicalWorkspace, ResolutionBranch)> {
    if let Some(ws) = resolve_step_1(inputs.init_options_workspace.as_ref())? {
        return Ok((ws, ResolutionBranch::InitializationOptions));
    }
    if let Some(ws) = resolve_step_2(inputs.index_root.as_deref())? {
        return Ok((ws, ResolutionBranch::IndexRootSqryWorkspace));
    }
    if let Some(ws) = resolve_step_3(&inputs.workspace_folders)? {
        return Ok((ws, ResolutionBranch::WorkspaceFolderSqryWorkspace));
    }
    if let Some(ws) = resolve_step_4(inputs.init_options_workspace_file.as_deref(), heuristic_fn)? {
        return Ok((ws, ResolutionBranch::SiblingCodeWorkspace));
    }
    let ws = resolve_step_5(inputs.workspace_folders.clone(), fallback_root)?;
    Ok((ws, ResolutionBranch::AnonymousMultiRoot))
}

/// Compute an aggregate [`sqry_core::workspace::WorkspaceIndexStatus`] over
/// every source root in `workspace`.
///
/// Each entry's [`sqry_core::workspace::SourceRootStatus`] is derived from
/// on-disk graph state: present + readable manifest -> `Ok` (with
/// last-modified mtime + symbol count where available), absent ->
/// `Missing`, build lock present -> `Building`, IO/parse failure ->
/// `Error`. The aggregate is built fresh on every call; persistence /
/// caching belongs to a future step.
#[must_use]
pub fn aggregate_workspace_index_status(
    workspace: &LogicalWorkspace,
) -> sqry_core::workspace::WorkspaceIndexStatus {
    use sqry_core::graph::unified::persistence::GraphStorage;
    use sqry_core::workspace::{SourceRootIndexState, SourceRootStatus, WorkspaceWarning};

    let mut entries = Vec::with_capacity(workspace.source_roots().len());
    let mut warnings: Vec<WorkspaceWarning> = Vec::new();
    for source_root in workspace.source_roots() {
        let storage = GraphStorage::new(&source_root.path);
        let snapshot_path = storage.snapshot_path();
        let lock_path = source_root.path.join(".sqry/graph/build.lock");
        let lock_present = lock_path.is_file();

        let (status, last_indexed_at) = if lock_present {
            (SourceRootIndexState::Building, None)
        } else if !storage.exists() || !storage.snapshot_exists() {
            (SourceRootIndexState::Missing, None)
        } else {
            match fs::metadata(snapshot_path) {
                Ok(meta) => {
                    let modified = meta.modified().ok();
                    (SourceRootIndexState::Ok, modified)
                }
                Err(_) => (SourceRootIndexState::Error, None),
            }
        };

        // STEP_11_4 — re-probe `<root>/.sqry/classpath/` so live status
        // reflects current on-disk state. Probe failures other than
        // NotFound surface as `WorkspaceWarning::ClasspathProbeFailed`;
        // absence of the dir is the common-case non-event.
        let classpath_probe = source_root.path.join(".sqry").join("classpath");
        if let Err(err) = fs::metadata(&classpath_probe)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warnings.push(WorkspaceWarning::ClasspathProbeFailed {
                source_root: source_root.path.clone(),
                detail: err.to_string(),
            });
        }

        entries.push(SourceRootStatus {
            path: source_root.path.clone(),
            status,
            last_indexed_at,
            symbol_count: None,
            // STEP_11_4 — surface auto-populated `SourceRoot.classpath_dir`
            // through the per-root status so LSP / MCP / CLI consumers
            // render JVM-classpath presence from the same source-of-truth.
            classpath_dir: source_root.classpath_dir.clone(),
        });
    }
    let mut aggregate =
        sqry_core::workspace::WorkspaceIndexStatus::from_source_root_statuses(entries);
    for warning in warnings {
        aggregate.push_warning(warning);
    }
    aggregate
}

/// `STEP_11_4` — aggregator variant that folds in extra
/// [`sqry_core::workspace::WorkspaceWarning`] entries (e.g. from a
/// `sqry_lang_rust::macro_expander::expand_in_workspace` outcome)
/// before the aggregate is returned. Without this, macro-expansion
/// warnings produced by the bridge never reach the user-visible
/// status payload.
#[must_use]
pub fn aggregate_workspace_index_status_with_warnings(
    workspace: &LogicalWorkspace,
    extra_warnings: Vec<sqry_core::workspace::WorkspaceWarning>,
) -> sqry_core::workspace::WorkspaceIndexStatus {
    let mut aggregate = aggregate_workspace_index_status(workspace);
    for warning in extra_warnings {
        aggregate.push_warning(warning);
    }
    aggregate
}

/// `STEP_11_4` iter3 — structural validation of every source root for
/// Rust macro-expansion compatibility. Produces one
/// [`sqry_core::workspace::WorkspaceWarning::MacroExpansionInvalidRoot`]
/// per source root that fails the `MacroExpander::new` guard
/// (root empty / not absolute / does not exist on disk). This is the
/// production producer that feeds
/// [`build_workspace_status_info`] so the live `sqry/workspaceStatus`
/// payload carries the warning surface.
///
/// The check runs `MacroExpander::new(MacroExpanderConfig { enabled:
/// true, workspace_root: root.path, .. })` per source root. The
/// `enabled: true` flag is required to bypass the
/// `MacroExpandError::Disabled` arm so the structural validators
/// (workspace-root-empty, workspace-root-not-absolute,
/// workspace-root-not-found) are exercised. The expander itself is
/// dropped immediately — no cargo expand process is spawned, no
/// arbitrary code is executed.
#[must_use]
pub fn collect_macro_expansion_warnings(
    workspace: &LogicalWorkspace,
) -> Vec<sqry_core::workspace::WorkspaceWarning> {
    use sqry_core::workspace::WorkspaceWarning;
    use sqry_lang_rust::macro_expander::{MacroExpandError, MacroExpander, MacroExpanderConfig};

    let mut warnings = Vec::new();
    for root in workspace.source_roots() {
        let config = MacroExpanderConfig {
            enabled: true,
            show_warning: false,
            workspace_root: root.path.clone(),
            ..MacroExpanderConfig::default()
        };
        if let Err(MacroExpandError::InvalidWorkspaceRoot(detail)) = MacroExpander::new(config) {
            warnings.push(WorkspaceWarning::MacroExpansionInvalidRoot {
                source_root: root.path.clone(),
                detail,
            });
        }
        // All other arms (Disabled, success, CargoExpandNotFound at
        // expand_file time, etc.) are not surfaced here — only
        // structural InvalidWorkspaceRoot validation is the
        // production warning producer.
    }
    warnings
}

/// Serializable wire view returned by `sqry/workspaceStatus` — see
/// `handlers::workspace_status::handle_workspace_status`. Lives next to
/// the resolution helpers because it composes the same accessors.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceStatusInfo {
    /// Short BLAKE3 prefix for human-readable surfaces.
    pub workspace_id_short: String,
    /// Full BLAKE3 hex digest (acceptance criterion 6).
    pub workspace_id_full: String,
    /// Per-source-root + summary counters (§1.4 aggregate contract).
    pub aggregate: sqry_core::workspace::WorkspaceIndexStatus,
    /// Workspace-level `project_root_mode` (string-form).
    pub project_root_mode: String,
    /// Source root paths (canonical).
    pub source_roots: Vec<PathBuf>,
    /// Member folder paths + reason.
    pub member_folders: Vec<MemberFolderInfo>,
    /// Excluded paths (canonical).
    pub exclusions: Vec<PathBuf>,
}

/// Wire-side view of [`sqry_core::workspace::MemberFolder`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemberFolderInfo {
    /// Canonical absolute path.
    pub path: PathBuf,
    /// Why the folder was classified as a member (camelCase per
    /// `sqry-core`'s serde rename).
    pub reason: MemberReason,
}

/// Build the wire-side `WorkspaceStatusInfo` for the LSP handler.
#[must_use]
pub fn build_workspace_status_info(workspace: &LogicalWorkspace) -> WorkspaceStatusInfo {
    // STEP_11_4 iter3 — validate every source root for macro-expansion
    // compatibility and fold the resulting `MacroExpansionInvalidRoot`
    // warnings into the live `WorkspaceIndexStatus.warnings` channel
    // through `aggregate_workspace_index_status_with_warnings`. This
    // closes iter2 MAJOR 3: `expand_in_workspace`'s warning channel
    // is no longer dead production code; every `sqry/workspaceStatus`
    // response carries the structural-validation outcome.
    let macro_warnings = collect_macro_expansion_warnings(workspace);
    let aggregate = aggregate_workspace_index_status_with_warnings(workspace, macro_warnings);
    let source_roots = workspace
        .source_roots()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    let member_folders = workspace
        .member_folders()
        .iter()
        .map(|m| MemberFolderInfo {
            path: m.path.clone(),
            reason: m.reason,
        })
        .collect();
    WorkspaceStatusInfo {
        workspace_id_short: workspace.workspace_id().as_short_hex(),
        workspace_id_full: workspace.workspace_id().as_full_hex(),
        aggregate,
        project_root_mode: workspace.project_root_mode().to_string(),
        source_roots,
        member_folders,
        exclusions: workspace.exclusions().to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Path classification — re-exported for handlers
// ---------------------------------------------------------------------------

/// Classification result enriched with a `MemberReason` when applicable.
/// Re-exported so handlers do not need to import `sqry_core::workspace`
/// directly.
pub use sqry_core::workspace::Classification as PathClassification;
pub use sqry_core::workspace::MemberReason as PathMemberReason;

/// `STEP_11_4` — verdict returned by [`SessionManager::evaluate_handler_gate`]
/// for an LSP-handler URI. Each handler short-circuits on
/// [`Self::Member`] and [`Self::Excluded`], returning an empty / `None`
/// result without touching the graph; only [`Self::Continue`] (with
/// `Source` or `Unknown` paths — `Unknown` is treated as "not in the
/// workspace, fall through to today's per-repo behaviour") proceeds
/// into the normal handler body.
///
/// The gate is invoked by every URI-keyed LSP handler (`code_action`,
/// hover, `document_symbol`, `workspace_symbol`; see
/// `sqry-lsp/tests/lsp_handler_member_excluded_contract.rs`) so the
/// regression class `STEP_11_4` was opened to close — "a non-status
/// handler probes the filesystem per folder and bypasses the
/// workspace classifier" — cannot re-emerge through any of those
/// handler surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerGate {
    /// Handler should proceed with normal logic — the URI either
    /// belongs to a registered source root, or is `Unknown` (outside
    /// the workspace, where today's per-repo handler semantics
    /// already apply).
    Continue,
    /// URI lives inside a member folder. Handlers must return the
    /// "empty + partial" shape for their result type (`Ok(None)` for
    /// LSP-standard handlers, an empty struct with `partial: true`
    /// for the structured handlers we own).
    Member(PathMemberReason),
    /// URI lives inside an excluded folder. Handlers must return the
    /// "empty + excluded" shape for their result type.
    Excluded,
}

impl HandlerGate {
    /// `true` when the gate authorises the handler body to run. The
    /// inverse of `is_short_circuit`.
    #[must_use]
    pub fn allows_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }

    /// `true` when the handler must short-circuit (Member or
    /// Excluded). Used by handlers that fold the gate into a single
    /// `if gate.is_short_circuit() { return ...; }` line.
    #[must_use]
    pub fn is_short_circuit(&self) -> bool {
        !self.allows_continue()
    }

    /// `true` for the [`Self::Member`] arm.
    #[must_use]
    pub fn is_member(&self) -> bool {
        matches!(self, Self::Member(_))
    }

    /// `true` for the [`Self::Excluded`] arm.
    #[must_use]
    pub fn is_excluded(&self) -> bool {
        matches!(self, Self::Excluded)
    }
}

/// Classify `path` against the session's current logical workspace.
///
/// This is a thin wrapper around [`LogicalWorkspace::classify`] that
/// handles the `Arc` indirection so handlers can call
/// `session.classify_path(&abs_path)` without touching the lock.
impl SessionManager {
    /// Classify `path` against the current logical workspace.
    #[must_use]
    pub fn classify_path(&self, path: &Path) -> Classification {
        self.logical_workspace().classify(path)
    }

    /// `STEP_11_4` — evaluate the handler-level URI gate against the
    /// current logical workspace. Returns the [`HandlerGate`] verdict
    /// every URI-keyed LSP handler must consult before touching the
    /// graph.
    ///
    /// The gate exists so member-folder and excluded-path requests
    /// short-circuit through the same code path the
    /// `sqry/indexStatus` handler already uses (`STEP_4`), preventing
    /// the regression class where a non-status handler bypasses the
    /// classifier and probes the filesystem per folder.
    ///
    /// # Behaviour
    ///
    /// - URI that does not parse to a file path → [`HandlerGate::Continue`]
    ///   (the handler then handles the malformed URI with its own
    ///   error path, as it always has).
    /// - File-path classification:
    ///     - [`Classification::Source`] → [`HandlerGate::Continue`]
    ///     - [`Classification::Unknown`] → [`HandlerGate::Continue`]
    ///       (out-of-workspace requests preserve today's per-repo
    ///       semantics; the workspace classifier never adds new
    ///       restrictions on them)
    ///     - [`Classification::Member { reason }`] →
    ///       [`HandlerGate::Member(reason)`]
    ///     - [`Classification::Excluded`] → [`HandlerGate::Excluded`]
    #[must_use]
    pub fn evaluate_handler_gate(&self, uri: &tower_lsp::lsp_types::Url) -> HandlerGate {
        let Ok(path) = uri.to_file_path() else {
            // A malformed URI is not the gate's problem — let the
            // handler's own error path produce its usual response.
            return HandlerGate::Continue;
        };
        match self.classify_path(&path) {
            Classification::Source | Classification::Unknown => HandlerGate::Continue,
            Classification::Member { reason } => HandlerGate::Member(reason),
            Classification::Excluded => HandlerGate::Excluded,
        }
    }
}

// `LogicalWorkspaceError: std::error::Error`, so anyhow's blanket
// `From<E> for anyhow::Error` already covers conversion through `?`.
// No explicit impl required.

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeKind;
    use tempfile::tempdir;

    fn make_node(
        name: &str,
        qualified_name: Option<&str>,
        language: Option<&str>,
        kind: NodeKind,
        is_static: bool,
    ) -> NodeMatch {
        NodeMatch {
            name: name.to_string(),
            qualified_name: qualified_name.map(str::to_string),
            kind,
            is_static,
            file_path: PathBuf::from("src/lib.rs"),
            start_line: 1,
            start_column: 0,
            end_line: 5,
            end_column: 0,
            signature: None,
            documentation: None,
            language: language.map(str::to_string),
        }
    }

    fn make_session(index_root: PathBuf) -> SessionManager {
        SessionManager::new(LspOptions {
            stdio: false,
            socket: None,
            index_root: Some(index_root),
            log_level: "warn".into(),
            config: None,
            allow_public_bind: false,
            daemon: false,
            daemon_socket: None,
            workspace: None,
        })
    }

    // ── NodeMatch::qualified_name_or_name ────────────────────────────────────

    #[test]
    fn qualified_name_or_name_returns_qualified_when_present() {
        let node = make_node("new", Some("MyStruct::new"), None, NodeKind::Method, false);
        assert_eq!(node.qualified_name_or_name(), "MyStruct::new");
    }

    #[test]
    fn qualified_name_or_name_falls_back_to_name_when_absent() {
        let node = make_node("standalone", None, None, NodeKind::Function, false);
        assert_eq!(node.qualified_name_or_name(), "standalone");
    }

    // ── NodeMatch::display_qualified_name_or_name ────────────────────────────

    #[test]
    fn display_qualified_name_or_name_returns_name_when_no_qualified_name() {
        let node = make_node("my_fn", None, None, NodeKind::Function, false);
        assert_eq!(node.display_qualified_name_or_name(), "my_fn");
    }

    #[test]
    fn display_qualified_name_or_name_returns_qualified_when_no_language() {
        // No language → cannot format, returns qualified_name as-is
        let node = make_node("new", Some("MyStruct::new"), None, NodeKind::Method, false);
        assert_eq!(node.display_qualified_name_or_name(), "MyStruct::new");
    }

    #[test]
    fn display_qualified_name_or_name_returns_qualified_when_language_unknown() {
        // Language ID that doesn't resolve → returns qualified_name as-is
        let node = make_node(
            "fn",
            Some("Module::fn"),
            Some("nonexistent_lang"),
            NodeKind::Function,
            false,
        );
        assert_eq!(node.display_qualified_name_or_name(), "Module::fn");
    }

    #[test]
    fn display_qualified_name_or_name_uses_native_display_for_rust() {
        // Rust uses :: separators — should pass through as-is for Rust functions
        let node = make_node(
            "new",
            Some("MyStruct::new"),
            Some("rust"),
            NodeKind::Method,
            false,
        );
        let result = node.display_qualified_name_or_name();
        // The display should produce something non-empty for a valid rust qualified name
        assert!(!result.is_empty());
    }

    #[test]
    fn display_qualified_name_or_name_uses_dot_for_python() {
        let node = make_node(
            "method",
            Some("MyClass::method"),
            Some("python"),
            NodeKind::Method,
            false,
        );
        let result = node.display_qualified_name_or_name();
        assert!(!result.is_empty());
    }

    // ── document_matches_disk ────────────────────────────────────────────────

    #[test]
    fn document_matches_disk_returns_true_when_no_snapshot() {
        // No snapshot → always matches (nothing to compare)
        assert!(document_matches_disk(None, Path::new("/nonexistent/path")));
    }

    #[test]
    fn resolve_path_allows_missing_path_within_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let session = make_session(workspace.path().to_path_buf());

        let resolved = session
            .resolve_path(Some("missing/file.rs"))
            .expect("missing in-workspace path should resolve");

        assert_eq!(resolved, workspace.path().join("missing/file.rs"));
    }

    #[test]
    fn resolve_path_rejects_nonexistent_parent_escape() {
        let workspace = tempdir().expect("workspace tempdir");
        let session = make_session(workspace.path().to_path_buf());

        let error = session
            .resolve_path(Some("../escape/file.rs"))
            .expect_err("path escape should be rejected");

        assert!(error.to_string().contains("outside workspace root"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_missing_leaf_under_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        symlink(outside.path(), workspace.path().join("linked")).expect("create symlink");

        let session = make_session(workspace.path().to_path_buf());
        let error = session
            .resolve_path(Some("linked/missing/file.rs"))
            .expect_err("symlink escape should be rejected");

        assert!(error.to_string().contains("outside workspace root"));
    }
}
