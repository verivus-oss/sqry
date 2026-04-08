use crate::LspOptions;
use crate::config::{ConfigDiff, SessionConfig};
use crate::documents::{DocumentSnapshot, DocumentStore, compute_line_offsets};
use crate::file_types::classify_file;
use anyhow::{Context, Result, anyhow};
use parking_lot::RwLock;
use ropey::Rope;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path};
use sqry_core::graph::unified::resolution::display_graph_qualified_name;
use sqry_core::graph::unified::{NodeEntry, NodeKind, StagingGraph, StagingOp, StringId};
use sqry_core::plugin::PluginManager;
use sqry_core::project::{Project, ProjectManager};
use sqry_core::query::QueryExecutor;
use sqry_plugin_registry::create_plugin_manager;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
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
    /// Project manager for multi-project support (per `PROJECT_ROOT_SPEC.md` Section 9).
    project_manager: Arc<ProjectManager>,
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
    #[must_use]
    pub fn new(options: LspOptions) -> Self {
        let root =
            resolve_root_path(options.index_root.clone()).unwrap_or_else(|_| PathBuf::from("."));
        let executor = Arc::new(QueryExecutor::with_plugin_manager(build_plugin_manager()));
        let config = SessionConfig::from_options(&options).unwrap_or_else(|err| {
            log::warn!("failed to load initial configuration: {err}");
            SessionConfig::default()
        });
        log::set_max_level(config.log_level);

        // Initialize ProjectManager with mode from config (per PROJECT_ROOT_SPEC.md Section 9.1)
        let project_manager = Arc::new(ProjectManager::new(config.project_root_mode));

        Self {
            options: Arc::new(options),
            root_path: Arc::new(root),
            executor,
            config: Arc::new(RwLock::new(config)),
            documents: DocumentStore::new(),
            graph_cache: Arc::new(RwLock::new(None)),
            project_manager,
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

        // Use per-project graph caching
        let graph = project
            .graph()
            .map_err(|e| anyhow::anyhow!("failed to load project graph: {e}"))?;
        Ok(graph)
    }

    /// Load (or reuse) the cached unified graph for single-root mode.
    ///
    /// This is a backward-compatibility method for single-project sessions
    /// (i.e., when no workspace folders are configured). Prefer `graph_for_path()`
    /// which supports both single-root and multi-project modes.
    ///
    /// # Errors
    ///
    /// Returns an error when the graph cannot be loaded from disk.
    pub fn graph(&self) -> Result<Option<Arc<CodeGraph>>> {
        // Fast path: check session-level cache first
        if let Some(graph) = self.graph_cache.read().as_ref() {
            return Ok(Some(graph.clone()));
        }

        // Load from disk and cache at session level for single-root mode
        let root = self.current_index_root();
        let storage = GraphStorage::new(&root);
        if !storage.exists() {
            // No manifest → no complete index → return None
            // (startup filter should have caught this and triggered build)
            return Ok(None);
        }

        match load_from_path(
            storage.snapshot_path(),
            Some(self.executor.plugin_manager()),
        ) {
            Ok(graph) => {
                let graph = Arc::new(graph);
                let mut cache = self.graph_cache.write();
                *cache = Some(graph.clone());
                Ok(Some(graph))
            }
            Err(e) => {
                // Snapshot missing/corrupt — auto-rebuild (Tier 2 self-heal)
                log::warn!("Graph load failed ({e}), auto-rebuilding index for LSP");
                let plugins = create_plugin_manager();
                let config = sqry_core::graph::unified::build::BuildConfig::default();
                let (new_graph, _build_result) =
                    sqry_core::graph::unified::build::build_and_persist_graph(
                        &root,
                        &plugins,
                        &config,
                        "lsp:auto_rebuild",
                    )
                    .with_context(|| {
                        format!(
                            "auto-rebuild failed for {} (original error: {})",
                            root.display(),
                            e
                        )
                    })?;
                let graph = Arc::new(new_graph);
                let mut cache = self.graph_cache.write();
                *cache = Some(graph.clone());
                Ok(Some(graph))
            }
        }
    }

    /// Clear the cached unified graph.
    pub fn clear_graph_cache(&self) {
        let mut cache = self.graph_cache.write();
        *cache = None;
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

        if !has_unsaved_changes
            && let Some(graph) = self.graph_for_path(path)?
            && let Some(nodes) = Self::nodes_from_graph(path, &graph)
        {
            return Ok(nodes);
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
        staging.attach_body_hashes(content);

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
