//! Binary persistence for analysis files
//!
//! Uses postcard for fast serialization with `AnalysisIdentity` validation.

use super::condensation::CondensationDag;
use super::csr::CsrAdjacency;
use super::scc::SccData;
use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::persistence::GraphStorage;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Identity metadata used to validate analysis files against the current graph.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisIdentity {
    /// SHA-256 hash of the manifest.json contents.
    pub manifest_hash: String,
    /// SHA-256 hash of node ordering and identity.
    pub node_id_hash: [u8; 32],
}

impl AnalysisIdentity {
    /// Create a new analysis identity.
    #[must_use]
    pub fn new(manifest_hash: String, node_id_hash: [u8; 32]) -> Self {
        Self {
            manifest_hash,
            node_id_hash,
        }
    }

    /// Ensure this identity matches the expected value (full validation).
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn ensure_matches(&self, expected: &AnalysisIdentity) -> Result<()> {
        if self.manifest_hash != expected.manifest_hash {
            anyhow::bail!(
                "analysis manifest hash mismatch: expected {}, got {}",
                expected.manifest_hash,
                self.manifest_hash
            );
        }
        if self.node_id_hash != expected.node_id_hash {
            anyhow::bail!(
                "analysis node_id_hash mismatch: expected {}, got {}",
                hex::encode(expected.node_id_hash),
                hex::encode(self.node_id_hash)
            );
        }
        Ok(())
    }

    /// Ensure the manifest hash matches (lightweight validation).
    ///
    /// This skips the expensive `node_id_hash` comparison and only validates
    /// the `manifest_hash`. Since the manifest contains `snapshot_sha256`,
    /// this transitively validates graph identity without the O(N) node hash
    /// recomputation needed for full validation.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest hash does not match.
    pub fn ensure_manifest_matches(&self, expected_manifest_hash: &str) -> Result<()> {
        if self.manifest_hash != expected_manifest_hash {
            anyhow::bail!(
                "analysis manifest hash mismatch: expected {}, got {}",
                expected_manifest_hash,
                self.manifest_hash
            );
        }
        Ok(())
    }
}

/// Compute a SHA-256 hash of the manifest file contents.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn compute_manifest_hash(path: &Path) -> Result<String> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

/// Compute a stable hash of node ID ordering and identity for validation.
#[must_use]
pub fn compute_node_id_hash(snapshot: &GraphSnapshot) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let strings = snapshot.strings();
    let files = snapshot.files();

    let mut nodes: Vec<_> = snapshot.nodes().iter().collect();
    nodes.sort_by_key(|(node_id, _)| node_id.index());

    for (node_id, entry) in nodes {
        hasher.update(node_id.index().to_le_bytes());
        hasher.update(node_id.generation().to_le_bytes());

        let kind_str = format!("{:?}", entry.kind);
        hash_str(&mut hasher, Some(kind_str.as_str()));
        let name = strings.resolve(entry.name);
        hash_str(&mut hasher, name.as_deref());

        let qualified = entry.qualified_name.and_then(|id| strings.resolve(id));
        hash_str(&mut hasher, qualified.as_deref());

        let file_path = files
            .resolve(entry.file)
            .map(|path| path.to_string_lossy().into_owned());
        hash_str(&mut hasher, file_path.as_deref());
    }

    let digest = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    output
}

#[allow(clippy::cast_possible_truncation)] // String lengths in practice won't exceed u32::MAX
fn hash_str(hasher: &mut Sha256, value: Option<&str>) {
    let len = value.map_or(0u32, |s| s.len() as u32);
    hasher.update(len.to_le_bytes());
    if let Some(s) = value {
        hasher.update(s.as_bytes());
    }
}

/// Persist CSR adjacency to disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn persist_csr(csr: &CsrAdjacency, identity: &AnalysisIdentity, path: &Path) -> Result<()> {
    let encoded = postcard::to_allocvec(&(identity, csr))?;
    std::fs::write(path, encoded)?;
    Ok(())
}

/// Load CSR adjacency from disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_csr(path: &Path) -> Result<(CsrAdjacency, AnalysisIdentity)> {
    let data = std::fs::read(path)?;
    let (identity, csr) = postcard::from_bytes(&data)?;
    Ok((csr, identity))
}

/// Persist SCC data to disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn persist_scc(scc: &SccData, identity: &AnalysisIdentity, path: &Path) -> Result<()> {
    let encoded = postcard::to_allocvec(&(identity, scc))?;
    std::fs::write(path, encoded)?;
    Ok(())
}

/// Load SCC data from disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_scc(path: &Path) -> Result<(SccData, AnalysisIdentity)> {
    let data = std::fs::read(path)?;
    let (identity, scc) = postcard::from_bytes(&data)?;
    Ok((scc, identity))
}

/// Persist condensation DAG to disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn persist_condensation(
    dag: &CondensationDag,
    identity: &AnalysisIdentity,
    path: &Path,
) -> Result<()> {
    let encoded = postcard::to_allocvec(&(identity, dag))?;
    std::fs::write(path, encoded)?;
    Ok(())
}

/// Load condensation DAG from disk.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_condensation(path: &Path) -> Result<(CondensationDag, AnalysisIdentity)> {
    let data = std::fs::read(path)?;
    let (identity, mut dag): (AnalysisIdentity, CondensationDag) = postcard::from_bytes(&data)?;
    dag.fixup_after_load();
    Ok((dag, identity))
}

/// Load CSR adjacency and validate against expected analysis identity.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_csr_checked(path: &Path, expected: &AnalysisIdentity) -> Result<CsrAdjacency> {
    let (csr, identity) = load_csr(path)?;
    identity.ensure_matches(expected)?;
    Ok(csr)
}

/// Load SCC data and validate against expected analysis identity.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_scc_checked(path: &Path, expected: &AnalysisIdentity) -> Result<SccData> {
    let (scc, identity) = load_scc(path)?;
    identity.ensure_matches(expected)?;
    Ok(scc)
}

/// Load condensation DAG and validate against expected analysis identity.
/// Returns an error if the operation fails.
///
/// # Errors
///
pub fn load_condensation_checked(
    path: &Path,
    expected: &AnalysisIdentity,
) -> Result<CondensationDag> {
    let (dag, identity) = load_condensation(path)?;
    identity.ensure_matches(expected)?;
    Ok(dag)
}

// ============================================================================
// Manifest-only validated loaders (fast path for runtime loading)
// ============================================================================

/// Load SCC data with manifest-hash-only validation.
///
/// This is the fast-path loader that avoids the O(N) `compute_node_id_hash()`
/// recomputation. Since the manifest contains `snapshot_sha256`, manifest-hash
/// comparison transitively validates graph identity.
///
/// # Errors
///
/// Returns an error if the file cannot be read, deserialized, or the manifest
/// hash does not match.
pub fn load_scc_manifest_checked(path: &Path, expected_manifest_hash: &str) -> Result<SccData> {
    let (scc, identity) = load_scc(path)?;
    identity.ensure_manifest_matches(expected_manifest_hash)?;
    Ok(scc)
}

/// Load condensation DAG with manifest-hash-only validation.
///
/// This is the fast-path loader that avoids the O(N) `compute_node_id_hash()`
/// recomputation.
///
/// # Errors
///
/// Returns an error if the file cannot be read, deserialized, or the manifest
/// hash does not match.
pub fn load_condensation_manifest_checked(
    path: &Path,
    expected_manifest_hash: &str,
) -> Result<CondensationDag> {
    let (dag, identity) = load_condensation(path)?;
    identity.ensure_manifest_matches(expected_manifest_hash)?;
    Ok(dag)
}

// ============================================================================
// High-level validated loaders (DRY wrappers used by MCP, LSP, CLI)
// ============================================================================

/// Load SCC data with automatic identity validation.
///
/// Validates the analysis file against the current manifest hash.
/// This uses manifest-hash-only validation to avoid the expensive O(N)
/// `compute_node_id_hash()` recomputation. Since the manifest includes
/// `snapshot_sha256`, this transitively ensures graph identity.
///
/// Returns `None` if analysis files don't exist or validation fails.
/// The caller should fall back to query-time computation in that case.
#[must_use]
pub fn try_load_scc(
    storage: &GraphStorage,
    _snapshot: &GraphSnapshot,
    edge_kind: &str,
) -> Option<SccData> {
    let scc_file = storage.analysis_scc_path(edge_kind);
    if !scc_file.exists() {
        return None;
    }

    let manifest_hash = compute_manifest_hash(storage.manifest_path()).ok()?;

    load_scc_manifest_checked(&scc_file, &manifest_hash).ok()
}

/// Load SCC and condensation DAG with automatic identity validation.
///
/// Validates analysis files against the current manifest hash.
/// This uses manifest-hash-only validation to avoid the expensive O(N)
/// `compute_node_id_hash()` recomputation.
///
/// Returns `None` if either analysis file doesn't exist or validation fails.
/// The caller should fall back to query-time computation in that case.
#[must_use]
pub fn try_load_scc_and_condensation(
    storage: &GraphStorage,
    _snapshot: &GraphSnapshot,
    edge_kind: &str,
) -> Option<(SccData, CondensationDag)> {
    let scc_file = storage.analysis_scc_path(edge_kind);
    let cond_file = storage.analysis_cond_path(edge_kind);

    if !scc_file.exists() || !cond_file.exists() {
        return None;
    }

    let manifest_hash = compute_manifest_hash(storage.manifest_path()).ok()?;

    let scc_data = load_scc_manifest_checked(&scc_file, &manifest_hash).ok()?;
    let cond_dag = load_condensation_manifest_checked(&cond_file, &manifest_hash).ok()?;

    Some((scc_data, cond_dag))
}

/// Load CSR + SCC + condensation DAG for path reconstruction.
///
/// Validates all analysis files against the current manifest hash.
/// Returns `None` if any file is missing or stale. The caller should
/// fall back to graph-level BFS in that case.
#[must_use]
pub fn try_load_path_analysis(
    storage: &GraphStorage,
    edge_kind: &str,
) -> Option<(CsrAdjacency, SccData, CondensationDag)> {
    let csr_file = storage.analysis_csr_path();
    let scc_file = storage.analysis_scc_path(edge_kind);
    let cond_file = storage.analysis_cond_path(edge_kind);

    if !csr_file.exists() || !scc_file.exists() || !cond_file.exists() {
        log::debug!("Analysis files not found for edge kind '{edge_kind}', skipping fast path");
        return None;
    }

    let manifest_hash = match compute_manifest_hash(storage.manifest_path()) {
        Ok(h) => h,
        Err(e) => {
            log::debug!("Cannot compute manifest hash: {e}, skipping analysis fast path");
            return None;
        }
    };

    let csr = match load_csr(&csr_file) {
        Ok((csr, identity)) => {
            if identity.ensure_manifest_matches(&manifest_hash).is_err() {
                log::info!("Analysis CSR is stale (manifest hash mismatch), falling back to BFS");
                return None;
            }
            csr
        }
        Err(e) => {
            log::info!("Failed to load CSR: {e}, skipping analysis fast path");
            return None;
        }
    };

    let scc_data = match load_scc_manifest_checked(&scc_file, &manifest_hash) {
        Ok(scc) => scc,
        Err(e) => {
            log::info!("Analysis SCC is stale or corrupt: {e}, falling back to BFS");
            return None;
        }
    };

    let cond_dag = match load_condensation_manifest_checked(&cond_file, &manifest_hash) {
        Ok(dag) => dag,
        Err(e) => {
            log::info!("Analysis condensation is stale or corrupt: {e}, falling back to BFS");
            return None;
        }
    };

    log::info!("Loaded precomputed analysis for edge kind '{edge_kind}'");
    Some((csr, scc_data, cond_dag))
}
