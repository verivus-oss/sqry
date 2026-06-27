//! Deterministic revision artifact manifest serialization.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqry_daemon_protocol::{ArtifactId, ArtifactInputDigest, ResolvedRevision};
use thiserror::Error;

use super::artifact_key::ArtifactKeyInputs;

/// Errors produced while canonicalizing manifest inputs.
#[derive(Debug, Error)]
pub enum ManifestHashError {
    /// JSON serialization failed.
    #[error("failed to serialize revision artifact manifest inputs: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Stored manifest for a revision graph artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevisionArtifactManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Opaque stable artifact id.
    pub artifact_id: ArtifactId,
    /// Digest over the canonical artifact inputs.
    pub artifact_inputs: ArtifactInputDigest,
    /// Resolved revision identity that produced the graph.
    pub resolved_revision: ResolvedRevision,
    /// Full set of key inputs used to compute the artifact id.
    pub key_inputs: ArtifactKeyInputs,
    /// SHA-256 of the persisted graph snapshot bytes.
    #[serde(default)]
    pub graph_snapshot_sha256: String,
    /// SHA-256 of the optional derived artifact bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_artifact_sha256: Option<String>,
}

impl RevisionArtifactManifest {
    /// Current revision artifact manifest schema.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Construct a manifest from resolved revision and artifact key inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestHashError`] if deterministic JSON serialization fails.
    pub fn new(
        artifact_id: ArtifactId,
        resolved_revision: ResolvedRevision,
        key_inputs: ArtifactKeyInputs,
    ) -> Result<Self, ManifestHashError> {
        let digest = ArtifactInputDigest {
            schema_version: Self::SCHEMA_VERSION,
            digest: key_inputs.input_digest()?,
        };
        Ok(Self {
            schema_version: Self::SCHEMA_VERSION,
            artifact_id,
            artifact_inputs: digest,
            resolved_revision,
            key_inputs,
            graph_snapshot_sha256: String::new(),
            derived_artifact_sha256: None,
        })
    }

    /// Attach persisted graph/derived artifact hashes.
    #[must_use]
    pub fn with_artifact_hashes(
        mut self,
        graph_snapshot_sha256: String,
        derived_artifact_sha256: Option<String>,
    ) -> Self {
        self.graph_snapshot_sha256 = graph_snapshot_sha256;
        self.derived_artifact_sha256 = derived_artifact_sha256;
        self
    }

    /// Serialize this manifest to deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestHashError`] if serialization fails.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, ManifestHashError> {
        Ok(serde_json::to_vec_pretty(self)?)
    }
}

/// SHA-256 hex digest of a serializable value's deterministic JSON bytes.
///
/// Struct field declaration order is the canonical order. Any map-like fields
/// used by revision artifact manifests must be represented as `BTreeMap` so
/// their JSON order is stable.
///
/// # Errors
///
/// Returns [`ManifestHashError`] if serialization fails.
pub fn canonical_json_sha256<T>(value: &T) -> Result<String, ManifestHashError>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    Ok(hex_sha256(&bytes))
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use sqry_daemon_protocol::{
        ArtifactId, ObjectFormat, RepositoryIdentity, ResolvedRevision, RevisionSelector,
        SourceByteMode,
    };

    use super::*;
    use crate::workspace::revision::artifact_key::{
        ArtifactKeyInputs, GraphSchemaFingerprint, PathScope, SourceDigest,
    };

    fn key_inputs() -> ArtifactKeyInputs {
        ArtifactKeyInputs {
            repo_identity_hash: "repo-hash".to_owned(),
            source_digest: SourceDigest::Tree {
                tree_oid: "a".repeat(40),
            },
            object_format: ObjectFormat::Sha1,
            path_scope: PathScope::Repository,
            source_byte_mode: SourceByteMode::RawGitObjects,
            checkout_fingerprint: None,
            graph_schema: GraphSchemaFingerprint {
                graph_schema_version: 1,
                derived_schema_version: 2,
                sqry_build_version: "22.0.4".to_owned(),
                plugin_roster_digest: "plugins".to_owned(),
                graph_config_hash: "config".to_owned(),
            },
        }
    }

    fn resolved_revision() -> ResolvedRevision {
        ResolvedRevision {
            selector: RevisionSelector::Commit {
                oid: "b".repeat(40),
            },
            repository: RepositoryIdentity {
                repo_identity_hash: "repo-hash".to_owned(),
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

    #[test]
    fn manifest_digest_is_deterministic() {
        let manifest_a = RevisionArtifactManifest::new(
            ArtifactId("artifact".to_owned()),
            resolved_revision(),
            key_inputs(),
        )
        .unwrap();
        let manifest_b = RevisionArtifactManifest::new(
            ArtifactId("artifact".to_owned()),
            resolved_revision(),
            key_inputs(),
        )
        .unwrap();

        assert_eq!(manifest_a.artifact_inputs, manifest_b.artifact_inputs);
        assert_eq!(
            manifest_a.to_canonical_json().unwrap(),
            manifest_b.to_canonical_json().unwrap()
        );
    }
}
