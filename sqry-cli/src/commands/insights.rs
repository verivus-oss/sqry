//! Insights command implementation
//!
//! Provides CLI interface for viewing usage insights and managing
//! local diagnostics data.

use crate::args::{Cli, InsightsAction};
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use sqry_core::uses::{DiagnosticsAggregator, UsesConfig, UsesStorage};

const KB_BYTES: u64 = 1024;
const MB_BYTES: u64 = KB_BYTES * 1024;
const GB_BYTES: u64 = MB_BYTES * 1024;
const KB_BYTES_F64: f64 = 1024.0;
const MB_BYTES_F64: f64 = 1024.0 * 1024.0;
const GB_BYTES_F64: f64 = 1024.0 * 1024.0 * 1024.0;

/// Run the insights command.
///
/// # Errors
/// Returns an error if insights data cannot be loaded or written.
pub fn run_insights(cli: &Cli, action: &InsightsAction) -> Result<()> {
    match action {
        InsightsAction::Show { week } => run_show(cli, week.as_deref()),
        InsightsAction::Config {
            enable,
            disable,
            retention,
        } => run_config(cli, *enable, *disable, *retention),
        InsightsAction::Status => run_status(cli),
        InsightsAction::Prune { older, dry_run } => run_prune(cli, older.as_deref(), *dry_run),
    }
}

/// Show usage summary for a time period
fn run_show(cli: &Cli, week: Option<&str>) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Get uses directory
    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;

    // Check if uses is enabled
    let config = UsesConfig::load();
    if !config.enabled {
        streams.write_diagnostic(
            "Uses capture is currently disabled. Enable with: sqry insights config --enable",
        )?;
        return Ok(());
    }

    // Create aggregator
    let aggregator = DiagnosticsAggregator::new(&uses_dir);

    // Get or generate summary
    let summary = if let Some(week_str) = week {
        aggregator
            .get_or_generate_summary(week_str)
            .with_context(|| format!("Failed to get summary for week {week_str}"))?
    } else {
        aggregator
            .summarize_current_week()
            .context("Failed to generate summary for current week")?
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&summary)
            .context("Failed to serialize summary to JSON")?;
        streams.write_result(&json)?;
    } else {
        // Text output
        let output = format_summary_text(&summary);
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Format summary as human-readable text
fn format_summary_text(summary: &sqry_core::uses::DiagnosticsSummary) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Usage Summary for {}", summary.period));
    lines.push(String::new());

    // Total uses
    lines.push(format!("Total uses: {}", summary.total_uses));
    if summary.dropped_events > 0 {
        lines.push(format!("Dropped events: {}", summary.dropped_events));
    }
    lines.push(String::new());

    // Top workflows
    if !summary.top_workflows.is_empty() {
        lines.push("Top Workflows:".to_string());
        for workflow in &summary.top_workflows {
            lines.push(format!("  {:?}: {}", workflow.kind, workflow.count));
        }
        lines.push(String::new());
    }

    // Timing metrics
    lines.push("Timing Metrics:".to_string());
    lines.push(format!(
        "  Average time to result: {:.2}s",
        summary.avg_time_to_result_sec
    ));
    lines.push(format!(
        "  Median time to result: {:.2}s",
        summary.median_time_to_result_sec
    ));
    lines.push(String::new());

    // Rates
    lines.push(format!(
        "Abandonment rate: {:.1}%",
        summary.abandon_rate * 100.0
    ));
    lines.push(format!(
        "AI requery rate: {:.1}%",
        summary.ai_requery_rate * 100.0
    ));

    // Per-kind abandonment if present
    if !summary.abandonment.is_empty() {
        lines.push(String::new());
        lines.push("Abandonment by graph type:".to_string());
        for abandon in &summary.abandonment {
            lines.push(format!(
                "  {:?}: {:.1}%",
                abandon.kind,
                abandon.rate * 100.0
            ));
        }
    }

    lines.join("\n")
}

/// Show or modify configuration
fn run_config(cli: &Cli, enable: bool, disable: bool, retention: Option<u32>) -> Result<()> {
    let mut streams = OutputStreams::new();
    let mut config = UsesConfig::load();
    let mut modified = false;

    // Apply changes
    if enable {
        config.enabled = true;
        modified = true;
    }
    if disable {
        config.enabled = false;
        modified = true;
    }
    if let Some(days) = retention {
        config.retention_days = days;
        modified = true;
    }

    // Save if modified
    if modified {
        config.save().context("Failed to save configuration")?;
        streams.write_diagnostic("Configuration updated successfully.")?;
    }

    // Output current config
    if cli.json {
        let json =
            serde_json::to_string_pretty(&config).context("Failed to serialize config to JSON")?;
        streams.write_result(&json)?;
    } else {
        let uses_dir = UsesConfig::uses_dir()
            .map_or_else(|| "(unavailable)".to_string(), |p| p.display().to_string());

        let output = format!(
            "Uses Configuration:\n\
             \n\
             Enabled: {}\n\
             Retention: {} days\n\
             Storage: {}\n\
             \n\
             Contextual Feedback:\n\
             - Enabled: {}\n\
             - Frequency: {:?}\n\
             \n\
             Auto-summarize: {}",
            if config.enabled { "yes" } else { "no" },
            config.retention_days,
            uses_dir,
            if config.contextual_feedback.enabled {
                "yes"
            } else {
                "no"
            },
            config.contextual_feedback.prompt_frequency,
            if config.auto_summarize.enabled {
                "yes"
            } else {
                "no"
            },
        );
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Show storage status
fn run_status(cli: &Cli) -> Result<()> {
    let mut streams = OutputStreams::new();

    let config = UsesConfig::load();
    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;

    // Calculate storage statistics
    let storage = UsesStorage::new(uses_dir.clone());
    let stats = calculate_storage_stats(&storage)?;

    if cli.json {
        let json_output = serde_json::json!({
            "enabled": config.enabled,
            "uses_dir": uses_dir.display().to_string(),
            "total_files": stats.total_files,
            "total_size_bytes": stats.total_size_bytes,
            "oldest_date": stats.oldest_date,
            "newest_date": stats.newest_date,
            "retention_days": config.retention_days,
        });
        let json = serde_json::to_string_pretty(&json_output)
            .context("Failed to serialize status to JSON")?;
        streams.write_result(&json)?;
    } else {
        let enabled_str = if config.enabled {
            "enabled"
        } else {
            "disabled"
        };

        let size_str = format_size(stats.total_size_bytes);
        let date_range =
            if let (Some(oldest), Some(newest)) = (&stats.oldest_date, &stats.newest_date) {
                format!("{oldest} to {newest}")
            } else {
                "no data".to_string()
            };

        let output = format!(
            "Uses Status:\n\
             \n\
             Capture: {enabled_str}\n\
             Storage: {}\n\
             Files: {}\n\
             Size: {size_str}\n\
             Date range: {date_range}\n\
             Retention: {} days",
            uses_dir.display(),
            stats.total_files,
            config.retention_days,
        );
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Storage statistics
struct StorageStats {
    total_files: usize,
    total_size_bytes: u64,
    oldest_date: Option<String>,
    newest_date: Option<String>,
}

/// Calculate storage statistics
fn calculate_storage_stats(storage: &UsesStorage) -> Result<StorageStats> {
    let events_dir = storage.events_dir();

    let mut total_files = 0;
    let mut total_size_bytes = 0u64;
    let mut oldest_date: Option<String> = None;
    let mut newest_date: Option<String> = None;

    if events_dir.exists() {
        for entry in std::fs::read_dir(&events_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !is_event_log_file(&path) {
                continue;
            }

            total_files += 1;
            if let Ok(metadata) = entry.metadata() {
                total_size_bytes += metadata.len();
            }

            if let Some(date) = extract_event_date(&path) {
                update_date_range(&mut oldest_date, &mut newest_date, date);
            }
        }
    }

    Ok(StorageStats {
        total_files,
        total_size_bytes,
        oldest_date,
        newest_date,
    })
}

fn is_event_log_file(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|ext| ext == "jsonl")
}

fn extract_event_date(path: &std::path::Path) -> Option<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|filename| filename.strip_prefix("events-"))
}

fn update_date_range(oldest: &mut Option<String>, newest: &mut Option<String>, date: &str) {
    match (oldest.as_deref(), newest.as_deref()) {
        (None, _) => {
            *oldest = Some(date.to_string());
            *newest = Some(date.to_string());
        }
        (Some(oldest_date), Some(newest_date)) => {
            if date < oldest_date {
                *oldest = Some(date.to_string());
            }
            if date > newest_date {
                *newest = Some(date.to_string());
            }
        }
        _ => {}
    }
}

/// Format size in human-readable form
fn format_size(bytes: u64) -> String {
    if bytes >= GB_BYTES {
        format!("{:.2} GB", u64_to_f64_lossy(bytes) / GB_BYTES_F64)
    } else if bytes >= MB_BYTES {
        format!("{:.2} MB", u64_to_f64_lossy(bytes) / MB_BYTES_F64)
    } else if bytes >= KB_BYTES {
        format!("{:.2} KB", u64_to_f64_lossy(bytes) / KB_BYTES_F64)
    } else {
        format!("{bytes} bytes")
    }
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    let narrowed = u32::try_from(value).unwrap_or(u32::MAX);
    f64::from(narrowed)
}

/// Prune old event data
fn run_prune(cli: &Cli, older: Option<&str>, dry_run: bool) -> Result<()> {
    let mut streams = OutputStreams::new();

    let config = UsesConfig::load();
    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;

    // Parse duration or use configured retention
    let retain_days = if let Some(duration_str) = older {
        parse_duration_days(duration_str).with_context(|| {
            format!("Invalid duration format: {duration_str}. Use format like '30d' or '90d'")
        })?
    } else {
        config.retention_days
    };

    let aggregator = DiagnosticsAggregator::new(&uses_dir);

    if dry_run {
        // Preview mode - count files that would be deleted
        let storage = UsesStorage::new(uses_dir.clone());
        let preview = count_files_to_prune(&storage, retain_days)?;

        if cli.json {
            let json_output = serde_json::json!({
                "dry_run": true,
                "files_to_delete": preview.file_count,
                "bytes_to_free": preview.total_bytes,
                "retain_days": retain_days,
            });
            let json = serde_json::to_string_pretty(&json_output)?;
            streams.write_result(&json)?;
        } else {
            let size_str = format_size(preview.total_bytes);
            streams.write_result(&format!(
                "Dry run: Would delete {} files ({size_str}) older than {retain_days} days",
                preview.file_count,
            ))?;
        }
    } else {
        // Actually prune
        let pruned_count = aggregator
            .prune(retain_days)
            .context("Failed to prune event logs")?;

        if cli.json {
            let json_output = serde_json::json!({
                "pruned_files": pruned_count,
                "retain_days": retain_days,
            });
            let json = serde_json::to_string_pretty(&json_output)?;
            streams.write_result(&json)?;
        } else {
            streams.write_result(&format!(
                "Pruned {pruned_count} files older than {retain_days} days"
            ))?;
        }
    }

    Ok(())
}

/// Parse duration string to days (e.g., "30d" -> 30)
fn parse_duration_days(duration: &str) -> Result<u32> {
    let trimmed = duration.trim();

    if let Some(days_str) = trimmed.strip_suffix('d') {
        days_str.parse::<u32>().context("Invalid number of days")
    } else {
        // Try parsing as plain number (assume days)
        trimmed
            .parse::<u32>()
            .context("Invalid duration. Use format like '30d' or '90d'")
    }
}

/// Preview of files to prune
struct PrunePreview {
    file_count: usize,
    total_bytes: u64,
}

/// Count files that would be pruned
fn count_files_to_prune(storage: &UsesStorage, retain_days: u32) -> Result<PrunePreview> {
    use chrono::{NaiveDate, Utc};

    let events_dir = storage.events_dir();
    let cutoff = Utc::now().date_naive() - chrono::Duration::days(i64::from(retain_days));

    let mut file_count = 0;
    let mut total_bytes = 0u64;

    if events_dir.exists() {
        for entry in std::fs::read_dir(&events_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "jsonl")
                && let Some(filename) = path.file_stem().and_then(|s| s.to_str())
                && let Some(date_str) = filename.strip_prefix("events-")
                && let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
                && date < cutoff
            {
                file_count += 1;
                if let Ok(metadata) = entry.metadata() {
                    total_bytes += metadata.len();
                }
            }
        }
    }

    Ok(PrunePreview {
        file_count,
        total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration_days("30d").unwrap(), 30);
        assert_eq!(parse_duration_days("90d").unwrap(), 90);
        assert_eq!(parse_duration_days("365d").unwrap(), 365);
        assert_eq!(parse_duration_days("30").unwrap(), 30);
        assert_eq!(parse_duration_days(" 30d ").unwrap(), 30);
    }

    #[test]
    fn test_parse_duration_days_invalid() {
        assert!(parse_duration_days("abc").is_err());
        assert!(parse_duration_days("30x").is_err());
        assert!(parse_duration_days("-30d").is_err());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(500), "500 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }
}
