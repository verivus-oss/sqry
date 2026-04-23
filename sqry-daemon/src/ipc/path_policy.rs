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

/// Resolve a user-supplied `index_root` into the canonical path used
/// to construct a [`crate::workspace::WorkspaceKey`].
///
/// # Errors
///
/// Returns [`MethodError::InvalidParams`] (wire code `-32602`) when:
/// - the input cannot be absolutised (e.g., the current working
///   directory probe fails)
/// - the path exists and is not a directory
/// - the path does not exist
/// - the filesystem stat call fails for any other reason
/// - the canonicalisation call fails on an existing directory
pub fn resolve_index_root(raw: &Path) -> Result<PathBuf, MethodError> {
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
    fn relative_path_absolutises_against_cwd() {
        let tmp = tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let prev = std::env::current_dir().unwrap();
        // chdir into tmp so "sub" is resolvable lexically.
        std::env::set_current_dir(tmp.path()).unwrap();
        let got = resolve_index_root(Path::new("sub"));
        // Restore cwd BEFORE asserting so a failure doesn't leak state.
        std::env::set_current_dir(&prev).unwrap();
        let got = got.expect("relative existing dir must resolve");
        assert!(got.is_absolute(), "result must be absolute: {got:?}");
    }
}
