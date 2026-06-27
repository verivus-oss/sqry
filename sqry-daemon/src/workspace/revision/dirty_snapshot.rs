//! Exact-byte dirty worktree snapshot source.
//!
//! Dirty snapshots are resident-only point-in-time sources. They capture the
//! Git baseline, index shape, staged/unstaged path sets, selected untracked
//! paths, deletions, file modes, symlink targets, and the exact byte stream
//! that will be fed to the parser materialization step.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use serde::{Deserialize, Serialize};

use crate::error::DaemonError;

use super::{
    DEFAULT_RAW_SOURCE_MAX_FILE_BYTES, PathScope, SourceDigest, VirtualPath, VirtualSourceEntry,
    VirtualSourceKind, VirtualSourceReader,
    manifest::{ManifestHashError, canonical_json_sha256, hex_sha256},
};

/// Dirty snapshot capture options.
#[derive(Debug, Clone)]
pub struct DirtySnapshotOptions {
    /// Repository root.
    pub repo_root: PathBuf,
    /// Include untracked nonignored files.
    pub include_untracked: bool,
    /// Include ignored files when untracked files are requested.
    pub include_ignored: bool,
    /// Indexed path scope.
    pub path_scope: PathScope,
    /// Maximum regular file size materialized into the parser source tree.
    pub max_file_bytes: u64,
}

impl DirtySnapshotOptions {
    /// Construct options with production defaults.
    #[must_use]
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            include_untracked: false,
            include_ignored: false,
            path_scope: PathScope::Repository,
            max_file_bytes: DEFAULT_RAW_SOURCE_MAX_FILE_BYTES,
        }
    }
}

/// Deterministic dirty snapshot identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirtySnapshotFingerprint {
    /// Schema version for dirty snapshot fingerprint inputs.
    pub schema_version: u32,
    /// HEAD commit at capture time, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_head_commit_oid: Option<String>,
    /// HEAD tree at capture time, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_head_tree_oid: Option<String>,
    /// Tree object representing current index state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_tree_oid: Option<String>,
    /// Staged path/status records.
    pub staged: Vec<DirtyPathStatus>,
    /// Unstaged path/status records.
    pub unstaged: Vec<DirtyPathStatus>,
    /// Captured source entries and byte hashes.
    pub entries: Vec<DirtySnapshotEntryDigest>,
    /// SHA-256 over the canonical fingerprint inputs excluding this field.
    pub snapshot_digest: String,
}

impl DirtySnapshotFingerprint {
    /// Source digest suitable for artifact-key construction.
    #[must_use]
    pub fn source_digest(&self) -> SourceDigest {
        SourceDigest::DirtySnapshot {
            snapshot_digest: self.snapshot_digest.clone(),
        }
    }
}

/// Path/status record from Git porcelain-style diffs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct DirtyPathStatus {
    /// Git status code.
    pub status: String,
    /// Repository-relative path.
    pub path: String,
    /// Rename/copy source path when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

/// Captured entry digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct DirtySnapshotEntryDigest {
    /// Repository-relative path.
    pub path: String,
    /// Entry kind.
    pub kind: String,
    /// Unix executable bit when available.
    pub executable: bool,
    /// Entry size when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// SHA-256 of exact bytes used for regular files or symlink target bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct DirtySnapshotFingerprintInputs<'a> {
    schema_version: u32,
    base_head_commit_oid: &'a Option<String>,
    base_head_tree_oid: &'a Option<String>,
    index_tree_oid: &'a Option<String>,
    staged: &'a [DirtyPathStatus],
    unstaged: &'a [DirtyPathStatus],
    entries: &'a [DirtySnapshotEntryDigest],
}

/// Captured dirty source.
#[derive(Debug, Clone)]
pub struct DirtySnapshotSource {
    entries: Vec<VirtualSourceEntry>,
    bytes_by_path: BTreeMap<Vec<u8>, Vec<u8>>,
    fingerprint: DirtySnapshotFingerprint,
}

impl DirtySnapshotSource {
    /// Capture a stable dirty snapshot, retrying once if the worktree mutates
    /// between initial capture and pre-publish validation.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::DirtySnapshotChanged`] if both attempts observe a
    /// mutation between capture and validation.
    pub fn capture(options: &DirtySnapshotOptions) -> Result<Self, DaemonError> {
        for attempt in 0..=1 {
            let captured = Self::capture_once(options)?;
            let validation = Self::capture_once(options)?;
            if captured.fingerprint.snapshot_digest == validation.fingerprint.snapshot_digest {
                return Ok(captured);
            }
            if attempt == 1 {
                return Err(DaemonError::DirtySnapshotChanged {
                    root: options.repo_root.clone(),
                });
            }
        }
        unreachable!("loop returns on success or final retry failure")
    }

    /// Single-pass dirty source capture.
    ///
    /// Uses `git write-tree` to derive the current index tree identity. That may
    /// create a local tree object, but it does not mutate the index or worktree
    /// and it does not fetch remote objects.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git status metadata or source bytes cannot be
    /// read.
    pub fn capture_once(options: &DirtySnapshotOptions) -> Result<Self, DaemonError> {
        let root = canonical_root(&options.repo_root)?;
        let base_head_commit_oid = git_optional(&root, &["rev-parse", "--verify", "HEAD"])?;
        let base_head_tree_oid = git_optional(&root, &["rev-parse", "--verify", "HEAD^{tree}"])?;
        let index_tree_oid = git_optional(&root, &["write-tree"])?;
        let staged = parse_name_status(&git_output(
            &root,
            &["diff", "--cached", "--name-status", "-z"],
        )?);
        let unstaged = parse_name_status(&git_output(&root, &["diff", "--name-status", "-z"])?);

        let candidate_paths = candidate_paths(&root, options, &staged, &unstaged)?;
        let mut entries = Vec::new();
        let mut bytes_by_path = BTreeMap::new();
        let mut entry_digests = Vec::new();

        for path_bytes in candidate_paths {
            let virtual_path = VirtualPath::from_git_path_bytes(path_bytes.clone())?;
            if !virtual_path.is_in_scope(&options.path_scope) {
                continue;
            }
            let full_path = virtual_path.to_path_buf_under(&root)?;
            let (entry, maybe_bytes, digest) =
                capture_entry(&virtual_path, &full_path, options.max_file_bytes)?;
            if let Some(bytes) = maybe_bytes {
                bytes_by_path.insert(path_bytes, bytes);
            }
            entries.push(entry);
            entry_digests.push(digest);
        }

        entries.sort_by(|left, right| left.path.cmp(&right.path));
        entry_digests.sort();
        let fingerprint = build_fingerprint(
            base_head_commit_oid,
            base_head_tree_oid,
            index_tree_oid,
            sorted_statuses(staged),
            sorted_statuses(unstaged),
            entry_digests,
        )?;

        Ok(Self {
            entries,
            bytes_by_path,
            fingerprint,
        })
    }

    /// Dirty snapshot fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &DirtySnapshotFingerprint {
        &self.fingerprint
    }
}

impl VirtualSourceReader for DirtySnapshotSource {
    fn entries(&self) -> &[VirtualSourceEntry] {
        &self.entries
    }

    fn read_entry_bytes(&self, entry: &VirtualSourceEntry) -> Result<Vec<u8>, DaemonError> {
        self.bytes_by_path
            .get(entry.path.as_bytes())
            .cloned()
            .ok_or_else(|| DaemonError::RevisionSourceUnavailable {
                reason: format!(
                    "dirty snapshot has no parse bytes for {}",
                    entry.path.display_lossy()
                ),
                path: Some(PathBuf::from(entry.path.display_lossy())),
            })
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, DaemonError> {
    root.canonicalize()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to canonicalize dirty snapshot root: {err}"),
            path: Some(root.to_path_buf()),
        })
}

fn git_output(root: &Path, args: &[&str]) -> Result<Vec<u8>, DaemonError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to run git {}: {err}", args.join(" ")),
            path: Some(root.to_path_buf()),
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(DaemonError::RevisionSourceUnavailable {
            reason: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            path: Some(root.to_path_buf()),
        })
    }
}

fn git_optional(root: &Path, args: &[&str]) -> Result<Option<String>, DaemonError> {
    match git_output(root, args) {
        Ok(bytes) => {
            let value = String::from_utf8_lossy(&bytes).trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        }
        Err(DaemonError::RevisionSourceUnavailable { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

fn parse_name_status(bytes: &[u8]) -> Vec<DirtyPathStatus> {
    let parts: Vec<&[u8]> = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect();
    let mut records = Vec::new();
    let mut idx = 0;
    while idx < parts.len() {
        let status = String::from_utf8_lossy(parts[idx]).into_owned();
        idx += 1;
        if status.starts_with('R') || status.starts_with('C') {
            if idx + 1 >= parts.len() {
                break;
            }
            let old_path = String::from_utf8_lossy(parts[idx]).into_owned();
            let path = String::from_utf8_lossy(parts[idx + 1]).into_owned();
            records.push(DirtyPathStatus {
                status,
                path,
                old_path: Some(old_path),
            });
            idx += 2;
        } else {
            if idx >= parts.len() {
                break;
            }
            let path = String::from_utf8_lossy(parts[idx]).into_owned();
            records.push(DirtyPathStatus {
                status,
                path,
                old_path: None,
            });
            idx += 1;
        }
    }
    sorted_statuses(records)
}

fn sorted_statuses(mut statuses: Vec<DirtyPathStatus>) -> Vec<DirtyPathStatus> {
    statuses.sort();
    statuses
}

fn candidate_paths(
    root: &Path,
    options: &DirtySnapshotOptions,
    staged: &[DirtyPathStatus],
    unstaged: &[DirtyPathStatus],
) -> Result<Vec<Vec<u8>>, DaemonError> {
    let mut paths = BTreeSet::new();
    for path in zsplit(&git_output(root, &["ls-files", "-z"])?) {
        paths.insert(path);
    }
    if options.include_untracked {
        let args: Vec<&str> = if options.include_ignored {
            vec!["ls-files", "--others", "-z"]
        } else {
            vec!["ls-files", "--others", "--exclude-standard", "-z"]
        };
        for path in zsplit(&git_output(root, &args)?) {
            paths.insert(path);
        }
    }
    for record in staged.iter().chain(unstaged) {
        paths.insert(record.path.as_bytes().to_vec());
        if let Some(old_path) = &record.old_path {
            paths.insert(old_path.as_bytes().to_vec());
        }
    }
    Ok(paths.into_iter().collect())
}

fn zsplit(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

fn capture_entry(
    virtual_path: &VirtualPath,
    full_path: &Path,
    max_file_bytes: u64,
) -> Result<
    (
        VirtualSourceEntry,
        Option<Vec<u8>>,
        DirtySnapshotEntryDigest,
    ),
    DaemonError,
> {
    let metadata = match fs::symlink_metadata(full_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let entry = VirtualSourceEntry::new(
                virtual_path.clone(),
                VirtualSourceKind::Deletion,
                None,
                None,
            );
            let digest = DirtySnapshotEntryDigest {
                path: virtual_path.display_lossy(),
                kind: "deletion".to_owned(),
                executable: false,
                size_bytes: None,
                byte_sha256: None,
            };
            return Ok((entry, None, digest));
        }
        Err(err) => {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to stat dirty snapshot path: {err}"),
                path: Some(full_path.to_path_buf()),
            });
        }
    };

    if metadata.file_type().is_symlink() {
        let target =
            fs::read_link(full_path).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to read symlink target: {err}"),
                path: Some(full_path.to_path_buf()),
            })?;
        let bytes = target.as_os_str().to_string_lossy().as_bytes().to_vec();
        let digest = hex_sha256(&bytes);
        return Ok((
            VirtualSourceEntry::new(
                virtual_path.clone(),
                VirtualSourceKind::Symlink,
                Some(format!("dirty-symlink:{digest}")),
                Some(bytes.len() as u64),
            ),
            None,
            DirtySnapshotEntryDigest {
                path: virtual_path.display_lossy(),
                kind: "symlink".to_owned(),
                executable: false,
                size_bytes: Some(bytes.len() as u64),
                byte_sha256: Some(digest),
            },
        ));
    }

    if !metadata.is_file() {
        let reason = "dirty snapshot path is not a regular file".to_owned();
        return Ok((
            VirtualSourceEntry::new(
                virtual_path.clone(),
                VirtualSourceKind::Unsupported {
                    reason: reason.clone(),
                },
                None,
                None,
            ),
            None,
            DirtySnapshotEntryDigest {
                path: virtual_path.display_lossy(),
                kind: "unsupported".to_owned(),
                executable: false,
                size_bytes: None,
                byte_sha256: Some(hex_sha256(reason.as_bytes())),
            },
        ));
    }

    let size = metadata.len();
    if size > max_file_bytes {
        let entry = VirtualSourceEntry::new(
            virtual_path.clone(),
            VirtualSourceKind::TooLarge {
                size_bytes: size,
                max_bytes: max_file_bytes,
            },
            None,
            Some(size),
        );
        let digest = DirtySnapshotEntryDigest {
            path: virtual_path.display_lossy(),
            kind: "too_large".to_owned(),
            executable: is_executable(&metadata),
            size_bytes: Some(size),
            byte_sha256: None,
        };
        return Ok((entry, None, digest));
    }

    let bytes = fs::read(full_path).map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("failed to read dirty snapshot bytes: {err}"),
        path: Some(full_path.to_path_buf()),
    })?;
    let byte_hash = hex_sha256(&bytes);
    let executable = is_executable(&metadata);
    let kind = if executable {
        VirtualSourceKind::ExecutableFile
    } else {
        VirtualSourceKind::RegularFile
    };
    Ok((
        VirtualSourceEntry::new(
            virtual_path.clone(),
            kind,
            Some(format!("dirty:{byte_hash}")),
            Some(size),
        ),
        Some(bytes),
        DirtySnapshotEntryDigest {
            path: virtual_path.display_lossy(),
            kind: if executable {
                "executable_file".to_owned()
            } else {
                "regular_file".to_owned()
            },
            executable,
            size_bytes: Some(size),
            byte_sha256: Some(byte_hash),
        },
    ))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn build_fingerprint(
    base_head_commit_oid: Option<String>,
    base_head_tree_oid: Option<String>,
    index_tree_oid: Option<String>,
    staged: Vec<DirtyPathStatus>,
    unstaged: Vec<DirtyPathStatus>,
    entries: Vec<DirtySnapshotEntryDigest>,
) -> Result<DirtySnapshotFingerprint, DaemonError> {
    let inputs = DirtySnapshotFingerprintInputs {
        schema_version: 1,
        base_head_commit_oid: &base_head_commit_oid,
        base_head_tree_oid: &base_head_tree_oid,
        index_tree_oid: &index_tree_oid,
        staged: &staged,
        unstaged: &unstaged,
        entries: &entries,
    };
    let snapshot_digest = canonical_json_sha256(&inputs).map_err(|err: ManifestHashError| {
        DaemonError::ArtifactKeyMismatch {
            artifact_id: "dirty-snapshot".to_owned(),
            reason: err.to_string(),
        }
    })?;
    Ok(DirtySnapshotFingerprint {
        schema_version: 1,
        base_head_commit_oid,
        base_head_tree_oid,
        index_tree_oid,
        staged,
        unstaged,
        entries,
        snapshot_digest,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use tempfile::TempDir;

    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init"]);
        fs::write(tmp.path().join("tracked.rs"), b"fn tracked() {}\n").unwrap();
        git(tmp.path(), &["add", "tracked.rs"]);
        git(tmp.path(), &["commit", "-m", "initial"]);
        tmp
    }

    #[test]
    fn dirty_snapshot_hashes_exact_file_bytes_not_mtime() {
        let tmp = repo();
        let options = DirtySnapshotOptions {
            include_untracked: false,
            ..DirtySnapshotOptions::new(tmp.path())
        };
        let first = DirtySnapshotSource::capture(&options).unwrap();
        let first_digest = first.fingerprint().snapshot_digest.clone();

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(tmp.path().join("tracked.rs"))
            .unwrap();
        file.write_all(b"// changed\n").unwrap();
        let changed = DirtySnapshotSource::capture(&options).unwrap();

        assert_ne!(first_digest, changed.fingerprint().snapshot_digest);
        let changed_again = DirtySnapshotSource::capture(&options).unwrap();
        assert_eq!(
            changed.fingerprint().snapshot_digest,
            changed_again.fingerprint().snapshot_digest
        );
    }

    #[test]
    fn dirty_snapshot_digest_stable_when_same_bytes_are_rewritten() {
        let tmp = repo();
        let path = tmp.path().join("tracked.rs");
        let options = DirtySnapshotOptions::new(tmp.path());
        let first = DirtySnapshotSource::capture(&options).unwrap();

        fs::write(path, b"fn tracked() {}\n").unwrap();
        let rewritten = DirtySnapshotSource::capture(&options).unwrap();

        assert_eq!(
            first.fingerprint().snapshot_digest,
            rewritten.fingerprint().snapshot_digest
        );
    }

    #[test]
    fn dirty_snapshot_includes_staged_unstaged_untracked_and_deletions() {
        let tmp = repo();
        fs::write(tmp.path().join("staged.rs"), b"fn staged() {}\n").unwrap();
        git(tmp.path(), &["add", "staged.rs"]);
        fs::write(tmp.path().join("tracked.rs"), b"fn changed() {}\n").unwrap();
        fs::write(tmp.path().join("untracked.rs"), b"fn untracked() {}\n").unwrap();
        fs::remove_file(tmp.path().join("staged.rs")).unwrap();

        let options = DirtySnapshotOptions {
            include_untracked: true,
            ..DirtySnapshotOptions::new(tmp.path())
        };
        let snapshot = DirtySnapshotSource::capture(&options).unwrap();
        let fingerprint = snapshot.fingerprint();

        assert!(fingerprint.index_tree_oid.is_some());
        assert!(
            fingerprint
                .staged
                .iter()
                .any(|status| status.path == "staged.rs")
        );
        assert!(
            fingerprint
                .unstaged
                .iter()
                .any(|status| status.path == "tracked.rs" || status.path == "staged.rs")
        );
        assert!(
            fingerprint
                .entries
                .iter()
                .any(|entry| entry.path == "untracked.rs")
        );
        assert!(
            fingerprint
                .entries
                .iter()
                .any(|entry| entry.path == "staged.rs" && entry.kind == "deletion")
        );
    }

    #[test]
    fn dirty_snapshot_exposes_exact_parse_bytes() {
        let tmp = repo();
        fs::write(tmp.path().join("tracked.rs"), b"fn exact() {}\n").unwrap();
        let options = DirtySnapshotOptions::new(tmp.path());
        let snapshot = DirtySnapshotSource::capture(&options).unwrap();
        let entry = snapshot
            .entries()
            .iter()
            .find(|entry| entry.path.display_lossy() == "tracked.rs")
            .unwrap();

        assert_eq!(
            snapshot.read_entry_bytes(entry).unwrap(),
            b"fn exact() {}\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dirty_snapshot_fingerprints_executable_mode_changes() {
        let tmp = repo();
        let path = tmp.path().join("tracked.rs");
        let options = DirtySnapshotOptions::new(tmp.path());
        let first = DirtySnapshotSource::capture(&options).unwrap();

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        let changed = DirtySnapshotSource::capture(&options).unwrap();

        assert_ne!(
            first.fingerprint().snapshot_digest,
            changed.fingerprint().snapshot_digest
        );
        assert!(
            changed
                .fingerprint()
                .entries
                .iter()
                .any(|entry| entry.path == "tracked.rs" && entry.executable)
        );
    }

    #[cfg(unix)]
    #[test]
    fn dirty_snapshot_fingerprints_symlink_target_changes() {
        let tmp = repo();
        symlink("tracked.rs", tmp.path().join("link.rs")).unwrap();
        let options = DirtySnapshotOptions {
            include_untracked: true,
            ..DirtySnapshotOptions::new(tmp.path())
        };
        let first = DirtySnapshotSource::capture(&options).unwrap();
        fs::remove_file(tmp.path().join("link.rs")).unwrap();
        symlink("missing.rs", tmp.path().join("link.rs")).unwrap();
        let changed = DirtySnapshotSource::capture(&options).unwrap();

        assert_ne!(
            first.fingerprint().snapshot_digest,
            changed.fingerprint().snapshot_digest
        );
        assert!(
            changed
                .fingerprint()
                .entries
                .iter()
                .any(|entry| entry.path == "link.rs" && entry.kind == "symlink")
        );
    }
}
