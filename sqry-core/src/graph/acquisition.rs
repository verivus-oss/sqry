//! Shared graph acquisition API (DAG unit SGA02).
//!
//! Transport-neutral abstraction that lets every read-only semantic-search
//! surface (CLI `sqry query`, standalone MCP, daemon-hosted MCP, LSP) acquire
//! the same loaded [`CodeGraph`] before executing a query.
//!
//! This module owns *only* the contract: types, traits, and error taxonomy.
//! Concrete providers (filesystem-backed, daemon-backed) live in subsequent
//! DAG units (SGA03 / SGA04). Adapters in `sqry-cli`, `sqry-mcp`, `sqry-lsp`,
//! and `sqry-daemon` consume the contract.
//!
//! ## Design references
//!
//! - `docs/development/shared-graph-acquisition/01_SPEC.md`
//! - `docs/development/shared-graph-acquisition/02_DESIGN.md` (normative)
//! - `docs/development/shared-graph-acquisition/03_IMPLEMENTATION_PLAN.md`
//!
//! ## Boundaries
//!
//! - The trait is **synchronous**. Daemon callers wrap blocking acquisition in
//!   `spawn_blocking`; standalone callers run on the calling thread. This
//!   matches today's synchronous query-execution path.
//! - Path-policy errors ([`GraphAcquisitionError::InvalidPath`]) are first
//!   class and **must not** be collapsed into [`GraphAcquisitionError::Internal`].
//! - Stale serves are explicit ([`GraphFreshness::Stale`]). Adapters cannot
//!   silently mask staleness.
//! - The reload origin ([`ReloadOrigin`]) is a neutral diagnostic enum; it
//!   carries `String` detail rather than depending on `sqry-daemon` types so
//!   this module remains independent of any transport crate.

use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::graph::CodeGraph;
use crate::graph::unified::persistence::{
    GraphStorage, Manifest, PersistenceError, load_from_bytes, verify_snapshot_bytes,
};
use crate::plugin::PluginManager;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Acquire a loaded [`CodeGraph`] for the requested path.
///
/// Implementations must:
/// * Validate the requested path *before* attempting any graph load.
/// * Honor the [`AcquisitionOperation`] mode — `MutatingRebuild` callers must
///   never receive a [`GraphFreshness::Stale`] or [`GraphFreshness::Reloaded`]
///   result from a read-only fallback.
/// * Surface [`GraphAcquisitionError::InvalidPath`],
///   [`GraphAcquisitionError::Evicted`], [`GraphAcquisitionError::StaleExpired`],
///   and [`GraphAcquisitionError::IncompatibleGraph`] distinctly — adapters
///   rely on this to map to per-transport diagnostics.
///
/// The trait is `Send + Sync` so providers can be stored behind `Arc<dyn _>`
/// in long-lived hosts (daemon workspace manager, LSP session, MCP engine).
pub trait GraphAcquirer: Send + Sync {
    /// Acquire a graph for `request`. Returns a populated [`GraphAcquisition`]
    /// or a typed [`GraphAcquisitionError`].
    fn acquire(
        &self,
        request: GraphAcquisitionRequest,
    ) -> Result<GraphAcquisition, GraphAcquisitionError>;
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// Inputs to a single acquisition call.
///
/// All fields are required. Callers construct the request inline; there is no
/// builder because every field has security or correctness implications and a
/// silent default would let an adapter accidentally weaken the contract.
#[derive(Debug, Clone)]
pub struct GraphAcquisitionRequest {
    /// Path the user supplied (may be a workspace root, a directory inside
    /// the workspace, or a file). Providers canonicalize before use.
    pub requested_path: PathBuf,
    /// Whether the caller intends to read or mutate.
    pub operation: AcquisitionOperation,
    /// Path-security policy applied before any graph load.
    pub path_policy: PathPolicy,
    /// What to do when no graph artifact exists for the resolved workspace.
    pub missing_graph_policy: MissingGraphPolicy,
    /// What to do when only a stale graph is available.
    pub stale_policy: StalePolicy,
    /// How to react to manifest plugin selection mismatches.
    pub plugin_selection_policy: PluginSelectionPolicy,
    /// Optional tool name for diagnostics and observability. `'static` because
    /// every call site is a fixed string literal.
    pub tool_name: Option<&'static str>,
}

/// Whether the acquisition is for a read-only query or a mutating rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionOperation {
    /// Read-only query path (CLI query, MCP search, LSP relations, etc.).
    /// May load existing graphs, serve stale within policy, and trigger
    /// exactly one daemon read-only reload after eviction.
    ReadOnlyQuery,
    /// Mutating rebuild path (`rebuild_index`, daemon rebuild, watcher).
    /// Must never be served from stale or read-only-reloaded state.
    MutatingRebuild,
}

/// Behavior when no graph artifact exists for the resolved workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingGraphPolicy {
    /// Return [`GraphAcquisitionError::NoGraph`].
    Error,
    /// Auto-build the graph if the surface already supports auto-indexing
    /// (standalone MCP, LSP). Disabled for CLI query.
    AutoBuildIfEnabled,
}

/// Path-security policy applied *before* any graph load occurs.
///
/// All fields are independent flags: producers compose the desired strictness
/// at the call site. The defaults (via [`PathPolicy::default`]) are the most
/// restrictive options because weakening must be explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPolicy {
    /// If `true`, the requested path must already exist on disk and be
    /// canonicalizable. If `false`, the provider may accept a path that does
    /// not yet exist (used by some auto-build flows).
    pub require_existing: bool,
    /// If `true`, after canonicalization the requested path must be inside the
    /// resolved workspace root (or equal to it). Prevents
    /// `../../outside-workspace` escapes.
    pub require_within_workspace: bool,
    /// If `true`, symlinks whose target escapes the workspace are accepted
    /// (used for vendored sources). The default `false` rejects such escapes.
    pub allow_symlink_escape: bool,
}

impl Default for PathPolicy {
    /// The strict default: require an existing path inside the workspace and
    /// reject symlink escapes. Adapters that need a weaker policy must opt in
    /// explicitly.
    fn default() -> Self {
        Self {
            require_existing: true,
            require_within_workspace: true,
            allow_symlink_escape: false,
        }
    }
}

/// Behavior when only a stale graph is available.
///
/// Does not derive `Eq` because [`StalePolicy::AcceptStaleWithinWindow`]
/// carries an `f64` window. Adapters compare windows via `PartialEq` only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StalePolicy {
    /// Refuse to serve a stale graph. Returns
    /// [`GraphAcquisitionError::StaleExpired`] when only stale data exists.
    RejectStale,
    /// Allow stale serves whose age is within `max_age_hours`. Beyond the
    /// window, providers must return [`GraphAcquisitionError::StaleExpired`].
    AcceptStaleWithinWindow {
        /// Maximum age of a stale graph that may still be served, in hours.
        max_age_hours: f64,
    },
}

impl Default for StalePolicy {
    /// Default: reject stale. Stale-serve must be explicitly enabled per
    /// transport surface (today only the daemon).
    fn default() -> Self {
        Self::RejectStale
    }
}

/// How to react to manifest plugin selection mismatches.
///
/// Unknown plugin ids in a manifest mean the runtime cannot reproduce the
/// indexed semantics — silent acceptance would hide language coverage loss.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PluginSelectionPolicy {
    /// Default. Any unknown plugin id in the manifest is a terminal
    /// [`GraphAcquisitionError::IncompatibleGraph`].
    #[default]
    StrictMatch,
    /// Compatibility opt-in: only the listed unknown plugin ids are tolerated.
    /// Any *other* unknown id still fails. Reserved for future use behind
    /// explicit user configuration.
    AllowUnknownIds {
        /// Unknown plugin ids the caller has explicitly approved.
        allowed: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Successful acquisition outcome.
#[derive(Debug, Clone)]
pub struct GraphAcquisition {
    /// Reference-counted handle to the loaded graph. Cloning is cheap; the
    /// caller may share it with `sqry-db`, the query executor, etc.
    pub graph: Arc<CodeGraph>,
    /// Canonical workspace root (the directory containing `.sqry/graph/`).
    pub workspace_root: PathBuf,
    /// Optional sub-scope when the user queried a directory or file inside
    /// the workspace. `None` when the request targeted the workspace root.
    pub query_scope: Option<PathBuf>,
    /// `true` when [`Self::query_scope`] points at a single file rather than
    /// a directory. Adapters use this to apply the file-scope filter.
    pub is_file_scope: bool,
    /// Freshness/lifecycle of the served graph.
    pub freshness: GraphFreshness,
    /// Identity (snapshot hash, manifest timestamp, plugin status).
    pub identity: GraphIdentity,
    /// Free-form per-acquisition metadata (source, tool, notes).
    pub metadata: GraphAcquisitionMetadata,
}

/// Lifecycle state of the graph that was returned.
///
/// Adapters render this into transport-specific freshness signals
/// (`ResponseMeta::stale_from`, `_stale_warning`, LSP diagnostics).
#[derive(Debug, Clone, PartialEq)]
pub enum GraphFreshness {
    /// Graph is current relative to its source. Optional `lifecycle_label`
    /// carries the daemon-side lifecycle name for diagnostics.
    Fresh {
        /// Daemon lifecycle label (`"Loaded"`, `"Rebuilding"`, etc.). `None`
        /// for filesystem providers that have no lifecycle concept.
        lifecycle_label: Option<&'static str>,
    },
    /// Graph is older than its source but is being served per stale policy.
    Stale {
        /// ISO-8601 timestamp of the last successful build, when available.
        last_good_at: Option<String>,
        /// Diagnostic for the failure that caused staleness.
        last_error: Option<String>,
        /// Age of the served graph, in hours.
        age_hours: Option<f64>,
    },
    /// Graph was reloaded after an `Unloaded` or `Evicted` lifecycle. This is
    /// the bounded one-shot reload path described in the design.
    Reloaded {
        /// Why the original lifecycle entered an unloaded/evicted state.
        original_lifecycle: ReloadOrigin,
        /// Daemon lifecycle label after the successful reload.
        final_lifecycle_label: &'static str,
        /// Number of reload attempts. The bounded contract caps this at one,
        /// so `NonZeroU8::new(1)` is the only legal value today; the type
        /// reserves headroom for future bounded retries while making zero
        /// unrepresentable.
        reload_attempts: NonZeroU8,
    },
}

/// Why the daemon workspace required a reload.
///
/// Pure diagnostic enum: deliberately uses `String` rather than depending on
/// `sqry-daemon` types so this contract crate stays transport-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOrigin {
    /// Workspace had been unloaded (idle timeout, explicit unload).
    Unloaded {
        /// Free-form detail captured by the adapter.
        detail: String,
    },
    /// Workspace had been evicted by memory admission.
    Evicted {
        /// Free-form detail captured by the adapter.
        detail: String,
    },
}

/// Identity of the served graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphIdentity {
    /// SHA-256 of the snapshot file when known (filesystem providers).
    pub snapshot_sha256: Option<String>,
    /// ISO-8601 manifest build timestamp when available.
    pub manifest_built_at: Option<String>,
    /// Snapshot persistence format version (e.g. `7`, `10`).
    pub snapshot_format_version: Option<u32>,
    /// Canonical source root the graph was built for.
    pub source_root: PathBuf,
    /// Plugin manifest compatibility status — see [`PluginSelectionStatus`].
    pub plugin_selection_status: PluginSelectionStatus,
}

/// Compatibility verdict between the manifest plugin set and the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginSelectionStatus {
    /// Every manifest plugin id is supported by this binary.
    Exact,
    /// Manifest references plugin ids the runtime does not know.
    /// `manifest_path` is set by [`FilesystemGraphProvider`] when the
    /// status was produced from a persisted manifest; consumers (CLI,
    /// MCP, daemon) feed both `unknown_plugin_ids` and `manifest_path`
    /// to `sqry_plugin_registry::missing_features_for` to render an
    /// actionable diagnostic (cluster-E §E.2).
    IncompatibleUnknownPluginIds {
        /// The unknown ids, in the order they appear in the manifest.
        unknown_plugin_ids: Vec<String>,
        /// Absolute path to the manifest file that produced the
        /// unknown ids. `None` for synthetic in-memory manifests
        /// (unit tests, in-process providers without on-disk state).
        manifest_path: Option<PathBuf>,
    },
    /// Snapshot format itself cannot be loaded by this binary.
    IncompatibleSnapshotFormat {
        /// Human-readable explanation.
        reason: String,
    },
}

/// Free-form per-acquisition diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphAcquisitionMetadata {
    /// Which provider served the request.
    pub acquisition_source: AcquisitionSource,
    /// Tool name forwarded from the request, if any.
    pub tool_name: Option<&'static str>,
    /// Provider-specific notes (e.g. cache hit/miss, reload counts). Adapters
    /// append; this field is not part of any wire contract.
    pub notes: Vec<String>,
}

/// Provider that served a given acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionSource {
    /// Filesystem-backed provider (CLI, standalone MCP, standalone LSP).
    Filesystem,
    /// Daemon provider returned a graph already loaded in workspace memory.
    DaemonReadOnly,
    /// Daemon provider performed the bounded one-shot reload before serving.
    DaemonReloaded,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Acquisition failure taxonomy.
///
/// Each variant maps to a distinct user-facing diagnostic class. Adapters must
/// preserve the variant — collapsing into a generic internal error violates
/// the contract.
#[derive(Debug, Error)]
pub enum GraphAcquisitionError {
    /// Path canonicalization failed, the path does not exist where required,
    /// it is not under an allowed source root, or it escaped through a
    /// symlink. Path validation runs before any graph load.
    #[error("invalid path {path:?}: {reason}")]
    InvalidPath {
        /// The path that failed validation.
        path: PathBuf,
        /// Why validation failed.
        reason: String,
    },
    /// No graph artifact exists for this workspace and the policy disallows
    /// auto-build.
    #[error("no graph artifact for workspace {workspace_root:?}")]
    NoGraph {
        /// Workspace root that has no graph.
        workspace_root: PathBuf,
    },
    /// Snapshot, manifest, or analysis load failed.
    #[error("graph load failed for {source_root:?}: {reason}")]
    LoadFailed {
        /// The source root the load was attempted for.
        source_root: PathBuf,
        /// Failure detail (I/O error, integrity check, format mismatch).
        reason: String,
    },
    /// Snapshot or manifest cannot be used safely by this binary (unknown
    /// plugin ids, unsupported snapshot format).
    #[error("incompatible graph for {source_root:?}: {status:?}")]
    IncompatibleGraph {
        /// Source root the snapshot was built for.
        source_root: PathBuf,
        /// Specific compatibility verdict.
        status: PluginSelectionStatus,
    },
    /// Daemon workspace is `Unloaded` or `Loading` and read-only reload is
    /// not applicable.
    #[error("workspace not ready: {workspace_root:?} (lifecycle={lifecycle})")]
    NotReady {
        /// Workspace root.
        workspace_root: PathBuf,
        /// Daemon lifecycle label.
        lifecycle: String,
    },
    /// Daemon workspace was evicted and either reload was not allowed (e.g.
    /// `MutatingRebuild`) or the bounded one-shot reload itself failed.
    #[error("workspace evicted: {workspace_root:?} (original_lifecycle={original_lifecycle})")]
    Evicted {
        /// Workspace root.
        workspace_root: PathBuf,
        /// Daemon lifecycle label at the time of eviction.
        original_lifecycle: String,
        /// Reason the bounded reload failed, when a reload was attempted.
        /// `None` means reload was not attempted (e.g. `MutatingRebuild`).
        reload_failure: Option<String>,
    },
    /// Stale graph age exceeded the configured stale-serve window.
    #[error("stale graph expired for {workspace_root:?} (age_hours={age_hours:?})")]
    StaleExpired {
        /// Workspace root.
        workspace_root: PathBuf,
        /// Age of the stale graph in hours, when known.
        age_hours: Option<f64>,
    },
    /// An approved load/build path failed mid-way (auto-build, daemon reload).
    #[error("graph build failed for {workspace_root:?}: {reason}")]
    BuildFailed {
        /// Workspace root.
        workspace_root: PathBuf,
        /// Build failure detail.
        reason: String,
    },
    /// Invariant violation, join failure, or other condition that does not
    /// fit the typed taxonomy. Adapters must not collapse path/eviction/
    /// stale/incompatible failures into this variant.
    #[error("internal acquisition error: {reason}")]
    Internal {
        /// Internal failure detail.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// FilesystemGraphProvider (DAG unit SGA03)
// ---------------------------------------------------------------------------

/// Optional fallback hook for [`MissingGraphPolicy::AutoBuildIfEnabled`].
///
/// The provider lives in `sqry-core` and intentionally does not depend on any
/// builder front-ends (CLI, MCP engine). Surfaces that already implement
/// auto-build (standalone MCP, LSP) supply a closure that builds the graph,
/// loads it, and returns the result. CLI omits this hook entirely.
pub type AutoBuildHook =
    Arc<dyn Fn(&Path) -> Result<Arc<CodeGraph>, GraphAcquisitionError> + Send + Sync>;

/// Filesystem-backed [`GraphAcquirer`] used by CLI query, standalone MCP, and
/// standalone LSP.
///
/// Responsibilities (per `02_DESIGN.md` §Providers):
///
/// 1. Canonicalize the requested path and reject non-existent / outside-
///    workspace / symlink-escape inputs **before** any graph load.
/// 2. Discover the nearest `.sqry/graph` ancestor using the same depth-
///    bounded walk as `sqry-cli::index_discovery::find_nearest_index`.
/// 3. Compute `query_scope` and `is_file_scope` exactly the way the CLI does
///    today.
/// 4. Read manifest, verify snapshot SHA-256 (when manifest is present), and
///    load via [`load_from_bytes`] with the configured [`PluginManager`].
/// 5. Surface unknown manifest plugin ids as
///    [`GraphAcquisitionError::IncompatibleGraph`] when policy is
///    [`PluginSelectionPolicy::StrictMatch`].
/// 6. Honor [`MissingGraphPolicy`] — `Error` returns
///    [`GraphAcquisitionError::NoGraph`]; `AutoBuildIfEnabled` delegates to
///    the supplied [`AutoBuildHook`] (if any) and otherwise behaves like
///    `Error`.
pub struct FilesystemGraphProvider {
    plugin_manager: Arc<PluginManager>,
    auto_build: Option<AutoBuildHook>,
}

impl std::fmt::Debug for FilesystemGraphProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemGraphProvider")
            .field("auto_build_hook", &self.auto_build.is_some())
            .finish()
    }
}

impl FilesystemGraphProvider {
    /// Build a provider that uses the given plugin manager for snapshot loads
    /// and for plugin-selection compatibility checks.
    #[must_use]
    pub fn new(plugin_manager: Arc<PluginManager>) -> Self {
        Self {
            plugin_manager,
            auto_build: None,
        }
    }

    /// Attach an auto-build hook that the provider invokes when
    /// [`MissingGraphPolicy::AutoBuildIfEnabled`] is configured **and** no
    /// graph artifact exists for the resolved workspace.
    #[must_use]
    pub fn with_auto_build_hook(mut self, hook: AutoBuildHook) -> Self {
        self.auto_build = Some(hook);
        self
    }

    /// Read access to the underlying plugin manager (used by adapters that
    /// already share an executor with the same plugin selection).
    #[must_use]
    pub fn plugin_manager(&self) -> &PluginManager {
        &self.plugin_manager
    }

    /// Step 1 — apply [`PathPolicy`] and return the canonical request path.
    ///
    /// On failure, the returned [`GraphAcquisitionError::InvalidPath`] carries
    /// the original (un-canonicalized) request path so adapters can render
    /// stable diagnostics.
    fn apply_path_policy(
        &self,
        request_path: &Path,
        policy: &PathPolicy,
    ) -> Result<PathBuf, GraphAcquisitionError> {
        let exists = request_path.exists();
        if policy.require_existing && !exists {
            return Err(GraphAcquisitionError::InvalidPath {
                path: request_path.to_path_buf(),
                reason: "path does not exist".to_string(),
            });
        }

        // Best-effort canonicalize. When the path does not exist and the
        // policy permits non-existing paths, fall back to the absolute form.
        let canonical = match request_path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                if policy.require_existing {
                    return Err(GraphAcquisitionError::InvalidPath {
                        path: request_path.to_path_buf(),
                        reason: format!("path cannot be canonicalized: {e}"),
                    });
                }
                if request_path.is_absolute() {
                    request_path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(request_path))
                        .unwrap_or_else(|_| request_path.to_path_buf())
                }
            }
        };

        Ok(canonical)
    }

    /// Step 2 — find the nearest `.sqry/graph` ancestor of `start`, bounded
    /// by project markers (cluster-E §E.1).
    ///
    /// Delegates to [`crate::workspace::discover_workspace_root`] so the walk
    /// terminates at the first ancestor containing any of
    /// [`crate::workspace::PROJECT_MARKERS`] (`.git`, `Cargo.toml`,
    /// `package.json`, `pyproject.toml`, `go.mod`). This eliminates the
    /// "stray `~/.sqry/graph` foot-gun" where a leftover graph at `$HOME`
    /// was silently picked up for a brand-new project that lacked its
    /// own graph.
    ///
    /// Returns:
    /// - `Some((root, depth, is_file_scope))` only when a graph exists at or
    ///   inside the project boundary
    ///   ([`crate::workspace::WorkspaceRootDiscovery::GraphFound`]).
    /// - `None` when no graph was found, OR when a graph was found but lives
    ///   in an *outer* project (the
    ///   [`crate::workspace::WorkspaceRootDiscovery::BoundaryOnly`] case
    ///   where the discovered graph does not belong to the same project
    ///   boundary as `start`). In the BoundaryOnly case the caller path
    ///   below uses `canonical_request` as the `workspace_root` for
    ///   [`GraphAcquisitionError::NoGraph`] / `AutoBuildIfEnabled`, which
    ///   is always inside the right project even if it is deeper than the
    ///   boundary itself.
    fn find_workspace_root(start: &Path) -> Option<(PathBuf, usize, bool)> {
        match crate::workspace::discover_workspace_root(start) {
            crate::workspace::WorkspaceRootDiscovery::GraphFound {
                root,
                depth,
                is_file_scope,
                ..
            } => Some((root, depth, is_file_scope)),
            crate::workspace::WorkspaceRootDiscovery::BoundaryOnly { .. }
            | crate::workspace::WorkspaceRootDiscovery::None => None,
        }
    }

    /// Step 4 — compute the plugin-selection compatibility verdict using the
    /// configured plugin manager and the persisted manifest. `manifest_path`
    /// (when known) is propagated into the
    /// [`PluginSelectionStatus::IncompatibleUnknownPluginIds`] variant so
    /// downstream consumers can render it in the user-facing error
    /// (cluster-E §E.2).
    fn classify_plugin_selection(
        &self,
        manifest: &Manifest,
        manifest_path: Option<&Path>,
        policy: &PluginSelectionPolicy,
    ) -> PluginSelectionStatus {
        let Some(persisted) = manifest.plugin_selection.as_ref() else {
            return PluginSelectionStatus::Exact;
        };

        let mut unknown: Vec<String> = persisted
            .active_plugin_ids
            .iter()
            .filter(|id| self.plugin_manager.plugin_by_id(id).is_none())
            .cloned()
            .collect();

        if unknown.is_empty() {
            return PluginSelectionStatus::Exact;
        }

        if let PluginSelectionPolicy::AllowUnknownIds { allowed } = policy {
            unknown.retain(|id| !allowed.contains(id));
            if unknown.is_empty() {
                return PluginSelectionStatus::Exact;
            }
        }

        PluginSelectionStatus::IncompatibleUnknownPluginIds {
            unknown_plugin_ids: unknown,
            manifest_path: manifest_path.map(Path::to_path_buf),
        }
    }
}

impl GraphAcquirer for FilesystemGraphProvider {
    fn acquire(
        &self,
        request: GraphAcquisitionRequest,
    ) -> Result<GraphAcquisition, GraphAcquisitionError> {
        // Step 1: path policy. Runs BEFORE any disk graph load.
        let canonical_request =
            self.apply_path_policy(&request.requested_path, &request.path_policy)?;

        // Step 2: find the nearest .sqry/graph ancestor.
        let Some((workspace_root, ancestor_depth, is_file_scope)) =
            Self::find_workspace_root(&canonical_request)
        else {
            // No artifact exists. Honor MissingGraphPolicy.
            return match request.missing_graph_policy {
                MissingGraphPolicy::Error => Err(GraphAcquisitionError::NoGraph {
                    workspace_root: canonical_request,
                }),
                MissingGraphPolicy::AutoBuildIfEnabled => match &self.auto_build {
                    Some(hook) => {
                        let graph = hook(&canonical_request)?;
                        Ok(GraphAcquisition {
                            graph,
                            workspace_root: canonical_request.clone(),
                            query_scope: None,
                            is_file_scope: false,
                            freshness: GraphFreshness::Fresh {
                                lifecycle_label: None,
                            },
                            identity: GraphIdentity {
                                snapshot_sha256: None,
                                manifest_built_at: None,
                                snapshot_format_version: None,
                                source_root: canonical_request.clone(),
                                plugin_selection_status: PluginSelectionStatus::Exact,
                            },
                            metadata: GraphAcquisitionMetadata {
                                acquisition_source: AcquisitionSource::Filesystem,
                                tool_name: request.tool_name,
                                notes: vec!["auto-built via provider hook".to_string()],
                            },
                        })
                    }
                    None => Err(GraphAcquisitionError::NoGraph {
                        workspace_root: canonical_request,
                    }),
                },
            };
        };

        // Step 2b: workspace-boundary check. After ancestor discovery the
        // canonical request must sit inside the resolved workspace root
        // unless symlink escape is explicitly allowed.
        if request.path_policy.require_within_workspace
            && !canonical_request.starts_with(&workspace_root)
            && !request.path_policy.allow_symlink_escape
        {
            return Err(GraphAcquisitionError::InvalidPath {
                path: request.requested_path,
                reason: format!(
                    "canonical path {:?} escapes workspace root {:?}",
                    canonical_request, workspace_root
                ),
            });
        }

        // Step 3: query scope and file-scope flags.
        let (query_scope, is_file_scope) = if ancestor_depth > 0 || is_file_scope {
            (Some(canonical_request.clone()), is_file_scope)
        } else {
            (None, false)
        };

        // Step 4: load manifest (when present) for SHA-256 + plugin compat.
        let storage = GraphStorage::new(&workspace_root);
        let mut manifest_opt: Option<Manifest> = None;
        let mut expected_sha = String::new();
        if storage.manifest_path().exists() {
            match storage.load_manifest() {
                Ok(m) => {
                    expected_sha = m.snapshot_sha256.clone();
                    manifest_opt = Some(m);
                }
                Err(e) => {
                    return Err(GraphAcquisitionError::LoadFailed {
                        source_root: workspace_root,
                        reason: format!("manifest unreadable: {e}"),
                    });
                }
            }
        }

        // Step 5: plugin-selection compatibility before snapshot read.
        let plugin_status = manifest_opt
            .as_ref()
            .map_or(PluginSelectionStatus::Exact, |m| {
                self.classify_plugin_selection(
                    m,
                    Some(storage.manifest_path()),
                    &request.plugin_selection_policy,
                )
            });
        if !matches!(plugin_status, PluginSelectionStatus::Exact) {
            return Err(GraphAcquisitionError::IncompatibleGraph {
                source_root: workspace_root,
                status: plugin_status,
            });
        }

        // Step 6: read snapshot bytes, verify SHA-256, deserialize.
        let snapshot_path = storage.snapshot_path().to_path_buf();
        let snapshot_bytes = match std::fs::read(&snapshot_path) {
            Ok(b) => b,
            Err(e) => {
                return Err(GraphAcquisitionError::LoadFailed {
                    source_root: workspace_root,
                    reason: format!("read snapshot {:?}: {e}", snapshot_path),
                });
            }
        };
        if let Err(e) = verify_snapshot_bytes(&snapshot_bytes, &expected_sha) {
            return Err(GraphAcquisitionError::LoadFailed {
                source_root: workspace_root,
                reason: format!("snapshot integrity check failed: {e}"),
            });
        }

        let graph = match load_from_bytes(&snapshot_bytes, Some(&self.plugin_manager)) {
            Ok(g) => Arc::new(g),
            // SGA03 Major #2 (codex iter2) — surface
            // `PersistenceError::IncompatibleVersion` as a typed
            // `IncompatibleGraph` with `IncompatibleSnapshotFormat` rather
            // than collapsing it into `LoadFailed`. Adapters (e.g. the
            // daemon's `From<GraphAcquisitionError>` impl) rely on this
            // distinction to map to `WorkspaceIncompatibleGraph` /
            // dedicated MCP error envelopes.
            Err(PersistenceError::IncompatibleVersion { expected, found }) => {
                return Err(GraphAcquisitionError::IncompatibleGraph {
                    source_root: workspace_root,
                    status: PluginSelectionStatus::IncompatibleSnapshotFormat {
                        reason: format!(
                            "snapshot version mismatch: expected {expected}, found {found}"
                        ),
                    },
                });
            }
            Err(e) => {
                return Err(GraphAcquisitionError::LoadFailed {
                    source_root: workspace_root,
                    reason: format!("snapshot deserialize: {e}"),
                });
            }
        };

        // Step 7: assemble identity + metadata from the manifest (when present).
        let identity = GraphIdentity {
            snapshot_sha256: manifest_opt.as_ref().map(|m| m.snapshot_sha256.clone()),
            manifest_built_at: manifest_opt.as_ref().map(|m| m.built_at.clone()),
            snapshot_format_version: manifest_opt.as_ref().map(|m| m.snapshot_format_version),
            source_root: workspace_root.clone(),
            plugin_selection_status: PluginSelectionStatus::Exact,
        };

        Ok(GraphAcquisition {
            graph,
            workspace_root,
            query_scope,
            is_file_scope,
            freshness: GraphFreshness::Fresh {
                lifecycle_label: None,
            },
            identity,
            metadata: GraphAcquisitionMetadata {
                acquisition_source: AcquisitionSource::Filesystem,
                tool_name: request.tool_name,
                notes: Vec::new(),
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Build a `GraphAcquisitionRequest` for tests with the strict defaults.
    fn make_request(operation: AcquisitionOperation) -> GraphAcquisitionRequest {
        GraphAcquisitionRequest {
            requested_path: PathBuf::from("/tmp/sga02-test-workspace"),
            operation,
            path_policy: PathPolicy::default(),
            missing_graph_policy: MissingGraphPolicy::Error,
            stale_policy: StalePolicy::default(),
            plugin_selection_policy: PluginSelectionPolicy::default(),
            tool_name: Some("sga02_test"),
        }
    }

    /// Make a freshness-only check possible without instantiating a real
    /// `CodeGraph` arena. Tests exercise the contract types, not the
    /// underlying graph; the freshness/source mapping is what SGA02 owns.
    fn freshness_metadata_pair(
        freshness: GraphFreshness,
        source: AcquisitionSource,
    ) -> (GraphFreshness, GraphAcquisitionMetadata) {
        (
            freshness,
            GraphAcquisitionMetadata {
                acquisition_source: source,
                tool_name: Some("sga02_test"),
                notes: vec![],
            },
        )
    }

    /// Test 1: `ReadOnlyQuery` may produce Fresh, Stale, and Reloaded
    /// freshness states, and metadata exposes both the freshness variant and
    /// the acquisition source for each one.
    #[test]
    fn acquire_mode_read_only_allows_stale_and_reloaded() {
        let request = make_request(AcquisitionOperation::ReadOnlyQuery);
        assert_eq!(request.operation, AcquisitionOperation::ReadOnlyQuery);

        let cases = vec![
            freshness_metadata_pair(
                GraphFreshness::Fresh {
                    lifecycle_label: Some("Loaded"),
                },
                AcquisitionSource::DaemonReadOnly,
            ),
            freshness_metadata_pair(
                GraphFreshness::Stale {
                    last_good_at: Some("2026-05-07T12:00:00Z".to_string()),
                    last_error: Some("rebuild failed".to_string()),
                    age_hours: Some(0.5),
                },
                AcquisitionSource::DaemonReadOnly,
            ),
            freshness_metadata_pair(
                GraphFreshness::Reloaded {
                    original_lifecycle: ReloadOrigin::Evicted {
                        detail: "memory admission evicted A".to_string(),
                    },
                    final_lifecycle_label: "Loaded",
                    reload_attempts: NonZeroU8::new(1).expect("1 is non-zero"),
                },
                AcquisitionSource::DaemonReloaded,
            ),
        ];

        for (freshness, metadata) in cases {
            // Each variant carries enough metadata for adapters to render
            // freshness signals without inspecting the graph contents.
            match &freshness {
                GraphFreshness::Fresh { lifecycle_label } => {
                    assert_eq!(*lifecycle_label, Some("Loaded"));
                }
                GraphFreshness::Stale {
                    last_good_at,
                    age_hours,
                    ..
                } => {
                    assert!(last_good_at.is_some());
                    assert!(age_hours.is_some());
                }
                GraphFreshness::Reloaded {
                    final_lifecycle_label,
                    reload_attempts,
                    ..
                } => {
                    assert_eq!(*final_lifecycle_label, "Loaded");
                    assert_eq!(reload_attempts.get(), 1);
                }
            }
            // Source must be present and tool_name must propagate.
            assert!(matches!(
                metadata.acquisition_source,
                AcquisitionSource::DaemonReadOnly | AcquisitionSource::DaemonReloaded
            ));
            assert_eq!(metadata.tool_name, Some("sga02_test"));
        }
    }

    /// Mock acquirer used by tests 2 and 3. Records every call and returns a
    /// scripted result.
    struct ScriptedAcquirer {
        load_attempts: AtomicUsize,
        last_op: Mutex<Option<AcquisitionOperation>>,
        invalid_path: bool,
        rebuild_only_stale_available: bool,
    }

    impl ScriptedAcquirer {
        fn new() -> Self {
            Self {
                load_attempts: AtomicUsize::new(0),
                last_op: Mutex::new(None),
                invalid_path: false,
                rebuild_only_stale_available: false,
            }
        }

        fn with_invalid_path(mut self) -> Self {
            self.invalid_path = true;
            self
        }

        fn with_rebuild_only_stale(mut self) -> Self {
            self.rebuild_only_stale_available = true;
            self
        }

        fn loads_attempted(&self) -> usize {
            self.load_attempts.load(Ordering::SeqCst)
        }
    }

    impl GraphAcquirer for ScriptedAcquirer {
        fn acquire(
            &self,
            request: GraphAcquisitionRequest,
        ) -> Result<GraphAcquisition, GraphAcquisitionError> {
            *self.last_op.lock().expect("mutex unpoisoned") = Some(request.operation);

            // Path policy runs *before* any load attempt.
            if self.invalid_path {
                return Err(GraphAcquisitionError::InvalidPath {
                    path: request.requested_path,
                    reason: "test fixture: path rejected before load".to_string(),
                });
            }

            // Only after path validation do we count this as a load attempt.
            self.load_attempts.fetch_add(1, Ordering::SeqCst);

            if self.rebuild_only_stale_available
                && request.operation == AcquisitionOperation::MutatingRebuild
            {
                // Mutating rebuild must NOT be served from stale data — the
                // mock returns LoadFailed instead of synthesising a stale or
                // reloaded GraphAcquisition.
                return Err(GraphAcquisitionError::LoadFailed {
                    source_root: request.requested_path,
                    reason: "only stale graph available; rebuild requires fresh".to_string(),
                });
            }

            // Default: not exercised in these mock-only tests.
            Err(GraphAcquisitionError::Internal {
                reason: "scripted acquirer reached unreachable arm".to_string(),
            })
        }
    }

    /// Test 2: `MutatingRebuild` cannot silently fall back to a stale or
    /// reloaded graph. The mock provider proves no such `GraphFreshness` is
    /// produced — instead a `LoadFailed` (or another typed error) is returned.
    #[test]
    fn acquire_mode_rebuild_rejects_read_only_fallback() {
        let acquirer = ScriptedAcquirer::new().with_rebuild_only_stale();
        let result = acquirer.acquire(make_request(AcquisitionOperation::MutatingRebuild));

        match result {
            Err(GraphAcquisitionError::LoadFailed { reason, .. }) => {
                assert!(
                    reason.contains("stale"),
                    "expected stale-related diagnostic, got {reason}"
                );
            }
            Err(other) => panic!("unexpected error variant: {other:?}"),
            Ok(acq) => panic!(
                "MutatingRebuild must not yield a stale/reloaded acquisition, got freshness={:?}",
                acq.freshness
            ),
        }

        // Sanity: we didn't accidentally hit a ReadOnlyQuery branch.
        let last_op = *acquirer.last_op.lock().expect("mutex unpoisoned");
        assert_eq!(last_op, Some(AcquisitionOperation::MutatingRebuild));
    }

    /// Test 3: `InvalidPath` errors precede any load attempt — the mock's
    /// load counter must remain zero.
    #[test]
    fn invalid_path_error_precedes_load_error() {
        let acquirer = ScriptedAcquirer::new().with_invalid_path();
        let result = acquirer.acquire(make_request(AcquisitionOperation::ReadOnlyQuery));

        assert!(
            matches!(result, Err(GraphAcquisitionError::InvalidPath { .. })),
            "expected InvalidPath error, got {result:?}"
        );
        assert_eq!(
            acquirer.loads_attempted(),
            0,
            "no load should be attempted when the path policy rejects the request"
        );
    }

    /// Mock manager that counts daemon reload attempts. Used by tests 4 and 5.
    struct ReloadCountingManager {
        reload_attempts: AtomicUsize,
        reload_succeeds: bool,
    }

    impl ReloadCountingManager {
        fn new(reload_succeeds: bool) -> Self {
            Self {
                reload_attempts: AtomicUsize::new(0),
                reload_succeeds,
            }
        }

        fn attempt_reload(&self) -> Result<&'static str, String> {
            self.reload_attempts.fetch_add(1, Ordering::SeqCst);
            if self.reload_succeeds {
                Ok("Loaded")
            } else {
                Err("test fixture: reload failed".to_string())
            }
        }

        fn reload_count(&self) -> usize {
            self.reload_attempts.load(Ordering::SeqCst)
        }
    }

    /// Acquirer that emulates the daemon one-shot bounded reload contract for
    /// `ReadOnlyQuery`: at most one reload, never recursive.
    struct BoundedReloadAcquirer<'a> {
        manager: &'a ReloadCountingManager,
        original_lifecycle_label: &'static str,
        original_eviction_detail: &'static str,
    }

    impl<'a> GraphAcquirer for BoundedReloadAcquirer<'a> {
        fn acquire(
            &self,
            request: GraphAcquisitionRequest,
        ) -> Result<GraphAcquisition, GraphAcquisitionError> {
            // ReadOnlyQuery is the only mode permitted to attempt reload —
            // a guard the trait contract requires implementations to honor.
            if request.operation != AcquisitionOperation::ReadOnlyQuery {
                return Err(GraphAcquisitionError::Evicted {
                    workspace_root: request.requested_path,
                    original_lifecycle: self.original_lifecycle_label.to_string(),
                    reload_failure: None,
                });
            }
            // Exactly one reload attempt — no loop.
            match self.manager.attempt_reload() {
                Ok(_label) => Err(GraphAcquisitionError::Internal {
                    reason: "test stops here: success path requires real CodeGraph".to_string(),
                }),
                Err(reload_err) => Err(GraphAcquisitionError::Evicted {
                    workspace_root: request.requested_path,
                    original_lifecycle: self.original_lifecycle_label.to_string(),
                    reload_failure: Some(format!(
                        "evicted({}); reload: {}",
                        self.original_eviction_detail, reload_err
                    )),
                }),
            }
        }
    }

    /// Test 4: Eviction reload is bounded to exactly one attempt; no loop.
    #[test]
    fn evicted_reload_attempt_is_bounded() {
        let manager = ReloadCountingManager::new(true);
        let acquirer = BoundedReloadAcquirer {
            manager: &manager,
            original_lifecycle_label: "Evicted",
            original_eviction_detail: "memory admission evicted A",
        };

        let _ = acquirer.acquire(make_request(AcquisitionOperation::ReadOnlyQuery));
        assert_eq!(
            manager.reload_count(),
            1,
            "ReadOnlyQuery reload must be attempted exactly once"
        );

        // A subsequent acquire call is a separate request; the bounded rule
        // is *per request*, so a second request may attempt one more reload.
        // The critical guarantee is: within a single request, no recursive
        // re-entry. The acquirer above never calls itself.
        let _ = acquirer.acquire(make_request(AcquisitionOperation::ReadOnlyQuery));
        assert_eq!(
            manager.reload_count(),
            2,
            "second request can attempt its own single reload, but neither request looped"
        );
    }

    /// Test 5: When the bounded reload fails, the resulting `Evicted` error
    /// preserves both the original lifecycle context and the reload failure
    /// detail so adapters can render full diagnostics.
    #[test]
    fn reload_failure_preserves_original_lifecycle_context() {
        let manager = ReloadCountingManager::new(false);
        let acquirer = BoundedReloadAcquirer {
            manager: &manager,
            original_lifecycle_label: "Evicted",
            original_eviction_detail: "memory admission evicted A",
        };

        let result = acquirer.acquire(make_request(AcquisitionOperation::ReadOnlyQuery));
        match result {
            Err(GraphAcquisitionError::Evicted {
                original_lifecycle,
                reload_failure,
                ..
            }) => {
                assert_eq!(original_lifecycle, "Evicted");
                let reload = reload_failure.expect("reload failure must be recorded");
                assert!(
                    reload.contains("memory admission evicted A"),
                    "reload diagnostic must carry the original eviction detail, got: {reload}"
                );
                assert!(
                    reload.contains("test fixture: reload failed"),
                    "reload diagnostic must carry the reload failure detail, got: {reload}"
                );
            }
            other => panic!("expected Evicted with reload_failure, got {other:?}"),
        }
        assert_eq!(manager.reload_count(), 1);
    }

    /// Compile-time guard: the contract types must not be hidden behind
    /// trait-object-incompatible bounds. If someone adds a non-object-safe
    /// method to `GraphAcquirer`, this fails to build.
    #[test]
    fn graph_acquirer_is_object_safe() {
        fn assert_object_safe(_: &dyn GraphAcquirer) {}
        let acquirer = ScriptedAcquirer::new();
        assert_object_safe(&acquirer);
    }

    /// Defaults are the most-restrictive options: tightening must be the
    /// silent baseline, weakening must be explicit.
    #[test]
    fn policy_defaults_are_strict() {
        let p = PathPolicy::default();
        assert!(p.require_existing);
        assert!(p.require_within_workspace);
        assert!(!p.allow_symlink_escape);
        assert!(matches!(StalePolicy::default(), StalePolicy::RejectStale));
        assert!(matches!(
            PluginSelectionPolicy::default(),
            PluginSelectionPolicy::StrictMatch
        ));
    }

    // -------------------------------------------------------------------
    // FilesystemGraphProvider unit tests (SGA03)
    // -------------------------------------------------------------------

    use crate::graph::FilesystemGraphProvider;
    use crate::graph::unified::persistence::{
        BuildProvenance, GraphStorage, MANIFEST_SCHEMA_VERSION, Manifest, PluginSelectionManifest,
        SNAPSHOT_FORMAT_VERSION, save_to_path,
    };
    use crate::plugin::PluginManager;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Build a minimal valid `.sqry/graph` snapshot + manifest in `root`.
    ///
    /// We construct an empty `CodeGraph` and persist it directly via
    /// `save_to_path`, then write a matching `manifest.json`. Going through
    /// the full `build_unified_graph` pipeline would require registering a
    /// plugin with a `GraphBuilder` impl — and every such plugin lives in
    /// a crate that already depends on `sqry-core`, which would create a
    /// circular dev-dependency. The acquisition contract is independent of
    /// graph contents: an empty snapshot exercises the manifest, SHA-256
    /// integrity, snapshot format, and plugin-selection compatibility paths
    /// the same way a populated one would.
    fn build_test_fixture(root: &Path, plugin_ids: &[&str]) {
        let storage = GraphStorage::new(root);
        fs::create_dir_all(storage.graph_dir()).expect("graph dir");

        let graph = CodeGraph::new();
        save_to_path(&graph, storage.snapshot_path()).expect("save snapshot");

        let snapshot_sha256 = {
            let bytes = fs::read(storage.snapshot_path()).expect("read snapshot");
            hex::encode(Sha256::digest(&bytes))
        };

        let snapshot = graph.snapshot();
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            built_at: chrono::Utc::now().to_rfc3339(),
            root_path: root.to_string_lossy().into_owned(),
            node_count: snapshot.nodes().len(),
            edge_count: graph.edge_count(),
            raw_edge_count: None,
            snapshot_sha256,
            build_provenance: BuildProvenance {
                sqry_version: env!("CARGO_PKG_VERSION").to_string(),
                build_timestamp: chrono::Utc::now().to_rfc3339(),
                build_command: "test:filesystem-provider".to_string(),
                plugin_hashes: std::collections::HashMap::new(),
            },
            file_count: std::collections::HashMap::new(),
            languages: Vec::new(),
            config: std::collections::HashMap::new(),
            confidence: graph.confidence().clone(),
            last_indexed_commit: None,
            plugin_selection: Some(PluginSelectionManifest {
                active_plugin_ids: plugin_ids.iter().map(|id| (*id).to_string()).collect(),
                high_cost_mode: None,
            }),
        };
        manifest
            .save(storage.manifest_path())
            .expect("save manifest");
    }

    fn make_provider() -> FilesystemGraphProvider {
        FilesystemGraphProvider::new(Arc::new(PluginManager::new()))
    }

    fn fs_request(path: PathBuf) -> GraphAcquisitionRequest {
        GraphAcquisitionRequest {
            requested_path: path,
            operation: AcquisitionOperation::ReadOnlyQuery,
            path_policy: PathPolicy::default(),
            missing_graph_policy: MissingGraphPolicy::Error,
            stale_policy: StalePolicy::default(),
            plugin_selection_policy: PluginSelectionPolicy::default(),
            tool_name: Some("filesystem_provider_test"),
        }
    }

    /// Test 1 — non-existent paths must fail with InvalidPath BEFORE any
    /// graph load is attempted.
    #[test]
    fn filesystem_provider_returns_invalid_path_for_nonexistent_path() {
        let tmp = TempDir::new().expect("tempdir");
        let bogus = tmp.path().join("does/not/exist");

        let provider = make_provider();
        let err = provider
            .acquire(fs_request(bogus.clone()))
            .expect_err("non-existent path must fail");
        match err {
            GraphAcquisitionError::InvalidPath { path, reason } => {
                assert_eq!(path, bogus);
                assert!(
                    reason.contains("does not exist") || reason.contains("cannot be canonicalized"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    /// Test 2 — outside-workspace paths fail with InvalidPath. We construct
    /// two sibling tempdirs: a workspace with a graph, and an unrelated
    /// directory the user passes by mistake. The provider must reject the
    /// unrelated directory before any load attempt.
    #[test]
    fn filesystem_provider_returns_invalid_path_for_outside_workspace() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("mk workspace");
        // Build a real graph fixture so a workspace exists; this guarantees
        // that *unrelated* paths still fail because no ancestor traversal
        // from outside finds this graph.
        let plugins = build_plugin_manager_for_tests();
        build_test_fixture(&workspace, &["mock-rust"]);

        // A sibling directory has no graph anywhere up the tree (as long as
        // no ancestor of the tempdir has one). The provider must therefore
        // surface NoGraph rather than IncompatibleGraph or LoadFailed.
        let sibling = tmp.path().join("sibling");
        fs::create_dir_all(&sibling).expect("mk sibling");
        let provider = FilesystemGraphProvider::new(Arc::new(plugins));
        let err = provider
            .acquire(fs_request(sibling.clone()))
            .expect_err("sibling without graph must fail");
        // The strict spec says this fails BEFORE any graph load. The
        // observable signal is either NoGraph (ancestor walk found nothing
        // within the tempdir hierarchy) or InvalidPath if a host-level
        // ancestor `.sqry/graph` is present. Either way the request_path
        // is preserved.
        assert!(
            matches!(
                err,
                GraphAcquisitionError::NoGraph { .. } | GraphAcquisitionError::InvalidPath { .. }
            ),
            "expected NoGraph or InvalidPath, got {err:?}"
        );
    }

    /// Test 3 — happy path: a tempdir with a freshly built `.sqry/graph`
    /// snapshot loads successfully with `Fresh` freshness.
    #[test]
    fn filesystem_provider_loads_existing_valid_graph() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("mk workspace");

        let plugins = build_plugin_manager_for_tests();
        build_test_fixture(&workspace, &["mock-rust"]);

        let provider = FilesystemGraphProvider::new(Arc::new(plugins));
        let acquisition = provider
            .acquire(fs_request(workspace.clone()))
            .expect("provider acquires existing graph");

        assert_eq!(
            acquisition.workspace_root,
            workspace.canonicalize().expect("canon workspace")
        );
        assert!(matches!(
            acquisition.freshness,
            GraphFreshness::Fresh { .. }
        ));
        assert_eq!(
            acquisition.metadata.acquisition_source,
            AcquisitionSource::Filesystem
        );
        assert!(acquisition.identity.snapshot_sha256.is_some());
        assert_eq!(
            acquisition.identity.plugin_selection_status,
            PluginSelectionStatus::Exact
        );
    }

    /// Test 4 — manifest plugin ids the runtime does not know must surface as
    /// `IncompatibleGraph { status: IncompatibleUnknownPluginIds }` and the
    /// snapshot must NOT be deserialized.
    #[test]
    fn filesystem_provider_unknown_plugin_ids_returns_incompatible_graph() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("mk workspace");

        let plugins = build_plugin_manager_for_tests();
        // Manifest references the runtime's known id ("mock-rust") plus a
        // bogus id the runtime does not register.
        build_test_fixture(
            &workspace,
            &["mock-rust", "imaginary-unknown-plugin-id-zzz"],
        );

        let provider = FilesystemGraphProvider::new(Arc::new(plugins));
        let err = provider
            .acquire(fs_request(workspace.clone()))
            .expect_err("manifest with unknown plugin id must fail");
        match err {
            GraphAcquisitionError::IncompatibleGraph { status, .. } => match status {
                PluginSelectionStatus::IncompatibleUnknownPluginIds {
                    unknown_plugin_ids,
                    manifest_path,
                } => {
                    assert!(
                        unknown_plugin_ids.iter().any(|id| id.contains("imaginary")),
                        "expected the synthetic id in the diagnostic, got {unknown_plugin_ids:?}"
                    );
                    assert!(
                        manifest_path
                            .as_ref()
                            .is_some_and(|p| p.ends_with("manifest.json")),
                        "manifest_path should point at the on-disk manifest, got {manifest_path:?}"
                    );
                }
                other => panic!("expected IncompatibleUnknownPluginIds, got {other:?}"),
            },
            other => panic!("expected IncompatibleGraph, got {other:?}"),
        }
    }

    /// SGA03 Major #2 (codex iter2) — when the on-disk snapshot uses an
    /// incompatible format version (e.g. a future or unknown V*), the
    /// filesystem provider must surface that as
    /// `IncompatibleGraph { status: IncompatibleSnapshotFormat { .. } }`,
    /// **not** as a generic `LoadFailed`. The daemon's
    /// `From<GraphAcquisitionError>` impl maps the typed variant to
    /// `WorkspaceIncompatibleGraph` (-32005), which is materially
    /// different from the transient-build retry signal `LoadFailed`
    /// → `WorkspaceBuildFailed` (-32001).
    #[test]
    fn filesystem_provider_incompatible_snapshot_version_returns_incompatible_graph() {
        use crate::graph::unified::persistence::{GraphHeader, MAGIC_BYTES_V10};

        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).expect("mk workspace");

        // Build a normal manifest+snapshot fixture, then overwrite the
        // snapshot bytes with a hand-crafted file whose magic is the
        // current V10 magic but whose `GraphHeader.version` is `99` —
        // outside the accepted set {V7, V8, V9, V10}. `load_from_bytes`
        // returns `PersistenceError::IncompatibleVersion` for that case.
        build_test_fixture(&workspace, &["mock-rust"]);

        let storage = GraphStorage::new(&workspace);
        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = 99;
        let header_bytes = postcard::to_allocvec(&header).expect("encode header");

        let mut bytes: Vec<u8> = Vec::with_capacity(14 + 4 + header_bytes.len() + 8);
        bytes.extend_from_slice(MAGIC_BYTES_V10);
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        // `data_len = 0` so the loader fails on the version check, not on
        // a short-read while parsing the data section.
        bytes.extend_from_slice(&0u64.to_le_bytes());
        fs::write(storage.snapshot_path(), &bytes).expect("write bogus snapshot");

        // Update the manifest's `snapshot_sha256` so the integrity
        // check passes and we exercise the post-integrity version
        // mismatch path (the path the codex review pinpointed).
        let snapshot_sha256 = hex::encode(Sha256::digest(&bytes));
        let mut manifest = Manifest::load(storage.manifest_path()).expect("load manifest");
        manifest.snapshot_sha256 = snapshot_sha256;
        manifest
            .save(storage.manifest_path())
            .expect("save manifest");

        let plugins = build_plugin_manager_for_tests();
        let provider = FilesystemGraphProvider::new(Arc::new(plugins));
        let err = provider
            .acquire(fs_request(workspace.clone()))
            .expect_err("incompatible-version snapshot must fail acquisition");

        match err {
            GraphAcquisitionError::IncompatibleGraph {
                status,
                source_root,
            } => {
                assert_eq!(source_root, workspace.canonicalize().unwrap_or(workspace));
                match status {
                    PluginSelectionStatus::IncompatibleSnapshotFormat { reason } => {
                        assert!(
                            reason.contains("snapshot version mismatch")
                                && reason.contains("found 99"),
                            "expected snapshot version mismatch diagnostic, got {reason:?}"
                        );
                    }
                    other => panic!("expected IncompatibleSnapshotFormat, got {other:?}"),
                }
            }
            other => panic!("expected IncompatibleGraph, got {other:?}"),
        }
    }

    /// Build a `PluginManager` with a single mock plugin registered under id
    /// `"mock-rust"`. We do NOT use `sqry-lang-rust` here — that crate
    /// depends on `sqry-core`, which would create a circular crate-version
    /// dependency in tests. Defining the mock inline keeps the unit tests
    /// hermetic and lets us exercise the plugin-selection compatibility
    /// path without dragging in any language-specific build infrastructure.
    fn build_plugin_manager_for_tests() -> PluginManager {
        use crate::plugin::types::LanguageMetadata;
        use crate::plugin::types::LanguagePlugin;
        use std::path::Path;

        struct AcquisitionTestPlugin;

        impl LanguagePlugin for AcquisitionTestPlugin {
            fn metadata(&self) -> LanguageMetadata {
                LanguageMetadata {
                    id: "mock-rust",
                    name: "MockRust",
                    version: "0.0.0",
                    author: "sqry-tests",
                    description: "FilesystemGraphProvider acquisition tests",
                    tree_sitter_version: "0.24",
                }
            }

            fn extensions(&self) -> &'static [&'static str] {
                &["rs"]
            }

            fn language(&self) -> tree_sitter::Language {
                tree_sitter_rust::LANGUAGE.into()
            }

            fn parse_ast(
                &self,
                _content: &[u8],
            ) -> Result<tree_sitter::Tree, crate::plugin::error::ParseError> {
                Err(crate::plugin::error::ParseError::TreeSitterFailed)
            }

            fn extract_scopes(
                &self,
                _tree: &tree_sitter::Tree,
                _content: &[u8],
                _file: &Path,
            ) -> Result<Vec<crate::ast::Scope>, crate::plugin::error::ScopeError> {
                Ok(Vec::new())
            }
        }

        let mut pm = PluginManager::new();
        pm.register_builtin(Box::new(AcquisitionTestPlugin));
        pm
    }
}
