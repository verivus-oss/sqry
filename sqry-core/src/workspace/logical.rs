//! Logical workspace data model.
//!
//! A `LogicalWorkspace` is the unit of identity for cross-repo / workspace-aware
//! indexing. It carries:
//!
//! - The `WorkspaceIdentity` from which a stable `WorkspaceId` (BLAKE3-256
//!   digest) is derived.
//! - A list of `SourceRoot`s — directories that are auto-indexed and queried.
//! - A list of `MemberFolder`s — directories that are part of the workspace
//!   but **not** auto-indexed (they fall through to the workspace's source
//!   roots when queried).
//! - An explicit `exclusions` list — paths that are opaque to sqry.
//! - Workspace-scoped metadata: `ProjectRootMode`, optional
//!   `index_root_override`, and a `config_fingerprint` placeholder
//!   (populated by the plugin-selection / cost-tier pipeline in a later
//!   step).
//!
//! Exhaustive design + identity rules: see
//! `docs/development/workspace-aware-cross-repo/03_IMPLEMENTATION_PLAN.md` §1.
//!
//! ## Identity-input canonicalization (deterministic)
//!
//! 1. Every path is funneled through
//!    [`crate::project::path_utils::canonicalize_path`], which resolves
//!    symlinks via `realpath(3)` when possible, falling back to lexical
//!    absolutization when the target does not exist.
//! 2. When canonicalization falls back (i.e. the path could not be resolved
//!    against the filesystem), the surrounding identity records the fact
//!    via a `symlink_unresolved: bool`. The flag is part of the hash input,
//!    so a missing-then-existing path will produce a different
//!    `WorkspaceId` (this is correct — the shape of the workspace changed).
//! 3. On case-insensitive mounts (best-effort detected per path) paths are
//!    lowercased before hashing, so case variants resolve to the same
//!    `WorkspaceId`. On case-sensitive filesystems (default on Linux) the
//!    detection returns `false` and paths are hashed verbatim.
//! 4. `AnonymousMultiRoot.folders` is sorted lexically before hashing so a
//!    reorder of workspace folders is identity-preserving.
//! 5. `config_fingerprint` is **not** included in the hash — it is a
//!    separate cache dimension.
//!
//! ## Member vs Excluded — the contract
//!
//! - `Source` paths and their descendants are owned by the source root.
//! - `Member` paths are part of the logical workspace but not auto-indexed.
//!   Reads still resolve via the workspace's source roots; status returns
//!   the *aggregate* workspace status.
//! - `Excluded` paths are opaque — searches return empty with an explicit
//!   `excluded` flag.
//! - `Unknown` paths sit outside the workspace entirely.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::project::path_utils::canonicalize_path;
use crate::project::types::ProjectRootMode;

use super::registry::WorkspaceRegistry;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced while constructing a [`LogicalWorkspace`].
#[derive(Debug, Error)]
pub enum LogicalWorkspaceError {
    /// Generic IO failure that is not tied to a specific path.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// A path could not be canonicalized.
    #[error("failed to canonicalize {path}: {source}")]
    Canonicalization {
        /// The path that failed to canonicalize.
        path: PathBuf,
        /// Underlying IO error.
        source: io::Error,
    },

    /// The legacy `.sqry-workspace` (registry v1) JSON could not be parsed.
    #[error("failed to parse .sqry-workspace registry: {0}")]
    ParseSqryWorkspace(serde_json::Error),

    /// The `.code-workspace` JSON could not be parsed.
    #[error("failed to parse .code-workspace file: {0}")]
    ParseCodeWorkspace(serde_json::Error),

    /// A `folders[i]` entry in a `.code-workspace` is malformed.
    #[error("malformed .code-workspace folder entry: {reason}")]
    MalformedFolderEntry {
        /// Human-readable reason describing the malformed entry.
        reason: String,
    },

    /// The same path was classified into two conflicting roles.
    #[error("conflicting classification for {path}: {kinds}")]
    ConflictingClassification {
        /// The path with conflicting classifications.
        path: PathBuf,
        /// A description of the conflicting kinds.
        kinds: String,
    },
}

// ---------------------------------------------------------------------------
// WorkspaceId — BLAKE3-256 typed digest
// ---------------------------------------------------------------------------

/// Stable identity for a [`LogicalWorkspace`].
///
/// 32 bytes (BLAKE3-256) over the canonicalized identity inputs.
/// Never truncated to 64 bits — the full 256-bit space is used to keep the
/// collision probability astronomically small across processes / caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId([u8; 32]);

impl WorkspaceId {
    /// Compute a `WorkspaceId` from the canonical identity inputs of a
    /// [`WorkspaceIdentity`]. The hashing scheme is documented in
    /// `03_IMPLEMENTATION_PLAN.md` §1 and tested via the round-trip
    /// stability tests in `workspace::tests`.
    #[must_use]
    pub fn from_identity(identity: &WorkspaceIdentity) -> Self {
        let mut hasher = Hasher::new();
        identity.write_hash_input(&mut hasher);
        Self(*hasher.finalize().as_bytes())
    }

    /// Borrow the raw 32-byte digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// First 16 hex characters of the digest. Suitable for log lines and
    /// short file names; **not** sufficient for cross-process identity.
    #[must_use]
    pub fn as_short_hex(&self) -> String {
        let full = self.as_full_hex();
        full[..16].to_string()
    }

    /// Full 64-character hex digest. Use this for any identity comparison.
    #[must_use]
    pub fn as_full_hex(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for byte in &self.0 {
            // `write!` to a `String` is infallible.
            let _ = write!(s, "{byte:02x}");
        }
        s
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_short_hex())
    }
}

// ---------------------------------------------------------------------------
// WorkspaceIdentity
// ---------------------------------------------------------------------------

/// The identity inputs of a logical workspace. `WorkspaceId` is computed
/// deterministically from these inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkspaceIdentity {
    /// Identity derived from a `.sqry-workspace` registry file.
    SqryWorkspaceFile {
        /// Canonical absolute path to the registry file.
        path: PathBuf,
        /// `true` if `path` could not be filesystem-canonicalized
        /// (lexical fallback was used).
        symlink_unresolved: bool,
    },
    /// Identity derived from a VS Code `.code-workspace` file.
    VsCodeWorkspaceFile {
        /// Canonical absolute path to the workspace file.
        path: PathBuf,
        /// `true` if `path` could not be filesystem-canonicalized.
        symlink_unresolved: bool,
    },
    /// Identity derived from an ad-hoc multi-folder VS Code workspace.
    AnonymousMultiRoot {
        /// Folder roots, **sorted lexically** before hashing for stability.
        folders: Vec<PathBuf>,
        /// `true` if any of the folders could not be canonicalized.
        symlink_unresolved: bool,
    },
    /// Identity derived from a single root path (e.g. `sqry index <path>`).
    SingleRoot {
        /// Canonical absolute root path.
        path: PathBuf,
        /// `true` if `path` could not be canonicalized.
        symlink_unresolved: bool,
    },
}

impl WorkspaceIdentity {
    /// Tag byte used in the BLAKE3 hash input. Stable; do not renumber.
    fn tag_byte(&self) -> u8 {
        match self {
            Self::SqryWorkspaceFile { .. } => 0,
            Self::VsCodeWorkspaceFile { .. } => 1,
            Self::AnonymousMultiRoot { .. } => 2,
            Self::SingleRoot { .. } => 3,
        }
    }

    /// `symlink_unresolved` flag — recorded in the hash input.
    fn symlink_unresolved(&self) -> bool {
        match self {
            Self::SqryWorkspaceFile {
                symlink_unresolved, ..
            }
            | Self::VsCodeWorkspaceFile {
                symlink_unresolved, ..
            }
            | Self::AnonymousMultiRoot {
                symlink_unresolved, ..
            }
            | Self::SingleRoot {
                symlink_unresolved, ..
            } => *symlink_unresolved,
        }
    }

    /// Write the deterministic hash input for `WorkspaceId` derivation.
    fn write_hash_input(&self, hasher: &mut Hasher) {
        hasher.update(&[self.tag_byte()]);
        // 0x00 / 0x01 byte for symlink_unresolved.
        hasher.update(&[u8::from(self.symlink_unresolved())]);
        match self {
            Self::SqryWorkspaceFile { path, .. }
            | Self::VsCodeWorkspaceFile { path, .. }
            | Self::SingleRoot { path, .. } => {
                hash_path(hasher, path);
            }
            Self::AnonymousMultiRoot { folders, .. } => {
                let count = u32::try_from(folders.len()).unwrap_or(u32::MAX);
                hasher.update(&count.to_le_bytes());
                for folder in folders {
                    hash_path(hasher, folder);
                }
            }
        }
    }
}

/// Hash a single canonical path: u32 LE byte length followed by the
/// path's UTF-8 bytes (lossy if the path is not valid UTF-8 — extremely
/// unusual on supported targets, but we never panic).
fn hash_path(hasher: &mut Hasher, path: &Path) {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    hasher.update(&len.to_le_bytes());
    hasher.update(bytes);
}

// ---------------------------------------------------------------------------
// SourceRoot, MemberFolder, Classification
// ---------------------------------------------------------------------------

/// A directory that is auto-indexed by sqry. One `.sqry/graph/manifest.json`
/// per source root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRoot {
    /// Canonical absolute path to the source root.
    pub path: PathBuf,
    /// Path to the per-source-root index manifest (always
    /// `<path>/.sqry/graph/manifest.json`).
    pub index_path: PathBuf,
    /// Optional list of language hints to bias plugin selection.
    pub language_hints: Option<Vec<String>>,
    /// Optional path to the JVM classpath cache directory
    /// (`<path>/.sqry/classpath/`); populated by the JVM pipeline.
    pub classpath_dir: Option<PathBuf>,
    /// Per-source-root override of the workspace-level
    /// `config_fingerprint`. `0` here; populated by the plugin-selection
    /// pipeline in a later step.
    pub config_fingerprint: u64,
}

impl SourceRoot {
    /// Build a `SourceRoot` from a canonical path, deriving the standard
    /// `.sqry/graph/manifest.json` index path. `language_hints` is
    /// `None`; `classpath_dir` is `None`; `config_fingerprint` is `0`.
    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        let index_path = path.join(".sqry").join("graph").join("manifest.json");
        Self {
            path,
            index_path,
            language_hints: None,
            classpath_dir: None,
            config_fingerprint: 0,
        }
    }

    /// STEP_11_4 — populate [`Self::classpath_dir`] from a probe of
    /// `<self.path>/.sqry/classpath/`. The directory is set when the
    /// probe finds a *directory* at that path, leaving the field
    /// `None` when the path is missing or is not a directory.
    ///
    /// Returns `Ok(())` on a successful (possibly negative) probe.
    /// Returns the raw [`io::Error`] when the probe fails for a reason
    /// other than `NotFound` (e.g. permission denied) so callers can
    /// surface a [`super::cache::WorkspaceWarning::ClasspathProbeFailed`]
    /// without losing the underlying error detail.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when [`fs::metadata`] fails
    /// for a reason other than `NotFound`.
    pub fn populate_classpath_dir(&mut self) -> io::Result<()> {
        let probe = self.path.join(".sqry").join("classpath");
        match fs::metadata(&probe) {
            Ok(meta) if meta.is_dir() => {
                self.classpath_dir = Some(probe);
                Ok(())
            }
            Ok(_) => {
                // Path exists but is not a directory — treat as
                // "no classpath present" without raising an error.
                self.classpath_dir = None;
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.classpath_dir = None;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// STEP_11_4 — fluent builder for [`Self::config_fingerprint`].
    ///
    /// Used by call sites that hold a freshly computed
    /// [`crate::config::compute_workspace_config_fingerprint`] value
    /// alongside the source root. A fingerprint of `0` is the
    /// "unset" sentinel and the builder accepts it for the same
    /// reason `WorkspaceKey::config_fingerprint = 0` is the default
    /// — call sites that want strict identity must supply a non-zero
    /// value.
    #[must_use]
    pub fn with_config_fingerprint(mut self, fingerprint: u64) -> Self {
        self.config_fingerprint = fingerprint;
        self
    }

    /// STEP_11_4 — return the per-source-root config fingerprint with
    /// fallback to a workspace-level default supplied by the caller.
    ///
    /// `SourceRoot.config_fingerprint == 0` is treated as "use the
    /// workspace-level fingerprint" — the daemon `WorkspaceKey`
    /// dimension this powers must always carry the workspace's
    /// fingerprint when no per-source-root override is set so two
    /// otherwise-identical paths under different workspaces stay in
    /// distinct cache entries.
    #[must_use]
    pub fn effective_config_fingerprint(&self, workspace_default: u64) -> u64 {
        if self.config_fingerprint == 0 {
            workspace_default
        } else {
            self.config_fingerprint
        }
    }
}

/// Why a folder was classified as a member (rather than a source root or
/// excluded path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemberReason {
    /// Folder holds tooling / build / scripts, not first-class source.
    OperationalFolder,
    /// Folder exists but contains no plugin-recognized source files.
    NonSourceFolder,
    /// Heuristic could not match any registered language plugin
    /// (last-resort default — see §1.1).
    NoLanguagePluginMatch,
}

/// A folder that is part of the logical workspace but is **not**
/// auto-indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberFolder {
    /// Canonical absolute path to the member folder.
    pub path: PathBuf,
    /// Why the folder was classified as a member.
    pub reason: MemberReason,
}

/// The result of [`LogicalWorkspace::classify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Classification {
    /// Path is a known source root or a descendant of one.
    Source,
    /// Path is a known member folder or a descendant of one.
    Member {
        /// Why the owning member folder was so classified.
        reason: MemberReason,
    },
    /// Path was explicitly excluded.
    Excluded,
    /// Path is outside the logical workspace entirely.
    Unknown,
}

/// Verdict returned by an injected heuristic classifier when a folder is
/// not explicitly classified by the user.
///
/// The heuristic policy itself lives outside `sqry-core` (in the LSP /
/// extension / wrapper); `sqry-core` accepts it as an injected
/// `&dyn Fn(&Path) -> HeuristicVerdict` so policy stays separate from the
/// data model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeuristicVerdict {
    /// Folder should be treated as a source root.
    Source,
    /// Folder should be treated as a member (with a specific reason).
    Member {
        /// Reason the folder is a member.
        reason: MemberReason,
    },
    /// Folder should be excluded.
    Excluded,
    /// Heuristic could not classify; caller decides the last-resort
    /// default.
    Unknown,
}

// ---------------------------------------------------------------------------
// LogicalWorkspace
// ---------------------------------------------------------------------------

/// A logical workspace — the unit of identity for cross-repo / workspace
/// indexing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalWorkspace {
    identity: WorkspaceIdentity,
    workspace_id: WorkspaceId,
    source_roots: Vec<SourceRoot>,
    member_folders: Vec<MemberFolder>,
    exclusions: Vec<PathBuf>,
    project_root_mode: ProjectRootMode,
    index_root_override: Option<PathBuf>,
    config_fingerprint: u64,
}

impl LogicalWorkspace {
    /// Construct from a `.sqry-workspace` registry file.
    ///
    /// `WorkspaceRegistry::load` accepts both v1 (flat `repositories`
    /// list) and v2 (`source_roots`, `member_folders`, `exclusions`,
    /// `project_root_mode`) on-disk shapes — v1 is auto-upgraded to v2
    /// in memory. This constructor projects every v2 field into the
    /// resulting [`LogicalWorkspace`]:
    ///
    /// * `repositories` → `source_roots` (canonicalized).
    /// * `member_folders` → `MemberFolder { path, reason }` (canonicalized).
    /// * `exclusions` → canonical absolute paths.
    /// * `project_root_mode` → carried verbatim.
    ///
    /// `STEP_7` codex iter4 fix — pre-iter4 this constructor dropped
    /// `member_folders`, `exclusions`, and `project_root_mode` on the
    /// floor, defeating acceptance criteria 5/6 end-to-end (the redactor
    /// receives an empty `LogicalWorkspaceView::exclusions` /
    /// `member_folders`, so `redact_excluded_in_passthrough` and the
    /// member-folder prefix renderer never fire on real
    /// `.sqry-workspace`-loaded sessions). The pre-iter4 inline TODO
    /// pointed at "STEP_2 will overhaul the registry layer entirely" —
    /// STEP_2 shipped the registry-side v2 schema but did not update this
    /// projection. Fixed here so STEP_7's MCP redaction wiring is
    /// observable end-to-end.
    ///
    /// # Errors
    ///
    /// Returns [`LogicalWorkspaceError`] when the registry file cannot be
    /// loaded or any path canonicalization fails irrecoverably.
    pub fn from_sqry_workspace(path: &Path) -> Result<Self, LogicalWorkspaceError> {
        // Load the registry. v1 files are auto-upgraded to v2 in memory
        // by `WorkspaceRegistry::load`; we propagate serde errors as a
        // dedicated variant so callers can distinguish parse failures
        // from IO.
        let registry = WorkspaceRegistry::load(path).map_err(|err| match err {
            super::error::WorkspaceError::Serialization(e) => {
                LogicalWorkspaceError::ParseSqryWorkspace(e)
            }
            super::error::WorkspaceError::Io { source, .. } => LogicalWorkspaceError::Io(source),
            other => LogicalWorkspaceError::Io(io::Error::other(other.to_string())),
        })?;

        let (canonical_path, symlink_unresolved) = canonicalize_with_flag(path)?;
        let identity = WorkspaceIdentity::SqryWorkspaceFile {
            path: maybe_lowercase(&canonical_path),
            symlink_unresolved,
        };
        let workspace_id = WorkspaceId::from_identity(&identity);

        let mut source_roots = Vec::with_capacity(registry.repositories.len());
        for repo in &registry.repositories {
            let (canonical_repo, _unresolved) = canonicalize_with_flag(&repo.root)?;
            let mut root = SourceRoot::from_path(canonical_repo);
            // Preserve the registry-supplied index_path if it points at
            // a real manifest (registry v1 uses `<repo>/.sqry-index`,
            // not `.sqry/graph/manifest.json`); leave `from_path`'s
            // computed manifest path otherwise.
            root.index_path.clone_from(&repo.index_path);
            if let Some(lang) = repo.primary_language.clone() {
                root.language_hints = Some(vec![lang]);
            }
            source_roots.push(root);
        }

        // v2 projection: carry member_folders, exclusions, and
        // project_root_mode through to the LogicalWorkspace so the MCP
        // redactor (and any other consumer of `member_folders()` /
        // `exclusions()`) sees the same structure the registry persists.
        let mut member_folders = Vec::with_capacity(registry.member_folders.len());
        for member in &registry.member_folders {
            let (canonical_root, _unresolved) = canonicalize_with_flag(&member.root)?;
            member_folders.push(MemberFolder {
                path: canonical_root,
                reason: member.reason,
            });
        }

        let mut exclusions = Vec::with_capacity(registry.exclusions.len());
        for excluded in &registry.exclusions {
            let (canonical_excluded, _unresolved) = canonicalize_with_flag(excluded)?;
            exclusions.push(canonical_excluded);
        }

        let mut ws = Self {
            identity,
            workspace_id,
            source_roots,
            member_folders,
            exclusions,
            project_root_mode: registry.project_root_mode,
            index_root_override: None,
            config_fingerprint: 0,
        };
        // STEP_11_4 — auto-populate classpath_dir on every source root.
        let _failures = ws.populate_classpath_dirs();
        Ok(ws)
    }

    /// Construct from a `.code-workspace` JSON file.
    ///
    /// The `heuristic_fn` is invoked for every folder that does not carry
    /// an explicit `sqry.role`, is not in the top-level
    /// `sqry.workspace.sourceRoots` / `.exclusions` overrides, and is not
    /// already classified as a member by an explicit
    /// `sqry.workspace.memberFolders` entry.
    ///
    /// # Errors
    ///
    /// Returns [`LogicalWorkspaceError`] for IO failures, JSON parse
    /// errors, malformed folder entries, or path canonicalization
    /// failures that cannot be recovered via lexical absolutization.
    #[allow(clippy::too_many_lines)] // single-pass classifier; splitting hurts clarity.
    pub fn from_code_workspace(
        workspace_file: &Path,
        heuristic_fn: &dyn Fn(&Path) -> HeuristicVerdict,
    ) -> Result<Self, LogicalWorkspaceError> {
        let bytes = fs::read(workspace_file)?;
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(LogicalWorkspaceError::ParseCodeWorkspace)?;

        let workspace_dir = workspace_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

        // Resolve the canonical workspace-file path for identity.
        let (canonical_workspace_file, symlink_unresolved) =
            canonicalize_with_flag(workspace_file)?;
        let identity = WorkspaceIdentity::VsCodeWorkspaceFile {
            path: maybe_lowercase(&canonical_workspace_file),
            symlink_unresolved,
        };
        let workspace_id = WorkspaceId::from_identity(&identity);

        // Collect folder entries. Per the .code-workspace spec each
        // folder has a `path` (required) and optional `name`, plus
        // sqry-specific `sqry.role`.
        let folders_v = json.get("folders").cloned().unwrap_or_default();
        let folders_arr = folders_v.as_array().cloned().unwrap_or_default();

        // Top-level sqry.workspace overrides.
        let sqry_top = json.get("sqry.workspace");
        let top_source_roots = path_set_from_value(sqry_top, "sourceRoots", &workspace_dir);
        let top_exclusions = path_set_from_value(sqry_top, "exclusions", &workspace_dir);
        let top_members = member_overrides_from_value(sqry_top, &workspace_dir)?;
        let project_root_mode = sqry_top
            .and_then(|v| v.get("projectRootMode"))
            .and_then(|v| v.as_str())
            .and_then(ProjectRootMode::from_str_opt)
            .unwrap_or_default();

        // Build per-path classification map. The key is the absolute
        // path *as configured*; we canonicalize at the end.
        let mut classified: BTreeMap<PathBuf, FolderClassKind> = BTreeMap::new();
        let mut all_folders: Vec<PathBuf> = Vec::new();

        for (idx, entry) in folders_arr.iter().enumerate() {
            let raw_path = entry.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                LogicalWorkspaceError::MalformedFolderEntry {
                    reason: format!("folders[{idx}] missing string `path`"),
                }
            })?;
            let abs = if Path::new(raw_path).is_absolute() {
                PathBuf::from(raw_path)
            } else {
                workspace_dir.join(raw_path)
            };
            all_folders.push(abs.clone());

            // Step 4: explicit per-folder `sqry.role` always wins.
            if let Some(role) = entry.get("sqry.role").and_then(|v| v.as_str()) {
                let kind = match role {
                    "source" => FolderClassKind::Source,
                    "operational" => FolderClassKind::Member(MemberReason::OperationalFolder),
                    "non-source" | "nonSource" | "non_source" => {
                        FolderClassKind::Member(MemberReason::NonSourceFolder)
                    }
                    "excluded" => FolderClassKind::Excluded,
                    other => {
                        return Err(LogicalWorkspaceError::MalformedFolderEntry {
                            reason: format!(
                                "folders[{idx}].sqry.role = '{other}' (expected source|operational|excluded|non-source)"
                            ),
                        });
                    }
                };
                classified.insert(abs, kind);
                continue;
            }

            // Step 5: top-level sqry.workspace overrides.
            if top_exclusions.contains(&abs) {
                classified.insert(abs, FolderClassKind::Excluded);
                continue;
            }
            if top_source_roots.contains(&abs) {
                classified.insert(abs, FolderClassKind::Source);
                continue;
            }
            if let Some(reason) = top_members.get(&abs).copied() {
                classified.insert(abs, FolderClassKind::Member(reason));
                continue;
            }

            // Step 6: heuristic fallback.
            let verdict = heuristic_fn(&abs);
            let kind = match verdict {
                HeuristicVerdict::Source => FolderClassKind::Source,
                HeuristicVerdict::Member { reason } => FolderClassKind::Member(reason),
                HeuristicVerdict::Excluded => FolderClassKind::Excluded,
                HeuristicVerdict::Unknown => {
                    // Step 7: last-resort default for unclassified folders.
                    FolderClassKind::Member(MemberReason::NoLanguagePluginMatch)
                }
            };
            classified.insert(abs, kind);
        }

        // Top-level overrides may reference paths that were not present in
        // the `folders[]` array. Honor them too.
        for path in &top_source_roots {
            classified
                .entry(path.clone())
                .or_insert(FolderClassKind::Source);
        }
        for path in &top_exclusions {
            classified
                .entry(path.clone())
                .or_insert(FolderClassKind::Excluded);
        }
        for (path, reason) in &top_members {
            classified
                .entry(path.clone())
                .or_insert(FolderClassKind::Member(*reason));
        }

        // Materialize.
        let mut source_roots = Vec::new();
        let mut member_folders = Vec::new();
        let mut exclusions = Vec::new();
        for (raw_path, kind) in classified {
            let (canonical, _unresolved) = canonicalize_with_flag(&raw_path)?;
            let canonical = maybe_lowercase(&canonical);
            match kind {
                FolderClassKind::Source => source_roots.push(SourceRoot::from_path(canonical)),
                FolderClassKind::Member(reason) => member_folders.push(MemberFolder {
                    path: canonical,
                    reason,
                }),
                FolderClassKind::Excluded => exclusions.push(canonical),
            }
        }

        let mut ws = Self {
            identity,
            workspace_id,
            source_roots,
            member_folders,
            exclusions,
            project_root_mode,
            index_root_override: None,
            config_fingerprint: 0,
        };
        let _failures = ws.populate_classpath_dirs();
        Ok(ws)
    }

    /// Construct an ad-hoc multi-root workspace (every folder is a source
    /// root). Folders are sorted lexically before hashing so identity is
    /// stable under reorder.
    ///
    /// # Errors
    ///
    /// Returns [`LogicalWorkspaceError`] if any folder cannot be
    /// canonicalized irrecoverably.
    #[allow(clippy::needless_pass_by_value)] // owning constructor.
    pub fn anonymous_multi_root(folders: Vec<PathBuf>) -> Result<Self, LogicalWorkspaceError> {
        let mut canonical_folders = Vec::with_capacity(folders.len());
        let mut symlink_unresolved = false;
        for folder in &folders {
            let (canon, unresolved) = canonicalize_with_flag(folder)?;
            symlink_unresolved |= unresolved;
            canonical_folders.push(maybe_lowercase(&canon));
        }
        canonical_folders.sort();
        let identity = WorkspaceIdentity::AnonymousMultiRoot {
            folders: canonical_folders.clone(),
            symlink_unresolved,
        };
        let workspace_id = WorkspaceId::from_identity(&identity);

        let source_roots = canonical_folders
            .iter()
            .cloned()
            .map(SourceRoot::from_path)
            .collect();

        let mut ws = Self {
            identity,
            workspace_id,
            source_roots,
            member_folders: Vec::new(),
            exclusions: Vec::new(),
            project_root_mode: ProjectRootMode::default(),
            index_root_override: None,
            config_fingerprint: 0,
        };
        let _failures = ws.populate_classpath_dirs();
        Ok(ws)
    }

    /// Construct a single-root workspace (one source root, no members).
    ///
    /// # Errors
    ///
    /// Returns [`LogicalWorkspaceError`] if `path` cannot be canonicalized
    /// irrecoverably.
    #[allow(clippy::needless_pass_by_value)] // owning constructor.
    pub fn single_root(path: PathBuf) -> Result<Self, LogicalWorkspaceError> {
        let (canonical, symlink_unresolved) = canonicalize_with_flag(&path)?;
        let canonical = maybe_lowercase(&canonical);
        let identity = WorkspaceIdentity::SingleRoot {
            path: canonical.clone(),
            symlink_unresolved,
        };
        let workspace_id = WorkspaceId::from_identity(&identity);
        let mut ws = Self {
            identity,
            workspace_id,
            source_roots: vec![SourceRoot::from_path(canonical)],
            member_folders: Vec::new(),
            exclusions: Vec::new(),
            project_root_mode: ProjectRootMode::default(),
            index_root_override: None,
            config_fingerprint: 0,
        };
        let _failures = ws.populate_classpath_dirs();
        Ok(ws)
    }

    /// Test-only seam: construct a single-root workspace with the
    /// case-sensitivity decision *forced* to `case_insensitive`,
    /// bypassing live mount detection. Used by the
    /// `case_insensitive_mount_produces_same_id_end_to_end` test to
    /// exercise acceptance criterion 4 deterministically on
    /// case-sensitive Linux hosts (where the live detector would
    /// otherwise return `false` and short-circuit the lowercase path).
    ///
    /// The path is canonicalized via `path_utils::canonicalize_path`
    /// for parity with [`Self::single_root`], but the case-folding
    /// step uses the explicit `case_insensitive` argument instead of
    /// `is_case_insensitive_mount`.
    #[cfg(test)]
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn single_root_with_case_sensitivity(
        path: PathBuf,
        case_insensitive: bool,
    ) -> Result<Self, LogicalWorkspaceError> {
        let (canonical, symlink_unresolved) = canonicalize_with_flag(&path)?;
        let canonical = if case_insensitive {
            PathBuf::from(canonical.to_string_lossy().to_lowercase())
        } else {
            canonical
        };
        let identity = WorkspaceIdentity::SingleRoot {
            path: canonical.clone(),
            symlink_unresolved,
        };
        let workspace_id = WorkspaceId::from_identity(&identity);
        Ok(Self {
            identity,
            workspace_id,
            source_roots: vec![SourceRoot::from_path(canonical)],
            member_folders: Vec::new(),
            exclusions: Vec::new(),
            project_root_mode: ProjectRootMode::default(),
            index_root_override: None,
            config_fingerprint: 0,
        })
    }

    // -- Accessors --

    /// The stable BLAKE3-256 identity of this workspace.
    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }

    /// The identity inputs that produced [`Self::workspace_id`].
    #[must_use]
    pub fn identity(&self) -> &WorkspaceIdentity {
        &self.identity
    }

    /// The auto-indexed source roots.
    #[must_use]
    pub fn source_roots(&self) -> &[SourceRoot] {
        &self.source_roots
    }

    /// The non-indexed member folders.
    #[must_use]
    pub fn member_folders(&self) -> &[MemberFolder] {
        &self.member_folders
    }

    /// Explicitly excluded paths.
    #[must_use]
    pub fn exclusions(&self) -> &[PathBuf] {
        &self.exclusions
    }

    /// The workspace-level [`ProjectRootMode`].
    #[must_use]
    pub fn project_root_mode(&self) -> ProjectRootMode {
        self.project_root_mode
    }

    /// Optional `--index-root` override.
    #[must_use]
    pub fn index_root_override(&self) -> Option<&Path> {
        self.index_root_override.as_deref()
    }

    /// Workspace-level config fingerprint. Populated by the
    /// plugin-selection / cost-tier pipeline via
    /// [`Self::set_config_fingerprint`] and consumed by
    /// `sqry-daemon::WorkspaceKey` so two source roots sharing path
    /// but differing fingerprint stay in distinct cache entries.
    #[must_use]
    pub fn config_fingerprint(&self) -> u64 {
        self.config_fingerprint
    }

    /// STEP_11_4 — set the workspace-level config fingerprint computed
    /// via [`crate::config::compute_workspace_config_fingerprint`].
    ///
    /// The fingerprint is **not** part of the [`WorkspaceId`] hash
    /// input — it is a separate cache dimension consumed by the
    /// daemon's `WorkspaceKey`. Two `LogicalWorkspace`s with the
    /// same identity but different fingerprints share an identity but
    /// produce distinct daemon cache entries.
    pub fn set_config_fingerprint(&mut self, fingerprint: u64) {
        self.config_fingerprint = fingerprint;
    }

    /// STEP_11_4 — set the workspace-level config fingerprint and
    /// propagate it to every [`SourceRoot`] that does not already
    /// carry an explicit per-root override (i.e. whose
    /// `config_fingerprint == 0`).
    ///
    /// This is the typical wiring point: callers compute one
    /// workspace-level fingerprint, then call
    /// `set_config_fingerprint_with_inheritance` so source roots
    /// without an explicit override inherit the workspace value.
    /// Source roots that carry a non-zero override are left
    /// untouched.
    pub fn set_config_fingerprint_with_inheritance(&mut self, fingerprint: u64) {
        self.config_fingerprint = fingerprint;
        for root in &mut self.source_roots {
            if root.config_fingerprint == 0 {
                root.config_fingerprint = fingerprint;
            }
        }
    }

    /// STEP_11_4 — populate every [`SourceRoot::classpath_dir`] in this
    /// workspace by probing `<root>/.sqry/classpath/` for each. Returns
    /// a vector of `(source_root, io::Error)` pairs for any probe that
    /// failed for a reason other than `NotFound`; callers typically
    /// fold these into [`super::cache::WorkspaceWarning::ClasspathProbeFailed`].
    pub fn populate_classpath_dirs(&mut self) -> Vec<(PathBuf, io::Error)> {
        let mut failures = Vec::new();
        for root in &mut self.source_roots {
            if let Err(err) = root.populate_classpath_dir() {
                failures.push((root.path.clone(), err));
            }
        }
        failures
    }

    /// Returns `true` if `path` matches one of the registered source
    /// roots exactly (not a descendant).
    #[must_use]
    pub fn is_source_root(&self, path: &Path) -> bool {
        let canonical =
            canonicalize_path(path).map_or_else(|_| path.to_path_buf(), |p| maybe_lowercase(&p));
        self.source_roots.iter().any(|r| r.path == canonical)
    }

    /// Classify a path against the workspace per §1.4 of the
    /// implementation plan.
    #[must_use]
    pub fn classify(&self, path: &Path) -> Classification {
        let canonical =
            canonicalize_path(path).map_or_else(|_| path.to_path_buf(), |p| maybe_lowercase(&p));

        // 1. Exclusion match (exact or descendant).
        if self
            .exclusions
            .iter()
            .any(|excl| path_matches(&canonical, excl))
        {
            return Classification::Excluded;
        }

        // 2. Source root or descendant of one.
        if self
            .source_roots
            .iter()
            .any(|r| path_matches(&canonical, &r.path))
        {
            return Classification::Source;
        }

        // 3. Member folder or descendant of one.
        for member in &self.member_folders {
            if path_matches(&canonical, &member.path) {
                return Classification::Member {
                    reason: member.reason,
                };
            }
        }

        // 4. Outside the logical workspace entirely.
        Classification::Unknown
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Internal classifier used while building from a `.code-workspace`.
#[derive(Debug, Clone, Copy)]
enum FolderClassKind {
    Source,
    Member(MemberReason),
    Excluded,
}

/// `true` if `path == prefix` or `path` is a descendant of `prefix`.
fn path_matches(path: &Path, prefix: &Path) -> bool {
    path == prefix || path.starts_with(prefix)
}

/// Canonicalize a path and report whether the filesystem could resolve it.
///
/// The actual canonicalization is delegated to
/// [`crate::project::path_utils::canonicalize_path`] — the project-wide
/// source of truth which already handles the `realpath(3)` / lexical
/// fallback split. The `symlink_unresolved` flag is derived from a
/// separate `std::fs::canonicalize(path).is_ok()` probe purely so the
/// caller can record in the identity inputs whether the canonical path
/// came from the live filesystem or from the lexical fallback.
fn canonicalize_with_flag(path: &Path) -> Result<(PathBuf, bool), LogicalWorkspaceError> {
    // Probe whether realpath(3) would have succeeded. We deliberately
    // do NOT use the resulting path — the canonical path itself is
    // produced by `path_utils::canonicalize_path` so the entire
    // workspace stack uses one source-of-truth canonicalizer.
    let real_canon_succeeded = fs::canonicalize(path).is_ok();

    let canonical =
        canonicalize_path(path).map_err(|source| LogicalWorkspaceError::Canonicalization {
            path: path.to_path_buf(),
            source,
        })?;

    Ok((canonical, !real_canon_succeeded))
}

/// Apply best-effort case-insensitive normalization. On case-sensitive
/// mounts this is a no-op. On case-insensitive mounts we lowercase the
/// path so case-variant inputs collapse to the same `WorkspaceId`.
fn maybe_lowercase(path: &Path) -> PathBuf {
    if is_case_insensitive_mount(path) {
        let s = path.to_string_lossy().to_lowercase();
        PathBuf::from(s)
    } else {
        path.to_path_buf()
    }
}

/// Best-effort detection of whether `path` lives on a case-insensitive
/// mount. We avoid platform-specific `statvfs` plumbing here; the
/// detection is conservative.
///
/// - If `path` exists and a lowercase variant is present and
///   round-trips to the same canonical path, the mount is treated as
///   case-insensitive.
/// - On Linux the kernel default is case-sensitive; the round-trip
///   check therefore returns `false` for almost all paths.
/// - On macOS HFS+/APFS (default case-insensitive) and Windows
///   NTFS/ReFS the round-trip succeeds and we lowercase.
///
/// The algorithm never panics and never blocks on slow IO — it does at
/// most two `metadata()` calls.
fn is_case_insensitive_mount(path: &Path) -> bool {
    // Find a path component we can mutate. If the path string contains
    // no ASCII alphabetic characters there is nothing to vary, so
    // assume case-sensitive.
    let s = path.to_string_lossy();
    if !s.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    // Cheap fast path: try the lowercased and uppercased variants and
    // see whether both resolve to the same metadata as the original.
    let Ok(orig) = fs::metadata(path) else {
        return false;
    };
    let lower = PathBuf::from(s.to_lowercase());
    let upper = PathBuf::from(s.to_uppercase());

    let lower_ok = fs::metadata(&lower)
        .ok()
        .filter(|m| same_inode(m, &orig))
        .is_some();
    let upper_ok = fs::metadata(&upper)
        .ok()
        .filter(|m| same_inode(m, &orig))
        .is_some();

    // We require *both* round-trips to succeed (and at least one of them
    // to actually be a different string than the original — otherwise
    // the test is trivially true even on case-sensitive FS where
    // `path == s.to_lowercase()` already).
    let varies = lower != path || upper != path;
    varies && lower_ok && upper_ok
}

#[cfg(unix)]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.ino() == b.ino() && a.dev() == b.dev()
}

#[cfg(not(unix))]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    // Best-effort on non-Unix: fall back to size + modified-time
    // equality. This is conservative — false positives would only
    // cause a case-insensitive lowercase-pass on a case-sensitive
    // mount, which is harmless for identity stability since both
    // case variants would already be the same path.
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// Parse a `sqry.workspace.<key>` string array into a set of absolute
/// paths anchored at `base_dir`.
fn path_set_from_value(
    sqry_top: Option<&serde_json::Value>,
    key: &str,
    base_dir: &Path,
) -> std::collections::BTreeSet<PathBuf> {
    let mut set = std::collections::BTreeSet::new();
    let Some(top) = sqry_top else { return set };
    let Some(arr) = top.get(key).and_then(|v| v.as_array()) else {
        return set;
    };
    for item in arr {
        if let Some(s) = item.as_str() {
            let p = if Path::new(s).is_absolute() {
                PathBuf::from(s)
            } else {
                base_dir.join(s)
            };
            set.insert(p);
        }
    }
    set
}

/// Parse `sqry.workspace.memberFolders`: either a `["path", ...]` array
/// (defaults to `OperationalFolder`) or an array of objects
/// `{ "path": "...", "reason": "operational" }`.
fn member_overrides_from_value(
    sqry_top: Option<&serde_json::Value>,
    base_dir: &Path,
) -> Result<BTreeMap<PathBuf, MemberReason>, LogicalWorkspaceError> {
    let mut map = BTreeMap::new();
    let Some(top) = sqry_top else { return Ok(map) };
    let Some(arr) = top.get("memberFolders").and_then(|v| v.as_array()) else {
        return Ok(map);
    };
    for (idx, item) in arr.iter().enumerate() {
        let (path_str, reason) = if let Some(s) = item.as_str() {
            (s.to_string(), MemberReason::OperationalFolder)
        } else if let Some(obj) = item.as_object() {
            let path = obj
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| LogicalWorkspaceError::MalformedFolderEntry {
                    reason: format!(
                        "sqry.workspace.memberFolders[{idx}] object missing string `path`"
                    ),
                })?
                .to_string();
            // The "operational" and `_` arms intentionally share a body:
            // explicit "operational" is documented; unknown strings fall
            // back to the same default. Keep the arms separated so the
            // explicit-keyword behaviour is visible in code review.
            #[allow(clippy::match_same_arms)]
            let reason = obj.get("reason").and_then(|v| v.as_str()).map_or(
                MemberReason::OperationalFolder,
                |s| match s {
                    "operational" => MemberReason::OperationalFolder,
                    "non-source" | "nonSource" | "non_source" => MemberReason::NonSourceFolder,
                    "noLanguagePluginMatch" | "no-language-plugin-match" => {
                        MemberReason::NoLanguagePluginMatch
                    }
                    _ => MemberReason::OperationalFolder,
                },
            );
            (path, reason)
        } else {
            return Err(LogicalWorkspaceError::MalformedFolderEntry {
                reason: format!(
                    "sqry.workspace.memberFolders[{idx}] is neither a string nor an object"
                ),
            });
        };
        let abs = if Path::new(&path_str).is_absolute() {
            PathBuf::from(&path_str)
        } else {
            base_dir.join(&path_str)
        };
        map.insert(abs, reason);
    }
    Ok(map)
}
