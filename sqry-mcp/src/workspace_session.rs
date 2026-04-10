//! Session-scoped workspace resolution for MCP requests.
//!
//! `sqry-mcp` currently serves a single stdio connection per process, so this
//! module keeps one connection-scoped workspace state on the server instance.
//! The implementation is structured as a registry so it can grow into a true
//! multi-connection transport without changing the resolution logic.

use anyhow::{Context, Result, bail};
use parking_lot::RwLock;
use rmcp::{RoleServer, model::ClientInfo, service::RequestContext};
use serde::Serialize;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use url::Url;

use crate::path_resolver::WorkspaceResolver;

thread_local! {
    static WORKSPACE_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

struct WorkspaceOverrideGuard {
    previous: Option<PathBuf>,
}

impl Drop for WorkspaceOverrideGuard {
    fn drop(&mut self) {
        WORKSPACE_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

/// Execute `f` with a thread-local workspace override for the current blocking
/// tool execution.
pub fn with_workspace_override<T>(workspace_root: Option<&Path>, f: impl FnOnce() -> T) -> T {
    let previous = WORKSPACE_OVERRIDE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let previous = guard.clone();
        *guard = workspace_root.map(Path::to_path_buf);
        previous
    });

    let _guard = WorkspaceOverrideGuard { previous };
    f()
}

/// Read the workspace override for the current blocking thread, if any.
#[must_use]
pub fn current_workspace_override() -> Option<PathBuf> {
    WORKSPACE_OVERRIDE.with(|cell| cell.borrow().clone())
}

/// Why the request resolved to a particular workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceResolutionSource {
    ExplicitPath,
    FileHint,
    SessionRoots,
    LastResolvedWorkspace,
    LegacyFallback,
}

impl WorkspaceResolutionSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitPath => "explicit_path",
            Self::FileHint => "file_hint",
            Self::SessionRoots => "session_roots",
            Self::LastResolvedWorkspace => "last_resolved_workspace",
            Self::LegacyFallback => "legacy_fallback",
        }
    }
}

/// Resolved workspace for one tool request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspaceContext {
    workspace_root: PathBuf,
    resolution_source: WorkspaceResolutionSource,
}

impl ResolvedWorkspaceContext {
    #[must_use]
    pub fn new(workspace_root: PathBuf, resolution_source: WorkspaceResolutionSource) -> Self {
        Self {
            workspace_root,
            resolution_source,
        }
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub const fn resolution_source(&self) -> WorkspaceResolutionSource {
        self.resolution_source
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkspaceHints {
    explicit_path: Option<String>,
    file_hints: Vec<String>,
}

impl WorkspaceHints {
    fn from_serializable<T: Serialize>(params: &T) -> Result<Self> {
        let value = serde_json::to_value(params).context("Failed to serialize MCP tool params")?;
        let object = value
            .as_object()
            .context("MCP tool params must serialize to a JSON object")?;

        let explicit_path = object
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty() && *path != ".")
            .map(ToOwned::to_owned);

        let mut file_hints = Vec::new();
        if let Some(file_path) = object.get("file_path").and_then(serde_json::Value::as_str) {
            let file_path = file_path.trim();
            if !file_path.is_empty() {
                file_hints.push(file_path.to_string());
            }
        }

        if let Some(expand_files) = object
            .get("expand_files")
            .and_then(serde_json::Value::as_array)
        {
            for file in expand_files {
                if let Some(file_path) =
                    file.as_str().map(str::trim).filter(|path| !path.is_empty())
                {
                    file_hints.push(file_path.to_string());
                }
            }
        }

        Ok(Self {
            explicit_path,
            file_hints,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct WorkspaceSessionState {
    client_supports_roots: bool,
    cached_roots: Vec<PathBuf>,
    roots_cache_valid: bool,
    last_resolved_workspace: Option<PathBuf>,
}

/// Session-scoped workspace registry.
#[derive(Debug, Default)]
pub struct WorkspaceSessionRegistry {
    state: RwLock<WorkspaceSessionState>,
    roots_fetch_lock: tokio::sync::Mutex<()>,
}

impl WorkspaceSessionRegistry {
    /// Record client capability information after initialize.
    pub fn record_client_info(&self, client_info: Option<&ClientInfo>) {
        let Some(client_info) = client_info else {
            return;
        };

        let client_supports_roots = client_info.capabilities.roots.is_some();
        {
            let state = self.state.read();
            if state.client_supports_roots == client_supports_roots {
                return;
            }
        }

        let mut state = self.state.write();
        state.client_supports_roots = client_supports_roots;
        if !state.client_supports_roots {
            state.cached_roots.clear();
            state.roots_cache_valid = false;
        }
    }

    /// Invalidate cached client roots after `notifications/roots/list_changed`.
    pub fn invalidate_roots(&self) {
        let mut state = self.state.write();
        state.roots_cache_valid = false;
    }

    /// Resolve the workspace for a tool request.
    pub async fn resolve_for_request<T: Serialize>(
        &self,
        params: &T,
        context: &RequestContext<RoleServer>,
    ) -> Result<ResolvedWorkspaceContext> {
        let hints = WorkspaceHints::from_serializable(params)?;
        let roots = self.session_roots(context).await?;
        let last_resolved = {
            let state = self.state.read();
            state.last_resolved_workspace.clone()
        };

        let resolved = if let Some(explicit_path) = hints
            .explicit_path
            .as_deref()
            .filter(|path| should_resolve_explicit_path(path, &roots, last_resolved.as_deref()))
        {
            ResolvedWorkspaceContext::new(
                resolve_explicit_workspace(explicit_path, &roots, last_resolved.as_deref())?,
                WorkspaceResolutionSource::ExplicitPath,
            )
        } else if let Some(workspace_root) =
            resolve_from_file_hints(&hints.file_hints, &roots, last_resolved.as_deref())?
        {
            ResolvedWorkspaceContext::new(workspace_root, WorkspaceResolutionSource::FileHint)
        } else if roots.len() == 1 {
            ResolvedWorkspaceContext::new(roots[0].clone(), WorkspaceResolutionSource::SessionRoots)
        } else if let Some(last_resolved_workspace) =
            last_resolved.filter(|path| roots.is_empty() || roots.iter().any(|root| root == path))
        {
            ResolvedWorkspaceContext::new(
                last_resolved_workspace,
                WorkspaceResolutionSource::LastResolvedWorkspace,
            )
        } else if roots.len() > 1 {
            bail!(
                "Multiple workspace roots are active for this MCP session: {}. \
                 Pass an explicit `path` to select one workspace.",
                display_paths(&roots)
            );
        } else {
            ResolvedWorkspaceContext::new(
                WorkspaceResolver::new(None).resolve()?,
                WorkspaceResolutionSource::LegacyFallback,
            )
        };

        let mut state = self.state.write();
        state.last_resolved_workspace = Some(resolved.workspace_root.clone());

        Ok(resolved)
    }

    async fn session_roots(&self, context: &RequestContext<RoleServer>) -> Result<Vec<PathBuf>> {
        let should_fetch = {
            let state = self.state.read();
            state.client_supports_roots && !state.roots_cache_valid
        };

        if !should_fetch {
            let state = self.state.read();
            return Ok(state.cached_roots.clone());
        }

        let _roots_fetch_guard = self.roots_fetch_lock.lock().await;
        let should_fetch = {
            let state = self.state.read();
            state.client_supports_roots && !state.roots_cache_valid
        };
        if !should_fetch {
            let state = self.state.read();
            return Ok(state.cached_roots.clone());
        }

        let roots_result = context
            .peer
            .list_roots()
            .await
            .context("Client advertised roots support, but `roots/list` failed")?;
        let roots = canonicalize_roots(&roots_result.roots)?;

        let mut state = self.state.write();
        state.cached_roots.clone_from(&roots);
        state.roots_cache_valid = true;

        Ok(roots)
    }
}

fn should_resolve_explicit_path(
    explicit_path: &str,
    roots: &[PathBuf],
    last_resolved: Option<&Path>,
) -> bool {
    Path::new(explicit_path).is_absolute() || !roots.is_empty() || last_resolved.is_some()
}

fn canonicalize_roots(roots: &[rmcp::model::Root]) -> Result<Vec<PathBuf>> {
    let mut canonical_roots = Vec::new();
    for root in roots {
        let Some(root_path) = root_uri_to_path(&root.uri)? else {
            continue;
        };
        let canonical_root = root_path
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize MCP root {}", root_path.display()))?;
        if !canonical_root.is_dir() {
            continue;
        }
        if !canonical_roots
            .iter()
            .any(|existing| existing == &canonical_root)
        {
            canonical_roots.push(canonical_root);
        }
    }
    canonical_roots.sort();
    Ok(canonical_roots)
}

fn root_uri_to_path(uri: &str) -> Result<Option<PathBuf>> {
    let parsed_uri = Url::parse(uri).with_context(|| format!("Invalid MCP root URI: {uri}"))?;
    if parsed_uri.scheme() != "file" {
        return Ok(None);
    }
    parsed_uri
        .to_file_path()
        .map(Some)
        .map_err(|()| anyhow::anyhow!("MCP root URI is not a valid file path: {uri}"))
}

fn resolve_explicit_workspace(
    explicit_path: &str,
    roots: &[PathBuf],
    last_resolved: Option<&Path>,
) -> Result<PathBuf> {
    let explicit_path_buf = PathBuf::from(explicit_path);
    if explicit_path_buf.is_absolute() {
        return canonicalize_workspace_candidate(&explicit_path_buf);
    }

    let mut base_candidates: Vec<PathBuf> = roots.to_vec();
    if let Some(last_resolved) = last_resolved
        && !base_candidates.iter().any(|root| root == last_resolved)
    {
        base_candidates.push(last_resolved.to_path_buf());
    }

    if base_candidates.is_empty() {
        return WorkspaceResolver::new(Some(explicit_path_buf)).resolve();
    }

    let matches = base_candidates
        .iter()
        .filter_map(|base| canonicalize_workspace_candidate(&base.join(explicit_path)).ok())
        .collect::<Vec<_>>();

    choose_unique_workspace(
        matches,
        "Explicit `path` matches multiple active workspaces. Pass an absolute `path` to disambiguate.",
    )
}

fn resolve_from_file_hints(
    file_hints: &[String],
    roots: &[PathBuf],
    last_resolved: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if file_hints.is_empty() {
        return Ok(None);
    }

    let mut inferred_roots = Vec::new();
    for file_hint in file_hints {
        let mut matched_roots = Vec::new();
        let file_path = PathBuf::from(file_hint);

        if file_path.is_absolute() {
            let canonical_file = file_path.canonicalize().with_context(|| {
                format!("Failed to canonicalize file hint {}", file_path.display())
            })?;
            matched_roots.extend(
                roots
                    .iter()
                    .filter(|root| canonical_file.starts_with(root.as_path()))
                    .cloned(),
            );
            if matched_roots.is_empty()
                && let Some(last_resolved) =
                    last_resolved.filter(|root| canonical_file.starts_with(*root))
            {
                matched_roots.push(last_resolved.to_path_buf());
            }
        } else {
            matched_roots.extend(
                roots
                    .iter()
                    .filter(|root| {
                        canonicalize_relative_file_hint(file_path.as_path(), root).is_ok()
                    })
                    .cloned(),
            );

            if matched_roots.is_empty()
                && let Some(last_resolved) = last_resolved.filter(|root| {
                    canonicalize_relative_file_hint(file_path.as_path(), root).is_ok()
                })
            {
                matched_roots.push(last_resolved.to_path_buf());
            }
        }

        if matched_roots.is_empty() {
            continue;
        }

        matched_roots.sort();
        matched_roots.dedup();
        if matched_roots.len() > 1 {
            bail!(
                "File hint `{}` matches multiple active workspaces: {}. \
                 Pass an explicit `path` to disambiguate.",
                file_hint,
                display_paths(&matched_roots)
            );
        }

        inferred_roots.push(matched_roots.remove(0));
    }

    if inferred_roots.is_empty() {
        return Ok(None);
    }

    inferred_roots.sort();
    inferred_roots.dedup();
    if inferred_roots.len() > 1 {
        bail!(
            "Request file hints point at different workspaces: {}. \
             Pass an explicit `path` to select one workspace.",
            display_paths(&inferred_roots)
        );
    }

    Ok(inferred_roots.into_iter().next())
}

fn canonicalize_relative_file_hint(relative_path: &Path, root: &Path) -> Result<PathBuf> {
    let joined = root.join(relative_path);
    let canonical_path = joined
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", joined.display()))?;
    if !canonical_path.starts_with(root) {
        bail!(
            "Path `{}` escapes workspace root `{}`",
            canonical_path.display(),
            root.display()
        );
    }
    Ok(canonical_path)
}

fn canonicalize_workspace_candidate(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize workspace candidate {}",
            path.display()
        )
    })?;

    if canonical.is_dir() {
        return Ok(canonical);
    }

    canonical
        .parent()
        .map(Path::to_path_buf)
        .context("Workspace candidate resolved to a file without a parent directory")
}

fn choose_unique_workspace(matches: Vec<PathBuf>, ambiguity_message: &str) -> Result<PathBuf> {
    let mut unique_matches = matches;
    unique_matches.sort();
    unique_matches.dedup();

    match unique_matches.len() {
        0 => bail!("Could not resolve a workspace from the provided request context"),
        1 => Ok(unique_matches.remove(0)),
        _ => bail!(
            "{ambiguity_message} Candidates: {}",
            display_paths(&unique_matches)
        ),
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn create_workspace() -> TempDir {
        TempDir::new().expect("temp dir")
    }

    #[test]
    fn workspace_hints_extract_standard_fields() {
        let hints = WorkspaceHints::from_serializable(&json!({
            "path": ".",
            "file_path": "sqry-mcp/src/server.rs",
            "expand_files": ["sqry-mcp/src/main.rs", ""]
        }))
        .expect("extract hints");

        assert_eq!(hints.explicit_path, None);
        assert_eq!(
            hints.file_hints,
            vec![
                "sqry-mcp/src/server.rs".to_string(),
                "sqry-mcp/src/main.rs".to_string()
            ]
        );
    }

    #[test]
    fn explicit_relative_path_uses_unique_root() {
        let workspace = create_workspace();
        let subdir = workspace.path().join("sqry-mcp");
        std::fs::create_dir_all(&subdir).expect("create subdir");

        let resolved =
            resolve_explicit_workspace("sqry-mcp", &[workspace.path().to_path_buf()], None)
                .expect("resolve");
        assert_eq!(resolved, subdir.canonicalize().expect("canonical subdir"));
    }

    #[test]
    fn explicit_relative_path_without_roots_or_history_uses_legacy_fallback() {
        assert!(!should_resolve_explicit_path("src", &[], None));
        assert!(should_resolve_explicit_path(
            "src",
            &[PathBuf::from("/tmp/workspace")],
            None
        ));
        assert!(should_resolve_explicit_path(
            "src",
            &[],
            Some(Path::new("/tmp/workspace"))
        ));
    }

    #[test]
    fn file_hint_selects_matching_root() {
        let workspace_a = create_workspace();
        let workspace_b = create_workspace();
        let file_a = workspace_a.path().join("src/lib.rs");
        let file_b = workspace_b.path().join("src/lib.rs");
        std::fs::create_dir_all(file_a.parent().expect("parent")).expect("mkdir a");
        std::fs::create_dir_all(file_b.parent().expect("parent")).expect("mkdir b");
        std::fs::write(&file_a, "fn a() {}\n").expect("write a");
        std::fs::write(&file_b, "fn b() {}\n").expect("write b");

        let resolved = resolve_from_file_hints(
            &["src/lib.rs".to_string()],
            &[workspace_a.path().to_path_buf()],
            Some(workspace_b.path()),
        )
        .expect("resolve")
        .expect("workspace");

        assert_eq!(
            resolved,
            workspace_a
                .path()
                .canonicalize()
                .expect("canonical workspace")
        );
    }

    #[test]
    fn file_hint_ambiguity_requires_explicit_path() {
        let workspace_a = create_workspace();
        let workspace_b = create_workspace();
        for workspace in [&workspace_a, &workspace_b] {
            let file = workspace.path().join("src/lib.rs");
            std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
            std::fs::write(&file, "fn sample() {}\n").expect("write");
        }

        let err = resolve_from_file_hints(
            &["src/lib.rs".to_string()],
            &[
                workspace_a.path().to_path_buf(),
                workspace_b.path().to_path_buf(),
            ],
            None,
        )
        .expect_err("ambiguous file hint should fail");

        assert!(
            err.to_string()
                .contains("matches multiple active workspaces")
        );
    }
}
