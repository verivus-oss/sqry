//! Graph config schema - unified config partition types and validation.
//!
//! Implements Step 2 of the Unified Graph Config Partition feature:
//! - Type definitions for `.sqry/graph/config/config.json`
//! - Default values for all configuration sections
//! - Validation logic for schema and values
//!
//! # Schema Version
//!
//! The schema version is incremented when breaking changes are made to the
//! config structure. Non-breaking additions (new optional fields) do not
//! require a version bump.
//!
//! # Design
//!
//! See: `docs/development/unified-graph-config-partition/02_DESIGN.md`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

use super::project_config::{
    CacheConfig, IgnoreConfig, IncludeConfig, IndexingConfig, LanguageConfig,
};

/// Current schema version for config files
pub const SCHEMA_VERSION: u32 = 1;

/// Errors that can occur during schema validation
#[derive(Debug, Error)]
pub enum SchemaValidationError {
    /// Schema version is incompatible
    #[error("Incompatible schema version: expected {expected}, found {found}")]
    IncompatibleVersion {
        /// Expected schema version
        expected: u32,
        /// Actual schema version found
        found: u32,
    },

    /// A required field is missing
    #[error("Missing required field: {0}")]
    MissingField(String),

    /// A field has an invalid type
    #[error("Invalid type for field '{field}': expected {expected}, got {got}")]
    InvalidType {
        /// Name of the field with invalid type
        field: String,
        /// Expected type name
        expected: String,
        /// Actual type name found
        got: String,
    },

    /// A field has an invalid value
    #[error("Invalid value for field '{field}': {reason}")]
    InvalidValue {
        /// Name of the field with invalid value
        field: String,
        /// Reason why the value is invalid
        reason: String,
    },
}

/// Result type for schema validation
pub type ValidationResult<T> = Result<T, SchemaValidationError>;

// ============================================================================
// Top-level structure
// ============================================================================

/// Top-level config file structure
///
/// This is the complete structure stored in `.sqry/graph/config/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphConfigFile {
    /// Schema version for compatibility checking
    pub schema_version: u32,

    /// Metadata about the config file itself
    pub metadata: GraphConfigMetadata,

    /// Integrity information for corruption detection
    pub integrity: GraphConfigIntegrity,

    /// The actual configuration settings
    pub config: GraphConfig,

    /// Custom extensions for future use
    #[serde(default)]
    pub extensions: GraphConfigExtensions,
}

impl Default for GraphConfigFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            metadata: GraphConfigMetadata::default(),
            integrity: GraphConfigIntegrity::default(),
            config: GraphConfig::default(),
            extensions: GraphConfigExtensions::default(),
        }
    }
}

impl GraphConfigFile {
    /// Create a new config file with default settings
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate the schema version is compatible
    ///
    /// # Errors
    ///
    /// Returns an error if the schema version is incompatible.
    pub fn validate_version(&self) -> ValidationResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SchemaValidationError::IncompatibleVersion {
                expected: SCHEMA_VERSION,
                found: self.schema_version,
            });
        }
        Ok(())
    }

    /// Validate the entire config structure
    ///
    /// # Errors
    ///
    /// Returns an error if any nested config section fails validation.
    pub fn validate(&self) -> ValidationResult<()> {
        self.validate_version()?;
        self.config.validate()?;
        Ok(())
    }
}

// ============================================================================
// Metadata section
// ============================================================================

/// Metadata about the config file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphConfigMetadata {
    /// When the config file was first created
    pub created_at: String,

    /// When the config file was last updated
    pub updated_at: String,

    /// Information about the tool that wrote this config
    pub written_by: WrittenByInfo,
}

impl Default for GraphConfigMetadata {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            created_at: now.clone(),
            updated_at: now,
            written_by: WrittenByInfo::default(),
        }
    }
}

/// Information about the tool that wrote the config
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WrittenByInfo {
    /// sqry version that wrote this config
    pub sqry_version: String,

    /// Rust compiler version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rustc_version: Option<String>,

    /// Git revision (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_revision: Option<String>,
}

impl Default for WrittenByInfo {
    fn default() -> Self {
        Self {
            sqry_version: env!("CARGO_PKG_VERSION").to_string(),
            rustc_version: None,
            git_revision: None,
        }
    }
}

// ============================================================================
// Integrity section
// ============================================================================

/// Integrity information for corruption detection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphConfigIntegrity {
    /// Hash algorithm used
    pub normalized_hash_alg: String,

    /// What was hashed (always "config" for now)
    pub normalized_hash_of: String,

    /// The computed hash (hex-encoded)
    pub normalized_hash: String,

    /// When the hash was last verified
    pub last_verified_at: String,
}

impl Default for GraphConfigIntegrity {
    fn default() -> Self {
        Self {
            normalized_hash_alg: "blake3".to_string(),
            normalized_hash_of: "config".to_string(),
            normalized_hash: String::new(), // Computed on save
            last_verified_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ============================================================================
// Main config section
// ============================================================================

/// Main configuration settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct GraphConfig {
    /// User-defined query aliases
    pub aliases: BTreeMap<String, AliasEntry>,

    /// CLI preferences
    pub cli: CliPreferences,

    /// Validation behavior
    pub validation: ValidationConfig,

    /// Locking behavior
    pub locking: LockingConfig,

    /// Durability settings
    pub durability: DurabilityConfig,

    /// Operational limits (configurable, not hard-coded)
    pub limits: LimitsConfig,

    /// Output formatting settings
    pub output: OutputConfig,

    /// Parallelism settings
    pub parallelism: ParallelismConfig,

    /// Timeout settings
    pub timeouts: TimeoutsConfig,

    /// Graph persistence settings
    pub persistence: PersistenceConfig,

    /// Indexing configuration (from `ProjectConfig`)
    #[serde(default)]
    pub indexing: IndexingConfig,

    /// Cache configuration (from `ProjectConfig`)
    #[serde(default)]
    pub cache: CacheConfig,

    /// Language configuration (from `ProjectConfig`)
    #[serde(default)]
    pub languages: LanguageConfig,

    /// Include patterns (from `ProjectConfig`)
    #[serde(default)]
    pub include: IncludeConfig,

    /// Ignore patterns (from `ProjectConfig`)
    #[serde(default)]
    pub ignore: IgnoreConfig,

    /// Buffer size configurations
    #[serde(default)]
    pub buffers: BuffersConfig,
}

impl GraphConfig {
    /// Validate all config settings
    ///
    /// # Errors
    ///
    /// Returns an error if any section fails validation.
    pub fn validate(&self) -> ValidationResult<()> {
        self.limits.validate()?;
        self.locking.validate()?;
        self.output.validate()?;
        self.timeouts.validate()?;
        self.persistence.validate()?;
        Ok(())
    }
}

// ============================================================================
// Alias entry
// ============================================================================

/// A user-defined query alias
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AliasEntry {
    /// The query this alias expands to
    pub query: String,

    /// Optional description of what the alias does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// When the alias was created
    pub created_at: String,

    /// When the alias was last updated
    pub updated_at: String,
}

impl AliasEntry {
    /// Create a new alias entry
    pub fn new(query: impl Into<String>, description: Option<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            query: query.into(),
            description,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

// ============================================================================
// CLI preferences
// ============================================================================

/// CLI behavior preferences
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CliPreferences {
    /// Default output format (pretty, json, raw)
    pub default_output_format: String,

    /// Whether to use JSON output by default
    pub default_json: bool,
}

impl Default for CliPreferences {
    fn default() -> Self {
        Self {
            default_output_format: "pretty".to_string(),
            default_json: false,
        }
    }
}

// ============================================================================
// Validation config
// ============================================================================

/// Validation behavior settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ValidationConfig {
    /// Validation mode: "warn", "strict", or "off"
    pub mode: String,

    /// Whether to enforce integrity hash matching
    pub enforce_integrity: bool,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            mode: "warn".to_string(),
            enforce_integrity: false,
        }
    }
}

// ============================================================================
// Locking config
// ============================================================================

/// Writer lock behavior settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LockingConfig {
    /// Timeout for acquiring write lock (milliseconds)
    pub write_lock_timeout_ms: u64,

    /// Timeout before a lock is considered stale (milliseconds)
    pub stale_lock_timeout_ms: u64,

    /// Policy for stale locks: "deny", "warn", or "allow"
    pub stale_takeover_policy: String,
}

impl Default for LockingConfig {
    fn default() -> Self {
        Self {
            write_lock_timeout_ms: 5000,
            stale_lock_timeout_ms: 30000,
            stale_takeover_policy: "allow".to_string(),
        }
    }
}

impl LockingConfig {
    /// Validate locking config values
    ///
    /// # Errors
    ///
    /// Returns an error if the takeover policy is invalid.
    pub fn validate(&self) -> ValidationResult<()> {
        let valid_policies = ["deny", "warn", "allow"];
        if !valid_policies.contains(&self.stale_takeover_policy.as_str()) {
            return Err(SchemaValidationError::InvalidValue {
                field: "locking.stale_takeover_policy".to_string(),
                reason: format!(
                    "must be one of {:?}, got '{}'",
                    valid_policies, self.stale_takeover_policy
                ),
            });
        }
        Ok(())
    }
}

// ============================================================================
// Durability config
// ============================================================================

/// Durability and filesystem settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct DurabilityConfig {
    /// Allow operation on network filesystems (not recommended)
    pub allow_network_filesystems: bool,
}

// ============================================================================
// Limits config (no hard limits - all configurable)
// ============================================================================

/// Operational limits - all configurable, no hard-coded caps
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimitsConfig {
    /// Maximum number of results to return (0 = unlimited)
    pub max_results: u64,

    /// Maximum traversal depth for graph queries (0 = unlimited)
    pub max_depth: u64,

    /// Maximum bytes per file to process (0 = unlimited)
    pub max_bytes_per_file: u64,

    /// Maximum number of files to index (0 = unlimited)
    pub max_files: u64,

    /// Maximum repositories/workspaces (0 = unlimited)
    pub max_repositories: u64,

    /// Maximum query string length (0 = unlimited)
    pub max_query_length: u64,

    // === Node & Relation Limits ===
    /// Maximum sample locations to track per cross-language relation (0 = unlimited)
    pub max_sample_locations: u64,

    /// Maximum cross-language relations per language pair (0 = unlimited)
    pub max_relations_per_language_pair: u64,

    // === Query Limits ===
    /// Maximum regex pattern length in queries (0 = unlimited)
    pub max_regex_length: u64,

    /// Maximum repetition count in query patterns (0 = unlimited)
    pub max_repetition_count: u64,

    /// Maximum predicates per query (0 = unlimited)
    pub max_predicates: u64,

    /// Maximum query memory usage in bytes (0 = unlimited)
    pub max_query_memory_bytes: u64,

    /// Maximum query cost units (0 = unlimited)
    pub max_query_cost: u64,

    // === Git & External Tools ===
    /// Maximum git command output size in bytes (0 = unlimited)
    pub max_git_output_bytes: u64,

    // === Index & Storage Limits ===
    /// Maximum uncompressed index size in bytes (0 = unlimited)
    pub max_index_uncompressed_bytes: u64,

    /// Maximum compression ratio for index files (0 = unlimited)
    pub max_compression_ratio: u64,

    /// Maximum index size in bytes (compressed) (0 = unlimited)
    pub max_index_bytes: u64,

    // === Prewarm Storage Limits ===
    /// Maximum prewarm header size in bytes (0 = unlimited)
    pub max_prewarm_header_bytes: u64,

    /// Maximum prewarm payload size in bytes (0 = unlimited)
    pub max_prewarm_payload_bytes: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_results: 5000,
            max_depth: 6,
            max_bytes_per_file: 10 * 1024 * 1024, // 10 MB
            max_files: 0,                         // unlimited
            max_repositories: 0,                  // unlimited
            max_query_length: 0,                  // unlimited

            // Node & Relation Limits
            max_sample_locations: 3,
            max_relations_per_language_pair: 10_000,

            // Query Limits
            max_regex_length: 1000,
            max_repetition_count: 1000,
            max_predicates: 100,
            max_query_memory_bytes: 512 * 1024 * 1024, // 512 MB
            max_query_cost: 1_000_000,

            // Git & External Tools
            max_git_output_bytes: 10 * 1024 * 1024, // 10 MB

            // Index & Storage Limits
            max_index_uncompressed_bytes: 500 * 1024 * 1024, // 500 MB
            max_compression_ratio: 100,
            max_index_bytes: 1_000_000_000, // 1 GB

            // Prewarm Storage Limits
            max_prewarm_header_bytes: 4 * 1024,            // 4 KB
            max_prewarm_payload_bytes: 1024 * 1024 * 1024, // 1 GB
        }
    }
}

impl LimitsConfig {
    /// Validate limits config - values are always valid (0 = unlimited)
    ///
    /// # Errors
    ///
    /// Returns an error if limits validation fails.
    pub fn validate(&self) -> ValidationResult<()> {
        // All limits are valid - 0 means unlimited
        Ok(())
    }
}

// ============================================================================
// Output config
// ============================================================================

/// Output formatting settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputConfig {
    /// Enable pagination by default
    pub default_pagination: bool,

    /// Default page size for paginated output
    pub page_size: u64,

    /// Maximum bytes to show in previews
    pub max_preview_bytes: u64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            default_pagination: true,
            page_size: 50,
            max_preview_bytes: 64 * 1024, // 64 KB
        }
    }
}

impl OutputConfig {
    /// Validate output config values
    ///
    /// # Errors
    ///
    /// Returns an error if output values are invalid.
    pub fn validate(&self) -> ValidationResult<()> {
        if self.page_size == 0 {
            return Err(SchemaValidationError::InvalidValue {
                field: "output.page_size".to_string(),
                reason: "must be greater than 0".to_string(),
            });
        }
        Ok(())
    }
}

// ============================================================================
// Parallelism config
// ============================================================================

/// Parallelism settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ParallelismConfig {
    /// Maximum threads for parallel operations (0 = auto-detect)
    pub max_threads: u64,

    /// Maximum lexer pool size (0 = auto-detect)
    pub lexer_pool_max: u64,

    /// Compaction chunk size for interruptible compaction
    pub compaction_chunk_size: u64,
}

impl Default for ParallelismConfig {
    fn default() -> Self {
        Self {
            max_threads: 0, // auto-detect
            lexer_pool_max: 4,
            compaction_chunk_size: 10_000,
        }
    }
}

// ============================================================================
// Timeouts config
// ============================================================================

/// Timeout settings for various operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TimeoutsConfig {
    /// Query timeout in milliseconds (0 = unlimited)
    pub query_timeout_ms: u64,

    /// Build/index timeout in milliseconds (0 = unlimited)
    pub build_timeout_ms: u64,

    /// Parse timeout in microseconds for tree-sitter (0 = unlimited)
    pub parse_timeout_us: u64,

    /// Session idle timeout in milliseconds (0 = unlimited)
    pub session_timeout_ms: u64,

    /// File watch debounce timeout in milliseconds (0 = unlimited)
    pub watch_debounce_ms: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            query_timeout_ms: 0,         // unlimited
            build_timeout_ms: 0,         // unlimited
            parse_timeout_us: 2_000_000, // 2 seconds
            session_timeout_ms: 120_000, // 2 minutes
            watch_debounce_ms: 50,       // 50 milliseconds
        }
    }
}

impl TimeoutsConfig {
    /// Validate timeout values
    ///
    /// # Errors
    ///
    /// Returns an error if timeout validation fails.
    pub fn validate(&self) -> ValidationResult<()> {
        // 0 means unlimited, all values are valid
        Ok(())
    }
}

// ============================================================================
// Persistence config
// ============================================================================

/// Graph persistence size limits
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersistenceConfig {
    /// Maximum manifest file size in bytes (0 = unlimited)
    pub max_manifest_bytes: u64,

    /// Maximum snapshot file size in bytes (0 = unlimited)
    pub max_snapshot_bytes: u64,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 1024 * 1024, // 1 MB
            max_snapshot_bytes: 0,           // unlimited
        }
    }
}

impl PersistenceConfig {
    /// Validate persistence config values
    ///
    /// # Errors
    ///
    /// Returns an error if persistence validation fails.
    pub fn validate(&self) -> ValidationResult<()> {
        // 0 means unlimited, all values are valid
        Ok(())
    }
}

// ============================================================================
// Buffers config
// ============================================================================

/// Buffer size configurations
///
/// These were previously environment variables or hard-coded defaults.
/// Now they're fully configurable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BuffersConfig {
    /// Parse buffer size in bytes
    pub parse_buffer_bytes: u64,

    /// Node extraction buffer size in bytes
    pub symbol_buffer_bytes: u64,

    /// LRU cache capacity for parsed ASTs
    pub ast_cache_capacity: u64,

    /// Query result buffer initial capacity
    pub query_result_capacity: u64,

    /// Watch event queue capacity
    pub watch_event_queue_size: u64,

    /// Channel capacity for background workers
    pub channel_capacity: u64,

    /// Read buffer size in bytes
    pub read_buffer_bytes: u64,

    /// Write buffer size in bytes
    pub write_buffer_bytes: u64,

    /// Index buffer size in bytes
    pub index_buffer_bytes: u64,
}

impl Default for BuffersConfig {
    fn default() -> Self {
        Self {
            parse_buffer_bytes: 1024 * 1024, // 1 MB
            symbol_buffer_bytes: 512 * 1024, // 512 KB
            ast_cache_capacity: 100,         // 100 entries
            query_result_capacity: 1000,     // 1000 results
            watch_event_queue_size: 10_000,  // 10k events
            channel_capacity: 1000,          // 1000 items
            read_buffer_bytes: 8192,         // 8 KB
            write_buffer_bytes: 8192,        // 8 KB
            index_buffer_bytes: 1024 * 1024, // 1 MB
        }
    }
}

// ============================================================================
// Extensions
// ============================================================================

/// Custom extensions for future use
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GraphConfigExtensions {
    /// Custom key-value pairs
    #[serde(flatten)]
    pub custom: BTreeMap<String, serde_json::Value>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_roundtrip() {
        let config = GraphConfigFile::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: GraphConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_schema_version_check() {
        let config = GraphConfigFile::default();
        assert!(config.validate_version().is_ok());

        #[allow(clippy::field_reassign_with_default)]
        let old_config = {
            let mut c = GraphConfigFile::default();
            c.schema_version = 0;
            c
        };
        assert!(old_config.validate_version().is_err());
    }

    #[test]
    fn test_validation_rejects_invalid_policy() {
        let mut config = GraphConfigFile::default();
        config.config.locking.stale_takeover_policy = "invalid".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_accepts_valid_policies() {
        for policy in ["deny", "warn", "allow"] {
            let mut config = GraphConfigFile::default();
            config.config.locking.stale_takeover_policy = policy.to_string();
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn test_validation_rejects_zero_page_size() {
        let mut config = GraphConfigFile::default();
        config.config.output.page_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_limits_zero_means_unlimited() {
        let mut config = LimitsConfig::default();
        config.max_results = 0;
        config.max_depth = 0;
        config.max_bytes_per_file = 0;
        config.max_files = 0;
        config.max_repositories = 0;
        config.max_query_length = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_alias_entry_creation() {
        let alias = AliasEntry::new("kind:function", Some("Find functions".to_string()));
        assert_eq!(alias.query, "kind:function");
        assert_eq!(alias.description, Some("Find functions".to_string()));
        assert!(!alias.created_at.is_empty());
        assert!(!alias.updated_at.is_empty());
    }

    #[test]
    fn test_metadata_timestamps() {
        let metadata = GraphConfigMetadata::default();
        assert!(!metadata.created_at.is_empty());
        assert!(!metadata.updated_at.is_empty());
    }

    #[test]
    fn test_written_by_has_version() {
        let written_by = WrittenByInfo::default();
        assert!(!written_by.sqry_version.is_empty());
    }

    #[test]
    fn test_extensions_empty_by_default() {
        let ext = GraphConfigExtensions::default();
        assert!(ext.custom.is_empty());
    }

    #[test]
    fn test_full_validation() {
        let config = GraphConfigFile::default();
        assert!(config.validate().is_ok());
    }
}
