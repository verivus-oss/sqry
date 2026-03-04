//! Configuration snapshot for `CodeGraph` embedding.
//!
//! This module captures the effective sqry configuration used during graph build,
//! enabling provenance tracking, reproducibility, and security audits.
//!
//! # Overview
//!
//! The [`ConfigSnapshot`] captures all runtime limits/knobs from:
//! - Environment variables (`SQRY_*`)
//! - CLI arguments (future)
//! - Project config (`.sqry-config.toml`)
//! - Defaults
//!
//! Each entry includes provenance metadata (source, default, min/max range)
//! per  requirements.
//!
//! # Schema Version
//!
//! The `CONFIG_SCHEMA_VERSION` constant tracks the schema version for
//! forward/backward compatibility in graph exports.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use super::buffers::{
    self, DEFAULT_INDEX_BUFFER, DEFAULT_MAX_PREDICATES, DEFAULT_MAX_QUERY_LENGTH,
    DEFAULT_MAX_REPOSITORIES, DEFAULT_MAX_SOURCE_FILE_SIZE, DEFAULT_MMAP_THRESHOLD,
    DEFAULT_PARSE_BUFFER, DEFAULT_READ_BUFFER, DEFAULT_WATCH_EVENT_QUEUE, DEFAULT_WRITE_BUFFER,
};

/// Current schema version for config snapshots.
///
/// Increment when:
/// - Adding new config entries
/// - Changing entry field semantics
/// - Modifying serialization format
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Source of a configuration value.
///
/// Ordered by precedence (highest to lowest):
/// CLI > Env > `ProjectConfig` > Default
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Value from CLI argument (highest precedence)
    Cli,
    /// Value from environment variable
    Env,
    /// Value from `.sqry-config.toml`
    ProjectConfig,
    /// Default value (lowest precedence)
    Default,
}

impl std::fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigSource::Cli => write!(f, "cli"),
            ConfigSource::Env => write!(f, "env"),
            ConfigSource::ProjectConfig => write!(f, "project_config"),
            ConfigSource::Default => write!(f, "default"),
        }
    }
}

/// Scope of a configuration entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    /// Applies globally across all projects
    Global,
    /// Applies to a specific project only
    Project,
}

/// Risk category for a configuration entry.
///
/// Used for security audits to identify which settings
/// affect system safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRisk {
    /// Denial of Service prevention (memory/CPU exhaustion)
    Dos,
    /// Performance tuning (throughput, latency)
    Perf,
    /// Security-sensitive (access control, limits)
    Security,
    /// System reliability (timeouts, retries)
    Reliability,
}

/// A single configuration entry captured in the graph.
///
/// Contains the effective value plus provenance metadata for auditing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Configuration key name (e.g., "`SQRY_MAX_SOURCE_FILE_SIZE`")
    pub name: String,

    /// Effective value after precedence resolution (serialized as string)
    pub effective_value: String,

    /// Default value for this entry
    pub default_value: String,

    /// Minimum allowed value (if bounded)
    #[serde(default)]
    pub min_value: Option<String>,

    /// Maximum allowed value (if bounded)
    #[serde(default)]
    pub max_value: Option<String>,

    /// Source that provided the effective value
    pub source: ConfigSource,

    /// Scope of this configuration
    pub scope: ConfigScope,

    /// Risk category for security audits
    #[serde(default)]
    pub risk: Option<ConfigRisk>,

    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
}

impl ConfigEntry {
    /// Create a new config entry with required fields.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        effective_value: impl Into<String>,
        default_value: impl Into<String>,
        source: ConfigSource,
        scope: ConfigScope,
    ) -> Self {
        Self {
            name: name.into(),
            effective_value: effective_value.into(),
            default_value: default_value.into(),
            min_value: None,
            max_value: None,
            source,
            scope,
            risk: None,
            description: None,
        }
    }

    /// Set the min/max range for this entry.
    #[must_use]
    pub fn with_range(mut self, min: impl Into<String>, max: impl Into<String>) -> Self {
        self.min_value = Some(min.into());
        self.max_value = Some(max.into());
        self
    }

    /// Set the risk category for this entry.
    #[must_use]
    pub fn with_risk(mut self, risk: ConfigRisk) -> Self {
        self.risk = Some(risk);
        self
    }

    /// Set the description for this entry.
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Snapshot of effective configuration used for graph build.
///
/// Captures all runtime limits and knobs with provenance metadata.
/// This is embedded into the `CodeGraph` for auditing and reproducibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    /// Schema version for forward/backward compatibility
    pub schema_version: u32,

    /// Timestamp when the snapshot was collected
    #[serde(with = "system_time_serde")]
    pub collected_at: SystemTime,

    /// All configuration entries
    pub entries: Vec<ConfigEntry>,
}

impl ConfigSnapshot {
    /// Create a new empty snapshot with current timestamp.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            collected_at: SystemTime::now(),
            entries: Vec::new(),
        }
    }

    /// Add an entry to the snapshot.
    pub fn add_entry(&mut self, entry: ConfigEntry) {
        self.entries.push(entry);
    }

    /// Get the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the snapshot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find an entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ConfigEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Compute a hash of the snapshot for sidecar verification.
    ///
    /// Uses BLAKE3 for deterministic, cross-process-stable hashing.
    /// The hash covers:
    /// - Schema version
    /// - Collected timestamp (for provenance)
    /// - All `ConfigEntry` fields (complete coverage for integrity)
    #[must_use]
    pub fn compute_hash(&self) -> String {
        use crate::hash::hash_bytes;
        use std::fmt::Write;
        use std::time::UNIX_EPOCH;

        // Build canonical representation for hashing
        // Use a deterministic format that includes all fields
        let mut canonical = String::new();

        // Schema version
        let _ = writeln!(canonical, "schema_version:{}", self.schema_version);

        // Timestamp (milliseconds since epoch for consistency)
        let millis = self
            .collected_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(canonical, "collected_at:{millis}");

        // Sort entries by name for deterministic ordering
        let mut sorted_entries: Vec<_> = self.entries.iter().collect();
        sorted_entries.sort_by_key(|e| &e.name);

        // Include ALL fields from each entry
        for entry in sorted_entries {
            let _ = writeln!(canonical, "entry:{}", entry.name);
            let _ = writeln!(canonical, "  effective_value:{}", entry.effective_value);
            let _ = writeln!(canonical, "  default_value:{}", entry.default_value);
            let _ = writeln!(
                canonical,
                "  min_value:{}",
                entry.min_value.as_deref().unwrap_or("")
            );
            let _ = writeln!(
                canonical,
                "  max_value:{}",
                entry.max_value.as_deref().unwrap_or("")
            );
            let _ = writeln!(canonical, "  source:{}", entry.source);
            let _ = writeln!(canonical, "  scope:{:?}", entry.scope);
            if let Some(risk) = entry.risk {
                let _ = writeln!(canonical, "  risk:{risk:?}");
            } else {
                let _ = writeln!(canonical, "  risk:");
            }
            let _ = writeln!(
                canonical,
                "  description:{}",
                entry.description.as_deref().unwrap_or("")
            );
        }

        // Hash with BLAKE3 and return hex string
        hash_bytes(canonical.as_bytes()).to_hex()
    }
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// Serde module for `SystemTime` serialization.
mod system_time_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        let millis = u64::try_from(duration.as_millis()).map_err(serde::ser::Error::custom)?;
        millis.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_millis(millis))
    }
}

/// Builder for constructing a [`ConfigSnapshot`] from various sources.
///
/// Implements precedence: CLI > Env > `ProjectConfig` > Default
pub struct ConfigSnapshotBuilder {
    snapshot: ConfigSnapshot,
}

impl ConfigSnapshotBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: ConfigSnapshot::new(),
        }
    }

    /// Build the complete snapshot by resolving all known config entries.
    ///
    /// This collects all documented configuration items from:
    /// - `DoS` prevention limits
    /// - Git limits
    /// - Buffer sizes
    /// - Memory management
    /// - Cache settings (including regex cache and lexer pool)
    #[must_use]
    pub fn build(mut self) -> ConfigSnapshot {
        // DoS Prevention Limits
        self.add_dos_limits();

        // Git Limits (P1-17)
        self.add_git_limits();

        // Buffer Sizes
        self.add_buffer_sizes();

        // Memory Management
        self.add_memory_settings();

        // Cache Configuration (includes regex cache, lexer pool)
        self.add_cache_settings();

        self.snapshot
    }

    /// Add `DoS` prevention limit entries.
    fn add_dos_limits(&mut self) {
        // SQRY_MAX_SOURCE_FILE_SIZE
        let effective = buffers::max_source_file_size();
        let source = Self::detect_source(
            "SQRY_MAX_SOURCE_FILE_SIZE",
            &effective,
            &DEFAULT_MAX_SOURCE_FILE_SIZE,
        );
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_MAX_SOURCE_FILE_SIZE",
                effective.to_string(),
                DEFAULT_MAX_SOURCE_FILE_SIZE.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1048576", "524288000") // 1 MB - 500 MB
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum source file size for indexing"),
        );

        // SQRY_MAX_REPOSITORIES
        let effective = buffers::max_repositories();
        let source = Self::detect_source(
            "SQRY_MAX_REPOSITORIES",
            &effective,
            &DEFAULT_MAX_REPOSITORIES,
        );
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_MAX_REPOSITORIES",
                effective.to_string(),
                DEFAULT_MAX_REPOSITORIES.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("10", "10000")
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum repositories per workspace"),
        );

        // SQRY_WATCH_EVENT_QUEUE
        let effective = buffers::watch_event_queue_capacity();
        let source = Self::detect_source(
            "SQRY_WATCH_EVENT_QUEUE",
            &effective,
            &DEFAULT_WATCH_EVENT_QUEUE,
        );
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_WATCH_EVENT_QUEUE",
                effective.to_string(),
                DEFAULT_WATCH_EVENT_QUEUE.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("100", "100000")
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum queued filesystem events"),
        );

        // SQRY_MAX_QUERY_LENGTH
        let effective = buffers::max_query_length();
        let source = Self::detect_source(
            "SQRY_MAX_QUERY_LENGTH",
            &effective,
            &DEFAULT_MAX_QUERY_LENGTH,
        );
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_MAX_QUERY_LENGTH",
                effective.to_string(),
                DEFAULT_MAX_QUERY_LENGTH.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1024", "102400") // 1 KB - 100 KB
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum query string length in bytes"),
        );

        // SQRY_MAX_PREDICATES
        let effective = buffers::max_predicates();
        let source =
            Self::detect_source("SQRY_MAX_PREDICATES", &effective, &DEFAULT_MAX_PREDICATES);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_MAX_PREDICATES",
                effective.to_string(),
                DEFAULT_MAX_PREDICATES.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("10", "1000")
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum predicates per query"),
        );
    }

    /// Add buffer size entries.
    fn add_buffer_sizes(&mut self) {
        // SQRY_READ_BUFFER
        let effective = buffers::read_buffer_size();
        let source = Self::detect_source("SQRY_READ_BUFFER", &effective, &DEFAULT_READ_BUFFER);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_READ_BUFFER",
                effective.to_string(),
                DEFAULT_READ_BUFFER.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1024", "1048576") // 1 KB - 1 MB
            .with_risk(ConfigRisk::Perf)
            .with_description("Read buffer size for file I/O"),
        );

        // SQRY_WRITE_BUFFER
        let effective = buffers::write_buffer_size();
        let source = Self::detect_source("SQRY_WRITE_BUFFER", &effective, &DEFAULT_WRITE_BUFFER);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_WRITE_BUFFER",
                effective.to_string(),
                DEFAULT_WRITE_BUFFER.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1024", "1048576")
            .with_risk(ConfigRisk::Perf)
            .with_description("Write buffer size for file I/O"),
        );

        // SQRY_PARSE_BUFFER
        let effective = buffers::parse_buffer_size();
        let source = Self::detect_source("SQRY_PARSE_BUFFER", &effective, &DEFAULT_PARSE_BUFFER);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_PARSE_BUFFER",
                effective.to_string(),
                DEFAULT_PARSE_BUFFER.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("4096", "10485760") // 4 KB - 10 MB
            .with_risk(ConfigRisk::Perf)
            .with_description("Parse buffer size for tree-sitter"),
        );

        // SQRY_INDEX_BUFFER
        let effective = buffers::index_buffer_size();
        let source = Self::detect_source("SQRY_INDEX_BUFFER", &effective, &DEFAULT_INDEX_BUFFER);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_INDEX_BUFFER",
                effective.to_string(),
                DEFAULT_INDEX_BUFFER.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("65536", "104857600") // 64 KB - 100 MB
            .with_risk(ConfigRisk::Perf)
            .with_description("Index buffer size for serialization"),
        );
    }

    /// Add memory management entries.
    fn add_memory_settings(&mut self) {
        // SQRY_MMAP_THRESHOLD
        let effective = buffers::mmap_threshold();
        let source =
            Self::detect_source_u64("SQRY_MMAP_THRESHOLD", effective, DEFAULT_MMAP_THRESHOLD);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_MMAP_THRESHOLD",
                effective.to_string(),
                DEFAULT_MMAP_THRESHOLD.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1048576", "1073741824") // 1 MB - 1 GB
            .with_risk(ConfigRisk::Perf)
            .with_description("File size threshold for memory-mapped I/O"),
        );
    }

    /// Add cache configuration entries.
    fn add_cache_settings(&mut self) {
        // SQRY_CACHE_BUDGET_ENTRIES
        let default_entries: usize = 10_000;
        let effective = std::env::var("SQRY_CACHE_BUDGET_ENTRIES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_entries);
        let source = Self::detect_source("SQRY_CACHE_BUDGET_ENTRIES", &effective, &default_entries);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_CACHE_BUDGET_ENTRIES",
                effective.to_string(),
                default_entries.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_risk(ConfigRisk::Perf)
            .with_description("Maximum cache entries"),
        );

        // SQRY_CACHE_BUDGET_BYTES
        let default_bytes: u64 = 100 * 1024 * 1024; // 100 MB
        let effective = std::env::var("SQRY_CACHE_BUDGET_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_bytes);
        let source = Self::detect_source_u64("SQRY_CACHE_BUDGET_BYTES", effective, default_bytes);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_CACHE_BUDGET_BYTES",
                effective.to_string(),
                default_bytes.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_risk(ConfigRisk::Perf)
            .with_description("Maximum cache size in bytes"),
        );

        // SQRY_CACHE_MAX_BYTES (CacheConfig size cap)
        // Default: 50 MB per CacheConfig::DEFAULT_MAX_BYTES
        let default_cache_max: u64 = 50 * 1024 * 1024; // 50 MB
        let effective = std::env::var("SQRY_CACHE_MAX_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_cache_max);
        let source = Self::detect_source_u64("SQRY_CACHE_MAX_BYTES", effective, default_cache_max);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_CACHE_MAX_BYTES",
                effective.to_string(),
                default_cache_max.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1048576", "10737418240") // 1 MB - 10 GB
            .with_risk(ConfigRisk::Dos)
            .with_description("Cache size cap (CacheConfig max_bytes limit)"),
        );

        // SQRY_REGEX_CACHE_SIZE
        let default_regex_cache: usize = 100;
        let effective = std::env::var("SQRY_REGEX_CACHE_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&s| (1..=10_000).contains(&s))
            .unwrap_or(default_regex_cache);
        let source = Self::detect_source("SQRY_REGEX_CACHE_SIZE", &effective, &default_regex_cache);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_REGEX_CACHE_SIZE",
                effective.to_string(),
                default_regex_cache.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range("1", "10000")
            .with_risk(ConfigRisk::Perf)
            .with_description("LRU cache size for compiled regexes"),
        );

        // SQRY_LEXER_POOL_MAX
        let default_lexer_pool: usize = 4;
        let effective = std::env::var("SQRY_LEXER_POOL_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_lexer_pool);
        let source = Self::detect_source("SQRY_LEXER_POOL_MAX", &effective, &default_lexer_pool);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_LEXER_POOL_MAX",
                effective.to_string(),
                default_lexer_pool.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_risk(ConfigRisk::Perf)
            .with_description("Maximum lexer pool size"),
        );
    }

    /// Add Git-related limit entries.
    fn add_git_limits(&mut self) {
        // SQRY_GIT_MAX_OUTPUT_SIZE
        // Uses crate::git::max_git_output_size() but we need to access the value directly
        // to avoid circular dependencies. The function clamps to 1MB-100MB range.
        let default_git_output: usize = 10 * 1024 * 1024; // 10 MB
        let min_git_output: usize = 1024 * 1024; // 1 MB
        let max_git_output: usize = 100 * 1024 * 1024; // 100 MB

        let effective = std::env::var("SQRY_GIT_MAX_OUTPUT_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .map_or(default_git_output, |size: usize| {
                size.clamp(min_git_output, max_git_output)
            });
        let source =
            Self::detect_source("SQRY_GIT_MAX_OUTPUT_SIZE", &effective, &default_git_output);
        self.snapshot.add_entry(
            ConfigEntry::new(
                "SQRY_GIT_MAX_OUTPUT_SIZE",
                effective.to_string(),
                default_git_output.to_string(),
                source,
                ConfigScope::Global,
            )
            .with_range(min_git_output.to_string(), max_git_output.to_string())
            .with_risk(ConfigRisk::Dos)
            .with_description("Maximum git command output size to prevent memory exhaustion"),
        );
    }

    /// Detect the source of a usize config value.
    fn detect_source<T: PartialEq>(env_var: &str, effective: &T, default: &T) -> ConfigSource {
        if std::env::var(env_var).is_ok() {
            ConfigSource::Env
        } else if effective != default {
            // Value differs from default but not from env - must be from project config or CLI
            // For now, we assume project config since CLI integration is future work
            ConfigSource::ProjectConfig
        } else {
            ConfigSource::Default
        }
    }

    /// Detect the source of a u64 config value.
    fn detect_source_u64(env_var: &str, effective: u64, default: u64) -> ConfigSource {
        if std::env::var(env_var).is_ok() {
            ConfigSource::Env
        } else if effective != default {
            ConfigSource::ProjectConfig
        } else {
            ConfigSource::Default
        }
    }
}

impl Default for ConfigSnapshotBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Collect a complete configuration snapshot.
///
/// This is the primary entry point for capturing the effective configuration
/// for embedding into the `CodeGraph`.
///
/// # Example
///
/// ```
/// use sqry_core::config::snapshot::collect_snapshot;
///
/// let snapshot = collect_snapshot();
/// assert!(snapshot.len() > 0);
/// assert_eq!(snapshot.schema_version, 1);
/// ```
#[must_use]
pub fn collect_snapshot() -> ConfigSnapshot {
    ConfigSnapshotBuilder::new().build()
}

/// Inventory of all known config entry names.
///
/// Used for completeness validation per Invariant
/// Must include ALL entries from `HARD_LIMIT_INVENTORY.md` that affect indexing safety/perf.
pub const CONFIG_INVENTORY: &[&str] = &[
    // DoS Prevention
    "SQRY_MAX_SOURCE_FILE_SIZE",
    "SQRY_MAX_REPOSITORIES",
    "SQRY_WATCH_EVENT_QUEUE",
    "SQRY_MAX_QUERY_LENGTH",
    "SQRY_MAX_PREDICATES",
    "SQRY_GIT_MAX_OUTPUT_SIZE", // P1-17: Git output limit
    // Buffers
    "SQRY_READ_BUFFER",
    "SQRY_WRITE_BUFFER",
    "SQRY_PARSE_BUFFER",
    "SQRY_INDEX_BUFFER",
    // Memory
    "SQRY_MMAP_THRESHOLD",
    // Cache
    "SQRY_CACHE_BUDGET_ENTRIES",
    "SQRY_CACHE_BUDGET_BYTES",
    "SQRY_CACHE_MAX_BYTES",  // Cache size cap (DoS prevention)
    "SQRY_REGEX_CACHE_SIZE", // Regex compilation cache
    "SQRY_LEXER_POOL_MAX",   // Lexer pool size
];

/// Validate that a snapshot contains all required entries.
///
/// Returns an error with the list of missing entries if validation fails.
///
/// # Errors
///
/// Returns `Err` with a list of missing entry names if the snapshot
/// is incomplete.
pub fn validate_completeness(snapshot: &ConfigSnapshot) -> Result<(), Vec<&'static str>> {
    let present: std::collections::HashSet<_> =
        snapshot.entries.iter().map(|e| e.name.as_str()).collect();
    let missing: Vec<_> = CONFIG_INVENTORY
        .iter()
        .filter(|name| !present.contains(*name))
        .copied()
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Sidecar provenance artifact for resilient config recovery.
///
/// This is written alongside the main index for recovery if the
/// graph is corrupted or unavailable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProvenance {
    /// Schema version matching the embedded snapshot
    pub schema_version: u32,

    /// Hash of the config snapshot for integrity verification
    pub config_hash: String,

    /// Timestamp when sidecar was generated
    #[serde(with = "system_time_serde")]
    pub generated_at: SystemTime,

    /// All configuration entries (same as in-graph data)
    pub entries: Vec<ConfigEntry>,
}

impl ConfigProvenance {
    /// Create a new provenance sidecar from a config snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            schema_version: snapshot.schema_version,
            config_hash: snapshot.compute_hash(),
            generated_at: SystemTime::now(),
            entries: snapshot.entries.clone(),
        }
    }

    /// Write the sidecar to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created or written.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    /// Load a sidecar from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let provenance: Self = serde_json::from_reader(file)?;
        Ok(provenance)
    }

    /// Verify that the sidecar matches a config snapshot.
    #[must_use]
    pub fn verify(&self, snapshot: &ConfigSnapshot) -> bool {
        self.config_hash == snapshot.compute_hash()
            && self.schema_version == snapshot.schema_version
    }
}

/// Standard filename for the config provenance sidecar.
pub const CONFIG_PROVENANCE_FILENAME: &str = "config-provenance.json";

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_config_entry_creation() {
        let entry = ConfigEntry::new(
            "TEST_VAR",
            "100",
            "50",
            ConfigSource::Env,
            ConfigScope::Global,
        )
        .with_range("10", "1000")
        .with_risk(ConfigRisk::Dos)
        .with_description("Test variable");

        assert_eq!(entry.name, "TEST_VAR");
        assert_eq!(entry.effective_value, "100");
        assert_eq!(entry.default_value, "50");
        assert_eq!(entry.min_value, Some("10".to_string()));
        assert_eq!(entry.max_value, Some("1000".to_string()));
        assert_eq!(entry.source, ConfigSource::Env);
        assert_eq!(entry.scope, ConfigScope::Global);
        assert_eq!(entry.risk, Some(ConfigRisk::Dos));
        assert_eq!(entry.description, Some("Test variable".to_string()));
    }

    #[test]
    fn test_config_snapshot_new() {
        let snapshot = ConfigSnapshot::new();
        assert_eq!(snapshot.schema_version, CONFIG_SCHEMA_VERSION);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn test_config_snapshot_add_entry() {
        let mut snapshot = ConfigSnapshot::new();
        snapshot.add_entry(ConfigEntry::new(
            "TEST",
            "value",
            "default",
            ConfigSource::Default,
            ConfigScope::Global,
        ));

        assert_eq!(snapshot.len(), 1);
        assert!(!snapshot.is_empty());
        assert!(snapshot.get("TEST").is_some());
        assert!(snapshot.get("NONEXISTENT").is_none());
    }

    #[test]
    fn test_config_snapshot_hash() {
        let mut snapshot1 = ConfigSnapshot::new();
        snapshot1.add_entry(ConfigEntry::new(
            "TEST",
            "value",
            "default",
            ConfigSource::Default,
            ConfigScope::Global,
        ));

        let mut snapshot2 = ConfigSnapshot::new();
        snapshot2.add_entry(ConfigEntry::new(
            "TEST",
            "value",
            "default",
            ConfigSource::Default,
            ConfigScope::Global,
        ));

        // Same content should produce same hash
        assert_eq!(snapshot1.compute_hash(), snapshot2.compute_hash());

        // Different content should produce different hash
        snapshot2.add_entry(ConfigEntry::new(
            "TEST2",
            "value2",
            "default2",
            ConfigSource::Env,
            ConfigScope::Global,
        ));
        assert_ne!(snapshot1.compute_hash(), snapshot2.compute_hash());
    }

    #[test]
    #[serial]
    fn test_collect_snapshot_defaults() {
        // Clear all env vars to get defaults
        for var in CONFIG_INVENTORY {
            unsafe { std::env::remove_var(var) };
        }

        let snapshot = collect_snapshot();

        // Should have all inventory entries
        assert_eq!(snapshot.len(), CONFIG_INVENTORY.len());

        // Validate completeness
        assert!(validate_completeness(&snapshot).is_ok());

        // Check a specific entry
        let entry = snapshot.get("SQRY_MAX_SOURCE_FILE_SIZE").unwrap();
        assert_eq!(entry.source, ConfigSource::Default);
        assert_eq!(
            entry.effective_value,
            DEFAULT_MAX_SOURCE_FILE_SIZE.to_string()
        );
    }

    #[test]
    #[serial]
    fn test_collect_snapshot_env_override() {
        unsafe {
            std::env::set_var("SQRY_MAX_SOURCE_FILE_SIZE", "104857600"); // 100 MB
        }

        let snapshot = collect_snapshot();
        let entry = snapshot.get("SQRY_MAX_SOURCE_FILE_SIZE").unwrap();

        assert_eq!(entry.source, ConfigSource::Env);
        assert_eq!(entry.effective_value, "104857600");

        unsafe {
            std::env::remove_var("SQRY_MAX_SOURCE_FILE_SIZE");
        }
    }

    #[test]
    fn test_validate_completeness_missing() {
        let snapshot = ConfigSnapshot::new(); // Empty snapshot
        let result = validate_completeness(&snapshot);

        assert!(result.is_err());
        let missing = result.unwrap_err();
        assert_eq!(missing.len(), CONFIG_INVENTORY.len());
    }

    #[test]
    fn test_config_source_display() {
        assert_eq!(ConfigSource::Cli.to_string(), "cli");
        assert_eq!(ConfigSource::Env.to_string(), "env");
        assert_eq!(ConfigSource::ProjectConfig.to_string(), "project_config");
        assert_eq!(ConfigSource::Default.to_string(), "default");
    }

    #[test]
    fn test_config_entry_serialization() {
        let entry = ConfigEntry::new("TEST", "100", "50", ConfigSource::Env, ConfigScope::Global)
            .with_range("10", "1000")
            .with_risk(ConfigRisk::Dos);

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: ConfigEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_config_snapshot_serialization() {
        let snapshot = collect_snapshot();
        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: ConfigSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(snapshot.schema_version, deserialized.schema_version);
        assert_eq!(snapshot.entries.len(), deserialized.entries.len());
    }

    #[test]
    fn test_config_provenance_from_snapshot() {
        let snapshot = collect_snapshot();
        let provenance = ConfigProvenance::from_snapshot(&snapshot);

        assert_eq!(provenance.schema_version, snapshot.schema_version);
        assert_eq!(provenance.config_hash, snapshot.compute_hash());
        assert_eq!(provenance.entries.len(), snapshot.entries.len());
    }

    #[test]
    fn test_config_provenance_verify() {
        let snapshot = collect_snapshot();
        let provenance = ConfigProvenance::from_snapshot(&snapshot);

        // Same snapshot should verify
        assert!(provenance.verify(&snapshot));

        // Modified snapshot should not verify
        let mut modified = snapshot.clone();
        modified.add_entry(ConfigEntry::new(
            "NEW_ENTRY",
            "value",
            "default",
            ConfigSource::Default,
            ConfigScope::Global,
        ));
        assert!(!provenance.verify(&modified));
    }

    #[test]
    fn test_config_provenance_save_load() {
        let snapshot = collect_snapshot();
        let provenance = ConfigProvenance::from_snapshot(&snapshot);

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test-provenance.json");

        // Save
        provenance.save(&path).unwrap();
        assert!(path.exists());

        // Load
        let loaded = ConfigProvenance::load(&path).unwrap();
        assert_eq!(loaded.schema_version, provenance.schema_version);
        assert_eq!(loaded.config_hash, provenance.config_hash);
        assert_eq!(loaded.entries.len(), provenance.entries.len());

        // Verify loaded matches original snapshot
        assert!(loaded.verify(&snapshot));
    }

    /// Regression test for `postcard` serialization of `ConfigEntry` with optional fields.
    ///
    /// This test prevents reintroduction of `#[serde(skip_serializing_if)]` on
    /// `ConfigEntry` fields. Binary formats like `postcard` require fixed field order;
    /// conditionally omitting fields corrupts the stream and breaks deserialization.
    ///
    /// Bug: 2025-12-12 - Used `skip_serializing_if` on `min_value`, `max_value`, `risk`,
    /// `description`
    /// Fix: Changed to `#[serde(default)]` which doesn't affect serialization
    #[test]
    fn test_config_entry_postcard_roundtrip_with_none() {
        // Test case 1: All optional fields are None
        let entry_all_none = ConfigEntry {
            name: "test.setting".to_string(),
            effective_value: "123".to_string(),
            default_value: "123".to_string(),
            min_value: None,
            max_value: None,
            source: ConfigSource::Default,
            scope: ConfigScope::Global,
            risk: None,
            description: None,
        };

        // Test case 2: All optional fields are Some
        let entry_all_some = ConfigEntry {
            name: "test.setting".to_string(),
            effective_value: "456".to_string(),
            default_value: "100".to_string(),
            min_value: Some("1".to_string()),
            max_value: Some("1000".to_string()),
            source: ConfigSource::Env,
            scope: ConfigScope::Project,
            risk: Some(ConfigRisk::Dos),
            description: Some("Test description".to_string()),
        };

        // Test case 3: Mixed None/Some (the pattern that triggered the bug)
        let entry_mixed = ConfigEntry {
            name: "mixed.setting".to_string(),
            effective_value: "789".to_string(),
            default_value: "500".to_string(),
            min_value: Some("100".to_string()),
            max_value: None, // This None after a Some triggered the deserialization bug
            source: ConfigSource::ProjectConfig,
            scope: ConfigScope::Global,
            risk: None,
            description: Some("Mixed case".to_string()),
        };

        // Test all cases
        for (name, entry) in [
            ("all_none", entry_all_none),
            ("all_some", entry_all_some),
            ("mixed", entry_mixed),
        ] {
            let serialized = postcard::to_allocvec(&entry)
                .unwrap_or_else(|e| panic!("Failed to serialize {name}: {e}"));
            let deserialized: ConfigEntry = postcard::from_bytes(&serialized)
                .unwrap_or_else(|e| panic!("Failed to deserialize {name}: {e}"));

            assert_eq!(
                entry,
                deserialized,
                "Roundtrip failed for {name}: serialized {len} bytes",
                len = serialized.len()
            );
        }
    }

    /// Test `ConfigSnapshot` postcard roundtrip (embedded in legacy index metadata).
    #[test]
    fn test_config_snapshot_postcard_roundtrip() {
        let mut snapshot = ConfigSnapshot::new();

        // Add entries with various optional field combinations
        snapshot.add_entry(
            ConfigEntry::new(
                "SETTING_1",
                "value1",
                "default1",
                ConfigSource::Default,
                ConfigScope::Global,
            )
            .with_range("0", "100")
            .with_risk(ConfigRisk::Dos)
            .with_description("First setting"),
        );

        snapshot.add_entry(ConfigEntry::new(
            "SETTING_2",
            "value2",
            "default2",
            ConfigSource::Env,
            ConfigScope::Project,
        )); // No optional fields set

        snapshot.add_entry(
            ConfigEntry::new(
                "SETTING_3",
                "value3",
                "default3",
                ConfigSource::ProjectConfig,
                ConfigScope::Global,
            )
            .with_risk(ConfigRisk::Perf), // Only risk set
        );

        let serialized = postcard::to_allocvec(&snapshot).expect("Failed to serialize snapshot");
        let deserialized: ConfigSnapshot =
            postcard::from_bytes(&serialized).expect("Failed to deserialize snapshot");

        assert_eq!(snapshot.schema_version, deserialized.schema_version);
        assert_eq!(snapshot.entries.len(), deserialized.entries.len());

        for (original, restored) in snapshot.entries.iter().zip(deserialized.entries.iter()) {
            assert_eq!(original, restored, "Entry mismatch for {}", original.name);
        }
    }
}
