//! Troubleshoot command implementation
//!
//! Generates a type-safe troubleshooting bundle for issue reporting.
//! All data is sanitized - no paths, code content, or secrets are included.

use crate::args::Cli;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use chrono::Utc;
use sqry_core::uses::{
    DiagnosticsSummary, IsoWeekPeriod, SanitizedConfig, SqryVersion, StructuredError, SystemInfo,
    TroubleshootBundle, UsesConfig, UsesStorage,
};
use std::path::Path;

/// Default cache size for sanitized config (when actual size unavailable)
const DEFAULT_CACHE_SIZE: usize = 100;

/// Run the troubleshoot command.
///
/// # Errors
/// Returns an error if the bundle cannot be generated or written.
pub fn run_troubleshoot(
    cli: &Cli,
    output: Option<&str>,
    preview: bool,
    include_trace: bool,
    window: &str,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Parse the time window
    let hours = parse_duration_hours(window).with_context(|| {
        format!("Invalid duration format: {window}. Use format like '24h' or '7d'")
    })?;

    // Load configuration
    let config = UsesConfig::load();

    // Generate the bundle
    let bundle = generate_bundle(&config, hours, include_trace)?;

    if preview {
        // Preview mode - show what would be included
        let preview_text = format_bundle_preview(&bundle);
        streams.write_result(&preview_text)?;
        return Ok(());
    }

    // Serialize to JSON
    let json =
        serde_json::to_string_pretty(&bundle).context("Failed to serialize troubleshoot bundle")?;

    // Output to file or stdout
    if let Some(output_path) = output {
        std::fs::write(output_path, &json)
            .with_context(|| format!("Failed to write bundle to {output_path}"))?;
        streams.write_diagnostic(&format!("Bundle written to: {output_path}"))?;
    } else if cli.json {
        streams.write_result(&json)?;
    } else {
        // Non-JSON mode to stdout - still output JSON but with a header
        streams.write_result("Troubleshoot bundle (copy and paste to share):\n")?;
        streams.write_result(&json)?;
    }

    Ok(())
}

/// Generate a troubleshoot bundle
fn generate_bundle(
    config: &UsesConfig,
    hours: u64,
    include_trace: bool,
) -> Result<TroubleshootBundle> {
    // Get uses directory
    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;

    // Load recent events (API takes days, so convert hours to days, rounding up)
    let days = u32::try_from(hours.div_ceil(24)).unwrap_or(u32::MAX);
    let storage = UsesStorage::new(uses_dir.clone());
    let (recent_events, _file_count) = storage
        .load_recent_events(days)
        .context("Failed to load recent events")?;

    // Count dropped events (from summary if available)
    let dropped_events = count_dropped_events(&uses_dir);

    // Build the bundle
    let bundle = TroubleshootBundle {
        generated_at: Utc::now(),
        sqry_version: SqryVersion::current(),
        system_info: SystemInfo::current(),
        config_sanitized: SanitizedConfig {
            uses_enabled: config.enabled,
            cache_size: DEFAULT_CACHE_SIZE,
        },
        recent_uses: recent_events,
        recent_errors: collect_recent_errors(&uses_dir),
        workflow_trace: if include_trace {
            Some(generate_workflow_trace(&uses_dir))
        } else {
            None
        },
        dropped_events,
    };

    Ok(bundle)
}

/// Parse duration string to hours (e.g., "24h" -> 24, "7d" -> 168)
fn parse_duration_hours(duration: &str) -> Result<u64> {
    let trimmed = duration.trim();

    if let Some(hours_str) = trimmed.strip_suffix('h') {
        return hours_str.parse::<u64>().context("Invalid number of hours");
    }

    if let Some(days_str) = trimmed.strip_suffix('d') {
        let days = days_str.parse::<u64>().context("Invalid number of days")?;
        return Ok(days * 24);
    }

    // Try parsing as plain hours
    trimmed
        .parse::<u64>()
        .context("Invalid duration. Use format like '24h' or '7d'")
}

/// Count dropped events from summaries
fn count_dropped_events(uses_dir: &Path) -> u64 {
    // Try to load the current week's summary to get dropped events count
    let storage = UsesStorage::new(uses_dir.to_path_buf());
    let current_week = IsoWeekPeriod::current();

    // read_summary returns raw bytes, need to deserialize
    if let Ok(bytes) = storage.read_summary(current_week.as_str())
        && let Ok(summary) = serde_json::from_slice::<DiagnosticsSummary>(&bytes)
    {
        return summary.dropped_events;
    }
    0
}

/// Collect recent structured errors
///
/// Loads error records from the errors directory for the troubleshoot bundle.
/// Errors are stored in daily JSONL files and loaded for the past 7 days.
fn collect_recent_errors(uses_dir: &Path) -> Vec<StructuredError> {
    let storage = UsesStorage::new(uses_dir.to_path_buf());

    // Load errors from the past 7 days
    match storage.load_recent_errors(7) {
        Ok((errors, skipped)) => {
            if skipped > 0 {
                log::debug!("Skipped {skipped} malformed error records");
            }
            // Limit to most recent 50 errors to keep bundle size reasonable
            errors.into_iter().take(50).collect()
        }
        Err(e) => {
            log::debug!("Failed to load error records: {e}");
            Vec::new()
        }
    }
}

/// Generate a workflow trace from recent events
///
/// Analyzes recent events to reconstruct the workflow.
/// Converts telemetry events into semantic workflow steps.
fn generate_workflow_trace(uses_dir: &Path) -> sqry_core::uses::WorkflowTrace {
    use sqry_core::uses::{UseEventType, WorkflowStep};

    let storage = UsesStorage::new(uses_dir.to_path_buf());

    // Load events from past day
    let (recent_events, _skipped) = match storage.load_recent_events(1) {
        Ok(result) => result,
        Err(e) => {
            log::debug!("Failed to load recent events for workflow trace: {e}");
            return sqry_core::uses::WorkflowTrace { steps: Vec::new() };
        }
    };

    // Take most recent 20 events to keep trace focused
    let events_to_process: Vec<_> = recent_events.into_iter().take(20).collect();

    // Convert events to workflow steps
    let mut steps = Vec::new();
    for event in events_to_process {
        match event.event_type {
            UseEventType::QueryExecuted { kind, result_count } => {
                // Query execution = query started + results displayed
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
            // Skip other event types that don't map to workflow steps
            UseEventType::AiAnswerGenerated { .. }
            | UseEventType::ViewAbandoned { .. }
            | UseEventType::FeedbackProvided { .. } => {
                // These don't translate to workflow steps
            }
        }
    }

    // Add session end marker if we have steps
    if !steps.is_empty() {
        steps.push(WorkflowStep::SessionEnded);
    }

    sqry_core::uses::WorkflowTrace { steps }
}

/// Format bundle preview for human reading
fn format_bundle_preview(bundle: &TroubleshootBundle) -> String {
    let mut lines = Vec::new();

    lines.push("Troubleshoot Bundle Preview".to_string());
    lines.push("=".repeat(40));
    lines.push(String::new());

    lines.push("System Information:".to_string());
    lines.push(format!("  OS: {:?}", bundle.system_info.os));
    lines.push(format!("  Architecture: {:?}", bundle.system_info.arch));
    lines.push(format!(
        "  sqry version: {}",
        bundle.system_info.sqry_version
    ));
    lines.push(format!("  Build type: {:?}", bundle.system_info.sqry_build));
    lines.push(String::new());

    lines.push("Configuration (sanitized):".to_string());
    lines.push(format!(
        "  Uses enabled: {}",
        bundle.config_sanitized.uses_enabled
    ));
    lines.push(format!(
        "  Cache size: {}",
        bundle.config_sanitized.cache_size
    ));
    lines.push(String::new());

    lines.push(format!("Recent events: {}", bundle.recent_uses.len()));
    lines.push(format!("Recent errors: {}", bundle.recent_errors.len()));
    lines.push(format!("Dropped events: {}", bundle.dropped_events));

    if let Some(trace) = &bundle.workflow_trace {
        lines.push(format!("Workflow trace: {} steps", trace.steps.len()));
    } else {
        lines.push("Workflow trace: not included".to_string());
    }

    lines.push(String::new());
    lines.push("Note: This preview shows what will be included.".to_string());
    lines.push("Run without --preview to generate the actual bundle.".to_string());

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration_hours("24h").unwrap(), 24);
        assert_eq!(parse_duration_hours("48h").unwrap(), 48);
        assert_eq!(parse_duration_hours("1d").unwrap(), 24);
        assert_eq!(parse_duration_hours("7d").unwrap(), 168);
        assert_eq!(parse_duration_hours("24").unwrap(), 24);
        assert_eq!(parse_duration_hours(" 24h ").unwrap(), 24);
    }

    #[test]
    fn test_parse_duration_hours_invalid() {
        assert!(parse_duration_hours("abc").is_err());
        assert!(parse_duration_hours("24x").is_err());
        assert!(parse_duration_hours("-24h").is_err());
    }
}
