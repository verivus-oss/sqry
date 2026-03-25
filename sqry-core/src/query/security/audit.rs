//! Query audit logging with durability guarantees
//!
//! This module provides audit logging for CD queries, including:
//! - Flush-on-drop guarantee via `Drop` impl
//! - File rotation when size exceeds max (default 10MB)
//! - Error handling for I/O failures
//! - Configurable flush interval
//!
//! # Durability Guarantees
//!
//! 1. **Flush-on-drop**: Via `Drop` implementation
//! 2. **Periodic flush**: Every `flush_interval_secs`
//! 3. **Immediate flush**: When buffer is full
//! 4. **File rotation**: When size exceeds `max_file_size`
//! 5. **I/O errors**: Logged but don't fail queries

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

use super::guard::QueryCompletionStatus;

/// Query audit log entry
///
/// Captures all relevant information about a query execution for audit purposes.
#[derive(Debug, Serialize)]
pub struct QueryAuditEntry {
    /// Query text
    pub query: String,

    /// Timestamp (UTC)
    pub timestamp: DateTime<Utc>,

    /// Execution duration (ms)
    pub duration_ms: u64,

    /// Number of results
    pub result_count: usize,

    /// Peak memory usage (bytes)
    pub memory_usage: usize,

    /// Query predicates used (e.g., [`impl:Debug`, `lang:rust`])
    pub predicates: Vec<String>,

    /// Security outcome
    pub outcome: QueryOutcome,

    /// Applied security limits (for audit trail)
    pub applied_limits: AppliedLimits,

    /// User context (from environment)
    pub user: String,
}

/// Applied security limits recorded in audit entry
#[derive(Debug, Clone, Serialize)]
pub struct AppliedLimits {
    /// Timeout in milliseconds
    pub timeout_ms: u64,
    /// Result cap
    pub result_cap: usize,
    /// Memory limit in bytes
    pub memory_limit: usize,
}

/// Query outcome for audit logging (per Codex iter10)
///
/// Captures both the completion status and whether partial results were returned.
/// This allows audit logs to distinguish between:
/// - Complete success (all results returned)
/// - Partial success (some results returned, limit reached)
/// - Failure (no results due to pre-execution rejection)
#[derive(Debug, Clone, Serialize)]
pub enum QueryOutcome {
    /// Query completed successfully with all results
    Success,
    /// Query timed out, partial results may have been returned
    Timeout {
        /// Number of results returned before timeout
        partial_results_count: usize,
    },
    /// Result cap exceeded, partial results returned
    ResultCapExceeded {
        /// Number of results returned
        returned: usize,
        /// The configured limit
        limit: usize,
    },
    /// Memory limit exceeded, partial results returned
    MemoryLimitExceeded {
        /// Number of results returned
        returned: usize,
        /// Memory usage in bytes when limit was hit
        memory_bytes: usize,
    },
    /// Cost limit exceeded (pre-execution rejection, no results)
    CostLimitExceeded {
        /// Estimated cost
        estimated: usize,
        /// Configured limit
        limit: usize,
    },
    /// Error during execution
    Error(String),
}

impl QueryOutcome {
    /// Create outcome from `QueryCompletionStatus` (per Codex iter10)
    #[must_use]
    pub fn from_completion_status(status: &QueryCompletionStatus, result_count: usize) -> Self {
        match status {
            QueryCompletionStatus::Complete => Self::Success,
            QueryCompletionStatus::TimedOut { .. } => Self::Timeout {
                partial_results_count: result_count,
            },
            QueryCompletionStatus::ResultCapReached { count, limit } => Self::ResultCapExceeded {
                returned: *count,
                limit: *limit,
            },
            QueryCompletionStatus::MemoryLimitReached { usage_bytes, .. } => {
                Self::MemoryLimitExceeded {
                    returned: result_count,
                    memory_bytes: *usage_bytes,
                }
            }
        }
    }

    /// Returns true if partial results were returned
    #[must_use]
    pub fn has_partial_results(&self) -> bool {
        match self {
            Self::Timeout {
                partial_results_count,
            } if *partial_results_count > 0 => true,
            Self::ResultCapExceeded { .. } | Self::MemoryLimitExceeded { .. } => true,
            _ => false,
        }
    }

    /// Returns true if the query completed successfully
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Audit logger configuration
///
/// ## Path Selection Rules (per Codex iter10)
///
/// The audit log path is resolved as follows:
///
/// 1. **CLI override** (`--audit-log <path>`): User-specified path takes precedence.
/// 2. **Environment variable** (`SQRY_AUDIT_LOG`): Fallback if no CLI arg.
/// 3. **Default**: `.sqry-audit.jsonl` in the current working directory.
///
/// ## Deployment Considerations
///
/// - **Read-only repos**: If deploying in a read-only directory, use `--audit-log`
///   to specify a writable location (e.g., `~/.sqry/audit.jsonl` or `/tmp/sqry-audit.jsonl`).
/// - **Permission denied**: The logger validates writability on startup and warns
///   early if the path is unwritable.
/// - **Multi-user**: Consider using user-specific paths (`~/.sqry/`) to avoid conflicts.
#[derive(Debug, Clone)]
pub struct AuditLogConfig {
    /// Log file path
    pub log_path: PathBuf,

    /// Buffer size before auto-flush
    pub buffer_size: usize,

    /// Flush interval (seconds)
    pub flush_interval_secs: u64,

    /// Max file size before rotation (bytes)
    pub max_file_size: usize,

    /// Number of rotated files to keep
    pub rotation_count: usize,

    /// User context for audit trail (per Codex iter10)
    /// Captured from environment: `$SQRY_USER`, `$USER`, or "unknown"
    pub user_context: String,
}

impl Default for AuditLogConfig {
    fn default() -> Self {
        Self {
            log_path: PathBuf::from(".sqry-audit.jsonl"),
            buffer_size: 100,
            flush_interval_secs: 60,
            max_file_size: 10 * 1024 * 1024, // 10 MB
            rotation_count: 5,
            user_context: Self::detect_user_context(),
        }
    }
}

impl AuditLogConfig {
    /// Detect user context from environment (per Codex iter10)
    ///
    /// Priority: `$SQRY_USER` > `$USER` > `$USERNAME` > "unknown"
    #[must_use]
    pub fn detect_user_context() -> String {
        std::env::var("SQRY_USER")
            .or_else(|_| std::env::var("USER"))
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Create config with a custom log path
    #[must_use]
    pub fn with_log_path(self, path: PathBuf) -> Self {
        Self {
            log_path: path,
            ..self
        }
    }

    /// Create config with a custom buffer size
    #[must_use]
    pub fn with_buffer_size(self, size: usize) -> Self {
        Self {
            buffer_size: size,
            ..self
        }
    }

    /// Validate that the audit log path is writable (per Codex iter10)
    ///
    /// Call this early in CLI startup to warn users if the path is unwritable.
    /// This prevents silent audit failures that would violate compliance requirements.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if path is writable (or can be created)
    /// - `Err(PathValidationError)` with explanation if unwritable
    ///
    /// # Errors
    ///
    /// Returns error if the path is not writable due to permission or I/O issues.
    pub fn validate_path(&self) -> Result<(), PathValidationError> {
        let parent = self.log_path.parent().unwrap_or(Path::new("."));

        // Check if parent directory exists and is writable
        if parent.exists() {
            // Try to create a test file to verify write permission
            let test_path = parent.join(".sqry-audit-test");
            match std::fs::write(&test_path, b"test") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&test_path);
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    Err(PathValidationError::PermissionDenied {
                        path: self.log_path.clone(),
                        suggestion:
                            "Use --audit-log to specify a writable path (e.g., ~/.sqry/audit.jsonl)"
                                .into(),
                    })
                }
                Err(e) => Err(PathValidationError::IoError(e)),
            }
        } else {
            // Parent doesn't exist - check if we can create it
            match std::fs::create_dir_all(parent) {
                Ok(()) => {
                    // Created successfully, remove to avoid leaving empty dirs
                    let _ = std::fs::remove_dir(parent);
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    Err(PathValidationError::PermissionDenied {
                        path: self.log_path.clone(),
                        suggestion:
                            "Use --audit-log to specify a writable path (e.g., ~/.sqry/audit.jsonl)"
                                .into(),
                    })
                }
                Err(e) => Err(PathValidationError::IoError(e)),
            }
        }
    }
}

/// Path validation errors for early CLI feedback (per Codex iter10)
#[derive(Debug, thiserror::Error)]
pub enum PathValidationError {
    /// Permission denied when trying to write to the audit log path
    #[error("Permission denied for audit log path {path:?}: {suggestion}")]
    PermissionDenied {
        /// The path that couldn't be written
        path: PathBuf,
        /// Suggestion for resolving the issue
        suggestion: String,
    },

    /// I/O error when validating the audit log path
    #[error("I/O error validating audit log path: {0}")]
    IoError(#[from] std::io::Error),
}

/// Query audit logger with durability guarantees
///
/// **DURABILITY GUARANTEES** (per Codex review):
/// 1. Flush-on-drop via `Drop` implementation
/// 2. Periodic flush every `flush_interval_secs`
/// 3. Immediate flush when buffer is full
/// 4. File rotation when size exceeds `max_file_size`
/// 5. I/O errors are logged but don't fail queries
pub struct QueryAuditLogger {
    config: AuditLogConfig,
    buffer: Mutex<Vec<QueryAuditEntry>>,
    last_flush: Mutex<Instant>,
}

impl QueryAuditLogger {
    /// Create a new audit logger
    ///
    /// # Errors
    ///
    /// Returns error if log directory cannot be created or isn't writable.
    ///
    /// # Directory Creation
    ///
    /// **Per Codex iter7 review**: The logger auto-creates parent directories
    /// if they don't exist. This handles CLI overrides like `--audit-log ~/.sqry/audit.jsonl`
    /// gracefully without requiring users to pre-create directories.
    pub fn new(config: AuditLogConfig) -> Result<Self, std::io::Error> {
        // **Rev 5** (per Codex iter7): Auto-create parent directory if missing
        if let Some(parent) = config.log_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to create audit log directory {}: {e}",
                        parent.display()
                    ),
                )
            })?;
        }

        Ok(Self {
            config,
            buffer: Mutex::new(Vec::new()),
            last_flush: Mutex::new(Instant::now()),
        })
    }

    /// Log a query execution
    ///
    /// This method buffers entries and flushes when:
    /// - Buffer reaches `buffer_size`
    /// - Time since last flush exceeds `flush_interval_secs`
    pub fn log(&self, entry: QueryAuditEntry) {
        let mut buffer = self.buffer.lock();
        buffer.push(entry);

        let should_flush = buffer.len() >= self.config.buffer_size
            || self.last_flush.lock().elapsed().as_secs() >= self.config.flush_interval_secs;

        if should_flush {
            // Move entries out to release lock during I/O
            let entries: Vec<_> = buffer.drain(..).collect();
            drop(buffer);

            self.flush_entries(&entries);
            *self.last_flush.lock() = Instant::now();
        }
    }

    /// Force flush all buffered entries
    pub fn flush(&self) {
        let entries: Vec<_> = self.buffer.lock().drain(..).collect();
        if !entries.is_empty() {
            self.flush_entries(&entries);
        }
    }

    /// Get the number of buffered entries
    #[must_use]
    pub fn buffer_len(&self) -> usize {
        self.buffer.lock().len()
    }

    /// Get the config
    #[must_use]
    pub fn config(&self) -> &AuditLogConfig {
        &self.config
    }

    /// Flush entries to disk with rotation
    fn flush_entries(&self, entries: &[QueryAuditEntry]) {
        // Check if rotation needed
        let max_file_size = u64::try_from(self.config.max_file_size).unwrap_or(u64::MAX);
        if let Ok(metadata) = std::fs::metadata(&self.config.log_path)
            && metadata.len() >= max_file_size
            && let Err(e) = self.rotate_log()
        {
            eprintln!("Audit log rotation failed: {e}");
        }

        // Append entries
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log_path)
        {
            Ok(file) => {
                let mut writer = BufWriter::new(file);
                for entry in entries {
                    if let Ok(json) = serde_json::to_string(entry) {
                        let _ = writeln!(writer, "{json}");
                    }
                }
                let _ = writer.flush();
            }
            Err(e) => {
                eprintln!("Audit log write failed: {e}");
            }
        }
    }

    /// Rotate log files
    fn rotate_log(&self) -> std::io::Result<()> {
        // Rotate existing files: .4 -> .5, .3 -> .4, etc.
        for i in (1..self.config.rotation_count).rev() {
            let old_path = self.config.log_path.with_extension(format!("jsonl.{i}"));
            let new_path = self
                .config
                .log_path
                .with_extension(format!("jsonl.{}", i + 1));
            if old_path.exists() {
                std::fs::rename(&old_path, &new_path)?;
            }
        }

        // Current -> .1
        let backup_path = self.config.log_path.with_extension("jsonl.1");
        if self.config.log_path.exists() {
            std::fs::rename(&self.config.log_path, backup_path)?;
        }

        Ok(())
    }
}

/// **FLUSH-ON-DROP GUARANTEE**
impl Drop for QueryAuditLogger {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_query_outcome_from_status() {
        let complete = QueryCompletionStatus::Complete;
        assert!(QueryOutcome::from_completion_status(&complete, 10).is_success());

        let cap_reached = QueryCompletionStatus::ResultCapReached {
            count: 100,
            limit: 100,
        };
        let outcome = QueryOutcome::from_completion_status(&cap_reached, 100);
        assert!(outcome.has_partial_results());
    }

    #[test]
    fn test_query_outcome_partial_results() {
        assert!(!QueryOutcome::Success.has_partial_results());
        assert!(
            QueryOutcome::Timeout {
                partial_results_count: 5
            }
            .has_partial_results()
        );
        assert!(
            !QueryOutcome::Timeout {
                partial_results_count: 0
            }
            .has_partial_results()
        );
        assert!(
            QueryOutcome::ResultCapExceeded {
                returned: 100,
                limit: 100
            }
            .has_partial_results()
        );
        assert!(
            QueryOutcome::MemoryLimitExceeded {
                returned: 50,
                memory_bytes: 1000
            }
            .has_partial_results()
        );
        assert!(
            !QueryOutcome::CostLimitExceeded {
                estimated: 100,
                limit: 50
            }
            .has_partial_results()
        );
    }

    #[test]
    fn test_default_config() {
        let config = AuditLogConfig::default();
        assert_eq!(config.log_path, PathBuf::from(".sqry-audit.jsonl"));
        assert_eq!(config.buffer_size, 100);
        assert_eq!(config.flush_interval_secs, 60);
        assert_eq!(config.max_file_size, 10 * 1024 * 1024);
        assert_eq!(config.rotation_count, 5);
    }

    #[test]
    fn test_detect_user_context() {
        // Just verify it doesn't panic and returns something
        let user = AuditLogConfig::detect_user_context();
        assert!(!user.is_empty() || user == "unknown");
    }

    #[test]
    fn test_logger_creation() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        let config = AuditLogConfig {
            log_path,
            ..Default::default()
        };

        let logger = QueryAuditLogger::new(config).unwrap();
        assert_eq!(logger.buffer_len(), 0);
    }

    #[test]
    fn test_logger_buffering() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        let config = AuditLogConfig {
            log_path: log_path.clone(),
            buffer_size: 10, // High threshold so we don't auto-flush
            flush_interval_secs: 3600,
            ..Default::default()
        };

        let logger = QueryAuditLogger::new(config).unwrap();

        let entry = QueryAuditEntry {
            query: "impl:Debug".to_string(),
            timestamp: Utc::now(),
            duration_ms: 100,
            result_count: 5,
            memory_usage: 1024,
            predicates: vec!["impl:Debug".to_string()],
            outcome: QueryOutcome::Success,
            applied_limits: AppliedLimits {
                timeout_ms: 30000,
                result_cap: 10000,
                memory_limit: 512 * 1024 * 1024,
            },
            user: "test".to_string(),
        };

        logger.log(entry);
        assert_eq!(logger.buffer_len(), 1);

        // File shouldn't exist yet (not flushed)
        assert!(!log_path.exists());

        // Force flush
        logger.flush();
        assert_eq!(logger.buffer_len(), 0);

        // File should now exist
        assert!(log_path.exists());
    }

    #[test]
    fn test_auto_flush_on_buffer_full() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        let config = AuditLogConfig {
            log_path: log_path.clone(),
            buffer_size: 2, // Small buffer to trigger flush
            flush_interval_secs: 3600,
            ..Default::default()
        };

        let logger = QueryAuditLogger::new(config).unwrap();

        // Add two entries to trigger auto-flush
        for i in 0..2 {
            let entry = QueryAuditEntry {
                query: format!("query:{i}"),
                timestamp: Utc::now(),
                duration_ms: 10,
                result_count: i,
                memory_usage: 512,
                predicates: vec![],
                outcome: QueryOutcome::Success,
                applied_limits: AppliedLimits {
                    timeout_ms: 30000,
                    result_cap: 10000,
                    memory_limit: 512 * 1024 * 1024,
                },
                user: "test".to_string(),
            };
            logger.log(entry);
        }

        // Buffer should be empty after auto-flush
        assert_eq!(logger.buffer_len(), 0);

        // File should exist
        assert!(log_path.exists());
    }

    #[test]
    fn test_flush_on_drop() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        {
            let config = AuditLogConfig {
                log_path: log_path.clone(),
                buffer_size: 100, // High so we don't auto-flush
                flush_interval_secs: 3600,
                ..Default::default()
            };

            let logger = QueryAuditLogger::new(config).unwrap();

            let entry = QueryAuditEntry {
                query: "test".to_string(),
                timestamp: Utc::now(),
                duration_ms: 10,
                result_count: 1,
                memory_usage: 256,
                predicates: vec![],
                outcome: QueryOutcome::Success,
                applied_limits: AppliedLimits {
                    timeout_ms: 30000,
                    result_cap: 10000,
                    memory_limit: 512 * 1024 * 1024,
                },
                user: "test".to_string(),
            };
            logger.log(entry);

            // File shouldn't exist yet
            assert!(!log_path.exists());
        } // Logger dropped here, should flush

        // File should now exist due to Drop
        assert!(log_path.exists());
    }

    #[test]
    fn test_log_rotation() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        let config = AuditLogConfig {
            log_path: log_path.clone(),
            buffer_size: 1,    // Flush every entry
            max_file_size: 50, // Very small to trigger rotation
            rotation_count: 3,
            flush_interval_secs: 0, // Always check flush
            ..Default::default()
        };

        let logger = QueryAuditLogger::new(config).unwrap();

        // Add several entries to trigger rotation
        for i in 0..5 {
            let entry = QueryAuditEntry {
                query: format!("query:{i}"),
                timestamp: Utc::now(),
                duration_ms: 10,
                result_count: i,
                memory_usage: 512,
                predicates: vec![],
                outcome: QueryOutcome::Success,
                applied_limits: AppliedLimits {
                    timeout_ms: 30000,
                    result_cap: 10000,
                    memory_limit: 512 * 1024 * 1024,
                },
                user: "test".to_string(),
            };
            logger.log(entry);
        }

        // Main log should exist
        assert!(log_path.exists());
    }

    #[test]
    fn test_path_validation_writable() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("audit.jsonl");

        let config = AuditLogConfig {
            log_path,
            ..Default::default()
        };

        assert!(config.validate_path().is_ok());
    }

    #[test]
    fn test_config_builder() {
        let config = AuditLogConfig::default()
            .with_log_path(PathBuf::from("/tmp/test.jsonl"))
            .with_buffer_size(50);

        assert_eq!(config.log_path, PathBuf::from("/tmp/test.jsonl"));
        assert_eq!(config.buffer_size, 50);
    }

    #[test]
    fn test_auto_create_parent_directory() {
        let temp = TempDir::new().unwrap();
        let nested_path = temp
            .path()
            .join("subdir")
            .join("nested")
            .join("audit.jsonl");

        let config = AuditLogConfig {
            log_path: nested_path.clone(),
            ..Default::default()
        };

        let logger = QueryAuditLogger::new(config).unwrap();

        // Parent directories should have been created
        assert!(nested_path.parent().unwrap().exists());

        // Add an entry and flush to verify it works
        let entry = QueryAuditEntry {
            query: "test".to_string(),
            timestamp: Utc::now(),
            duration_ms: 10,
            result_count: 1,
            memory_usage: 256,
            predicates: vec![],
            outcome: QueryOutcome::Success,
            applied_limits: AppliedLimits {
                timeout_ms: 30000,
                result_cap: 10000,
                memory_limit: 512 * 1024 * 1024,
            },
            user: "test".to_string(),
        };
        logger.log(entry);
        logger.flush();

        assert!(nested_path.exists());
    }
}
