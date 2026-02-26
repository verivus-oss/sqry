//! Snapshot save/load implementation for the unified graph.
//!
//! This module provides functions to save and load graph snapshots
//! to/from disk using postcard serialization with length-prefixed framing.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use super::format::{GraphHeader, MAGIC_BYTES, VERSION};
use super::manifest::ConfigProvenance;
use crate::graph::unified::BidirectionalEdgeStore;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::storage::{AuxiliaryIndices, FileRegistry, NodeArena, StringInterner};
use crate::plugin::PluginManager;

/// Maximum snapshot data size (2 GB).
///
/// This limit is consistent with the full-buffer deserialization architecture
/// where both header and data buffers must fit in process memory simultaneously.
/// The largest known sqry snapshot is ~167 MB, well within this bound.
///
/// If snapshots grow beyond 2 GB, a streaming deserialization approach can replace
/// the buffer-based approach without a wire format change.
const MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum header size (1 MB).
const MAX_HEADER_BYTES: usize = 1_048_576;

/// Maximum reasonable node count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_NODES: usize = 100_000_000; // 100M nodes

/// Maximum reasonable edge count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_EDGES: usize = 1_000_000_000; // 1B edges

/// Maximum reasonable string count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_STRINGS: usize = 50_000_000; // 50M strings

/// Maximum reasonable file count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_FILES: usize = 1_000_000; // 1M files

/// Error type for persistence operations.
#[derive(Debug)]
pub enum PersistenceError {
    /// I/O error during read/write
    Io(std::io::Error),
    /// Serialization/deserialization error
    Serialization(String),
    /// Invalid magic bytes (not a sqry graph file)
    InvalidMagic {
        /// Expected magic bytes
        expected: Vec<u8>,
        /// Actual magic bytes found
        found: Vec<u8>,
    },
    /// Incompatible version
    IncompatibleVersion {
        /// Expected version number
        expected: u32,
        /// Actual version number found
        found: u32,
    },
    /// Plugin version mismatch (index built with different plugin versions)
    PluginVersionMismatch {
        /// Plugin ID with version mismatch
        plugin_id: String,
        /// Expected version (current)
        expected: String,
        /// Actual version found in index
        found: String,
    },
    /// Graph validation failed
    ValidationFailed(String),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Serialization(e) => write!(f, "Serialization error: {e}"),
            Self::InvalidMagic { expected, found } => {
                write!(
                    f,
                    "Invalid magic bytes: expected {expected:?}, found {found:?}. \
                     Index was created with an older version. Run `sqry index` to rebuild."
                )
            }
            Self::IncompatibleVersion { expected, found } => {
                write!(
                    f,
                    "Incompatible format version: expected {expected}, found {found}. \
                     Index was created with an older version. Run `sqry index` to rebuild."
                )
            }
            Self::PluginVersionMismatch {
                plugin_id,
                expected,
                found,
            } => {
                write!(
                    f,
                    "Plugin version mismatch for {plugin_id}: expected {expected}, found {found} (index needs rebuild)"
                )
            }
            Self::ValidationFailed(msg) => write!(f, "Validation failed: {msg}"),
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<std::io::Error> for PersistenceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<postcard::Error> for PersistenceError {
    fn from(e: postcard::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

/// Serializable snapshot of the graph state.
///
/// This is the complete graph state that gets persisted to disk.
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotData {
    /// Node storage
    nodes: NodeArena,
    /// Edge storage (forward + reverse)
    edges: BidirectionalEdgeStore,
    /// String interner
    strings: StringInterner,
    /// File registry
    files: FileRegistry,
    /// Auxiliary indices
    indices: AuxiliaryIndices,
}

/// Validate header counts to prevent allocation overflow on corrupted data.
///
/// This function checks that header counts are within reasonable bounds
/// to prevent memory allocation panics when postcard tries to deserialize
/// corrupted data with huge length fields.
fn validate_header_sanity(header: &GraphHeader) -> Result<(), PersistenceError> {
    if header.node_count > MAX_REASONABLE_NODES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable node_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.node_count, MAX_REASONABLE_NODES
        )));
    }
    if header.edge_count > MAX_REASONABLE_EDGES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable edge_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.edge_count, MAX_REASONABLE_EDGES
        )));
    }
    if header.string_count > MAX_REASONABLE_STRINGS {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable string_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.string_count, MAX_REASONABLE_STRINGS
        )));
    }
    if header.file_count > MAX_REASONABLE_FILES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable file_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.file_count, MAX_REASONABLE_FILES
        )));
    }
    Ok(())
}

fn validate_loaded_snapshot(
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotData,
) -> Result<(), PersistenceError> {
    let forward_stats = snapshot_data.edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;

    if header.node_count != snapshot_data.nodes.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "node_count mismatch: header={}, data={}",
            header.node_count,
            snapshot_data.nodes.len()
        )));
    }
    if header.edge_count != total_edges {
        return Err(PersistenceError::ValidationFailed(format!(
            "edge_count mismatch: header={}, data={}",
            header.edge_count, total_edges
        )));
    }
    if header.string_count != snapshot_data.strings.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "string_count mismatch: header={}, data={}",
            header.string_count,
            snapshot_data.strings.len()
        )));
    }
    if header.file_count != snapshot_data.files.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "file_count mismatch: header={}, data={}",
            header.file_count,
            snapshot_data.files.len()
        )));
    }

    Ok(())
}

/// Read a little-endian u32 from a reader.
fn read_u32_le(reader: &mut impl Read) -> Result<u32, std::io::Error> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Read a little-endian u64 from a reader.
fn read_u64_le(reader: &mut impl Read) -> Result<u64, std::io::Error> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Saves a graph to the specified path.
///
/// The graph is serialized using postcard with length-prefixed framing:
/// 1. Magic bytes (`SQRY_GRAPH_V5`)
/// 2. Header length (LE u32)
/// 3. Header (postcard-encoded `GraphHeader`)
/// 4. Data length (LE u64)
/// 5. Snapshot data (postcard-encoded `GraphSnapshotData`)
///
/// # Errors
///
/// Returns an error if the file cannot be created or serialization fails.
pub fn save_to_path(graph: &CodeGraph, path: impl AsRef<Path>) -> Result<(), PersistenceError> {
    let path = path.as_ref();
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    // Get a snapshot of the graph
    let snapshot = graph.snapshot();

    // Extract components from snapshot
    let nodes = snapshot.nodes().clone();
    let edges = snapshot.edges().clone();
    let strings = snapshot.strings().clone();
    let files = snapshot.files().clone();
    let indices = snapshot.indices().clone();

    // Create header
    let forward_stats = edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
    let header = GraphHeader::new(nodes.len(), total_edges, strings.len(), files.len());

    // Serialize header and data to buffers
    let header_bytes = postcard::to_allocvec(&header)?;
    let snapshot_data = GraphSnapshotData {
        nodes,
        edges,
        strings,
        files,
        indices,
    };
    let data_bytes = postcard::to_allocvec(&snapshot_data)?;

    // Validate sizes match load-time limits (symmetry: reject on save what load would reject)
    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large to save".to_string(),
        ));
    }
    if data_bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "data section too large to save".to_string(),
        ));
    }

    // Write framed format
    writer.write_all(MAGIC_BYTES)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Saves a graph to the specified path with config provenance.
///
/// This is the recommended save method when building graphs, as it records
/// the configuration used to build the graph for reproducibility tracking.
///
/// # Errors
///
/// Returns an error if the file cannot be created or serialization fails.
pub fn save_to_path_with_provenance(
    graph: &CodeGraph,
    path: impl AsRef<Path>,
    provenance: ConfigProvenance,
    plugins: &PluginManager,
) -> Result<(), PersistenceError> {
    let path = path.as_ref();
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    // Get a snapshot of the graph
    let snapshot = graph.snapshot();

    // Extract components from snapshot
    let nodes = snapshot.nodes().clone();
    let edges = snapshot.edges().clone();
    let strings = snapshot.strings().clone();
    let files = snapshot.files().clone();
    let indices = snapshot.indices().clone();

    // Collect plugin versions
    let plugin_versions: HashMap<String, String> = plugins
        .plugins()
        .iter()
        .map(|p| {
            let meta = p.metadata();
            (meta.id.to_string(), meta.version.to_string())
        })
        .collect();

    // Create header with provenance and plugin versions
    let forward_stats = edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
    let header = GraphHeader::with_provenance_and_plugins(
        nodes.len(),
        total_edges,
        strings.len(),
        files.len(),
        provenance,
        plugin_versions,
    );

    // Serialize header and data to buffers
    let header_bytes = postcard::to_allocvec(&header)?;
    let snapshot_data = GraphSnapshotData {
        nodes,
        edges,
        strings,
        files,
        indices,
    };
    let data_bytes = postcard::to_allocvec(&snapshot_data)?;

    // Validate sizes match load-time limits (symmetry: reject on save what load would reject)
    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large to save".to_string(),
        ));
    }
    if data_bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "data section too large to save".to_string(),
        ));
    }

    // Write framed format
    writer.write_all(MAGIC_BYTES)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Validates that plugin versions in the graph match current plugin versions.
fn validate_plugin_versions(
    header: &GraphHeader,
    plugins: &PluginManager,
) -> Result<(), PersistenceError> {
    // Collect current plugin versions
    let current_versions: HashMap<String, String> = plugins
        .plugins()
        .iter()
        .map(|p| {
            let meta = p.metadata();
            (meta.id.to_string(), meta.version.to_string())
        })
        .collect();

    // Check each plugin that was used to build the index
    for (plugin_id, stored_version) in header.plugin_versions() {
        match current_versions.get(plugin_id) {
            Some(current_version) if current_version != stored_version => {
                return Err(PersistenceError::PluginVersionMismatch {
                    plugin_id: plugin_id.clone(),
                    expected: current_version.clone(),
                    found: stored_version.clone(),
                });
            }
            None => {
                // Plugin was used to build index but is no longer available
                return Err(PersistenceError::PluginVersionMismatch {
                    plugin_id: plugin_id.clone(),
                    expected: "not installed".to_string(),
                    found: stored_version.clone(),
                });
            }
            Some(_) => {
                // Version matches, continue
            }
        }
    }

    Ok(())
}

/// Loads a graph from the specified path.
///
/// Reads the V4 framed format with length-prefixed sections and pre-allocation
/// validation. Rejects V3 and earlier snapshots with a clear error message.
///
/// # Errors
///
/// Returns an error if the file is invalid, corrupt, or incompatible.
#[allow(clippy::cast_possible_truncation)] // data_len validated < MAX_SNAPSHOT_BYTES (2 GB)
pub fn load_from_path(
    path: impl AsRef<Path>,
    plugins: Option<&PluginManager>,
) -> Result<CodeGraph, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    // Read and validate magic bytes
    let mut magic = [0u8; 13];
    reader.read_exact(&mut magic)?;
    bytes_consumed += 13;
    if &magic != MAGIC_BYTES {
        return Err(PersistenceError::InvalidMagic {
            expected: MAGIC_BYTES.to_vec(),
            found: magic.to_vec(),
        });
    }

    // Read header length and validate before allocation
    let header_len = read_u32_le(&mut reader)? as usize;
    bytes_consumed += 4;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    bytes_consumed += header_len as u64;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version
    if header.version != VERSION {
        return Err(PersistenceError::IncompatibleVersion {
            expected: VERSION,
            found: header.version,
        });
    }

    // Validate plugin versions (requires rebuild if mismatch) - skip if no plugin manager
    if let Some(plugin_manager) = plugins {
        validate_plugin_versions(&header, plugin_manager)?;
    }

    // Validate header counts before attempting data deserialization
    validate_header_sanity(&header)?;

    // Read data length and validate before allocation
    let data_len = read_u64_le(&mut reader)?;
    bytes_consumed += 8;
    if data_len > MAX_SNAPSHOT_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "data section too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if data_len > remaining {
        return Err(PersistenceError::ValidationFailed(
            "data length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize data
    let mut data_buf = vec![0u8; data_len as usize];
    reader.read_exact(&mut data_buf)?;
    let snapshot_data: GraphSnapshotData = postcard::from_bytes(&data_buf)?;

    // Reject trailing bytes
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? > 0 {
        return Err(PersistenceError::ValidationFailed(
            "unexpected trailing bytes after data section".to_string(),
        ));
    }

    validate_loaded_snapshot(&header, &snapshot_data)?;

    Ok(CodeGraph::from_components(
        snapshot_data.nodes,
        snapshot_data.edges,
        snapshot_data.strings,
        snapshot_data.files,
        snapshot_data.indices,
    ))
}

/// Validates a graph snapshot file without fully loading it.
///
/// Checks magic bytes, version, and header deserialization.
///
/// # Errors
///
/// Returns an error if validation fails.
pub fn validate_snapshot(path: impl AsRef<Path>) -> Result<bool, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    // Read and validate magic bytes
    let mut magic = [0u8; 13];
    reader.read_exact(&mut magic)?;
    bytes_consumed += 13;
    if &magic != MAGIC_BYTES {
        return Err(PersistenceError::InvalidMagic {
            expected: MAGIC_BYTES.to_vec(),
            found: magic.to_vec(),
        });
    }

    // Read header length
    let header_len = read_u32_le(&mut reader)? as usize;
    bytes_consumed += 4;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version
    if header.version != VERSION {
        return Err(PersistenceError::IncompatibleVersion {
            expected: VERSION,
            found: header.version,
        });
    }

    // Basic validation passed
    Ok(true)
}

/// Loads just the header from a graph file (fast, doesn't load graph data).
///
/// # Errors
///
/// Returns an error if the file cannot be read or is invalid.
pub fn load_header_from_path(path: impl AsRef<Path>) -> Result<GraphHeader, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    // Read and validate magic bytes
    let mut magic = [0u8; 13];
    reader.read_exact(&mut magic)?;
    bytes_consumed += 13;
    if &magic != MAGIC_BYTES {
        return Err(PersistenceError::InvalidMagic {
            expected: MAGIC_BYTES.to_vec(),
            found: magic.to_vec(),
        });
    }

    // Read header length
    let header_len = read_u32_le(&mut reader)? as usize;
    bytes_consumed += 4;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version
    if header.version != VERSION {
        return Err(PersistenceError::IncompatibleVersion {
            expected: VERSION,
            found: header.version,
        });
    }

    Ok(header)
}

/// Checks if a graph's config has drifted from the current config.
///
/// # Errors
///
/// Returns an error if the graph header cannot be read or if provenance is missing.
pub fn check_config_drift(
    graph_path: impl AsRef<Path>,
    current_checksum: &str,
) -> Result<bool, PersistenceError> {
    let header = load_header_from_path(graph_path)?;

    match header.config_provenance {
        Some(provenance) => Ok(provenance.config_matches(current_checksum)),
        None => Err(PersistenceError::ValidationFailed(
            "Graph has no config provenance".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{OverrideEntry, OverrideSource};
    use super::*;
    use tempfile::NamedTempFile;

    // Test helper to create an empty plugin manager
    fn create_test_plugin_manager() -> PluginManager {
        PluginManager::new()
    }

    #[test]
    fn test_save_load_empty_graph() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Save
        save_to_path(&graph, path).unwrap();

        // Validate
        assert!(validate_snapshot(path).unwrap());

        // Load
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        let snapshot = loaded.snapshot();

        assert_eq!(snapshot.nodes().len(), 0);
        assert_eq!(snapshot.strings().len(), 0);
        assert_eq!(snapshot.files().len(), 0);
    }

    #[test]
    fn test_save_load_with_provenance() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance
        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "abc123checksum".to_string(),
            1,
        );

        // Save with provenance
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load header and check provenance
        let header = load_header_from_path(path).unwrap();
        assert!(header.has_provenance());

        let loaded_provenance = header.provenance().unwrap();
        assert_eq!(loaded_provenance.config_checksum, "abc123checksum");
        assert_eq!(loaded_provenance.schema_version, 1);
    }

    #[test]
    fn test_config_drift_detection() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance with known checksum
        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "original_checksum".to_string(),
            1,
        );

        // Save with provenance
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Check drift - same checksum should match
        assert!(check_config_drift(path, "original_checksum").unwrap());

        // Check drift - different checksum should not match
        assert!(!check_config_drift(path, "different_checksum").unwrap());
    }

    #[test]
    fn test_config_drift_no_provenance() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Save without provenance
        save_to_path(&graph, path).unwrap();

        // Check drift should fail - no provenance
        let result = check_config_drift(path, "any_checksum");
        assert!(result.is_err());
    }

    #[test]
    fn test_provenance_with_overrides() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance with overrides
        let mut provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "checksum".to_string(), 1);
        provenance.add_override(OverrideEntry {
            source: OverrideSource::Cli,
            key: "parallelism.max_workers".to_string(),
            value: "16".to_string(),
            original_value: Some("8".to_string()),
        });

        // Save
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load and verify overrides
        let header = load_header_from_path(path).unwrap();
        let loaded_provenance = header.provenance().unwrap();

        assert!(loaded_provenance.has_overrides());
        assert_eq!(loaded_provenance.override_count(), 1);
    }

    #[test]
    fn test_load_rejects_invalid_magic() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write garbage magic bytes
        let mut file = File::create(path).unwrap();
        file.write_all(b"NOT_SQRY_MAGIC").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::InvalidMagic { .. } => {}
            other => panic!("Expected InvalidMagic, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_v3_snapshot() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write V3 magic bytes (old format)
        let mut file = File::create(path).unwrap();
        file.write_all(b"SQRY_GRAPH_V3").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::InvalidMagic { .. } => {}
            other => panic!("Expected InvalidMagic for V3 snapshot, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_corrupted_header_counts() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write a valid V4 file with corrupted header counts
        let corrupt_header = GraphHeader::new(
            100_000_001, // Corrupted node_count (just over the limit)
            0,
            0,
            0,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        // Write dummy data length and no data
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());

        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable node_count"));
                assert!(msg.contains("corrupted"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_header_length_exceeding_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write magic + header_len that exceeds remaining file bytes
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&999_999u32.to_le_bytes()).unwrap(); // header_len way too big
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("header length exceeds remaining file bytes"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_data_length_exceeding_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write valid magic + valid header + data_len exceeding file
        let header = GraphHeader::new(0, 0, 0, 0);
        let header_bytes = postcard::to_allocvec(&header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&999_999u64.to_le_bytes()).unwrap(); // data_len way too big
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("data length exceeds remaining file bytes"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_trailing_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Save a valid graph
        let graph = CodeGraph::new();
        save_to_path(&graph, path).unwrap();

        // Append trailing bytes
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"junk").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("trailing bytes"));
            }
            other => panic!("Expected ValidationFailed for trailing bytes, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_edge_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100,
            1_000_001_000, // Edge count exceeds limit
            10,
            1,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable edge_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_string_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100, 1000, 50_001_000, // String count exceeds limit
            1,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable string_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_file_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100, 1000, 1000, 1_001_000, // File count exceeds limit
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable file_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_plugin_version_tracking() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "test_checksum".to_string(),
            1,
        );

        // Save with plugin versions
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load header and verify plugin versions are empty (no plugins registered)
        let header = load_header_from_path(path).unwrap();
        assert_eq!(header.plugin_versions().len(), 0);

        // Load should succeed with matching plugin manager
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        assert_eq!(loaded.snapshot().nodes().len(), 0);
    }

    #[test]
    fn test_load_rejects_header_exceeding_max_header_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Write magic, then a header_len that exceeds MAX_HEADER_BYTES (1 MB)
        let declared_header_len: u32 = (MAX_HEADER_BYTES as u32) + 1;
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&declared_header_len.to_le_bytes()).unwrap();
        // Write enough padding so the file is large enough that the
        // "exceeds remaining bytes" check doesn't trigger first
        let padding = vec![0u8; declared_header_len as usize + 16];
        file.write_all(&padding).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, None);
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(
                    msg.contains("header too large"),
                    "Expected 'header too large', got: {msg}"
                );
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_data_exceeding_max_snapshot_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Build a valid header so we get past header validation
        let header = GraphHeader::new(0, 0, 0, 0);
        let header_bytes = postcard::to_allocvec(&header).unwrap();

        // Write the framed format with a data_len exceeding MAX_SNAPSHOT_BYTES (2 GB)
        let declared_data_len: u64 = MAX_SNAPSHOT_BYTES + 1;
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&declared_data_len.to_le_bytes()).unwrap();
        // We don't need actual data — the check happens before reading the data section
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(
                    msg.contains("data section too large"),
                    "Expected 'data section too large', got: {msg}"
                );
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }
}
