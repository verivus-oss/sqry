//! Query execution guard with timeout, memory, and resource limits
//!
//! This module provides the runtime guard that enforces security limits
//! during query execution. The guard tracks:
//! - Elapsed time vs timeout limit
//! - Result count vs result cap
//! - Memory usage vs memory limit
//!
//! All limits are NON-NEGOTIABLE per the security requirements.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::config::QuerySecurityConfig;

/// Query execution guard with timeout, memory, and resource limits
///
/// **MEMORY ENFORCEMENT** (per Codex review):
/// Uses a tracked allocation approach where memory usage is estimated
/// based on result sizes and checked at regular intervals.
///
/// # Example
///
/// ```
/// use sqry_core::query::security::{QuerySecurityConfig, QueryGuard};
///
/// let config = QuerySecurityConfig::default();
/// let guard = QueryGuard::new(config);
///
/// // During query execution:
/// guard.should_continue().expect("should not fail initially");
/// guard.record_result(128); // Record a result with estimated size
/// ```
pub struct QueryGuard {
    config: QuerySecurityConfig,
    start_time: Instant,
    result_count: AtomicUsize,
    memory_usage: AtomicUsize,
    check_interval: usize,
    checks_performed: AtomicUsize,
}

impl QueryGuard {
    /// Create a new query guard
    #[must_use]
    pub fn new(config: QuerySecurityConfig) -> Self {
        Self {
            config,
            start_time: Instant::now(),
            result_count: AtomicUsize::new(0),
            memory_usage: AtomicUsize::new(0),
            check_interval: 100, // Check every 100 results
            checks_performed: AtomicUsize::new(0),
        }
    }

    /// Create a new query guard with custom check interval
    ///
    /// The check interval controls how often memory limits are checked.
    /// Lower values mean more frequent checks but higher overhead.
    #[must_use]
    pub fn with_check_interval(config: QuerySecurityConfig, interval: usize) -> Self {
        Self {
            check_interval: interval.max(1), // At least check every result
            ..Self::new(config)
        }
    }

    /// Check if query should continue
    ///
    /// # Errors
    ///
    /// Returns `QuerySecurityError` if any limit is exceeded:
    /// - `Timeout`: Query has run longer than the configured timeout
    /// - `ResultCapExceeded`: More results collected than the result cap
    /// - `MemoryLimitExceeded`: Estimated memory usage exceeds limit
    ///
    /// **NOTE** (per Codex iter6): Uses getter methods since fields are private.
    pub fn should_continue(&self) -> Result<(), QuerySecurityError> {
        // Check timeout - use getter since field is private
        let elapsed = self.start_time.elapsed();
        let timeout_limit = self.config.timeout();
        if elapsed > timeout_limit {
            return Err(QuerySecurityError::Timeout {
                elapsed,
                limit: timeout_limit,
            });
        }

        // Check result cap - use getter since field is private
        let count = self.result_count.load(Ordering::Relaxed);
        let result_limit = self.config.result_cap();
        if count >= result_limit {
            return Err(QuerySecurityError::ResultCapExceeded {
                count,
                limit: result_limit,
            });
        }

        // Check memory (periodically to reduce overhead) - use getter
        let checks = self.checks_performed.fetch_add(1, Ordering::Relaxed);
        if checks.is_multiple_of(self.check_interval) {
            let usage = self.memory_usage.load(Ordering::Relaxed);
            let memory_limit = self.config.memory_limit();
            if usage >= memory_limit {
                return Err(QuerySecurityError::MemoryLimitExceeded {
                    usage,
                    limit: memory_limit,
                });
            }
        }

        Ok(())
    }

    /// Record a result and its estimated memory footprint
    ///
    /// This should be called for each result added to the result set.
    /// The estimated size should include:
    /// - The Node/TraitImpl struct size
    /// - String allocations for names, paths, etc.
    /// - Any metadata stored with the result
    pub fn record_result(&self, estimated_size: usize) {
        self.result_count.fetch_add(1, Ordering::Relaxed);
        self.memory_usage
            .fetch_add(estimated_size, Ordering::Relaxed);
    }

    /// Get elapsed time since query started
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get current result count
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.result_count.load(Ordering::Relaxed)
    }

    /// Get current memory usage estimate
    #[must_use]
    pub fn memory_usage(&self) -> usize {
        self.memory_usage.load(Ordering::Relaxed)
    }

    /// Get the security configuration
    #[must_use]
    pub fn config(&self) -> &QuerySecurityConfig {
        &self.config
    }
}

/// Security errors from query execution
#[derive(Debug, thiserror::Error)]
pub enum QuerySecurityError {
    /// Query execution exceeded the timeout limit
    #[error("Query timeout: {elapsed:?} exceeded {limit:?}")]
    Timeout {
        /// How long the query ran before being stopped
        elapsed: Duration,
        /// The configured timeout limit
        limit: Duration,
    },

    /// Query returned more results than the result cap
    #[error("Result cap exceeded: {count} >= {limit}")]
    ResultCapExceeded {
        /// Number of results collected
        count: usize,
        /// The configured result cap
        limit: usize,
    },

    /// Query memory usage exceeded the memory limit
    #[error("Memory limit exceeded: {usage} bytes >= {limit} bytes")]
    MemoryLimitExceeded {
        /// Estimated memory usage in bytes
        usage: usize,
        /// The configured memory limit in bytes
        limit: usize,
    },

    /// Pre-execution cost estimate exceeds the cost limit
    #[error("Query cost exceeds limit: {estimated} > {limit}")]
    CostLimitExceeded {
        /// Estimated cost of the query
        estimated: usize,
        /// The configured cost limit
        limit: usize,
    },
}

impl QuerySecurityError {
    /// Convert security error to completion status for partial results (per Codex iter10)
    ///
    /// When a limit is exceeded during execution, this converts the error
    /// into a status indicator that can be returned with partial results.
    #[must_use]
    pub fn into_completion_status(self) -> QueryCompletionStatus {
        match self {
            Self::Timeout { elapsed, limit } => QueryCompletionStatus::TimedOut { elapsed, limit },
            Self::ResultCapExceeded { count, limit } => {
                QueryCompletionStatus::ResultCapReached { count, limit }
            }
            Self::MemoryLimitExceeded { usage, limit } => {
                QueryCompletionStatus::MemoryLimitReached {
                    usage_bytes: usage,
                    limit_bytes: limit,
                }
            }
            Self::CostLimitExceeded { .. } =>
            // Cost limit is checked before execution, not during
            // If we somehow hit this, treat as complete (no partial results scenario)
            {
                QueryCompletionStatus::Complete
            }
        }
    }
}

/// Completion status for query results (per Codex iter10)
///
/// Indicates whether the result set is complete or was truncated due to limits.
/// This allows callers to know if they received all matching results or only
/// a partial set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCompletionStatus {
    /// All matching results returned
    Complete,

    /// Results truncated due to result cap (see count for how many were returned)
    ResultCapReached {
        /// Number of results returned
        count: usize,
        /// The configured result cap
        limit: usize,
    },

    /// Results truncated due to memory limit
    MemoryLimitReached {
        /// Actual memory usage in bytes
        usage_bytes: usize,
        /// The configured memory limit in bytes
        limit_bytes: usize,
    },

    /// Results truncated due to timeout
    TimedOut {
        /// How long the query ran
        elapsed: Duration,
        /// The configured timeout limit
        limit: Duration,
    },
}

impl QueryCompletionStatus {
    /// Returns true if the result set is complete
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Returns a user-friendly message for CLI output
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Complete => "Query completed successfully".to_string(),
            Self::ResultCapReached { count, limit } => {
                format!(
                    "Results truncated: showing {count} of {limit}+ matches (result cap reached)"
                )
            }
            Self::MemoryLimitReached {
                usage_bytes,
                limit_bytes,
            } => {
                format!(
                    "Results truncated: memory limit reached ({} of {} MB)",
                    usage_bytes / (1024 * 1024),
                    limit_bytes / (1024 * 1024)
                )
            }
            Self::TimedOut { elapsed, limit } => {
                format!(
                    "Results truncated: query timed out after {:.1}s (limit: {}s)",
                    elapsed.as_secs_f64(),
                    limit.as_secs()
                )
            }
        }
    }

    /// Returns the JSON field name for this status type
    ///
    /// Used for JSON output format consistency.
    #[must_use]
    pub fn status_field(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ResultCapReached { .. } => "result_cap_reached",
            Self::MemoryLimitReached { .. } => "memory_limit_reached",
            Self::TimedOut { .. } => "timed_out",
        }
    }

    /// Returns the CLI exit code for this status
    ///
    /// - Complete: 0 (success)
    /// - Truncated results: 2 (partial success, distinct from errors)
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Complete => 0,
            _ => 2, // Partial results - distinct from error (1)
        }
    }
}

/// Result set with completion status (per Codex iter10)
///
/// Wraps query results with a status indicator so callers know whether
/// the results are complete or truncated.
#[derive(Debug)]
pub struct QueryResultSet<T> {
    /// The results (may be partial if status != Complete)
    pub results: Vec<T>,

    /// Completion status indicating if results are complete or truncated
    pub status: QueryCompletionStatus,

    /// Actual memory usage tracked during execution
    pub memory_usage_bytes: usize,

    /// Actual elapsed time
    pub elapsed: Duration,
}

impl<T> QueryResultSet<T> {
    /// Create a complete result set
    #[must_use]
    pub fn complete(results: Vec<T>, memory_usage_bytes: usize, elapsed: Duration) -> Self {
        Self {
            results,
            status: QueryCompletionStatus::Complete,
            memory_usage_bytes,
            elapsed,
        }
    }

    /// Create a truncated result set
    #[must_use]
    pub fn truncated(
        results: Vec<T>,
        status: QueryCompletionStatus,
        memory_usage_bytes: usize,
        elapsed: Duration,
    ) -> Self {
        Self {
            results,
            status,
            memory_usage_bytes,
            elapsed,
        }
    }

    /// Returns true if all results were returned
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.status.is_complete()
    }

    /// Get the number of results
    #[must_use]
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Check if the result set is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_initial_state() {
        let guard = QueryGuard::new(QuerySecurityConfig::default());
        assert_eq!(guard.result_count(), 0);
        assert_eq!(guard.memory_usage(), 0);
        assert!(guard.should_continue().is_ok());
    }

    #[test]
    fn test_guard_record_result() {
        let guard = QueryGuard::new(QuerySecurityConfig::default());
        guard.record_result(1024);
        assert_eq!(guard.result_count(), 1);
        assert_eq!(guard.memory_usage(), 1024);
    }

    #[test]
    fn test_guard_result_cap() {
        let config = QuerySecurityConfig::default().with_result_cap(5);
        let guard = QueryGuard::new(config);

        for _ in 0..5 {
            guard.record_result(100);
        }

        let err = guard.should_continue().unwrap_err();
        assert!(matches!(
            err,
            QuerySecurityError::ResultCapExceeded { count: 5, limit: 5 }
        ));
    }

    #[test]
    fn test_guard_memory_limit() {
        // Set check interval to 1 so we check every time
        let config = QuerySecurityConfig::default().with_memory_limit(1000);
        let guard = QueryGuard::with_check_interval(config, 1);

        // Add enough to exceed limit
        guard.record_result(500);
        assert!(guard.should_continue().is_ok());

        guard.record_result(600);
        let err = guard.should_continue().unwrap_err();
        assert!(matches!(
            err,
            QuerySecurityError::MemoryLimitExceeded { .. }
        ));
    }

    #[test]
    fn test_completion_status_messages() {
        assert_eq!(
            QueryCompletionStatus::Complete.message(),
            "Query completed successfully"
        );

        let cap_status = QueryCompletionStatus::ResultCapReached {
            count: 100,
            limit: 100,
        };
        assert!(cap_status.message().contains("100"));

        let mem_status = QueryCompletionStatus::MemoryLimitReached {
            usage_bytes: 10 * 1024 * 1024,
            limit_bytes: 10 * 1024 * 1024,
        };
        assert!(mem_status.message().contains("MB"));

        let timeout_status = QueryCompletionStatus::TimedOut {
            elapsed: Duration::from_secs(5),
            limit: Duration::from_secs(5),
        };
        assert!(timeout_status.message().contains("timed out"));
    }

    #[test]
    fn test_completion_status_is_complete() {
        assert!(QueryCompletionStatus::Complete.is_complete());
        assert!(
            !QueryCompletionStatus::ResultCapReached {
                count: 10,
                limit: 10
            }
            .is_complete()
        );
    }

    #[test]
    fn test_error_to_status_conversion() {
        let timeout_err = QuerySecurityError::Timeout {
            elapsed: Duration::from_secs(10),
            limit: Duration::from_secs(5),
        };
        assert!(matches!(
            timeout_err.into_completion_status(),
            QueryCompletionStatus::TimedOut { .. }
        ));

        let cap_err = QuerySecurityError::ResultCapExceeded {
            count: 100,
            limit: 50,
        };
        assert!(matches!(
            cap_err.into_completion_status(),
            QueryCompletionStatus::ResultCapReached { .. }
        ));
    }

    #[test]
    fn test_result_set_complete() {
        let results = vec![1, 2, 3];
        let set = QueryResultSet::complete(results, 100, Duration::from_millis(10));
        assert!(set.is_complete());
        assert_eq!(set.len(), 3);
        assert!(!set.is_empty());
    }

    #[test]
    fn test_result_set_truncated() {
        let results = vec![1, 2];
        let status = QueryCompletionStatus::ResultCapReached { count: 2, limit: 2 };
        let set = QueryResultSet::truncated(results, status, 50, Duration::from_millis(5));
        assert!(!set.is_complete());
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(QueryCompletionStatus::Complete.exit_code(), 0);
        assert_eq!(
            QueryCompletionStatus::ResultCapReached {
                count: 10,
                limit: 10
            }
            .exit_code(),
            2
        );
        assert_eq!(
            QueryCompletionStatus::TimedOut {
                elapsed: Duration::from_secs(5),
                limit: Duration::from_secs(5)
            }
            .exit_code(),
            2
        );
    }
}
