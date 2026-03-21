//! Security controls for query execution
//!
//! This module provides the security infrastructure for CD queries,
//! enforcing NON-NEGOTIABLE limits specified in the security requirements:
//!
//! - **Timeout**: 30s hard ceiling (spec AC-8)
//! - **Result cap**: 10k default results (spec AC-9)
//! - **Memory limit**: 512MB default (spec AC-10)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    QuerySecurityConfig                          │
//! │  - timeout: Duration (max 30s)                                  │
//! │  - result_cap: usize (default 10k)                              │
//! │  - memory_limit: usize (default 512MB)                          │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        QueryGuard                               │
//! │  - Tracks elapsed time, result count, memory usage              │
//! │  - should_continue() -> Result<(), QuerySecurityError>          │
//! │  - record_result(estimated_size)                                │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    QueryResultSet<T>                            │
//! │  - results: Vec<T>                                              │
//! │  - status: QueryCompletionStatus                                │
//! │  - Returns partial results when limits exceeded                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```
//! use sqry_core::query::security::{QuerySecurityConfig, QueryGuard};
//! use std::time::Duration;
//!
//! // Create security config (30s timeout is capped automatically)
//! let config = QuerySecurityConfig::default()
//!     .with_timeout(Duration::from_secs(10))
//!     .with_result_cap(1000);
//!
//! // Create guard for query execution
//! let guard = QueryGuard::new(config);
//!
//! // Check limits during query execution
//! assert!(guard.should_continue().is_ok());
//!
//! // Record results with estimated memory footprint
//! guard.record_result(128);  // 128 bytes per result
//! guard.record_result(128);
//!
//! // Check current state
//! assert_eq!(guard.result_count(), 2);
//! assert_eq!(guard.memory_usage(), 256);
//! ```
//!
//! For a complete usage example with partial results, see the integration pattern
//! in [`QueryGuard`] documentation.

mod audit;
mod config;
mod guard;
mod recursion_guard;

pub use audit::{
    AppliedLimits, AuditLogConfig, PathValidationError, QueryAuditEntry, QueryAuditLogger,
    QueryOutcome,
};
pub use config::QuerySecurityConfig;
pub use guard::{QueryCompletionStatus, QueryGuard, QueryResultSet, QuerySecurityError};
pub use recursion_guard::{ExprFuelCounter, RecursionError, RecursionGuard};
