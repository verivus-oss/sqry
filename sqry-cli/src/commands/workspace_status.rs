//! `sqry workspace status <path>` — aggregate index-status reporting.
//!
//! Routes the user-supplied workspace path through [`LogicalWorkspace`]
//! to obtain the canonical source-root list, then aggregates the
//! per-source-root index status into a [`WorkspaceIndexStatus`]. The
//! result is cached at
//! `<workspace>/.sqry/workspace-cache/status.json` with a 60-second
//! mtime-bound TTL — see [`sqry_core::workspace::cache`] for the
//! durability contract.

use anyhow::{Context, Result, anyhow};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sqry_core::workspace::{
    LogicalWorkspace, SourceRootIndexState, SourceRootStatus, WorkspaceIndexStatus,
    cache_path as workspace_cache_path, read_cache as read_status_cache,
    write_cache as write_status_cache,
};

use crate::args::Cli;
use crate::output::OutputStreams;

/// Filename that signals an in-progress index build (presence ⇒
/// `building`). The unified-graph build pipeline writes this lockfile
/// while [Phase 1–4 + Pass 5](../../../../CLAUDE.md) is running. We
/// inspect the file from outside the daemon, so a stale lock from a
/// crashed `sqry index` will look like `building` until the next clean
/// build — that mirrors the per-source-root contract today.
const BUILD_LOCK_FILENAME: &str = "build.lock";

/// Per the storage contract in §1.2 of the implementation plan, the
/// canonical snapshot path is `<source_root>/.sqry/graph/snapshot.sqry`.
const GRAPH_SUBDIR: &str = ".sqry";
const GRAPH_GRAPHDIR: &str = "graph";
const SNAPSHOT_FILENAME: &str = "snapshot.sqry";

/// Stable magic-byte prefix shared by every supported snapshot version
/// (`SQRY_GRAPH_V7` … `SQRY_GRAPH_V10`). See
/// `sqry-core/src/graph/unified/persistence/format.rs`. We only check
/// the family prefix here — the full version-aware integrity check
/// happens inside `sqry-core`'s loader; the goal of this surface is to
/// distinguish a healthy snapshot file from an obviously corrupt or
/// truncated one for the `SourceRootIndexState::Error` bucket.
const SNAPSHOT_MAGIC_PREFIX: &[u8] = b"SQRY_GRAPH_V";

/// Minimum bytes we require before treating a snapshot as plausibly
/// valid — long enough to hold the longest known prefix
/// (`SQRY_GRAPH_V10` is 14 bytes) plus one trailing byte.
const SNAPSHOT_MIN_VALID_BYTES: usize = SNAPSHOT_MAGIC_PREFIX.len() + 2;

/// Run `sqry workspace status <path>`.
///
/// # Errors
///
/// Surfaces any error from path canonicalization, registry parsing, or
/// cache I/O. Per-source-root failures (missing snapshot, unreadable
/// metadata) are folded into the aggregate as `Missing` / `Error`
/// entries rather than propagated.
pub fn run(cli: &Cli, workspace: &str, json: bool, no_cache: bool) -> Result<()> {
    let workspace_dir = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = workspace_dir.join(".sqry-workspace");

    let logical = if registry_path.exists() {
        LogicalWorkspace::from_sqry_workspace(&registry_path).map_err(|err| {
            anyhow!(
                "Failed to load workspace at {}: {err}",
                registry_path.display()
            )
        })?
    } else {
        // Fall back to a single-root workspace so `status` works even
        // before `sqry workspace init` runs. The cache directory still
        // lives under <workspace_dir>/.sqry/workspace-cache.
        LogicalWorkspace::single_root(workspace_dir.clone()).map_err(|err| {
            anyhow!(
                "Failed to derive single-root workspace at {}: {err}",
                workspace_dir.display()
            )
        })?
    };

    let status = if no_cache {
        compute_and_persist(&workspace_dir, &logical)
    } else {
        match read_status_cache(&workspace_dir).with_context(|| {
            format!(
                "Failed to read aggregate status cache at {}",
                workspace_cache_path(&workspace_dir).display()
            )
        })? {
            Some(cached) => cached,
            None => compute_and_persist(&workspace_dir, &logical),
        }
    };

    let mut streams = OutputStreams::with_pager(cli.pager_config());
    if json {
        let payload = render_json(&workspace_dir, &logical, &status);
        streams.write_result(&serde_json::to_string_pretty(&payload)?)?;
    } else {
        for line in render_text(&workspace_dir, &logical, &status) {
            streams.write_result(&line)?;
        }
    }
    streams.finish_checked()
}

/// Canonicalize an on-disk path or return a friendly error.
fn canonicalize_existing(path: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        candidate
            .canonicalize()
            .with_context(|| format!("Failed to resolve path {path}"))
    } else {
        Err(anyhow!("Path '{path}' does not exist"))
    }
}

/// Derive a per-source-root status from on-disk artefacts.
fn compute_source_root_status(source_root: &Path) -> SourceRootStatus {
    let graph_dir = source_root.join(GRAPH_SUBDIR).join(GRAPH_GRAPHDIR);
    let snapshot = graph_dir.join(SNAPSHOT_FILENAME);
    let lock = graph_dir.join(BUILD_LOCK_FILENAME);

    // `building` wins over `missing` and `ok`: a rebuild may produce a
    // fresh snapshot at any moment, so we surface the in-progress state
    // even if the previous snapshot is still on disk.
    if lock.exists() {
        return SourceRootStatus {
            path: source_root.to_path_buf(),
            status: SourceRootIndexState::Building,
            last_indexed_at: snapshot_modified_time(&snapshot),
            symbol_count: None,
            classpath_dir: probe_classpath_dir(source_root),
        };
    }

    match fs::metadata(&snapshot) {
        Ok(meta) => match snapshot_appears_valid(&snapshot) {
            Ok(true) => {
                let last_indexed_at = meta.modified().ok();
                SourceRootStatus {
                    path: source_root.to_path_buf(),
                    status: SourceRootIndexState::Ok,
                    last_indexed_at,
                    symbol_count: None,
                    classpath_dir: probe_classpath_dir(source_root),
                }
            }
            // Truncated, corrupt, or unreadable payload — distinct
            // from `Missing` (no file at all) and surfaced via the
            // `Error` bucket so operators see the failure.
            Ok(false) | Err(_) => SourceRootStatus {
                path: source_root.to_path_buf(),
                status: SourceRootIndexState::Error,
                last_indexed_at: None,
                symbol_count: None,
                classpath_dir: probe_classpath_dir(source_root),
            },
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => SourceRootStatus {
            path: source_root.to_path_buf(),
            status: SourceRootIndexState::Missing,
            last_indexed_at: None,
            symbol_count: None,
            classpath_dir: probe_classpath_dir(source_root),
        },
        Err(_) => SourceRootStatus {
            path: source_root.to_path_buf(),
            status: SourceRootIndexState::Error,
            last_indexed_at: None,
            symbol_count: None,
            classpath_dir: probe_classpath_dir(source_root),
        },
    }
}

/// STEP_11_4 — probe `<source_root>/.sqry/classpath/` for the JVM
/// classpath directory and return its absolute path when present.
/// Returns `None` when the directory is absent or the probe failed.
fn probe_classpath_dir(source_root: &Path) -> Option<PathBuf> {
    let probe = source_root.join(GRAPH_SUBDIR).join("classpath");
    match fs::metadata(&probe) {
        Ok(meta) if meta.is_dir() => Some(probe),
        _ => None,
    }
}

/// Lightweight integrity probe for `<source_root>/.sqry/graph/snapshot.sqry`.
///
/// Reads only the leading magic-byte header (≤16 bytes) and confirms
/// the file starts with [`SNAPSHOT_MAGIC_PREFIX`]. Returns:
///
/// - `Ok(true)`  — file opens, is at least [`SNAPSHOT_MIN_VALID_BYTES`]
///   long, and starts with the shared magic prefix.
/// - `Ok(false)` — file opens but is too short or the prefix doesn't
///   match (truncated / corrupt payload).
/// - `Err(_)`    — open or read failed; the caller folds this into the
///   `Error` bucket alongside `Ok(false)`.
///
/// This is intentionally a fast smoke test — full version-aware
/// validation lives in `sqry-core`'s snapshot loader. The goal here is
/// to distinguish "indexed and healthy enough to claim Ok" from
/// "snapshot file present but unreadable garbage" without paying the
/// cost of a full deserialise from the workspace-status surface.
fn snapshot_appears_valid(snapshot: &Path) -> std::io::Result<bool> {
    let mut buf = [0u8; 16];
    let mut file = fs::File::open(snapshot)?;
    let n = file.read(&mut buf)?;
    Ok(n >= SNAPSHOT_MIN_VALID_BYTES && buf.starts_with(SNAPSHOT_MAGIC_PREFIX))
}

/// Read the snapshot file's mtime if it exists; used as `last_indexed_at`
/// when a `Building` lockfile is also present.
fn snapshot_modified_time(snapshot: &Path) -> Option<SystemTime> {
    fs::metadata(snapshot).ok().and_then(|m| m.modified().ok())
}

/// Compute the aggregate and persist the cache.
///
/// Cache write failures are logged at warn level but never propagated:
/// the user-visible `sqry workspace status` command must succeed even
/// when the workspace lives on a read-only filesystem.
fn compute_and_persist(workspace_dir: &Path, logical: &LogicalWorkspace) -> WorkspaceIndexStatus {
    let entries: Vec<SourceRootStatus> = logical
        .source_roots()
        .iter()
        .map(|sr| compute_source_root_status(&sr.path))
        .collect();
    let aggregate = WorkspaceIndexStatus::from_source_root_statuses(entries);

    if let Err(err) = write_status_cache(workspace_dir, &aggregate) {
        log::warn!(
            "failed to persist workspace status cache at {}: {err}",
            workspace_cache_path(workspace_dir).display()
        );
    }

    aggregate
}

/// Build the human-readable text rendering as a flat line list.
fn render_text(
    workspace_dir: &Path,
    logical: &LogicalWorkspace,
    status: &WorkspaceIndexStatus,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("Workspace: {}", workspace_dir.display()));
    out.push(format!(
        "Workspace ID: {}  (full: {})",
        logical.workspace_id().as_short_hex(),
        logical.workspace_id().as_full_hex()
    ));
    out.push(format!(
        "Project root mode: {}",
        logical.project_root_mode()
    ));
    out.push(format!(
        "Source roots: {} total / {} indexed / {} missing / {} building / {} error",
        status.total(),
        status.ok_count,
        status.missing_count,
        status.building_count,
        status.error_count
    ));
    for entry in &status.source_root_statuses {
        let glyph = match entry.status {
            SourceRootIndexState::Ok => "ok",
            SourceRootIndexState::Missing => "missing",
            SourceRootIndexState::Building => "building",
            SourceRootIndexState::Error => "error",
        };
        let last = entry
            .last_indexed_at
            .map_or_else(|| "never".to_string(), format_system_time);
        out.push(format!(
            "  [{glyph}] {}  (last indexed: {last})",
            entry.path.display()
        ));
    }
    out.push(format!(
        "Member folders: {}",
        logical.member_folders().len()
    ));
    for member in logical.member_folders() {
        let reason = match member.reason {
            sqry_core::workspace::MemberReason::OperationalFolder => "operational",
            sqry_core::workspace::MemberReason::NonSourceFolder => "non-source",
            sqry_core::workspace::MemberReason::NoLanguagePluginMatch => "no-language-plugin-match",
        };
        out.push(format!("  {}  (reason: {reason})", member.path.display()));
    }
    out.push(format!("Exclusions: {}", logical.exclusions().len()));
    for excl in logical.exclusions() {
        out.push(format!("  {}", excl.display()));
    }
    out
}

/// Build the `--json` payload as a `serde_json::Value` so the caller
/// can re-pretty-print and so tests can assert against the structure.
fn render_json(
    workspace_dir: &Path,
    logical: &LogicalWorkspace,
    status: &WorkspaceIndexStatus,
) -> serde_json::Value {
    let source_roots: Vec<serde_json::Value> = status
        .source_root_statuses
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.path,
                "status": index_state_str(entry.status),
                "last_indexed_at": entry.last_indexed_at.map(format_system_time),
                "symbol_count": entry.symbol_count,
            })
        })
        .collect();
    let member_folders: Vec<serde_json::Value> = logical
        .member_folders()
        .iter()
        .map(|m| {
            serde_json::json!({
                "path": m.path,
                "reason": member_reason_str(m.reason),
            })
        })
        .collect();
    let exclusions: Vec<serde_json::Value> = logical
        .exclusions()
        .iter()
        .map(|p| serde_json::json!(p))
        .collect();

    serde_json::json!({
        "workspace_path": workspace_dir,
        "workspace_id_short": logical.workspace_id().as_short_hex(),
        "workspace_id_full": logical.workspace_id().as_full_hex(),
        "project_root_mode": logical.project_root_mode().as_str(),
        "source_roots": source_roots,
        "member_folders": member_folders,
        "exclusions": exclusions,
        "aggregate": {
            "total": status.total(),
            "ok_count": status.ok_count,
            "missing_count": status.missing_count,
            "building_count": status.building_count,
            "error_count": status.error_count,
            // Backwards-friendly aliases (the brief spells some keys
            // both ways): emit both so JSON consumers tolerate either.
            "indexed": status.ok_count,
            "missing": status.missing_count,
            "building": status.building_count,
        },
    })
}

fn index_state_str(state: SourceRootIndexState) -> &'static str {
    match state {
        SourceRootIndexState::Ok => "ok",
        SourceRootIndexState::Missing => "missing",
        SourceRootIndexState::Building => "building",
        SourceRootIndexState::Error => "error",
    }
}

fn member_reason_str(reason: sqry_core::workspace::MemberReason) -> &'static str {
    match reason {
        sqry_core::workspace::MemberReason::OperationalFolder => "operational",
        sqry_core::workspace::MemberReason::NonSourceFolder => "non-source",
        sqry_core::workspace::MemberReason::NoLanguagePluginMatch => "no-language-plugin-match",
    }
}

/// Render a `SystemTime` as a stable RFC-3339 / ISO-8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`). We avoid pulling in `chrono` /
/// `humantime` here so the workspace-status surface stays free of new
/// transitive dependencies.
fn format_system_time(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days_since_epoch = i64::try_from(secs / 86_400).unwrap_or(0);
    let secs_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days_since_epoch);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-since-1970-01-01 → (year, month, day) using Howard Hinnant's
/// proleptic-Gregorian formulae
/// (<https://howardhinnant.github.io/date_algorithms.html#days_from_civil>).
/// Deterministic; never panics for any value the caller can produce
/// from a `SystemTime` since the UNIX epoch.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names
)]
// `month` and `day` are bounded to 1..=31 / 1..=12 by construction,
// so the `as u32` casts cannot truncate meaningful bits or lose sign.
// The `yoe` / `y` / `year` triple are distinct intermediate values from
// the cited algorithm and must keep their canonical names so the code
// stays trivially auditable against the reference implementation.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the `compute_source_root_status` integrity probe.
    //!
    //! Codex iter1 `APPROVE_WITH_CHANGES` required that
    //! `SourceRootIndexState::Error` actually distinguish a corrupt /
    //! truncated `snapshot.sqry` from a healthy one. These tests
    //! exercise the lightweight magic-byte check in
    //! [`snapshot_appears_valid`] from each direction (healthy / too
    //! short / wrong magic / missing).

    use super::*;
    use tempfile::tempdir;

    fn write_snapshot(source_root: &Path, bytes: &[u8]) -> PathBuf {
        let graph_dir = source_root.join(GRAPH_SUBDIR).join(GRAPH_GRAPHDIR);
        std::fs::create_dir_all(&graph_dir).unwrap();
        let snapshot = graph_dir.join(SNAPSHOT_FILENAME);
        std::fs::write(&snapshot, bytes).unwrap();
        snapshot
    }

    #[test]
    fn compute_source_root_status_returns_ok_for_valid_magic() {
        let temp = tempdir().unwrap();
        let source_root = temp.path();
        // Valid V10 magic + payload byte → should be `Ok`.
        write_snapshot(source_root, b"SQRY_GRAPH_V10\0postcard-payload-bytes");
        let status = compute_source_root_status(source_root);
        assert_eq!(
            status.status,
            SourceRootIndexState::Ok,
            "valid magic must yield Ok, got {:?}",
            status.status
        );
        assert!(
            status.last_indexed_at.is_some(),
            "Ok must carry last_indexed_at"
        );
    }

    #[test]
    fn compute_source_root_status_returns_ok_for_v7_magic() {
        // Confirms the family-prefix check covers all supported
        // versions (V7 through V10), not just V10.
        let temp = tempdir().unwrap();
        let source_root = temp.path();
        write_snapshot(source_root, b"SQRY_GRAPH_V7\0\0\0postcard-payload");
        let status = compute_source_root_status(source_root);
        assert_eq!(status.status, SourceRootIndexState::Ok);
    }

    #[test]
    fn compute_source_root_status_returns_error_for_corrupt_snapshot() {
        // File present, metadata readable, but the magic prefix is
        // wrong → Error (not Ok, not Missing).
        let temp = tempdir().unwrap();
        let source_root = temp.path();
        write_snapshot(source_root, b"\x00\x01\x02junk-payload-with-no-magic-bytes");
        let status = compute_source_root_status(source_root);
        assert_eq!(
            status.status,
            SourceRootIndexState::Error,
            "corrupt snapshot must yield Error, got {:?}",
            status.status
        );
        assert!(
            status.last_indexed_at.is_none(),
            "Error entries do not carry last_indexed_at"
        );
    }

    #[test]
    fn compute_source_root_status_returns_error_for_truncated_snapshot() {
        // File too short to even hold the magic prefix → Error.
        let temp = tempdir().unwrap();
        let source_root = temp.path();
        write_snapshot(source_root, b"SQRY"); // 4 bytes, far below minimum
        let status = compute_source_root_status(source_root);
        assert_eq!(status.status, SourceRootIndexState::Error);
    }

    #[test]
    fn compute_source_root_status_returns_missing_when_absent() {
        let temp = tempdir().unwrap();
        // Don't create `.sqry/graph/snapshot.sqry` at all.
        let status = compute_source_root_status(temp.path());
        assert_eq!(status.status, SourceRootIndexState::Missing);
    }

    #[test]
    fn compute_source_root_status_returns_building_when_lock_present() {
        // Building wins over both Ok and Error/Missing per the
        // pre-existing contract — make sure the new magic-byte check
        // doesn't accidentally override that priority.
        let temp = tempdir().unwrap();
        let source_root = temp.path();
        let graph_dir = source_root.join(GRAPH_SUBDIR).join(GRAPH_GRAPHDIR);
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::write(graph_dir.join(BUILD_LOCK_FILENAME), b"").unwrap();
        // Even with a corrupt snapshot the lockfile must dominate.
        std::fs::write(graph_dir.join(SNAPSHOT_FILENAME), b"junk").unwrap();
        let status = compute_source_root_status(source_root);
        assert_eq!(status.status, SourceRootIndexState::Building);
    }
}
