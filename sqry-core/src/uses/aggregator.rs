//! Diagnostics Aggregator - transforms raw events into actionable summaries
//!
//! This module provides the "local reducer" that converts daily event logs
//! into weekly `DiagnosticsSummary` objects. Raw events are useless until
//! interpreted locally.
//!
//! # What the Aggregator Produces
//!
//! The reducer output answers:
//! - **What is used**: `top_workflows` counts
//! - **What is ignored**: Low counts relative to others
//! - **What is confusing**: High per-kind `abandonment` rates
//! - **What needs refinement**: High `ai_requery_rate`
//!
//! Nothing sensitive. All local.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::uses::{DiagnosticsAggregator, IsoWeekPeriod};
//!
//! let aggregator = DiagnosticsAggregator::new(&uses_dir);
//! let summary = aggregator.summarize_week("2025-W50")?;
//! println!("Top workflow: {:?}", summary.top_workflows.first());
//! ```

use super::storage::UsesStorage;
use super::types::{
    DiagnosticsSummary, GraphAbandonRate, GraphKind, IsoWeekPeriod, QueryKind, TopWorkflow,
    UseEvent, UseEventType, ViewKind,
};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use std::collections::HashMap;
use std::path::Path;

/// Diagnostics aggregator - transforms raw events into weekly summaries
pub struct DiagnosticsAggregator {
    storage: UsesStorage,
}

impl DiagnosticsAggregator {
    /// Create a new aggregator for the given uses directory
    #[must_use]
    pub fn new(uses_dir: &Path) -> Self {
        Self {
            storage: UsesStorage::new(uses_dir.to_path_buf()),
        }
    }

    /// Get the underlying storage
    #[must_use]
    pub fn storage(&self) -> &UsesStorage {
        &self.storage
    }

    /// Generate a summary for an ISO week
    ///
    /// # Arguments
    ///
    /// * `week` - ISO week string (e.g., "2025-W50")
    ///
    /// # Returns
    ///
    /// A `DiagnosticsSummary` containing aggregated metrics for the week.
    ///
    /// # Errors
    ///
    /// Returns an error if the week format is invalid or events cannot be loaded.
    pub fn summarize_week(&self, week: &str) -> Result<DiagnosticsSummary, AggregatorError> {
        let period = IsoWeekPeriod::try_new(week)
            .map_err(|_| AggregatorError::InvalidWeekFormat(week.to_string()))?;

        // Calculate date range for the week
        let (start_date, end_date) = week_to_date_range(week)?;

        // Load events for the week
        let (events, _skipped) = self
            .storage
            .load_events_for_range(&start_date, &end_date)
            .map_err(|e| AggregatorError::StorageError(e.to_string()))?;

        // Aggregate the events
        Ok(Self::aggregate_events(&events, period))
    }

    /// Generate a summary for a custom date range
    ///
    /// # Arguments
    ///
    /// * `start_date` - Start date in "YYYY-MM-DD" format
    /// * `end_date` - End date in "YYYY-MM-DD" format
    ///
    /// # Returns
    ///
    /// A `DiagnosticsSummary` containing aggregated metrics for the range.
    ///
    /// # Errors
    ///
    /// Returns an error if events cannot be loaded.
    pub fn summarize_range(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<DiagnosticsSummary, AggregatorError> {
        let (events, _skipped) = self
            .storage
            .load_events_for_range(start_date, end_date)
            .map_err(|e| AggregatorError::StorageError(e.to_string()))?;

        // Use current week as period label
        Ok(Self::aggregate_events(&events, IsoWeekPeriod::current()))
    }

    /// Generate a summary for the current week
    ///
    /// # Returns
    ///
    /// A `DiagnosticsSummary` for the current ISO week.
    ///
    /// # Errors
    ///
    /// Returns an error if events cannot be loaded.
    pub fn summarize_current_week(&self) -> Result<DiagnosticsSummary, AggregatorError> {
        let week = current_iso_week();
        self.summarize_week(&week)
    }

    /// Aggregate events into a summary
    fn aggregate_events(events: &[UseEvent], period: IsoWeekPeriod) -> DiagnosticsSummary {
        if events.is_empty() {
            return DiagnosticsSummary {
                period,
                ..Default::default()
            };
        }

        let top_workflows = Self::count_workflows(events);
        let (avg_time, median_time) = Self::calculate_timing_metrics(events);
        let abandon_rate = Self::calculate_overall_abandon_rate(events);
        let abandonment = Self::calculate_per_kind_abandonment(events);
        let ai_requery_rate = Self::calculate_requery_rate(events);

        DiagnosticsSummary {
            period,
            top_workflows,
            avg_time_to_result_sec: avg_time,
            median_time_to_result_sec: median_time,
            abandon_rate,
            abandonment,
            ai_requery_rate,
            total_uses: events.len(),
            dropped_events: 0, // This is populated from collector, not events
        }
    }

    /// Count workflow usage by query kind
    fn count_workflows(events: &[UseEvent]) -> Vec<TopWorkflow> {
        let mut counts: HashMap<QueryKind, usize> = HashMap::new();

        for event in events {
            if let UseEventType::QueryExecuted { kind, .. } = &event.event_type {
                *counts.entry(*kind).or_insert(0) += 1;
            }
        }

        // Convert to TopWorkflow and sort by count descending
        let mut workflows: Vec<TopWorkflow> = counts
            .into_iter()
            .map(|(kind, count)| TopWorkflow { kind, count })
            .collect();

        workflows.sort_by(|a, b| b.count.cmp(&a.count));
        workflows
    }

    /// Calculate average and median timing metrics
    fn calculate_timing_metrics(events: &[UseEvent]) -> (f64, f64) {
        let durations: Vec<f64> = events
            .iter()
            .filter_map(|e| e.duration_ms)
            .map(|ms| f64::from(u32::try_from(ms).unwrap_or(u32::MAX)) / 1000.0)
            .collect();

        if durations.is_empty() {
            return (0.0, 0.0);
        }

        let avg = average_duration(&durations);
        let mut sorted = durations.clone();
        let median = median_duration(&mut sorted);

        (avg, median)
    }

    /// Calculate overall abandonment rate
    ///
    /// Abandonment rate = `ViewAbandoned` events / (`ViewAbandoned` + successful interactions)
    fn calculate_overall_abandon_rate(events: &[UseEvent]) -> f64 {
        let abandoned = events
            .iter()
            .filter(|e| matches!(e.event_type, UseEventType::ViewAbandoned { .. }))
            .count();

        // Consider graph expansions as "successful interactions"
        let graph_expansions = events
            .iter()
            .filter(|e| matches!(e.event_type, UseEventType::GraphExpanded { .. }))
            .count();

        let total = abandoned + graph_expansions;
        if total == 0 {
            return 0.0;
        }

        usize_to_f64(abandoned) / usize_to_f64(total)
    }

    /// Calculate per-graph-kind abandonment rates
    fn calculate_per_kind_abandonment(events: &[UseEvent]) -> Vec<GraphAbandonRate> {
        let abandoned_counts = Self::count_abandoned_by_kind(events);
        let graph_counts = Self::count_graph_expansions(events);
        let graph_abandonments = *abandoned_counts.get(&ViewKind::Graph).unwrap_or(&0);
        let total_graph_expansions: usize = graph_counts.values().sum();

        let mut rates = Vec::new();

        for graph_kind in [
            GraphKind::CallGraph,
            GraphKind::DependencyGraph,
            GraphKind::ImportGraph,
        ] {
            if let Some(rate) = Self::graph_abandonment_rate(
                graph_kind,
                graph_abandonments,
                total_graph_expansions,
                &graph_counts,
            ) {
                rates.push(GraphAbandonRate {
                    kind: graph_kind,
                    rate,
                });
            }
        }

        rates
    }

    fn count_abandoned_by_kind(events: &[UseEvent]) -> HashMap<ViewKind, usize> {
        let mut abandoned_counts: HashMap<ViewKind, usize> = HashMap::new();
        for event in events {
            if let UseEventType::ViewAbandoned { kind, .. } = &event.event_type {
                *abandoned_counts.entry(*kind).or_insert(0) += 1;
            }
        }
        abandoned_counts
    }

    fn count_graph_expansions(events: &[UseEvent]) -> HashMap<GraphKind, usize> {
        let mut graph_counts: HashMap<GraphKind, usize> = HashMap::new();
        for event in events {
            if let UseEventType::GraphExpanded { kind, .. } = &event.event_type {
                *graph_counts.entry(*kind).or_insert(0) += 1;
            }
        }
        graph_counts
    }

    fn graph_abandonment_rate(
        graph_kind: GraphKind,
        graph_abandonments: usize,
        total_graph_expansions: usize,
        graph_counts: &HashMap<GraphKind, usize>,
    ) -> Option<f64> {
        let expansions = *graph_counts.get(&graph_kind).unwrap_or(&0);
        if expansions == 0 && graph_abandonments == 0 {
            return None;
        }

        let proportional_abandonments = if total_graph_expansions > 0 {
            (usize_to_f64(graph_abandonments) * usize_to_f64(expansions))
                / usize_to_f64(total_graph_expansions)
        } else {
            0.0
        };

        let rate = if expansions > 0 {
            proportional_abandonments / (proportional_abandonments + usize_to_f64(expansions))
        } else {
            0.0
        };

        Some(rate)
    }

    /// Calculate AI requery rate
    ///
    /// Requery rate = AI answers where requeried=true / total AI answers
    fn calculate_requery_rate(events: &[UseEvent]) -> f64 {
        let mut total_ai = 0;
        let mut requeried = 0;

        for event in events {
            if let UseEventType::AiAnswerGenerated { requeried: r, .. } = &event.event_type {
                total_ai += 1;
                if *r {
                    requeried += 1;
                }
            }
        }

        if total_ai == 0 {
            return 0.0;
        }

        f64::from(requeried) / f64::from(total_ai)
    }

    /// Prune old event files
    ///
    /// Delegates to storage layer.
    ///
    /// # Arguments
    ///
    /// * `retain_days` - Number of days to retain
    ///
    /// # Returns
    ///
    /// Number of files deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if pruning fails.
    pub fn prune(&self, retain_days: u32) -> Result<usize, AggregatorError> {
        self.storage
            .prune_old_events(retain_days)
            .map_err(|e| AggregatorError::StorageError(e.to_string()))
    }

    /// Save a summary to the summaries directory
    ///
    /// # Arguments
    ///
    /// * `summary` - The summary to save
    ///
    /// # Errors
    ///
    /// Returns an error if the summary cannot be serialized or written.
    pub fn save_summary(&self, summary: &DiagnosticsSummary) -> Result<(), AggregatorError> {
        let json = serde_json::to_vec_pretty(summary)
            .map_err(|e| AggregatorError::SerializationError(e.to_string()))?;

        self.storage
            .write_summary(summary.period.as_str(), &json)
            .map_err(|e| AggregatorError::StorageError(e.to_string()))
    }

    /// Load a saved summary
    ///
    /// # Arguments
    ///
    /// * `week` - ISO week string (e.g., "2025-W50")
    ///
    /// # Returns
    ///
    /// The saved summary if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the summary cannot be read or parsed.
    pub fn load_summary(&self, week: &str) -> Result<DiagnosticsSummary, AggregatorError> {
        let data = self
            .storage
            .read_summary(week)
            .map_err(|e| AggregatorError::StorageError(e.to_string()))?;

        serde_json::from_slice(&data)
            .map_err(|e| AggregatorError::SerializationError(e.to_string()))
    }

    /// Check if a summary exists for a week
    #[must_use]
    pub fn summary_exists(&self, week: &str) -> bool {
        self.storage.summary_exists(week)
    }

    /// Get or generate summary for a week
    ///
    /// Loads cached summary if available, otherwise generates and saves it.
    ///
    /// # Arguments
    ///
    /// * `week` - ISO week string (e.g., "2025-W50")
    ///
    /// # Returns
    ///
    /// The summary for the week.
    ///
    /// # Errors
    ///
    /// Returns an error if the summary cannot be loaded or generated.
    pub fn get_or_generate_summary(
        &self,
        week: &str,
    ) -> Result<DiagnosticsSummary, AggregatorError> {
        // Try to load cached
        if self.summary_exists(week)
            && let Ok(summary) = self.load_summary(week)
        {
            return Ok(summary);
        }

        // Generate fresh
        let summary = self.summarize_week(week)?;

        // Save for future use
        let _ = self.save_summary(&summary);

        Ok(summary)
    }
}

/// Convert ISO week string to date range
fn week_to_date_range(week: &str) -> Result<(String, String), AggregatorError> {
    let (year, week_num) = parse_week_parts(week)?;

    // Calculate first day of the ISO week (Monday)
    // ISO week 1 is the week containing the first Thursday of the year
    let jan4 = NaiveDate::from_ymd_opt(year, 1, 4).ok_or_else(|| invalid_week_error(week))?;

    // Find Monday of week 1
    let days_since_monday = jan4.weekday().num_days_from_monday();
    let week1_monday = jan4 - Duration::days(i64::from(days_since_monday));

    // Calculate start of requested week
    let start = week1_monday + Duration::weeks(i64::from(week_num - 1));
    let end = start + Duration::days(6);

    Ok((
        start.format("%Y-%m-%d").to_string(),
        end.format("%Y-%m-%d").to_string(),
    ))
}

/// Get the current ISO week string
fn current_iso_week() -> String {
    Utc::now().format("%G-W%V").to_string()
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn average_duration(durations: &[f64]) -> f64 {
    durations.iter().sum::<f64>() / usize_to_f64(durations.len())
}

fn median_duration(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.len().is_multiple_of(2) {
        let mid = values.len() / 2;
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[values.len() / 2]
    }
}

fn parse_week_parts(week: &str) -> Result<(i32, u32), AggregatorError> {
    // Parse "YYYY-Www" format
    if week.len() != 8 || !week.contains("-W") {
        return Err(invalid_week_error(week));
    }

    let year: i32 = week[0..4].parse().map_err(|_| invalid_week_error(week))?;
    let week_num: u32 = week[6..8].parse().map_err(|_| invalid_week_error(week))?;

    Ok((year, week_num))
}

fn invalid_week_error(week: &str) -> AggregatorError {
    AggregatorError::InvalidWeekFormat(week.to_string())
}

/// Errors that can occur during aggregation
#[derive(Debug, thiserror::Error)]
pub enum AggregatorError {
    /// Invalid ISO week format
    #[error("invalid ISO week format: {0}")]
    InvalidWeekFormat(String),

    /// Storage operation failed
    #[error("storage error: {0}")]
    StorageError(String),

    /// Serialization/deserialization error
    #[error("serialization error: {0}")]
    SerializationError(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const FLOAT_EPSILON: f64 = 1.0e-9;

    fn assert_f64_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < FLOAT_EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn create_test_events() -> Vec<UseEvent> {
        vec![
            UseEvent::with_duration(
                UseEventType::QueryExecuted {
                    kind: QueryKind::CallChain,
                    result_count: 10,
                },
                100,
            ),
            UseEvent::with_duration(
                UseEventType::QueryExecuted {
                    kind: QueryKind::CallChain,
                    result_count: 20,
                },
                200,
            ),
            UseEvent::with_duration(
                UseEventType::QueryExecuted {
                    kind: QueryKind::ImpactAnalysis,
                    result_count: 5,
                },
                300,
            ),
            UseEvent::new(UseEventType::GraphExpanded {
                kind: GraphKind::CallGraph,
                depth: 3,
            }),
            UseEvent::new(UseEventType::ViewAbandoned {
                kind: ViewKind::Graph,
                time_spent_ms: 5000,
            }),
            UseEvent::new(UseEventType::AiAnswerGenerated {
                accepted: true,
                requeried: false,
            }),
            UseEvent::new(UseEventType::AiAnswerGenerated {
                accepted: false,
                requeried: true,
            }),
        ]
    }

    #[test]
    fn test_count_workflows() {
        let events = create_test_events();

        let workflows = DiagnosticsAggregator::count_workflows(&events);

        assert_eq!(workflows.len(), 2);
        // CallChain should be first (2 occurrences)
        assert_eq!(workflows[0].kind, QueryKind::CallChain);
        assert_eq!(workflows[0].count, 2);
        // ImpactAnalysis second (1 occurrence)
        assert_eq!(workflows[1].kind, QueryKind::ImpactAnalysis);
        assert_eq!(workflows[1].count, 1);
    }

    #[test]
    fn test_timing_metrics() {
        let events = create_test_events();

        let (avg, median) = DiagnosticsAggregator::calculate_timing_metrics(&events);

        // Events have durations: 100ms, 200ms, 300ms
        // Avg = 200ms = 0.2s
        assert!((avg - 0.2).abs() < 0.001);
        // Median = 200ms = 0.2s (middle of 3 values)
        assert!((median - 0.2).abs() < 0.001);
    }

    #[test]
    fn test_overall_abandon_rate() {
        let events = create_test_events();

        let rate = DiagnosticsAggregator::calculate_overall_abandon_rate(&events);

        // 1 abandoned, 1 graph expansion = 0.5
        assert!((rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_requery_rate() {
        let events = create_test_events();

        let rate = DiagnosticsAggregator::calculate_requery_rate(&events);

        // 1 requeried out of 2 AI answers = 0.5
        assert!((rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_aggregate_events() {
        let events = create_test_events();
        let period = IsoWeekPeriod::try_new("2025-W50").unwrap();

        let summary = DiagnosticsAggregator::aggregate_events(&events, period);

        assert_eq!(summary.total_uses, 7);
        assert!(!summary.top_workflows.is_empty());
        assert!((summary.abandon_rate - 0.5).abs() < 0.001);
        assert!((summary.ai_requery_rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_empty_events() {
        let events: Vec<UseEvent> = vec![];
        let period = IsoWeekPeriod::try_new("2025-W50").unwrap();

        let summary = DiagnosticsAggregator::aggregate_events(&events, period);

        assert_eq!(summary.total_uses, 0);
        assert!(summary.top_workflows.is_empty());
        assert_f64_close(summary.avg_time_to_result_sec, 0.0);
        assert_f64_close(summary.abandon_rate, 0.0);
    }

    #[test]
    fn test_week_to_date_range() {
        // Week 1 of 2025
        let (start, end) = week_to_date_range("2025-W01").unwrap();
        assert_eq!(start, "2024-12-30"); // Monday of ISO week 1
        assert_eq!(end, "2025-01-05"); // Sunday

        // Week 50 of 2025
        let (start, end) = week_to_date_range("2025-W50").unwrap();
        assert_eq!(start, "2025-12-08");
        assert_eq!(end, "2025-12-14");
    }

    #[test]
    fn test_invalid_week_format() {
        let result = week_to_date_range("invalid");
        assert!(result.is_err());

        let result = week_to_date_range("2025-50");
        assert!(result.is_err());
    }

    #[test]
    fn test_save_and_load_summary() {
        let dir = tempdir().unwrap();
        let uses_dir = dir.path().join("uses");
        let aggregator = DiagnosticsAggregator::new(&uses_dir);

        // Ensure directories exist
        aggregator.storage.ensure_directories().unwrap();

        let summary = DiagnosticsSummary {
            period: IsoWeekPeriod::try_new("2025-W50").unwrap(),
            top_workflows: vec![TopWorkflow {
                kind: QueryKind::CallChain,
                count: 42,
            }],
            avg_time_to_result_sec: 1.5,
            median_time_to_result_sec: 1.2,
            abandon_rate: 0.1,
            abandonment: vec![],
            ai_requery_rate: 0.3,
            total_uses: 100,
            dropped_events: 0,
        };

        aggregator.save_summary(&summary).unwrap();
        assert!(aggregator.summary_exists("2025-W50"));

        let loaded = aggregator.load_summary("2025-W50").unwrap();
        assert_eq!(loaded.total_uses, 100);
        assert_eq!(loaded.top_workflows[0].count, 42);
    }

    #[test]
    fn test_current_iso_week() {
        let week = current_iso_week();
        // Should match format YYYY-Www
        assert!(week.len() == 8);
        assert!(week.contains("-W"));
    }
}
