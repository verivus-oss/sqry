//! Troubleshoot command implementation
//!
//! Generates a type-safe troubleshooting bundle for issue reporting.
//! All data is sanitized - no paths, code content, or secrets are included.

use crate::args::Cli;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use sqry_core::uses::{TroubleshootBundle, UsesConfig, generate_bundle};

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
