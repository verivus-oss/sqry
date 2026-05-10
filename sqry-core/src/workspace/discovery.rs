//! Workspace repository discovery utilities.
//!
//! Discovery scans a workspace root for repositories that have been indexed
//! by `sqry index`. The canonical marker is `.sqry/graph/manifest.json` —
//! the same file that `build_unified_graph_inner` writes (see
//! `graph/unified/persistence/mod.rs`'s `GRAPH_DIR_NAME` and
//! `MANIFEST_FILE_NAME` constants). The earlier `.sqry-index` placeholder
//! was never written by the live build pipeline and is removed outright;
//! there is no legacy fallback (RR-10 Gap #2 retains the per-workspace
//! repository cap to bound walker work regardless of marker name).
//!
//! The walker honours `.gitignore` rules and additionally skips a small
//! set of dependency / build directories whose contents must never be
//! treated as discoverable repositories even when those directories are
//! present without a `.gitignore` (e.g. `node_modules`, `target`). The
//! ignore list is the repo-wide
//! [`crate::project::path_utils::DEFAULT_IGNORED_DIRS`] (consulted via
//! [`crate::project::path_utils::is_ignored_dir`]) so workspace discovery
//! and single-repo project detection share one source of truth.

use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use thiserror::Error;

use super::error::{WorkspaceError, WorkspaceResult};
use super::registry::{WorkspaceRepoId, WorkspaceRepository};
// RR-10 Gap #2: Import repository count limit for DoS prevention
use crate::config::buffers::max_repositories;
// Repo-wide source of truth for directories to skip during repo discovery.
use crate::project::path_utils::is_ignored_dir;

/// Canonical marker filename written by `sqry index` under
/// `<repo>/.sqry/graph/`. Discovery treats any file whose name matches this
/// constant and whose parent directory is `.sqry/graph` as evidence of a
/// repository root one level above.
const MANIFEST_FILE_NAME: &str = "manifest.json";

/// Directory segment containing [`MANIFEST_FILE_NAME`]. Used to validate
/// that a candidate `manifest.json` actually lives inside a sqry graph
/// directory (and not, say, an unrelated NPM `manifest.json`).
const GRAPH_DIR_SEGMENT: &str = "graph";

/// Parent of [`GRAPH_DIR_SEGMENT`]. The full canonical relative path is
/// `.sqry/graph/manifest.json`.
const SQRY_DIR_SEGMENT: &str = ".sqry";

/// Discovery strategy for locating repositories within a workspace root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    /// Locate repositories by finding `.sqry/graph/manifest.json` markers
    /// anywhere under root.
    IndexFiles,
    /// Only include repositories that are git roots (contain `.git/`) with
    /// an index marker.
    GitRoots,
}

/// Discover repositories beneath `root` according to `mode`.
///
/// # Errors
///
/// Returns [`WorkspaceError`] when filesystem traversal fails.
pub fn discover_repositories(
    root: &Path,
    mode: DiscoveryMode,
) -> WorkspaceResult<Vec<WorkspaceRepository>> {
    let mut repositories = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|entry| {
            // Skip well-known dependency / build directories so discovery
            // never wastes work descending into them. The ignore list is
            // owned by `crate::project::path_utils::DEFAULT_IGNORED_DIRS`
            // and consulted through `is_ignored_dir` to keep workspace
            // discovery and single-repo project detection in lockstep.
            !is_ignored_dir(entry.file_name())
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(ok) => ok,
            Err(err) => {
                let message = err.to_string();
                let io_err = err
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other(message));
                return Err(WorkspaceError::Discovery {
                    root: root.to_path_buf(),
                    source: io_err,
                });
            }
        };

        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }

        if entry.file_name() != MANIFEST_FILE_NAME {
            continue;
        }

        let manifest_path = entry.into_path();

        // Validate the candidate sits under `<repo>/.sqry/graph/manifest.json`.
        // Without this guard, any `manifest.json` (e.g. NPM's package
        // manifest) would be misclassified as a sqry index marker.
        let Some(graph_dir) = manifest_path.parent() else {
            continue;
        };
        if graph_dir.file_name().and_then(|s| s.to_str()) != Some(GRAPH_DIR_SEGMENT) {
            continue;
        }
        let Some(sqry_dir) = graph_dir.parent() else {
            continue;
        };
        if sqry_dir.file_name().and_then(|s| s.to_str()) != Some(SQRY_DIR_SEGMENT) {
            continue;
        }
        let Some(repo_root) = sqry_dir.parent().map(Path::to_path_buf) else {
            continue;
        };

        if matches!(mode, DiscoveryMode::GitRoots) && !repo_root.join(".git").is_dir() {
            continue;
        }

        let relative_path = repo_root.strip_prefix(root).unwrap_or(repo_root.as_path());
        let repo_id = WorkspaceRepoId::new(relative_path);
        let name = repo_root.file_name().map_or_else(
            || repo_id.as_str().to_string(),
            |os| os.to_string_lossy().into_owned(),
        );

        let metadata = fs::metadata(&manifest_path);
        let last_indexed_at = metadata.ok().and_then(|meta| meta.modified().ok());

        // RR-10 Gap #2: Enforce repository count limit to prevent DoS via
        // workspaces containing thousands of indexed repositories.
        let max_repos = max_repositories();
        if repositories.len() >= max_repos {
            return Err(WorkspaceError::TooManyRepositories {
                found: repositories.len(),
                limit: max_repos,
            });
        }

        repositories.push(WorkspaceRepository::new(
            repo_id,
            name,
            repo_root,
            manifest_path,
            last_indexed_at,
        ));
    }

    repositories.sort_by(|a, b| a.id.cmp(&b.id));
    repositories.dedup_by(|a, b| a.id == b.id);
    Ok(repositories)
}

// ───────────────────────────────────────────────────────────────────────
// Ancestor-walk discovery (sqry-mcp flakiness P1, cluster E foundation)
// ───────────────────────────────────────────────────────────────────────
//
// The ancestor-walk discovery below is a **separate concern** from
// `discover_repositories` above. Where `discover_repositories` walks
// **down** a workspace root looking for indexed repositories, the
// ancestor walker walks **up** from a starting path to locate the
// project boundary that should anchor `.sqry/graph` lookups.
//
// Source: `E_p1_cluster.md` §E.1 + `E_p1_cluster.md` §Hand-offs +
// `00_contracts.md` §3.CC-4. The ancestor walk fixes #237 (workspace
// discovery and plugin-selection recovery) and gates #239 (workspace
// artifact hygiene + nested-index creation guard).

/// Maximum ancestor walk depth for [`discover_workspace_root`]. Mirrors
/// the `MAX_ANCESTOR_DEPTH = 64` already used by
/// `sqry-core/src/graph/acquisition.rs::find_workspace_root` and
/// `sqry-cli/src/index_discovery.rs::find_nearest_index`. Bound is
/// O(64) lstat calls per discovery — cheap enough that no caching is
/// required, and small enough that no realistic project layout
/// approaches the limit.
pub const MAX_ANCESTOR_DEPTH: usize = 64;

/// Project-boundary marker filenames consulted by
/// [`discover_workspace_root`]. The walker stops at the **first**
/// ancestor containing any of these markers, even if no
/// `.sqry/graph` exists in or above that ancestor. Order does not
/// matter — presence of any marker terminates the walk.
///
/// The five chosen markers cover ~95% of sqry's known user base by
/// language. Expansion to `setup.py`, `Gemfile`, `mix.exs`,
/// `build.gradle`, `pom.xml`, or `composer.json` is deliberately
/// deferred until field reports show false-`None` discoveries in
/// Ruby / Elixir / JVM monorepos (see `E_p1_cluster.md` Open Q4).
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
];

/// Outcome of [`discover_workspace_root`]. Distinguishes "found a
/// graph inside the project boundary" from "hit the project boundary
/// with no graph above it" so callers that *create* indexes (e.g.
/// `sqry index`) can refuse to descend into ancestors of an outer
/// project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRootDiscovery {
    /// `.sqry/graph` (or legacy `.sqry-index`) found at `root` AT OR
    /// INSIDE the project boundary at `boundary`. `depth` is the
    /// ancestor distance from the discovery starting point to `root`.
    GraphFound {
        /// The directory immediately above `.sqry/graph`. Use this as
        /// the workspace root for graph-load operations.
        root: PathBuf,
        /// The first ancestor containing a [`PROJECT_MARKERS`] entry.
        /// Equal to `root` when the project marker lives at the same
        /// directory as the `.sqry/graph` artifact.
        boundary: PathBuf,
        /// Depth from the original starting point.
        depth: usize,
        /// `true` when the walk started at a regular file (we walk up
        /// from its parent); `false` when it started at a directory.
        is_file_scope: bool,
    },
    /// Project boundary reached without seeing any `.sqry/graph`.
    /// Used by callers that want to *create* an index here.
    BoundaryOnly {
        /// The first ancestor containing a [`PROJECT_MARKERS`] entry.
        boundary: PathBuf,
        /// `true` when the walk started at a regular file (we walk up
        /// from its parent); `false` when it started at a directory.
        is_file_scope: bool,
    },
    /// Walked the full [`MAX_ANCESTOR_DEPTH`] (or hit the filesystem
    /// root) without finding either a graph or a project marker.
    /// Caller must require an explicit `--workspace-root`.
    None,
}

/// Walk `start`'s ancestors looking for the canonical project root
/// for sqry's purposes (per `E_p1_cluster.md` §E.1).
///
/// The walker:
///
/// 1. Best-effort canonicalises `start` (falls back to `start` on
///    error).
/// 2. If `start` is a file, begins from the parent directory.
/// 3. Walks up to [`MAX_ANCESTOR_DEPTH`] ancestors, recording the
///    *closest* `.sqry/graph` directory **and** the *closest*
///    [`PROJECT_MARKERS`] hit. The walk terminates as soon as a
///    project marker is observed — that marker is the
///    boundary-of-record even if a graph also exists at a deeper
///    level.
/// 4. Maps the `(graph_found, boundary)` pair to the
///    [`WorkspaceRootDiscovery`] enum:
///    - graph inside boundary → [`WorkspaceRootDiscovery::GraphFound`].
///    - graph above boundary (e.g. stray `~/.sqry/graph` while a
///      `Cargo.toml` lives at `~/work/proj`) →
///      [`WorkspaceRootDiscovery::BoundaryOnly`] (the outer graph is
///      discarded).
///    - graph but no marker → [`WorkspaceRootDiscovery::GraphFound`]
///      (legacy bare-directory layouts).
///    - no graph, marker only → [`WorkspaceRootDiscovery::BoundaryOnly`].
///    - neither → [`WorkspaceRootDiscovery::None`].
#[must_use]
pub fn discover_workspace_root(start: &Path) -> WorkspaceRootDiscovery {
    let canonical = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let is_file_scope = canonical.is_file();
    let mut current: PathBuf = if is_file_scope {
        canonical
            .parent()
            .map_or_else(|| canonical.clone(), Path::to_path_buf)
    } else {
        canonical
    };

    let mut graph_found: Option<(PathBuf, usize)> = None;
    let mut boundary: Option<PathBuf> = None;

    for depth in 0..MAX_ANCESTOR_DEPTH {
        // (a) Has the *current* directory been indexed?
        let graph_dir = current.join(".sqry").join("graph");
        let legacy_index = current.join(".sqry-index");
        if graph_found.is_none() && (graph_dir.is_dir() || legacy_index.exists()) {
            graph_found = Some((current.clone(), depth));
        }

        // (b) Is the *current* directory a project boundary?
        if boundary.is_none() && PROJECT_MARKERS.iter().any(|m| current.join(m).exists()) {
            boundary = Some(current.clone());
            // Project marker terminates the walk — even if no graph
            // was found, the project root is the boundary-of-record.
            break;
        }

        // (c) Walk up.
        if !current.pop() {
            break;
        }
    }

    match (graph_found, boundary) {
        (Some((root, depth)), Some(boundary_path)) => {
            if root.starts_with(&boundary_path) {
                WorkspaceRootDiscovery::GraphFound {
                    root,
                    boundary: boundary_path,
                    depth,
                    is_file_scope,
                }
            } else {
                // The graph is in an *outer* project (e.g. stray
                // `~/.sqry/graph` above `~/work/proj/Cargo.toml`).
                // Discard it — return BoundaryOnly so callers can
                // build a fresh index inside the project.
                WorkspaceRootDiscovery::BoundaryOnly {
                    boundary: boundary_path,
                    is_file_scope,
                }
            }
        }
        (Some((root, depth)), None) => {
            // Bare-directory legacy layout: no project marker, but a
            // graph was found. Treat the graph itself as the boundary
            // to preserve backward compatibility for marker-less
            // workspaces.
            WorkspaceRootDiscovery::GraphFound {
                boundary: root.clone(),
                root,
                depth,
                is_file_scope,
            }
        }
        (None, Some(boundary_path)) => WorkspaceRootDiscovery::BoundaryOnly {
            boundary: boundary_path,
            is_file_scope,
        },
        (None, None) => WorkspaceRootDiscovery::None,
    }
}

/// Error returned by [`assert_no_ancestor_graph`] when a nested
/// `.sqry/` would be created inside an outer project that already has
/// one. Rendered with the full recovery template (per
/// `E_p1_cluster.md` §E.3) so the user sees the offending path, the
/// ancestor's graph location, and the project boundary together.
#[derive(Debug, Clone, Error)]
pub enum NestedIndexError {
    #[error(
        "refusing to create a nested .sqry/ index.\n\
         An ancestor index already exists at: {ancestor_graph}\n\
         Requested location:                  {requested}\n\
         Project boundary detected at:        {boundary}\n\
         \n\
         If this is intentional (e.g. a sub-project with its own graph), \
         re-run with --allow-nested.\n\
         Otherwise: cd to the project root ({boundary}) and run \
         `sqry update` (incremental) or `sqry index --force` (rebuild).",
        ancestor_graph = ancestor_graph.display(),
        requested = requested.display(),
        boundary = boundary.display(),
    )]
    /// Nested-index pollution: a `.sqry/graph` already exists at an
    /// ancestor of `requested`, and they share the same project
    /// boundary. The recovery message identifies all three paths.
    AncestorExists {
        /// The path the caller asked to index (canonicalised).
        requested: PathBuf,
        /// The `.sqry/graph` directory belonging to the outer project.
        ancestor_graph: PathBuf,
        /// The first ancestor containing a [`PROJECT_MARKERS`] entry.
        boundary: PathBuf,
    },
}

/// Returns `Ok(())` iff creating a `.sqry/graph` under `requested` is
/// safe — i.e. there is no ancestor graph belonging to the same
/// project boundary, OR the caller passed `allow_nested = true`.
///
/// Called by `sqry-cli/src/commands/index.rs::run_index` and by
/// `FilesystemGraphProvider::acquire`'s
/// `MissingGraphPolicy::AutoBuildIfEnabled` branch (cluster-E
/// Layer-2).
///
/// The condition `requested.starts_with(&boundary)` is the
/// load-bearing predicate: a graph at `~/.sqry/graph` and a project
/// at `~/work/proj/.sqry/graph` do NOT share a boundary (the
/// project's `Cargo.toml` is at `~/work/proj`, not at `~`), so the
/// guard does not fire for the legitimate "different project" case.
/// It fires only for the "same project, nested location" case.
///
/// # Errors
///
/// Returns [`NestedIndexError::AncestorExists`] when an ancestor
/// graph belongs to the same project boundary as `requested`.
pub fn assert_no_ancestor_graph(
    requested: &Path,
    allow_nested: bool,
) -> Result<(), NestedIndexError> {
    if allow_nested {
        return Ok(());
    }
    // Canonicalise *before* delegating so a relative path like `.`
    // resolves to an absolute path. If `canonicalize` fails (path does
    // not exist yet), fall back to joining against the caller's cwd —
    // this preserves the "act on the caller's directory, not the
    // process's" intent for sub-process and test-harness invocations
    // where `current.canonicalize()` would walk into an unrelated
    // ancestor.
    let canonical_requested = canonicalise_or_join_cwd(requested);
    // Refuse to evaluate against a still-relative or empty path —
    // discover_workspace_root would walk `current.join(".sqry/graph")`
    // relative to the process cwd, producing the cluster-E §E.3
    // spurious-error mode reported on 2026-05-10.
    if !canonical_requested.is_absolute() || canonical_requested.as_os_str().is_empty() {
        return Ok(());
    }
    if let WorkspaceRootDiscovery::GraphFound { root, boundary, .. } =
        discover_workspace_root(&canonical_requested)
        && canonical_requested != root
        && canonical_requested.starts_with(&boundary)
    {
        return Err(NestedIndexError::AncestorExists {
            requested: canonical_requested,
            ancestor_graph: root.join(".sqry").join("graph"),
            boundary,
        });
    }
    Ok(())
}

/// Canonicalise `path`, falling back to `cwd.join(path)` (also
/// canonicalised) when the path does not exist on disk yet — the
/// `assert_no_ancestor_graph` caller is by definition about to create
/// the directory, so a strict canonicalise would always fail in the
/// happy path.
fn canonicalise_or_join_cwd(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_path_buf();
    };
    let joined = cwd.join(path);
    joined.canonicalize().unwrap_or(joined)
}

// ───────────────────────────────────────────────────────────────────────
// `WorkspaceCleanReport` types (sqry-mcp flakiness P1, cluster E
// foundation — `E_p1_cluster.md` §E.4 + Hand-off E4)
// ───────────────────────────────────────────────────────────────────────
//
// Public types for `sqry workspace clean`'s `--json` output. Stable
// across patch releases inside `schema_version 1`; additive changes
// only. Cluster-E Layer-2 owns the discovery + serialization logic;
// foundation only owns the type shape.

/// Top-level JSON shape produced by `sqry workspace clean --json`.
/// `schema_version 1` is the wire contract for this release; future
/// additive fields can be added without bumping the version, but a
/// breaking field rename or removal must increment to `schema_version 2`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceCleanReport {
    /// Always `1` for this release.
    pub schema_version: u32,
    /// Canonicalised root path the cleanup walked.
    pub root: PathBuf,
    /// The `.sqry/graph` at the project boundary for `root`, if any.
    /// Populated by [`discover_workspace_root`]; never auto-deleted
    /// without `--force`.
    pub canonical_active_artifact: Option<PathBuf>,
    /// Daemon-locked artifacts surfaced via the `daemon/active-artifacts`
    /// IPC method. Empty when the daemon is unreachable; the JSON
    /// envelope additionally surfaces a fallback warning in that
    /// case (cluster-E Layer-2 owns the warning shape).
    pub daemon_locked_artifacts: Vec<PathBuf>,
    /// Every artifact the cleanup walk found, classified.
    pub discovered: Vec<DiscoveredArtifact>,
    /// Subset of `discovered` that the policy filter is willing to
    /// remove (post `is_canonical_active` / `is_daemon_locked` /
    /// `is_user_state` filtering).
    pub planned_removals: Vec<PathBuf>,
    /// Discovered but not planned for removal, with the reason.
    pub skipped: Vec<SkippedArtifact>,
    /// Mirrors the `--apply` flag.
    pub applied: bool,
    /// Empty when `applied = false`; otherwise the actually-removed
    /// paths from `planned_removals` (subset on per-entry I/O error).
    pub removed: Vec<PathBuf>,
    /// Per-entry removal failures during `--apply`.
    pub errors: Vec<RemovalError>,
}

/// A single artifact discovered by the cleanup walk, with the
/// classification + size + freshness data needed by the dry-run
/// summary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredArtifact {
    /// Canonicalised absolute path.
    pub path: PathBuf,
    /// Classification of this artifact (graph / cache / user state / ...).
    pub kind: ArtifactKind,
    /// Sum of all files within (capped at 10 MiB sample for cache
    /// directories with millions of entries; see `E_p1_cluster.md`
    /// §E.4 step 5c).
    pub size_bytes: u64,
    /// Last-modified time of the artifact root (best-effort; `None`
    /// when the filesystem does not expose mtime).
    pub last_modified: Option<chrono::DateTime<chrono::Utc>>,
    /// `true` when this is the project's canonical
    /// `.sqry/graph` artifact — never auto-deleted.
    pub is_canonical_active: bool,
    /// `true` when the daemon currently has this artifact loaded
    /// (per the `daemon/active-artifacts` IPC method).
    pub is_daemon_locked: bool,
    /// `true` when this is `.sqry-index.user` — user-curated state
    /// hidden behind `--include-user-state`.
    pub is_user_state: bool,
}

/// Classification of a discovered artifact. The kind drives the
/// policy filter:
///
/// | Kind             | Default behaviour without flags         |
/// |------------------|-----------------------------------------|
/// | `Graph`          | Skipped if `is_canonical_active` (or `is_daemon_locked`) without `--force` |
/// | `GraphRoot`      | Same as `Graph`                          |
/// | `Cache`          | Removed                                  |
/// | `Prof`           | Removed                                  |
/// | `UserState`      | Skipped without `--include-user-state`   |
/// | `LegacyIndex`    | Removed                                  |
/// | `WorkspaceRegistry` | Always skipped (never auto-deleted)   |
/// | `NestedGraph`    | Removed unless `is_daemon_locked`        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ArtifactKind {
    /// `<root>/.sqry/graph` — canonical unified graph snapshot.
    Graph,
    /// `<root>/.sqry/` — parent of `Graph`. Listed separately so the
    /// dry-run can show "graph + cache + manifest" in one entry.
    GraphRoot,
    /// `<root>/.sqry-cache` — incremental indexer cache.
    Cache,
    /// `<root>/.sqry-prof` — profiler dumps (legacy / external).
    Prof,
    /// `<root>/.sqry-index.user` — user-curated state (aliases,
    /// recent queries). NEVER auto-deleted.
    UserState,
    /// `<root>/.sqry-index` — legacy v1 index marker file. Stale
    /// since v2.0.0; safe to delete unconditionally.
    LegacyIndex,
    /// `<root>/.sqry-workspace` — multi-repo registry. NEVER
    /// auto-deleted.
    WorkspaceRegistry,
    /// `.sqry/graph` discovered inside an outer project that already
    /// has its own canonical graph (E.3 nested-index pollution).
    NestedGraph,
}

/// Discovered but not planned for removal — `reason` carries the
/// policy verdict so the dry-run output can explain why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedArtifact {
    /// Canonicalised path of the skipped artifact.
    pub path: PathBuf,
    /// Why the policy filter skipped this entry.
    pub reason: SkipReason,
}

/// Why a discovered artifact was skipped from `planned_removals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SkipReason {
    /// Path equals the project's current canonical
    /// `.sqry/graph` artifact.
    CanonicalActive,
    /// Daemon currently has the artifact loaded.
    DaemonLocked,
    /// `.sqry-index.user` without `--include-user-state`.
    UserState,
    /// `.sqry-workspace` registry — never auto-deleted.
    WorkspaceRegistry,
    /// Symlink that the walker refused to follow.
    SymlinkRefused,
    /// Path canonicalised outside the cleanup `root`.
    OutsideRoot,
}

/// Per-entry removal failure (e.g. `EACCES` on `remove_dir_all`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemovalError {
    /// Canonicalised path the cleanup tried to remove.
    pub path: PathBuf,
    /// `Display` form of the underlying I/O error.
    pub error: String,
}

#[cfg(test)]
mod ancestor_tests {
    use super::*;
    use tempfile::TempDir;

    /// Sanity: discovery returns `None` for a deeply-nested empty
    /// hierarchy (no markers, no graphs).
    #[test]
    fn discover_returns_none_for_empty_hierarchy() {
        let tmp = TempDir::new().unwrap();
        let leaf = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&leaf).unwrap();
        let outcome = discover_workspace_root(&leaf);
        // Either None (filesystem root above tmp has no marker) or
        // BoundaryOnly (the filesystem root happens to host a
        // marker like `.git`). Both are acceptable here; the test
        // pins that GraphFound is NOT returned without a graph.
        assert!(
            !matches!(outcome, WorkspaceRootDiscovery::GraphFound { .. }),
            "no .sqry/graph above leaf, expected None or BoundaryOnly, got {outcome:?}"
        );
    }

    /// `Cargo.toml` at the project root halts the walk and the
    /// outcome is `BoundaryOnly` when no graph exists.
    #[test]
    fn discover_stops_at_cargo_toml_marker_with_no_graph() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub/deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        let outcome = discover_workspace_root(&sub);
        match outcome {
            WorkspaceRootDiscovery::BoundaryOnly { boundary, .. } => {
                assert_eq!(
                    boundary.canonicalize().unwrap(),
                    proj.canonicalize().unwrap(),
                    "boundary must equal proj root"
                );
            }
            other => panic!("expected BoundaryOnly, got {other:?}"),
        }
    }

    /// `.sqry/graph` inside the project boundary returns
    /// `GraphFound` with both fields populated.
    #[test]
    fn discover_returns_graph_found_when_graph_inside_boundary() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::create_dir_all(proj.join(".sqry").join("graph")).unwrap();
        let outcome = discover_workspace_root(&sub);
        match outcome {
            WorkspaceRootDiscovery::GraphFound { root, boundary, .. } => {
                assert_eq!(root.canonicalize().unwrap(), proj.canonicalize().unwrap());
                assert_eq!(
                    boundary.canonicalize().unwrap(),
                    proj.canonicalize().unwrap()
                );
            }
            other => panic!("expected GraphFound, got {other:?}"),
        }
    }

    /// Stray `~/.sqry/graph` outside the project boundary does NOT
    /// satisfy `GraphFound` — the project marker wins. (The exact
    /// reproducer from `E_p1_cluster.md` §E.1 "stray ~/.sqry/graph".)
    #[test]
    fn discover_discards_outer_graph_when_inner_marker_exists() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path();
        std::fs::create_dir_all(outer.join(".sqry").join("graph")).unwrap();
        let proj = outer.join("work/new-project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        let outcome = discover_workspace_root(&proj);
        match outcome {
            WorkspaceRootDiscovery::BoundaryOnly { boundary, .. } => {
                assert_eq!(
                    boundary.canonicalize().unwrap(),
                    proj.canonicalize().unwrap(),
                    "boundary should be the inner project root, not the outer stray graph"
                );
            }
            other => {
                panic!("outer-graph + inner-marker must collapse to BoundaryOnly, got {other:?}")
            }
        }
    }

    /// `assert_no_ancestor_graph(requested, false)` rejects nested
    /// `.sqry/graph` creation when the same project already has one.
    #[test]
    fn assert_no_ancestor_graph_rejects_nested_creation() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".sqry").join("graph")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        let nested = proj.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let err = assert_no_ancestor_graph(&nested, false)
            .expect_err("nested creation must error when ancestor graph exists");
        assert!(matches!(err, NestedIndexError::AncestorExists { .. }));
    }

    /// `allow_nested = true` bypasses the guard.
    #[test]
    fn assert_no_ancestor_graph_passes_with_allow_nested() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".sqry").join("graph")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        let nested = proj.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(assert_no_ancestor_graph(&nested, true).is_ok());
    }
}
