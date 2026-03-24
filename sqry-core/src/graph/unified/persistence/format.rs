//! Binary format definition for graph persistence.
//!
//! This module defines the on-disk format for persisted graphs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::manifest::ConfigProvenance;

/// Magic bytes identifying a sqry graph file: "`SQRY_GRAPH_V5`"
///
/// Version history:
/// - V1: Initial format (bincode)
/// - V2: Added config provenance support (bincode)
/// - V3: Added plugin version tracking (bincode)
/// - V4: Migrated to postcard serialization with length-prefixed framing
/// - V5: Added `HttpMethod::All` variant for wildcard endpoint matching
pub const MAGIC_BYTES: &[u8; 13] = b"SQRY_GRAPH_V5";

/// Current format version
pub const VERSION: u32 = 5;

/// Header for persisted graph files.
///
/// The header provides metadata about the graph for validation
/// and efficient loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphHeader {
    /// Format version (for compatibility checking)
    pub version: u32,

    /// Number of nodes in the graph
    pub node_count: usize,

    /// Number of edges in the graph
    pub edge_count: usize,

    /// Number of interned strings
    pub string_count: usize,

    /// Number of registered files
    pub file_count: usize,

    /// Timestamp when graph was saved (unix epoch seconds)
    pub timestamp: u64,

    /// Configuration provenance - records which config was used to build this graph.
    #[serde(default)]
    pub config_provenance: Option<ConfigProvenance>,

    /// Plugin versions used to build this graph (`plugin_id` → version).
    ///
    /// Tracks which language plugin versions were active during indexing.
    /// Used to detect stale indexes when plugin versions change.
    #[serde(default)]
    pub plugin_versions: HashMap<String, String>,
}

impl GraphHeader {
    /// Creates a new graph header with the given counts.
    #[must_use]
    pub fn new(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: None,
            plugin_versions: HashMap::new(),
        }
    }

    /// Creates a new graph header with config provenance.
    #[must_use]
    pub fn with_provenance(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
        provenance: ConfigProvenance,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: Some(provenance),
            plugin_versions: HashMap::new(),
        }
    }

    /// Creates a new graph header with config provenance and plugin versions.
    #[must_use]
    pub fn with_provenance_and_plugins(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
        provenance: ConfigProvenance,
        plugin_versions: HashMap<String, String>,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: Some(provenance),
            plugin_versions,
        }
    }

    /// Returns the config provenance if available.
    #[must_use]
    pub fn provenance(&self) -> Option<&ConfigProvenance> {
        self.config_provenance.as_ref()
    }

    /// Checks if the graph was built with tracked config provenance.
    #[must_use]
    pub fn has_provenance(&self) -> bool {
        self.config_provenance.is_some()
    }

    /// Returns the plugin versions used to build this graph.
    #[must_use]
    pub fn plugin_versions(&self) -> &HashMap<String, String> {
        &self.plugin_versions
    }

    /// Sets the plugin versions for this graph header.
    pub fn set_plugin_versions(&mut self, versions: HashMap<String, String>) {
        self.plugin_versions = versions;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_test_provenance() -> ConfigProvenance {
        ConfigProvenance {
            config_file: PathBuf::from(".sqry/graph/config/config.json"),
            config_checksum: "abc123def456".to_string(),
            schema_version: 1,
            overrides: HashMap::new(),
            build_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            build_host: Some("test-host".to_string()),
        }
    }

    #[test]
    fn test_magic_bytes() {
        assert_eq!(MAGIC_BYTES, b"SQRY_GRAPH_V5");
        assert_eq!(MAGIC_BYTES.len(), 13);
    }

    #[test]
    fn test_version() {
        assert_eq!(VERSION, 5);
    }

    #[test]
    fn test_graph_header_new() {
        let header = GraphHeader::new(100, 50, 200, 10);

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert_eq!(header.edge_count, 50);
        assert_eq!(header.string_count, 200);
        assert_eq!(header.file_count, 10);
        assert!(header.timestamp > 0);
        assert!(header.config_provenance.is_none());
    }

    #[test]
    fn test_graph_header_with_provenance() {
        let provenance = make_test_provenance();
        let header = GraphHeader::with_provenance(100, 50, 200, 10, provenance);

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert_eq!(header.edge_count, 50);
        assert!(header.config_provenance.is_some());
        assert_eq!(
            header.config_provenance.as_ref().unwrap().config_checksum,
            "abc123def456"
        );
    }

    #[test]
    fn test_graph_header_provenance_method() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(header.provenance().is_none());

        let provenance = make_test_provenance();
        let header_with = GraphHeader::with_provenance(10, 5, 20, 2, provenance);
        assert!(header_with.provenance().is_some());
        assert_eq!(
            header_with.provenance().unwrap().config_checksum,
            "abc123def456"
        );
    }

    #[test]
    fn test_graph_header_has_provenance() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(!header.has_provenance());

        let provenance = make_test_provenance();
        let header_with = GraphHeader::with_provenance(10, 5, 20, 2, provenance);
        assert!(header_with.has_provenance());
    }

    #[test]
    fn test_graph_header_clone() {
        let header = GraphHeader::new(100, 50, 200, 10);
        let cloned = header.clone();

        assert_eq!(header.version, cloned.version);
        assert_eq!(header.node_count, cloned.node_count);
        assert_eq!(header.edge_count, cloned.edge_count);
        assert_eq!(header.string_count, cloned.string_count);
        assert_eq!(header.file_count, cloned.file_count);
    }

    #[test]
    fn test_graph_header_debug() {
        let header = GraphHeader::new(100, 50, 200, 10);
        let debug_str = format!("{:?}", header);

        assert!(debug_str.contains("GraphHeader"));
        assert!(debug_str.contains("version"));
        assert!(debug_str.contains("node_count"));
    }

    #[test]
    fn test_graph_header_timestamp_is_recent() {
        let header = GraphHeader::new(10, 5, 20, 2);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Timestamp should be within 1 second of now
        assert!(header.timestamp <= now);
        assert!(header.timestamp >= now - 1);
    }

    #[test]
    fn test_graph_header_zero_counts() {
        let header = GraphHeader::new(0, 0, 0, 0);

        assert_eq!(header.node_count, 0);
        assert_eq!(header.edge_count, 0);
        assert_eq!(header.string_count, 0);
        assert_eq!(header.file_count, 0);
    }

    #[test]
    fn test_graph_header_large_counts() {
        let header = GraphHeader::new(1_000_000, 5_000_000, 10_000_000, 100_000);

        assert_eq!(header.node_count, 1_000_000);
        assert_eq!(header.edge_count, 5_000_000);
        assert_eq!(header.string_count, 10_000_000);
        assert_eq!(header.file_count, 100_000);
    }

    #[test]
    fn test_graph_header_plugin_versions_empty_by_default() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(header.plugin_versions().is_empty());
    }

    #[test]
    fn test_graph_header_set_plugin_versions() {
        let mut header = GraphHeader::new(10, 5, 20, 2);

        let mut versions = HashMap::new();
        versions.insert("rust".to_string(), "3.3.0".to_string());
        versions.insert("javascript".to_string(), "3.3.0".to_string());

        header.set_plugin_versions(versions.clone());

        assert_eq!(header.plugin_versions().len(), 2);
        assert_eq!(
            header.plugin_versions().get("rust"),
            Some(&"3.3.0".to_string())
        );
        assert_eq!(
            header.plugin_versions().get("javascript"),
            Some(&"3.3.0".to_string())
        );
    }

    #[test]
    fn test_graph_header_with_provenance_and_plugins() {
        let provenance = make_test_provenance();

        let mut plugin_versions = HashMap::new();
        plugin_versions.insert("rust".to_string(), "3.3.0".to_string());
        plugin_versions.insert("python".to_string(), "3.3.0".to_string());

        let header = GraphHeader::with_provenance_and_plugins(
            100,
            50,
            200,
            10,
            provenance,
            plugin_versions.clone(),
        );

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert!(header.config_provenance.is_some());
        assert_eq!(header.plugin_versions().len(), 2);
        assert_eq!(
            header.plugin_versions().get("rust"),
            Some(&"3.3.0".to_string())
        );
    }
}
