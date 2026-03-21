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
        #[cfg(feature = "share")]
        InsightsAction::Share {
            week,
            from,
            to,
            output,
            dry_run,
        } => run_share(
            cli,
            week.as_deref(),
            from.as_deref(),
            to.as_deref(),
            output.as_deref(),
            *dry_run,
        ),
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

/// Generate and output an anonymous usage snapshot.
///
/// Implements the `sqry insights share` subcommand.  Output contract:
///
/// | Flags                 | stdout                     | File       |
/// |-----------------------|----------------------------|------------|
/// | (none)                | Human-readable summary     | —          |
/// | `--json`              | JSON (`ShareSnapshot`)     | —          |
/// | `--output FILE`       | Confirmation message       | JSON file  |
/// | `--json --output FILE`| JSON (`ShareSnapshot`)     | JSON file  |
/// | `--dry-run`           | Human-readable preview     | —          |
///
/// Consent message is always printed to stderr.
#[cfg(feature = "share")]
fn run_share(
    cli: &Cli,
    week: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    output: Option<&std::path::Path>,
    dry_run: bool,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Gate: uses must be enabled
    let config = UsesConfig::load();
    if !config.enabled {
        streams.write_diagnostic(
            "Error: Uses capture is disabled. Enable with: sqry insights config --enable",
        )?;
        return Err(anyhow::anyhow!("Uses capture is disabled"));
    }

    let uses_dir = UsesConfig::uses_dir()
        .context("Could not determine uses directory (home directory unavailable)")?;
    let aggregator = DiagnosticsAggregator::new(&uses_dir);

    // Build snapshot: single week, range, or current
    let snapshot = match (week, from, to) {
        (None, Some(from_str), Some(to_str)) => {
            // Range: generate each week, then merge
            let weeks = iso_weeks_in_range(from_str, to_str)?;
            let snapshots: Result<Vec<_>> = weeks
                .iter()
                .map(|w| sqry_core::uses::generate_share_snapshot(&aggregator, w))
                .collect();
            sqry_core::uses::merge_snapshots(&snapshots?)
                .context("Failed to merge weekly snapshots")?
        }
        (Some(week_str), None, None) => {
            sqry_core::uses::generate_share_snapshot(&aggregator, week_str)
                .with_context(|| format!("Failed to generate snapshot for week {week_str}"))?
        }
        _ => {
            // Default: current week
            sqry_core::uses::generate_current_share_snapshot(&aggregator)
                .context("Failed to generate current week snapshot")?
        }
    };

    // Consent message → always goes to stderr
    streams.write_diagnostic("This file stays on your machine. No data is sent.")?;

    // --dry-run: preview only, no file
    if dry_run {
        let preview = sqry_core::uses::format_share_preview(&snapshot);
        streams.write_result(&preview)?;
        return Ok(());
    }

    // --output FILE: write JSON to file, confirmation (or JSON) to stdout
    if let Some(output_path) = output {
        let json = serde_json::to_string_pretty(&snapshot)
            .context("Failed to serialize snapshot to JSON")?;
        std::fs::write(output_path, &json)
            .with_context(|| format!("Failed to write snapshot to {}", output_path.display()))?;
        if cli.json {
            // --json --output FILE: JSON on stdout AND write file
            streams.write_result(&json)?;
        } else {
            // --output FILE (no --json): human-readable confirmation
            streams.write_result(&format!("Snapshot written to {}", output_path.display()))?;
        }
        return Ok(());
    }

    // Default / --json-only: output snapshot to stdout
    if cli.json {
        let json = serde_json::to_string_pretty(&snapshot)
            .context("Failed to serialize snapshot to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = sqry_core::uses::format_share_preview(&snapshot);
        streams.write_result(&text)?;
    }

    Ok(())
}

/// Enumerate ISO week strings in the range [`from`, `to`] (inclusive).
///
/// Both `from` and `to` must be valid ISO week strings (`YYYY-Www`).
/// Returns an error if the format is invalid or `from` is after `to`.
#[cfg(feature = "share")]
fn iso_weeks_in_range(from: &str, to: &str) -> Result<Vec<String>> {
    use chrono::Duration;

    // Validate both bounds with the canonical ISO week validator before doing
    // any date arithmetic.  This ensures invalid inputs like "2026-W00" or
    // "2026-W54" are rejected with exit-1 semantics consistent with
    // CLI_INTEGRATION.md §3.
    sqry_core::uses::IsoWeekPeriod::try_new(from).map_err(|_| {
        anyhow::anyhow!("Invalid ISO week format: {from}. Expected YYYY-Www (e.g. 2026-W09)")
    })?;
    sqry_core::uses::IsoWeekPeriod::try_new(to).map_err(|_| {
        anyhow::anyhow!("Invalid ISO week format: {to}. Expected YYYY-Www (e.g. 2026-W09)")
    })?;

    let from_monday = iso_week_to_monday(from)?;
    let to_monday = iso_week_to_monday(to)?;

    anyhow::ensure!(
        from_monday <= to_monday,
        "--from ({from}) must not be after --to ({to})"
    );

    let mut weeks = Vec::new();
    let mut current = from_monday;
    while current <= to_monday {
        weeks.push(current.format("%G-W%V").to_string());
        current += Duration::weeks(1);
    }
    Ok(weeks)
}

/// Convert an ISO week string (`YYYY-Www`) to the Monday of that week.
#[cfg(feature = "share")]
fn iso_week_to_monday(week: &str) -> Result<chrono::NaiveDate> {
    use chrono::{Datelike, Duration, NaiveDate};

    let (year_str, week_part) = week
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("Invalid week format: {week}"))?;
    let year: i32 = year_str
        .parse()
        .with_context(|| format!("Invalid year in week: {week}"))?;
    let week_num: u32 = week_part
        .strip_prefix('W')
        .ok_or_else(|| anyhow::anyhow!("Expected 'W' prefix in: {week}"))?
        .parse()
        .with_context(|| format!("Invalid week number in: {week}"))?;

    // ISO week arithmetic: Jan 4 is always in week 1
    let jan4 = NaiveDate::from_ymd_opt(year, 1, 4)
        .ok_or_else(|| anyhow::anyhow!("Invalid year: {year}"))?;
    let days_from_monday = jan4.weekday().num_days_from_monday();
    let week1_monday = jan4 - Duration::days(i64::from(days_from_monday));
    let week_monday = week1_monday + Duration::weeks(i64::from(week_num) - 1);
    Ok(week_monday)
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

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_week_to_monday_known_weeks() {
        use chrono::NaiveDate;
        // 2026-W01 Monday = 2025-12-29 (ISO: year 2026's week 1)
        // Verify 2026-W09 = 2026-02-23 Monday
        let monday = iso_week_to_monday("2026-W09").unwrap();
        assert_eq!(monday.format("%Y-%m-%d").to_string(), "2026-02-23");

        // 2026-W01 Monday
        let monday_w1 = iso_week_to_monday("2026-W01").unwrap();
        // ISO week 1 of 2026: Jan 4 is Sunday, so week 1 Monday is 2025-12-29
        assert_eq!(monday_w1.format("%Y-%m-%d").to_string(), "2025-12-29");
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_three_weeks() {
        let weeks = iso_weeks_in_range("2026-W07", "2026-W09").unwrap();
        assert_eq!(weeks, vec!["2026-W07", "2026-W08", "2026-W09"]);
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_single_week() {
        let weeks = iso_weeks_in_range("2026-W09", "2026-W09").unwrap();
        assert_eq!(weeks, vec!["2026-W09"]);
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_reversed_returns_error() {
        let result = iso_weeks_in_range("2026-W09", "2026-W07");
        assert!(result.is_err(), "from > to should return error");
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_invalid_week_w00_returns_error() {
        // W00 is not a valid ISO week — must be rejected before date arithmetic
        let result = iso_weeks_in_range("2026-W00", "2026-W01");
        assert!(result.is_err(), "W00 should be rejected as invalid");
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_invalid_week_w54_returns_error() {
        let result = iso_weeks_in_range("2026-W01", "2026-W54");
        assert!(result.is_err(), "W54 should be rejected as invalid");
    }

    #[cfg(feature = "share")]
    #[test]
    fn test_iso_weeks_in_range_invalid_non_padded_returns_error() {
        // "W9" (no zero padding) is not valid ISO week format
        let result = iso_weeks_in_range("2026-W9", "2026-W09");
        assert!(result.is_err(), "non-padded week W9 should be rejected");
    }
}
