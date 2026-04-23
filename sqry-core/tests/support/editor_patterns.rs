#![allow(dead_code)] // Consumed from select integration test binaries only.

//! Editor save pattern simulation for watcher tests.
//!
//! Each editor family has a distinct file-save strategy that produces different
//! sequences of filesystem events. The [`simulate_save`] function reproduces
//! these patterns so watcher tests can assert that every pattern normalizes to
//! "exactly one logical changed file in the debounced `ChangeSet`."
//!
//! # Patterns
//!
//! | Pattern | Events (Linux/macOS) | Events (Windows) |
//! |---|---|---|
//! | `DirectWrite` | 1 Modify | 1 Modify |
//! | `VimAtomicRename` | Create(.swp) + Rename(.swp → target) | Remove(target) + Create(target) |
//! | `JetBrainsAtomicSave` | Create(tmp) + Rename(tmp → target) | Remove(target) + Create(target) |
//! | `VscodeSafeSave` | Rename(target → .bak) + Create(target) + Remove(.bak) | same |
//! | `EmacsBackup` | Modify(target) + Create(target~) | same |
//!
//! On all platforms, the watcher must collapse these into exactly one logical
//! file change for the target path. Swap files (`.swp`), backup files
//! (`target~`, `.bak`), and JetBrains temporaries are filtered by the
//! editor-temporary filter in `source_tree.rs`.

use std::fs;
use std::path::Path;

/// Editor save pattern to simulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorSavePattern {
    /// `std::fs::write` — simple truncate-and-write.
    DirectWrite,
    /// Vim: write to `.foo.swp`, then rename over the target.
    VimAtomicRename,
    /// JetBrains: write to `___jb_tmp___` suffixed file, fsync, rename over target.
    JetBrainsAtomicSave,
    /// VS Code: rename target to `.bak`, write new file at target path, delete `.bak`.
    VscodeSafeSave,
    /// Emacs: write target in place, leave `target~` backup behind.
    EmacsBackup,
}

impl EditorSavePattern {
    /// Returns all five patterns for iteration.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::DirectWrite,
            Self::VimAtomicRename,
            Self::JetBrainsAtomicSave,
            Self::VscodeSafeSave,
            Self::EmacsBackup,
        ]
    }
}

/// Simulates an editor save of `content` to `path` using the given pattern.
///
/// The target file (`path`) must already exist for patterns that rename or
/// back up the original (all except `DirectWrite` on a new file).
///
/// # Panics
///
/// Panics if any filesystem operation fails (test-only code).
pub fn simulate_save(path: &Path, content: &[u8], pattern: EditorSavePattern) {
    match pattern {
        EditorSavePattern::DirectWrite => {
            fs::write(path, content).expect("DirectWrite failed");
        }
        EditorSavePattern::VimAtomicRename => {
            // Vim writes to a swap file (.filename.swp), then renames it over
            // the target. The swap file name uses a leading dot.
            let file_name = path.file_name().expect("path must have filename");
            let swp_name = format!(".{}.swp", file_name.to_string_lossy());
            let swp_path = path.with_file_name(&swp_name);

            fs::write(&swp_path, content).expect("Vim: write swap failed");
            // Small delay to ensure events are ordered.
            std::thread::sleep(std::time::Duration::from_millis(5));
            fs::rename(&swp_path, path).expect("Vim: rename swap → target failed");
        }
        EditorSavePattern::JetBrainsAtomicSave => {
            // JetBrains writes to a ___jb_tmp___ suffixed file, fsyncs, then
            // renames to the target (and optionally renames old to ___jb_old___).
            let tmp_path = path.with_extension(format!(
                "{}___jb_tmp___",
                path.extension()
                    .map(|e| format!("{}.", e.to_string_lossy()))
                    .unwrap_or_default()
            ));
            // Rename target to ___jb_old___ first (like JetBrains does).
            let old_path = path.with_extension(format!(
                "{}___jb_old___",
                path.extension()
                    .map(|e| format!("{}.", e.to_string_lossy()))
                    .unwrap_or_default()
            ));

            if path.exists() {
                fs::rename(path, &old_path).expect("JetBrains: rename target → old failed");
            }
            fs::write(&tmp_path, content).expect("JetBrains: write tmp failed");
            // Simulate fsync.
            {
                let f = fs::File::open(&tmp_path).expect("JetBrains: open tmp for sync failed");
                f.sync_all().expect("JetBrains: fsync failed");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            fs::rename(&tmp_path, path).expect("JetBrains: rename tmp → target failed");
            // Clean up old file.
            if old_path.exists() {
                let _ = fs::remove_file(&old_path);
            }
        }
        EditorSavePattern::VscodeSafeSave => {
            // VS Code: rename original to .bak, write new content, delete .bak.
            let bak_path = path.with_extension(format!(
                "{}.bak",
                path.extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default()
            ));

            if path.exists() {
                fs::rename(path, &bak_path).expect("VSCode: rename target → bak failed");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            fs::write(path, content).expect("VSCode: write new content failed");
            std::thread::sleep(std::time::Duration::from_millis(5));
            if bak_path.exists() {
                let _ = fs::remove_file(&bak_path);
            }
        }
        EditorSavePattern::EmacsBackup => {
            // Emacs: copy original to backup (target~), then write target.
            let backup_path = path.with_extension(format!(
                "{}~",
                path.extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default()
            ));

            // Copy original to backup.
            if path.exists() {
                fs::copy(path, &backup_path).expect("Emacs: copy to backup failed");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            // Write new content to target.
            fs::write(path, content).expect("Emacs: write target failed");
        }
    }
}
