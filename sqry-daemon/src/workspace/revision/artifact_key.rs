//! Deterministic revision graph artifact key construction.

use serde::{Deserialize, Serialize};
use sqry_daemon_protocol::{ArtifactId, ObjectFormat, SourceByteMode};

use super::manifest::{ManifestHashError, canonical_json_sha256};

/// Source identity component for artifact keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceDigest {
    /// Immutable Git tree object id.
    Tree {
        /// Full tree object id.
        tree_oid: String,
    },
    /// Dirty snapshot digest over captured exact bytes.
    DirtySnapshot {
        /// SHA-256 digest of the dirty snapshot manifest.
        snapshot_digest: String,
    },
    /// Managed worktree state digest.
    Worktree {
        /// Worktree id.
        worktree_id: String,
        /// Source digest for the worktree contents.
        source_digest: String,
    },
}

/// Path scope represented in an artifact key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PathScope {
    /// Entire repository.
    Repository,
    /// Explicit normalized relative paths.
    Paths {
        /// Sorted, normalized repository-relative path list.
        paths: Vec<String>,
    },
}

/// Checkout-byte filter fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutFilterFingerprint {
    /// Filter name from Git attributes/config.
    pub name: String,
    /// Redacted deterministic config fingerprint for this filter.
    pub config_hash: String,
}

/// Checkout-byte mode fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutByteFingerprint {
    /// `git --version` output.
    pub git_version: String,
    /// SHA-256 over relevant `.gitattributes` and info attributes bytes.
    pub attributes_fingerprint: String,
    /// Effective `core.autocrlf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_autocrlf: Option<String>,
    /// Effective EOL configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_eol: Option<String>,
    /// Effective working tree encoding configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_tree_encoding: Option<String>,
    /// Sorted filter fingerprints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<CheckoutFilterFingerprint>,
    /// Sparse checkout state/config fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_checkout_fingerprint: Option<String>,
    /// Whether Git LFS/smudge support was available.
    pub lfs_available: bool,
    /// Worktree-specific config fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_config_fingerprint: Option<String>,
}

/// Graph-affecting schema/config inputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphSchemaFingerprint {
    /// Graph schema version.
    pub graph_schema_version: u32,
    /// Derived-query DB schema version.
    pub derived_schema_version: u32,
    /// sqry build version.
    pub sqry_build_version: String,
    /// Digest of the plugin roster and plugin versions.
    pub plugin_roster_digest: String,
    /// Digest of graph-affecting daemon/sqry config.
    pub graph_config_hash: String,
}

/// Complete deterministic artifact key input set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactKeyInputs {
    /// Local repository identity hash.
    pub repo_identity_hash: String,
    /// Source digest for immutable tree, dirty snapshot, or worktree.
    pub source_digest: SourceDigest,
    /// Git object id format.
    pub object_format: ObjectFormat,
    /// Indexed path scope.
    pub path_scope: PathScope,
    /// Source byte mode.
    pub source_byte_mode: SourceByteMode,
    /// Checkout-byte fingerprint when `source_byte_mode == CheckoutBytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_fingerprint: Option<CheckoutByteFingerprint>,
    /// Graph-affecting schema/config/plugin inputs.
    pub graph_schema: GraphSchemaFingerprint,
}

impl ArtifactKeyInputs {
    /// Compute the manifest-verifiable input digest.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestHashError`] if deterministic JSON serialization fails.
    pub fn input_digest(&self) -> Result<String, ManifestHashError> {
        canonical_json_sha256(self)
    }

    /// Compute an opaque stable artifact id.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestHashError`] if deterministic JSON serialization fails.
    pub fn artifact_id(&self) -> Result<ArtifactId, ManifestHashError> {
        let digest = self.input_digest()?;
        let repo_prefix: String = self.repo_identity_hash.chars().take(16).collect();
        let input_prefix: String = digest.chars().take(32).collect();
        Ok(ArtifactId(format!(
            "revgraph-v1-{repo_prefix}-{input_prefix}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use sqry_daemon_protocol::{ObjectFormat, SourceByteMode};

    use super::*;

    fn graph_schema(config_hash: &str) -> GraphSchemaFingerprint {
        GraphSchemaFingerprint {
            graph_schema_version: 1,
            derived_schema_version: 1,
            sqry_build_version: "22.0.4".to_owned(),
            plugin_roster_digest: "rust@22.0.4,python@22.0.4".to_owned(),
            graph_config_hash: config_hash.to_owned(),
        }
    }

    fn tree_inputs(mode: SourceByteMode) -> ArtifactKeyInputs {
        ArtifactKeyInputs {
            repo_identity_hash: "repo-identity-hash".to_owned(),
            source_digest: SourceDigest::Tree {
                tree_oid: "a".repeat(40),
            },
            object_format: ObjectFormat::Sha1,
            path_scope: PathScope::Repository,
            source_byte_mode: mode,
            checkout_fingerprint: None,
            graph_schema: graph_schema("config-a"),
        }
    }

    #[test]
    fn tree_oid_alone_is_not_the_artifact_key() {
        let raw = tree_inputs(SourceByteMode::RawGitObjects);
        let mut checkout = tree_inputs(SourceByteMode::CheckoutBytes);
        checkout.checkout_fingerprint = Some(CheckoutByteFingerprint {
            git_version: "git version 2.51.0".to_owned(),
            attributes_fingerprint: "attrs".to_owned(),
            core_autocrlf: Some("false".to_owned()),
            core_eol: None,
            working_tree_encoding: None,
            filters: vec![CheckoutFilterFingerprint {
                name: "lfs".to_owned(),
                config_hash: "lfs-config".to_owned(),
            }],
            sparse_checkout_fingerprint: None,
            lfs_available: true,
            worktree_config_fingerprint: Some("worktree-config".to_owned()),
        });

        assert_eq!(raw.source_digest, checkout.source_digest);
        assert_ne!(raw.artifact_id().unwrap(), checkout.artifact_id().unwrap());
    }

    #[test]
    fn plugin_or_config_changes_change_artifact_id() {
        let first = tree_inputs(SourceByteMode::RawGitObjects);
        let mut second = tree_inputs(SourceByteMode::RawGitObjects);
        second.graph_schema = graph_schema("config-b");

        assert_eq!(first.source_digest, second.source_digest);
        assert_ne!(first.artifact_id().unwrap(), second.artifact_id().unwrap());
    }

    #[test]
    fn path_scope_changes_change_artifact_id() {
        let first = tree_inputs(SourceByteMode::RawGitObjects);
        let mut second = tree_inputs(SourceByteMode::RawGitObjects);
        second.path_scope = PathScope::Paths {
            paths: vec!["src/lib.rs".to_owned()],
        };

        assert_ne!(first.artifact_id().unwrap(), second.artifact_id().unwrap());
    }
}
