//! `sqry workspace clean <root>` — discover and (optionally) remove
//! stale `.sqry/`, `.sqry-cache`, `.sqry-prof`, and legacy
//! `.sqry-index` artifacts (cluster-E §E.4).
//!
//! Dry-run-by-default. The command emits a [`WorkspaceCleanReport`]
//! summarising every artifact below `<root>`, classifies each, and
//! prints the planned-removal set. Pass `--apply` to actually delete
//! the planned set; the canonical active artifact and any artifact
//! the running daemon currently has loaded are excluded unless
//! `--force` is also passed. `.sqry-index.user` (user-curated state —
//! aliases, recent queries) is excluded unless `--include-user-state`.
//!
//! ## Safety
//!
//! - `walkdir` is constructed with `follow_links(false)`. A symlink
//!   that resolves to a `.sqry/`-shaped target is recorded as
//!   `SkippedArtifact { reason: SymlinkRefused }` and never deleted.
//! - Every discovered path is canonicalised; entries whose canonical
//!   form does not start with `canonical(root)` land under
//!   `SkippedArtifact { reason: OutsideRoot }`.
//! - Removal uses `fs::remove_dir_all` / `fs::remove_file` only on
//!   canonicalised absolute paths.
//!
//! ## Daemon hand-off
//!
//! Before discovery, the command queries the running daemon's
//! `daemon/active-artifacts` IPC method (250 ms budget) for the list
//! of `.sqry/graph` directories currently loaded. Those paths get
//! `is_daemon_locked = true` in the report and are excluded from the
//! removal plan unless `--force` is passed. When the daemon is down
//! the list is treated as empty and a warning surfaces in the JSON
//! envelope.

use std::collections::HashSet;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use sqry_core::workspace::{
    ArtifactKind, DiscoveredArtifact, RemovalError, SkipReason, SkippedArtifact,
    WorkspaceCleanReport, WorkspaceRootDiscovery, discover_workspace_root,
};

use crate::args::Cli;

/// Daemon `daemon/active-artifacts` budget (per cluster-E §E.4 step 3).
const DAEMON_ARTIFACTS_TIMEOUT_MS: u64 = 250;
/// Walkdir depth cap; mirrors `sqry_core::workspace::MAX_ANCESTOR_DEPTH`.
const WALK_MAX_DEPTH: usize = 64;

pub enum RemovalMode {
    DryRun,
    ApplyConfirmed,
    ApplyForced,
}

pub enum UserStatePolicy {
    Include,
    Exclude,
}

pub enum CleanOutput {
    Json,
    Text,
}

pub struct CleanOptions {
    pub removal: RemovalMode,
    pub user_state: UserStatePolicy,
    pub output: CleanOutput,
}

/// Entry point for `sqry workspace clean`.
///
/// # Errors
///
/// Returns an error if the root path is invalid, if `--apply` removal
/// fails for the entire planned set, or if the JSON renderer fails.
/// Per-entry removal failures are accumulated in
/// [`WorkspaceCleanReport::errors`] and do not abort the run.
pub fn run(_cli: &Cli, root: &str, options: &CleanOptions) -> Result<()> {
    let root_input = PathBuf::from(root);
    let canonical_root = root_input.canonicalize().with_context(|| {
        format!(
            "workspace clean: cannot canonicalise root {}",
            root_input.display()
        )
    })?;
    if !canonical_root.is_dir() {
        anyhow::bail!(
            "workspace clean: root {} is not a directory",
            canonical_root.display()
        );
    }

    // Step 1: identify the canonical active artifact for this root via
    // the shared workspace walker (cluster-E §E.1). When the walker
    // returns a graph below the project boundary, that path is the
    // "do not delete by default" anchor.
    let canonical_active_artifact = match discover_workspace_root(&canonical_root) {
        WorkspaceRootDiscovery::GraphFound { root: r, .. } => Some(r.join(".sqry").join("graph")),
        _ => None,
    };

    // Step 2: probe the daemon for active artifacts. 250 ms budget; on
    // timeout / connection failure the list is empty plus a warning
    // surfaced in the JSON envelope.
    let (daemon_locked_artifacts, daemon_warning) = probe_daemon_active_artifacts();

    // Step 3: walk the tree.
    let (discovered, mut skipped) = walk_artifacts(
        &canonical_root,
        canonical_active_artifact.as_deref(),
        &daemon_locked_artifacts,
    );

    // Step 4: filter to planned removals per the §E.4 step-6 policy.
    let planned_removals = plan_removals(&discovered, &mut skipped, options);

    // Step 5: confirmation gating (cluster-E iter-2 §E.4 fix).
    //
    // Truth table:
    //   --apply           → text/TTY: prompt; text/non-TTY: apply.
    //   --apply --force   → always apply (force opts out of all gates).
    //   --apply --json    → REFUSE to apply when confirmation would
    //                       normally be required (TTY) — JSON mode
    //                       must never prompt and must never silently
    //                       apply unconfirmed removals. Combine with
    //                       `--force` for non-interactive scripted
    //                       removal.
    //   --apply --json --force → apply (force is the explicit
    //                       non-interactive opt-in).
    let mut removed: Vec<PathBuf> = Vec::new();
    let mut errors: Vec<RemovalError> = Vec::new();
    let mut effective_apply = !matches!(options.removal, RemovalMode::DryRun);
    if matches!(options.removal, RemovalMode::ApplyConfirmed) && !planned_removals.is_empty() {
        if matches!(options.output, CleanOutput::Json) {
            // JSON callers must opt in via --force; record the
            // refusal as a per-entry error and demote to dry-run.
            for path in &planned_removals {
                errors.push(RemovalError {
                    path: path.clone(),
                    error: "skipped: --apply --json requires --force \
                            (JSON mode never prompts; pass --force to \
                            confirm non-interactive removal)"
                        .to_string(),
                });
            }
            effective_apply = false;
        } else if std::io::stdin().is_terminal() && !confirm_removal(&planned_removals)? {
            // User declined.
            effective_apply = false;
        }
        // Non-TTY text mode without --force preserves the existing
        // contract: apply silently. Pipelines + sudo flows stay
        // unchanged.
    }
    if effective_apply {
        for path in &planned_removals {
            match remove_path(path) {
                Ok(()) => removed.push(path.clone()),
                Err(e) => errors.push(RemovalError {
                    path: path.clone(),
                    error: e.to_string(),
                }),
            }
        }
    }

    let report = WorkspaceCleanReport {
        schema_version: 1,
        root: canonical_root,
        canonical_active_artifact,
        daemon_locked_artifacts,
        discovered,
        planned_removals,
        skipped,
        // `applied` reflects what we ACTUALLY did, not what the user
        // asked for. The JSON-mode-without-force gate above demotes
        // `--apply` to a dry-run; surface that on the wire.
        applied: effective_apply,
        removed,
        errors,
    };
    emit_report(
        &report,
        matches!(options.output, CleanOutput::Json),
        daemon_warning,
    )
}

/// Render the report. JSON mode emits the canonical schema verbatim
/// (plus an optional `_warning` field for daemon-down state); text
/// mode prints a human-readable summary.
fn emit_report(
    report: &WorkspaceCleanReport,
    json: bool,
    daemon_warning: Option<&'static str>,
) -> Result<()> {
    if json {
        let mut value = serde_json::to_value(report)
            .context("workspace clean: failed to serialise WorkspaceCleanReport")?;
        if let (Some(warning), Some(obj)) = (daemon_warning, value.as_object_mut()) {
            obj.insert(
                "_warning".to_string(),
                serde_json::Value::String(warning.to_string()),
            );
        }
        let pretty = serde_json::to_string_pretty(&value)
            .context("workspace clean: failed to render JSON")?;
        println!("{pretty}");
        return Ok(());
    }

    print_text_summary(report, daemon_warning);
    Ok(())
}

fn plan_removals(
    discovered: &[DiscoveredArtifact],
    skipped: &mut Vec<SkippedArtifact>,
    options: &CleanOptions,
) -> Vec<PathBuf> {
    let mut planned_removals: Vec<PathBuf> = Vec::new();
    for art in discovered {
        if should_skip_artifact(art, skipped, options) {
            continue;
        }
        planned_removals.push(art.path.clone());
    }
    planned_removals
}

fn should_skip_artifact(
    artifact: &DiscoveredArtifact,
    skipped: &mut Vec<SkippedArtifact>,
    options: &CleanOptions,
) -> bool {
    let reason = if artifact.is_canonical_active
        && !matches!(options.removal, RemovalMode::ApplyForced)
    {
        Some(SkipReason::CanonicalActive)
    } else if artifact.is_daemon_locked && !matches!(options.removal, RemovalMode::ApplyForced) {
        Some(SkipReason::DaemonLocked)
    } else if matches!(artifact.kind, ArtifactKind::WorkspaceRegistry) {
        Some(SkipReason::WorkspaceRegistry)
    } else if matches!(artifact.kind, ArtifactKind::UserState)
        && matches!(options.user_state, UserStatePolicy::Exclude)
    {
        Some(SkipReason::UserState)
    } else {
        None
    };
    let Some(reason) = reason else {
        return false;
    };
    skipped.push(SkippedArtifact {
        path: artifact.path.clone(),
        reason,
    });
    true
}

fn print_text_summary(report: &WorkspaceCleanReport, daemon_warning: Option<&'static str>) {
    println!("sqry workspace clean — root: {}", report.root.display());
    if let Some(active) = &report.canonical_active_artifact {
        println!("  canonical active: {}", active.display());
    }
    if let Some(w) = daemon_warning {
        println!("  warning: {w}");
    }
    println!();
    println!("Discovered ({} entries):", report.discovered.len());
    for art in &report.discovered {
        let mut tags: Vec<&'static str> = Vec::new();
        if art.is_canonical_active {
            tags.push("active");
        }
        if art.is_daemon_locked {
            tags.push("daemon-locked");
        }
        if art.is_user_state {
            tags.push("user-state");
        }
        let tag_str = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        println!(
            "  {kind:?}  {size_kib:>8} KiB  {path}{tag}",
            kind = art.kind,
            size_kib = art.size_bytes / 1024,
            path = art.path.display(),
            tag = tag_str,
        );
    }
    println!();
    if report.planned_removals.is_empty() {
        println!("No removable artifacts under this policy.");
    } else {
        println!(
            "Planned removals ({} entries):",
            report.planned_removals.len()
        );
        for p in &report.planned_removals {
            println!("  - {}", p.display());
        }
    }
    if !report.skipped.is_empty() {
        println!();
        println!("Skipped ({} entries):", report.skipped.len());
        for s in &report.skipped {
            println!("  {} ({:?})", s.path.display(), s.reason);
        }
    }
    if report.applied {
        println!();
        println!(
            "Applied: removed {} of {} planned artifacts.",
            report.removed.len(),
            report.planned_removals.len(),
        );
        if !report.errors.is_empty() {
            println!("Errors ({}):", report.errors.len());
            for err in &report.errors {
                println!("  {} — {}", err.path.display(), err.error);
            }
        }
    } else {
        println!();
        println!("DRY RUN — re-run with --apply to remove the planned artifacts.");
    }
}

fn confirm_removal(planned: &[PathBuf]) -> Result<bool> {
    eprintln!(
        "sqry: about to remove {} artifact(s). Continue? [y/N] ",
        planned.len()
    );
    std::io::stderr().flush().ok();
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .context("workspace clean: failed to read confirmation")?;
    let trimmed = buf.trim().to_ascii_lowercase();
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

/// Walk `root` collecting every `.sqry/`, `.sqry-cache`,
/// `.sqry-prof`, `.sqry-index`, `.sqry-index.user`, and
/// `.sqry-workspace` entry. Returns `(discovered, skipped)` —
/// canonicalisation failures land in `skipped` so the dry run can
/// still account for them without blowing up the whole walk.
fn walk_artifacts(
    canonical_root: &Path,
    canonical_active_artifact: Option<&Path>,
    daemon_locked: &[PathBuf],
) -> (Vec<DiscoveredArtifact>, Vec<SkippedArtifact>) {
    let mut discovered: Vec<DiscoveredArtifact> = Vec::new();
    let mut skipped: Vec<SkippedArtifact> = Vec::new();
    let daemon_set: HashSet<PathBuf> = daemon_locked
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    // Track every directory that has already been classified as an
    // artifact root so we can prune children (avoid double-counting
    // contents of `.sqry-cache` etc.).
    let mut pruned: HashSet<PathBuf> = HashSet::new();

    let mut walker = walkdir::WalkDir::new(canonical_root)
        .follow_links(false)
        .max_depth(WALK_MAX_DEPTH)
        .into_iter();

    while let Some(entry_result) = walker.next() {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                // Surface the path if walkdir gives one; otherwise just a
                // synthetic skipped entry under the root.
                let p = e
                    .path()
                    .map_or_else(|| canonical_root.to_path_buf(), Path::to_path_buf);
                skipped.push(SkippedArtifact {
                    path: p,
                    reason: SkipReason::OutsideRoot,
                });
                continue;
            }
        };
        let path = entry.path();

        // Prune children of an already-classified artifact directory.
        if pruned.iter().any(|p| path.starts_with(p)) {
            continue;
        }

        // Only directory entries (and the legacy `.sqry-index` file)
        // can be artifacts. File traversal still produces entries; we
        // skip them quickly here.
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(kind) = artifact_kind_for_entry(&entry, file_name) else {
            continue;
        };

        // Symlink defence — `walkdir` won't follow links because we set
        // `follow_links(false)`, but a directory entry whose own
        // metadata is a symlink should be refused outright.
        if entry.path_is_symlink() {
            skipped.push(SkippedArtifact {
                path: path.to_path_buf(),
                reason: SkipReason::SymlinkRefused,
            });
            // Don't descend into linked target; walkdir already won't.
            walker.skip_current_dir();
            continue;
        }

        // Path-traversal defence — canonicalise and verify the result
        // stays under `canonical_root`. A `.sqry-cache` symlink that
        // points outside should already be caught above, but defence
        // in depth.
        let Ok(canonical_path) = path.canonicalize() else {
            skipped.push(SkippedArtifact {
                path: path.to_path_buf(),
                reason: SkipReason::OutsideRoot,
            });
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        };
        if !canonical_path.starts_with(canonical_root) {
            skipped.push(SkippedArtifact {
                path: canonical_path,
                reason: SkipReason::OutsideRoot,
            });
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }

        let inner_graph = canonical_path.join("graph");
        let is_canonical_active = canonical_active_artifact
            .is_some_and(|a| a == canonical_path.as_path() || a == inner_graph.as_path());
        let is_daemon_locked = daemon_set
            .iter()
            .any(|p| *p == canonical_path || *p == inner_graph);
        let is_user_state = matches!(kind, ArtifactKind::UserState);

        discovered.push(DiscoveredArtifact {
            path: canonical_path.clone(),
            kind: final_artifact_kind(kind, &canonical_path, is_canonical_active),
            size_bytes: artifact_size_bytes(kind, &canonical_path),
            last_modified: artifact_last_modified(&canonical_path),
            is_canonical_active,
            is_daemon_locked,
            is_user_state,
        });

        // Don't descend into the artifact root itself; everything below
        // belongs to the same logical artifact.
        if entry.file_type().is_dir() {
            pruned.insert(canonical_path);
            walker.skip_current_dir();
        }
    }

    (discovered, skipped)
}

fn artifact_kind_for_entry(entry: &walkdir::DirEntry, file_name: &str) -> Option<ArtifactKind> {
    match file_name {
        ".sqry" if entry.file_type().is_dir() => Some(ArtifactKind::GraphRoot),
        ".sqry-cache" if entry.file_type().is_dir() => Some(ArtifactKind::Cache),
        ".sqry-prof" if entry.file_type().is_dir() => Some(ArtifactKind::Prof),
        ".sqry-index" if entry.file_type().is_file() => Some(ArtifactKind::LegacyIndex),
        ".sqry-index.user" if entry.file_type().is_file() => Some(ArtifactKind::UserState),
        ".sqry-workspace" if entry.file_type().is_file() => Some(ArtifactKind::WorkspaceRegistry),
        _ => None,
    }
}

fn artifact_size_bytes(kind: ArtifactKind, canonical_path: &Path) -> u64 {
    match kind {
        ArtifactKind::Graph
        | ArtifactKind::GraphRoot
        | ArtifactKind::Cache
        | ArtifactKind::Prof
        | ArtifactKind::NestedGraph => directory_size(canonical_path),
        ArtifactKind::LegacyIndex | ArtifactKind::UserState | ArtifactKind::WorkspaceRegistry => {
            fs::metadata(canonical_path).map_or(0, |m| m.len())
        }
    }
}

fn artifact_last_modified(canonical_path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    fs::metadata(canonical_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| {
            let secs = i64::try_from(d.as_secs()).ok()?;
            chrono::DateTime::<chrono::Utc>::from_timestamp(secs, d.subsec_nanos())
        })
}

fn final_artifact_kind(
    kind: ArtifactKind,
    canonical_path: &Path,
    is_canonical_active: bool,
) -> ArtifactKind {
    if !matches!(kind, ArtifactKind::GraphRoot) || is_canonical_active {
        return kind;
    }
    let Some(parent) = canonical_path.parent() else {
        return ArtifactKind::GraphRoot;
    };
    match discover_workspace_root(parent) {
        WorkspaceRootDiscovery::GraphFound { root, .. } if root.join(".sqry") != canonical_path => {
            ArtifactKind::NestedGraph
        }
        _ => ArtifactKind::GraphRoot,
    }
}

/// Recursive size in bytes. Best-effort: any I/O failure yields 0 for
/// that subtree rather than aborting the dry run.
fn directory_size(root: &Path) -> u64 {
    let mut total: u64 = 0;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file()
            && let Ok(meta) = entry.metadata()
        {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Connect to the daemon, send `daemon/active-artifacts`, return the
/// list (or empty + warning on timeout / connection failure).
fn probe_daemon_active_artifacts() -> (Vec<PathBuf>, Option<&'static str>) {
    let socket_path = match sqry_daemon::config::DaemonConfig::load() {
        Ok(cfg) => cfg.socket_path(),
        Err(_) => {
            return (
                Vec::new(),
                Some("daemon config not loadable; daemon-locked check skipped"),
            );
        }
    };
    if !crate::commands::daemon::try_connect_sync(&socket_path).unwrap_or(false) {
        return (
            Vec::new(),
            Some("sqryd is not running; daemon-locked check skipped"),
        );
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return (
            Vec::new(),
            Some("could not start tokio runtime to probe daemon; check skipped"),
        );
    };
    rt.block_on(async {
        let timeout = Duration::from_millis(DAEMON_ARTIFACTS_TIMEOUT_MS);
        let probe = async {
            let mut client = sqry_daemon_client::DaemonClient::connect(&socket_path).await?;
            client.active_artifacts().await
        };
        match tokio::time::timeout(timeout, probe).await {
            Ok(Ok(list)) => (list, None),
            Ok(Err(_)) => (
                Vec::new(),
                Some("daemon/active-artifacts request failed; daemon-locked check skipped"),
            ),
            Err(_) => (
                Vec::new(),
                Some("daemon/active-artifacts timed out at 250ms; daemon-locked check skipped"),
            ),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a fresh canonical path for a freshly-created tempdir.
    fn canonical(p: &Path) -> PathBuf {
        p.canonicalize().unwrap()
    }

    /// Build a representative artifact layout under `root`:
    ///
    /// - `<root>/.sqry/graph/snapshot.sqry` (canonical active)
    /// - `<root>/.sqry-cache/file`
    /// - `<root>/.sqry-prof/file`
    /// - `<root>/.sqry-index` (legacy)
    /// - `<root>/.sqry-index.user`
    /// - `<root>/Cargo.toml` (project marker so `discover_workspace_root`
    ///   anchors the active artifact correctly).
    fn make_layout(root: &Path) {
        fs::create_dir_all(root.join(".sqry").join("graph")).unwrap();
        fs::write(root.join(".sqry").join("graph").join("snapshot.sqry"), b"x").unwrap();
        fs::create_dir_all(root.join(".sqry-cache")).unwrap();
        fs::write(root.join(".sqry-cache").join("file"), b"x").unwrap();
        fs::create_dir_all(root.join(".sqry-prof")).unwrap();
        fs::write(root.join(".sqry-prof").join("file"), b"x").unwrap();
        fs::write(root.join(".sqry-index"), b"legacy").unwrap();
        fs::write(root.join(".sqry-index.user"), b"alias=foo").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
    }

    /// Helper: walk + filter without daemon probe, returning the
    /// (planned, skipped) split that the policy filter produces.
    fn dry_run(
        root: &Path,
        force: bool,
        include_user_state: bool,
        daemon_locked: &[PathBuf],
    ) -> (Vec<DiscoveredArtifact>, Vec<PathBuf>, Vec<SkippedArtifact>) {
        let canonical_root = canonical(root);
        let canonical_active = match discover_workspace_root(&canonical_root) {
            WorkspaceRootDiscovery::GraphFound { root: r, .. } => {
                Some(r.join(".sqry").join("graph"))
            }
            _ => None,
        };
        let (discovered, mut skipped) =
            walk_artifacts(&canonical_root, canonical_active.as_deref(), daemon_locked);
        let options = CleanOptions {
            removal: if force {
                RemovalMode::ApplyForced
            } else {
                RemovalMode::DryRun
            },
            user_state: if include_user_state {
                UserStatePolicy::Include
            } else {
                UserStatePolicy::Exclude
            },
            output: CleanOutput::Text,
        };
        let planned = plan_removals(&discovered, &mut skipped, &options);
        (discovered, planned, skipped)
    }

    /// §E.4 row 1: dry run lists all five artifact kinds, plans removal of
    /// `.sqry-cache`, `.sqry-prof`, `.sqry-index`; protects the canonical
    /// `.sqry/` and `.sqry-index.user`.
    #[test]
    fn dry_run_lists_stale() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        make_layout(&root);

        let (discovered, planned, _skipped) = dry_run(&root, false, false, &[]);

        assert_eq!(
            discovered.len(),
            5,
            "expected 5 artifacts (sqry/graph-root, cache, prof, legacy, user-state), got {discovered:?}"
        );
        let active = canonical(&root).join(".sqry");
        assert!(
            !planned.iter().any(|p| p == &active),
            "canonical active must be skipped without --force, planned={planned:?}"
        );
        let user = canonical(&root).join(".sqry-index.user");
        assert!(
            !planned.iter().any(|p| p == &user),
            "user state must be skipped without --include-user-state, planned={planned:?}"
        );
        let must_be_planned = [
            canonical(&root).join(".sqry-cache"),
            canonical(&root).join(".sqry-prof"),
            canonical(&root).join(".sqry-index"),
        ];
        for p in must_be_planned {
            assert!(
                planned.contains(&p),
                "{} must be in planned removals, planned={planned:?}",
                p.display()
            );
        }
    }

    /// §E.4 row 2: removal-via-end-to-end run leaves the canonical
    /// `.sqry/` and `.sqry-index.user` intact, deletes the rest of the
    /// planned set.
    #[test]
    fn apply_removes_planned_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        make_layout(&root);

        let (_discovered, planned, _skipped) = dry_run(&root, false, false, &[]);
        // Apply: directly invoke the helper's removal step (the public
        // `run()` would also call the daemon probe; this test isolates
        // the filesystem effect).
        for p in &planned {
            remove_path(p).unwrap();
        }

        assert!(
            root.join(".sqry").join("graph").exists(),
            "canonical active must survive"
        );
        assert!(
            root.join(".sqry-index.user").exists(),
            "user state must survive"
        );
        assert!(!root.join(".sqry-cache").exists());
        assert!(!root.join(".sqry-prof").exists());
        assert!(!root.join(".sqry-index").exists());
    }

    /// §E.4 row 3: `--apply` without `--force` must NOT remove the
    /// canonical active artifact even when the user explicitly opts
    /// into removal.
    #[test]
    fn apply_protects_canonical_without_force() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        make_layout(&root);

        let (_discovered, planned, _skipped) = dry_run(&root, false, false, &[]);
        let active = canonical(&root).join(".sqry");
        assert!(
            !planned.contains(&active),
            "without --force the canonical active must not appear in planned removals"
        );
    }

    /// §E.4 row 5: an artifact reported by `daemon/active-artifacts` is
    /// excluded from the planned-removal set when `--force` is absent.
    #[test]
    fn daemon_locked_protected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        make_layout(&root);

        let canonical_graph = canonical(&root).join(".sqry").join("graph");
        let (discovered, planned, _skipped) =
            dry_run(&root, false, false, std::slice::from_ref(&canonical_graph));

        // The classifier walks `.sqry/` (the directory containing
        // `graph/`) — daemon-locked detection covers either
        // `canonical_graph` or its parent `.sqry/`.
        let saw_lock = discovered.iter().any(|a| {
            a.is_daemon_locked && (a.path == canonical_graph || a.path.ends_with(".sqry"))
        });
        assert!(
            saw_lock,
            "daemon-locked detection must flag .sqry/ when its inner graph/ matches"
        );
        assert!(
            !planned
                .iter()
                .any(|p| p == &canonical_graph || p.ends_with(".sqry")),
            "daemon-locked artifact must be excluded from planned removals, got {planned:?}"
        );
    }
}
