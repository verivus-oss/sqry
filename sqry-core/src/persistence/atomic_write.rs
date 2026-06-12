//! Atomic file-write helper for sqry's on-disk persistence paths.
//!
//! # Overview
//!
//! [`atomic_write_bytes`] implements the canonical "write to tempfile in the
//! same directory, then rename" pattern. This gives callers a **best-effort
//! atomic replace** on any POSIX-compliant filesystem: readers either see the
//! old content or the new content, never a partial write.
//!
//! ## Protocol
//!
//! 1. Reject if `target_path` itself is an existing **symlink** — we refuse to
//!    follow or replace symlinks silently.
//! 2. Reject if `target_path`'s **parent directory** resolves to a symlink
//!    (canonicalize + re-stat check) — avoids TOCTOU races.
//! 3. Create a named tempfile **inside the same directory** as the target.
//!    Same-directory placement is critical: `rename(2)` is only guaranteed
//!    atomic within a single filesystem/device boundary.
//! 4. Write all bytes, then `fsync` the file to flush kernel page-cache to
//!    durable storage.
//! 5. Close the tempfile handle (implicit on drop after `persist`).
//! 6. `rename(temp, target)` — atomic on POSIX.
//! 7. On **Unix only**: open the parent directory and call `fsync` on its file
//!    descriptor to flush the directory entry pointing at the new inode.
//!    This step ensures the rename itself survives a crash/power-loss event.
//!    On **Windows** and other non-Unix targets, the parent-directory fsync is
//!    a **no-op** (Windows rename semantics differ; the OS provides sufficient
//!    durability guarantees for the scenarios sqry targets on that platform).
//!
//! ## Error semantics
//!
//! On any error the tempfile is removed before returning. The target path is
//! never modified unless the rename succeeds.

use std::io::{self, Write as _};
use std::path::Path;

use tempfile::NamedTempFile;

/// Write `bytes` to `target_path` atomically.
///
/// # Errors
///
/// Returns `Err` if:
/// - `target_path` exists and is a **symlink** (we will not follow or replace
///   symlinks).
/// - The **parent directory** of `target_path` is itself a symlink (detected
///   after canonicalization).
/// - The parent directory does not exist (caller's responsibility).
/// - Any I/O error occurs during tempfile creation, writing, syncing, or
///   renaming.
///
/// On error the target file is left unmodified. Any tempfile created during
/// the operation is cleaned up before returning the error.
///
/// # Platform notes
///
/// - **Unix**: `fsync(2)` is called on both the tempfile and, after the rename,
///   on the parent directory file descriptor. This makes the rename durable
///   against power loss.
/// - **Windows / other non-Unix**: Parent-directory fsync is a no-op. The
///   tempfile is still written and renamed atomically via the OS rename call.
pub fn atomic_write_bytes(target_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (parent, canonical_parent) = validate_atomic_write_target(target_path)?;
    write_and_persist_tempfile(&parent, target_path, bytes)?;
    fsync_parent_dir(&canonical_parent)?;

    Ok(())
}

fn validate_atomic_write_target(
    target_path: &Path,
) -> io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    reject_target_symlink(target_path)?;

    let parent = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic_write_bytes: target path has no parent directory: {}",
                target_path.display()
            ),
        )
    })?;
    reject_parent_symlink(parent)?;
    let canonical_parent = canonical_parent_dir(parent)?;

    Ok((parent.to_path_buf(), canonical_parent))
}

fn reject_target_symlink(target_path: &Path) -> io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(target_path)
        && meta.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic_write_bytes: target path is a symlink and will not be followed: {}",
                target_path.display()
            ),
        ));
    }
    Ok(())
}

fn reject_parent_symlink(parent: &Path) -> io::Result<()> {
    let raw_parent_meta = std::fs::symlink_metadata(parent).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "atomic_write_bytes: cannot stat parent directory '{}': {e}",
                parent.display()
            ),
        )
    })?;
    if raw_parent_meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic_write_bytes: parent directory is a symlink and will not be followed: {}",
                parent.display()
            ),
        ));
    }
    Ok(())
}

fn canonical_parent_dir(parent: &Path) -> io::Result<std::path::PathBuf> {
    let canonical_parent = parent.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "atomic_write_bytes: cannot canonicalize parent directory '{}': {e}",
                parent.display()
            ),
        )
    })?;
    // After canonicalization the result must be a directory, not a symlink.
    let canon_meta = std::fs::symlink_metadata(&canonical_parent).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "atomic_write_bytes: cannot stat canonical parent '{}': {e}",
                canonical_parent.display()
            ),
        )
    })?;
    if !canon_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!(
                "atomic_write_bytes: canonical parent path is not a directory: {}",
                canonical_parent.display()
            ),
        ));
    }
    Ok(canonical_parent)
}

fn write_and_persist_tempfile(parent: &Path, target_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = NamedTempFile::new_in(parent).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "atomic_write_bytes: failed to create tempfile in '{}': {e}",
                parent.display()
            ),
        )
    })?;

    if let Err(write_err) = tmp.write_all(bytes) {
        let _ = tmp.close();
        return Err(io::Error::new(
            write_err.kind(),
            format!("atomic_write_bytes: write failed: {write_err}"),
        ));
    }

    if let Err(sync_err) = tmp.as_file().sync_all() {
        let _ = tmp.close();
        return Err(io::Error::new(
            sync_err.kind(),
            format!("atomic_write_bytes: fsync(file) failed: {sync_err}"),
        ));
    }

    // ── Step 5: rename(temp, target) ──────────────────────────────────────
    //
    // `NamedTempFile::persist` calls `rename(2)` (or equivalent). On
    // failure it returns the original `NamedTempFile` back so we can close
    // (and thus delete) it cleanly.
    tmp.persist(target_path).map_err(|persist_err| {
        // `persist_err.file` is the `NamedTempFile` that was NOT renamed.
        // Dropping it (via close) removes the tempfile.
        let _ = persist_err.file.close();
        io::Error::new(
            persist_err.error.kind(),
            format!(
                "atomic_write_bytes: rename to '{}' failed: {}",
                target_path.display(),
                persist_err.error
            ),
        )
    })?;
    Ok(())
}

/// Fsync the parent directory on Unix; no-op on other platforms.
///
/// Opens the directory read-only and calls `sync_all` on the resulting file
/// descriptor. This flushes the directory block containing the updated entry
/// created by the preceding `rename` call to durable storage.
#[cfg(unix)]
fn fsync_parent_dir(canonical_parent: &Path) -> io::Result<()> {
    use std::fs::OpenOptions;
    let dir_file = OpenOptions::new()
        .read(true)
        .open(canonical_parent)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "atomic_write_bytes: cannot open parent dir for fsync '{}': {e}",
                    canonical_parent.display()
                ),
            )
        })?;
    dir_file.sync_all().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "atomic_write_bytes: fsync(parent_dir) failed for '{}': {e}",
                canonical_parent.display()
            ),
        )
    })
}

/// No-op parent-directory fsync on non-Unix platforms.
///
/// On Windows the OS provides sufficient rename-durability guarantees for
/// sqry's persistence use-cases. This function intentionally does nothing.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn fsync_parent_dir(_canonical_parent: &Path) -> io::Result<()> {
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    // ── Helper ────────────────────────────────────────────────────────────────

    /// Create a temporary directory that is automatically removed on drop.
    fn tmp_dir() -> TempDir {
        TempDir::new().expect("TempDir::new failed")
    }

    // ── Test: happy path ─────────────────────────────────────────────────────

    /// Write bytes to a non-existing target, verify content, verify no temp
    /// files are left behind in the parent directory.
    #[test]
    fn atomic_write_happy_path() {
        let dir = tmp_dir();
        let target = dir.path().join("output.bin");
        let content = b"hello atomic world";

        // Target must not pre-exist.
        assert!(!target.exists(), "pre-condition: target must not exist");

        atomic_write_bytes(&target, content).expect("atomic_write_bytes failed");

        // Content must match.
        let read_back = fs::read(&target).expect("read back failed");
        assert_eq!(read_back, content, "content mismatch after atomic write");

        // No leftover tempfiles in parent.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .expect("read_dir failed")
            .filter_map(|e| e.ok())
            .collect();
        // Only the target file should exist.
        assert_eq!(
            entries.len(),
            1,
            "unexpected files left in parent dir: {entries:?}"
        );
        assert_eq!(
            entries[0].path(),
            target,
            "the only file in parent should be the target"
        );
    }

    // ── Test: overwrite existing regular file ────────────────────────────────

    /// Verify that an existing regular file is replaced with new content.
    #[test]
    fn atomic_write_overwrites_existing_regular_file() {
        let dir = tmp_dir();
        let target = dir.path().join("existing.txt");
        let old_content = b"old content";
        let new_content = b"new content -- replaced atomically";

        // Write old content directly (not through our helper).
        fs::write(&target, old_content).expect("pre-write failed");
        assert!(target.is_file(), "pre-condition: target is a regular file");

        atomic_write_bytes(&target, new_content).expect("atomic_write_bytes failed on overwrite");

        let read_back = fs::read(&target).expect("read back failed");
        assert_eq!(read_back, new_content, "content should have been replaced");
    }

    // ── Test: symlink target rejection ───────────────────────────────────────

    /// If the target path is itself a symlink, the call must return Err
    /// without modifying the symlink or its destination.
    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink_target() {
        let dir = tmp_dir();
        let real_file = dir.path().join("real.txt");
        let symlink_target = dir.path().join("link.txt");

        // Create a real file and a symlink pointing to it.
        fs::write(&real_file, b"original").expect("pre-write failed");
        std::os::unix::fs::symlink(&real_file, &symlink_target).expect("symlink creation failed");

        assert!(
            symlink_target
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "pre-condition: symlink_target must be a symlink"
        );

        let result = atomic_write_bytes(&symlink_target, b"new bytes");
        assert!(result.is_err(), "expected Err for symlink target, got Ok");

        // The real file behind the symlink must remain unchanged.
        let real_content = fs::read(&real_file).expect("read real_file failed");
        assert_eq!(real_content, b"original", "real file must not be modified");

        // The symlink itself must still exist and still be a symlink.
        let lmeta = symlink_target
            .symlink_metadata()
            .expect("symlink should still exist");
        assert!(
            lmeta.file_type().is_symlink(),
            "symlink must remain a symlink"
        );
    }

    // ── Test: symlink parent rejection ───────────────────────────────────────

    /// If the parent directory of the target is a symlink, the call must
    /// return Err. The target must not be created.
    ///
    /// Only compiled on Unix because creating directory symlinks on Windows
    /// requires elevated privileges.
    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink_parent() {
        let dir = tmp_dir();
        // Create a real subdirectory and a symlink pointing to it.
        let real_subdir = dir.path().join("real_subdir");
        let link_subdir = dir.path().join("link_subdir");
        fs::create_dir(&real_subdir).expect("create real_subdir failed");
        std::os::unix::fs::symlink(&real_subdir, &link_subdir)
            .expect("symlink to directory failed");

        // The target's parent will be the symlinked directory.
        let target = link_subdir.join("output.txt");

        let result = atomic_write_bytes(&target, b"should not be written");
        assert!(result.is_err(), "expected Err for symlink parent, got Ok");

        // No file should have been created under real_subdir or link_subdir.
        assert!(
            !real_subdir.join("output.txt").exists(),
            "file must not be created in real_subdir"
        );
    }

    // ── Test: temp cleanup on rename failure ─────────────────────────────────

    /// Induce a rename failure by pointing the target inside a read-only
    /// directory (on Unix). Verify that:
    ///   1. The call returns Err.
    ///   2. No tempfile is left behind in the (writable) temp source dir.
    ///
    /// This test is Unix-only because chmod on directories behaves differently
    /// on Windows.
    #[cfg(unix)]
    #[test]
    fn atomic_write_temp_cleanup_on_failure() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tmp_dir();

        // Create a subdirectory that will be made read-only so rename into it
        // fails. The tempfile is created in a *different* writable dir.
        //
        // Strategy: create the target inside a read-only dir; the tempfile
        // will be created in `writable_dir` (which the target path resolves
        // its parent from). We do this by having the target path *literally*
        // inside a read-only dir so that `parent()` returns that dir.
        let readonly_dir = dir.path().join("readonly");
        fs::create_dir(&readonly_dir).expect("create readonly_dir failed");

        // Make it read-only *before* we try to write so rename fails.
        let mut perms = fs::metadata(&readonly_dir)
            .expect("stat readonly_dir")
            .permissions();
        perms.set_mode(0o500); // r-x------
        fs::set_permissions(&readonly_dir, perms).expect("chmod failed");

        let target = readonly_dir.join("output.txt");
        let result = atomic_write_bytes(&target, b"data");
        assert!(
            result.is_err(),
            "expected Err when rename into read-only dir"
        );

        // Restore permissions so TempDir cleanup can remove the directory.
        let mut perms = fs::metadata(&readonly_dir)
            .expect("stat readonly_dir")
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&readonly_dir, perms).ok();

        // No tempfile left behind in readonly_dir.
        let remaining: Vec<_> = fs::read_dir(&readonly_dir)
            .expect("read_dir readonly_dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            remaining.is_empty(),
            "no tempfile should remain after failure: {remaining:?}"
        );
    }
}
