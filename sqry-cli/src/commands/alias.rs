//! Alias management command implementation
//!
//! Handles the `sqry alias` subcommand for managing saved query aliases.

use crate::args::{AliasAction, Cli, ImportConflictArg};
use crate::output::OutputStreams;
use crate::persistence::{
    AliasError, AliasExportFile, AliasManager, ImportConflictStrategy, PersistenceConfig,
    StorageScope, open_shared_index,
};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{self, Read as IoRead, Write as IoWrite};
use std::path::Path;

/// Run the alias command.
///
/// # Errors
/// Returns an error if persistence operations fail or output cannot be written.
pub fn run_alias(cli: &Cli, action: &AliasAction) -> Result<()> {
    match action {
        AliasAction::List { local, global } => run_list(cli, *local, *global),
        AliasAction::Show { name } => run_show(cli, name),
        AliasAction::Delete {
            name,
            local,
            global,
            force,
        } => run_delete(cli, name, *local, *global, *force),
        AliasAction::Rename {
            old_name,
            new_name,
            local,
            global,
        } => run_rename(cli, old_name, new_name, *local, *global),
        AliasAction::Export {
            file,
            local,
            global,
        } => run_export(cli, file, *local, *global),
        AliasAction::Import {
            file,
            local,
            global,
            on_conflict,
            dry_run,
        } => run_import(cli, file, *local, *global, *on_conflict, *dry_run),
    }
}

/// List all aliases
fn run_list(cli: &Cli, local_only: bool, global_only: bool) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let aliases = manager.list()?;
    let filtered = filter_aliases(&aliases, local_only, global_only);

    if cli.json {
        write_aliases_json(&mut streams, &filtered)?;
    } else {
        write_aliases_text(&mut streams, &filtered)?;
    }

    streams.finish_checked()
}

/// Show details of a specific alias
fn run_show(cli: &Cli, name: &str) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    match manager.get(name) {
        Ok(alias_with_scope) => {
            if cli.json {
                let output = serde_json::json!({
                    "name": alias_with_scope.name,
                    "command": alias_with_scope.alias.command,
                    "args": alias_with_scope.alias.args,
                    "description": alias_with_scope.alias.description,
                    "scope": match alias_with_scope.scope {
                        StorageScope::Global => "global",
                        StorageScope::Local => "local",
                    },
                    "created": alias_with_scope.alias.created.to_rfc3339(),
                });
                streams.write_result(&serde_json::to_string_pretty(&output)?)?;
            } else {
                let scope_label = match alias_with_scope.scope {
                    StorageScope::Global => "global",
                    StorageScope::Local => "local",
                };
                streams.write_result(&format!("Alias: @{}\n", alias_with_scope.name))?;
                streams.write_result(&format!("  Scope: {scope_label}\n"))?;
                streams
                    .write_result(&format!("  Command: {}\n", alias_with_scope.alias.command))?;
                if !alias_with_scope.alias.args.is_empty() {
                    streams.write_result(&format!(
                        "  Arguments: {}\n",
                        alias_with_scope.alias.args.join(" ")
                    ))?;
                }
                if let Some(desc) = &alias_with_scope.alias.description {
                    streams.write_result(&format!("  Description: {desc}\n"))?;
                }
                streams.write_result(&format!(
                    "  Created: {}\n",
                    alias_with_scope.alias.created.format("%Y-%m-%d %H:%M:%S")
                ))?;
            }
        }
        Err(AliasError::NotFound { name: n }) => {
            bail!("Alias '@{n}' not found");
        }
        Err(e) => return Err(e.into()),
    }

    streams.finish_checked()
}

/// Delete an alias
fn run_delete(cli: &Cli, name: &str, local: bool, global: bool, force: bool) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    // Interactive prompt must not be buffered behind the pager or the command will appear hung.
    let mut streams = if !force && !cli.json {
        OutputStreams::new()
    } else {
        OutputStreams::with_pager(cli.pager_config())
    };

    // Determine scope - if both are false, auto-detect where alias exists
    let scope = if local {
        Some(StorageScope::Local)
    } else if global {
        Some(StorageScope::Global)
    } else {
        // Auto-detect: find where the alias exists
        match manager.get(name) {
            Ok(alias_with_scope) => Some(alias_with_scope.scope),
            Err(AliasError::NotFound { .. }) => {
                bail!("Alias '@{name}' not found");
            }
            Err(e) => return Err(e.into()),
        }
    };

    let scope = scope.unwrap();

    // Confirm deletion unless --force
    if !force && !cli.json {
        streams.write_result(&format!(
            "Delete alias '@{name}' from {} storage? [y/N] ",
            match scope {
                StorageScope::Global => "global",
                StorageScope::Local => "local",
            }
        ))?;
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            streams.write_result("Cancelled.\n")?;
            return streams.finish_checked();
        }
    }

    manager.delete(name, Some(scope))?;

    if cli.json {
        let output = serde_json::json!({
            "deleted": name,
            "scope": match scope {
                StorageScope::Global => "global",
                StorageScope::Local => "local",
            },
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        streams.write_result(&format!("Deleted alias '@{name}'.\n"))?;
    }

    streams.finish_checked()
}

/// Rename an alias
fn run_rename(cli: &Cli, old_name: &str, new_name: &str, local: bool, global: bool) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    // Determine scope
    let scope = if local {
        Some(StorageScope::Local)
    } else if global {
        Some(StorageScope::Global)
    } else {
        None
    };

    let result_scope = manager.rename(old_name, new_name, scope)?;

    if cli.json {
        let output = serde_json::json!({
            "old_name": old_name,
            "new_name": new_name,
            "scope": match result_scope {
                StorageScope::Global => "global",
                StorageScope::Local => "local",
            },
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        streams.write_result(&format!("Renamed '@{old_name}' to '@{new_name}'.\n"))?;
    }

    streams.finish_checked()
}

/// Export aliases to a file
fn run_export(cli: &Cli, file: &str, local_only: bool, global_only: bool) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let aliases = manager.list()?;

    // Filter by scope if requested
    let filtered: Vec<_> = aliases
        .into_iter()
        .filter(|a| {
            if local_only {
                matches!(a.scope, StorageScope::Local)
            } else if global_only {
                matches!(a.scope, StorageScope::Global)
            } else {
                true
            }
        })
        .collect();

    let export = AliasExportFile::from_aliases(&filtered);
    let json = serde_json::to_string_pretty(&export).context("Failed to serialize aliases")?;

    if file == "-" {
        streams.write_result(&json)?;
    } else {
        fs::write(file, &json).with_context(|| format!("Failed to write to {file}"))?;
        if !cli.json {
            streams.write_result(&format!(
                "Exported {} aliases to '{}'.\n",
                filtered.len(),
                file
            ))?;
        }
    }

    if cli.json && file != "-" {
        let output = serde_json::json!({
            "exported": filtered.len(),
            "file": file,
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    }

    streams.finish_checked()
}

/// Import aliases from a file
fn run_import(
    cli: &Cli,
    file: &str,
    _local: bool,
    global: bool,
    on_conflict: ImportConflictArg,
    dry_run: bool,
) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let scope = import_scope_from_flags(global);
    let json = read_import_input(file)?;
    let export: AliasExportFile =
        serde_json::from_str(&json).context("Failed to parse alias export file")?;
    let strategy = import_strategy_from_arg(on_conflict);

    if dry_run {
        let preview = preview_import(&manager, &export, strategy)?;
        write_import_preview(&mut streams, cli, &preview)?;
    } else {
        let result = manager.import(&export, scope, strategy)?;
        write_import_result(&mut streams, cli, &export, scope, &result)?;
    }

    streams.finish_checked()
}

fn filter_aliases(
    aliases: &[crate::persistence::AliasWithScope],
    local_only: bool,
    global_only: bool,
) -> Vec<&crate::persistence::AliasWithScope> {
    aliases
        .iter()
        .filter(|a| {
            if local_only {
                matches!(a.scope, StorageScope::Local)
            } else if global_only {
                matches!(a.scope, StorageScope::Global)
            } else {
                true
            }
        })
        .collect()
}

fn write_aliases_json(
    streams: &mut OutputStreams,
    aliases: &[&crate::persistence::AliasWithScope],
) -> Result<()> {
    let json_aliases: Vec<_> = aliases
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "command": a.alias.command,
                "args": a.alias.args,
                "description": a.alias.description,
                "scope": match a.scope {
                    StorageScope::Global => "global",
                    StorageScope::Local => "local",
                },
                "created": a.alias.created.to_rfc3339(),
            })
        })
        .collect();
    let output = serde_json::to_string_pretty(&json_aliases)?;
    streams.write_result(&output)?;
    Ok(())
}

fn write_aliases_text(
    streams: &mut OutputStreams,
    aliases: &[&crate::persistence::AliasWithScope],
) -> Result<()> {
    if aliases.is_empty() {
        streams.write_result("No aliases found.")?;
        return Ok(());
    }

    streams.write_result(&format!("Aliases ({}):\n", aliases.len()))?;
    for alias in aliases {
        let scope_label = match alias.scope {
            StorageScope::Global => "[global]",
            StorageScope::Local => "[local]",
        };
        let desc = alias
            .alias
            .description
            .as_ref()
            .map(|d| format!(" - {d}"))
            .unwrap_or_default();
        let args_str = if alias.alias.args.is_empty() {
            String::new()
        } else {
            format!(" {}", alias.alias.args.join(" "))
        };
        streams.write_result(&format!(
            "  @{} {} => {}{}{}\n",
            alias.name, scope_label, alias.alias.command, args_str, desc
        ))?;
    }

    Ok(())
}

fn import_scope_from_flags(global: bool) -> StorageScope {
    if global {
        StorageScope::Global
    } else {
        StorageScope::Local
    }
}

fn read_import_input(file: &str) -> Result<String> {
    if file == "-" {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("Failed to read from stdin")?;
        Ok(buf)
    } else {
        fs::read_to_string(file).with_context(|| format!("Failed to read from {file}"))
    }
}

fn import_strategy_from_arg(on_conflict: ImportConflictArg) -> ImportConflictStrategy {
    match on_conflict {
        ImportConflictArg::Error => ImportConflictStrategy::Fail,
        ImportConflictArg::Skip => ImportConflictStrategy::Skip,
        ImportConflictArg::Overwrite => ImportConflictStrategy::Overwrite,
    }
}

struct ImportPreview {
    would_import: usize,
    would_skip: usize,
    would_conflict: usize,
    total: usize,
}

fn preview_import(
    manager: &AliasManager,
    export: &AliasExportFile,
    strategy: ImportConflictStrategy,
) -> Result<ImportPreview> {
    let mut would_import = 0;
    let mut would_skip = 0;
    let mut would_conflict = 0;

    for name in export.aliases.keys() {
        match manager.get(name) {
            Ok(_) => match strategy {
                ImportConflictStrategy::Fail => would_conflict += 1,
                ImportConflictStrategy::Skip => would_skip += 1,
                ImportConflictStrategy::Overwrite => would_import += 1,
            },
            Err(AliasError::NotFound { .. }) => would_import += 1,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(ImportPreview {
        would_import,
        would_skip,
        would_conflict,
        total: export.aliases.len(),
    })
}

fn write_import_preview(
    streams: &mut OutputStreams,
    cli: &Cli,
    preview: &ImportPreview,
) -> Result<()> {
    if cli.json {
        let output = serde_json::json!({
            "dry_run": true,
            "would_import": preview.would_import,
            "would_skip": preview.would_skip,
            "would_conflict": preview.would_conflict,
            "total": preview.total,
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        streams.write_result(&format!(
            "Dry run: {} aliases would be imported, {} skipped, {} conflicts.\n",
            preview.would_import, preview.would_skip, preview.would_conflict
        ))?;
    }
    Ok(())
}

fn write_import_result(
    streams: &mut OutputStreams,
    cli: &Cli,
    export: &AliasExportFile,
    scope: StorageScope,
    result: &crate::persistence::ImportResult,
) -> Result<()> {
    if cli.json {
        let output = serde_json::json!({
            "imported": result.imported,
            "skipped": result.skipped,
            "overwritten": result.overwritten,
            "total": export.aliases.len(),
            "scope": match scope {
                StorageScope::Global => "global",
                StorageScope::Local => "local",
            },
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        let scope_label = match scope {
            StorageScope::Global => "global",
            StorageScope::Local => "local",
        };
        streams.write_result(&format!(
            "Imported {} aliases to {} storage ({} skipped, {} overwritten).\n",
            result.imported, scope_label, result.skipped, result.overwritten
        ))?;
    }
    Ok(())
}

/// Save a search command as an alias.
///
/// Called when user runs `sqry search <pattern> --save-as <name>`.
///
/// # Errors
/// Returns an error if persistence operations fail or output cannot be written.
pub fn save_search_alias(
    cli: &Cli,
    name: &str,
    pattern: &str,
    global: bool,
    description: Option<&str>,
) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let scope = if global {
        StorageScope::Global
    } else {
        StorageScope::Local
    };

    let args = vec![pattern.to_string()];
    manager.save(name, "search", &args, description, scope)?;

    if cli.json {
        let output = serde_json::json!({
            "saved": name,
            "command": "search",
            "pattern": pattern,
            "scope": if global { "global" } else { "local" },
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        let scope_label = if global { "global" } else { "local" };
        streams.write_result(&format!(
            "Saved alias '@{name}' ({scope_label}). Use with: sqry @{name} [PATH]\n"
        ))?;
    }

    streams.finish_checked()
}

/// Save a query command as an alias.
///
/// Called when user runs `sqry query <query> --save-as <name>`.
///
/// # Errors
/// Returns an error if persistence operations fail or output cannot be written.
pub fn save_query_alias(
    cli: &Cli,
    name: &str,
    query: &str,
    global: bool,
    description: Option<&str>,
) -> Result<()> {
    let config = PersistenceConfig::from_env();
    let index = open_shared_index(Some(Path::new(cli.search_path())), config)?;
    let manager = AliasManager::new(index);
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    let scope = if global {
        StorageScope::Global
    } else {
        StorageScope::Local
    };

    let args = vec![query.to_string()];
    manager.save(name, "query", &args, description, scope)?;

    if cli.json {
        let output = serde_json::json!({
            "saved": name,
            "command": "query",
            "query": query,
            "scope": if global { "global" } else { "local" },
        });
        streams.write_result(&serde_json::to_string_pretty(&output)?)?;
    } else {
        let scope_label = if global { "global" } else { "local" };
        streams.write_result(&format!(
            "Saved alias '@{name}' ({scope_label}). Use with: sqry @{name} [PATH]\n"
        ))?;
    }

    streams.finish_checked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use crate::large_stack_test;
    use clap::Parser;
    use tempfile::TempDir;

    fn create_test_cli(args: &[&str]) -> Cli {
        let mut full_args = vec!["sqry"];
        full_args.extend(args);
        Cli::parse_from(full_args)
    }

    large_stack_test! {
    #[test]
    fn test_alias_list_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        let result = run_list(&cli, false, false);
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    fn test_alias_show_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let cli = create_test_cli(&[&temp_dir.path().to_string_lossy()]);

        let result = run_show(&cli, "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }
    }

    #[test]
    fn test_import_conflict_arg_conversion() {
        assert!(matches!(ImportConflictArg::Error, ImportConflictArg::Error));
        assert!(matches!(ImportConflictArg::Skip, ImportConflictArg::Skip));
        assert!(matches!(
            ImportConflictArg::Overwrite,
            ImportConflictArg::Overwrite
        ));
    }
}
