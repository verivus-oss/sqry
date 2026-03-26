//! Graph config persistence - atomic IO, integrity, recovery, and locking.
//!
//! Implements Step 3 of the Unified Graph Config Partition feature:
//! - Atomic write protocol (crash-safe)
//! - Integrity hashing with blake3
//! - Recovery from corrupt/missing config files
//! - Advisory file locking for writers
//!
//! # Write Protocol
//!
//! 1. Acquire lock
//! 2. Read current config (if present)
//! 3. Apply mutation
//! 4. Serialize with deterministic ordering
//! 5. Write to temp file + fsync
//! 6. Rename current to `.previous`
//! 7. Rename temp to current (atomic)
//! 8. Fsync directory (where supported)
//! 9. Release lock
//!
//! # Recovery Protocol
//!
//! 1. Try to load `config.json`
//! 2. If corrupt, quarantine and try `.previous`
//! 3. If both fail, return error requiring explicit action
//!
//! # Design
//!
//! See: `docs/development/unified-graph-config-partition/02_DESIGN.md`

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use fs2::FileExt;
use serde_json;
use thiserror::Error;

use super::graph_config_schema::{GraphConfigFile, SCHEMA_VERSION};
use super::graph_config_store::{GraphConfigPaths, GraphConfigStore};

/// Errors that can occur during config persistence operations
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// IO error
    #[error("IO error at {path}: {source}")]
    IoError {
        /// Path where the IO error occurred
        path: PathBuf,
        /// The underlying IO error
        #[source]
        source: std::io::Error,
    },

    /// Failed to serialize config
    #[error("Failed to serialize config: {0}")]
    SerializationError(String),

    /// Failed to deserialize config
    #[error("Failed to deserialize config: {0}")]
    DeserializationError(String),

    /// Lock acquisition failed
    #[error("Failed to acquire lock at {path} within {timeout_ms}ms")]
    LockTimeout {
        /// Path to the lock file
        path: PathBuf,
        /// Timeout duration in milliseconds
        timeout_ms: u64,
    },

    /// Lock file is stale
    #[error("Stale lock detected at {path}: {details}")]
    StaleLock {
        /// Path to the stale lock file
        path: PathBuf,
        /// Details about why the lock is considered stale
        details: String,
    },

    /// Config file is corrupt
    #[error("Corrupt config file at {path}: {reason}")]
    CorruptConfig {
        /// Path to the corrupt config file
        path: PathBuf,
        /// Reason why the config is considered corrupt
        reason: String,
    },

    /// No usable config found after recovery attempts
    #[error("No usable config found: {reason}")]
    NoUsableConfig {
        /// Reason why no usable config was found
        reason: String,
    },

    /// Integrity mismatch
    #[error("Integrity mismatch: expected {expected}, found {found}")]
    IntegrityMismatch {
        /// Expected hash value
        expected: String,
        /// Actual hash value found
        found: String,
    },
}

/// Result type for persistence operations
pub type PersistenceResult<T> = Result<T, PersistenceError>;

/// Status of integrity verification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityStatus {
    /// Hash matches
    Ok,
    /// Hash doesn't match (possibly manual edit)
    Mismatch,
    /// No hash available
    Unavailable,
}

/// Status of schema validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStatus {
    /// Schema is valid
    Ok,
    /// Schema is invalid
    Invalid,
}

/// Report from loading a config file
#[derive(Debug, Clone)]
pub struct LoadReport {
    /// Warnings encountered during load
    pub warnings: Vec<String>,
    /// Recovery actions taken
    pub recovery_actions: Vec<String>,
    /// Integrity verification status
    pub integrity_status: IntegrityStatus,
    /// Schema validation status
    pub schema_status: SchemaStatus,
}

impl Default for LoadReport {
    fn default() -> Self {
        Self {
            warnings: Vec::new(),
            recovery_actions: Vec::new(),
            integrity_status: IntegrityStatus::Unavailable,
            schema_status: SchemaStatus::Ok,
        }
    }
}

/// Lock file content for diagnostics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LockInfo {
    /// Process ID of lock holder
    pub pid: u32,
    /// Hostname of lock holder
    pub hostname: String,
    /// When the lock was acquired
    pub acquired_at_utc: String,
    /// Tool that acquired the lock (cli/lsp/mcp)
    pub tool: String,
    /// Optional command being executed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Default for LockInfo {
    fn default() -> Self {
        Self {
            pid: std::process::id(),
            hostname: hostname::get().map_or_else(
                |_| "unknown".to_string(),
                |h| h.to_string_lossy().to_string(),
            ),
            acquired_at_utc: Utc::now().to_rfc3339(),
            tool: "cli".to_string(),
            command: None,
        }
    }
}

/// Configuration persistence manager
///
/// Provides atomic load/save operations with integrity verification
/// and recovery support.
pub struct ConfigPersistence {
    paths: GraphConfigPaths,
}

impl ConfigPersistence {
    /// Create a new persistence manager from a config store
    #[must_use]
    pub fn new(store: &GraphConfigStore) -> Self {
        Self {
            paths: store.paths().clone(),
        }
    }

    /// Create a new persistence manager from paths
    #[must_use]
    pub fn from_paths(paths: GraphConfigPaths) -> Self {
        Self { paths }
    }

    // ========================================================================
    // Load operations
    // ========================================================================

    /// Load config with recovery support
    ///
    /// Returns the loaded config and a report of any recovery actions taken.
    ///
    /// # Errors
    ///
    /// Returns an error if no usable config file can be loaded or parsed.
    pub fn load(&self) -> PersistenceResult<(GraphConfigFile, LoadReport)> {
        let mut report = LoadReport::default();

        // Try loading primary config file
        let config_path = self.paths.config_file();
        match Self::try_load_file(&config_path) {
            Ok((config, file_report)) => {
                report.warnings.extend(file_report.warnings);
                report.integrity_status = file_report.integrity_status;
                report.schema_status = file_report.schema_status;
                return Ok((config, report));
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("Failed to load config.json: {e}"));
            }
        }

        // Try loading previous config
        let previous_path = self.paths.previous_file();
        if previous_path.exists() {
            report
                .recovery_actions
                .push("Attempting to load config.json.previous".to_string());

            match Self::try_load_file(&previous_path) {
                Ok((config, file_report)) => {
                    report.warnings.extend(file_report.warnings);
                    report.integrity_status = file_report.integrity_status;
                    report.schema_status = file_report.schema_status;
                    report
                        .recovery_actions
                        .push("Recovered from config.json.previous".to_string());
                    return Ok((config, report));
                }
                Err(e) => {
                    report
                        .warnings
                        .push(format!("Failed to load config.json.previous: {e}"));
                }
            }
        }

        // No usable config found
        Err(PersistenceError::NoUsableConfig {
            reason: "Neither config.json nor config.json.previous could be loaded. \
                     Run `sqry config init` to create a new config file."
                .to_string(),
        })
    }

    /// Try to load a specific config file
    fn try_load_file(path: &Path) -> PersistenceResult<(GraphConfigFile, LoadReport)> {
        let mut report = LoadReport::default();

        if !path.exists() {
            return Err(PersistenceError::IoError {
                path: path.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "File not found"),
            });
        }

        // Read file content
        let content = fs::read_to_string(path).map_err(|e| PersistenceError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Parse JSON
        let config: GraphConfigFile = serde_json::from_str(&content)
            .map_err(|e| PersistenceError::DeserializationError(e.to_string()))?;

        // Validate schema version
        if config.schema_version != SCHEMA_VERSION {
            report.schema_status = SchemaStatus::Invalid;
            return Err(PersistenceError::CorruptConfig {
                path: path.to_path_buf(),
                reason: format!(
                    "Incompatible schema version: expected {}, found {}",
                    SCHEMA_VERSION, config.schema_version
                ),
            });
        }

        // Verify integrity
        let computed_hash = Self::compute_integrity_hash(&config.config)?;
        if config.integrity.normalized_hash.is_empty() {
            report.integrity_status = IntegrityStatus::Unavailable;
        } else if config.integrity.normalized_hash != computed_hash {
            report.integrity_status = IntegrityStatus::Mismatch;
            report.warnings.push(format!(
                "Integrity hash mismatch (possibly manual edit). \
                 Expected: {}, Found: {}",
                config.integrity.normalized_hash, computed_hash
            ));
        } else {
            report.integrity_status = IntegrityStatus::Ok;
        }

        Ok((config, report))
    }

    /// Check if config exists (either primary or previous)
    #[must_use]
    pub fn exists(&self) -> bool {
        self.paths.config_file().exists() || self.paths.previous_file().exists()
    }

    // ========================================================================
    // Save operations
    // ========================================================================

    /// Save config atomically with locking
    ///
    /// # Arguments
    ///
    /// * `config` - The config to save
    /// * `lock_timeout_ms` - How long to wait for the lock
    /// * `tool` - The tool performing the save (for lock info)
    ///
    /// # Errors
    ///
    /// Returns an error if locking, serialization, or atomic write fails.
    pub fn save(
        &self,
        config: &mut GraphConfigFile,
        lock_timeout_ms: u64,
        tool: &str,
    ) -> PersistenceResult<()> {
        // Ensure config directory exists
        let config_dir = self.paths.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).map_err(|e| PersistenceError::IoError {
                path: config_dir.clone(),
                source: e,
            })?;
        }

        // Acquire lock
        let lock_guard = self.acquire_lock(lock_timeout_ms, tool)?;

        // Update metadata
        config.metadata.updated_at = Utc::now().to_rfc3339();

        // Compute integrity hash
        let hash = Self::compute_integrity_hash(&config.config)?;
        config.integrity.normalized_hash = hash;
        config.integrity.last_verified_at = Utc::now().to_rfc3339();

        // Serialize with deterministic ordering
        let json = serde_json::to_string_pretty(config)
            .map_err(|e| PersistenceError::SerializationError(e.to_string()))?;

        // Atomic write protocol
        self.atomic_write(&json)?;

        // Lock is released when guard is dropped
        drop(lock_guard);

        Ok(())
    }

    /// Initialize a new config file with defaults
    ///
    /// # Errors
    ///
    /// Returns an error if the config cannot be saved.
    pub fn init(&self, lock_timeout_ms: u64, tool: &str) -> PersistenceResult<GraphConfigFile> {
        let mut config = GraphConfigFile::default();
        self.save(&mut config, lock_timeout_ms, tool)?;
        Ok(config)
    }

    // ========================================================================
    // Atomic write implementation
    // ========================================================================

    /// Perform atomic write: temp + fsync + rename protocol
    fn atomic_write(&self, content: &str) -> PersistenceResult<()> {
        let config_path = self.paths.config_file();
        let config_dir = self.paths.config_dir();

        // Generate unique temp file name
        let temp_name = format!(
            "config.json.tmp.{}.{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let temp_path = config_dir.join(&temp_name);

        // Write to temp file
        let mut temp_file = File::create(&temp_path).map_err(|e| PersistenceError::IoError {
            path: temp_path.clone(),
            source: e,
        })?;

        temp_file
            .write_all(content.as_bytes())
            .map_err(|e| PersistenceError::IoError {
                path: temp_path.clone(),
                source: e,
            })?;

        // Fsync temp file
        temp_file
            .sync_all()
            .map_err(|e| PersistenceError::IoError {
                path: temp_path.clone(),
                source: e,
            })?;

        drop(temp_file);

        // If config.json exists, rename to .previous
        if config_path.exists() {
            let previous_path = self.paths.previous_file();
            fs::rename(&config_path, &previous_path).map_err(|e| PersistenceError::IoError {
                path: config_path.clone(),
                source: e,
            })?;
        }

        // Rename temp to config.json (atomic on POSIX)
        fs::rename(&temp_path, &config_path).map_err(|e| PersistenceError::IoError {
            path: temp_path.clone(),
            source: e,
        })?;

        // Fsync directory (best-effort on some platforms)
        Self::fsync_dir(&config_dir)?;

        Ok(())
    }

    /// Fsync directory for rename durability
    #[cfg(unix)]
    fn fsync_dir(dir: &Path) -> PersistenceResult<()> {
        let dir_file = File::open(dir).map_err(|e| PersistenceError::IoError {
            path: dir.to_path_buf(),
            source: e,
        })?;

        dir_file.sync_all().map_err(|e| PersistenceError::IoError {
            path: dir.to_path_buf(),
            source: e,
        })?;

        Ok(())
    }

    #[cfg(not(unix))]
    fn fsync_dir(_dir: &Path) -> PersistenceResult<()> {
        // Directory fsync is not reliably available on all platforms
        // Fall back to file fsync and document the limitation
        Ok(())
    }

    // ========================================================================
    // Integrity hashing
    // ========================================================================

    /// Compute integrity hash of the config section
    fn compute_integrity_hash(
        config: &super::graph_config_schema::GraphConfig,
    ) -> PersistenceResult<String> {
        // Serialize config section deterministically
        let json = serde_json::to_string(config)
            .map_err(|e| PersistenceError::SerializationError(e.to_string()))?;

        // Compute blake3 hash
        let hash = blake3::hash(json.as_bytes());
        Ok(hash.to_hex().to_string())
    }

    // ========================================================================
    // Locking
    // ========================================================================

    /// Acquire exclusive lock for write operations
    fn acquire_lock(&self, timeout_ms: u64, tool: &str) -> PersistenceResult<LockGuard> {
        let lock_path = self.paths.lock_file();

        // Ensure config directory exists
        let config_dir = self.paths.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).map_err(|e| PersistenceError::IoError {
                path: config_dir,
                source: e,
            })?;
        }

        // Open or create lock file
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| PersistenceError::IoError {
                path: lock_path.clone(),
                source: e,
            })?;

        // Try to acquire exclusive lock with timeout
        let timeout = Duration::from_millis(timeout_ms);
        let start = std::time::Instant::now();

        loop {
            if let Ok(()) = lock_file.try_lock_exclusive() {
                // Write lock info
                let lock_info = LockInfo {
                    tool: tool.to_string(),
                    ..Default::default()
                };
                let info_json =
                    serde_json::to_string_pretty(&lock_info).unwrap_or_else(|_| "{}".to_string());

                // Truncate and write lock info
                let _ = lock_file.set_len(0);
                let _ = (&lock_file).write_all(info_json.as_bytes());
                let _ = lock_file.sync_all();

                return Ok(LockGuard {
                    file: lock_file,
                    path: lock_path,
                });
            }
            if start.elapsed() >= timeout {
                return Err(PersistenceError::LockTimeout {
                    path: lock_path,
                    timeout_ms,
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // ========================================================================
    // Recovery operations
    // ========================================================================

    /// Quarantine a corrupt config file
    ///
    /// # Errors
    ///
    /// Returns an error if the corrupt file cannot be moved to quarantine.
    pub fn quarantine_corrupt(&self, path: &Path) -> PersistenceResult<PathBuf> {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let corrupt_path = self.paths.corrupt_file(&timestamp);

        fs::rename(path, &corrupt_path).map_err(|e| PersistenceError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

        Ok(corrupt_path)
    }

    /// Repair config by quarantining corrupt files and restoring from previous
    ///
    /// # Errors
    ///
    /// Returns an error if lock acquisition or file operations fail.
    pub fn repair(&self, lock_timeout_ms: u64) -> PersistenceResult<RepairReport> {
        let mut report = RepairReport::default();

        let _lock_guard = self.acquire_lock(lock_timeout_ms, "cli")?;

        let config_path = self.paths.config_file();
        let previous_path = self.paths.previous_file();

        // Check for temp artifacts
        let config_dir = self.paths.config_dir();
        if let Ok(entries) = fs::read_dir(&config_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("config.json.tmp.") {
                    let artifact_path = entry.path();
                    let quarantine_path = self.quarantine_corrupt(&artifact_path)?;
                    report.quarantined.push((artifact_path, quarantine_path));
                }
            }
        }

        // Check if config.json is valid
        if config_path.exists() {
            match Self::try_load_file(&config_path) {
                Ok(_) => {
                    report.config_status = "valid".to_string();
                }
                Err(e) => {
                    report.config_status = format!("corrupt: {e}");
                    let quarantine_path = self.quarantine_corrupt(&config_path)?;
                    report
                        .quarantined
                        .push((config_path.clone(), quarantine_path));

                    // Try to promote previous
                    if previous_path.exists() {
                        fs::rename(&previous_path, &config_path).map_err(|e| {
                            PersistenceError::IoError {
                                path: previous_path.clone(),
                                source: e,
                            }
                        })?;
                        report.restored_from_previous = true;
                    }
                }
            }
        } else if previous_path.exists() {
            // config.json missing but previous exists (power loss recovery)
            fs::rename(&previous_path, &config_path).map_err(|e| PersistenceError::IoError {
                path: previous_path.clone(),
                source: e,
            })?;
            report.restored_from_previous = true;
        }

        Ok(report)
    }
}

/// RAII guard for file lock
struct LockGuard {
    file: File,
    #[allow(dead_code)] // Retained for debugging and future error messages
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        // Don't delete lock file - it persists
    }
}

/// Report from repair operation
#[derive(Debug, Default)]
pub struct RepairReport {
    /// Status of config.json
    pub config_status: String,
    /// Files that were quarantined (original path, quarantine path)
    pub quarantined: Vec<(PathBuf, PathBuf)>,
    /// Whether config was restored from .previous
    pub restored_from_previous: bool,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_persistence() -> (TempDir, ConfigPersistence) {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();
        let persistence = ConfigPersistence::from_paths(paths);
        (temp, persistence)
    }

    #[test]
    fn test_init_creates_config() {
        let (_temp, persistence) = create_test_persistence();

        let config = persistence.init(5000, "test").unwrap();
        assert_eq!(config.schema_version, SCHEMA_VERSION);

        // Verify file exists
        assert!(persistence.paths.config_file().exists());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let (_temp, persistence) = create_test_persistence();

        // Create and save config
        let mut config = GraphConfigFile::default();
        config.config.limits.max_results = 12345;
        persistence.save(&mut config, 5000, "test").unwrap();

        // Load and verify
        let (loaded, report) = persistence.load().unwrap();
        assert_eq!(loaded.config.limits.max_results, 12345);
        assert_eq!(report.integrity_status, IntegrityStatus::Ok);
    }

    #[test]
    fn test_integrity_hash_computed() {
        let (_temp, persistence) = create_test_persistence();

        let mut config = GraphConfigFile::default();
        persistence.save(&mut config, 5000, "test").unwrap();

        // Hash should be populated after save
        assert!(!config.integrity.normalized_hash.is_empty());
    }

    #[test]
    fn test_previous_file_created_on_update() {
        let (_temp, persistence) = create_test_persistence();

        // Initial save
        let mut config = GraphConfigFile::default();
        config.config.limits.max_results = 100;
        persistence.save(&mut config, 5000, "test").unwrap();

        // Update
        config.config.limits.max_results = 200;
        persistence.save(&mut config, 5000, "test").unwrap();

        // Previous should exist
        assert!(persistence.paths.previous_file().exists());
    }

    #[test]
    fn test_load_nonexistent_returns_error() {
        let (_temp, persistence) = create_test_persistence();

        let result = persistence.load();
        assert!(result.is_err());
    }

    #[test]
    fn test_exists_false_when_no_config() {
        let (_temp, persistence) = create_test_persistence();
        assert!(!persistence.exists());
    }

    #[test]
    fn test_exists_true_after_init() {
        let (_temp, persistence) = create_test_persistence();
        persistence.init(5000, "test").unwrap();
        assert!(persistence.exists());
    }

    #[test]
    fn test_integrity_mismatch_warning() {
        let (_temp, persistence) = create_test_persistence();

        // Create config
        let mut config = GraphConfigFile::default();
        persistence.save(&mut config, 5000, "test").unwrap();

        // Manually modify the file to simulate manual edit
        let config_path = persistence.paths.config_file();
        let content = fs::read_to_string(&config_path).unwrap();
        let modified = content.replace("5000", "9999");
        fs::write(&config_path, modified).unwrap();

        // Load should succeed but report integrity mismatch
        let (_, report) = persistence.load().unwrap();
        assert_eq!(report.integrity_status, IntegrityStatus::Mismatch);
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn test_repair_promotes_previous_when_config_missing() {
        let (_temp, persistence) = create_test_persistence();

        // Create initial config
        let mut config = GraphConfigFile::default();
        config.config.limits.max_results = 42;
        persistence.save(&mut config, 5000, "test").unwrap();

        // Save again to create .previous (first config becomes previous)
        config.config.limits.max_results = 43;
        persistence.save(&mut config, 5000, "test").unwrap();

        // Verify previous exists
        assert!(persistence.paths.previous_file().exists());

        // Simulate power loss: delete config.json but keep previous
        fs::remove_file(persistence.paths.config_file()).unwrap();
        assert!(!persistence.paths.config_file().exists());
        assert!(persistence.paths.previous_file().exists());

        // Repair should promote previous
        let report = persistence.repair(5000).unwrap();
        assert!(report.restored_from_previous);
        assert!(persistence.paths.config_file().exists());
    }

    #[test]
    fn test_quarantine_corrupt_file() {
        let (_temp, persistence) = create_test_persistence();

        // Create a corrupt file
        fs::create_dir_all(persistence.paths.config_dir()).unwrap();
        let config_path = persistence.paths.config_file();
        fs::write(&config_path, "not valid json").unwrap();

        // Quarantine it
        let quarantine_path = persistence.quarantine_corrupt(&config_path).unwrap();
        assert!(!config_path.exists());
        assert!(quarantine_path.exists());
        assert!(
            quarantine_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("corrupt")
        );
    }

    #[test]
    fn test_lock_timeout() {
        let (_temp, persistence) = create_test_persistence();

        // First lock should succeed
        let lock1 = persistence.acquire_lock(5000, "test1").unwrap();

        // Second lock should timeout quickly
        let result = persistence.acquire_lock(100, "test2");
        assert!(matches!(result, Err(PersistenceError::LockTimeout { .. })));

        drop(lock1);
    }

    #[test]
    fn test_lock_released_on_drop() {
        let (_temp, persistence) = create_test_persistence();

        {
            let _lock = persistence.acquire_lock(5000, "test1").unwrap();
            // Lock held in this scope
        }

        // Lock should be released, second acquire should succeed
        let _lock2 = persistence.acquire_lock(5000, "test2").unwrap();
    }
}
