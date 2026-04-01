//! Persistence layer for the unified graph architecture.
//!
//! This module provides save/load functionality for the unified graph,
//! enabling efficient serialization and deserialization of the complete
//! graph state including nodes, edges, strings, files, and indices.
//!
//! # Format
//!
//! The persistence format is a binary format using postcard serialization:
//! - Magic bytes: `SQRY_GRAPH_V5` (13 bytes)
//! - Version header with counts and config provenance
//! - Serialized components in order
//!
//! # Config Provenance
//!
//! Starting with V2, the graph header includes `config_provenance` which
//! records which configuration was used when building the graph. This enables:
//! - Detecting config drift (config changed since graph was built)
//! - Tracking CLI/env overrides used during build
//! - Reproducibility analysis
//!
//! # Storage Layout
//!
//! The unified graph is stored in the `.sqry/graph/` directory:
//! ```text
//! .sqry/graph/
//! ├── manifest.json     # Metadata and checksums
//! ├── snapshot.sqry     # Binary graph snapshot
//! └── config/           # Configuration files
//!     └── config.json   # Build configuration
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::persistence::{GraphStorage, Manifest};
//! use sqry_core::graph::unified::CodeGraph;
//! use std::path::Path;
//!
//! // Create storage for a project
//! let storage = GraphStorage::new(Path::new("/path/to/project"));
//!
//! // Check if graph exists
//! if storage.exists() {
//!     let manifest = storage.load_manifest()?;
//!     println!("Graph has {} nodes", manifest.node_count);
//! }
//!
//! // Save graph to disk
//! let graph = CodeGraph::new();
//! persistence::save_to_path(&graph, storage.snapshot_path())?;
//! ```

pub mod format;
pub mod manifest;
pub mod snapshot;

use std::path::{Path, PathBuf};
use std::time::Duration;

pub use format::{GraphHeader, MAGIC_BYTES, VERSION};
pub use manifest::{
    BuildProvenance, ConfigProvenance, ConfigProvenanceBuilder, MANIFEST_SCHEMA_VERSION, Manifest,
    OverrideEntry, OverrideSource, SNAPSHOT_FORMAT_VERSION, compute_config_checksum,
    default_provenance,
};
pub use snapshot::{
    PersistenceError, check_config_drift, load_from_bytes, load_from_path, load_header_from_path,
    save_to_path, save_to_path_with_provenance, validate_snapshot, verify_snapshot_bytes,
};

// ============================================================================
// Graph Storage (directory-based storage manager)
// ============================================================================

/// Directory name for unified graph storage.
const GRAPH_DIR_NAME: &str = ".sqry/graph";

/// Directory name for analysis artifacts.
const ANALYSIS_DIR_NAME: &str = ".sqry/analysis";

/// Filename for the graph manifest.
const MANIFEST_FILE_NAME: &str = "manifest.json";

/// Filename for the graph snapshot.
const SNAPSHOT_FILE_NAME: &str = "snapshot.sqry";

/// Storage manager for unified graph and analysis files.
///
/// `GraphStorage` manages the `.sqry/` directory structure, providing
/// access to graph files (manifest, snapshot) and analysis artifacts
/// (CSR, SCC, condensation DAGs).
///
/// # Directory Structure
///
/// ```text
/// .sqry/
/// ├── graph/
/// │   ├── manifest.json     # Graph metadata (node/edge counts, checksums)
/// │   ├── snapshot.sqry     # Binary graph snapshot
/// │   └── config/           # Build configuration
/// │       └── config.json   # Configuration used during build
/// └── analysis/
///     ├── adjacency.csr     # CSR adjacency matrix
///     ├── scc_calls.scc     # SCC data for call edges
///     ├── scc_imports.scc   # SCC data for import edges
///     ├── cond_calls.dag    # Condensation DAG for call edges
///     └── ...               # Other edge-kind artifacts
/// ```
///
/// # Example
///
/// ```rust,ignore
/// use sqry_core::graph::unified::persistence::GraphStorage;
/// use std::path::Path;
///
/// let storage = GraphStorage::new(Path::new("/path/to/project"));
///
/// if storage.exists() {
///     let manifest = storage.load_manifest()?;
///     let age = storage.snapshot_age(&manifest)?;
///     println!("Graph built {} seconds ago", age.as_secs());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct GraphStorage {
    /// Path to the `.sqry/graph/` directory.
    graph_dir: PathBuf,
    /// Path to the `.sqry/analysis/` directory.
    analysis_dir: PathBuf,
    /// Path to the manifest file.
    manifest_path: PathBuf,
    /// Path to the snapshot file.
    snapshot_path: PathBuf,
}

impl GraphStorage {
    /// Creates a new storage manager for the given project root.
    ///
    /// # Arguments
    ///
    /// * `root_path` - Root directory of the project
    ///
    /// # Returns
    ///
    /// A `GraphStorage` instance configured for `{root_path}/.sqry/`
    #[must_use]
    pub fn new(root_path: &Path) -> Self {
        let graph_dir = root_path.join(GRAPH_DIR_NAME);
        let analysis_dir = root_path.join(ANALYSIS_DIR_NAME);
        Self {
            manifest_path: graph_dir.join(MANIFEST_FILE_NAME),
            snapshot_path: graph_dir.join(SNAPSHOT_FILE_NAME),
            graph_dir,
            analysis_dir,
        }
    }

    /// Returns the path to the `.sqry/graph/` directory.
    #[must_use]
    pub fn graph_dir(&self) -> &Path {
        &self.graph_dir
    }

    /// Returns the path to the `.sqry/analysis/` directory.
    #[must_use]
    pub fn analysis_dir(&self) -> &Path {
        &self.analysis_dir
    }

    /// Returns the path to an SCC artifact file for a given edge kind.
    ///
    /// Example: `analysis_scc_path("calls")` returns `.sqry/analysis/scc_calls.scc`
    #[must_use]
    pub fn analysis_scc_path(&self, edge_kind: &str) -> PathBuf {
        self.analysis_dir.join(format!("scc_{edge_kind}.scc"))
    }

    /// Returns the path to a condensation DAG artifact file for a given edge kind.
    ///
    /// Example: `analysis_cond_path("calls")` returns `.sqry/analysis/cond_calls.dag`
    #[must_use]
    pub fn analysis_cond_path(&self, edge_kind: &str) -> PathBuf {
        self.analysis_dir.join(format!("cond_{edge_kind}.dag"))
    }

    /// Returns the path to the CSR adjacency artifact file.
    #[must_use]
    pub fn analysis_csr_path(&self) -> PathBuf {
        self.analysis_dir.join("adjacency.csr")
    }

    /// Returns the path to the manifest file.
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Returns the path to the snapshot file.
    #[must_use]
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Checks if a unified graph exists (manifest file exists).
    #[must_use]
    pub fn exists(&self) -> bool {
        self.manifest_path.exists()
    }

    /// Checks if the snapshot file exists.
    #[must_use]
    pub fn snapshot_exists(&self) -> bool {
        self.snapshot_path.exists()
    }

    /// Loads the graph manifest from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest file cannot be read or parsed.
    pub fn load_manifest(&self) -> std::io::Result<Manifest> {
        Manifest::load(&self.manifest_path)
    }

    /// Computes the age of the snapshot based on the manifest timestamp.
    ///
    /// # Arguments
    ///
    /// * `manifest` - The loaded manifest containing the build timestamp
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp cannot be parsed or if system time is invalid.
    pub fn snapshot_age(&self, manifest: &Manifest) -> std::io::Result<Duration> {
        // Parse the RFC3339 timestamp from the manifest
        let built_at = chrono::DateTime::parse_from_rfc3339(&manifest.built_at)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let now = chrono::Utc::now();
        let duration = now.signed_duration_since(built_at.with_timezone(&chrono::Utc));

        // Convert to std::time::Duration (clamped to non-negative)
        let seconds = duration.num_seconds().max(0);
        let seconds = u64::try_from(seconds).unwrap_or(0);
        Ok(Duration::from_secs(seconds))
    }

    /// Returns the path to the config directory.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.graph_dir.join("config")
    }

    /// Returns the path to the config file.
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.config_dir().join("config.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_graph_storage_paths() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        assert_eq!(storage.graph_dir(), tmp.path().join(".sqry/graph"));
        assert_eq!(
            storage.manifest_path(),
            tmp.path().join(".sqry/graph/manifest.json")
        );
        assert_eq!(
            storage.snapshot_path(),
            tmp.path().join(".sqry/graph/snapshot.sqry")
        );
        assert!(!storage.exists());
        assert!(!storage.snapshot_exists());
    }

    #[test]
    fn test_graph_storage_exists() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        // Initially doesn't exist
        assert!(!storage.exists());

        // Create the directory and manifest
        std::fs::create_dir_all(storage.graph_dir()).unwrap();
        std::fs::write(storage.manifest_path(), "{}").unwrap();

        // Now exists
        assert!(storage.exists());
    }

    #[test]
    fn test_manifest_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        // Create directory
        std::fs::create_dir_all(storage.graph_dir()).unwrap();

        // Create and save manifest
        let provenance = BuildProvenance::new("0.15.0", "sqry index");
        let manifest = Manifest::new("/test/path", 100, 200, "abc123", provenance);
        manifest.save(storage.manifest_path()).unwrap();

        // Load and verify
        let loaded = storage.load_manifest().unwrap();
        assert_eq!(loaded.node_count, 100);
        assert_eq!(loaded.edge_count, 200);
        assert_eq!(loaded.snapshot_sha256, "abc123");
        assert_eq!(loaded.build_provenance.sqry_version, "0.15.0");
    }

    #[test]
    fn test_snapshot_age() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        // Create manifest with current timestamp
        let provenance = BuildProvenance::new("0.15.0", "sqry index");
        let manifest = Manifest::new("/test/path", 100, 200, "abc123", provenance);

        // Age should be very small (just created)
        let age = storage.snapshot_age(&manifest).unwrap();
        assert!(age.as_secs() < 2, "Age should be less than 2 seconds");
    }

    /// Regression test (Step 10, #10): Snapshot without manifest → not ready.
    ///
    /// Under manifest-last persistence, a snapshot file without manifest means
    /// the build was interrupted. `storage.exists()` must return false.
    #[test]
    fn test_reader_readiness_snapshot_without_manifest() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        // Create graph directory and snapshot (but no manifest)
        std::fs::create_dir_all(storage.graph_dir()).unwrap();
        std::fs::write(storage.snapshot_path(), b"fake snapshot data").unwrap();

        // snapshot_exists() should be true (file exists)
        assert!(storage.snapshot_exists(), "Snapshot file should exist");

        // exists() should be false (no manifest → not ready)
        assert!(
            !storage.exists(),
            "Index should NOT be ready without manifest (manifest-last ordering)"
        );
    }

    /// Regression test (Step 10, #11): Manifest without snapshot → exists() true, load fails gracefully.
    ///
    /// Manifest present but snapshot missing indicates corruption. `storage.exists()`
    /// returns true (manifest present), but `load_from_path()` must fail gracefully
    /// (error, not panic), so auto-index paths can trigger rebuild.
    #[test]
    fn test_reader_readiness_manifest_without_snapshot() {
        let tmp = TempDir::new().unwrap();
        let storage = GraphStorage::new(tmp.path());

        // Create graph directory and manifest (but no snapshot)
        std::fs::create_dir_all(storage.graph_dir()).unwrap();
        let provenance = BuildProvenance::new("3.6.0", "test");
        let manifest = Manifest::new(
            tmp.path().display().to_string(),
            100,
            200,
            "sha256",
            provenance,
        );
        manifest.save(storage.manifest_path()).unwrap();

        // exists() should be true (manifest present)
        assert!(
            storage.exists(),
            "Index should report exists (manifest present)"
        );

        // snapshot_exists() should be false (no snapshot file)
        assert!(!storage.snapshot_exists(), "Snapshot should not exist");

        // load_from_path should fail gracefully (error, not panic)
        let result = load_from_path(storage.snapshot_path(), None);
        assert!(
            result.is_err(),
            "Loading from missing snapshot should return error, not panic"
        );
    }
}
