//! History management command implementation
//!
//! Handles the `sqry history` subcommand for managing query history.

use crate::args::{Cli, HistoryAction};
use crate::output::OutputStreams;
use crate::persistence::{HistoryManager, PersistenceConfig, open_shared_index, parse_duration};
use anyhow::{Result, bail};
use chrono::Utc;
use std::path::Path;

/// Run the history command.
///
/// # Errors
/// Returns an error if history cannot be loaded or written.
pub fn run_history(cli: &Cli, action: &HistoryAction) -> Result<()> {
    match action {
        HistoryAction::List { limit } => run_list(cli, *limit),
        HistoryAction::Search { pattern, limit } => run_search(cli, pattern, *limit),
        HistoryAction::Clear { older, confirm } => run_clear(cli, older.as_deref(), *confirm),
        HistoryAction::Stats => run_stats(cli),
    }
}

/// List recent history entries
fn run_list(cli: &Cli, limit: usize) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = HistoryManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let entries = manager.list(limit)?;

    if cli.json {
        let json_entries: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "timestamp": e.timestamp.to_rfc3339(),
                    "command": e.command,
                    "args": e.args,
                    "working_dir": e.working_dir,
                    "success": e.success,
                    "duration_ms": e.duration_ms,
                })
            })
            .collect();
        let output = serde_json::to_string_pretty(&json_entries)?;
        streams.write_result(&output)?;
    } else if entries.is_empty() {
        streams.write_result("No history entries.\n")?;
    } else {
        streams.write_result(&format!("History ({} entries):\n\n", entries.len()))?;
        for entry in &entries {
            let status = if entry.success { "" } else { " [FAILED]" };
            let duration = entry
                .duration_ms
                .map(|d| format!(" ({d}ms)"))
                .unwrap_or_default();
            let args_str = if entry.args.is_empty() {
                String::new()
            } else {
                format!(" {}", entry.args.join(" "))
            };
            let timestamp = entry.timestamp.format("%Y-%m-%d %H:%M:%S");
            streams.write_result(&format!(
                "  [{timestamp}] {}{}{}{}\n",
                entry.command, args_str, status, duration
            ))?;
        }
    }

    streams.finish_checked()
}

/// Search history entries
fn run_search(cli: &Cli, pattern: &str, limit: usize) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = HistoryManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let entries = manager.search(pattern, limit)?;

    if cli.json {
        let json_entries: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "timestamp": e.timestamp.to_rfc3339(),
                    "command": e.command,
                    "args": e.args,
                    "working_dir": e.working_dir,
                    "success": e.success,
                    "duration_ms": e.duration_ms,
                })
            })
            .collect();
        let output = serde_json::to_string_pretty(&json_entries)?;
        streams.write_result(&output)?;
    } else if entries.is_empty() {
        streams.write_result(&format!("No history entries matching '{pattern}'.\n"))?;
    } else {
        streams.write_result(&format!(
            "History entries matching '{}' ({}):\n\n",
            pattern,
            entries.len()
        ))?;
        for entry in &entries {
            let status = if entry.success { "" } else { " [FAILED]" };
            let args_str = if entry.args.is_empty() {
                String::new()
            } else {
                format!(" {}", entry.args.join(" "))
            };
            let timestamp = entry.timestamp.format("%Y-%m-%d %H:%M:%S");
            streams.write_result(&format!(
                "  [{timestamp}] {}{}{}\n",
                entry.command, args_str, status
            ))?;
        }
    }

    streams.finish_checked()
}

/// Clear history entries
fn run_clear(cli: &Cli, older: Option<&str>, confirm: bool) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = HistoryManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    if let Some(duration_str) = older {
        // Clear entries older than the specified duration
        let duration = parse_duration(duration_str)
            .map_err(|e| anyhow::anyhow!("Invalid duration format '{duration_str}': {e}"))?;
        let cutoff = Utc::now() - duration;

        let cleared = manager.clear_older_than(cutoff)?;

        if cli.json {
            let output = serde_json::json!({
                "cleared": cleared,
                "older_than": duration_str,
            });
            streams.write_result(&serde_json::to_string_pretty(&output)?)?;
        } else {
            streams.write_result(&format!(
                "Cleared {cleared} history entries older than {duration_str}.\n"
            ))?;
        }
    } else {
        // Clear all - requires confirmation
        if !confirm && !cli.json {
            streams.write_result("Clear ALL history entries? This cannot be undone.\n")?;
            streams.write_result(
                "Use --confirm to confirm, or --older <duration> to clear selectively.\n",
            )?;
            streams.finish_checked()?;
            bail!("Confirmation required to clear all history");
        }

        let cleared = manager.clear()?;

        if cli.json {
            let output = serde_json::json!({
                "cleared": cleared,
            });
            streams.write_result(&serde_json::to_string_pretty(&output)?)?;
        } else {
            streams.write_result(&format!("Cleared {cleared} history entries.\n"))?;
        }
    }

    streams.finish_checked()
}

/// Show history statistics
fn run_stats(cli: &Cli) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = HistoryManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let stats = manager.stats()?;

    if cli.json {
        let output = serde_json::json!({
            "total_entries": stats.total_entries,
            "oldest_entry": stats.oldest_entry.map(|t| t.to_rfc3339()),
            "newest_entry": stats.newest_entry.map(|t| t.to_rfc3339()),
            "success_count": stats.success_count,
            "failure_count": stats.failure_count,
            "commands": stats.command_counts,
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        streams.write_result("History Statistics:\n\n")?;
        streams.write_result(&format!("  Total entries: {}\n", stats.total_entries))?;

        if let Some(oldest) = stats.oldest_entry {
            streams.write_result(&format!(
                "  Oldest entry: {}\n",
                oldest.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }
        if let Some(newest) = stats.newest_entry {
            streams.write_result(&format!(
                "  Newest entry: {}\n",
                newest.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }

        streams.write_result(&format!("  Successful: {}\n", stats.success_count))?;
        streams.write_result(&format!("  Failed: {}\n", stats.failure_count))?;

        if !stats.command_counts.is_empty() {
            streams.write_result("\n  Commands used:\n")?;
            let mut counts: Vec<_> = stats.command_counts.iter().collect();
            counts.sort_by(|a, b| b.1.cmp(a.1)); // Sort by count descending
            for (cmd, count) in counts {
                streams.write_result(&format!("    {cmd}: {count}\n"))?;
            }
        }
    }

    streams.finish_checked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use crate::large_stack_test;
    use clap::Parser;
    use serial_test::serial;
    use tempfile::TempDir;

    fn create_test_cli(args: &[&str]) -> Cli {
        let mut full_args = vec!["sqry"];
        full_args.extend(args);
        // Use isolated config dir for tests to avoid host state
        unsafe {
            std::env::set_var("SQRY_CONFIG_DIR", args.last().unwrap_or(&"."));
        }
        Cli::parse_from(full_args)
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_history_list_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        let result = run_list(&cli, 100);
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_history_search_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        let result = run_search(&cli, "test", 100);
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_history_stats_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        let result = run_stats(&cli);
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_history_clear_requires_confirm() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        // Without --confirm, should fail
        let result = run_clear(&cli, None, false);
        assert!(result.is_err());
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_history_clear_with_older() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        // With --older, should succeed
        let result = run_clear(&cli, Some("30d"), false);
        assert!(result.is_ok());
    }
    }
}
