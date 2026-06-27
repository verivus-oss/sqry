//! Durable revision graph artifact storage.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs2::FileExt as _;
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_daemon_protocol::{ArtifactId, ResolvedRevision};

use crate::error::DaemonError;

use super::manifest::hex_sha256;
use super::{ArtifactKeyInputs, RevisionArtifactManifest};

const GRAPH_FILE_NAME: &str = "graph.bin";
const DERIVED_FILE_NAME: &str = "derived.bin";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const LOCK_SUFFIX: &str = ".lock";

/// Result of publishing an immutable revision artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactPublishStatus {
    /// Artifact was written by this call.
    Published,
    /// A manifest-valid artifact already existed and was left untouched.
    AlreadyPresent,
}

/// Published artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPublishResult {
    /// Publish status.
    pub status: ArtifactPublishStatus,
    /// Manifest associated with the artifact.
    pub manifest: RevisionArtifactManifest,
    /// Artifact directory.
    pub artifact_dir: PathBuf,
}

/// On-disk revision artifact or partial artifact directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactInventoryEntry {
    /// Repository identity hash directory containing the artifact.
    pub repo_identity_hash: String,
    /// Artifact id inferred from the directory name.
    pub artifact_id: ArtifactId,
    /// Artifact directory.
    pub artifact_dir: PathBuf,
    /// Recursive disk usage in bytes.
    pub size_bytes: u64,
    /// Best-effort modification timestamp used for LRU pruning.
    pub modified_at: SystemTime,
    /// Whether `manifest.json` exists.
    pub has_manifest: bool,
    /// Whether `graph.bin` exists.
    pub has_graph: bool,
}

impl ArtifactInventoryEntry {
    /// True when the artifact directory is incomplete after a crash.
    #[must_use]
    pub const fn is_partial(&self) -> bool {
        !self.has_manifest || !self.has_graph
    }
}

/// Revision artifact store rooted under daemon cache state.
#[derive(Debug, Clone)]
pub struct RevisionArtifactStore {
    root: PathBuf,
}

impl RevisionArtifactStore {
    /// Construct an artifact store at an explicit root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default daemon cache root for revision graph artifacts.
    #[must_use]
    pub fn default_cache_root() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sqry")
            .join("revision-graphs")
    }

    /// Artifact store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory containing all artifacts for one local repository identity.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::ArtifactKeyMismatch`] if the repository identity
    /// hash contains path separators or unsafe characters.
    pub fn repo_dir(&self, repo_identity_hash: &str) -> Result<PathBuf, DaemonError> {
        validate_safe_component(repo_identity_hash, "repo_identity_hash")?;
        Ok(self.root.join(repo_identity_hash))
    }

    /// Directory for an artifact id.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::ArtifactKeyMismatch`] if any path component is
    /// unsafe.
    pub fn artifact_dir(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
    ) -> Result<PathBuf, DaemonError> {
        validate_safe_component(&artifact_id.0, "artifact_id")?;
        Ok(self.repo_dir(repo_identity_hash)?.join(&artifact_id.0))
    }

    /// Publish a graph artifact and manifest atomically.
    ///
    /// Existing manifest-valid artifacts are returned unchanged. Partial final
    /// directories without a valid manifest are removed before publishing.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if path validation, graph persistence, manifest
    /// serialization, locking, or atomic publish fails.
    pub fn publish_graph(
        &self,
        graph: &CodeGraph,
        artifact_id: ArtifactId,
        resolved_revision: ResolvedRevision,
        key_inputs: ArtifactKeyInputs,
        derived_bytes: Option<&[u8]>,
    ) -> Result<ArtifactPublishResult, DaemonError> {
        let mut manifest =
            RevisionArtifactManifest::new(artifact_id.clone(), resolved_revision, key_inputs)
                .map_err(|err| DaemonError::ArtifactKeyMismatch {
                    artifact_id: artifact_id.0.clone(),
                    reason: err.to_string(),
                })?;
        self.validate_manifest_identity(&manifest)?;

        let repo_dir = self.repo_dir(&manifest.key_inputs.repo_identity_hash)?;
        fs::create_dir_all(&repo_dir).map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to create revision artifact repo dir: {err}"),
            path: Some(repo_dir.clone()),
        })?;
        let _lock = ArtifactPublishLock::acquire(&repo_dir, &artifact_id)?;

        let artifact_dir =
            self.artifact_dir(&manifest.key_inputs.repo_identity_hash, &artifact_id)?;
        let graph_path = artifact_dir.join(GRAPH_FILE_NAME);
        if artifact_dir.join(MANIFEST_FILE_NAME).exists() {
            if graph_path.exists() {
                match self.load_manifest_for_inputs(
                    &manifest.key_inputs.repo_identity_hash,
                    &artifact_id,
                    &manifest.key_inputs,
                ) {
                    Ok(existing) => {
                        return Ok(ArtifactPublishResult {
                            status: ArtifactPublishStatus::AlreadyPresent,
                            manifest: existing,
                            artifact_dir,
                        });
                    }
                    Err(_err) => {
                        remove_artifact_dir(
                            &artifact_dir,
                            "failed to remove invalid artifact directory before republish",
                        )?;
                    }
                }
            } else {
                remove_artifact_dir(
                    &artifact_dir,
                    "failed to remove manifest-only artifact directory",
                )?;
            }
        }

        if artifact_dir.exists() {
            remove_artifact_dir(&artifact_dir, "failed to remove partial artifact directory")?;
        }

        let staging_dir = tempfile::Builder::new()
            .prefix(&format!("{}.", artifact_id.0))
            .tempdir_in(&repo_dir)
            .map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to create revision artifact staging dir: {err}"),
                path: Some(repo_dir.clone()),
            })?;
        let staging_path = staging_dir.path().to_path_buf();
        let graph_path = staging_path.join(GRAPH_FILE_NAME);
        save_to_path(graph, &graph_path).map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to save revision graph artifact: {err}"),
            path: Some(graph_path.clone()),
        })?;
        let graph_sha256 = file_sha256(&graph_path)?;

        let mut derived_sha256 = None;
        if let Some(bytes) = derived_bytes {
            let derived_path = staging_path.join(DERIVED_FILE_NAME);
            write_file_durable(&derived_path, bytes)?;
            derived_sha256 = Some(hex_sha256(bytes));
        }

        manifest = manifest.with_artifact_hashes(graph_sha256, derived_sha256);
        let manifest_bytes =
            manifest
                .to_canonical_json()
                .map_err(|err| DaemonError::ArtifactKeyMismatch {
                    artifact_id: artifact_id.0.clone(),
                    reason: err.to_string(),
                })?;
        write_manifest_last(&staging_path.join(MANIFEST_FILE_NAME), &manifest_bytes)?;

        let kept_staging_path = staging_dir.keep();
        debug_assert_eq!(kept_staging_path, staging_path);
        fs::rename(&staging_path, &artifact_dir).map_err(|err| {
            DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to atomically publish artifact directory: {err}"),
                path: Some(artifact_dir.clone()),
            }
        })?;
        sync_parent(&repo_dir)?;

        Ok(ArtifactPublishResult {
            status: ArtifactPublishStatus::Published,
            manifest,
            artifact_dir,
        })
    }

    /// Load and validate an artifact manifest against expected key inputs.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::ArtifactKeyMismatch`] if the manifest does not
    /// match the requested artifact id or key inputs.
    pub fn load_manifest_for_inputs(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
        expected_inputs: &ArtifactKeyInputs,
    ) -> Result<RevisionArtifactManifest, DaemonError> {
        let manifest_path = self
            .artifact_dir(repo_identity_hash, artifact_id)?
            .join(MANIFEST_FILE_NAME);
        let bytes =
            fs::read(&manifest_path).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to read revision artifact manifest: {err}"),
                path: Some(manifest_path.clone()),
            })?;
        let manifest: RevisionArtifactManifest =
            serde_json::from_slice(&bytes).map_err(|err| DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: format!("invalid revision artifact manifest JSON: {err}"),
            })?;
        self.validate_manifest_identity(&manifest)?;
        self.validate_artifact_files(repo_identity_hash, artifact_id, &manifest)?;
        if &manifest.artifact_id != artifact_id {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: format!("manifest artifact id was {}", manifest.artifact_id.0),
            });
        }
        if &manifest.key_inputs != expected_inputs {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: "manifest key inputs do not match requested inputs".to_owned(),
            });
        }
        Ok(manifest)
    }

    /// Load a graph only after manifest validation succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if manifest validation or graph loading fails.
    pub fn load_graph_for_inputs(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
        expected_inputs: &ArtifactKeyInputs,
    ) -> Result<(CodeGraph, RevisionArtifactManifest), DaemonError> {
        let manifest =
            self.load_manifest_for_inputs(repo_identity_hash, artifact_id, expected_inputs)?;
        let graph_path = self
            .artifact_dir(repo_identity_hash, artifact_id)?
            .join(GRAPH_FILE_NAME);
        let graph = load_from_path(&graph_path, None).map_err(|err| {
            DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to load revision graph artifact: {err}"),
                path: Some(graph_path),
            }
        })?;
        Ok((graph, manifest))
    }

    /// Path to the derived artifact for the same graph snapshot identity.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::ArtifactKeyMismatch`] if path components are
    /// unsafe.
    pub fn derived_path(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
    ) -> Result<PathBuf, DaemonError> {
        Ok(self
            .artifact_dir(repo_identity_hash, artifact_id)?
            .join(DERIVED_FILE_NAME))
    }

    /// Inventory all artifact directories under this store.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the root or a repository directory cannot be
    /// listed.
    pub fn inventory(&self) -> Result<Vec<ArtifactInventoryEntry>, DaemonError> {
        let mut entries = Vec::new();
        if !self.root.exists() {
            return Ok(entries);
        }
        for repo_entry in
            fs::read_dir(&self.root).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to list revision artifact root: {err}"),
                path: Some(self.root.clone()),
            })?
        {
            let repo_entry = repo_entry.map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to read revision artifact root entry: {err}"),
                path: Some(self.root.clone()),
            })?;
            let repo_path = repo_entry.path();
            if !repo_path.is_dir() {
                continue;
            }
            let repo_identity_hash = repo_entry.file_name().to_string_lossy().into_owned();
            if validate_safe_component(&repo_identity_hash, "repo_identity_hash").is_err() {
                continue;
            }
            self.inventory_repo(&repo_identity_hash, &repo_path, &mut entries)?;
        }
        entries.sort_by(|left, right| {
            left.repo_identity_hash
                .cmp(&right.repo_identity_hash)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        Ok(entries)
    }

    /// Remove one artifact directory by identity.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the identity is unsafe or removal fails.
    pub fn remove_artifact(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
    ) -> Result<u64, DaemonError> {
        let artifact_dir = self.artifact_dir(repo_identity_hash, artifact_id)?;
        let bytes = dir_size(&artifact_dir);
        if artifact_dir.exists() {
            remove_artifact_dir(&artifact_dir, "failed to remove revision artifact")?;
        }
        Ok(bytes)
    }

    /// Remove crash-left partial artifact directories.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if inventory or removal fails.
    pub fn remove_partial_artifacts(&self) -> Result<Vec<ArtifactInventoryEntry>, DaemonError> {
        let partials: Vec<_> = self
            .inventory()?
            .into_iter()
            .filter(ArtifactInventoryEntry::is_partial)
            .collect();
        for entry in &partials {
            remove_artifact_dir(
                &entry.artifact_dir,
                "failed to remove partial revision artifact during recovery",
            )?;
        }
        Ok(partials)
    }

    fn inventory_repo(
        &self,
        repo_identity_hash: &str,
        repo_path: &Path,
        entries: &mut Vec<ArtifactInventoryEntry>,
    ) -> Result<(), DaemonError> {
        for artifact_entry in
            fs::read_dir(repo_path).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to list revision artifact repo dir: {err}"),
                path: Some(repo_path.to_path_buf()),
            })?
        {
            let artifact_entry =
                artifact_entry.map_err(|err| DaemonError::RevisionSourceUnavailable {
                    reason: format!("failed to read revision artifact entry: {err}"),
                    path: Some(repo_path.to_path_buf()),
                })?;
            let artifact_dir = artifact_entry.path();
            if !artifact_dir.is_dir() {
                continue;
            }
            let artifact_name = artifact_entry.file_name().to_string_lossy().into_owned();
            if validate_safe_component(&artifact_name, "artifact_id").is_err() {
                continue;
            }
            let metadata = fs::symlink_metadata(&artifact_dir).map_err(|err| {
                DaemonError::RevisionSourceUnavailable {
                    reason: format!("failed to stat revision artifact dir: {err}"),
                    path: Some(artifact_dir.clone()),
                }
            })?;
            entries.push(ArtifactInventoryEntry {
                repo_identity_hash: repo_identity_hash.to_owned(),
                artifact_id: ArtifactId(artifact_name),
                size_bytes: dir_size(&artifact_dir),
                modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                has_manifest: artifact_dir.join(MANIFEST_FILE_NAME).is_file(),
                has_graph: artifact_dir.join(GRAPH_FILE_NAME).is_file(),
                artifact_dir,
            });
        }
        Ok(())
    }

    fn validate_manifest_identity(
        &self,
        manifest: &RevisionArtifactManifest,
    ) -> Result<(), DaemonError> {
        let computed_id =
            manifest
                .key_inputs
                .artifact_id()
                .map_err(|err| DaemonError::ArtifactKeyMismatch {
                    artifact_id: manifest.artifact_id.0.clone(),
                    reason: err.to_string(),
                })?;
        if computed_id != manifest.artifact_id {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: manifest.artifact_id.0.clone(),
                reason: format!("computed artifact id was {}", computed_id.0),
            });
        }
        let computed_digest =
            manifest
                .key_inputs
                .input_digest()
                .map_err(|err| DaemonError::ArtifactKeyMismatch {
                    artifact_id: manifest.artifact_id.0.clone(),
                    reason: err.to_string(),
                })?;
        if computed_digest != manifest.artifact_inputs.digest {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: manifest.artifact_id.0.clone(),
                reason: "manifest input digest does not match key inputs".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_artifact_files(
        &self,
        repo_identity_hash: &str,
        artifact_id: &ArtifactId,
        manifest: &RevisionArtifactManifest,
    ) -> Result<(), DaemonError> {
        if manifest.graph_snapshot_sha256.is_empty() {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: "manifest is missing graph snapshot SHA-256".to_owned(),
            });
        }
        let graph_path = self
            .artifact_dir(repo_identity_hash, artifact_id)?
            .join(GRAPH_FILE_NAME);
        let actual_graph_sha = file_sha256(&graph_path)?;
        if actual_graph_sha != manifest.graph_snapshot_sha256 {
            return Err(DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: "graph snapshot SHA-256 does not match manifest".to_owned(),
            });
        }

        if let Some(expected_derived_sha) = &manifest.derived_artifact_sha256 {
            let derived_path = self.derived_path(repo_identity_hash, artifact_id)?;
            let actual_derived_sha = file_sha256(&derived_path)?;
            if &actual_derived_sha != expected_derived_sha {
                return Err(DaemonError::ArtifactKeyMismatch {
                    artifact_id: artifact_id.0.clone(),
                    reason: "derived artifact SHA-256 does not match manifest".to_owned(),
                });
            }
        }
        Ok(())
    }
}

struct ArtifactPublishLock {
    file: File,
}

impl ArtifactPublishLock {
    fn acquire(repo_dir: &Path, artifact_id: &ArtifactId) -> Result<Self, DaemonError> {
        validate_safe_component(&artifact_id.0, "artifact_id")?;
        let lock_path = repo_dir.join(format!("{}{}", artifact_id.0, LOCK_SUFFIX));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to open artifact lock: {err}"),
                path: Some(lock_path.clone()),
            })?;
        file.lock_exclusive()
            .map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to lock artifact: {err}"),
                path: Some(lock_path),
            })?;
        Ok(Self { file })
    }
}

impl Drop for ArtifactPublishLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn validate_safe_component(value: &str, name: &str) -> Result<(), DaemonError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(DaemonError::ArtifactKeyMismatch {
            artifact_id: value.to_owned(),
            reason: format!("{name} is not a safe artifact path component"),
        });
    }
    Ok(())
}

fn remove_artifact_dir(path: &Path, reason: &str) -> Result<(), DaemonError> {
    fs::remove_dir_all(path).map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("{reason}: {err}"),
        path: Some(path.to_path_buf()),
    })
}

fn dir_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| dir_size(&entry.path()))
            .sum()
    })
}

fn write_file_durable(path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    let mut file = File::create(path).map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("failed to create artifact file: {err}"),
        path: Some(path.to_path_buf()),
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to write artifact file durably: {err}"),
            path: Some(path.to_path_buf()),
        })
}

fn write_manifest_last(path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    let parent = path
        .parent()
        .ok_or_else(|| DaemonError::RevisionSourceUnavailable {
            reason: "manifest path had no parent".to_owned(),
            path: Some(path.to_path_buf()),
        })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|err| {
        DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to create temporary manifest: {err}"),
            path: Some(parent.to_path_buf()),
        }
    })?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to write temporary manifest: {err}"),
            path: Some(path.to_path_buf()),
        })?;
    temp.persist(path)
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to publish manifest: {}", err.error),
            path: Some(path.to_path_buf()),
        })?;
    sync_parent(parent)
}

fn file_sha256(path: &Path) -> Result<String, DaemonError> {
    let mut file = File::open(path).map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("failed to open artifact for hashing: {err}"),
        path: Some(path.to_path_buf()),
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to read artifact for hashing: {err}"),
            path: Some(path.to_path_buf()),
        })?;
    Ok(hex_sha256(&bytes))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), DaemonError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to sync artifact parent directory: {err}"),
            path: Some(path.to_path_buf()),
        })
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), DaemonError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::workspace::revision::{GraphSchemaFingerprint, PathScope, SourceDigest};
    use sqry_daemon_protocol::{
        ArtifactId, ObjectFormat, RepositoryIdentity, RevisionSelector, SourceByteMode,
    };

    fn key_inputs(config_hash: &str) -> ArtifactKeyInputs {
        ArtifactKeyInputs {
            repo_identity_hash: "repoabcdef0123456789".to_owned(),
            source_digest: SourceDigest::Tree {
                tree_oid: "a".repeat(40),
            },
            object_format: ObjectFormat::Sha1,
            path_scope: PathScope::Repository,
            source_byte_mode: SourceByteMode::RawGitObjects,
            checkout_fingerprint: None,
            graph_schema: GraphSchemaFingerprint {
                graph_schema_version: 1,
                derived_schema_version: 1,
                sqry_build_version: "22.0.4".to_owned(),
                plugin_roster_digest: "plugins".to_owned(),
                graph_config_hash: config_hash.to_owned(),
            },
        }
    }

    fn resolved_revision() -> ResolvedRevision {
        ResolvedRevision {
            selector: RevisionSelector::Commit {
                oid: "b".repeat(40),
            },
            repository: RepositoryIdentity {
                repo_identity_hash: "repoabcdef0123456789".to_owned(),
                object_format: ObjectFormat::Sha1,
                remote_fingerprint: None,
            },
            commit_oid: Some("b".repeat(40)),
            tree_oid: "a".repeat(40),
            object_format: ObjectFormat::Sha1,
            source_byte_mode: SourceByteMode::RawGitObjects,
            resolved_at: "2026-06-26T00:00:00Z".to_owned(),
        }
    }

    fn artifact_id(inputs: &ArtifactKeyInputs) -> ArtifactId {
        inputs.artifact_id().unwrap()
    }

    #[test]
    fn publish_writes_graph_derived_and_manifest_under_cache_root() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);

        let result = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                Some(b"derived bytes"),
            )
            .unwrap();

        assert_eq!(result.status, ArtifactPublishStatus::Published);
        assert!(result.artifact_dir.join(GRAPH_FILE_NAME).is_file());
        assert!(result.artifact_dir.join(DERIVED_FILE_NAME).is_file());
        assert!(result.artifact_dir.join(MANIFEST_FILE_NAME).is_file());
        assert!(!result.manifest.graph_snapshot_sha256.is_empty());
        assert!(result.manifest.derived_artifact_sha256.is_some());
        assert_eq!(
            fs::read(
                store
                    .derived_path(&inputs.repo_identity_hash, &artifact_id)
                    .unwrap()
            )
            .unwrap(),
            b"derived bytes"
        );
    }

    #[test]
    fn graph_checksum_mismatch_blocks_reuse_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        let published = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                None,
            )
            .unwrap();
        fs::write(published.artifact_dir.join(GRAPH_FILE_NAME), b"corrupt").unwrap();

        let err = store
            .load_graph_for_inputs(&inputs.repo_identity_hash, &artifact_id, &inputs)
            .expect_err("corrupt graph must fail manifest validation");

        assert!(matches!(err, DaemonError::ArtifactKeyMismatch { .. }));
    }

    #[test]
    fn corrupt_complete_artifact_is_replaced_on_publish() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        let published = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                None,
            )
            .unwrap();
        fs::write(published.artifact_dir.join(GRAPH_FILE_NAME), b"corrupt").unwrap();

        let republished = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id,
                resolved_revision(),
                inputs,
                Some(b"healed derived"),
            )
            .unwrap();

        assert_eq!(republished.status, ArtifactPublishStatus::Published);
        assert_ne!(
            fs::read(republished.artifact_dir.join(GRAPH_FILE_NAME)).unwrap(),
            b"corrupt"
        );
        assert_eq!(
            fs::read(republished.artifact_dir.join(DERIVED_FILE_NAME)).unwrap(),
            b"healed derived"
        );
    }

    #[test]
    fn load_validates_manifest_before_graph_load() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                None,
            )
            .unwrap();

        let mut wrong_inputs = inputs.clone();
        wrong_inputs.graph_schema.graph_config_hash = "config-b".to_owned();
        let err = store
            .load_graph_for_inputs(&inputs.repo_identity_hash, &artifact_id, &wrong_inputs)
            .expect_err("manifest mismatch must block graph load");

        assert!(matches!(err, DaemonError::ArtifactKeyMismatch { .. }));
    }

    #[test]
    fn load_graph_succeeds_after_manifest_and_snapshot_validation() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                None,
            )
            .unwrap();

        let (graph, manifest) = store
            .load_graph_for_inputs(&inputs.repo_identity_hash, &artifact_id, &inputs)
            .unwrap();

        assert_eq!(graph.node_count(), 0);
        assert_eq!(manifest.artifact_id, artifact_id);
    }

    #[test]
    fn second_publish_reuses_manifest_valid_artifact_without_overwrite() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        let first = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id.clone(),
                resolved_revision(),
                inputs.clone(),
                None,
            )
            .unwrap();
        fs::write(first.artifact_dir.join("sentinel"), b"keep").unwrap();

        let second = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id,
                resolved_revision(),
                inputs,
                Some(b"new derived"),
            )
            .unwrap();

        assert_eq!(second.status, ArtifactPublishStatus::AlreadyPresent);
        assert_eq!(
            fs::read(first.artifact_dir.join("sentinel")).unwrap(),
            b"keep"
        );
        assert!(!first.artifact_dir.join(DERIVED_FILE_NAME).exists());
    }

    #[test]
    fn partial_final_artifact_without_manifest_is_replaced() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        let artifact_dir = store
            .artifact_dir(&inputs.repo_identity_hash, &artifact_id)
            .unwrap();
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(artifact_dir.join(GRAPH_FILE_NAME), b"partial").unwrap();

        let result = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id,
                resolved_revision(),
                inputs,
                None,
            )
            .unwrap();

        assert_eq!(result.status, ArtifactPublishStatus::Published);
        assert!(result.artifact_dir.join(MANIFEST_FILE_NAME).is_file());
        assert_ne!(
            fs::read(result.artifact_dir.join(GRAPH_FILE_NAME)).unwrap(),
            b"partial"
        );
    }

    #[test]
    fn manifest_only_artifact_is_replaced_on_publish() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        let artifact_dir = store
            .artifact_dir(&inputs.repo_identity_hash, &artifact_id)
            .unwrap();
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(artifact_dir.join(MANIFEST_FILE_NAME), b"{}").unwrap();

        let result = store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id,
                resolved_revision(),
                inputs,
                None,
            )
            .unwrap();

        assert_eq!(result.status, ArtifactPublishStatus::Published);
        assert!(result.artifact_dir.join(GRAPH_FILE_NAME).is_file());
    }

    #[test]
    fn live_source_root_graph_directory_is_not_touched() {
        let tmp = TempDir::new().unwrap();
        let live_root = tmp.path().join("repo");
        let live_graph = live_root.join(".sqry/graph");
        fs::create_dir_all(&live_graph).unwrap();
        fs::write(live_graph.join("live-marker"), b"live").unwrap();

        let store = RevisionArtifactStore::new(tmp.path().join("cache/revision-graphs"));
        let inputs = key_inputs("config-a");
        let artifact_id = artifact_id(&inputs);
        store
            .publish_graph(
                &CodeGraph::new(),
                artifact_id,
                resolved_revision(),
                inputs,
                None,
            )
            .unwrap();

        assert_eq!(fs::read(live_graph.join("live-marker")).unwrap(), b"live");
    }
}
