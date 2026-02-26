//! Local uses and insights module
//!
//! This module provides privacy-respecting behavioral capture for sqry,
//! enabling users to understand their own usage patterns and share
//! anonymous insights if they choose to.
//!
//! # Architecture
//!
//! ```text
//! sqry operations ──▶ UsesCollector ──▶ Daily JSONL files
//!                       (fire-and-forget)     │
//!                                             ▼
//!                     DiagnosticsAggregator ◀─┘
//!                              │
//!                              ▼
//!                     Weekly summaries + Insights reports
//! ```
//!
//! # Privacy Guarantees
//!
//! All data is local-first:
//! - Events capture behavioral patterns, never code content
//! - All fields are strongly-typed enums (no arbitrary strings)
//! - Sharing is always explicit and requires confirmation
//! - Users can disable entirely via config or environment
//!
//! # Feature Gating
//!
//! This module is gated behind the `uses` feature flag. The hierarchy is:
//! - `uses`: Local uses recording
//! - `insights`: Local summaries and reports (requires `uses`)
//! - `troubleshoot`: Support bundle generation (requires `insights`)
//! - `share`: Shareable file generation (independent, off by default)
//!
//! Build with `--no-default-features` to produce a binary with no collection code.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::uses::{UseEvent, UseEventType, QueryKind, DiagnosticsAggregator};
//!
//! // Record an event (fire-and-forget)
//! let event = UseEvent::new(UseEventType::QueryExecuted {
//!     kind: QueryKind::CallChain,
//!     result_count: 42,
//! });
//! collector.record(event);
//!
//! // Get weekly insights
//! let aggregator = DiagnosticsAggregator::new(&uses_dir);
//! let summary = aggregator.summarize_week("2025-W50")?;
//! ```

mod types;

// Core uses modules
mod collector;
mod config;
mod storage;

// Insights module (aggregator)
#[cfg(feature = "insights")]
mod aggregator;

// Feature-gated submodules
// (These will be added in subsequent implementation steps)

// #[cfg(feature = "insights")]
// mod reporter;

// #[cfg(feature = "troubleshoot")]
// mod troubleshoot;

// Re-export all types from the types module
pub use types::*;

// Re-export collector, config, and storage
pub use collector::{CollectorTimedUse, UsesCollector};
pub use config::{
    AutoSummarizeConfig, ConfigLoadError, ConfigSaveError, ContextualFeedbackConfig,
    PromptFrequency, UsesConfig,
};
pub use storage::{UsesStorage, UsesWriter};

// Feature-gated re-exports
#[cfg(feature = "insights")]
pub use aggregator::{AggregatorError, DiagnosticsAggregator};

// #[cfg(feature = "insights")]
// pub use reporter::InsightsReporter;

// #[cfg(feature = "troubleshoot")]
// pub use troubleshoot::generate_bundle;
