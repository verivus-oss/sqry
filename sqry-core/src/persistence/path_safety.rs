//! Path-safety validation for sqry's on-disk persistence write/read paths.
//!
//! # Overview
//!
//! [`validate_path_in_workspace`] enforces the workspace-containment and
//! no-symlink contract that all callers writing or reading derived-cache files
//! must satisfy. The function is intentionally self-contained within
//! `sqry-core` so that crates such as `sqry-db` can depend on it without
//! pulling in MCP-specific code.
//!
//! # Security model
//!
//! sqry writes data derived from the user's workspace onto disk. Allowing a
//! crafted symlink inside the workspace to redirect those writes to an
//! arbitrary path would be a classic path-traversal / TOCTOU vulnerability.
//! This helper prevents that by:
//!
//! 1. **Join-before-canonicalize**: Relative paths are joined against the
//!    canonical workspace root before any `canonicalize` call. This matches
//!    the caller-boundary pattern used in `sqry-mcp` (see
//!    `sqry-mcp/src/engine.rs:457`).
//! 2. **Parent-only canonicalize**: The target itself may not exist yet
//!    (first save on a fresh workspace). We canonicalize the *parent* and
//!    reconstruct the full path, avoiding `canonicalize` failures on missing
//!    files.
//! 3. **Descendant check**: The resulting canonical path must start with the
//!    canonical workspace root. Pure-path prefix matching prevents `..`
//!    escapes that survive canonicalization.
//! 4. **Symlink rejection on target**: `symlink_metadata` (lstat) is used;
//!    we never follow symlinks on the final file component.
//! 5. **Symlink rejection on ancestors**: Every directory component between
//!    the canonical file path and the canonical workspace root is checked with
//!    `symlink_metadata`. A symlink anywhere in that chain is rejected.

use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Error variants returned by [`validate_path_in_workspace`].
#[derive(Debug)]
pub enum PathSafetyError {
    /// The canonicalized path is not a descendant of the workspace root.
    ///
    /// This is triggered by absolute paths that escape the workspace, or by
    /// relative paths containing enough `..` components to escape after
    /// joining.
    OutsideWorkspace {
        /// The resolved path that was found to be outside the workspace.
        path: PathBuf,
        /// The canonical workspace root used for the comparison.
        workspace_root: PathBuf,
    },

    /// The final path component is a symlink (detected via `symlink_metadata`;
    /// the symlink is NOT followed).
    ///
    /// sqry refuses to write to or read from symlink targets to prevent
    /// TOCTOU races and silent redirection of persistence data.
    SymlinkTarget {
        /// The path whose final component is a symlink.
        path: PathBuf,
    },

    /// A directory ancestor of the path — between the path and the workspace
    /// root — is a symlink.
    ///
    /// An attacker controlling a symlinked ancestor directory could redirect
    /// all writes inside that subtree to an arbitrary location.
    SymlinkInAncestor {
        /// The ancestor directory path that was found to be a symlink.
        ancestor: PathBuf,
    },

    /// A transparent wrapper around a [`std::io::Error`].
    Io(std::io::Error),
}

impl std::fmt::Display for PathSafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideWorkspace {
                path,
                workspace_root,
            } => write!(
                f,
                "path '{}' is outside workspace root '{}'",
                path.display(),
                workspace_root.display(),
            ),
            Self::SymlinkTarget { path } => write!(
                f,
                "path '{}' is a symlink; sqry refuses to follow symlinks on persistence paths",
                path.display(),
            ),
            Self::SymlinkInAncestor { ancestor } => write!(
                f,
                "ancestor directory '{}' is a symlink; \
                 all ancestor directories up to the workspace root must be real directories",
                ancestor.display(),
            ),
            Self::Io(e) => write!(f, "I/O error during path validation: {e}"),
        }
    }
}

impl std::error::Error for PathSafetyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PathSafetyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public function
// ─────────────────────────────────────────────────────────────────────────────

/// Validate that `path` is safe to open for read/write inside `workspace_root`.
///
/// # Validation steps
///
/// 1. If `path` is relative, join it against `workspace_root` first. This
///    is the **caller-boundary** pattern: all paths are resolved in the context
///    of the workspace, not the process working directory.
/// 2. Canonicalize `workspace_root` (it must exist on disk).
/// 3. **Pre-canonicalization ancestor symlink scan**: walk the raw joined path
///    component-by-component (starting after the workspace root prefix), and
///    for each directory component call `symlink_metadata`. If any component
///    is a symlink, return `Err(SymlinkInAncestor)`. This check must happen
///    *before* `canonicalize` resolves symlinks away.
/// 4. Canonicalize the **parent** of the joined path. The target itself may
///    not exist yet (legitimate for `save_derived` on a fresh workspace). If
///    the parent does not exist, `Err(Io(...))` is returned — callers are
///    responsible for creating the parent directory first.
/// 5. Reconstruct `canonical_path = canonical_parent.join(file_name)`.
/// 6. Confirm `canonical_path` starts with `canonical_workspace_root`
///    (descendant check). If not, return `Err(OutsideWorkspace)`.
/// 7. If the target currently exists, call `symlink_metadata` on it. If it is
///    a symlink, return `Err(SymlinkTarget)`.
///
/// On success returns the fully resolved `canonical_path` (parent
/// canonicalized + file name appended). The caller can use this for all
/// subsequent I/O.
///
/// # Errors
///
/// Returns [`PathSafetyError`] in any of the documented failure cases.
pub fn validate_path_in_workspace(
    path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, PathSafetyError> {
    // ── Step 1: Resolve relative paths against the workspace root ────────────
    //
    // Do NOT use the process CWD — all paths are in the context of the
    // workspace. This is the caller-boundary pattern from engine.rs:457.
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };

    // ── Step 2: Canonicalize the workspace root ───────────────────────────────
    let canonical_ws = workspace_root.canonicalize()?;

    // ── Step 3: Pre-canonicalization ancestor symlink scan ───────────────────
    //
    // `canonicalize` follows symlinks, so after canonicalization the path
    // components no longer reflect the raw filesystem structure — symlinks
    // are silently resolved. We MUST scan for symlinked ancestors on the raw
    // (pre-canonical) path BEFORE calling canonicalize.
    //
    // We walk the component chain incrementally:
    //   workspace_root / c1 / c2 / ... / cN-1  (all except the final filename)
    // For each accumulated prefix, call `symlink_metadata` on it. If any
    // prefix is a symlink, reject immediately.
    //
    // The scan begins after the workspace root itself (the root is trusted).
    {
        // Collect all ancestor components that lie between workspace_root and
        // the parent of `joined`. We build them up incrementally.
        let parent = joined.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "validate_path_in_workspace: path has no parent component: {}",
                    joined.display()
                ),
            )
        })?;

        // Strip the workspace_root prefix from `parent` to get the relative
        // sub-path. If `joined` is absolute and shares no prefix with
        // workspace_root we will catch it in step 6 (outside workspace check).
        // For now, proceed with whatever sub-components we have.
        let sub_path = parent.strip_prefix(workspace_root).unwrap_or(parent);

        // Walk the sub-path components, building incremental prefixes rooted
        // at `workspace_root`.
        let mut cursor = workspace_root.to_path_buf();
        for component in sub_path.components() {
            cursor.push(component);

            // `symlink_metadata` is lstat — it sees the link node itself,
            // not the target. If `cursor` does not exist yet, `symlink_metadata`
            // returns `NotFound` which we treat as "not a symlink" (the
            // ancestor simply hasn't been created; that will be caught later
            // when canonicalize fails).
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(PathSafetyError::SymlinkInAncestor { ancestor: cursor });
                }
                // Not found or other IO error: skip the symlink check for
                // this component. A missing ancestor will surface as an IO
                // error in the canonicalize step below.
                _ => {}
            }
        }
    }

    // ── Step 4: Canonicalize the PARENT of the joined path ───────────────────
    //
    // We intentionally do NOT canonicalize `joined` itself because the target
    // file may not exist yet (first save on a fresh workspace). Canonicalizing
    // a non-existing path fails on POSIX. Instead, canonicalize the parent
    // directory, which MUST exist, and then re-attach the file name.
    let parent = joined.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "validate_path_in_workspace: path has no parent component: {}",
                joined.display()
            ),
        )
    })?;

    let canonical_parent = parent.canonicalize().map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "validate_path_in_workspace: cannot canonicalize parent directory '{}': {e}",
                parent.display()
            ),
        )
    })?;

    // ── Step 5: Reconstruct the full canonical path ───────────────────────────
    let file_name = joined.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "validate_path_in_workspace: path has no file name component: {}",
                joined.display()
            ),
        )
    })?;
    let canonical_path = canonical_parent.join(file_name);

    // ── Step 6: Descendant (workspace-containment) check ─────────────────────
    //
    // Use `starts_with` on the canonical paths. This is safe after
    // canonicalization because both paths are fully resolved (no `..`, no
    // symlinks up to this point) and use the OS-native separator.
    if !canonical_path.starts_with(&canonical_ws) {
        return Err(PathSafetyError::OutsideWorkspace {
            path: canonical_path,
            workspace_root: canonical_ws,
        });
    }

    // ── Step 7: Reject if the target itself is a symlink ─────────────────────
    //
    // `symlink_metadata` does NOT follow the symlink, so we see the link node
    // itself. Only check if the path exists (non-existent targets are fine —
    // they will be created fresh).
    //
    // Note: we check the raw `joined` path here (not `canonical_path`) because
    // the file name is the same in both, and `joined` still carries the
    // original link if the final component is itself a symlink.
    if let Ok(meta) = std::fs::symlink_metadata(&joined)
        && meta.file_type().is_symlink()
    {
        return Err(PathSafetyError::SymlinkTarget {
            path: canonical_path,
        });
    }
    // Also check via canonical_path in case it differs from joined.
    if let Ok(meta) = std::fs::symlink_metadata(&canonical_path)
        && meta.file_type().is_symlink()
    {
        return Err(PathSafetyError::SymlinkTarget {
            path: canonical_path,
        });
    }

    Ok(canonical_path)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn tmp_workspace() -> TempDir {
        TempDir::new().expect("TempDir::new failed")
    }

    // ── Happy path: relative path inside workspace ────────────────────────────

    /// A relative path such as `.sqry/graph/derived.sqry` joined against the
    /// workspace root must canonicalize correctly and return
    /// `Ok(canonical_path)` once the parent directory exists.
    #[test]
    fn happy_path_relative_under_workspace() {
        let ws = tmp_workspace();

        // Parent directory must exist for canonicalize to succeed.
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();

        let result = validate_path_in_workspace(Path::new(".sqry/graph/derived.sqry"), ws.path());

        assert!(result.is_ok(), "happy path should succeed; got {result:?}");
        let canonical = result.unwrap();

        // Must be inside the workspace.
        assert!(
            canonical.starts_with(ws.path().canonicalize().unwrap()),
            "canonical path must be inside workspace: {canonical:?}"
        );
        // Must retain the file name.
        assert!(
            canonical.ends_with("derived.sqry"),
            "canonical path must end with 'derived.sqry': {canonical:?}"
        );
    }

    // ── Happy path: non-existing target file is not an error ─────────────────

    /// `validate_path_in_workspace` must succeed even when the target file
    /// does not yet exist. This is the normal "first save" scenario.
    #[test]
    fn happy_path_nonexistent_target_is_ok() {
        let ws = tmp_workspace();
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();

        // `derived.sqry` does NOT exist yet.
        let target = Path::new(".sqry/graph/derived.sqry");
        assert!(
            !ws.path().join(target).exists(),
            "pre-condition: target must not exist"
        );

        let result = validate_path_in_workspace(target, ws.path());
        assert!(
            result.is_ok(),
            "non-existent target should be allowed; got {result:?}"
        );
    }

    // ── Happy path: absolute path inside workspace ────────────────────────────

    /// An absolute path that is genuinely inside the workspace must succeed.
    #[test]
    fn happy_path_absolute_under_workspace() {
        let ws = tmp_workspace();
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();

        // Absolute path constructed from the workspace root.
        let abs = ws.path().join(".sqry/graph/derived.sqry");
        assert!(abs.is_absolute(), "pre-condition: abs must be absolute");

        let result = validate_path_in_workspace(&abs, ws.path());
        assert!(
            result.is_ok(),
            "absolute in-workspace path should succeed; got {result:?}"
        );
    }

    // ── Happy path: already-existing regular file is fine ────────────────────

    /// An existing regular file (not a symlink) must be accepted.
    #[test]
    fn happy_path_existing_regular_file() {
        let ws = tmp_workspace();
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();
        let target = ws.path().join(".sqry/graph/derived.sqry");
        fs::write(&target, b"previous data").unwrap();

        let result = validate_path_in_workspace(&target, ws.path());
        assert!(
            result.is_ok(),
            "existing regular file should succeed; got {result:?}"
        );
    }

    // ── Rejection: path outside workspace ────────────────────────────────────

    /// An absolute path that lives in a completely different directory must be
    /// rejected with `PathSafetyError::OutsideWorkspace`.
    #[test]
    fn rejects_path_outside_workspace() {
        let ws = tmp_workspace();
        let outside = TempDir::new().unwrap();

        // Pre-create the parent inside the outside dir so the parent
        // canonicalization step succeeds — the rejection must come from the
        // descendant check, not from a missing parent.
        let result = validate_path_in_workspace(&outside.path().join("derived.sqry"), ws.path());

        match result {
            Err(PathSafetyError::OutsideWorkspace { .. }) => {}
            other => panic!("expected OutsideWorkspace, got {other:?}"),
        }
    }

    /// A relative path that escapes via `../..` must be rejected as
    /// `OutsideWorkspace` after joining and canonicalization.
    #[test]
    fn rejects_dotdot_escape() {
        let ws = tmp_workspace();
        // The parent of the temp dir is guaranteed to exist.
        let result = validate_path_in_workspace(Path::new("../../etc/passwd"), ws.path());

        match result {
            Err(PathSafetyError::OutsideWorkspace { .. }) => {}
            // If `../../etc` doesn't exist the canonicalize of the parent
            // returns an Io error, which is also an acceptable rejection.
            Err(PathSafetyError::Io(_)) => {}
            other => panic!("expected OutsideWorkspace or Io, got {other:?}"),
        }
    }

    // ── Rejection: symlink target ─────────────────────────────────────────────

    /// When the target path itself is a symlink, `SymlinkTarget` must be
    /// returned. This test only runs on Unix (Windows symlinks need elevation).
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        let ws = tmp_workspace();
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();

        // Create a real file and a symlink pointing to it, both inside the
        // workspace. The symlink is the path we pass as the target.
        let real = ws.path().join(".sqry/graph/real.sqry");
        fs::write(&real, b"x").unwrap();
        let link = ws.path().join(".sqry/graph/derived.sqry");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let result = validate_path_in_workspace(&link, ws.path());

        match result {
            Err(PathSafetyError::SymlinkTarget { .. }) => {}
            other => panic!("expected SymlinkTarget, got {other:?}"),
        }
    }

    /// Symlink target rejection also fires for dangling symlinks (symlinks
    /// whose destination does not exist).
    #[cfg(unix)]
    #[test]
    fn rejects_dangling_symlink_target() {
        let ws = tmp_workspace();
        fs::create_dir_all(ws.path().join(".sqry/graph")).unwrap();

        let link = ws.path().join(".sqry/graph/derived.sqry");
        // Point at a non-existing destination.
        std::os::unix::fs::symlink(ws.path().join("nonexistent"), &link).unwrap();

        // The symlink itself exists even though its target does not.
        assert!(
            link.symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "pre-condition: link must be a symlink"
        );

        let result = validate_path_in_workspace(&link, ws.path());

        match result {
            Err(PathSafetyError::SymlinkTarget { .. }) => {}
            other => panic!("expected SymlinkTarget for dangling symlink, got {other:?}"),
        }
    }

    // ── Rejection: symlink in ancestor ───────────────────────────────────────

    /// When an ancestor directory is a symlink the call must return
    /// `PathSafetyError::SymlinkInAncestor`.
    ///
    /// Setup: `.sqry` is a symlink → `.sqry_real` (a real directory).
    /// The path `.sqry/graph/derived.sqry` passes through the symlinked
    /// ancestor `.sqry`.
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_in_ancestor() {
        let ws = tmp_workspace();

        // Create real backing directory tree.
        fs::create_dir_all(ws.path().join(".sqry_real/graph")).unwrap();

        // Make `.sqry` a symlink pointing to `.sqry_real`.
        std::os::unix::fs::symlink(ws.path().join(".sqry_real"), ws.path().join(".sqry")).unwrap();

        // The relative path traverses the symlinked ancestor `.sqry`.
        let result = validate_path_in_workspace(Path::new(".sqry/graph/derived.sqry"), ws.path());

        match result {
            Err(PathSafetyError::SymlinkInAncestor { .. }) => {}
            other => panic!("expected SymlinkInAncestor, got {other:?}"),
        }
    }

    /// Intermediate-level symlink: only `graph` is a symlink inside a real
    /// `.sqry` directory. The ancestor walk must still detect it.
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_intermediate_ancestor() {
        let ws = tmp_workspace();

        // Real tree: `.sqry/` (real dir) → `graph_real/` (real dir).
        fs::create_dir_all(ws.path().join(".sqry")).unwrap();
        fs::create_dir_all(ws.path().join("graph_real")).unwrap();

        // `.sqry/graph` → `../../graph_real` (relative symlink into workspace).
        std::os::unix::fs::symlink(ws.path().join("graph_real"), ws.path().join(".sqry/graph"))
            .unwrap();

        let result = validate_path_in_workspace(Path::new(".sqry/graph/derived.sqry"), ws.path());

        match result {
            Err(PathSafetyError::SymlinkInAncestor { .. }) => {}
            other => panic!("expected SymlinkInAncestor for intermediate symlink, got {other:?}"),
        }
    }

    // ── Error quality: Display messages cite paths ────────────────────────────

    /// `Display` for each variant must produce a non-empty message that
    /// contains the offending path string.
    #[test]
    fn display_outside_workspace_cites_paths() {
        let err = PathSafetyError::OutsideWorkspace {
            path: PathBuf::from("/tmp/escape/foo.sqry"),
            workspace_root: PathBuf::from("/home/user/project"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/tmp/escape/foo.sqry"),
            "Display must cite the offending path; got: {msg}"
        );
        assert!(
            msg.contains("/home/user/project"),
            "Display must cite the workspace root; got: {msg}"
        );
    }

    #[test]
    fn display_symlink_target_cites_path() {
        let err = PathSafetyError::SymlinkTarget {
            path: PathBuf::from("/ws/.sqry/graph/derived.sqry"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/ws/.sqry/graph/derived.sqry"),
            "Display must cite the symlink path; got: {msg}"
        );
    }

    #[test]
    fn display_symlink_in_ancestor_cites_path() {
        let err = PathSafetyError::SymlinkInAncestor {
            ancestor: PathBuf::from("/ws/.sqry/graph"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/ws/.sqry/graph"),
            "Display must cite the ancestor path; got: {msg}"
        );
    }

    #[test]
    fn display_io_delegates_to_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = PathSafetyError::Io(io_err);
        let msg = err.to_string();
        assert!(!msg.is_empty(), "Display for Io variant must not be empty");
    }

    // ── Error trait: source() for Io variant ─────────────────────────────────

    #[test]
    fn error_source_io_variant_is_some() {
        use std::error::Error as _;
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = PathSafetyError::Io(io_err);
        assert!(
            err.source().is_some(),
            "Io variant must expose source via std::error::Error::source()"
        );
    }

    #[test]
    fn error_source_non_io_variants_are_none() {
        use std::error::Error as _;

        let outside = PathSafetyError::OutsideWorkspace {
            path: PathBuf::from("/a"),
            workspace_root: PathBuf::from("/b"),
        };
        assert!(outside.source().is_none());

        let sym_target = PathSafetyError::SymlinkTarget {
            path: PathBuf::from("/a"),
        };
        assert!(sym_target.source().is_none());

        let sym_anc = PathSafetyError::SymlinkInAncestor {
            ancestor: PathBuf::from("/a"),
        };
        assert!(sym_anc.source().is_none());
    }

    // ── From<io::Error> ───────────────────────────────────────────────────────

    #[test]
    fn from_io_error_constructs_io_variant() {
        let io_err = std::io::Error::other("test");
        let safety_err = PathSafetyError::from(io_err);
        assert!(
            matches!(safety_err, PathSafetyError::Io(_)),
            "From<io::Error> must yield the Io variant"
        );
    }
}
