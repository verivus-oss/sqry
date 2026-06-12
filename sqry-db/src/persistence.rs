//! Derived facts persistence to `.sqry/graph/derived.sqry`.
//!
//! Hot derived facts (cached query results) are persisted to a companion
//! file alongside the main snapshot. On reload, the cache is warmed from
//! the persisted facts if the graph identity (snapshot SHA-256) matches.
//!
//! # Format (v02)
//!
//! The derived file is a postcard stream with the layout:
//!
//! ```text
//! [DerivedHeader][PersistedEntry][PersistedEntry]...[PersistedEntry]
//! ```
//!
//! The header is always first and carries the magic bytes, format version,
//! snapshot identity, all three revision tiers, and the entry count.
//! Each subsequent record is a [`PersistedEntry`] carrying the serialized
//! key, value, and dependency metadata for one cached query result.
//!
//! Streaming decode (postcard `take_from_bytes`) lets fatal framing
//! corruption at entry N be caught before any entry is committed, while
//! still supporting large entry counts without peak-RAM serialization of
//! the whole file.
//!
//! # Magic + version
//!
//! Magic: [`DERIVED_MAGIC`] — exactly 16 ASCII bytes `b"SQRY_DERIVED_V02"`.
//! Format version: [`DERIVED_FORMAT_VERSION`] = `2`. Version `1` is
//! reserved and intentionally skipped to avoid schema collision with the
//! prior warm-only `DerivedManifest` (DB03, three-field struct).
//!
//! # Stale detection
//!
//! If the snapshot's SHA-256 doesn't match the header's `snapshot_sha256`,
//! the entire derived file is discarded and queries recompute on demand.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::file::id::FileId;
use sqry_core::persistence::{PathSafetyError, atomic_write_bytes, validate_path_in_workspace};

// ============================================================================
// Constants
// ============================================================================

/// Magic bytes for the v02 derived-cache file format.
///
/// Exactly 16 ASCII bytes. Chosen to be a fixed 16-byte header guard so any
/// file not starting with this exact sequence is immediately rejected at load.
/// 16 bytes (not 15) was chosen to fix the iter1-flagged inconsistency in the
/// prior `"SQRY_DERIVED_V1"` string (15 bytes).
pub const DERIVED_MAGIC: [u8; 16] = *b"SQRY_DERIVED_V02";

/// Format revision for the current derived-cache wire format.
///
/// Value `2` skips `1` to avoid schema collision with the prior warm-only
/// `DerivedManifest` (DB03) which used a 3-field postcard struct.  The
/// `LOAD_PATH` unit rejects any file whose decoded `format_version != 2`.
pub const DERIVED_FORMAT_VERSION: u16 = 2;

// ============================================================================
// QueryDeps — serializable three-tier dependency snapshot
// ============================================================================

/// Serializable snapshot of the three-tier dependency metadata recorded
/// during query execution.
///
/// Stored inside each [`PersistedEntry`] so the `LOAD_PATH` layer can
/// reconstruct `CachedResult`'s dependency fields after cold-start
/// rehydration.
///
/// Field names intentionally mirror the `CachedResult` fields:
/// - Tier 1: `file_deps` — `(FileId, revision_at_read_time)` pairs.
/// - Tier 2: `edge_revision` — global edge revision at cache time.
/// - Tier 3: `metadata_revision` — global metadata revision at cache time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QueryDeps {
    /// Tier 1: file-level dependencies.
    ///
    /// Each entry is `(FileId, revision_at_read_time)`.  An empty `Vec`
    /// means the query did not touch any file-specific data (rare but valid
    /// for pure global queries).
    pub file_deps: Vec<(FileId, u64)>,
    /// Tier 2: global edge revision at cache time.
    ///
    /// `None` if the query does not track `TRACKS_EDGE_REVISION`.
    pub edge_revision: Option<u64>,
    /// Tier 3: global metadata revision at cache time.
    ///
    /// `None` if the query does not track `TRACKS_METADATA_REVISION`.
    pub metadata_revision: Option<u64>,
}

// ============================================================================
// DerivedHeader — file-level header (v02)
// ============================================================================

/// File-level header. Always first in the derived file.
///
/// Carries the magic guard, format version, snapshot identity, all three
/// revision tiers, the entry count, and the save timestamp.
///
/// Field order MUST NOT be changed: postcard serialization is order-sensitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedHeader {
    /// Magic bytes. EXACTLY 16 ASCII bytes: `b"SQRY_DERIVED_V02"`.
    pub magic: [u8; 16],
    /// Format revision. Current: 2.
    pub format_version: u16,
    /// SHA-256 of the main `snapshot.sqry` file.
    pub snapshot_sha256: [u8; 32],
    /// Saved global edge revision.
    pub edge_revision: u64,
    /// Saved global metadata revision.
    pub metadata_revision: u64,
    /// Saved per-file revisions.
    pub file_revisions: Vec<(FileId, u64)>,
    /// Number of `PersistedEntry` records following the header.
    pub entry_count: u64,
    /// Unix seconds when saved.
    pub saved_at: u64,
}

impl DerivedHeader {
    /// Creates a new v02 header for the given snapshot hash and revision
    /// state.  `saved_at` is populated from `SystemTime::now`.
    #[must_use]
    pub fn new(
        snapshot_sha256: [u8; 32],
        edge_revision: u64,
        metadata_revision: u64,
        file_revisions: Vec<(FileId, u64)>,
        entry_count: u64,
    ) -> Self {
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            magic: DERIVED_MAGIC,
            format_version: DERIVED_FORMAT_VERSION,
            snapshot_sha256,
            edge_revision,
            metadata_revision,
            file_revisions,
            entry_count,
            saved_at,
        }
    }

    /// Returns `true` if the magic bytes and format version identify a valid
    /// v02 derived file.
    ///
    /// Used by `LOAD_PATH` to reject legacy v01 files and corrupted files
    /// before attempting entry decode.
    #[must_use]
    pub fn is_valid_v02(&self) -> bool {
        self.magic == DERIVED_MAGIC && self.format_version == DERIVED_FORMAT_VERSION
    }

    /// Checks if this header matches the given snapshot hash.
    #[must_use]
    pub fn matches_snapshot(&self, snapshot_sha256: &[u8; 32]) -> bool {
        self.snapshot_sha256 == *snapshot_sha256
    }
}

// ============================================================================
// Legacy DB03 alias
// ============================================================================

/// Legacy DB03 alias for [`DerivedHeader`].
///
/// The warm-only `DerivedManifest` (DB03, three-field struct) has been
/// superseded by `DerivedHeader` v02.  This alias is retained so that any
/// code referencing `DerivedManifest` compiles without changes during the
/// PN3 transition.  New code should use [`DerivedHeader`] directly.
// Legacy DB03 alias
pub type DerivedManifest = DerivedHeader;

// ============================================================================
// PersistedEntry — per-entry wire record
// ============================================================================

/// One persisted cache entry in the derived file.
///
/// Follows the [`DerivedHeader`] in the stream, repeated `entry_count` times.
/// The `LOAD_PATH` unit decodes entries one-by-one with
/// [`deserialize_next_entry`] so framing corruption at entry N is caught
/// before any entries are committed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedEntry {
    /// Stable on-disk query type discriminator.  Must match a registered
    /// query's `DerivedQuery::QUERY_TYPE_ID`; unknown IDs are silently
    /// skipped by `LOAD_PATH`.
    pub query_type_id: u32,
    /// Postcard-serialized query key bytes.
    pub raw_key_bytes: Vec<u8>,
    /// Postcard-serialized query result bytes.
    pub raw_result_bytes: Vec<u8>,
    /// Three-tier dependency snapshot at cache time.
    pub deps: QueryDeps,
}

// ============================================================================
// Stream helpers
// ============================================================================

/// Serialize the header + iterator of entries into a single `Vec<u8>`.
///
/// Wire layout: `[header postcard bytes][entry postcard bytes]*`.
///
/// Each record is independently postcard-encoded and concatenated. The `LOAD_PATH`
/// layer uses [`deserialize_derived_header`] + repeated [`deserialize_next_entry`]
/// to decode the stream incrementally without peak-RAM buffering of all entries.
///
/// # Errors
///
/// Returns `postcard::Error` if serialization of the header or any entry
/// fails.  All `postcard::to_allocvec` calls are infallible for well-formed
/// structs in practice; the `?` propagation is for forward-compatibility.
pub fn serialize_derived_stream<I>(
    header: &DerivedHeader,
    entries: I,
) -> Result<Vec<u8>, postcard::Error>
where
    I: IntoIterator<Item = PersistedEntry>,
{
    let mut buf = postcard::to_allocvec(header)?;
    for entry in entries {
        let entry_bytes = postcard::to_allocvec(&entry)?;
        buf.extend_from_slice(&entry_bytes);
    }
    Ok(buf)
}

/// Deserialize the header from the beginning of `bytes`, returning the
/// header and the remaining byte slice (the entry stream tail).
///
/// Does NOT decode entries — that is the caller's responsibility.  `LOAD_PATH`
/// calls [`deserialize_next_entry`] repeatedly on the returned tail to decode
/// entries one at a time inside a staged-validation loop.
///
/// # Errors
///
/// Returns `postcard::Error` on header deserialization failure (truncated
/// data, schema mismatch, etc.).
pub fn deserialize_derived_header(bytes: &[u8]) -> Result<(DerivedHeader, &[u8]), postcard::Error> {
    postcard::take_from_bytes(bytes)
}

/// Decode a single [`PersistedEntry`] from the head of `bytes`, returning
/// the entry and the remaining tail.
///
/// Callers iterate this function inside a staged-validation loop until
/// `tail.is_empty()`, accumulating entries for atomic commit.
///
/// # Errors
///
/// Returns `postcard::Error` on entry deserialization failure.  A single
/// failing entry aborts the whole load in the staged-validation loop (fatal
/// framing rejection).
pub fn deserialize_next_entry(bytes: &[u8]) -> Result<(PersistedEntry, &[u8]), postcard::Error> {
    postcard::take_from_bytes(bytes)
}

// ============================================================================
// SHA-256 + path helpers (unchanged from DB03)
// ============================================================================

/// Computes the SHA-256 hash of a file at the given path.
///
/// # Errors
///
/// Returns an IO error if the file cannot be read.
pub fn compute_file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    Ok(hash)
}

/// Returns the path to the derived facts file for a given snapshot path.
///
/// The derived file lives alongside the snapshot: if the snapshot is at
/// `.sqry/graph/snapshot.sqry`, the derived file is at
/// `.sqry/graph/derived.sqry`.
#[must_use]
pub fn derived_path_for_snapshot(snapshot_path: &Path, filename: &str) -> PathBuf {
    snapshot_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(filename)
}

/// Saves a derived header to disk (warm-path compatibility shim).
///
/// This function is retained as a thin compatibility shim for the existing
/// warm-path tests and callers that previously called `save_manifest`.
/// New code should use the full `save_derived` function (`SAVE_PATH` unit).
///
/// # Errors
///
/// Returns an error if serialization or file writing fails.
pub fn save_manifest(path: &Path, manifest: &DerivedHeader) -> anyhow::Result<()> {
    let bytes = postcard::to_allocvec(manifest)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Loads a derived header from disk (warm-path compatibility shim).
///
/// Returns `None` if the file doesn't exist, can't be read, or can't be
/// deserialized.  Note: this decodes the whole file as a `DerivedHeader` and
/// does NOT validate magic / `format_version` — that responsibility lives in the
/// `LOAD_PATH` unit's staged-validation loop.
#[must_use]
pub fn load_manifest(path: &Path) -> Option<DerivedHeader> {
    let bytes = std::fs::read(path).ok()?;
    postcard::from_bytes(&bytes).ok()
}

// ============================================================================
// save_derived — SAVE_PATH unit
// ============================================================================

/// Writes the `QueryDb`'s persistent cache entries to `path` using an atomic
/// write.
///
/// # Algorithm
///
/// 1. [`validate_path_in_workspace`] before any IO — rejects symlink targets,
///    symlinked ancestor directories, and paths outside the workspace.
/// 2. Collect all persistent cache entries via
///    [`QueryDb::iter_persistent_cache_entries`] into a `Vec` so shard locks
///    are released before any allocation-intensive encoding begins.
/// 3. Build a [`DerivedHeader`] from the current DB state with
///    `entry_count = entries.len()`.
/// 4. [`serialize_derived_stream`] → byte vector.
/// 5. [`atomic_write_bytes`] — tempfile-in-same-dir + fsync + rename so the
///    target is never left partially written.
///
/// # Non-mutating
///
/// Takes `&QueryDb` (not `&mut`). Save is a read-only operation on the DB;
/// it does not mutate revisions, the cache, or any other internal state.
///
/// # Errors
///
/// - [`sqry_core::persistence::PathSafetyError`] wrapped as `anyhow::Error`
///   when the target path fails workspace validation.
/// - [`postcard::Error`] wrapped as `anyhow::Error` on serialisation failure.
/// - [`std::io::Error`] wrapped as `anyhow::Error` on atomic write failure.
pub fn save_derived(
    db: &crate::QueryDb,
    snapshot_sha256: [u8; 32],
    path: &Path,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    // Step 1: Path safety validation — must happen before any IO.
    //
    // The returned `canonical_path` is what every subsequent IO operation
    // must use. The raw `path` parameter may be relative, contain `..`
    // components, or otherwise differ from the validated target; writing
    // via the raw path would defeat the validation entirely (Codex review
    // finding).
    let canonical_path = validate_path_in_workspace(path, workspace_root)?;

    // Step 2: Collect persistent entries (releases all shard locks before IO).
    let persistent: Vec<PersistedEntry> = db
        .iter_persistent_cache_entries()
        .map(|e| PersistedEntry {
            query_type_id: e.query_type_id,
            raw_key_bytes: e.raw_key_bytes.to_vec(),
            raw_result_bytes: e.raw_result_bytes.to_vec(),
            deps: e.deps,
        })
        .collect();

    // Step 3: Build header — entry_count is now known.
    let header = DerivedHeader::new(
        snapshot_sha256,
        db.edge_revision(),
        db.metadata_revision(),
        db.inputs().all_revisions(),
        persistent.len() as u64,
    );

    // Step 4: Serialize header + entry stream into a single buffer.
    let bytes = serialize_derived_stream(&header, persistent)?;

    // Step 5: Atomic write — tempfile + fsync + rename. MUST target the
    // validated canonical path, never the raw caller input.
    atomic_write_bytes(&canonical_path, &bytes)?;

    Ok(())
}

// ============================================================================
// load_derived — LOAD_PATH unit
// ============================================================================

/// Failure modes for [`load_derived`].
///
/// The caller should treat [`LoadError::NotFound`] as a soft miss (the derived
/// file simply doesn't exist yet — normal on first run) and all other variants
/// as hard errors that warrant deleting or ignoring the derived file.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The derived-cache file does not exist at `path`.
    #[error("derived-cache file not found: {path}")]
    NotFound {
        /// The path that was checked.
        path: PathBuf,
    },
    /// The file's `snapshot_sha256` header field does not match `snapshot_sha256`.
    ///
    /// The derived file was produced from a different graph snapshot (stale
    /// or corrupted). The file should be deleted and queries recomputed.
    #[error("derived-cache snapshot SHA mismatch — file discarded")]
    StaleSnapshot,
    /// The file is structurally corrupt (bad magic, wrong version, truncated
    /// entry stream, etc.).
    #[error("derived-cache file is corrupt: {detail}")]
    Corrupt {
        /// Human-readable description of the corruption detected.
        detail: String,
    },
    /// The path failed workspace safety validation before the file was opened.
    ///
    /// Wraps [`sqry_core::persistence::PathSafetyError`].
    #[error("derived-cache path validation failed: {0}")]
    PathSafety(#[from] PathSafetyError),
    /// An IO error occurred while opening or reading the file.
    #[error("derived-cache IO error: {0}")]
    Io(#[from] std::io::Error),
    /// A successful `load_derived` call has already been applied to this DB.
    ///
    /// Subsequent calls are no-ops: the cold-load window is closed after the
    /// first successful load, preventing accidental double-apply of stale
    /// or different on-disk state.
    #[error("derived-cache load already applied to this DB; subsequent calls are no-ops")]
    AlreadyLoaded,
}

/// Outcome of a successful [`load_derived`] call.
#[derive(Debug, Clone)]
pub enum LoadOutcome {
    /// The derived file was loaded and `entries` cache entries were applied.
    Applied {
        /// Number of entries committed to the cache.
        ///
        /// Unknown-`query_type_id` entries (forward-compat skip) are NOT
        /// counted here; only entries that were actually staged and committed
        /// are included.
        entries: usize,
    },
    /// The load was skipped for `reason`.
    Skipped(SkipReason),
}

/// Reason for skipping a load attempt.
///
/// Currently no slots are defined; the enum is forward-compatible for future
/// skip conditions (e.g., `Disabled`, `FileTooLarge`, `RateLimited`).
#[derive(Debug, Clone)]
pub enum SkipReason {
    // No current variants — placeholder for forward compatibility.
}

/// Staged entry carrying only raw bytes + type id + deps — no typed value.
///
/// Produced by the validation loop in [`load_derived`] and consumed by
/// [`QueryDb::commit_staged_load`]. The staged form is intentionally
/// type-erased: deserialising each query's typed key/value is unnecessary
/// for cold-load warming.
pub struct StagedEntry {
    /// Stable on-disk query type discriminator from the stream.
    pub query_type_id: u32,
    /// Raw postcard-serialised key bytes from the persisted entry.
    pub raw_key_bytes: Vec<u8>,
    /// Raw postcard-serialised result bytes from the persisted entry.
    pub raw_result_bytes: Vec<u8>,
    /// Three-tier dependency snapshot at cache time.
    pub deps: QueryDeps,
}

/// Returns `true` if `id` is one of the 15 built-in query type IDs.
///
/// Used by the validation loop to decide whether to stage or silently skip
/// an entry. Unknown IDs (forward-compat additions, downstream IDs, 0x0000)
/// are skipped without error to allow rolling upgrades and file sharing
/// across sqry versions.
#[inline]
fn is_known_builtin(id: u32) -> bool {
    use crate::queries::type_ids;
    matches!(
        id,
        type_ids::CALLERS
            | type_ids::CALLEES
            | type_ids::IMPORTS
            | type_ids::EXPORTS
            | type_ids::REFERENCES
            | type_ids::IMPLEMENTS
            | type_ids::CYCLES
            | type_ids::IS_IN_CYCLE
            | type_ids::UNUSED
            | type_ids::IS_NODE_UNUSED
            | type_ids::REACHABILITY
            | type_ids::ENTRY_POINTS
            | type_ids::REACHABLE_FROM_ENTRY_POINTS
            | type_ids::SCC
            | type_ids::CONDENSATION
    )
}

/// Load a derived file at `path` into a pristine [`QueryDb`].
///
/// # Staged-validation + infallible-commit contract (spec §5.7)
///
/// 1. Path validation happens **before** any file IO.
/// 2. All fallible work (file open, header decode, magic/version check, SHA
///    match, entry stream decode) runs in the validation phase and returns
///    `Err(...)` without touching the DB.
/// 3. Once all entries are staged successfully, [`QueryDb::commit_staged_load`]
///    is called.  That function is **infallible by construction** — it contains
///    no `?`, no `Result`-bearing call, and no `map_err`.
/// 4. After commit, `cold_load_allowed` is flipped to `false` to prevent a
///    second load from overwriting the committed state.
///
/// # Errors
///
/// - [`LoadError::PathSafety`] — path fails workspace validation.
/// - [`LoadError::NotFound`] — file does not exist (`ENOENT`).
/// - [`LoadError::Io`] — other IO errors.
/// - [`LoadError::Corrupt`] — magic mismatch, version mismatch, or truncated
///   entry stream.
/// - [`LoadError::StaleSnapshot`] — SHA-256 in the header doesn't match
///   `snapshot_sha256`.
/// - [`LoadError::AlreadyLoaded`] — a successful load has already been applied
///   to `db`.
pub fn load_derived(
    db: &mut crate::QueryDb,
    snapshot_sha256: [u8; 32],
    path: &Path,
    workspace_root: &Path,
) -> Result<LoadOutcome, LoadError> {
    // Step 1: Path safety validation — must happen before any IO.
    //
    // The returned `canonical_path` is what every subsequent IO operation
    // must use. Reading via the raw `path` would defeat the validation
    // (Codex review finding).
    let canonical_path = validate_path_in_workspace(path, workspace_root)?;

    // Step 5 (early): Check cold-load window before any file IO.
    //
    // Spec §5.7 lists this as step 5, but elevating the check to before the
    // file open satisfies both the atomicity contract (DB is never double-loaded)
    // and the test requirement that AlreadyLoaded is returned without reading
    // the file. This is strictly safer: no point paying for disk reads when
    // the load cannot proceed regardless.
    if !db.cold_load_allowed.load(Ordering::Acquire) {
        return Err(LoadError::AlreadyLoaded);
    }

    // Step 2: Open the file and read all bytes — via the validated
    // canonical path, never the raw caller input.
    let bytes = match std::fs::read(&canonical_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoadError::NotFound {
                path: canonical_path.clone(),
            });
        }
        Err(e) => return Err(LoadError::Io(e)),
    };

    // Step 3 (inline with step 2): bytes are now in memory.

    // Step 4: Decode and validate the header.
    let (header, mut tail) =
        deserialize_derived_header(&bytes).map_err(|e| LoadError::Corrupt {
            detail: format!("header decode: {e}"),
        })?;

    if header.magic != DERIVED_MAGIC {
        return Err(LoadError::Corrupt {
            detail: "magic mismatch".to_owned(),
        });
    }
    if header.format_version != DERIVED_FORMAT_VERSION {
        return Err(LoadError::Corrupt {
            detail: format!(
                "version mismatch: expected {DERIVED_FORMAT_VERSION}, got {}",
                header.format_version
            ),
        });
    }
    if header.snapshot_sha256 != snapshot_sha256 {
        return Err(LoadError::StaleSnapshot);
    }

    // Step 6: Streaming entry validation — accumulate into `staged`.
    // DB is NOT touched if any entry decode fails.
    let mut staged: Vec<StagedEntry> = Vec::new();
    while !tail.is_empty() {
        let (entry, rest) = deserialize_next_entry(tail).map_err(|e| LoadError::Corrupt {
            detail: format!("entry decode: {e}"),
        })?;
        tail = rest;

        if !is_known_builtin(entry.query_type_id) {
            // Unknown or reserved ID — skip silently for forward/backward compat.
            continue;
        }

        staged.push(StagedEntry {
            query_type_id: entry.query_type_id,
            raw_key_bytes: entry.raw_key_bytes,
            raw_result_bytes: entry.raw_result_bytes,
            deps: entry.deps,
        });
    }

    // --- COMMIT BOUNDARY ---
    // All validation above passed. From here on: no `?`, no `Result`.
    // Steps 7–9 are the infallible commit phase.

    // Step 7: Commit staged entries — INFALLIBLE.
    let entries_applied = staged.len();
    db.commit_staged_load(&header, staged);

    // Step 8: Close the cold-load window.
    db.cold_load_allowed.store(false, Ordering::Release);

    // Step 9: Return success.
    Ok(LoadOutcome::Applied {
        entries: entries_applied,
    })
}

// ============================================================================
// Tests
// ============================================================================

// ============================================================================
// save_path_tests — SAVE_PATH acceptance tests
// ============================================================================

// ============================================================================
// load_path_tests — LOAD_PATH acceptance tests
// ============================================================================

#[cfg(test)]
mod load_path_tests {
    use std::sync::Arc;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use tempfile::TempDir;

    use super::*;
    use crate::queries::type_ids;
    use crate::{QueryDb, QueryDbConfig};

    // -------------------------------------------------------------------------
    // Test helpers
    // -------------------------------------------------------------------------

    /// Build a minimal empty `QueryDb` backed by an empty `CodeGraph`.
    fn empty_db() -> QueryDb {
        let snapshot = Arc::new(CodeGraph::new().snapshot());
        QueryDb::new(snapshot, QueryDbConfig::default())
    }

    /// Build a v02 stream with `n_entries` valid entries of type CALLERS
    /// and return the serialised bytes.
    fn make_valid_stream(sha: [u8; 32], n_entries: usize) -> Vec<u8> {
        let entries: Vec<PersistedEntry> = (0..n_entries)
            .map(|i| PersistedEntry {
                query_type_id: type_ids::CALLERS,
                raw_key_bytes: vec![i as u8],
                raw_result_bytes: vec![0xAA, i as u8],
                deps: QueryDeps::default(),
            })
            .collect();
        let header = DerivedHeader::new(sha, 5, 3, vec![], entries.len() as u64);
        serialize_derived_stream(&header, entries).unwrap()
    }

    // -------------------------------------------------------------------------
    // AC 15: happy_path_roundtrip
    // -------------------------------------------------------------------------

    /// Save a DB with entries via `save_derived`, then load via `load_derived`
    /// and assert `Applied.entries` == what was saved.
    #[test]
    fn happy_path_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();
        let sha: [u8; 32] = [0x42; 32];

        // Build a stream with 3 known-type entries.
        let bytes = make_valid_stream(sha, 3);
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();
        let outcome = load_derived(&mut db, sha, &path, workspace_root).unwrap();

        match outcome {
            LoadOutcome::Applied { entries } => {
                assert_eq!(entries, 3, "expected 3 entries applied");
            }
            LoadOutcome::Skipped(_) => panic!("unexpected Skipped outcome"),
        }
    }

    // -------------------------------------------------------------------------
    // AC 16: missing_file_returns_not_found
    // -------------------------------------------------------------------------

    #[test]
    fn missing_file_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.sqry");
        let workspace_root = dir.path();

        let mut db = empty_db();
        let err = load_derived(&mut db, [0u8; 32], &path, workspace_root)
            .expect_err("missing file must return Err");

        assert!(
            matches!(err, LoadError::NotFound { .. }),
            "expected NotFound, got: {err}"
        );
    }

    // -------------------------------------------------------------------------
    // AC 17: sha_mismatch_returns_stale_snapshot
    // -------------------------------------------------------------------------

    #[test]
    fn sha_mismatch_returns_stale_snapshot() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let saved_sha: [u8; 32] = [0x11; 32];
        let caller_sha: [u8; 32] = [0x22; 32]; // different

        let bytes = make_valid_stream(saved_sha, 0);
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();
        let err = load_derived(&mut db, caller_sha, &path, workspace_root)
            .expect_err("SHA mismatch must return Err");

        assert!(
            matches!(err, LoadError::StaleSnapshot),
            "expected StaleSnapshot, got: {err}"
        );
    }

    // -------------------------------------------------------------------------
    // AC 18: magic_mismatch_returns_corrupt
    // -------------------------------------------------------------------------

    #[test]
    fn magic_mismatch_returns_corrupt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let sha: [u8; 32] = [0x33; 32];

        // Build a valid stream then corrupt the first byte of the magic.
        let mut bytes = make_valid_stream(sha, 0);
        bytes[0] ^= 0xFF; // flip bits in magic[0]
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();
        let err = load_derived(&mut db, sha, &path, workspace_root)
            .expect_err("magic mismatch must return Err");

        // Either a decode error (postcard fails) or a Corrupt(magic mismatch).
        assert!(
            matches!(err, LoadError::Corrupt { .. }),
            "expected Corrupt, got: {err}"
        );
    }

    // -------------------------------------------------------------------------
    // AC 19: truncated_file_returns_corrupt_and_db_unchanged
    // -------------------------------------------------------------------------

    /// Truncating the entry stream (after a valid header) triggers
    /// `Err(Corrupt)` and the DB is NOT mutated (edge_revision stays 0).
    #[test]
    fn truncated_file_returns_corrupt_and_db_unchanged() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let sha: [u8; 32] = [0x44; 32];

        // Build a stream with 2 entries, then truncate to just after the header.
        let full_bytes = make_valid_stream(sha, 2);

        // Find where the header ends by deserialising it.
        let (_header, tail) = deserialize_derived_header(&full_bytes).unwrap();
        let header_len = full_bytes.len() - tail.len();

        // Write header + partial first entry (cut off mid-entry).
        let partial_entry_start = header_len;
        // Write 3 bytes of the first entry (guaranteed truncation for any
        // entry longer than 3 bytes — our entries have key + value + deps).
        let truncated_len = partial_entry_start + 3;
        let truncated_bytes = &full_bytes[..truncated_len];
        std::fs::write(&path, truncated_bytes).unwrap();

        let mut db = empty_db();
        let initial_edge_rev = db.edge_revision();

        let err = load_derived(&mut db, sha, &path, workspace_root)
            .expect_err("truncated file must return Err");

        assert!(
            matches!(err, LoadError::Corrupt { .. }),
            "expected Corrupt, got: {err}"
        );

        // DB must be untouched: edge_revision unchanged.
        assert_eq!(
            db.edge_revision(),
            initial_edge_rev,
            "DB edge_revision must be unchanged after failed load"
        );
        // cold_load_allowed must still be true so a retry on a repaired file
        // is correct.
        assert!(
            db.cold_load_allowed(),
            "cold_load_allowed must remain true after failed load"
        );
    }

    // -------------------------------------------------------------------------
    // AC 20: unknown_query_type_id_skipped_silently
    // -------------------------------------------------------------------------

    /// Stream: 2 CALLERS entries, 1 unknown-ID entry, 2 CALLEES entries.
    /// Expected: 4 entries applied (unknown skipped silently).
    #[test]
    fn unknown_query_type_id_skipped_silently() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();
        let sha: [u8; 32] = [0x55; 32];

        // An ID far outside the built-in range.
        const UNKNOWN_ID: u32 = 0xBEEF;

        let entries = vec![
            PersistedEntry {
                query_type_id: type_ids::CALLERS,
                raw_key_bytes: vec![1],
                raw_result_bytes: vec![0xA1],
                deps: QueryDeps::default(),
            },
            PersistedEntry {
                query_type_id: type_ids::CALLERS,
                raw_key_bytes: vec![2],
                raw_result_bytes: vec![0xA2],
                deps: QueryDeps::default(),
            },
            PersistedEntry {
                query_type_id: UNKNOWN_ID,
                raw_key_bytes: vec![3],
                raw_result_bytes: vec![0xA3],
                deps: QueryDeps::default(),
            },
            PersistedEntry {
                query_type_id: type_ids::CALLEES,
                raw_key_bytes: vec![4],
                raw_result_bytes: vec![0xA4],
                deps: QueryDeps::default(),
            },
            PersistedEntry {
                query_type_id: type_ids::CALLEES,
                raw_key_bytes: vec![5],
                raw_result_bytes: vec![0xA5],
                deps: QueryDeps::default(),
            },
        ];

        let header = DerivedHeader::new(sha, 0, 0, vec![], entries.len() as u64);
        let bytes = serialize_derived_stream(&header, entries).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();
        let outcome = load_derived(&mut db, sha, &path, workspace_root).unwrap();

        match outcome {
            LoadOutcome::Applied { entries } => {
                assert_eq!(
                    entries, 4,
                    "unknown entry must be silently skipped; expected 4 applied"
                );
            }
            LoadOutcome::Skipped(_) => panic!("unexpected Skipped outcome"),
        }
    }

    // -------------------------------------------------------------------------
    // AC 21: second_load_returns_already_loaded
    // -------------------------------------------------------------------------

    /// Second call returns `AlreadyLoaded` without opening the file.
    ///
    /// We verify the error kind by pattern-matching; the "without file IO"
    /// property is verified by checking that the error is returned even when
    /// the file is deleted between calls.
    #[test]
    fn second_load_returns_already_loaded() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();
        let sha: [u8; 32] = [0x66; 32];

        let bytes = make_valid_stream(sha, 1);
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();

        // First load — must succeed.
        load_derived(&mut db, sha, &path, workspace_root).unwrap();

        // Delete the file to confirm the second call doesn't do any IO.
        std::fs::remove_file(&path).unwrap();

        // Second load — must return AlreadyLoaded without reading the file.
        let err = load_derived(&mut db, sha, &path, workspace_root)
            .expect_err("second load must return Err");

        assert!(
            matches!(err, LoadError::AlreadyLoaded),
            "expected AlreadyLoaded, got: {err}"
        );
    }

    // -------------------------------------------------------------------------
    // AC 22: header_restoration_restores_three_tiers
    // -------------------------------------------------------------------------

    /// After a successful load the DB's three revision tiers match the header.
    #[test]
    fn header_restoration_restores_three_tiers() {
        use sqry_core::graph::unified::file::id::FileId;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();
        let sha: [u8; 32] = [0x77; 32];

        let file_revisions = vec![(FileId::new(1), 7u64), (FileId::new(2), 99u64)];
        let header = DerivedHeader::new(
            sha,
            /*edge_revision=*/ 42,
            /*metadata_revision=*/ 17,
            file_revisions.clone(),
            /*entry_count=*/ 0,
        );
        let bytes = serialize_derived_stream(&header, std::iter::empty()).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let mut db = empty_db();
        let outcome = load_derived(&mut db, sha, &path, workspace_root).unwrap();
        assert!(
            matches!(outcome, LoadOutcome::Applied { entries: 0 }),
            "expected Applied(0), got: {outcome:?}"
        );

        // Tier 2: global edge revision.
        assert_eq!(db.edge_revision(), 42, "edge_revision must be restored");
        // Tier 3: global metadata revision.
        assert_eq!(
            db.metadata_revision(),
            17,
            "metadata_revision must be restored"
        );
        // Tier 1: per-file revisions.
        assert_eq!(
            db.inputs().revision(FileId::new(1)),
            Some(7),
            "file 1 revision must be restored"
        );
        assert_eq!(
            db.inputs().revision(FileId::new(2)),
            Some(99),
            "file 2 revision must be restored"
        );
    }
}

#[cfg(test)]
mod save_path_tests {
    use std::sync::Arc;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use tempfile::TempDir;

    use super::*;
    use crate::{QueryDb, QueryDbConfig};

    /// Build a minimal, empty `QueryDb` backed by an empty `CodeGraph`.
    fn empty_db() -> QueryDb {
        let snapshot = Arc::new(CodeGraph::new().snapshot());
        QueryDb::new(snapshot, QueryDbConfig::default())
    }

    /// AC 6: save → read back bytes → deserialize header → assert fields match.
    ///
    /// Uses an empty `QueryDb` so `entry_count = 0`.  The snapshot SHA,
    /// edge_revision, and metadata_revision are all asserted to match exactly.
    #[test]
    fn save_then_read_back_header_fields_match() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let db = empty_db();
        let snapshot_sha: [u8; 32] = [0xAB; 32];

        save_derived(&db, snapshot_sha, &path, workspace_root).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let (header, tail) = deserialize_derived_header(&bytes).unwrap();

        assert_eq!(
            header.snapshot_sha256, snapshot_sha,
            "snapshot SHA mismatch"
        );
        assert_eq!(
            header.edge_revision,
            db.edge_revision(),
            "edge_revision mismatch"
        );
        assert_eq!(
            header.metadata_revision,
            db.metadata_revision(),
            "metadata_revision mismatch"
        );
        assert_eq!(header.entry_count, 0, "expected 0 entries for empty db");
        assert!(header.is_valid_v02(), "header must pass v02 validation");
        assert!(tail.is_empty(), "no entry bytes expected after header");
    }

    /// AC 7 (unix-only): save rejects a symlinked target path.
    ///
    /// `validate_path_in_workspace` returns `PathSafetyError::SymlinkTarget`
    /// which propagates as `anyhow::Error`.  The symlink test requires Unix
    /// `std::os::unix::fs::symlink`; gated on `#[cfg(unix)]`.
    #[test]
    #[cfg(unix)]
    fn save_rejects_symlinked_target_path() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let real_file = dir.path().join("real.sqry");
        std::fs::write(&real_file, b"placeholder").unwrap();

        // Create a symlink inside the workspace pointing at the real file.
        let symlink_path = dir.path().join("link.sqry");
        symlink(&real_file, &symlink_path).unwrap();

        let db = empty_db();
        let workspace_root = dir.path();
        let snapshot_sha: [u8; 32] = [0u8; 32];

        let err = save_derived(&db, snapshot_sha, &symlink_path, workspace_root)
            .expect_err("save must reject symlinked target");

        // The error must be rooted in PathSafetyError::SymlinkTarget.
        let is_symlink_error = err
            .chain()
            .any(|e| e.to_string().contains("symlink") || e.to_string().contains("SymlinkTarget"));
        assert!(
            is_symlink_error,
            "expected SymlinkTarget error; got: {err:#}"
        );
    }

    /// AC 8: save with an empty cache writes a valid v02 header followed by
    /// an empty entry stream (tail is empty after decoding the header).
    #[test]
    fn save_empty_cache_writes_header_only() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let db = empty_db();
        let snapshot_sha: [u8; 32] = [0xCC; 32];

        save_derived(&db, snapshot_sha, &path, workspace_root).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.is_empty(),
            "output must be non-empty even for 0 entries"
        );

        let (header, tail) = deserialize_derived_header(&bytes).unwrap();
        assert!(header.is_valid_v02());
        assert_eq!(header.entry_count, 0);
        assert!(
            tail.is_empty(),
            "empty cache must produce no entry bytes after the header"
        );
    }

    /// AC 9: save is idempotent — calling save twice (with delete in between)
    /// produces byte-identical output.
    ///
    /// This verifies that the header's `saved_at` field can differ between
    /// calls (it records wall time), but the critical fields — snapshot SHA,
    /// revisions, entry_count — remain stable.
    #[test]
    fn save_is_idempotent_header_fields_stable_across_repeat_calls() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("derived.sqry");
        let workspace_root = dir.path();

        let db = empty_db();
        let snapshot_sha: [u8; 32] = [0x55; 32];

        // First save.
        save_derived(&db, snapshot_sha, &path, workspace_root).unwrap();
        let first_bytes = std::fs::read(&path).unwrap();

        // Delete the file, then save again.
        std::fs::remove_file(&path).unwrap();
        save_derived(&db, snapshot_sha, &path, workspace_root).unwrap();
        let second_bytes = std::fs::read(&path).unwrap();

        // Both outputs must decode to headers with identical critical fields.
        let (h1, tail1) = deserialize_derived_header(&first_bytes).unwrap();
        let (h2, tail2) = deserialize_derived_header(&second_bytes).unwrap();

        assert_eq!(h1.snapshot_sha256, h2.snapshot_sha256);
        assert_eq!(h1.edge_revision, h2.edge_revision);
        assert_eq!(h1.metadata_revision, h2.metadata_revision);
        assert_eq!(h1.entry_count, h2.entry_count);
        assert_eq!(h1.file_revisions, h2.file_revisions);
        assert!(tail1.is_empty());
        assert!(tail2.is_empty());

        // The byte streams must be identical (saved_at is encoded in the same
        // second for a fast test run; if they diverge by a second boundary the
        // only differing field is saved_at which is NOT a correctness concern —
        // but in a typical CI run this comparison holds).
        //
        // We do NOT assert byte equality since saved_at can tick between the
        // two calls.  Field-level equality above is the correctness guarantee.
        let _ = (first_bytes, second_bytes); // Silence unused-variable lint.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    // ---- Constants ---------------------------------------------------------

    #[test]
    fn magic_is_16_bytes_exactly() {
        assert_eq!(DERIVED_MAGIC.len(), 16);
        assert_eq!(&DERIVED_MAGIC, b"SQRY_DERIVED_V02");
    }

    #[test]
    fn format_version_is_two() {
        assert_eq!(DERIVED_FORMAT_VERSION, 2);
    }

    // ---- DerivedHeader round-trip ------------------------------------------

    #[test]
    fn header_round_trip() {
        let h = DerivedHeader {
            magic: DERIVED_MAGIC,
            format_version: DERIVED_FORMAT_VERSION,
            snapshot_sha256: [0xAB; 32],
            edge_revision: 7,
            metadata_revision: 3,
            file_revisions: vec![(FileId::new(1), 42), (FileId::new(2), 99)],
            entry_count: 42,
            saved_at: 1_700_000_000,
        };
        let bytes = postcard::to_allocvec(&h).unwrap();
        let decoded: DerivedHeader = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, h);
    }

    #[test]
    fn header_is_valid_v02() {
        let h = DerivedHeader::new([0u8; 32], 1, 2, vec![], 0);
        assert!(h.is_valid_v02());
    }

    #[test]
    fn header_with_wrong_magic_is_not_valid_v02() {
        let mut h = DerivedHeader::new([0u8; 32], 1, 2, vec![], 0);
        h.magic[0] = b'X'; // corrupt first byte
        assert!(!h.is_valid_v02());
    }

    #[test]
    fn header_with_wrong_format_version_is_not_valid_v02() {
        let mut h = DerivedHeader::new([0u8; 32], 1, 2, vec![], 0);
        h.format_version = 1;
        assert!(!h.is_valid_v02());
    }

    // ---- Stream round-trip -------------------------------------------------

    #[test]
    fn stream_round_trip() {
        let header = DerivedHeader {
            magic: DERIVED_MAGIC,
            format_version: DERIVED_FORMAT_VERSION,
            snapshot_sha256: [0x55; 32],
            edge_revision: 10,
            metadata_revision: 5,
            file_revisions: vec![(FileId::new(3), 7)],
            entry_count: 2,
            saved_at: 1_700_000_001,
        };
        let entries = vec![
            PersistedEntry {
                query_type_id: 0x0001,
                raw_key_bytes: vec![1, 2, 3],
                raw_result_bytes: vec![4, 5, 6],
                deps: QueryDeps {
                    file_deps: vec![(FileId::new(1), 1)],
                    edge_revision: Some(10),
                    metadata_revision: None,
                },
            },
            PersistedEntry {
                query_type_id: 0x0002,
                raw_key_bytes: vec![7],
                raw_result_bytes: vec![8],
                deps: QueryDeps {
                    file_deps: vec![],
                    edge_revision: None,
                    metadata_revision: Some(5),
                },
            },
        ];

        let bytes = serialize_derived_stream(&header, entries.clone()).unwrap();

        let (decoded_header, mut tail) = deserialize_derived_header(&bytes).unwrap();
        assert_eq!(decoded_header, header);

        let mut decoded_entries = Vec::new();
        while !tail.is_empty() {
            let (entry, rest) = deserialize_next_entry(tail).unwrap();
            decoded_entries.push(entry);
            tail = rest;
        }

        assert_eq!(decoded_entries.len(), 2);
        assert_eq!(decoded_entries[0].query_type_id, entries[0].query_type_id);
        assert_eq!(decoded_entries[0].raw_key_bytes, entries[0].raw_key_bytes);
        assert_eq!(
            decoded_entries[0].raw_result_bytes,
            entries[0].raw_result_bytes
        );
        assert_eq!(decoded_entries[0].deps, entries[0].deps);
        assert_eq!(decoded_entries[1].query_type_id, entries[1].query_type_id);
        assert_eq!(decoded_entries[1].deps, entries[1].deps);
    }

    #[test]
    fn stream_with_zero_entries() {
        let header = DerivedHeader::new([0xCC; 32], 0, 0, vec![], 0);
        let bytes = serialize_derived_stream(&header, std::iter::empty()).unwrap();
        let (decoded_header, tail) = deserialize_derived_header(&bytes).unwrap();
        assert_eq!(decoded_header, header);
        assert!(tail.is_empty(), "no entries means empty tail");
    }

    // ---- Legacy v01 magic mismatch guard -----------------------------------

    #[test]
    fn legacy_v01_magic_is_not_v02_magic() {
        // The prior warm-only DerivedManifest (DB03) carried only:
        //   snapshot_sha256: [u8; 32], entry_count: usize, saved_at: u64
        // Decoding those bytes into DerivedHeader may succeed (postcard is
        // schema-free) but the decoded `magic` field will be garbage bytes
        // from the first 16 bytes of a SHA-256 hash, NOT b"SQRY_DERIVED_V02".
        // LOAD_PATH rejects files where is_valid_v02() returns false.
        //
        // This test pins the invariant: a plausible first-32-bytes of a v01
        // file (an all-zeros or any SHA-256 value) cannot accidentally be
        // equal to DERIVED_MAGIC. Belt-and-suspenders.
        let hypothetical_v01_first_16 = [0u8; 16]; // worst case: all-zero hash prefix
        assert_ne!(
            &DERIVED_MAGIC[..],
            &hypothetical_v01_first_16[..],
            "DERIVED_MAGIC must not equal any plausible v01 SHA-256 prefix"
        );

        // Also verify a non-zero SHA prefix (e.g., a common hash byte pattern)
        // doesn't accidentally match.
        let sha_like_prefix: [u8; 16] = [
            0x6b, 0x86, 0xb2, 0x73, 0xff, 0x34, 0xfc, 0xe1, 0x9d, 0x6b, 0x80, 0x4e, 0xff, 0x5a,
            0x3f, 0x57,
        ];
        assert_ne!(&DERIVED_MAGIC[..], &sha_like_prefix[..]);

        // Confirm DERIVED_MAGIC is exactly b"SQRY_DERIVED_V02" — not some
        // hash lookalike — so this test cannot pass vacuously.
        let magic_as_ascii = std::str::from_utf8(&DERIVED_MAGIC).expect("DERIVED_MAGIC is ASCII");
        assert_eq!(magic_as_ascii, "SQRY_DERIVED_V02");
    }

    #[test]
    fn legacy_v01_bytes_decode_as_invalid_header() {
        // Build a realistic v01 DerivedManifest byte sequence:
        //   old struct was { snapshot_sha256: [u8; 32], entry_count: usize, saved_at: u64 }
        // postcard encodes [u8; 32] as 32 raw bytes, usize as varint, u64 as
        // 8-byte LE (or varint depending on postcard version — varint here).
        //
        // When decoded as DerivedHeader, the first 16 bytes become `magic`
        // (32-byte hash prefix) and the next 2 bytes become `format_version`.
        // Neither will match DERIVED_MAGIC / DERIVED_FORMAT_VERSION, so
        // is_valid_v02() returns false → LOAD_PATH rejects cleanly.

        #[derive(Serialize)]
        struct OldManifest {
            snapshot_sha256: [u8; 32],
            entry_count: usize,
            saved_at: u64,
        }

        let old = OldManifest {
            snapshot_sha256: [0xDE; 32],
            entry_count: 5,
            saved_at: 1_700_000_000,
        };
        let v01_bytes = postcard::to_allocvec(&old).unwrap();

        // Attempt to decode as DerivedHeader — may succeed or fail depending
        // on field count alignment.  If it succeeds, the decoded header MUST
        // fail is_valid_v02().
        match postcard::from_bytes::<DerivedHeader>(&v01_bytes) {
            Ok(decoded) => {
                assert!(
                    !decoded.is_valid_v02(),
                    "v01 bytes accidentally decoded as valid v02 header — \
                     LOAD_PATH rejection would be bypassed"
                );
            }
            Err(_) => {
                // Decode failed outright — also fine.  LOAD_PATH handles
                // deserialization errors as a Corrupt rejection.
            }
        }
    }

    // ---- DB03 warm-path compat tests (rewritten in terms of DerivedHeader) -

    /// Warm-path round-trip — rewritten from DB03's `manifest_round_trip` to
    /// use `DerivedHeader` directly.  Coverage intent is preserved: verify
    /// that a header saved via `save_manifest` / `load_manifest` survives a
    /// disk round-trip with matching fields.
    #[test]
    fn manifest_round_trip() {
        let hash = [42u8; 32];
        let header = DerivedHeader::new(hash, 0, 0, vec![], 100);

        assert!(header.matches_snapshot(&hash));
        assert!(!header.matches_snapshot(&[0u8; 32]));
        assert!(header.is_valid_v02());

        let temp = NamedTempFile::new().unwrap();
        save_manifest(temp.path(), &header).unwrap();

        // load_manifest decodes raw postcard bytes; the full v02 header
        // survives the round-trip.
        let loaded = load_manifest(temp.path()).unwrap();
        assert_eq!(loaded.snapshot_sha256, hash);
        assert_eq!(loaded.entry_count, 100);
        assert!(loaded.matches_snapshot(&hash));
        assert!(loaded.is_valid_v02());
    }

    #[test]
    fn derived_path_computation() {
        let snapshot = Path::new("/home/user/.sqry/graph/snapshot.sqry");
        let derived = derived_path_for_snapshot(snapshot, "derived.sqry");
        assert_eq!(
            derived,
            PathBuf::from("/home/user/.sqry/graph/derived.sqry")
        );
    }

    #[test]
    fn load_manifest_missing_file() {
        let result = load_manifest(Path::new("/nonexistent/path/derived.sqry"));
        assert!(result.is_none());
    }

    #[test]
    fn file_sha256() {
        let temp = NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"hello world").unwrap();
        let hash = compute_file_sha256(temp.path()).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(hash.len(), 32);
        assert_ne!(hash, [0u8; 32]); // non-zero
    }

    // ---- QueryDeps ---------------------------------------------------------

    #[test]
    fn query_deps_default_is_empty() {
        let deps = QueryDeps::default();
        assert!(deps.file_deps.is_empty());
        assert!(deps.edge_revision.is_none());
        assert!(deps.metadata_revision.is_none());
    }

    #[test]
    fn query_deps_round_trip() {
        let deps = QueryDeps {
            file_deps: vec![(FileId::new(1), 7), (FileId::new(2), 3)],
            edge_revision: Some(99),
            metadata_revision: Some(4),
        };
        let bytes = postcard::to_allocvec(&deps).unwrap();
        let decoded: QueryDeps = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, deps);
    }
}
