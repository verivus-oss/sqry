//! Aggregate workspace status cache.
//!
//! The aggregate cache lives at
//! `<workspace_dir>/.sqry/workspace-cache/status.json` and answers
//! `sqry workspace status` / `sqry/indexStatus` queries that span the
//! entire [`super::logical::LogicalWorkspace`]. Per the §1.2 storage
//! contract the file is:
//!
//! - **Atomically written** via tempfile + rename, so a partially
//!   written file is never observed.
//! - **mtime-bound TTL** of [`CACHE_TTL`] (60 s). [`read_cache`] returns
//!   `Ok(None)` when the file is missing *or* older than the TTL, which
//!   the caller treats as a soft-miss and recomputes.
//!
//! The per-source-root snapshots continue to live at
//! `<source_root>/.sqry/graph/snapshot.sqry`; this file is **derived**
//! from those snapshots and may safely be deleted at any time.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::error::WorkspaceError;
use super::serde_time;

/// Subdirectory under `<workspace_dir>/.sqry/` that holds the aggregate
/// status cache. Kept as a const so the path is single-sourced.
pub const WORKSPACE_CACHE_DIRNAME: &str = "workspace-cache";

/// File name (under [`WORKSPACE_CACHE_DIRNAME`]) for the aggregate
/// status payload.
pub const WORKSPACE_STATUS_FILENAME: &str = "status.json";

/// Time-to-live for the aggregate status cache entry. Older files are
/// treated as a soft-miss by [`read_cache`].
pub const CACHE_TTL: Duration = Duration::from_secs(60);

/// Per-source-root status entry inside an aggregate
/// [`WorkspaceIndexStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRootStatus {
    /// Absolute path to the source root the entry describes.
    pub path: PathBuf,
    /// One-word machine-readable status:
    /// `"ok"` (indexed), `"missing"` (no snapshot), `"building"` (a
    /// rebuild is in progress), or `"error"` (snapshot read failed).
    pub status: SourceRootIndexState,
    /// Last-indexed timestamp for the source root, if available.
    #[serde(default, with = "serde_time::option")]
    pub last_indexed_at: Option<SystemTime>,
    /// Cached symbol count for the source root, if available.
    pub symbol_count: Option<u64>,
    /// `STEP_11_4` — JVM classpath directory for this source root, if
    /// the workspace builder populated
    /// [`crate::workspace::SourceRoot::classpath_dir`].
    /// `None` when the source root has no `<root>/.sqry/classpath/`
    /// directory or the workspace was built before `STEP_11_4`. Surfaces
    /// the auto-populated field through the LSP / MCP / CLI status
    /// payload from the same source-of-truth as the daemon's
    /// `daemon/workspaceStatus` `classpath_present: bool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classpath_dir: Option<PathBuf>,
}

/// Coarse status for a single source root inside an aggregate
/// [`WorkspaceIndexStatus`]. Kept deliberately small — the surface that
/// emits this is the LSP wire / `--json` output, where additional detail
/// (file counts, language hints, last-error) is folded into the
/// per-source-root [`crate::json_response::IndexStatus`] payload that
/// `sqry index --status` already returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceRootIndexState {
    /// Snapshot is present and considered healthy.
    Ok,
    /// No snapshot file exists at the canonical path.
    Missing,
    /// A `.sqry/graph/build.lock` is present — a rebuild is in progress.
    Building,
    /// Reading the snapshot or its metadata failed.
    Error,
}

/// Non-fatal warning attached to a [`WorkspaceIndexStatus`] aggregate.
///
/// `STEP_11_4` (workspace-aware-cross-repo, 2026-04-26) — adds the
/// "soft-failure" surface so cross-source-root macro expansion errors,
/// classpath probe failures, and similar partial-degradation events do
/// not have to escalate to a hard `Err` that masks the rest of the
/// workspace. Consumers (LSP / MCP / CLI) render the warnings inline
/// next to the per-source-root rollup; downstream tooling can decide
/// whether to treat them as advisory or as build-gating.
///
/// `#[non_exhaustive]` so future warning variants can be added without
/// breaking downstream pattern matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum WorkspaceWarning {
    /// Rust macro expansion against a logical-workspace source root
    /// produced an `InvalidWorkspaceRoot` error. Surfaces the affected
    /// source root and the underlying error detail. Macro expansion is
    /// degraded (the affected root contributes no expanded macro
    /// metadata) but the rest of the workspace still indexes cleanly.
    MacroExpansionInvalidRoot {
        /// Canonical absolute path of the affected source root.
        source_root: PathBuf,
        /// Error detail from
        /// `sqry_lang_rust::macro_expander::MacroExpandError::InvalidWorkspaceRoot`.
        detail: String,
    },
    /// Source root advertised a `<source_root>/.sqry/classpath/`
    /// directory but the directory could not be opened (permission
    /// error, racy unlink, etc.). The JVM classpath analyzer falls
    /// back to source-only mode for the affected root.
    ClasspathProbeFailed {
        /// Canonical absolute path of the affected source root.
        source_root: PathBuf,
        /// IO error detail.
        detail: String,
    },
}

/// Aggregate workspace-level status payload.
///
/// Field shape follows §1.4 of the implementation plan: a vector of
/// per-source-root statuses plus precomputed counts so the LSP / CLI
/// can render summary lines without re-iterating the vector.
///
/// `STEP_11_4` (workspace-aware-cross-repo, 2026-04-26) — adds the
/// `warnings` channel so non-fatal degradations (macro expansion root
/// errors, classpath probe failures, …) surface to LSP / MCP without
/// escalating to a hard build error. `warnings` is `Vec<WorkspaceWarning>`
/// so multiple warnings from independent source roots compose
/// deterministically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIndexStatus {
    /// Per-source-root entries (sorted by `path` for determinism).
    pub source_root_statuses: Vec<SourceRootStatus>,
    /// Number of source roots whose snapshot file is missing.
    pub missing_count: u32,
    /// Number of source roots currently being rebuilt.
    pub building_count: u32,
    /// Number of source roots reporting `ok`.
    pub ok_count: u32,
    /// Number of source roots reporting `error`.
    pub error_count: u32,
    /// Wall-clock time at which this aggregate was computed. Round-trips
    /// through the same millisecond encoding as
    /// [`crate::workspace::registry::WorkspaceMetadata`] timestamps.
    #[serde(with = "serde_time")]
    pub generated_at: SystemTime,
    /// Non-fatal warnings attached to this aggregate. Empty in the
    /// common case; populated by analyzers (macro expansion root,
    /// classpath probe, …) that hit a partial-degradation condition
    /// they want to surface to LSP / MCP / CLI consumers without
    /// failing the whole status response.
    ///
    /// `#[serde(default)]` so v1 cache files (which never carry the
    /// field) round-trip into an empty vec on read.
    /// `skip_serializing_if = "Vec::is_empty"` keeps the wire form
    /// identical to v1 in the common (no-warning) path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkspaceWarning>,
}

impl WorkspaceIndexStatus {
    /// Construct an aggregate from a vector of per-source-root statuses,
    /// recomputing the summary counters and stamping `generated_at` to
    /// `SystemTime::now()`.
    #[must_use]
    pub fn from_source_root_statuses(mut entries: Vec<SourceRootStatus>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut missing_count: u32 = 0;
        let mut building_count: u32 = 0;
        let mut ok_count: u32 = 0;
        let mut error_count: u32 = 0;
        for entry in &entries {
            match entry.status {
                SourceRootIndexState::Missing => missing_count = missing_count.saturating_add(1),
                SourceRootIndexState::Building => building_count = building_count.saturating_add(1),
                SourceRootIndexState::Ok => ok_count = ok_count.saturating_add(1),
                SourceRootIndexState::Error => error_count = error_count.saturating_add(1),
            }
        }
        Self {
            source_root_statuses: entries,
            missing_count,
            building_count,
            ok_count,
            error_count,
            generated_at: SystemTime::now(),
            warnings: Vec::new(),
        }
    }

    /// Total number of source roots covered.
    #[must_use]
    pub fn total(&self) -> u32 {
        u32::try_from(self.source_root_statuses.len()).unwrap_or(u32::MAX)
    }

    /// Append a [`WorkspaceWarning`] to the aggregate. Returns `&mut Self`
    /// for the same fluent-builder feel as
    /// [`Self::from_source_root_statuses`].
    ///
    /// Used by macro-boundary / classpath analyzers to surface non-fatal
    /// degradations through the same status payload the LSP and MCP
    /// already render.
    pub fn push_warning(&mut self, warning: WorkspaceWarning) -> &mut Self {
        self.warnings.push(warning);
        self
    }

    /// Returns `true` when the aggregate carries any
    /// [`WorkspaceWarning`]. Convenience for CLI / MCP renderers.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Resolve the absolute path to the aggregate-status cache file under
/// `workspace_dir`.
#[must_use]
pub fn cache_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir
        .join(".sqry")
        .join(WORKSPACE_CACHE_DIRNAME)
        .join(WORKSPACE_STATUS_FILENAME)
}

/// Read the aggregate status cache for `workspace_dir`.
///
/// Returns:
///
/// - `Ok(Some(status))` if the cache file exists and was last modified
///   within [`CACHE_TTL`].
/// - `Ok(None)` if the file is absent, older than the TTL, or has an
///   unreadable mtime (caller treats all three as soft-misses).
///
/// # Errors
///
/// Returns [`WorkspaceError::Io`] for filesystem failures other than
/// `NotFound`, and [`WorkspaceError::Serialization`] when the file is
/// present but not parseable as a [`WorkspaceIndexStatus`].
pub fn read_cache(workspace_dir: &Path) -> Result<Option<WorkspaceIndexStatus>, WorkspaceError> {
    let path = cache_path(workspace_dir);

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(WorkspaceError::io(&path, err)),
    };

    // mtime-bound TTL. If the mtime is unreadable we honour the
    // documented contract above: treat the entry as a soft-miss
    // (`Ok(None)`) rather than escalating an Io error. Clocks can
    // jump and the cache file is *derived* — recomputing is cheap and
    // always safe.
    //
    // The seam is exposed as a closure-shaped helper so test code can
    // inject an `Err` without having to fabricate a real filesystem
    // that returns an unreadable mtime (which is impossible to do
    // portably on Linux/macOS/Windows).
    let Ok(modified) = read_modified(&metadata) else {
        return Ok(None);
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age > CACHE_TTL {
        return Ok(None);
    }

    let bytes = fs::read(&path).map_err(|err| WorkspaceError::io(&path, err))?;
    let status: WorkspaceIndexStatus =
        serde_json::from_slice(&bytes).map_err(WorkspaceError::Serialization)?;
    Ok(Some(status))
}

/// Atomically persist `status` to the aggregate cache for
/// `workspace_dir`.
///
/// Writes to a sibling `<status>.json.tmp.<pid>.<nanos>` file inside the
/// cache directory and then renames over the canonical path, so a
/// concurrent reader either sees the previous payload or the new one in
/// full (no torn reads). Creates the cache directory if missing.
///
/// # Errors
///
/// Returns [`WorkspaceError::Io`] for filesystem failures and
/// [`WorkspaceError::Serialization`] when the payload cannot be encoded.
///
/// # Panics
///
/// Panics if [`cache_path`] returns a path without a parent directory,
/// which is structurally impossible — `cache_path` always returns a
/// path with at least three components (`<dir>/.sqry/workspace-cache/status.json`).
pub fn write_cache(
    workspace_dir: &Path,
    status: &WorkspaceIndexStatus,
) -> Result<(), WorkspaceError> {
    let path = cache_path(workspace_dir);
    let dir = path
        .parent()
        .expect("cache_path always returns a path with a parent");

    fs::create_dir_all(dir).map_err(|err| WorkspaceError::io(dir, err))?;

    let bytes = serde_json::to_vec_pretty(status).map_err(WorkspaceError::Serialization)?;

    let tmp_path = temp_sibling_path(&path);
    {
        // Inner scope so the file handle is closed before the rename.
        let mut file =
            fs::File::create(&tmp_path).map_err(|err| WorkspaceError::io(&tmp_path, err))?;
        file.write_all(&bytes)
            .map_err(|err| WorkspaceError::io(&tmp_path, err))?;
        file.sync_all()
            .map_err(|err| WorkspaceError::io(&tmp_path, err))?;
    }
    // `rename` is atomic within a single filesystem on Unix and on
    // modern Windows (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`,
    // which `std::fs::rename` uses). If the rename fails we tidy the
    // tempfile up before propagating the error so callers don't accrue
    // detritus on retry.
    if let Err(err) = fs::rename(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(WorkspaceError::io(&path, err));
    }
    Ok(())
}

/// Extract the mtime from `metadata`, with a test-only seam that lets
/// `aggregate_cache_returns_none_when_mtime_unreadable` simulate the
/// `Err` branch without having to fabricate a real filesystem on which
/// `metadata.modified()` fails (no such filesystem exists portably on
/// Linux / macOS / Windows). In production builds this is a thin
/// wrapper around [`fs::Metadata::modified`].
#[cfg(not(test))]
fn read_modified(metadata: &fs::Metadata) -> std::io::Result<SystemTime> {
    metadata.modified()
}

#[cfg(test)]
fn read_modified(metadata: &fs::Metadata) -> std::io::Result<SystemTime> {
    if test_hooks::FORCE_MTIME_UNREADABLE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "test hook: simulated unreadable mtime",
        ));
    }
    metadata.modified()
}

/// Test-only hooks for [`read_cache`]'s mtime seam. The flags here are
/// `pub(crate)` so the in-module `tests` block can flip them without
/// exposing the seam to downstream crates.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::atomic::AtomicBool;

    /// When `true`, `read_modified` returns `Err` regardless of the
    /// underlying `metadata.modified()` result. Drives the
    /// `aggregate_cache_returns_none_when_mtime_unreadable` test.
    pub(crate) static FORCE_MTIME_UNREADABLE: AtomicBool = AtomicBool::new(false);
}

/// Build a tempfile path that is a sibling of `path` so the eventual
/// rename stays on the same filesystem (a cross-FS `rename` is not
/// atomic on either Unix or Windows).
fn temp_sibling_path(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!(".tmp.{pid}.{nanos}"));
    let mut tmp = path.to_path_buf();
    tmp.set_file_name(name);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    fn sample_status() -> WorkspaceIndexStatus {
        WorkspaceIndexStatus::from_source_root_statuses(vec![
            SourceRootStatus {
                path: PathBuf::from("/ws/a"),
                status: SourceRootIndexState::Ok,
                last_indexed_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                symbol_count: Some(42),
                classpath_dir: None,
            },
            SourceRootStatus {
                path: PathBuf::from("/ws/b"),
                status: SourceRootIndexState::Missing,
                last_indexed_at: None,
                symbol_count: None,
                classpath_dir: None,
            },
        ])
    }

    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_writes_and_reads_under_ttl() {
        let temp = tempdir().unwrap();
        let status = sample_status();
        write_cache(temp.path(), &status).unwrap();
        let read = read_cache(temp.path())
            .unwrap()
            .expect("cache hit expected");
        assert_eq!(read.source_root_statuses, status.source_root_statuses);
        assert_eq!(read.ok_count, 1);
        assert_eq!(read.missing_count, 1);
    }

    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_returns_none_when_absent() {
        let temp = tempdir().unwrap();
        assert!(read_cache(temp.path()).unwrap().is_none());
    }

    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_returns_none_after_ttl() {
        let temp = tempdir().unwrap();
        let status = sample_status();
        write_cache(temp.path(), &status).unwrap();

        // Force the on-disk mtime backwards beyond CACHE_TTL by using
        // `filetime`-equivalent direct syscall. We can't depend on the
        // `filetime` crate from sqry-core, so we rely on `utimes` via
        // the `std::fs::File::set_modified` API that landed in 1.75.
        let path = cache_path(temp.path());
        let stale = SystemTime::now() - (CACHE_TTL + Duration::from_secs(5));
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(stale).unwrap();
        drop(f);

        assert!(read_cache(temp.path()).unwrap().is_none());
    }

    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_atomic_write_no_partial_files() {
        let temp = tempdir().unwrap();
        let status = sample_status();
        write_cache(temp.path(), &status).unwrap();
        // After a successful write, no `.tmp.*` files must remain in
        // the cache directory.
        let cache_dir = temp.path().join(".sqry").join(WORKSPACE_CACHE_DIRNAME);
        let leftovers: Vec<_> = fs::read_dir(&cache_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no tempfile leftovers, got {leftovers:?}"
        );
    }

    /// Stronger atomic-write coverage: a concurrent reader running
    /// alongside a tight writer loop must NEVER observe a half-written
    /// `status.json`. The single-shot test above proves we don't leave
    /// `.tmp.*` files behind on the happy path; this test proves the
    /// rename-into-place pattern is the *only* way readers see
    /// canonical content (no torn JSON, no truncated reads, no
    /// deserialise errors from a partial payload).
    ///
    /// Codex iter1 `APPROVE_WITH_CHANGES` required this stronger
    /// acceptance check for the §1.2 atomic-write contract.
    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_atomic_write_visible_only_complete() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;

        let temp = tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));

        // Pre-populate so the very first read can't fail with
        // `NotFound` — we want to assert the steady-state contract,
        // not the cold-start race.
        write_cache(&dir, &sample_status()).unwrap();

        let writer_stop = Arc::clone(&stop);
        let writer_dir = dir.clone();
        let writer = thread::spawn(move || {
            // Tight write loop: every iteration writes the full
            // payload through the same temp-then-rename path the
            // production code uses.
            while !writer_stop.load(Ordering::Relaxed) {
                write_cache(&writer_dir, &sample_status()).unwrap();
            }
        });

        // Read aggressively. The exact iteration count is tuned so
        // the test runs comfortably under 2 s on a typical dev
        // machine while still exercising thousands of read/write
        // interleavings.
        let reads: usize = 5_000;
        let mut hits: usize = 0;
        let mut misses: usize = 0;
        for _ in 0..reads {
            match read_cache(&dir) {
                // The point of this test is the deserialise contract,
                // not the cache-hit ratio: a `None` here means the
                // mtime was too old at the instant of the read (the
                // writer hadn't yet renamed the new payload into
                // place), which is also a legal observation. Either
                // way we MUST never see `Err`, because that would
                // imply a torn read or a half-written JSON payload
                // ever became visible to a reader.
                Ok(Some(s)) => {
                    // Spot-check that the payload is structurally
                    // complete — `from_source_root_statuses` is the
                    // only way to populate `source_root_statuses`,
                    // and the sample fixture always has 2 entries.
                    assert_eq!(
                        s.source_root_statuses.len(),
                        2,
                        "concurrent read returned an incomplete status payload",
                    );
                    hits += 1;
                }
                Ok(None) => misses += 1,
                Err(err) => panic!(
                    "read_cache observed a torn / partial status.json during \
                     concurrent writes: {err:?}"
                ),
            }
        }

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();

        // We expect overwhelmingly cache hits (the writer keeps the
        // mtime fresh), but tolerate a small misses tail to keep the
        // test stable across CI hosts. The acceptance criterion is
        // the absence of `Err` above.
        assert!(
            hits > 0,
            "expected at least some cache hits, got {hits} hits / {misses} misses"
        );
    }

    /// Codex iter1 `APPROVE_WITH_CHANGES` — confirms the documented
    /// `read_cache` contract: an unreadable mtime maps to
    /// `Ok(None)` (soft-miss), not `Err(WorkspaceError::Io)`. Driven
    /// by the `FORCE_MTIME_UNREADABLE` test seam in `read_modified`.
    #[test]
    #[serial(workspace_cache_read)]
    fn aggregate_cache_returns_none_when_mtime_unreadable() {
        use std::sync::atomic::Ordering;

        let temp = tempdir().unwrap();
        let status = sample_status();
        write_cache(temp.path(), &status).unwrap();

        // Sanity: with the seam disabled, the read returns Some.
        assert!(
            read_cache(temp.path()).unwrap().is_some(),
            "baseline read should hit"
        );

        // Flip the seam, re-read, expect Ok(None).
        test_hooks::FORCE_MTIME_UNREADABLE.store(true, Ordering::SeqCst);
        let result = read_cache(temp.path());
        // Reset before any assertion that might unwind, so a panic
        // does not leak the seam state into other tests.
        test_hooks::FORCE_MTIME_UNREADABLE.store(false, Ordering::SeqCst);

        assert!(
            matches!(result, Ok(None)),
            "unreadable mtime must yield Ok(None), got {result:?}"
        );
    }

    #[test]
    fn aggregate_status_summary_counts_match_entries() {
        let status = sample_status();
        assert_eq!(status.total(), 2);
        assert_eq!(status.ok_count, 1);
        assert_eq!(status.missing_count, 1);
        assert_eq!(status.building_count, 0);
        assert_eq!(status.error_count, 0);
    }
}
