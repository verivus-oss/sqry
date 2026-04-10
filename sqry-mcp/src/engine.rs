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
use std::time::SystemTime;

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
    executor: QueryExecutor,
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
        let executor = QueryExecutor::with_plugin_manager(plugin_manager);
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
        let executor = QueryExecutor::with_plugin_manager(plugin_manager);
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

/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn canonicalize_in_workspace(path_str: &str, workspace_root: &Path) -> Result<PathBuf> {
    let input_path = Path::new(path_str);
    let joined = if input_path.is_absolute() {
        input_path.to_path_buf()
    } else {
        workspace_root.join(input_path)
    };

    // Normalize the path to resolve ".." and "." components without requiring file existence
    // This is critical for security - we must detect directory traversal even if path doesn't exist
    let normalized = normalize_path(&joined);

    // Security check: Ensure the normalized path is within workspace root
    // This check must happen BEFORE canonicalization to catch malicious paths
    if !normalized.starts_with(workspace_root) {
        bail!(
            "Path '{}' is outside of the workspace root '{}'",
            normalized.display(),
            workspace_root.display()
        );
    }

    // Now attempt to canonicalize the actual path (follows symlinks, verifies existence)
    // If this fails, we return the failure, but we've already prevented directory traversal
    let canon = std::fs::canonicalize(&joined).map_err(|e| {
        anyhow::anyhow!("Failed to canonicalize path: {} ({})", joined.display(), e)
    })?;

    // Double-check after canonicalization (symlinks could still escape).
    // Canonicalize the workspace root too — on macOS, /var is a symlink to
    // /private/var, so the canonical path and raw workspace_root may diverge.
    let canon_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    if !canon.starts_with(&canon_root) {
        bail!(
            "Path '{}' is outside of the workspace root '{}'",
            canon.display(),
            canon_root.display()
        );
    }
    Ok(canon)
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

    // Phase 3: Cold path - manifest changed, reload identity and metadata atomically
    tracing::debug!(
        workspace = %workspace.display(),
        "Manifest changed, reloading identity"
    );

    let (new_identity, new_metadata) = read_graph_identity_with_metadata(workspace)?;

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
/// - `Ok(false)` if manifest changed (any metadata differs)
/// - `Err(_)` if stat syscall fails
fn is_manifest_fresh(cached: &ManifestMetadata, workspace: &Path) -> Result<bool> {
    let manifest_path = workspace.join(".sqry/graph/manifest.json");
    let current = std::fs::metadata(&manifest_path)
        .context("Failed to stat manifest.json for freshness check")?;

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
    fn test_is_manifest_fresh_missing_file_errors() {
        let temp = TempDir::new().unwrap();
        // Create a fake metadata by using a file that exists
        let _manifest_path = temp.path().join(".sqry/graph/manifest.json");
        // The file doesn't exist — stat should fail
        let fake_metadata = ManifestMetadata {
            mtime: std::time::SystemTime::now(),
            size: 100,
            file_id: None,
        };
        let result = is_manifest_fresh(&fake_metadata, temp.path());
        assert!(result.is_err());
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
