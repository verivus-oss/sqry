//! Canonicalisation policy for user-supplied `index_root` paths.
//!
//! Phase 8a iter-3 rule: every accepted `WorkspaceKey` is backed by an
//! **existing directory** whose path has been canonicalised via
//! [`sqry_core::project::canonicalize_path`]. This gives two guarantees
//! that admission bookkeeping (specifically the `WorkspaceKey` hash)
//! depends on:
//!
//! 1. **Symlink deduplication** — `canonicalize_path` wraps
//!    `std::fs::canonicalize`, which resolves every intermediate
//!    symlink. Two symlinks pointing at the same target therefore hash
//!    to the same `WorkspaceKey`.
//! 2. **Case normalisation on case-insensitive filesystems**
//!    (NTFS on Windows, APFS/HFS+ on macOS, VFAT on Linux). The
//!    platform-native canonicalisation primitive returns the real,
//!    case-stable on-disk path — so two clients sending
//!    `"/Repos/Project"` and `"/repos/project"` on NTFS end up with
//!    the same cache entry.
//!
//! Non-existent roots cannot satisfy either guarantee (no filesystem
//! entry to canonicalise), so they are uniformly rejected with
//! `-32602 Invalid Params`.

use std::io;
use std::path::{Path, PathBuf};

use serde::ser::Error as _;
use sqry_core::project::{absolutize_without_resolution, canonicalize_path};

use super::methods::MethodError;

/// #566: enforce that a user-supplied workspace path is absolute before it is
/// resolved.
///
/// The daemon serves many workspaces and has no client working directory to
/// resolve a relative path against (unlike standalone sqry-mcp, which pins the
/// client root via the MCP `roots/list` callback). Any resolver that joins a
/// relative path onto `std::env::current_dir()` therefore silently selects the
/// daemon's own process directory (`$HOME` under the systemd user unit),
/// picking the wrong workspace. This is the single guard shared by every
/// daemon-side path resolver (`resolve_index_root` here,
/// `tool_core::resolve_path`, and the revision-target query/search handlers).
///
/// Returns the canonical rejection message on failure so each caller can wrap
/// it in its own error type.
pub(crate) fn ensure_absolute_workspace_path(raw: &Path) -> Result<(), String> {
    if raw.is_absolute() {
        return Ok(());
    }
    Err(format!(
        "workspace path must be absolute in daemon mode; received relative path {raw:?}. \
         The daemon has no client working directory to resolve it against (it would resolve \
         against the daemon's own directory). Pass an absolute path, or load the workspace \
         with `sqry daemon load <ABSOLUTE_PATH>`."
    ))
}

/// Resolve a user-supplied `index_root` into the canonical path used
/// to construct a [`crate::workspace::WorkspaceKey`].
///
/// # Errors
///
/// Returns [`MethodError::InvalidParams`] (wire code `-32602`) when:
/// - the path is not absolute (#566: no daemon-side CWD to resolve against)
/// - the input cannot be absolutised (e.g., the current working
///   directory probe fails)
/// - the path exists and is not a directory
/// - the path does not exist
/// - the filesystem stat call fails for any other reason
/// - the canonicalisation call fails on an existing directory
pub fn resolve_index_root(raw: &Path) -> Result<PathBuf, MethodError> {
    ensure_absolute_workspace_path(raw)
        .map_err(|reason| MethodError::InvalidParams(serde_json::Error::custom(reason)))?;
    let absolutised = absolutize_without_resolution(raw).map_err(|e| {
        MethodError::InvalidParams(serde_json::Error::custom(format!(
            "index_root absolutise: {e}"
        )))
    })?;
    match std::fs::metadata(&absolutised) {
        Ok(meta) if meta.is_dir() => canonicalize_path(&absolutised).map_err(|e| {
            MethodError::InvalidParams(serde_json::Error::custom(format!(
                "index_root canonicalize: {e}"
            )))
        }),
        Ok(_) => Err(MethodError::InvalidParams(serde_json::Error::custom(
            "index_root exists but is not a directory",
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(MethodError::InvalidParams(serde_json::Error::custom(
                "index_root does not exist; daemon/load requires an \
                 existing directory so a canonical WorkspaceKey can be \
                 computed",
            )))
        }
        Err(e) => Err(MethodError::InvalidParams(serde_json::Error::custom(
            format!("index_root stat: {e}"),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn existing_directory_canonicalises() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let got = resolve_index_root(root).expect("existing dir must resolve");
        assert_eq!(got, canonicalize_path(root).unwrap());
    }

    #[test]
    fn nonexistent_rejected() {
        let tmp = tempdir().unwrap();
        let ghost = tmp.path().join("does-not-exist");
        let err = resolve_index_root(&ghost).unwrap_err();
        match err {
            MethodError::InvalidParams(e) => {
                assert!(e.to_string().contains("does not exist"), "{e}");
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn file_not_directory_rejected() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, b"content").unwrap();
        let err = resolve_index_root(&file).unwrap_err();
        match err {
            MethodError::InvalidParams(e) => {
                assert!(e.to_string().contains("not a directory"), "{e}");
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_directory_dedups_to_target() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let a = resolve_index_root(&link).unwrap();
        let b = resolve_index_root(&real).unwrap();
        assert_eq!(a, b, "symlink and target must dedup");
    }

    #[test]
    fn ensure_absolute_workspace_path_contract() {
        // Absolute paths pass; relative paths (and `.`) are rejected with a
        // message naming the absolute-path requirement.
        assert!(ensure_absolute_workspace_path(Path::new("/abs/dir")).is_ok());
        for rel in ["sub", ".", "./x", "../y"] {
            let msg = ensure_absolute_workspace_path(Path::new(rel))
                .expect_err("relative path must be rejected");
            assert!(
                msg.contains("absolute"),
                "message must mention absolute requirement for {rel:?}: {msg}"
            );
        }
    }

    #[test]
    fn relative_path_rejected() {
        // #566: a relative path in daemon mode has no client working directory
        // to resolve against, so it must be rejected rather than absolutised
        // against the daemon's own CWD (which would silently select the wrong
        // workspace). No chdir needed: the guard fires before any CWD use.
        let err = resolve_index_root(Path::new("sub")).unwrap_err();
        match err {
            MethodError::InvalidParams(e) => {
                assert!(
                    e.to_string().contains("absolute"),
                    "reason must mention the absolute-path requirement: {e}"
                );
            }
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }
}
