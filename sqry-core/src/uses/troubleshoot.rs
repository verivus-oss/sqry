//! Support bundle generation for issue reporting.
//!
//! Generates a type-safe troubleshooting bundle containing sanitized
//! system information, configuration, and recent usage events.
//! No paths, code content, or secrets are included.

use anyhow::{Context, Result};
use chrono::Utc;

use super::{
    DiagnosticsSummary, IsoWeekPeriod, SanitizedConfig, SqryVersion, StructuredError, SystemInfo,
    TroubleshootBundle, UseEventType, UsesConfig, UsesStorage, WorkflowStep, WorkflowTrace,
};

/// Default cache size for sanitized config (when actual size unavailable)
const DEFAULT_CACHE_SIZE: usize = 100;

/// Maximum number of errors to include in a bundle
const MAX_ERRORS: usize = 50;

/// Maximum number of events for workflow trace
const MAX_WORKFLOW_EVENTS: usize = 20;

/// Generate a troubleshoot bundle from usage data.
///
/// # Errors
///
/// Returns an error if the uses directory cannot be determined or events cannot be loaded.
pub fn generate_bundle(
    config: &UsesConfig,
    hours: u64,
    include_trace: bool,
) -> Result<TroubleshootBundle> {
    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;

    // Load recent events (API takes days, so convert hours to days, rounding up)
    let days = u32::try_from(hours.div_ceil(24)).unwrap_or(u32::MAX);
    let storage = UsesStorage::new(uses_dir.clone());
    let (recent_events, _file_count) = storage
        .load_recent_events(days)
        .context("Failed to load recent events")?;

    // Count dropped events (from summary if available)
    let dropped_events = count_dropped_events(&storage);

    let bundle = TroubleshootBundle {
        generated_at: Utc::now(),
        sqry_version: SqryVersion::current(),
        system_info: SystemInfo::current(),
        config_sanitized: SanitizedConfig {
            uses_enabled: config.enabled,
            cache_size: DEFAULT_CACHE_SIZE,
        },
        recent_uses: recent_events,
        recent_errors: collect_recent_errors(&storage),
        workflow_trace: if include_trace {
            Some(generate_workflow_trace(&storage))
        } else {
            None
        },
        dropped_events,
    };

    Ok(bundle)
}

/// Count dropped events from the current week's summary.
fn count_dropped_events(storage: &UsesStorage) -> u64 {
    let current_week = IsoWeekPeriod::current();
    if let Ok(bytes) = storage.read_summary(current_week.as_str())
        && let Ok(summary) = serde_json::from_slice::<DiagnosticsSummary>(&bytes)
    {
        return summary.dropped_events;
    }
    0
}

/// Collect recent structured errors (past 7 days, max 50).
fn collect_recent_errors(storage: &UsesStorage) -> Vec<StructuredError> {
    match storage.load_recent_errors(7) {
        Ok((errors, skipped)) => {
            if skipped > 0 {
                log::debug!("Skipped {skipped} malformed error records");
            }
            errors.into_iter().take(MAX_ERRORS).collect()
        }
        Err(e) => {
            log::debug!("Failed to load error records: {e}");
            Vec::new()
        }
    }
}

/// Generate a workflow trace from recent events.
///
/// Analyzes recent events to reconstruct the workflow, converting
/// telemetry events into semantic workflow steps.
fn generate_workflow_trace(storage: &UsesStorage) -> WorkflowTrace {
    let (recent_events, _skipped) = match storage.load_recent_events(1) {
        Ok(result) => result,
        Err(e) => {
            log::debug!("Failed to load recent events for workflow trace: {e}");
            return WorkflowTrace { steps: Vec::new() };
        }
    };

    let events_to_process: Vec<_> = recent_events
        .into_iter()
        .take(MAX_WORKFLOW_EVENTS)
        .collect();

    let mut steps = Vec::new();
    for event in events_to_process {
        match event.event_type {
            UseEventType::QueryExecuted { kind, result_count } => {
                steps.push(WorkflowStep::QueryStarted { kind });
                steps.push(WorkflowStep::ResultsDisplayed {
                    count: result_count,
                });
            }
            UseEventType::GraphExpanded { kind, depth } => {
                steps.push(WorkflowStep::GraphExpanded { kind, depth });
            }
            UseEventType::ExportGenerated { format } => {
                steps.push(WorkflowStep::ExportGenerated { format });
            }
            UseEventType::AiAnswerGenerated { .. }
            | UseEventType::ViewAbandoned { .. }
            | UseEventType::FeedbackProvided { .. } => {}
        }
    }

    if !steps.is_empty() {
        steps.push(WorkflowStep::SessionEnded);
    }

    WorkflowTrace { steps }
}
