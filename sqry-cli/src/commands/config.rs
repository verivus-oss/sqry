//! Config command implementation for unified graph config partition
//!
//! Provides CLI access to `.sqry/graph/config/config.json` management:
//! - init: Create config with defaults
//! - show: Display effective config
//! - set: Update config keys
//! - get: Retrieve config values
//! - validate: Check config syntax/schema
//! - alias: Manage query aliases

use anyhow::{Context, Result, anyhow};
use sqry_core::config::{
    graph_config_persistence::{ConfigPersistence, LoadReport},
    graph_config_schema::{AliasEntry, GraphConfigFile},
    graph_config_store::GraphConfigStore,
};
use std::io::{self, BufRead};
use std::path::Path;

const KB_BYTES: u64 = 1024;
const MB_BYTES: u64 = KB_BYTES * 1024;
const GB_BYTES: u64 = MB_BYTES * 1024;
const KB_BYTES_F64: f64 = 1024.0;
const MB_BYTES_F64: f64 = 1024.0 * 1024.0;
const GB_BYTES_F64: f64 = 1024.0 * 1024.0 * 1024.0;

// ============================================================================
// Config subcommands
// ============================================================================

/// Initialize a new config file with defaults.
///
/// # Errors
/// Returns an error if the config store cannot be created, validation fails,
/// or the config cannot be initialized.
pub fn run_config_init(path: Option<&str>, force: bool) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    // Check if already initialized
    if store.is_initialized() && !force {
        anyhow::bail!(
            "Config already initialized at {}. Use --force to overwrite.",
            store.paths().config_file().display()
        );
    }

    // Validate filesystem (check for network filesystems)
    store
        .validate(false)
        .context("Filesystem validation failed")?;

    // Initialize config with defaults
    let persistence = ConfigPersistence::new(&store);
    let config = persistence
        .init(5000, "cli")
        .context("Failed to initialize config")?;

    println!(
        "✓ Config initialized at {}",
        store.paths().config_file().display()
    );
    println!("  Schema version: {}", config.schema_version);
    println!("  Created at: {}", config.metadata.created_at);

    Ok(())
}

/// Show effective config with source annotations.
///
/// # Errors
/// Returns an error if the config store cannot be opened, config loading fails,
/// or the requested key is invalid.
pub fn run_config_show(path: Option<&str>, json: bool, key: Option<&str>) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (config, report) = persistence.load().context("Failed to load config")?;

    print_config_diagnostics(&report);

    // If specific key requested, show only that value
    if let Some(key_path) = key {
        return show_config_key(&config, key_path, json);
    }

    // Show full config
    if json {
        let json_str =
            serde_json::to_string_pretty(&config).context("Failed to serialize config")?;
        println!("{json_str}");
    } else {
        print_config_human(&store, &config, &report);
    }

    Ok(())
}

fn print_config_diagnostics(report: &LoadReport) {
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }

    for action in &report.recovery_actions {
        eprintln!("Recovery: {action}");
    }
}

fn print_config_human(store: &GraphConfigStore, config: &GraphConfigFile, report: &LoadReport) {
    println!("Config file: {}", store.paths().config_file().display());
    println!("Schema version: {}", config.schema_version);
    println!("Integrity: {:?}", report.integrity_status);
    println!();

    println!("=== Metadata ===");
    println!("Created at: {}", config.metadata.created_at);
    println!("Updated at: {}", config.metadata.updated_at);
    println!("sqry version: {}", config.metadata.written_by.sqry_version);
    println!();

    println!("=== Limits ===");
    println!(
        "max_results: {}",
        if config.config.limits.max_results == 0 {
            "unlimited".to_string()
        } else {
            config.config.limits.max_results.to_string()
        }
    );
    println!(
        "max_depth: {}",
        if config.config.limits.max_depth == 0 {
            "unlimited".to_string()
        } else {
            config.config.limits.max_depth.to_string()
        }
    );
    println!(
        "max_bytes_per_file: {}",
        if config.config.limits.max_bytes_per_file == 0 {
            "unlimited".to_string()
        } else {
            format_bytes(config.config.limits.max_bytes_per_file)
        }
    );
    println!(
        "max_files: {}",
        if config.config.limits.max_files == 0 {
            "unlimited".to_string()
        } else {
            config.config.limits.max_files.to_string()
        }
    );
    println!();

    println!("=== Locking ===");
    println!(
        "write_lock_timeout_ms: {}",
        config.config.locking.write_lock_timeout_ms
    );
    println!(
        "stale_lock_timeout_ms: {}",
        config.config.locking.stale_lock_timeout_ms
    );
    println!(
        "stale_takeover_policy: {}",
        config.config.locking.stale_takeover_policy
    );
    println!();

    println!("=== Output ===");
    println!(
        "default_pagination: {}",
        config.config.output.default_pagination
    );
    println!("page_size: {}", config.config.output.page_size);
    println!(
        "max_preview_bytes: {}",
        format_bytes(config.config.output.max_preview_bytes)
    );
    println!();

    println!("=== Parallelism ===");
    println!(
        "max_threads: {}",
        if config.config.parallelism.max_threads == 0 {
            "auto-detect".to_string()
        } else {
            config.config.parallelism.max_threads.to_string()
        }
    );
    println!();

    println!("=== Aliases ({}) ===", config.config.aliases.len());
    for (name, alias) in &config.config.aliases {
        println!("  {}: {}", name, alias.query);
        if let Some(desc) = &alias.description {
            println!("    Description: {desc}");
        }
    }
}

/// Show a specific config key
fn show_config_key(config: &GraphConfigFile, key_path: &str, json: bool) -> Result<()> {
    // Parse key path (e.g., "limits.max_results")
    let parts: Vec<&str> = key_path.split('.').collect();

    if parts.is_empty() {
        anyhow::bail!("Invalid key path: {key_path}");
    }

    // Navigate the config structure
    let value = match parts[0] {
        "limits" => match parts.get(1) {
            Some(&"max_results") => serde_json::to_value(config.config.limits.max_results)?,
            Some(&"max_depth") => serde_json::to_value(config.config.limits.max_depth)?,
            Some(&"max_bytes_per_file") => {
                serde_json::to_value(config.config.limits.max_bytes_per_file)?
            }
            Some(&"max_files") => serde_json::to_value(config.config.limits.max_files)?,
            _ => anyhow::bail!("Unknown limits key: {:?}", parts.get(1)),
        },
        "locking" => match parts.get(1) {
            Some(&"write_lock_timeout_ms") => {
                serde_json::to_value(config.config.locking.write_lock_timeout_ms)?
            }
            Some(&"stale_lock_timeout_ms") => {
                serde_json::to_value(config.config.locking.stale_lock_timeout_ms)?
            }
            Some(&"stale_takeover_policy") => {
                serde_json::to_value(&config.config.locking.stale_takeover_policy)?
            }
            _ => anyhow::bail!("Unknown locking key: {:?}", parts.get(1)),
        },
        "output" => match parts.get(1) {
            Some(&"default_pagination") => {
                serde_json::to_value(config.config.output.default_pagination)?
            }
            Some(&"page_size") => serde_json::to_value(config.config.output.page_size)?,
            Some(&"max_preview_bytes") => {
                serde_json::to_value(config.config.output.max_preview_bytes)?
            }
            _ => anyhow::bail!("Unknown output key: {:?}", parts.get(1)),
        },
        "parallelism" => match parts.get(1) {
            Some(&"max_threads") => serde_json::to_value(config.config.parallelism.max_threads)?,
            _ => anyhow::bail!("Unknown parallelism key: {:?}", parts.get(1)),
        },
        _ => anyhow::bail!("Unknown config section: {}", parts[0]),
    };

    if json {
        let json_str = serde_json::to_string_pretty(&value)?;
        println!("{json_str}");
    } else {
        println!("{value}");
    }

    Ok(())
}

/// Set a config key to a new value.
///
/// # Errors
/// Returns an error if the config cannot be loaded, validation fails, or the key is invalid.
pub fn run_config_set(path: Option<&str>, key: &str, value: &str, yes: bool) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (mut config, _report) = persistence.load().context("Failed to load config")?;

    // Store old value for diff
    let old_value = get_config_value(&config, key)?;

    // Set new value
    set_config_value(&mut config, key, value)?;

    // Validate the updated config
    config
        .validate()
        .context("Config validation failed after update")?;

    // Show diff and confirm
    if !yes {
        println!("Config change:");
        println!("  {key}: {old_value} → {value}");
        println!();
        print!("Apply this change? [y/N] ");

        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;

        if !line.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Save updated config
    persistence
        .save(&mut config, 5000, "cli")
        .context("Failed to save config")?;

    println!("✓ Config updated: {key} = {value}");

    Ok(())
}

/// Get current value of a config key
fn get_config_value(config: &GraphConfigFile, key: &str) -> Result<String> {
    let parts: Vec<&str> = key.split('.').collect();

    let value = match parts[0] {
        "limits" => match parts.get(1) {
            Some(&"max_results") => config.config.limits.max_results.to_string(),
            Some(&"max_depth") => config.config.limits.max_depth.to_string(),
            Some(&"max_bytes_per_file") => config.config.limits.max_bytes_per_file.to_string(),
            Some(&"max_files") => config.config.limits.max_files.to_string(),
            _ => anyhow::bail!("Unknown limits key: {:?}", parts.get(1)),
        },
        "locking" => match parts.get(1) {
            Some(&"write_lock_timeout_ms") => {
                config.config.locking.write_lock_timeout_ms.to_string()
            }
            Some(&"stale_lock_timeout_ms") => {
                config.config.locking.stale_lock_timeout_ms.to_string()
            }
            Some(&"stale_takeover_policy") => config.config.locking.stale_takeover_policy.clone(),
            _ => anyhow::bail!("Unknown locking key: {:?}", parts.get(1)),
        },
        "output" => match parts.get(1) {
            Some(&"default_pagination") => config.config.output.default_pagination.to_string(),
            Some(&"page_size") => config.config.output.page_size.to_string(),
            Some(&"max_preview_bytes") => config.config.output.max_preview_bytes.to_string(),
            _ => anyhow::bail!("Unknown output key: {:?}", parts.get(1)),
        },
        "parallelism" => match parts.get(1) {
            Some(&"max_threads") => config.config.parallelism.max_threads.to_string(),
            _ => anyhow::bail!("Unknown parallelism key: {:?}", parts.get(1)),
        },
        _ => anyhow::bail!("Unknown config section: {}", parts[0]),
    };

    Ok(value)
}

/// Set a config value
fn set_config_value(config: &mut GraphConfigFile, key: &str, value: &str) -> Result<()> {
    let parts: Vec<&str> = key.split('.').collect();

    match parts[0] {
        "limits" => match parts.get(1) {
            Some(&"max_results") => {
                config.config.limits.max_results = value
                    .parse()
                    .context("Invalid value for max_results (expected u64)")?;
            }
            Some(&"max_depth") => {
                config.config.limits.max_depth = value
                    .parse()
                    .context("Invalid value for max_depth (expected u64)")?;
            }
            Some(&"max_bytes_per_file") => {
                config.config.limits.max_bytes_per_file = value
                    .parse()
                    .context("Invalid value for max_bytes_per_file (expected u64)")?;
            }
            Some(&"max_files") => {
                config.config.limits.max_files = value
                    .parse()
                    .context("Invalid value for max_files (expected u64)")?;
            }
            _ => anyhow::bail!("Unknown limits key: {:?}", parts.get(1)),
        },
        "locking" => match parts.get(1) {
            Some(&"write_lock_timeout_ms") => {
                config.config.locking.write_lock_timeout_ms = value
                    .parse()
                    .context("Invalid value for write_lock_timeout_ms (expected u64)")?;
            }
            Some(&"stale_lock_timeout_ms") => {
                config.config.locking.stale_lock_timeout_ms = value
                    .parse()
                    .context("Invalid value for stale_lock_timeout_ms (expected u64)")?;
            }
            Some(&"stale_takeover_policy") => {
                if !["deny", "warn", "allow"].contains(&value) {
                    anyhow::bail!("Invalid stale_takeover_policy (expected: deny, warn, or allow)");
                }
                config.config.locking.stale_takeover_policy = value.to_string();
            }
            _ => anyhow::bail!("Unknown locking key: {:?}", parts.get(1)),
        },
        "output" => match parts.get(1) {
            Some(&"default_pagination") => {
                config.config.output.default_pagination = value
                    .parse()
                    .context("Invalid value for default_pagination (expected bool)")?;
            }
            Some(&"page_size") => {
                let val: u64 = value
                    .parse()
                    .context("Invalid value for page_size (expected u64)")?;
                if val == 0 {
                    anyhow::bail!("page_size must be greater than 0");
                }
                config.config.output.page_size = val;
            }
            Some(&"max_preview_bytes") => {
                config.config.output.max_preview_bytes = value
                    .parse()
                    .context("Invalid value for max_preview_bytes (expected u64)")?;
            }
            _ => anyhow::bail!("Unknown output key: {:?}", parts.get(1)),
        },
        "parallelism" => match parts.get(1) {
            Some(&"max_threads") => {
                config.config.parallelism.max_threads = value
                    .parse()
                    .context("Invalid value for max_threads (expected u64)")?;
            }
            _ => anyhow::bail!("Unknown parallelism key: {:?}", parts.get(1)),
        },
        _ => anyhow::bail!("Unknown config section: {}", parts[0]),
    }

    Ok(())
}

/// Get a single config value.
///
/// # Errors
/// Returns an error if the config cannot be loaded or the key is invalid.
pub fn run_config_get(path: Option<&str>, key: &str) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (config, _report) = persistence.load().context("Failed to load config")?;

    let value = get_config_value(&config, key)?;
    println!("{value}");

    Ok(())
}

/// Validate config file.
///
/// # Errors
/// Returns an error if the config cannot be loaded or fails validation.
pub fn run_config_validate(path: Option<&str>) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);

    match persistence.load() {
        Ok((config, report)) => {
            // Check for warnings
            if !report.warnings.is_empty() {
                println!("⚠ Warnings:");
                for warning in &report.warnings {
                    println!("  - {warning}");
                }
                println!();
            }

            // Validate schema
            match config.validate() {
                Ok(()) => {
                    println!("✓ Config is valid");
                    println!("  Schema version: {}", config.schema_version);
                    println!("  Integrity: {:?}", report.integrity_status);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("✗ Config validation failed: {e}");
                    Err(anyhow!("Validation failed"))
                }
            }
        }
        Err(e) => {
            eprintln!("✗ Failed to load config: {e}");
            Err(anyhow!("Load failed"))
        }
    }
}

/// Create or update an alias.
///
/// # Errors
/// Returns an error if the config cannot be loaded or saved.
pub fn run_config_alias_set(
    path: Option<&str>,
    name: &str,
    query: &str,
    description: Option<&str>,
) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (mut config, _report) = persistence.load().context("Failed to load config")?;

    // Check if alias already exists
    let is_update = config.config.aliases.contains_key(name);

    // Create/update alias
    let alias_entry = AliasEntry::new(query, description.map(String::from));
    config.config.aliases.insert(name.to_string(), alias_entry);

    // Save updated config
    persistence
        .save(&mut config, 5000, "cli")
        .context("Failed to save config")?;

    if is_update {
        println!("✓ Alias '{name}' updated");
    } else {
        println!("✓ Alias '{name}' created");
    }
    println!("  Query: {query}");
    if let Some(desc) = description {
        println!("  Description: {desc}");
    }

    Ok(())
}

/// List all aliases.
///
/// # Errors
/// Returns an error if the config cannot be loaded or aliases cannot be serialized.
pub fn run_config_alias_list(path: Option<&str>, json: bool) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (config, _report) = persistence.load().context("Failed to load config")?;

    if config.config.aliases.is_empty() {
        println!("No aliases defined.");
        return Ok(());
    }

    if json {
        let json_str = serde_json::to_string_pretty(&config.config.aliases)
            .context("Failed to serialize aliases")?;
        println!("{json_str}");
    } else {
        println!("Aliases ({}):", config.config.aliases.len());
        for (name, alias) in &config.config.aliases {
            println!();
            println!("  {name}");
            println!("    Query: {}", alias.query);
            if let Some(desc) = &alias.description {
                println!("    Description: {desc}");
            }
            println!("    Created: {}", alias.created_at);
            println!("    Updated: {}", alias.updated_at);
        }
    }

    Ok(())
}

/// Remove an alias.
///
/// # Errors
/// Returns an error if the config cannot be loaded or the alias does not exist.
pub fn run_config_alias_remove(path: Option<&str>, name: &str) -> Result<()> {
    let project_root = Path::new(path.unwrap_or("."));
    let store = GraphConfigStore::new(project_root).context("Failed to create config store")?;

    if !store.is_initialized() {
        anyhow::bail!("Config not initialized. Run 'sqry config init' first.");
    }

    let persistence = ConfigPersistence::new(&store);
    let (mut config, _report) = persistence.load().context("Failed to load config")?;

    // Check if alias exists
    if !config.config.aliases.contains_key(name) {
        anyhow::bail!("Alias '{name}' not found");
    }

    // Remove alias
    config.config.aliases.remove(name);

    // Save updated config
    persistence
        .save(&mut config, 5000, "cli")
        .context("Failed to save config")?;

    println!("✓ Alias '{name}' removed");

    Ok(())
}

// ============================================================================
// Helper functions
// ============================================================================

/// Format bytes as human-readable string
fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "unlimited".to_string();
    }

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
