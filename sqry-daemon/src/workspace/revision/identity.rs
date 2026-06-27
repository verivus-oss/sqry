//! Local Git repository identity discovery for revision artifacts.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use sqry_daemon_protocol::{ObjectFormat, RepositoryIdentity};
use thiserror::Error;

use super::manifest::{ManifestHashError, canonical_json_sha256, hex_sha256};

/// Local repository identity discovery errors.
#[derive(Debug, Error)]
pub enum RepositoryIdentityError {
    /// A Git command failed.
    #[error("git {args:?} failed in {}: {stderr}", root.display())]
    Git {
        root: PathBuf,
        args: Vec<String>,
        stderr: String,
    },
    /// Git returned an unsupported object format.
    #[error("unsupported Git object format {format:?} in {}", root.display())]
    UnsupportedObjectFormat { root: PathBuf, format: String },
    /// Filesystem error while reading Git metadata.
    #[error("failed to read Git metadata at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Identity hash serialization failed.
    #[error(transparent)]
    ManifestHash(#[from] ManifestHashError),
}

/// Daemon-local Git repository identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalRepositoryIdentity {
    /// Canonical Git common directory.
    pub common_dir: PathBuf,
    /// Git object id format.
    pub object_format: ObjectFormat,
    /// SHA-256 fingerprint over alternates metadata.
    pub alternates_fingerprint: String,
    /// Whether `git worktree` operations are supported locally.
    pub supports_worktrees: bool,
    /// Optional redacted remote fingerprint for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_fingerprint: Option<String>,
    /// Stable daemon-local hash over identity inputs.
    pub repo_identity_hash: String,
}

impl LocalRepositoryIdentity {
    /// Discover local repository identity without fetching network objects.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryIdentityError`] when `root` is not a Git repository,
    /// Git metadata cannot be read, or Git reports an unsupported object
    /// format.
    pub fn discover(root: &Path) -> Result<Self, RepositoryIdentityError> {
        let common_dir = git_stdout(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let common_dir = PathBuf::from(common_dir.trim());
        let object_format = parse_object_format(
            root,
            git_stdout(root, &["rev-parse", "--show-object-format"])?.trim(),
        )?;
        let alternates_fingerprint = alternates_fingerprint(&common_dir)?;
        let supports_worktrees = git_status(root, &["worktree", "list", "--porcelain"]);
        let remote_fingerprint = git_stdout(root, &["remote", "get-url", "origin"])
            .ok()
            .map(|remote| redacted_remote_fingerprint(remote.trim()));

        Self::from_parts(
            common_dir,
            object_format,
            alternates_fingerprint,
            supports_worktrees,
            remote_fingerprint,
        )
    }

    /// Construct identity from precomputed local metadata.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryIdentityError`] if deterministic identity hashing
    /// fails.
    pub fn from_parts(
        common_dir: PathBuf,
        object_format: ObjectFormat,
        alternates_fingerprint: String,
        supports_worktrees: bool,
        remote_fingerprint: Option<String>,
    ) -> Result<Self, RepositoryIdentityError> {
        let hash_inputs = RepositoryIdentityHashInputs {
            common_dir: common_dir.display().to_string(),
            object_format,
            alternates_fingerprint: alternates_fingerprint.clone(),
            supports_worktrees,
            remote_fingerprint: remote_fingerprint.clone(),
        };
        let repo_identity_hash = canonical_json_sha256(&hash_inputs)?;
        Ok(Self {
            common_dir,
            object_format,
            alternates_fingerprint,
            supports_worktrees,
            remote_fingerprint,
            repo_identity_hash,
        })
    }

    /// Redacted protocol identity.
    #[must_use]
    pub fn to_wire(&self) -> RepositoryIdentity {
        RepositoryIdentity {
            repo_identity_hash: self.repo_identity_hash.clone(),
            object_format: self.object_format,
            remote_fingerprint: self.remote_fingerprint.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct RepositoryIdentityHashInputs {
    common_dir: String,
    object_format: ObjectFormat,
    alternates_fingerprint: String,
    supports_worktrees: bool,
    remote_fingerprint: Option<String>,
}

fn parse_object_format(root: &Path, format: &str) -> Result<ObjectFormat, RepositoryIdentityError> {
    match format {
        "sha1" => Ok(ObjectFormat::Sha1),
        "sha256" => Ok(ObjectFormat::Sha256),
        other => Err(RepositoryIdentityError::UnsupportedObjectFormat {
            root: root.to_path_buf(),
            format: other.to_owned(),
        }),
    }
}

fn alternates_fingerprint(common_dir: &Path) -> Result<String, RepositoryIdentityError> {
    let path = common_dir.join("objects").join("info").join("alternates");
    match std::fs::read(&path) {
        Ok(bytes) => Ok(hex_sha256(&normalize_alternates(&bytes))),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(hex_sha256(b"")),
        Err(source) => Err(RepositoryIdentityError::Io { path, source }),
    }
}

fn normalize_alternates(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    lines.sort_unstable();
    lines.join("\n").into_bytes()
}

fn redacted_remote_fingerprint(remote: &str) -> String {
    if remote.is_empty() {
        return hex_sha256(b"");
    }
    format!("remote-sha256:{}", hex_sha256(remote.as_bytes()))
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String, RepositoryIdentityError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|source| RepositoryIdentityError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(RepositoryIdentityError::Git {
            root: root.to_path_buf(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn git_status(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_fingerprint_redacts_raw_remote() {
        let raw = "https://token@example.com/org/repo.git";
        let fingerprint = redacted_remote_fingerprint(raw);
        assert!(fingerprint.starts_with("remote-sha256:"));
        assert!(!fingerprint.contains("token"));
        assert!(!fingerprint.contains("example.com"));
    }

    #[test]
    fn alternates_fingerprint_normalizes_order_and_comments() {
        let first = normalize_alternates(b"# comment\n/b\n/a\n\n");
        let second = normalize_alternates(b"/a\n/b\n");
        assert_eq!(first, second);
    }

    #[test]
    fn identity_hash_changes_when_common_dir_changes() {
        let first = LocalRepositoryIdentity::from_parts(
            PathBuf::from("/repo/.git"),
            ObjectFormat::Sha1,
            "alts".to_owned(),
            true,
            Some(redacted_remote_fingerprint("https://example.com/repo.git")),
        )
        .unwrap();
        let second = LocalRepositoryIdentity::from_parts(
            PathBuf::from("/other/.git"),
            ObjectFormat::Sha1,
            "alts".to_owned(),
            true,
            Some(redacted_remote_fingerprint("https://example.com/repo.git")),
        )
        .unwrap();

        assert_ne!(first.repo_identity_hash, second.repo_identity_hash);
        assert_eq!(
            first.to_wire().remote_fingerprint,
            second.to_wire().remote_fingerprint
        );
    }
}
