//! Analysis cache with memory-mapped file support

use super::{CondensationDag, CsrAdjacency, SccData};
use crate::graph::unified::edge::EdgeKind;
#[cfg(test)]
use crate::graph::unified::edge::ResolvedVia;
use crate::graph::unified::persistence::{GraphStorage, load_from_path};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Memory-mapped analysis cache for fast loading
pub struct AnalysisCache {
    /// Hash of the manifest file
    pub manifest_hash: String,
    /// Hash of node IDs for identity verification
    pub node_id_hash: [u8; 32],
    csr: Option<Arc<CsrAdjacency>>,
    sccs: HashMap<EdgeKind, Arc<SccData>>,
    condensations: HashMap<EdgeKind, Arc<CondensationDag>>,
}

impl AnalysisCache {
    /// Load analysis cache from index directory
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn load(index_path: &Path) -> Result<Self> {
        let storage = GraphStorage::new(index_path);

        let manifest_hash = super::persistence::compute_manifest_hash(storage.manifest_path())?;
        let graph = load_from_path(storage.snapshot_path(), None)?;
        let node_id_hash = super::persistence::compute_node_id_hash(&graph.snapshot());
        let identity =
            super::persistence::AnalysisIdentity::new(manifest_hash.clone(), node_id_hash);

        let csr = super::persistence::load_csr_checked(&storage.analysis_csr_path(), &identity)?;

        let scc_calls =
            super::persistence::load_scc_checked(&storage.analysis_scc_path("calls"), &identity)?;
        let scc_imports =
            super::persistence::load_scc_checked(&storage.analysis_scc_path("imports"), &identity)?;
        let scc_references = super::persistence::load_scc_checked(
            &storage.analysis_scc_path("references"),
            &identity,
        )?;
        let scc_inherits = super::persistence::load_scc_checked(
            &storage.analysis_scc_path("inherits"),
            &identity,
        )?;

        let cond_calls = super::persistence::load_condensation_checked(
            &storage.analysis_cond_path("calls"),
            &identity,
        )?;
        let cond_imports = super::persistence::load_condensation_checked(
            &storage.analysis_cond_path("imports"),
            &identity,
        )?;
        let cond_references = super::persistence::load_condensation_checked(
            &storage.analysis_cond_path("references"),
            &identity,
        )?;
        let cond_inherits = super::persistence::load_condensation_checked(
            &storage.analysis_cond_path("inherits"),
            &identity,
        )?;

        let mut sccs = HashMap::new();
        sccs.insert(scc_calls.edge_kind.clone(), Arc::new(scc_calls));
        sccs.insert(scc_imports.edge_kind.clone(), Arc::new(scc_imports));
        sccs.insert(scc_references.edge_kind.clone(), Arc::new(scc_references));
        sccs.insert(scc_inherits.edge_kind.clone(), Arc::new(scc_inherits));

        let mut condensations = HashMap::new();
        condensations.insert(cond_calls.edge_kind.clone(), Arc::new(cond_calls));
        condensations.insert(cond_imports.edge_kind.clone(), Arc::new(cond_imports));
        condensations.insert(cond_references.edge_kind.clone(), Arc::new(cond_references));
        condensations.insert(cond_inherits.edge_kind.clone(), Arc::new(cond_inherits));

        Ok(Self {
            manifest_hash,
            node_id_hash,
            csr: Some(Arc::new(csr)),
            sccs,
            condensations,
        })
    }

    /// Get CSR adjacency
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn get_csr(&self) -> Result<Arc<CsrAdjacency>> {
        self.csr
            .clone()
            .ok_or_else(|| anyhow::anyhow!("CSR not loaded"))
    }

    /// Get SCC data for an edge kind (matches by discriminant, ignores metadata)
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn get_scc(&self, kind: &EdgeKind) -> Result<Arc<SccData>> {
        let kind_discriminant = std::mem::discriminant(kind);
        self.sccs
            .iter()
            .find(|(k, _)| std::mem::discriminant(*k) == kind_discriminant)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| anyhow::anyhow!("SCC for {kind:?} not loaded"))
    }

    /// Get condensation DAG for an edge kind (matches by discriminant, ignores metadata)
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn get_condensation(&self, kind: &EdgeKind) -> Result<Arc<CondensationDag>> {
        let kind_discriminant = std::mem::discriminant(kind);
        self.condensations
            .iter()
            .find(|(k, _)| std::mem::discriminant(*k) == kind_discriminant)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| anyhow::anyhow!("Condensation for {kind:?} not loaded"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::analysis::{AnalysisIdentity, GraphAnalyses};
    use crate::graph::unified::compaction::snapshot_edges;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::persistence::{BuildProvenance, Manifest, save_to_path};
    use crate::graph::unified::storage::NodeEntry;
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_analysis_cache_load_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).unwrap();

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let name = graph.strings_mut().intern("A").unwrap();
        let node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file_id))
            .unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        graph
            .edges_mut()
            .add_edge(node, node, kind.clone(), file_id);

        save_to_path(&graph, storage.snapshot_path()).unwrap();

        let snapshot_bytes = std::fs::read(storage.snapshot_path()).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&snapshot_bytes);
        let snapshot_hash = hex::encode(hasher.finalize());

        let node_count = graph.nodes().len();
        let edge_stats = graph.edges().stats();
        let edge_count = edge_stats.forward.csr_edge_count + edge_stats.forward.delta_edge_count;

        let provenance = BuildProvenance::default();
        let manifest = Manifest::new(
            root.to_string_lossy().to_string(),
            node_count,
            edge_count,
            snapshot_hash,
            provenance,
        );
        manifest.save(storage.manifest_path()).unwrap();

        let snapshot = graph.snapshot();
        let edges = snapshot.edges();
        let forward_store = edges.forward();
        let compaction_snapshot = snapshot_edges(&forward_store, snapshot.nodes().len());
        let analyses = GraphAnalyses::build_all(&compaction_snapshot).unwrap();

        let manifest_hash =
            crate::graph::unified::analysis::compute_manifest_hash(storage.manifest_path())
                .unwrap();
        let node_id_hash = crate::graph::unified::analysis::compute_node_id_hash(&snapshot);
        let identity = AnalysisIdentity::new(manifest_hash, node_id_hash);
        analyses.persist_all(&storage, &identity).unwrap();

        let cache = AnalysisCache::load(root).unwrap();
        assert!(cache.get_csr().is_ok());
        assert!(cache.get_scc(&kind).is_ok());
        assert!(cache.get_condensation(&kind).is_ok());
    }
}
