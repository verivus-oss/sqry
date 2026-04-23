//! Manifest and provenance tracking for graph persistence.
//!
//! This module provides structures for tracking the configuration used
//! when building a graph, enabling reproducibility and drift detection.
//!
//! # Manifest Structure
//!
//! The graph manifest (`manifest.json`) contains metadata about the built graph:
//! - Schema and format versions for compatibility checking
//! - Build timestamps and provenance information
//! - Node/edge counts for quick validation
//! - SHA256 checksum of the snapshot file
//!
//! # Version Constants
//!
//! - `MANIFEST_SCHEMA_VERSION`: Version of the manifest JSON schema
//! - `SNAPSHOT_FORMAT_VERSION`: Version of the binary snapshot format

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::confidence::ConfidenceMetadata;

/// Current version of the manifest JSON schema.
///
/// Increment this when the manifest structure changes in a backwards-incompatible way.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Current version of the snapshot binary format.
///
/// This should match the `VERSION` constant in `format.rs`.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 2;

/// Provenance information for the configuration used during graph build.
///
/// Records which config file was used, its integrity checksum, and any
/// ephemeral overrides (CLI flags, environment variables) that were applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigProvenance {
    /// Path to the config file used (relative to project root).
    ///
    /// Standard path: `.sqry/graph/config/config.json`
    pub config_file: PathBuf,

    /// blake3 checksum of the config file at build time.
    ///
    /// Used to detect if the config has changed since the graph was built.
    pub config_checksum: String,

    /// Schema version of the config file.
    pub schema_version: u32,

    /// Ephemeral overrides applied during this build.
    ///
    /// Maps override source (e.g., "cli:--parallel-jobs", "`env:SQRY_CACHE_SIZE`")
    /// to the value used.
    pub overrides: HashMap<String, OverrideEntry>,

    /// Timestamp when build started (unix epoch seconds).
    pub build_timestamp: u64,

    /// Host information for debugging multi-machine issues.
    pub build_host: Option<String>,
}

/// An ephemeral override applied during graph build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverrideEntry {
    /// Source of the override (e.g., "cli", "env").
    pub source: OverrideSource,

    /// The config key that was overridden (e.g., "`parallelism.max_workers`").
    pub key: String,

    /// The value used (as string representation).
    pub value: String,

    /// The default/persisted value that was overridden.
    pub original_value: Option<String>,
}

/// Source of a configuration override.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OverrideSource {
    /// Override from CLI flag (e.g., `--parallel-jobs 8`).
    Cli,
    /// Override from environment variable (e.g., `SQRY_PARALLEL_JOBS=8`).
    Env,
    /// Override from programmatic API.
    Api,
}

impl ConfigProvenance {
    /// Creates a new provenance record for a graph build.
    ///
    /// # Arguments
    ///
    /// * `config_file` - Path to the config file used
    /// * `config_checksum` - blake3 checksum of the config file
    /// * `schema_version` - Schema version of the config
    #[must_use]
    pub fn new(
        config_file: impl Into<PathBuf>,
        config_checksum: String,
        schema_version: u32,
    ) -> Self {
        Self {
            config_file: config_file.into(),
            config_checksum,
            schema_version,
            overrides: HashMap::new(),
            build_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            build_host: hostname::get().ok().and_then(|h| h.into_string().ok()),
        }
    }

    /// Adds an override entry to the provenance record.
    pub fn add_override(&mut self, entry: OverrideEntry) {
        let key = format!("{}:{}", entry.source.as_str(), entry.key);
        self.overrides.insert(key, entry);
    }

    /// Checks if the config file matches the recorded checksum.
    ///
    /// # Arguments
    ///
    /// * `current_checksum` - The current checksum of the config file
    ///
    /// # Returns
    ///
    /// `true` if checksums match, `false` if config has changed.
    #[must_use]
    pub fn config_matches(&self, current_checksum: &str) -> bool {
        self.config_checksum == current_checksum
    }

    /// Returns the number of overrides applied.
    #[must_use]
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// Checks if any overrides were applied during the build.
    #[must_use]
    pub fn has_overrides(&self) -> bool {
        !self.overrides.is_empty()
    }
}

impl OverrideSource {
    /// Returns the string representation of the override source.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Env => "env",
            Self::Api => "api",
        }
    }
}

impl std::fmt::Display for OverrideSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Builder for constructing `ConfigProvenance` with overrides.
#[derive(Debug)]
pub struct ConfigProvenanceBuilder {
    config_file: PathBuf,
    config_checksum: String,
    schema_version: u32,
    overrides: Vec<OverrideEntry>,
}

impl ConfigProvenanceBuilder {
    /// Creates a new builder.
    #[must_use]
    pub fn new(
        config_file: impl Into<PathBuf>,
        config_checksum: String,
        schema_version: u32,
    ) -> Self {
        Self {
            config_file: config_file.into(),
            config_checksum,
            schema_version,
            overrides: Vec::new(),
        }
    }

    /// Adds a CLI override.
    #[must_use]
    pub fn with_cli_override(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        original: Option<String>,
    ) -> Self {
        self.overrides.push(OverrideEntry {
            source: OverrideSource::Cli,
            key: key.into(),
            value: value.into(),
            original_value: original,
        });
        self
    }

    /// Adds an environment variable override.
    #[must_use]
    pub fn with_env_override(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        original: Option<String>,
    ) -> Self {
        self.overrides.push(OverrideEntry {
            source: OverrideSource::Env,
            key: key.into(),
            value: value.into(),
            original_value: original,
        });
        self
    }

    /// Builds the `ConfigProvenance` instance.
    #[must_use]
    pub fn build(self) -> ConfigProvenance {
        let mut provenance =
            ConfigProvenance::new(self.config_file, self.config_checksum, self.schema_version);
        for entry in self.overrides {
            provenance.add_override(entry);
        }
        provenance
    }
}

/// Creates a default provenance for when no config file exists.
///
/// This is used during initial graph builds before config is initialized.
#[must_use]
pub fn default_provenance() -> ConfigProvenance {
    ConfigProvenance {
        config_file: PathBuf::from(".sqry/graph/config/config.json"),
        config_checksum: String::from("none"),
        schema_version: 0,
        overrides: HashMap::new(),
        build_timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        build_host: hostname::get().ok().and_then(|h| h.into_string().ok()),
    }
}

/// Computes the blake3 checksum of a config file.
///
/// # Arguments
///
/// * `path` - Path to the config file
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn compute_config_checksum(path: impl AsRef<Path>) -> std::io::Result<String> {
    let content = std::fs::read(path)?;
    let hash = blake3::hash(&content);
    Ok(hash.to_hex().to_string())
}

// ============================================================================
// Build Provenance (for CLI/index command)
// ============================================================================

/// Build provenance information recorded in the graph manifest.
///
/// This captures metadata about the build environment and command used
/// to create the graph, enabling reproducibility and debugging.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildProvenance {
    /// Version of sqry that built this graph (e.g., "0.15.0").
    pub sqry_version: String,

    /// RFC3339 timestamp when the build started.
    pub build_timestamp: String,

    /// Command used to build the graph (e.g., "sqry index", "sqry build").
    pub build_command: String,

    /// SHA256 hashes of language plugins used during indexing.
    ///
    /// Maps plugin name to its content hash for reproducibility verification.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub plugin_hashes: HashMap<String, String>,
}

/// Persisted plugin selection used to build a graph snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginSelectionManifest {
    /// Deterministic ordered list of active built-in plugin ids.
    ///
    /// This is the authoritative field for reconstructing the plugin manager.
    pub active_plugin_ids: Vec<String>,

    /// High-cost selection mode used when the active ids were resolved.
    ///
    /// This is diagnostic provenance only; readers must not treat it as the
    /// source of truth for plugin reconstruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_cost_mode: Option<String>,
}

impl BuildProvenance {
    /// Creates a new build provenance record.
    #[must_use]
    pub fn new(sqry_version: impl Into<String>, build_command: impl Into<String>) -> Self {
        Self {
            sqry_version: sqry_version.into(),
            build_timestamp: chrono::Utc::now().to_rfc3339(),
            build_command: build_command.into(),
            plugin_hashes: HashMap::new(),
        }
    }

    /// Adds a plugin hash to the provenance record.
    pub fn add_plugin_hash(&mut self, plugin_name: impl Into<String>, hash: impl Into<String>) {
        self.plugin_hashes.insert(plugin_name.into(), hash.into());
    }
}

// ============================================================================
// Graph Manifest (JSON metadata file)
// ============================================================================

/// Graph manifest containing metadata about a built graph snapshot.
///
/// This is serialized as `manifest.json` in the `.sqry/graph/` directory and
/// provides quick access to graph statistics without loading the full snapshot.
///
/// # Example
///
/// ```json
/// {
///   "schema_version": 1,
///   "snapshot_format_version": 2,
///   "built_at": "2024-12-15T10:30:00Z",
///   "root_path": "/path/to/project",
///   "node_count": 5000,
///   "edge_count": 12000,
///   "snapshot_sha256": "abc123...",
///   "build_provenance": { ... }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version of this manifest (for format compatibility).
    pub schema_version: u32,

    /// Version of the snapshot binary format.
    pub snapshot_format_version: u32,

    /// RFC3339 timestamp when the graph was built.
    pub built_at: String,

    /// Absolute path to the project root that was indexed.
    pub root_path: String,

    /// Total number of nodes in the graph.
    pub node_count: usize,

    /// Total number of deduplicated edges in the graph (from analysis CSR).
    ///
    /// This is the canonical edge count. After consolidation, this reflects edges
    /// after merge/compaction, not the raw CSR + delta buffer count.
    pub edge_count: usize,

    /// Raw edge count before deduplication (CSR + delta buffer).
    ///
    /// Available for diagnostics. `None` for manifests built before the
    /// build pipeline consolidation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_edge_count: Option<usize>,

    /// SHA256 checksum of the snapshot file for integrity verification.
    pub snapshot_sha256: String,

    /// Build provenance information.
    pub build_provenance: BuildProvenance,

    /// File count per language (e.g., {"rust": 150, "python": 30}).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub file_count: HashMap<String, usize>,

    /// Languages detected in the indexed codebase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,

    /// Configuration values used during build.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, String>,

    /// Per-language confidence metadata for analysis quality tracking.
    /// Maps language name (e.g., "rust") to confidence metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub confidence: HashMap<String, ConfidenceMetadata>,

    /// Git commit SHA that was indexed.
    ///
    /// When present, enables git-aware incremental updates by tracking
    /// which commit the graph was built from. If the repository has no
    /// commits or is not a git repository, this will be `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed_commit: Option<String>,

    /// Plugin selection that built the persisted snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_selection: Option<PluginSelectionManifest>,
}

impl Manifest {
    /// Creates a new manifest with the given metadata.
    #[must_use]
    pub fn new(
        root_path: impl Into<String>,
        node_count: usize,
        edge_count: usize,
        snapshot_sha256: impl Into<String>,
        build_provenance: BuildProvenance,
    ) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
            built_at: chrono::Utc::now().to_rfc3339(),
            root_path: root_path.into(),
            node_count,
            edge_count,
            raw_edge_count: None,
            snapshot_sha256: snapshot_sha256.into(),
            build_provenance,
            file_count: HashMap::new(),
            languages: Vec::new(),
            config: HashMap::new(),
            confidence: HashMap::new(),
            last_indexed_commit: None,
            plugin_selection: None,
        }
    }

    /// Sets the last indexed git commit SHA.
    #[must_use]
    pub fn with_last_indexed_commit(mut self, commit: Option<String>) -> Self {
        self.last_indexed_commit = commit;
        self
    }

    /// Saves the manifest to a JSON file using atomic write.
    ///
    /// Uses platform-specific atomic file operations to ensure the manifest
    /// is written atomically (no partial writes visible to readers). This is
    /// critical for cache freshness detection based on `file_id` (`inode/file_index`).
    ///
    /// # Arguments
    ///
    /// * `path` - Path where the manifest should be saved
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created or written.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        write_manifest_atomic(path.as_ref(), self)
    }

    /// Loads a manifest from a JSON file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the manifest file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Sets the file count per language.
    #[must_use]
    pub fn with_file_count(mut self, file_count: HashMap<String, usize>) -> Self {
        self.file_count = file_count;
        self
    }

    /// Sets the detected languages.
    #[must_use]
    pub fn with_languages(mut self, languages: Vec<String>) -> Self {
        self.languages = languages;
        self
    }

    /// Sets the configuration values.
    #[must_use]
    pub fn with_config(mut self, config: HashMap<String, String>) -> Self {
        self.config = config;
        self
    }

    /// Sets the confidence metadata per language.
    #[must_use]
    pub fn with_confidence(mut self, confidence: HashMap<String, ConfidenceMetadata>) -> Self {
        self.confidence = confidence;
        self
    }

    /// Sets the persisted plugin selection metadata.
    #[must_use]
    pub fn with_plugin_selection(
        mut self,
        plugin_selection: Option<PluginSelectionManifest>,
    ) -> Self {
        self.plugin_selection = plugin_selection;
        self
    }
}

// ============================================================================
// ManifestCheck — first-class missing/corrupt result type
// ============================================================================

/// Result of attempting to load a manifest from disk.
///
/// Distinguishes three outcomes so callers can apply policy explicitly:
///
/// - [`ManifestCheck::Present`] — manifest loaded and parsed successfully.
/// - [`ManifestCheck::Missing`] — the manifest file does not exist (e.g.
///   during a rebuild window). Callers should treat this as stale or trigger
///   a rebuild, depending on context. **No snapshot should be served without
///   a valid manifest** — the SHA-256 integrity contract requires the manifest
///   to verify the snapshot.
/// - [`ManifestCheck::Corrupt`] — the manifest file exists but cannot be read
///   or parsed. The enclosed [`std::io::Error`] carries the underlying I/O or
///   deserialization error.
///
/// # Policy guidance
///
/// | Caller context | `Missing` policy | `Corrupt` policy |
/// |---------------|-----------------|-----------------|
/// | Daemon serve path | Return `Stale` (in-memory graph keeps serving) | Return `Stale` |
/// | Standalone cold start | Trigger rebuild | Trigger rebuild |
/// | CLI freshness check | Report stale, skip analysis | Report stale, skip analysis |
// `Manifest` is inherently large — boxing it would add indirection to every
// `Present(m)` read path without a meaningful memory benefit (the enum is
// short-lived: callers immediately pattern-match and move the inner value).
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ManifestCheck {
    /// Manifest loaded successfully; contains the parsed [`Manifest`].
    Present(Manifest),
    /// Manifest file does not exist (e.g. during rebuild window).
    Missing,
    /// Manifest file exists but cannot be read or parsed.
    Corrupt(std::io::Error),
}

impl ManifestCheck {
    /// Returns `true` if the manifest is present and valid.
    #[must_use]
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    /// Returns `true` if the manifest is missing.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// Returns `true` if the manifest is present but corrupt.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }

    /// Converts to `Option<Manifest>`, discarding error information.
    ///
    /// Returns `None` for both `Missing` and `Corrupt` variants.
    #[must_use]
    pub fn into_manifest(self) -> Option<Manifest> {
        match self {
            Self::Present(m) => Some(m),
            Self::Missing | Self::Corrupt(_) => None,
        }
    }
}

/// Load a manifest from disk, mapping `ENOENT` to [`ManifestCheck::Missing`] and
/// all other errors to [`ManifestCheck::Corrupt`].
///
/// This is the non-panicking variant of [`Manifest::load`]. Callers that
/// previously propagated `std::io::Error` on a missing manifest should use
/// this function instead and pattern-match the result.
///
/// # Examples
///
/// ```ignore
/// match try_load_manifest(path) {
///     ManifestCheck::Present(m) => { /* use manifest */ }
///     ManifestCheck::Missing    => { /* trigger rebuild or report stale */ }
///     ManifestCheck::Corrupt(e) => { /* log and trigger rebuild */ }
/// }
/// ```
#[must_use]
pub fn try_load_manifest(path: &Path) -> ManifestCheck {
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<Manifest>(&content) {
            Ok(manifest) => ManifestCheck::Present(manifest),
            Err(e) => {
                ManifestCheck::Corrupt(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ManifestCheck::Missing,
        Err(e) => ManifestCheck::Corrupt(e),
    }
}

// ============================================================================
// Atomic Manifest Writes
// ============================================================================

/// Write a manifest atomically to ensure `file_id` (`inode/file_index`) changes.
///
/// This function uses platform-specific atomic file operations to write the
/// manifest. Atomic writes are critical for cache freshness detection because
/// they ensure:
/// 1. No partial writes are visible to concurrent readers
/// 2. The `file_id` (inode on Unix, `file_index` on Windows) changes atomically
/// 3. mtime and size are updated together with content
///
/// # Platform Implementation
///
/// - **Unix**: `NamedTempFile::persist()` uses `rename(2)` which atomically
///   replaces the target file and changes its inode.
/// - **Windows**: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` atomically
///   replaces the target file and changes its `file_index`. The temporary file
///   handle must be closed before the move operation.
///
/// # Arguments
///
/// * `path` - Path where the manifest should be saved
/// * `manifest` - The manifest to write
///
/// # Errors
///
/// Returns an error if:
/// - Parent directory doesn't exist
/// - Temporary file creation fails
/// - JSON serialization fails
/// - Atomic rename/move fails
fn write_manifest_atomic(path: &Path, manifest: &Manifest) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Path has no parent")
    })?;

    // Create temporary file in same directory (required for atomic rename)
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;

    // Write JSON content
    serde_json::to_writer_pretty(&mut temp, manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Flush and fsync before rename (ensure content is on disk)
    temp.as_file().sync_all()?;

    // Platform-specific atomic rename/move
    #[cfg(unix)]
    {
        // Unix: persist() uses rename(2) which is atomic
        temp.persist(path)?;
    }

    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        // Convert NamedTempFile to TempPath (closes the file handle)
        // This is CRITICAL: Windows requires the handle to be closed before MoveFileExW
        let temp_path = temp.into_temp_path();
        // temp is now consumed, file handle is closed

        // Encode paths as wide strings (Windows UTF-16)
        let source: Vec<u16> = OsStr::new(&temp_path)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let dest: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();

        // SAFETY: Both source and dest are valid null-terminated wide strings
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                dest.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };

        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }

        // After successful MoveFileExW, the source file has been moved to dest
        // Call close() to release TempPath without attempting deletion
        // (the file no longer exists at the original temp location)
        drop(temp_path.close());
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for unsupported platforms (not atomic, but functional)
        // This branch should never be reached in production (Rust targets are Unix or Windows)
        compile_error!("Atomic manifest writes require Unix or Windows platform");
    }

    Ok(())
}

/// Atomically write pre-serialized manifest bytes to disk.
///
/// This writes the provided bytes to a temporary file in the same directory,
/// fsyncs, and then atomically renames. Used when the manifest has already been
/// serialized in memory (e.g., for hash computation before persistence).
///
/// # Errors
///
/// Returns an error if the parent directory doesn't exist, the temp file
/// cannot be created, or the atomic rename fails.
pub(crate) fn write_manifest_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Path has no parent")
    })?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;

    #[cfg(unix)]
    {
        temp.persist(path)?;
    }

    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let temp_path = temp.into_temp_path();
        let source: Vec<u16> = OsStr::new(&temp_path)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let dest: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();

        // SAFETY: Both source and dest are valid null-terminated wide strings
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                dest.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };

        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }

        drop(temp_path.close());
    }

    #[cfg(not(any(unix, windows)))]
    {
        compile_error!("Atomic manifest writes require Unix or Windows platform");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_config_provenance_new() {
        let provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "abc123".to_string(), 1);

        assert_eq!(
            provenance.config_file,
            PathBuf::from(".sqry/graph/config/config.json")
        );
        assert_eq!(provenance.config_checksum, "abc123");
        assert_eq!(provenance.schema_version, 1);
        assert!(!provenance.has_overrides());
        assert!(provenance.build_timestamp > 0);
    }

    #[test]
    fn test_config_provenance_with_overrides() {
        let mut provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "abc123".to_string(), 1);

        provenance.add_override(OverrideEntry {
            source: OverrideSource::Cli,
            key: "parallelism.max_workers".to_string(),
            value: "8".to_string(),
            original_value: Some("4".to_string()),
        });

        provenance.add_override(OverrideEntry {
            source: OverrideSource::Env,
            key: "cache.max_size_mb".to_string(),
            value: "512".to_string(),
            original_value: None,
        });

        assert!(provenance.has_overrides());
        assert_eq!(provenance.override_count(), 2);
        assert!(
            provenance
                .overrides
                .contains_key("cli:parallelism.max_workers")
        );
        assert!(provenance.overrides.contains_key("env:cache.max_size_mb"));
    }

    #[test]
    fn test_config_matches() {
        let provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "abc123".to_string(), 1);

        assert!(provenance.config_matches("abc123"));
        assert!(!provenance.config_matches("def456"));
    }

    #[test]
    fn test_provenance_builder() {
        let provenance = ConfigProvenanceBuilder::new(
            ".sqry/graph/config/config.json",
            "checksum123".to_string(),
            2,
        )
        .with_cli_override(
            "limits.max_file_size",
            "10485760",
            Some("5242880".to_string()),
        )
        .with_env_override("output.format", "json", None)
        .build();

        assert_eq!(provenance.schema_version, 2);
        assert_eq!(provenance.override_count(), 2);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "abc123".to_string(), 1);
        provenance.add_override(OverrideEntry {
            source: OverrideSource::Cli,
            key: "test.key".to_string(),
            value: "test_value".to_string(),
            original_value: Some("original".to_string()),
        });

        // Serialize to JSON
        let json = serde_json::to_string(&provenance).unwrap();

        // Deserialize back
        let deserialized: ConfigProvenance = serde_json::from_str(&json).unwrap();

        assert_eq!(provenance.config_file, deserialized.config_file);
        assert_eq!(provenance.config_checksum, deserialized.config_checksum);
        assert_eq!(provenance.schema_version, deserialized.schema_version);
        assert_eq!(provenance.overrides, deserialized.overrides);
    }

    #[test]
    fn test_compute_config_checksum() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "{{\"test\": \"config\"}}").unwrap();

        let checksum = compute_config_checksum(temp_file.path()).unwrap();

        // blake3 produces 64 hex characters
        assert_eq!(checksum.len(), 64);

        // Same content should produce same checksum
        let checksum2 = compute_config_checksum(temp_file.path()).unwrap();
        assert_eq!(checksum, checksum2);
    }

    #[test]
    fn test_default_provenance() {
        let provenance = default_provenance();

        assert_eq!(
            provenance.config_file,
            PathBuf::from(".sqry/graph/config/config.json")
        );
        assert_eq!(provenance.config_checksum, "none");
        assert_eq!(provenance.schema_version, 0);
        assert!(!provenance.has_overrides());
    }

    #[test]
    fn test_override_source_display() {
        assert_eq!(OverrideSource::Cli.to_string(), "cli");
        assert_eq!(OverrideSource::Env.to_string(), "env");
        assert_eq!(OverrideSource::Api.to_string(), "api");
    }

    #[test]
    fn test_manifest_with_confidence() {
        use crate::confidence::{ConfidenceLevel, ConfidenceMetadata};
        use std::collections::HashMap;

        let build_prov = BuildProvenance::new("2.8.0", "sqry index");

        let mut confidence_map = HashMap::new();
        confidence_map.insert(
            "rust".to_string(),
            ConfidenceMetadata {
                level: ConfidenceLevel::AstOnly,
                limitations: vec!["No rust-analyzer available".to_string()],
                unavailable_features: vec!["Type inference".to_string()],
            },
        );

        let manifest = Manifest::new("/test/path", 100, 200, "abc123", build_prov)
            .with_confidence(confidence_map.clone());

        assert_eq!(manifest.confidence.len(), 1);
        assert!(manifest.confidence.contains_key("rust"));

        let rust_confidence = &manifest.confidence["rust"];
        assert_eq!(rust_confidence.level, ConfidenceLevel::AstOnly);
        assert_eq!(rust_confidence.limitations.len(), 1);
        assert_eq!(rust_confidence.unavailable_features.len(), 1);
    }

    #[test]
    fn test_manifest_confidence_serialization() {
        use crate::confidence::{ConfidenceLevel, ConfidenceMetadata};
        use std::collections::HashMap;

        let build_prov = BuildProvenance::new("2.8.0", "sqry index");

        let mut confidence_map = HashMap::new();
        confidence_map.insert(
            "rust".to_string(),
            ConfidenceMetadata {
                level: ConfidenceLevel::Partial,
                limitations: vec![],
                unavailable_features: vec![],
            },
        );

        let manifest = Manifest::new("/test/path", 100, 200, "abc123", build_prov)
            .with_confidence(confidence_map);

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"confidence\""));
        assert!(json.contains("\"rust\""));
        assert!(json.contains("\"partial\""));
    }

    #[test]
    fn test_manifest_empty_confidence_omitted() {
        let build_prov = BuildProvenance::new("2.8.0", "sqry index");
        let manifest = Manifest::new("/test/path", 100, 200, "abc123", build_prov);

        let json = serde_json::to_string(&manifest).unwrap();
        // Empty confidence map should be omitted due to skip_serializing_if
        assert!(!json.contains("\"confidence\""));
    }

    #[test]
    fn test_manifest_plugin_selection_serialization() {
        let build_prov = BuildProvenance::new("2.8.0", "sqry index");
        let plugin_selection = PluginSelectionManifest {
            active_plugin_ids: vec![String::from("rust"), String::from("json")],
            high_cost_mode: Some(String::from("include_all")),
        };
        let manifest = Manifest::new("/test/path", 100, 200, "abc123", build_prov)
            .with_plugin_selection(Some(plugin_selection.clone()));

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"plugin_selection\""));
        assert!(json.contains("\"active_plugin_ids\""));
        assert!(json.contains("\"include_all\""));

        let round_trip: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.plugin_selection, Some(plugin_selection));
    }

    #[test]
    fn test_manifest_plugin_selection_omitted_when_absent() {
        let build_prov = BuildProvenance::new("2.8.0", "sqry index");
        let manifest = Manifest::new("/test/path", 100, 200, "abc123", build_prov);

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(!json.contains("\"plugin_selection\""));
    }

    #[test]
    fn test_legacy_manifest_without_plugin_selection_loads() {
        let legacy_manifest = r#"{
            "schema_version": 1,
            "snapshot_format_version": 2,
            "built_at": "2026-04-04T08:00:00Z",
            "root_path": "/test/path",
            "node_count": 100,
            "edge_count": 200,
            "snapshot_sha256": "abc123",
            "build_provenance": {
                "sqry_version": "1.0.0",
                "build_timestamp": "2026-04-04T08:00:00Z",
                "build_command": "test",
                "plugin_hashes": {}
            }
        }"#;

        let manifest: Manifest = serde_json::from_str(legacy_manifest).unwrap();
        assert!(manifest.plugin_selection.is_none());
        assert!(manifest.last_indexed_commit.is_none());
    }

    /// Regression test (Step 10, #6): New manifests have `raw_edge_count` field.
    #[test]
    fn test_manifest_raw_edge_count_field_present() {
        let build_prov = BuildProvenance::new("3.6.0", "cli:index");
        let mut manifest = Manifest::new("/test/path", 500, 300, "sha256", build_prov);
        manifest.raw_edge_count = Some(450);

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(
            json.contains("\"raw_edge_count\""),
            "New manifests should include raw_edge_count field"
        );
        assert!(
            json.contains("450"),
            "raw_edge_count value should be serialized"
        );

        // Roundtrip
        let loaded: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.raw_edge_count, Some(450));
        assert_eq!(loaded.edge_count, 300);
    }

    /// Regression test (Step 10, #7): Legacy manifests without `raw_edge_count` deserialize correctly.
    #[test]
    fn test_legacy_manifest_without_raw_edge_count() {
        // Simulate a manifest JSON from before consolidation (no raw_edge_count field)
        let json = r#"{
            "schema_version": 1,
            "snapshot_format_version": 2,
            "built_at": "2026-01-15T10:00:00Z",
            "root_path": "/legacy/path",
            "node_count": 1000,
            "edge_count": 2000,
            "snapshot_sha256": "legacy_sha256",
            "build_provenance": {
                "sqry_version": "3.4.0",
                "build_timestamp": "2026-01-15T10:00:00Z",
                "build_command": "sqry index"
            }
        }"#;

        let manifest: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.node_count, 1000);
        assert_eq!(manifest.edge_count, 2000);
        assert_eq!(
            manifest.raw_edge_count, None,
            "Legacy manifests should deserialize with raw_edge_count = None"
        );
    }

    /// Regression test (Step 10, #6b): `raw_edge_count`=None is omitted from serialization.
    #[test]
    fn test_manifest_raw_edge_count_none_omitted() {
        let build_prov = BuildProvenance::new("3.6.0", "cli:index");
        let manifest = Manifest::new("/test/path", 500, 300, "sha256", build_prov);
        // raw_edge_count defaults to None via Manifest::new()

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(
            !json.contains("raw_edge_count"),
            "None raw_edge_count should be omitted (skip_serializing_if)"
        );
    }

    // Atomic write tests
    #[test]
    fn test_atomic_manifest_write_basic() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("manifest.json");

        let build_prov = BuildProvenance::new("3.1.1", "sqry index");
        let manifest = Manifest::new("/test/workspace", 100, 200, "test_sha256", build_prov);

        // Write atomically
        write_manifest_atomic(&manifest_path, &manifest).unwrap();

        // Verify file exists and is readable
        assert!(manifest_path.exists());

        // Verify content is valid JSON and matches original
        let loaded = Manifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.node_count, 100);
        assert_eq!(loaded.edge_count, 200);
        assert_eq!(loaded.snapshot_sha256, "test_sha256");
    }

    #[test]
    fn test_file_id_changes_after_atomic_write() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("manifest.json");

        let build_prov = BuildProvenance::new("3.1.1", "sqry index");
        let manifest1 = Manifest::new("/test/workspace", 100, 200, "sha1", build_prov.clone());

        // Write first version
        write_manifest_atomic(&manifest_path, &manifest1).unwrap();

        // Get file_id of first version
        let metadata1 = std::fs::metadata(&manifest_path).unwrap();
        let file_id1 = extract_file_id(&metadata1);

        // Small delay to ensure mtime differs
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Write second version (different content)
        let manifest2 = Manifest::new("/test/workspace", 150, 250, "sha2", build_prov);
        write_manifest_atomic(&manifest_path, &manifest2).unwrap();

        // Get file_id of second version
        let metadata2 = std::fs::metadata(&manifest_path).unwrap();
        let file_id2 = extract_file_id(&metadata2);

        // On platforms with file_id support, it should change
        // (On Unix, inode changes due to rename; on Windows, file_index changes)
        if file_id1.is_some() && file_id2.is_some() {
            assert_ne!(
                file_id1, file_id2,
                "File ID should change after atomic write"
            );
        }

        // Verify content was actually updated
        let loaded = Manifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.node_count, 150);
        assert_eq!(loaded.snapshot_sha256, "sha2");
    }

    /// Helper function matching the one in sqry-mcp/src/engine.rs
    /// Used to extract platform-specific file identifiers for testing
    #[cfg(unix)]
    #[allow(clippy::unnecessary_wraps)]
    fn extract_file_id(metadata: &std::fs::Metadata) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.ino())
    }

    #[cfg(windows)]
    #[allow(clippy::unnecessary_wraps)]
    fn extract_file_id(_metadata: &std::fs::Metadata) -> Option<u64> {
        // `MetadataExt::file_index()` requires unstable feature `windows_by_handle` (rust#63010).
        None
    }

    #[cfg(not(any(unix, windows)))]
    fn extract_file_id(_metadata: &std::fs::Metadata) -> Option<u64> {
        None
    }

    #[test]
    fn test_atomic_write_replaces_existing() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("manifest.json");

        // Create initial file with different content
        std::fs::write(&manifest_path, "old content").unwrap();

        let build_prov = BuildProvenance::new("3.1.1", "sqry index");
        let manifest = Manifest::new("/test/workspace", 100, 200, "new_sha", build_prov);

        // Atomic write should replace
        write_manifest_atomic(&manifest_path, &manifest).unwrap();

        // Verify new content
        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("new_sha"));
        assert!(!content.contains("old content"));
    }

    #[test]
    #[cfg(unix)]
    fn test_atomic_manifest_write_unix_persist() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("manifest.json");

        let build_prov = BuildProvenance::new("3.1.1", "sqry index");
        let manifest = Manifest::new("/test/workspace", 100, 200, "unix_test", build_prov);

        // Write and verify no temp files left behind
        write_manifest_atomic(&manifest_path, &manifest).unwrap();

        // Check no .tmp files in directory
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert_eq!(entries.len(), 1, "Should only have manifest.json");
        assert_eq!(entries[0].file_name(), "manifest.json");
    }

    #[test]
    #[cfg(windows)]
    fn test_atomic_manifest_write_windows_movefile() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("manifest.json");

        let build_prov = BuildProvenance::new("3.1.1", "sqry index");
        let manifest = Manifest::new("/test/workspace", 100, 200, "windows_test", build_prov);

        // Write and verify no temp files left behind
        write_manifest_atomic(&manifest_path, &manifest).unwrap();

        // Check no .tmp files in directory
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert_eq!(entries.len(), 1, "Should only have manifest.json");
        assert_eq!(entries[0].file_name(), "manifest.json");
    }
}
