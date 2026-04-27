use crate::path_resolver::WorkspaceResolver;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::{
    GraphStorage, Manifest, load_from_bytes, verify_snapshot_bytes,
};
use sqry_core::plugin::PluginManager;
use sqry_core::query::QueryExecutor;
use sqry_plugin_registry::create_plugin_manager;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

//=============================================================================
// Cache Identity and Metadata Types
//=============================================================================

/// Canonical identifier for a code graph snapshot.
///
/// Derived from `.sqry/graph/manifest.json`. Ensures cache isolation between
/// workspaces and correct invalidation when graphs are rebuilt.
///
/// All fields contribute to equality: two `GraphIdentity` values are equal
/// only if they represent the exact same graph state (same content hash,
/// timestamp, format versions, and workspace).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GraphIdentity {
    /// SHA-256 hash of the snapshot content
    pub snapshot_sha256: String,
    /// Timestamp when the graph was built (RFC3339 from manifest)
    pub built_at: DateTime<Utc>,
    /// Schema version of the graph structure
    pub schema_version: u32,
    /// Binary format version of the snapshot
    pub snapshot_format_version: u32,
    /// Canonicalized workspace root path
    pub workspace_root: PathBuf,
}

/// Filesystem metadata for manifest freshness checks.
///
/// Used to detect manifest changes via fast stat syscalls before
/// re-reading the full manifest file.
#[derive(Clone, Debug)]
pub struct ManifestMetadata {
    /// Modification time of manifest.json
    pub mtime: SystemTime,
    /// File size in bytes
    pub size: u64,
    /// Platform-specific file identifier (inode on Unix, `file_index` on Windows)
    pub file_id: Option<u64>,
}

/// Cached engine with identity and metadata for freshness tracking.
///
/// Stored in the workspace engine cache (`ENGINE_CACHE`).
pub struct CachedEngine {
    /// The cached engine instance
    pub engine: Arc<Engine>,
    /// Identity of the loaded graph
    pub identity: GraphIdentity,
    /// Manifest metadata for freshness checks
    pub metadata: ManifestMetadata,
}

/// Global engine storage using `RwLock` to allow test reset
///
/// **Legacy**: Replaced by per-workspace engine cache (`ENGINE_CACHE`).
/// Retained for backward compatibility during migration.
#[allow(dead_code)]
static ENGINE: RwLock<Option<Arc<Engine>>> = RwLock::new(None);

pub struct Engine {
    workspace_root: PathBuf,
    executor: Arc<QueryExecutor>,
    graph_cache: RwLock<Option<Arc<CodeGraph>>>,
}

impl Engine {
    /// Initialize Engine from global environment (legacy single-workspace mode).
    ///
    /// **Legacy**: Replaced by `Engine::for_workspace()` for multi-workspace support.
    /// Retained for backward compatibility during migration.
    #[allow(dead_code)]
    fn initialize() -> Result<Self> {
        let workspace_root = resolve_workspace_root()?;
        tracing::info!(
            workspace_root = %workspace_root.display(),
            "Engine initializing with workspace root"
        );
        let plugin_manager = build_plugin_manager();
        let executor = Arc::new(QueryExecutor::with_plugin_manager(plugin_manager));
        Ok(Self {
            workspace_root,
            executor,
            graph_cache: RwLock::new(None),
        })
    }

    /// Create a new Engine for a specific workspace root.
    ///
    /// This bypasses the global singleton and creates a fresh Engine instance
    /// for the given workspace. Used for per-call workspace resolution.
    #[allow(clippy::unnecessary_wraps)] // Result for API consistency, may fail in future
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn for_workspace(workspace_root: PathBuf) -> Result<Self> {
        tracing::info!(
            workspace_root = %workspace_root.display(),
            "Creating Engine for specific workspace"
        );
        let plugin_manager = build_plugin_manager();
        let executor = Arc::new(QueryExecutor::with_plugin_manager(plugin_manager));
        Ok(Self {
            workspace_root,
            executor,
            graph_cache: RwLock::new(None),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn executor(&self) -> &QueryExecutor {
        &self.executor
    }

    /// Clone the shared `QueryExecutor` handle. Used by MCP tool handlers to
    /// build a [`crate::daemon_adapter::WorkspaceContext`] for
    /// `inner::execute_*` dispatch.
    #[must_use]
    pub fn executor_arc(&self) -> Arc<QueryExecutor> {
        Arc::clone(&self.executor)
    }

    #[must_use]
    pub fn plugin_manager() -> PluginManager {
        build_plugin_manager()
    }

    /// Returns the unified graph if available.
    ///
    /// The unified graph is loaded from `.sqry/graph/snapshot.sqry` if it exists.
    /// This provides access to `GraphSnapshot` for visualization and graph queries.
    pub fn graph(&self) -> Option<Arc<CodeGraph>> {
        {
            let cache = self.graph_cache.read();
            if let Some(graph) = cache.as_ref() {
                tracing::debug!("Returning cached graph");
                return Some(graph.clone());
            }
        }

        let storage = GraphStorage::new(&self.workspace_root);
        let snapshot_path = storage.snapshot_path();
        tracing::info!(
            snapshot_path = %snapshot_path.display(),
            exists = snapshot_path.exists(),
            "Checking for unified graph snapshot"
        );

        if !storage.exists() {
            tracing::warn!(
                workspace_root = %self.workspace_root.display(),
                "No unified graph snapshot found"
            );
            return None;
        }

        // Read manifest to get expected SHA256 for integrity verification.
        // If the manifest exists but is corrupt/unreadable, fail closed (return None)
        // rather than skipping verification — a corrupt manifest is suspicious.
        // Only skip verification when NO manifest exists (pre-manifest index format).
        let expected_sha256 = if storage.manifest_path().exists() {
            match std::fs::File::open(storage.manifest_path()).and_then(|f| {
                serde_json::from_reader::<_, Manifest>(f)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }) {
                Ok(manifest) => manifest.snapshot_sha256,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        manifest_path = %storage.manifest_path().display(),
                        "Manifest exists but cannot be read/parsed — refusing to load snapshot"
                    );
                    return None;
                }
            }
        } else {
            // No manifest at all — pre-manifest index format, skip integrity check
            String::new()
        };

        // Single-read: read bytes, verify hash, then deserialize (no TOCTOU)
        let snapshot_bytes = match std::fs::read(storage.snapshot_path()) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    snapshot_path = %storage.snapshot_path().display(),
                    "Failed to read snapshot"
                );
                return None;
            }
        };

        if let Err(e) = verify_snapshot_bytes(&snapshot_bytes, &expected_sha256) {
            // Could be transient during rebuild — log warning and return None
            tracing::warn!(
                error = %e,
                snapshot_path = %storage.snapshot_path().display(),
                "Snapshot integrity verification failed"
            );
            return None;
        }

        // Deserialize from the already-verified bytes (no second file read)
        match load_from_bytes(&snapshot_bytes, Some(self.executor.plugin_manager())) {
            Ok(graph) => {
                let arc = Arc::new(graph);
                let mut cache = self.graph_cache.write();
                *cache = Some(arc.clone());
                tracing::info!("Successfully loaded unified graph");
                Some(arc)
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    snapshot_path = %storage.snapshot_path().display(),
                    "Failed to load unified graph snapshot"
                );
                None
            }
        }
    }

    /// Get the graph, auto-building the index if no snapshot exists.
    ///
    /// Unlike `graph()` which returns `None` when no snapshot is found,
    /// this method triggers a full build pipeline (graph + snapshot + manifest + analysis).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Auto-indexing is disabled and no snapshot exists
    /// - Graph building fails
    /// - Persistence fails
    pub fn ensure_graph(&self) -> Result<Arc<CodeGraph>> {
        // Fast path: cached or existing graph
        if let Some(graph) = self.graph() {
            return Ok(graph);
        }

        // Check opt-out
        if !is_auto_index_enabled() {
            bail!(
                "No unified graph found. Auto-indexing is disabled (SQRY_AUTO_INDEX=false). \
                 Run `sqry index` to create the graph."
            );
        }

        // Daemon workspace mutual exclusion: bail if sqryd is already managing this workspace.
        // Must run before the slow build path to avoid concurrent writer corruption.
        check_daemon_workspace_conflict(&self.workspace_root)?;

        // Slow path: auto-build
        tracing::info!(
            workspace = %self.workspace_root.display(),
            "Auto-building graph index (no existing snapshot found)"
        );

        let plugins = create_plugin_manager();
        let config = sqry_core::graph::unified::build::BuildConfig::default();
        let (graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
            &self.workspace_root,
            &plugins,
            &config,
            "mcp:auto_index",
        )?;

        let arc = Arc::new(graph);
        let mut cache = self.graph_cache.write();
        *cache = Some(arc.clone());
        Ok(arc)
    }

    #[allow(dead_code)]
    pub fn clear_graph_cache(&self) {
        let mut cache = self.graph_cache.write();
        *cache = None;
    }
}

/// Get the global engine instance, initializing it if necessary (legacy).
///
/// Returns an `Arc<Engine>` that can be cloned and shared.
///
/// **Legacy**: Replaced by `engine_for_workspace()` for multi-workspace support.
/// Retained for backward compatibility during migration.
#[allow(dead_code)]
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn engine() -> Result<Arc<Engine>> {
    // Fast path: try to read existing engine
    {
        let guard = ENGINE.read();
        if let Some(ref engine) = *guard {
            return Ok(engine.clone());
        }
    }

    // Slow path: need to initialize
    let mut guard = ENGINE.write();
    // Double-check pattern: another thread may have initialized while we waited
    if let Some(ref engine) = *guard {
        return Ok(engine.clone());
    }

    let new_engine = Arc::new(Engine::initialize()?);
    *guard = Some(new_engine.clone());
    Ok(new_engine)
}

/// Get or create an engine for a specific workspace path.
///
/// This function uses the `WorkspaceResolver`'s 4-tier resolution strategy:
/// 1. Explicit `path` parameter (if provided)
/// 2. `SQRY_MCP_WORKSPACE_ROOT` environment variable (primary)
/// 3. `SQRY_WORKSPACE_ROOT` environment variable (legacy fallback)
/// 4. Upward directory discovery from CWD
///
/// The resolved workspace path is canonicalized and cached. On cache hit,
/// a freshness check (stat syscall) verifies the manifest hasn't changed.
/// On cache miss or stale entry, a fresh Engine is loaded and cached.
///
/// # Cache Insertion
///
/// When loading a fresh engine, this function also reads the `GraphIdentity`
/// and `ManifestMetadata` to populate the cache entry. This enables future
/// freshness checks without re-parsing the manifest.
///
/// # Errors
///
/// Returns an error if:
/// - Workspace resolution fails (no .sqry/graph found)
/// - Cache not initialized (call `init_engine_cache()` first)
/// - Manifest is missing or corrupt
/// - `GraphIdentity` validation fails (`root_path` mismatch)
pub fn engine_for_workspace(explicit_path: Option<&PathBuf>) -> Result<Arc<Engine>> {
    // Request-scoped resolution in `server.rs` is authoritative once the
    // blocking closure starts. In that case the override intentionally shadows
    // any explicit path passed by legacy tool code.
    if let Some(workspace_root) = crate::workspace_session::current_workspace_override() {
        return engine_for_workspace_root(&workspace_root);
    }

    // Use discovery cache if path is provided, otherwise fall back to direct resolution
    let workspace_root = if let Some(path) = explicit_path {
        // Use cached discovery for performance and platform-specific normalization
        crate::path_resolver::resolve_workspace_path(&path.to_string_lossy())?
    } else {
        // No explicit path - resolve from cwd/env
        let resolver = WorkspaceResolver::new(None);
        resolver.resolve()?
    };
    engine_for_workspace_root(&workspace_root)
}

fn engine_for_workspace_root(workspace_root: &Path) -> Result<Arc<Engine>> {
    // Canonicalization guarantee: workspace_root is always canonical
    // (WorkspaceResolver already canonicalizes)
    if !workspace_root.is_absolute() {
        bail!(
            "BUG: engine_for_workspace requires canonical path, got: {}",
            workspace_root.display()
        );
    }

    // Check cache first
    if let Some(engine) = get_cached_engine(workspace_root)? {
        tracing::debug!(
            workspace = %workspace_root.display(),
            "Using cached engine"
        );
        return Ok(engine);
    }

    // Cache miss or stale - load fresh engine
    tracing::info!(
        workspace = %workspace_root.display(),
        "Loading fresh engine (cache miss or stale)"
    );

    let engine = Arc::new(Engine::for_workspace(workspace_root.to_path_buf())?);

    // Read GraphIdentity and metadata atomically for cache insertion.
    // If manifest doesn't exist yet (pre auto-index), skip caching —
    // ensure_graph() will build it and the next call will cache properly.
    match read_graph_identity_with_metadata(workspace_root) {
        Ok((identity, metadata)) => {
            let mut cache = ENGINE_CACHE.lock();
            let lru = cache
                .as_mut()
                .context("Engine cache not initialized - call init_engine_cache() first")?;

            lru.put(
                workspace_root.to_path_buf(),
                CachedEngine {
                    engine: Arc::clone(&engine),
                    identity,
                    metadata,
                },
            );

            tracing::debug!(
                workspace = %workspace_root.display(),
                cache_size = lru.len(),
                "Engine cached"
            );
        }
        Err(e) => {
            tracing::info!(
                workspace = %workspace_root.display(),
                error = %e,
                "No manifest found — engine created without cache identity \
                 (auto-index will create it on first tool call)"
            );
        }
    }

    Ok(engine)
}

/// Resolve workspace root from environment or CWD (legacy).
///
/// **Legacy**: Replaced by `WorkspaceResolver` for configurable multi-tier resolution.
/// Retained for backward compatibility during migration.
#[allow(dead_code)]
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn resolve_workspace_root() -> Result<PathBuf> {
    let root = std::env::var("SQRY_MCP_WORKSPACE_ROOT").ok();
    let root_path = match root {
        Some(r) => PathBuf::from(r),
        None => std::env::current_dir().context("Failed to get current directory")?,
    };
    let canon = std::fs::canonicalize(&root_path).with_context(|| {
        format!(
            "Failed to canonicalize workspace root: {}",
            root_path.display()
        )
    })?;
    Ok(canon)
}

/// Typed errors emitted by the workspace-bound canonicalization paths.
///
/// Used by [`canonicalize_in_workspace`] /
/// [`canonicalize_in_workspace_with_logical`] /
/// [`crate::path_resolver::resolve_path_in_logical_workspace`] so callers
/// (MCP tool handlers, the redaction crate, future LSP integrations) can
/// distinguish "outside workspace" (invalid input) from "explicitly
/// excluded" (criterion 9: exclusions take precedence over containment)
/// without parsing free-form `anyhow` strings.
#[derive(Debug, thiserror::Error)]
pub enum WorkspacePathError {
    /// Path resolved outside the workspace root (containment violation).
    #[error("Path '{path}' is outside of the workspace root '{workspace_root}'")]
    OutsideWorkspace {
        /// The offending path as supplied (or normalized).
        path: PathBuf,
        /// The workspace root the request was resolved against.
        workspace_root: PathBuf,
    },
    /// Path matched a `LogicalWorkspace::exclusions` entry. Exclusion
    /// match is checked **before** the workspace-bound check so an
    /// excluded path is rejected even when it would have been
    /// in-workspace per containment alone (acceptance criterion 9).
    #[error("Path '{path}' is excluded by the logical workspace policy")]
    Excluded {
        /// The offending path as supplied.
        path: PathBuf,
    },
    /// Filesystem canonicalization (`realpath(3)`) failed — typically
    /// because the path does not exist.
    #[error("Failed to canonicalize path: {path} ({source})")]
    Canonicalize {
        /// The offending path.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn canonicalize_in_workspace(path_str: &str, workspace_root: &Path) -> Result<PathBuf> {
    canonicalize_in_workspace_inner(path_str, workspace_root, None).map_err(Into::into)
}

/// Workspace-bound canonicalization with optional `LogicalWorkspace`
/// awareness — `STEP_7` acceptance criterion 9.
///
/// When `workspace` is `Some`, the function checks the workspace's
/// `exclusions` list **before** the containment check; an excluded path
/// is rejected with [`WorkspacePathError::Excluded`] regardless of where
/// it sits relative to the workspace root.
///
/// When `workspace` is `None` the behavior is identical to
/// [`canonicalize_in_workspace`].
///
/// Tagged `#[allow(dead_code)]` because tool wiring to honour
/// exclusions is staged behind the redactor today; the tool-side caller
/// is added in a follow-on workspace-aware-cross-repo step.
///
/// # Errors
///
/// Returns [`WorkspacePathError`] for excluded, out-of-workspace, or
/// uncanonicalizable inputs.
#[allow(dead_code)]
pub fn canonicalize_in_workspace_with_logical(
    path_str: &str,
    workspace_root: &Path,
    workspace: Option<&sqry_core::workspace::LogicalWorkspace>,
) -> Result<PathBuf, WorkspacePathError> {
    canonicalize_in_workspace_inner(path_str, workspace_root, workspace)
}

fn canonicalize_in_workspace_inner(
    path_str: &str,
    workspace_root: &Path,
    workspace: Option<&sqry_core::workspace::LogicalWorkspace>,
) -> Result<PathBuf, WorkspacePathError> {
    let input_path = Path::new(path_str);
    let joined = if input_path.is_absolute() {
        input_path.to_path_buf()
    } else {
        workspace_root.join(input_path)
    };

    // Normalize the path to resolve ".." and "." components without requiring file existence
    // This is critical for security - we must detect directory traversal even if path doesn't exist
    let normalized = normalize_path(&joined);

    // Acceptance criterion 9: exclusions take precedence over the
    // workspace-bound check. A path that matches an `exclusions` entry
    // is rejected even when it would have been accepted by containment.
    if let Some(ws) = workspace
        && exclusion_matches(&normalized, ws)
    {
        return Err(WorkspacePathError::Excluded {
            path: normalized.clone(),
        });
    }

    // Security check: Ensure the normalized path is within workspace root
    // This check must happen BEFORE canonicalization to catch malicious paths
    if !normalized.starts_with(workspace_root) {
        return Err(WorkspacePathError::OutsideWorkspace {
            path: normalized,
            workspace_root: workspace_root.to_path_buf(),
        });
    }

    // Now attempt to canonicalize the actual path (follows symlinks, verifies existence)
    // If this fails, we return the failure, but we've already prevented directory traversal
    let canon =
        std::fs::canonicalize(&joined).map_err(|source| WorkspacePathError::Canonicalize {
            path: joined.clone(),
            source,
        })?;

    // Re-check exclusions after symlink resolution — a symlink could
    // point into an excluded directory even when the lexical path did
    // not (criterion 9 robustness).
    if let Some(ws) = workspace
        && exclusion_matches(&canon, ws)
    {
        return Err(WorkspacePathError::Excluded { path: canon });
    }

    // Double-check after canonicalization (symlinks could still escape).
    // Canonicalize the workspace root too — on macOS, /var is a symlink to
    // /private/var, so the canonical path and raw workspace_root may diverge.
    let canon_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    if !canon.starts_with(&canon_root) {
        return Err(WorkspacePathError::OutsideWorkspace {
            path: canon,
            workspace_root: canon_root,
        });
    }
    Ok(canon)
}

/// Returns `true` if `path` matches one of `workspace.exclusions()`
/// (exact or descendant) — same semantics as
/// [`sqry_core::workspace::LogicalWorkspace::classify`]'s exclusion
/// branch.
fn exclusion_matches(path: &Path, workspace: &sqry_core::workspace::LogicalWorkspace) -> bool {
    workspace
        .exclusions()
        .iter()
        .any(|excl| path == excl.as_path() || path.starts_with(excl))
}

/// Normalize a path by resolving "." and ".." components without accessing the filesystem.
///
/// This is used for security checks to detect directory traversal attempts even when
/// the path doesn't exist or is a broken symlink.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Only pop if we have relative components to go back from
                // If we're at the root, ignore the ".."
                if !components.is_empty() {
                    components.pop();
                }
            }
            std::path::Component::CurDir => {
                // Skip "." components
            }
            comp => {
                components.push(comp);
            }
        }
    }

    components.iter().collect()
}

fn build_plugin_manager() -> PluginManager {
    create_plugin_manager()
}

/// Check if auto-indexing is enabled.
///
/// Auto-indexing is on by default. It can be disabled via:
/// - `SQRY_AUTO_INDEX=false` or `SQRY_AUTO_INDEX=0` environment variable
/// - `auto_index: false` in MCP config
///
/// Environment variable takes priority over config.
fn is_auto_index_enabled() -> bool {
    if let Ok(val) = std::env::var("SQRY_AUTO_INDEX") {
        return val != "false" && val != "0";
    }
    // Default: enabled
    true
}

/// Check whether a running sqryd daemon is already managing `workspace_root`.
///
/// If the daemon socket is reachable and reports a workspace matching
/// `workspace_root`, standalone auto-indexing is unsafe (concurrent writer).
/// Bail with a clear message so the caller can switch to `--daemon` mode or
/// stop the daemon.
///
/// # Bail conditions
///
/// - `SQRY_FORCE_STANDALONE=1` → skips check entirely (warn logged)
/// - Socket not present → `Ok(())` (daemon not running, proceed)
/// - Connect/handshake failure → `Ok(())` (daemon dead socket, proceed)
/// - `daemon/status` timeout → `Ok(())` (daemon unresponsive, do not block auto-index)
/// - `daemon/status` RPC error → `Ok(())` (status unavailable, proceed)
/// - Workspace found in daemon → `Err(...)` (concurrent writer risk)
/// - Workspace NOT found in daemon → `Ok(())` (different workspace, safe)
fn check_daemon_workspace_conflict(workspace_root: &Path) -> Result<()> {
    if is_force_standalone() {
        tracing::warn!(
            workspace = %workspace_root.display(),
            "SQRY_FORCE_STANDALONE=1: skipping daemon workspace conflict check. \
             Concurrent writer risk accepted by caller."
        );
        return Ok(());
    }

    let socket_path = crate::daemon_shim::resolve_daemon_socket(None);
    if !socket_path.exists() {
        tracing::debug!(
            socket = %socket_path.display(),
            "Daemon socket not present — no conflict check needed"
        );
        return Ok(());
    }

    // We are in a sync context (ensure_graph is sync) but may be called from within
    // a tokio executor (rmcp spawns an async task per connection).
    // block_in_place suspends the executor thread so we can block_on safely without
    // triggering "Cannot start a runtime from within a runtime".
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let workspace_owned = workspace_root.to_path_buf();
            let socket_owned = socket_path.clone();
            tokio::task::block_in_place(move || {
                handle.block_on(async move {
                    check_daemon_workspace_conflict_async(&workspace_owned, &socket_owned).await
                })
            })
        }
        Err(_) => {
            // No async runtime available (e.g., called from a pure sync test binary).
            tracing::debug!(
                socket = %socket_path.display(),
                "No async runtime available — skipping daemon conflict check"
            );
            Ok(())
        }
    }
}

/// Async inner body of [`check_daemon_workspace_conflict`].
///
/// Connects to the daemon, fetches `daemon/status`, and checks whether the
/// daemon's workspace list includes `workspace_root`.
async fn check_daemon_workspace_conflict_async(
    workspace_root: &Path,
    socket_path: &Path,
) -> Result<()> {
    let connect_budget = Duration::from_secs(1);
    let hello_budget = Duration::from_secs(1);
    let status_budget = Duration::from_secs(2);

    let mut client = match sqry_daemon_client::DaemonClient::connect_with_timeouts(
        socket_path,
        connect_budget,
        hello_budget,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                socket = %socket_path.display(),
                error = %e,
                "Daemon socket present but connect/handshake failed — proceeding standalone"
            );
            return Ok(());
        }
    };

    let status_value = match tokio::time::timeout(status_budget, client.status()).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::debug!(
                socket = %socket_path.display(),
                error = %e,
                "daemon/status RPC failed — proceeding standalone"
            );
            return Ok(());
        }
        Err(_elapsed) => {
            tracing::warn!(
                socket = %socket_path.display(),
                timeout_secs = status_budget.as_secs(),
                "daemon/status timed out — proceeding standalone \
                 (do not block auto-index on unresponsive daemon)"
            );
            return Ok(());
        }
    };

    let canonical_workspace = match std::fs::canonicalize(workspace_root) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(
                workspace = %workspace_root.display(),
                error = %e,
                "Could not canonicalize workspace root — skipping conflict check"
            );
            return Ok(());
        }
    };

    // daemon/status returns a ResponseEnvelope<DaemonStatus> serialised as JSON.
    // DaemonStatus is in sqry-daemon which is not a dep of sqry-mcp — parse as
    // raw serde_json::Value.
    //
    // Wire shape:  { "result": { "workspaces": [...], ... }, "meta": { ... } }
    //
    // DaemonStatus does not carry a daemon_pid field; attempt to read the PID
    // from the pidfile in the same runtime directory as the socket.
    let daemon_pid = read_daemon_pid_from_socket_dir(socket_path)
        .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());

    let Some(workspaces) = status_value
        .get("result")
        .and_then(|r| r.get("workspaces"))
        .and_then(serde_json::Value::as_array)
    else {
        tracing::debug!(
            socket = %socket_path.display(),
            "daemon/status returned no 'result.workspaces' array — no conflict detected"
        );
        return Ok(());
    };

    for ws in workspaces {
        let Some(root_str) = ws.get("index_root").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let daemon_ws_root = PathBuf::from(root_str);
        let canonical_daemon_root =
            std::fs::canonicalize(&daemon_ws_root).unwrap_or(daemon_ws_root);
        if canonical_daemon_root == canonical_workspace {
            bail!(
                "sqryd daemon (PID {daemon_pid}) is managing workspace '{}'. \
                 Use --daemon mode or stop the daemon before standalone auto-indexing.",
                canonical_workspace.display()
            );
        }
    }

    tracing::debug!(
        workspace = %workspace_root.display(),
        socket = %socket_path.display(),
        "No daemon workspace conflict detected — proceeding with standalone auto-index"
    );
    Ok(())
}

/// Returns `true` when `SQRY_FORCE_STANDALONE=1` (or `true`) is set.
///
/// This env var is an escape hatch for callers that know it is safe to run
/// standalone alongside a daemon (e.g., different snapshots, test harnesses).
fn is_force_standalone() -> bool {
    matches!(
        std::env::var("SQRY_FORCE_STANDALONE").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Read the daemon PID from the `sqryd.pid` file in the same runtime directory
/// as the daemon socket.
///
/// `sqryd.pid` is written atomically by the daemon on start and unlinked on
/// clean exit. The file lives next to the socket:
/// - Unix: `$XDG_RUNTIME_DIR/sqry/sqryd.pid` or `$TMPDIR/sqry-<uid>/sqryd.pid`
/// - Windows: `%LOCALAPPDATA%\sqry\sqryd.pid`
///
/// Returns `None` if the file does not exist or cannot be parsed.
fn read_daemon_pid_from_socket_dir(socket_path: &Path) -> Option<u32> {
    let pid_path = socket_path.parent()?.join("sqryd.pid");
    let contents = std::fs::read_to_string(&pid_path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Simple concurrency guard for future mutable operations (e.g., index updates).
#[allow(dead_code)]
pub static WORKSPACE_LOCK: OnceLock<RwLock<()>> = OnceLock::new();

#[allow(dead_code)]
pub fn workspace_lock() -> &'static RwLock<()> {
    WORKSPACE_LOCK.get_or_init(|| RwLock::new(()))
}

//=============================================================================
// GraphIdentity Operations
//=============================================================================

/// Read `GraphIdentity` from the manifest.json file.
///
/// Validates that the manifest's `root_path` matches the provided workspace
/// path to prevent cross-workspace cache poisoning via symlinked `.sqry/graph/`
/// directories.
///
/// # Errors
///
/// Returns an error if:
/// - Manifest file is missing or corrupt
/// - Manifest cannot be parsed as valid JSON
/// - `root_path` validation fails (mismatch between manifest and workspace)
/// - `DateTime` parsing fails
pub fn read_graph_identity(workspace: &Path) -> Result<GraphIdentity> {
    let manifest_path = workspace.join(".sqry/graph/manifest.json");

    // Read and parse manifest
    let file = std::fs::File::open(&manifest_path).with_context(|| {
        format!(
            "Manifest missing - run `sqry index` in workspace: {}",
            workspace.display()
        )
    })?;

    let manifest: sqry_core::graph::unified::persistence::Manifest = serde_json::from_reader(file)
        .context("Failed to parse manifest.json - index may be corrupt")?;

    // Validate workspace root path
    let canonical_workspace = std::fs::canonicalize(workspace)?;
    // Resolve manifest root_path relative to workspace if it's a relative path
    let manifest_root_path = PathBuf::from(&manifest.root_path);
    let manifest_root = if manifest_root_path.is_absolute() {
        std::fs::canonicalize(&manifest_root_path)?
    } else {
        // Relative path - resolve relative to workspace, not cwd
        std::fs::canonicalize(workspace.join(&manifest_root_path))?
    };

    if canonical_workspace != manifest_root {
        bail!(
            "Manifest root_path mismatch: expected {}, got {}. \
             Possible symlinked .sqry/graph from different repo.",
            canonical_workspace.display(),
            manifest_root.display()
        );
    }

    // Parse built_at timestamp
    let built_at = DateTime::parse_from_rfc3339(&manifest.built_at)
        .with_context(|| {
            format!(
                "Invalid built_at timestamp in manifest: {}",
                manifest.built_at
            )
        })?
        .with_timezone(&Utc);

    Ok(GraphIdentity {
        snapshot_sha256: manifest.snapshot_sha256,
        built_at,
        schema_version: manifest.schema_version,
        snapshot_format_version: manifest.snapshot_format_version,
        workspace_root: canonical_workspace,
    })
}

/// Read manifest metadata for freshness checks.
///
/// Extracts filesystem metadata (mtime, size, file identifier) from the
/// manifest.json file to enable fast staleness detection via stat syscalls.
///
/// **Note**: Prefer `read_graph_identity_with_metadata()` for production use to avoid
/// TOCTOU issues. This function is retained for testing.
///
/// # Errors
///
/// Returns an error if the manifest file cannot be accessed or stat fails.
#[allow(dead_code)]
pub fn read_manifest_metadata(workspace: &Path) -> Result<ManifestMetadata> {
    let manifest_path = workspace.join(".sqry/graph/manifest.json");
    let metadata = std::fs::metadata(&manifest_path).context("Failed to stat manifest.json")?;

    let file_id = extract_file_id(&metadata);

    Ok(ManifestMetadata {
        mtime: metadata.modified()?,
        size: metadata.len(),
        file_id,
    })
}

/// Read graph identity and manifest metadata atomically.
///
/// This ensures identity and metadata are from the same file instance,
/// eliminating TOCTOU windows where a manifest update between identity
/// and metadata reads could cause stale data pairing.
///
/// # Errors
///
/// Returns an error if:
/// - Manifest file is missing or inaccessible
/// - JSON parsing fails
/// - Workspace root validation fails
/// - Timestamp parsing fails
pub fn read_graph_identity_with_metadata(
    workspace: &Path,
) -> Result<(GraphIdentity, ManifestMetadata)> {
    let manifest_path = workspace.join(".sqry/graph/manifest.json");

    // Open file once and get both content and metadata
    let file = std::fs::File::open(&manifest_path).with_context(|| {
        format!(
            "Manifest missing - run `sqry index` in workspace: {}",
            workspace.display()
        )
    })?;

    // Get metadata from the same file handle
    let file_metadata = file
        .metadata()
        .context("Failed to stat manifest.json from open file handle")?;

    // Parse manifest content
    let manifest: sqry_core::graph::unified::persistence::Manifest = serde_json::from_reader(file)
        .context("Failed to parse manifest.json - index may be corrupt")?;

    // Validate workspace root path
    let canonical_workspace = std::fs::canonicalize(workspace)?;
    // Resolve manifest root_path relative to workspace if it's a relative path
    let manifest_root_path = PathBuf::from(&manifest.root_path);
    let manifest_root = if manifest_root_path.is_absolute() {
        std::fs::canonicalize(&manifest_root_path)?
    } else {
        // Relative path - resolve relative to workspace, not cwd
        std::fs::canonicalize(workspace.join(&manifest_root_path))?
    };

    if canonical_workspace != manifest_root {
        bail!(
            "Manifest root_path mismatch: expected {}, got {}. \
             Possible symlinked .sqry/graph from different repo.",
            canonical_workspace.display(),
            manifest_root.display()
        );
    }

    // Parse built_at timestamp
    let built_at = DateTime::parse_from_rfc3339(&manifest.built_at)
        .with_context(|| {
            format!(
                "Invalid built_at timestamp in manifest: {}",
                manifest.built_at
            )
        })?
        .with_timezone(&Utc);

    let identity = GraphIdentity {
        snapshot_sha256: manifest.snapshot_sha256,
        built_at,
        schema_version: manifest.schema_version,
        snapshot_format_version: manifest.snapshot_format_version,
        workspace_root: canonical_workspace,
    };

    let file_id = extract_file_id(&file_metadata);
    let metadata = ManifestMetadata {
        mtime: file_metadata.modified()?,
        size: file_metadata.len(),
        file_id,
    };

    Ok((identity, metadata))
}

/// Extract platform-specific file identifier from metadata.
///
/// - Unix: inode number
/// - Windows: `file_index` (within volume)
/// - Other platforms: None (fallback to mtime+size only)
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // Option required for cross-platform API consistency
fn extract_file_id(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)] // Option required for cross-platform API consistency
fn extract_file_id(_metadata: &std::fs::Metadata) -> Option<u64> {
    // `MetadataExt::file_index()` requires unstable feature `windows_by_handle` (rust#63010).
    // Falls back to mtime+size comparison until the API is stabilized.
    None
}

#[cfg(not(any(unix, windows)))]
fn extract_file_id(_metadata: &std::fs::Metadata) -> Option<u64> {
    None // Fallback: mtime+size only on unsupported platforms
}

//=============================================================================
// Engine Cache Infrastructure
//=============================================================================

/// Global engine cache using LRU eviction.
///
/// Maps canonicalized workspace paths to `CachedEngine` entries. Cache entries
/// include `GraphIdentity` and `ManifestMetadata` to enable freshness checks without
/// re-reading the full manifest on every access.
///
/// Uses `Mutex<Option<...>>` so tests can reset state between runs.
/// Must be initialized via `init_engine_cache()` before first use
/// (typically during server startup).
static ENGINE_CACHE: parking_lot::Mutex<Option<lru::LruCache<PathBuf, CachedEngine>>> =
    parking_lot::Mutex::new(None);

/// Initialize the engine cache with the specified capacity.
///
/// This function must be called during server initialization before any
/// cache access. Subsequent calls are no-ops (idempotent).
///
/// # Panics
///
/// Panics if capacity is zero (prevented by `NonZeroUsize` type).
pub fn init_engine_cache(capacity: std::num::NonZeroUsize) {
    let mut cache = ENGINE_CACHE.lock();
    if cache.is_none() {
        tracing::info!(capacity = capacity.get(), "Initializing engine cache");
        *cache = Some(lru::LruCache::new(capacity));
    }
}

/// Get a cached engine for the given workspace, if available and fresh.
///
/// This function performs a three-phase freshness check:
/// 1. **Fast path**: Cache hit + manifest still fresh (stat syscall)
/// 2. **Reload path**: Manifest changed → reload identity + re-validate
/// 3. **TOCTOU guard**: Re-check cache after reload (another thread may have updated)
///
/// # Freshness Check
///
/// A cached engine is considered fresh if the manifest's `mtime`, `size`, and
/// `file_id` (`inode/file_index`) match the cached metadata. This avoids re-parsing
/// the manifest JSON on every access.
///
/// # TOCTOU Safety
///
/// If the manifest appears stale, we reload it outside the lock. Before invalidating
/// the cache, we re-lock and verify that another thread hasn't already updated the
/// entry with a different `GraphIdentity`. This prevents race conditions where:
/// - Thread A: sees stale metadata, releases lock
/// - Thread B: updates cache with new identity
/// - Thread A: re-locks and overwrites B's update incorrectly
///
/// The full `GraphIdentity` comparison ensures we only invalidate if the identity
/// truly changed.
///
/// # Returns
///
/// - `Ok(Some(engine))` if cached and fresh
/// - `Ok(None)` if cache miss or stale (caller should load fresh engine)
/// - `Err(_)` if cache not initialized or I/O error during freshness check
///
/// # Errors
///
/// Returns an error if:
/// - Cache not initialized (call `init_engine_cache()` first)
/// - Filesystem I/O errors during stat or manifest read
fn get_cached_engine(workspace: &Path) -> Result<Option<Arc<Engine>>> {
    // Phase 1: Lock scope - copy cached data for out-of-lock validation
    let (cached_engine, cached_identity, cached_metadata) = {
        let mut cache = ENGINE_CACHE.lock();
        let lru = cache
            .as_mut()
            .context("Engine cache not initialized - call init_engine_cache() first")?;
        if let Some(cached) = lru.get(workspace) {
            (
                Arc::clone(&cached.engine),
                cached.identity.clone(),
                cached.metadata.clone(),
            )
        } else {
            // Cache miss
            return Ok(None);
        }
    };

    // Phase 2: I/O outside lock - check if manifest is fresh via stat
    if is_manifest_fresh(&cached_metadata, workspace)? {
        // Fast path: manifest unchanged, return cached engine
        tracing::debug!(
            workspace = %workspace.display(),
            "Engine cache hit (fresh)"
        );
        return Ok(Some(cached_engine));
    }

    // Phase 3: Cold path - manifest stale or missing.
    //
    // If the manifest is absent (e.g. transiently deleted during a rebuild window),
    // evict the cache entry and return Ok(None) so the caller can trigger a fresh
    // load/rebuild.  Attempting read_graph_identity_with_metadata on a missing file
    // would propagate a "Manifest missing" error up to the MCP caller, which is the
    // very surface this fix targets.
    let manifest_path = workspace.join(".sqry/graph/manifest.json");
    match std::fs::metadata(&manifest_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                workspace = %workspace.display(),
                "Manifest absent during cache reload — evicting cache entry"
            );
            let mut cache = ENGINE_CACHE.lock();
            if let Some(lru) = cache.as_mut() {
                lru.pop(workspace);
            }
            return Ok(None);
        }
        Err(e) => {
            return Err(e).context("Failed to stat manifest.json during cache reload");
        }
        Ok(_) => {} // manifest present — proceed with full reload below
    }

    tracing::debug!(
        workspace = %workspace.display(),
        "Manifest changed, reloading identity"
    );

    // TOCTOU gap: the manifest could be removed between the stat above and here.
    // Handle NotFound from read_graph_identity_with_metadata the same way as the
    // stat-based NotFound check above: evict and return Ok(None).
    let (new_identity, new_metadata) = match read_graph_identity_with_metadata(workspace) {
        Ok(pair) => pair,
        Err(e) => {
            // Inspect the error chain for an underlying NotFound to handle the
            // TOCTOU case (manifest removed after our stat succeeded).
            let is_not_found = e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|ioe| ioe.kind() == std::io::ErrorKind::NotFound)
            });
            if is_not_found {
                tracing::debug!(
                    workspace = %workspace.display(),
                    "Manifest removed between stat and open (TOCTOU) — evicting cache entry"
                );
                let mut cache = ENGINE_CACHE.lock();
                if let Some(lru) = cache.as_mut() {
                    lru.pop(workspace);
                }
                return Ok(None);
            }
            return Err(e);
        }
    };

    // Phase 4: TOCTOU guard - re-lock and verify no concurrent update
    let mut cache = ENGINE_CACHE.lock();
    let Some(lru) = cache.as_mut() else {
        return Ok(None);
    };

    // Re-check: another thread may have updated cache while we reloaded
    if let Some(current) = lru.get(workspace) {
        if current.identity != cached_identity {
            // Another thread updated with different identity - use their version
            tracing::debug!(
                workspace = %workspace.display(),
                "Another thread updated cache, using their engine"
            );
            return Ok(Some(Arc::clone(&current.engine)));
        }
    } else {
        // Entry evicted between unlock and re-lock
        tracing::debug!(
            workspace = %workspace.display(),
            "Cache entry evicted during reload"
        );
        return Ok(None);
    }

    // Cache entry still matches our observed identity
    if new_identity == cached_identity {
        // Graph unchanged, update metadata only (mtime/size/inode rotated)
        tracing::debug!(
            workspace = %workspace.display(),
            "GraphIdentity unchanged, updating metadata only"
        );
        if let Some(cached) = lru.get_mut(workspace) {
            cached.metadata = new_metadata;
        }
        Ok(Some(cached_engine))
    } else {
        // GraphIdentity changed - invalidate cache entry
        tracing::info!(
            workspace = %workspace.display(),
            old_sha = %cached_identity.snapshot_sha256,
            new_sha = %new_identity.snapshot_sha256,
            "GraphIdentity changed, invalidating cache"
        );
        lru.pop(workspace);
        Ok(None)
    }
}

/// Check if cached manifest metadata is still fresh.
///
/// Performs a stat syscall to compare current filesystem metadata
/// (mtime, size, `file_id`) against cached values.
///
/// # Returns
///
/// - `Ok(true)` if manifest unchanged (all metadata matches)
/// - `Ok(false)` if manifest changed (any metadata differs), or if the manifest file
///   does not exist (e.g. transiently absent during a rebuild window)
/// - `Err(_)` if stat syscall fails for a reason other than `NotFound`
fn is_manifest_fresh(cached: &ManifestMetadata, workspace: &Path) -> Result<bool> {
    let manifest_path = workspace.join(".sqry/graph/manifest.json");
    let current = match std::fs::metadata(&manifest_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                workspace = %workspace.display(),
                "Manifest missing during freshness check — treating as stale"
            );
            return Ok(false);
        }
        Err(e) => {
            return Err(e).context("Failed to stat manifest.json for freshness check");
        }
    };

    Ok(current.modified()? == cached.mtime
        && current.len() == cached.size
        && extract_file_id(&current) == cached.file_id)
}

/// Get `GraphIdentity` for a workspace.
///
/// First checks the engine cache for a cached identity. If not cached or stale,
/// reads the identity from the manifest file.
///
/// This is used by query cache keys to include full workspace identity for
/// cache isolation.
///
/// # Errors
///
/// Returns an error if:
/// - Cache not initialized
/// - Manifest is missing or corrupt
/// - `GraphIdentity` validation fails
pub fn get_graph_identity(workspace: &Path) -> Result<GraphIdentity> {
    // Try to get from cache first
    {
        let cache = ENGINE_CACHE.lock();
        let lru = cache
            .as_ref()
            .context("Engine cache not initialized - call init_engine_cache() first")?;
        if let Some(cached) = lru.peek(workspace) {
            // Verify cache is still fresh before returning identity
            if is_manifest_fresh(&cached.metadata, workspace).unwrap_or(false) {
                tracing::debug!(
                    workspace = %workspace.display(),
                    "Returning cached GraphIdentity"
                );
                return Ok(cached.identity.clone());
            }
        }
    }

    // Cache miss or stale - read from manifest
    tracing::debug!(
        workspace = %workspace.display(),
        "Reading GraphIdentity from manifest"
    );
    read_graph_identity(workspace)
}

#[cfg(test)]
mod engine_cache_tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Reset the engine cache to uninitialized state for test isolation.
    fn reset_engine_cache() {
        let mut cache = ENGINE_CACHE.lock();
        *cache = None;
    }

    /// Helper to create a minimal test workspace with manifest
    fn create_test_workspace() -> Result<TempDir> {
        let temp_dir = TempDir::new()?;
        let graph_dir = temp_dir.path().join(".sqry/graph");
        std::fs::create_dir_all(&graph_dir)?;

        // Create a complete manifest matching the Manifest struct requirements
        let manifest = serde_json::json!({
            "schema_version": 1,
            "snapshot_format_version": 1,
            "built_at": "2026-01-01T00:00:00Z",
            "root_path": temp_dir.path().to_string_lossy(),
            "node_count": 0,
            "edge_count": 0,
            "snapshot_sha256": "aaaa",
            "build_provenance": {
                "sqry_version": "4.10.0",
                "build_timestamp": "2026-01-01T00:00:00Z",
                "build_command": "test"
            }
        });

        let manifest_path = graph_dir.join("manifest.json");
        let mut file = std::fs::File::create(&manifest_path)?;
        file.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
        file.sync_all()?;

        Ok(temp_dir)
    }

    #[test]
    fn test_manifest_freshness_detection() -> Result<()> {
        let workspace = create_test_workspace()?;
        let workspace_path = workspace.path();

        // Read initial metadata
        let metadata1 = read_manifest_metadata(workspace_path)?;

        // Check freshness (should be true initially)
        assert!(is_manifest_fresh(&metadata1, workspace_path)?);

        // Modify manifest (change content)
        let manifest_path = workspace_path.join(".sqry/graph/manifest.json");
        std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure mtime changes
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&manifest_path)?;
        let new_manifest = serde_json::json!({
            "schema_version": 1,
            "snapshot_format_version": 1,
            "built_at": "2026-01-02T00:00:00Z",
            "root_path": workspace_path.to_string_lossy(),
            "node_count": 0,
            "edge_count": 0,
            "snapshot_sha256": "bbbb",
            "build_provenance": {
                "sqry_version": "4.10.0",
                "build_timestamp": "2026-01-02T00:00:00Z",
                "build_command": "test"
            }
        });
        file.write_all(serde_json::to_string_pretty(&new_manifest)?.as_bytes())?;
        file.sync_all()?;

        // Check freshness (should be false now)
        assert!(!is_manifest_fresh(&metadata1, workspace_path)?);

        Ok(())
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_cache_requires_initialization() {
        reset_engine_cache();

        // Attempt to get cached engine before initialization
        let temp_dir = TempDir::new().unwrap();
        let result = get_cached_engine(temp_dir.path());

        // Should fail with "not initialized" error
        match result {
            Err(e) => assert!(e.to_string().contains("not initialized")),
            Ok(_) => panic!("Expected error, got success"),
        }
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_cache_miss_returns_none() -> Result<()> {
        // Initialize cache for this test
        init_engine_cache(std::num::NonZeroUsize::new(5).unwrap());

        let temp_dir = TempDir::new()?;
        let workspace_path = temp_dir.path();

        // Cache miss should return None
        let result = get_cached_engine(workspace_path)?;
        assert!(result.is_none());

        Ok(())
    }

    // ===== normalize_path tests =====

    #[test]
    fn test_normalize_path_resolves_parent_dir() {
        let path = std::path::Path::new("/workspace/src/../lib");
        let normalized = normalize_path(path);
        assert_eq!(normalized, std::path::PathBuf::from("/workspace/lib"));
    }

    #[test]
    fn test_normalize_path_resolves_current_dir() {
        let path = std::path::Path::new("/workspace/./src");
        let normalized = normalize_path(path);
        assert_eq!(normalized, std::path::PathBuf::from("/workspace/src"));
    }

    #[test]
    fn test_normalize_path_handles_multiple_traversals() {
        let path = std::path::Path::new("/a/b/c/../../d");
        let normalized = normalize_path(path);
        assert_eq!(normalized, std::path::PathBuf::from("/a/d"));
    }

    #[test]
    fn test_normalize_path_at_root_ignores_parent() {
        let path = std::path::Path::new("/workspace");
        let normalized = normalize_path(path);
        assert_eq!(normalized, std::path::PathBuf::from("/workspace"));
    }

    #[test]
    fn test_normalize_path_simple_path_unchanged() {
        let path = std::path::Path::new("/workspace/src/lib.rs");
        let normalized = normalize_path(path);
        assert_eq!(
            normalized,
            std::path::PathBuf::from("/workspace/src/lib.rs")
        );
    }

    // ===== canonicalize_in_workspace tests =====

    #[test]
    fn test_canonicalize_in_workspace_dot_returns_workspace_root() -> Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path();
        let result = canonicalize_in_workspace(".", workspace)?;
        // "." relative to workspace => workspace itself
        assert_eq!(result, workspace.canonicalize()?);
        Ok(())
    }

    #[test]
    fn test_canonicalize_in_workspace_outside_path_rejected() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        // Path traversal attempt
        let result = canonicalize_in_workspace("../../etc/passwd", workspace);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("outside") || err.contains("Failed to canonicalize"));
    }

    #[test]
    fn test_canonicalize_in_workspace_absolute_outside_rejected() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        // Absolute path outside workspace
        let result = canonicalize_in_workspace("/etc/passwd", workspace);
        assert!(result.is_err());
    }

    #[test]
    fn test_canonicalize_in_workspace_valid_subdir() -> Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path();
        let subdir = workspace.join("src");
        std::fs::create_dir(&subdir)?;

        let result = canonicalize_in_workspace("src", workspace)?;
        assert_eq!(result, subdir.canonicalize()?);
        Ok(())
    }

    // ===== read_graph_identity tests =====

    #[test]
    fn test_read_graph_identity_valid_manifest() -> Result<()> {
        let workspace = create_test_workspace()?;
        let workspace_path = workspace.path();

        let identity = read_graph_identity(workspace_path)?;
        assert!(!identity.snapshot_sha256.is_empty());
        assert_eq!(identity.schema_version, 1);
        assert_eq!(identity.snapshot_format_version, 1);
        Ok(())
    }

    #[test]
    fn test_read_graph_identity_missing_manifest_errors() {
        let temp = TempDir::new().unwrap();
        let result = read_graph_identity(temp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Manifest missing") || msg.contains("run `sqry index`"));
    }

    #[test]
    fn test_read_graph_identity_with_metadata_returns_both() -> Result<()> {
        let workspace = create_test_workspace()?;
        let workspace_path = workspace.path();

        let (identity, metadata) = read_graph_identity_with_metadata(workspace_path)?;
        assert!(!identity.snapshot_sha256.is_empty());
        assert!(metadata.size > 0);
        Ok(())
    }

    // ===== init_engine_cache tests =====

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_init_engine_cache_idempotent() {
        reset_engine_cache();
        let cap = std::num::NonZeroUsize::new(4).unwrap();
        init_engine_cache(cap);
        // Second call is a no-op (idempotent)
        init_engine_cache(cap);
        // Should still work
        let temp = TempDir::new().unwrap();
        let result = get_cached_engine(temp.path());
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // cache miss
    }

    // ===== is_auto_index_enabled tests =====
    // Serial attribute prevents env-var races when Rust runs tests in parallel.

    #[test]
    #[serial_test::serial(sqry_auto_index_env)]
    fn test_is_auto_index_enabled_defaults_true() {
        // Without SQRY_AUTO_INDEX set, should be true.
        // SAFETY: Test environment variable manipulation, guarded by serial attribute.
        unsafe { std::env::remove_var("SQRY_AUTO_INDEX") };
        assert!(is_auto_index_enabled());
    }

    #[test]
    #[serial_test::serial(sqry_auto_index_env)]
    fn test_is_auto_index_enabled_false_when_set_to_false() {
        // SAFETY: Test environment variable manipulation, guarded by serial attribute.
        unsafe { std::env::set_var("SQRY_AUTO_INDEX", "false") };
        assert!(!is_auto_index_enabled());
        unsafe { std::env::remove_var("SQRY_AUTO_INDEX") };
    }

    #[test]
    #[serial_test::serial(sqry_auto_index_env)]
    fn test_is_auto_index_enabled_false_when_set_to_zero() {
        // SAFETY: Test environment variable manipulation, guarded by serial attribute.
        unsafe { std::env::set_var("SQRY_AUTO_INDEX", "0") };
        assert!(!is_auto_index_enabled());
        unsafe { std::env::remove_var("SQRY_AUTO_INDEX") };
    }

    #[test]
    #[serial_test::serial(sqry_auto_index_env)]
    fn test_is_auto_index_enabled_true_when_set_to_true() {
        // SAFETY: Test environment variable manipulation, guarded by serial attribute.
        unsafe { std::env::set_var("SQRY_AUTO_INDEX", "true") };
        assert!(is_auto_index_enabled());
        unsafe { std::env::remove_var("SQRY_AUTO_INDEX") };
    }

    // ===== Engine::for_workspace tests =====

    #[test]
    fn test_engine_for_workspace_sets_root() -> Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path().to_path_buf();
        let engine = Engine::for_workspace(workspace.clone())?;
        assert_eq!(engine.workspace_root(), workspace.as_path());
        Ok(())
    }

    #[test]
    fn test_engine_graph_returns_none_without_snapshot() -> Result<()> {
        let temp = TempDir::new()?;
        let engine = Engine::for_workspace(temp.path().to_path_buf())?;
        // No snapshot file exists => graph() returns None
        let graph = engine.graph();
        assert!(graph.is_none());
        Ok(())
    }

    #[test]
    fn test_engine_graph_returns_none_when_manifest_missing() -> Result<()> {
        // Manifest absent (e.g. during a rebuild window) must not panic or error —
        // graph() delegates to GraphStorage::exists() which already handles NotFound
        // by returning false, so this test validates that contract end-to-end.
        let temp = TempDir::new()?;
        let engine = Engine::for_workspace(temp.path().to_path_buf())?;
        // No .sqry/graph directory or manifest at all
        let graph = engine.graph();
        assert!(
            graph.is_none(),
            "engine.graph() must return None when manifest is absent"
        );
        Ok(())
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_get_cached_engine_returns_none_when_manifest_removed() -> Result<()> {
        // Scenario: a workspace is cached (engine + identity seeded by a valid
        // manifest), then the manifest is removed mid-flight (rebuild window).
        // get_cached_engine() must return Ok(None) — not Err — and evict the stale
        // cache entry so the next call triggers a fresh load/rebuild.
        reset_engine_cache();
        let cap = std::num::NonZeroUsize::new(4).unwrap();
        init_engine_cache(cap);

        // Seed the cache with a valid workspace that has a manifest.
        let workspace = create_test_workspace()?;
        let workspace_path = workspace.path().canonicalize()?;
        let manifest_path = workspace_path.join(".sqry/graph/manifest.json");

        // Warm the cache by reading identity + inserting manually.
        let (identity, metadata) = read_graph_identity_with_metadata(&workspace_path)?;
        {
            let mut cache = ENGINE_CACHE.lock();
            let lru = cache.as_mut().expect("cache initialized");
            lru.put(
                workspace_path.clone(),
                CachedEngine {
                    engine: Arc::new(Engine::for_workspace(workspace_path.clone())?),
                    identity,
                    metadata,
                },
            );
        }

        // Verify cache is hot.
        assert!(
            get_cached_engine(&workspace_path)?.is_some(),
            "cache should be hot before manifest removal"
        );

        // Remove the manifest — simulates the rebuild window.
        std::fs::remove_file(&manifest_path)?;

        // get_cached_engine must return Ok(None), not Err.
        let result = get_cached_engine(&workspace_path);
        assert!(
            result.is_ok(),
            "get_cached_engine must return Ok when manifest is absent, got Err"
        );
        assert!(
            result.unwrap().is_none(),
            "get_cached_engine must return None (stale) when manifest is absent"
        );

        // Cache entry must have been evicted.
        let result2 = get_cached_engine(&workspace_path);
        assert!(result2.is_ok());
        assert!(
            result2.unwrap().is_none(),
            "cache entry must be evicted after manifest-absent eviction"
        );

        Ok(())
    }

    // ===== GraphIdentity equality tests =====

    #[test]
    fn test_graph_identity_equality() -> Result<()> {
        let workspace = create_test_workspace()?;
        let workspace_path = workspace.path();

        let id1 = read_graph_identity(workspace_path)?;
        let id2 = read_graph_identity(workspace_path)?;
        assert_eq!(id1, id2);
        Ok(())
    }

    // ===== ManifestMetadata read_manifest_metadata tests =====

    #[test]
    fn test_read_manifest_metadata_valid() -> Result<()> {
        let workspace = create_test_workspace()?;
        let metadata = read_manifest_metadata(workspace.path())?;
        assert!(metadata.size > 0);
        Ok(())
    }

    #[test]
    fn test_read_manifest_metadata_missing_errors() {
        let temp = TempDir::new().unwrap();
        let result = read_manifest_metadata(temp.path());
        assert!(result.is_err());
    }

    // ===== get_graph_identity tests =====

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_get_graph_identity_not_initialized_errors() {
        reset_engine_cache();
        let temp = TempDir::new().unwrap();
        let result = get_graph_identity(temp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not initialized"));
    }

    #[test]
    #[serial_test::serial(engine_cache)]
    fn test_get_graph_identity_falls_back_to_manifest() -> Result<()> {
        init_engine_cache(std::num::NonZeroUsize::new(4).unwrap());
        let workspace = create_test_workspace()?;

        // Cache miss => reads from manifest
        let identity = get_graph_identity(workspace.path())?;
        assert!(!identity.snapshot_sha256.is_empty());
        Ok(())
    }

    // ===== workspace_lock tests =====

    #[test]
    fn test_workspace_lock_returns_same_instance() {
        let lock1 = workspace_lock();
        let lock2 = workspace_lock();
        // Same static reference
        assert!(std::ptr::eq(lock1, lock2));
    }

    // ===== is_manifest_fresh with modified file =====

    #[test]
    fn test_is_manifest_fresh_missing_file_returns_stale() {
        let temp = TempDir::new().unwrap();
        // The manifest file does not exist — should return Ok(false) (stale), not Err.
        let fake_metadata = ManifestMetadata {
            mtime: std::time::SystemTime::now(),
            size: 100,
            file_id: None,
        };
        let result = is_manifest_fresh(&fake_metadata, temp.path());
        assert!(
            result.is_ok(),
            "Missing manifest should return Ok, not Err: {result:?}"
        );
        assert!(
            !result.unwrap(),
            "Missing manifest should be treated as stale (Ok(false))"
        );
    }

    #[test]
    fn test_engine_graph_rejects_corrupted_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let graph_dir = temp.path().join(".sqry/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();

        // Write a snapshot file
        let snapshot_data = b"fake snapshot data";
        std::fs::write(graph_dir.join("snapshot.sqry"), snapshot_data).unwrap();

        // Write a manifest with a WRONG sha256
        let manifest = serde_json::json!({
            "root_path": temp.path().to_string_lossy(),
            "node_count": 0,
            "edge_count": 0,
            "snapshot_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
            "built_at": "2026-01-01T00:00:00+00:00",
            "schema_version": 5,
            "snapshot_format_version": 5,
            "build_provenance": { "sqry_version": "test", "rustc_version": "test" }
        });
        std::fs::write(
            graph_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Create an Engine and try to load graph — should return None
        let engine =
            Engine::for_workspace(temp.path().to_path_buf()).expect("engine should create");
        assert!(
            engine.graph().is_none(),
            "Engine::graph() should return None when snapshot hash is wrong"
        );
    }

    #[test]
    fn test_engine_graph_rejects_corrupt_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let graph_dir = temp.path().join(".sqry/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();

        // Write a valid snapshot file
        std::fs::write(graph_dir.join("snapshot.sqry"), b"some data").unwrap();

        // Write INVALID JSON to manifest — must fail closed, not skip verification
        std::fs::write(graph_dir.join("manifest.json"), b"not valid json!!!").unwrap();

        let engine =
            Engine::for_workspace(temp.path().to_path_buf()).expect("engine should create");
        assert!(
            engine.graph().is_none(),
            "Engine::graph() must return None when manifest is corrupt (fail closed)"
        );
    }

    #[test]
    fn test_engine_graph_accepts_empty_hash() {
        let temp = tempfile::tempdir().unwrap();
        let graph_dir = temp.path().join(".sqry/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();

        // Write a snapshot with garbage data (will fail deserialization, not integrity)
        std::fs::write(graph_dir.join("snapshot.sqry"), b"not a real snapshot").unwrap();

        // Write manifest with empty sha256 (pre-hash index)
        let manifest = serde_json::json!({
            "root_path": temp.path().to_string_lossy(),
            "node_count": 0,
            "edge_count": 0,
            "snapshot_sha256": "",
            "built_at": "2026-01-01T00:00:00+00:00",
            "schema_version": 5,
            "snapshot_format_version": 5,
            "build_provenance": { "sqry_version": "test", "rustc_version": "test" }
        });
        std::fs::write(
            graph_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Should skip integrity check (empty hash), then fail on deserialization
        // — returning None, not panicking
        let engine =
            Engine::for_workspace(temp.path().to_path_buf()).expect("engine should create");
        assert!(
            engine.graph().is_none(),
            "Should return None (deserialization fails on garbage data, but integrity check skipped)"
        );
    }
}
