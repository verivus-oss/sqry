//! Cache command implementation

use crate::args::{CacheAction, Cli};
use anyhow::{Context, Result};
use sqry_core::cache::{CacheConfig, CacheManager, PruneOptions, PruneOutputMode, PruneReport};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Run cache management command
///
/// # Errors
/// Returns an error if cache operations fail or stats cannot be collected.
pub fn run_cache(cli: &Cli, action: &CacheAction) -> Result<()> {
    match action {
        CacheAction::Stats { path } => {
            let search_path = path.as_deref().unwrap_or(".");
            show_cache_stats(cli, search_path)
        }
        CacheAction::Clear { path, confirm } => {
            let search_path = path.as_deref().unwrap_or(".");
            clear_cache(cli, search_path, *confirm);
            Ok(())
        }
        CacheAction::Prune {
            days,
            size,
            dry_run,
            path,
        } => prune_cache(cli, *days, size.as_deref(), *dry_run, path.as_deref()),
        CacheAction::Expand {
            refresh,
            crate_name,
            dry_run,
            output,
        } => run_expand_cache(
            cli,
            *refresh,
            crate_name.as_deref(),
            *dry_run,
            output.as_deref(),
        ),
    }
}

/// Show cache statistics
fn show_cache_stats(cli: &Cli, _path: &str) -> Result<()> {
    // Create cache manager with default config
    let config = CacheConfig::from_env();
    let cache = CacheManager::new(config);
    let stats = cache.stats();

    if cli.json {
        // JSON output
        let json_stats = serde_json::json!({
            "ast_cache": {
                "hits": stats.hits,
                "misses": stats.misses,
                "evictions": stats.evictions,
                "entry_count": stats.entry_count,
                "total_bytes": stats.total_bytes,
                "total_mb": bytes_to_mb_lossy(stats.total_bytes),
                "hit_rate": stats.hit_rate(),
            },
        });
        println!("{}", serde_json::to_string_pretty(&json_stats)?);
    } else {
        // Human-readable output
        println!("AST Cache Statistics");
        println!("====================");
        println!();
        println!("Performance:");
        println!("  Hit rate:    {:.1}%", stats.hit_rate() * 100.0);
        println!("  Hits:        {}", stats.hits);
        println!("  Misses:      {}", stats.misses);
        println!("  Evictions:   {}", stats.evictions);
        println!();
        println!("Storage:");
        println!("  Entries:     {}", stats.entry_count);
        println!(
            "  Memory:      {:.2} MB",
            bytes_to_mb_lossy(stats.total_bytes)
        );
        println!();

        // Calculate effectiveness
        print_cache_effectiveness(stats.hits, stats.misses);

        // Show cache location and disk usage
        let cache_root =
            std::env::var("SQRY_CACHE_ROOT").unwrap_or_else(|_| ".sqry-cache".to_string());
        println!("Cache location: {cache_root}");

        // Show disk usage
        let disk_usage = get_disk_usage(&cache_root);
        println!();
        println!("Disk Usage:");
        println!("  Files:       {}", disk_usage.file_count);
        println!(
            "  Total size:  {:.2} MB",
            bytes_to_mb_lossy(disk_usage.bytes)
        );
    }

    Ok(())
}

/// Print estimated cache effectiveness metrics (time savings from cache hits).
fn print_cache_effectiveness(hits: usize, misses: usize) {
    if hits + misses > 0 {
        let total_accesses = hits + misses;
        let avg_savings_ms = 50; // Conservative estimate: parsing takes ~50ms
        let time_saved_ms = hits * avg_savings_ms;
        let time_saved_sec = time_saved_ms / 1000;

        println!("Estimated Impact:");
        println!("  Total accesses:  {total_accesses}");
        println!("  Time saved:      ~{time_saved_sec} seconds ({time_saved_ms} ms)");
        println!();
    }
}

struct DiskUsage {
    file_count: usize,
    bytes: u64,
}

fn get_disk_usage(cache_root: &str) -> DiskUsage {
    use walkdir::WalkDir;

    let mut file_count = 0;
    let mut total_bytes = 0u64;

    for entry in WalkDir::new(cache_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        if let Ok(metadata) = entry.metadata() {
            total_bytes += metadata.len();
            file_count += 1;
        }
    }

    DiskUsage {
        file_count,
        bytes: total_bytes,
    }
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    let narrowed = u32::try_from(value).unwrap_or(u32::MAX);
    f64::from(narrowed)
}

fn bytes_to_mb_lossy(bytes: u64) -> f64 {
    u64_to_f64_lossy(bytes) / 1_048_576.0
}

/// Clear the cache
fn clear_cache(_cli: &Cli, _path: &str, confirm: bool) {
    if !confirm {
        eprintln!("Error: Cache clear requires --confirm flag for safety");
        eprintln!();
        eprintln!("This will delete all cached AST data. Next queries will re-parse files.");
        eprintln!();
        eprintln!("To proceed, run:");
        eprintln!("  sqry cache clear --confirm");
        std::process::exit(1);
    }

    // Create cache manager and clear it
    let config = CacheConfig::from_env();
    let cache = CacheManager::new(config);

    // Get stats before clearing
    let stats_before = cache.stats();

    cache.clear();

    // Verify it's cleared
    let stats_after = cache.stats();

    println!("Cache cleared successfully");
    println!();
    println!("Removed:");
    println!("  Entries:     {}", stats_before.entry_count);
    println!(
        "  Memory:      {:.2} MB",
        bytes_to_mb_lossy(stats_before.total_bytes)
    );
    println!();
    println!("Current stats:");
    println!("  Entries:     {}", stats_after.entry_count);
    println!(
        "  Memory:      {:.2} MB",
        bytes_to_mb_lossy(stats_after.total_bytes)
    );
}

/// Prune the cache based on retention policies
fn prune_cache(
    cli: &Cli,
    days: Option<u64>,
    size_str: Option<&str>,
    dry_run: bool,
    path: Option<&str>,
) -> Result<()> {
    let options = build_prune_options(cli, days, size_str, dry_run, path)?;
    let report = execute_cache_prune(&options)?;
    write_prune_report(cli, dry_run, &report)?;

    Ok(())
}

/// Parse byte size from string (e.g., "1GB", "500MB")
fn parse_byte_size(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase();

    // Extract number and unit
    let (num_str, unit) = if s.ends_with("GB") {
        (&s[..s.len() - 2], 1024 * 1024 * 1024)
    } else if s.ends_with("MB") {
        (&s[..s.len() - 2], 1024 * 1024)
    } else if s.ends_with("KB") {
        (&s[..s.len() - 2], 1024)
    } else if s.ends_with('B') {
        (&s[..s.len() - 1], 1)
    } else {
        // Assume bytes if no unit
        (&s[..], 1)
    };

    let num: u64 = num_str.trim().parse().map_err(|_| {
        anyhow::anyhow!("Invalid size format {s}. Expected formats: 1GB, 500MB, 100KB")
    })?;

    Ok(num * unit)
}

fn build_prune_options(
    cli: &Cli,
    days: Option<u64>,
    size_str: Option<&str>,
    dry_run: bool,
    path: Option<&str>,
) -> Result<PruneOptions> {
    // Parse size if provided
    let max_size = size_str.map(parse_byte_size).transpose()?;

    // Convert days to Duration
    let max_age = days.map(|d| Duration::from_secs(d * 24 * 3600));

    // Build prune options
    let mut options = PruneOptions::new();

    if let Some(age) = max_age {
        options = options.with_max_age(age);
    }

    if let Some(size) = max_size {
        options = options.with_max_size(size);
    }

    options = options.with_dry_run(dry_run);

    let output_mode = if cli.json {
        PruneOutputMode::Json
    } else {
        PruneOutputMode::Human
    };
    options = options.with_output_mode(output_mode);

    if let Some(p) = path {
        options = options.with_target_dir(PathBuf::from(p));
    }

    Ok(options)
}

fn execute_cache_prune(options: &PruneOptions) -> Result<PruneReport> {
    let config = CacheConfig::from_env();
    let cache = CacheManager::new(config);
    cache.prune(options)
}

fn write_prune_report(cli: &Cli, dry_run: bool, report: &PruneReport) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    let header = if dry_run {
        "Cache Prune Preview (Dry Run)"
    } else {
        "Cache Prune Report"
    };
    println!("{header}");
    println!("====================");
    println!();

    if report.entries_removed == 0 {
        println!("No entries removed");
        println!("Cache is within configured limits");
        return Ok(());
    }

    println!("Entries:");
    println!("  Considered:  {}", report.entries_considered);
    println!("  Removed:     {}", report.entries_removed);
    println!("  Remaining:   {}", report.remaining_entries);
    println!();
    println!("Space:");
    println!(
        "  Reclaimed:   {:.2} MB",
        bytes_to_mb_lossy(report.bytes_removed)
    );
    println!(
        "  Remaining:   {:.2} MB",
        bytes_to_mb_lossy(report.remaining_bytes)
    );

    if dry_run {
        println!();
        println!("Run without --dry-run to actually delete files");
    }

    Ok(())
}

// =============================================================================
// Expand cache implementation
// =============================================================================

/// Default expand cache directory relative to workspace root.
const DEFAULT_EXPAND_CACHE_DIR: &str = ".sqry/expand-cache";

/// Maximum allowed expansion output size per file (10 MB).
const MAX_EXPANSION_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// Allowed character pattern for symbol names in expand cache (security validation).
fn is_valid_symbol_name(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '<' | '>' | ' ' | '&' | '\''))
}

/// Result of expanding a single crate.
#[derive(Debug)]
struct CrateExpandResult {
    crate_name: String,
    symbols_found: usize,
    generated_symbols: usize,
    cached: bool,
    skipped_reason: Option<String>,
}

/// Expand cache entry persisted as JSON.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ExpandCacheEntry {
    crate_name: String,
    rust_version: String,
    generated_at: String,
    source_hash: String,
    files: HashMap<String, ExpandCacheFileEntry>,
}

/// Per-file entry in the expand cache.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ExpandCacheFileEntry {
    original_symbols: Vec<String>,
    expanded_symbols: Vec<String>,
    generated_symbols: Vec<String>,
    confidence: String,
}

/// Run the expand cache command.
///
/// Generates or refreshes the macro expansion cache by running `cargo expand`
/// for workspace crates and diffing original vs expanded symbols.
///
/// # Errors
///
/// Returns an error if `cargo-expand` is not installed, the workspace cannot
/// be discovered, or cache files cannot be written.
fn run_expand_cache(
    cli: &Cli,
    refresh: bool,
    crate_name: Option<&str>,
    dry_run: bool,
    output: Option<&Path>,
) -> Result<()> {
    use sqry_lang_rust::macro_expander::MacroExpander;

    // Check cargo-expand availability
    if !MacroExpander::is_cargo_expand_available() {
        anyhow::bail!(
            "cargo-expand is not installed.\n\
             Install with: cargo install cargo-expand\n\
             \n\
             cargo-expand is required to generate macro expansion output.\n\
             It runs rustc to expand all macros in a crate."
        );
    }

    // Discover workspace root
    let workspace_root = discover_workspace_root()?;
    let cache_dir = output.map_or_else(
        || workspace_root.join(DEFAULT_EXPAND_CACHE_DIR),
        Path::to_path_buf,
    );

    // Discover workspace crates
    let crates = discover_workspace_crates(&workspace_root)?;

    // Filter to specific crate if requested
    let target_crates: Vec<_> = if let Some(name) = crate_name {
        let found: Vec<_> = crates.iter().filter(|(n, _)| n == name).cloned().collect();
        if found.is_empty() {
            let available: Vec<_> = crates.iter().map(|(n, _)| n.as_str()).collect();
            anyhow::bail!(
                "Crate '{}' not found in workspace.\nAvailable crates: {}",
                name,
                available.join(", ")
            );
        }
        found
    } else {
        crates
    };

    // Dry run: just list what would be expanded
    if dry_run {
        print_dry_run_plan(cli, &target_crates, &cache_dir, refresh)?;
        return Ok(());
    }

    // Ensure cache directory exists
    std::fs::create_dir_all(&cache_dir).with_context(|| {
        format!(
            "Failed to create expand cache directory: {}",
            cache_dir.display()
        )
    })?;

    // Expand each crate
    let mut results = Vec::new();
    for (name, path) in &target_crates {
        let result = expand_single_crate(name, path, &workspace_root, &cache_dir, refresh)?;
        results.push(result);
    }

    // Report results
    print_expand_results(cli, &results, &cache_dir)?;

    Ok(())
}

/// Discover the workspace root by looking for a `Cargo.toml` with `[workspace]`.
fn discover_workspace_root() -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .context("Failed to run cargo metadata")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cargo metadata failed: {stderr}");
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata output")?;

    let root = metadata["workspace_root"]
        .as_str()
        .context("workspace_root not found in cargo metadata")?;

    Ok(PathBuf::from(root))
}

/// Discover all workspace crates (name, manifest path).
fn discover_workspace_crates(workspace_root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(workspace_root)
        .output()
        .context("Failed to run cargo metadata")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cargo metadata failed: {stderr}");
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata")?;

    let packages = metadata["packages"]
        .as_array()
        .context("No packages in workspace")?;

    let mut crates = Vec::new();
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or("<unknown>").to_string();
        let manifest_path = pkg["manifest_path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_default();
        // Get the crate directory from manifest path
        let crate_dir = manifest_path
            .parent()
            .unwrap_or(workspace_root)
            .to_path_buf();
        crates.push((name, crate_dir));
    }

    crates.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(crates)
}

/// Compute SHA-256 hash of all Rust source files in a crate directory.
fn compute_source_hash(crate_dir: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use walkdir::WalkDir;

    let mut hasher = Sha256::new();
    let mut file_count = 0u64;

    let mut paths: Vec<PathBuf> = WalkDir::new(crate_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs"))
        .map(walkdir::DirEntry::into_path)
        .collect();

    // Sort for deterministic hashing
    paths.sort();

    for path in &paths {
        let content =
            std::fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
        hasher.update(&content);
        file_count += 1;
    }

    // Include file count to detect additions/deletions
    hasher.update(file_count.to_le_bytes());

    Ok(format!("{:x}", hasher.finalize()))
}

/// Check if a cached entry is fresh (source hash matches).
fn is_cache_fresh(cache_path: &Path, current_hash: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(cache_path) else {
        return false;
    };
    let Ok(entry) = serde_json::from_str::<ExpandCacheEntry>(&content) else {
        return false;
    };
    entry.source_hash == current_hash
}

/// Expand a single crate and write the cache entry.
fn expand_single_crate(
    crate_name: &str,
    crate_dir: &Path,
    workspace_root: &Path,
    cache_dir: &Path,
    refresh: bool,
) -> Result<CrateExpandResult> {
    // Compute source hash
    let source_hash = compute_source_hash(crate_dir)
        .with_context(|| format!("Failed to compute source hash for {crate_name}"))?;

    // Check freshness
    let cache_file = cache_dir.join(format!("{crate_name}.json"));
    if !refresh && is_cache_fresh(&cache_file, &source_hash) {
        return Ok(CrateExpandResult {
            crate_name: crate_name.to_string(),
            symbols_found: 0,
            generated_symbols: 0,
            cached: true,
            skipped_reason: Some("cache is fresh".to_string()),
        });
    }

    // Run cargo expand
    let expand_output = run_cargo_expand(crate_name, crate_dir)?;

    // Check size limit
    if expand_output.len() > MAX_EXPANSION_SIZE_BYTES {
        return Ok(CrateExpandResult {
            crate_name: crate_name.to_string(),
            symbols_found: 0,
            generated_symbols: 0,
            cached: false,
            skipped_reason: Some(format!(
                "expansion output too large ({} bytes, limit {})",
                expand_output.len(),
                MAX_EXPANSION_SIZE_BYTES
            )),
        });
    }

    // Parse expanded output to extract symbols
    let expanded_symbols = extract_rust_symbols_from_source(&expand_output);

    // Find original symbols from source files
    let original_symbols = collect_original_symbols(crate_dir)?;

    // Diff to find generated symbols
    let generated: Vec<String> = expanded_symbols
        .iter()
        .filter(|s| !original_symbols.contains(s))
        .filter(|s| is_valid_symbol_name(s))
        .cloned()
        .collect();

    let generated_count = generated.len();
    let total_expanded = expanded_symbols.len();

    // Compute relative file path for the entry
    let relative_src = crate_dir
        .strip_prefix(workspace_root)
        .unwrap_or(crate_dir)
        .join("src/lib.rs");

    // Build cache entry
    let entry = ExpandCacheEntry {
        crate_name: crate_name.to_string(),
        rust_version: get_rust_version(),
        generated_at: chrono_now_utc(),
        source_hash,
        files: {
            let mut map = HashMap::new();
            map.insert(
                relative_src.to_string_lossy().to_string(),
                ExpandCacheFileEntry {
                    original_symbols: original_symbols.into_iter().collect(),
                    expanded_symbols: expanded_symbols.into_iter().collect(),
                    generated_symbols: generated,
                    confidence: "heuristic".to_string(),
                },
            );
            map
        },
    };

    // Write cache file
    let json =
        serde_json::to_string_pretty(&entry).context("Failed to serialize expand cache entry")?;
    std::fs::write(&cache_file, json)
        .with_context(|| format!("Failed to write cache file: {}", cache_file.display()))?;

    Ok(CrateExpandResult {
        crate_name: crate_name.to_string(),
        symbols_found: total_expanded,
        generated_symbols: generated_count,
        cached: false,
        skipped_reason: None,
    })
}

/// Run `cargo expand` for a specific crate.
fn run_cargo_expand(crate_name: &str, crate_dir: &Path) -> Result<String> {
    let output = std::process::Command::new("cargo")
        .args(["expand", "--lib"])
        .current_dir(crate_dir)
        .output()
        .with_context(|| format!("Failed to execute cargo expand for {crate_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Try without --lib (might be a binary crate)
        let output2 = std::process::Command::new("cargo")
            .arg("expand")
            .current_dir(crate_dir)
            .output()
            .with_context(|| format!("Failed to execute cargo expand for {crate_name}"))?;

        if !output2.status.success() {
            let stderr2 = String::from_utf8_lossy(&output2.stderr);
            anyhow::bail!(
                "cargo expand failed for '{crate_name}':\n  --lib: {}\n  default: {}",
                stderr.lines().next().unwrap_or("unknown error"),
                stderr2.lines().next().unwrap_or("unknown error")
            );
        }
        return Ok(String::from_utf8_lossy(&output2.stdout).to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Extract symbol names from Rust source using simple heuristic parsing.
///
/// This extracts function, struct, enum, trait, impl, type, const, and static
/// declarations by scanning for declaration keywords. It's a lightweight
/// alternative to full tree-sitter parsing for the expand cache use case.
fn extract_rust_symbols_from_source(source: &str) -> Vec<String> {
    let mut symbols = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }

        // Extract declaration names
        if let Some(name) = extract_decl_name(trimmed, "fn ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "struct ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "enum ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "trait ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "type ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "const ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "static ") {
            symbols.push(name);
        } else if let Some(name) = extract_decl_name(trimmed, "mod ") {
            symbols.push(name);
        }
    }

    symbols.sort();
    symbols.dedup();
    symbols
}

/// Extract declaration name from a line starting with a keyword.
fn extract_decl_name(line: &str, keyword: &str) -> Option<String> {
    // Strip visibility modifiers
    let stripped = line
        .strip_prefix("pub(crate) ")
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub(in "))
        .or_else(|| line.strip_prefix("pub "))
        .unwrap_or(line);

    // Strip async/unsafe modifiers (not const — it's both a modifier and keyword)
    let stripped = stripped
        .strip_prefix("async ")
        .or_else(|| stripped.strip_prefix("unsafe "))
        .unwrap_or(stripped);

    // Only strip "const " as modifier when keyword is NOT "const " itself
    let stripped = if keyword == "const " {
        stripped
    } else {
        stripped.strip_prefix("const ").unwrap_or(stripped)
    };

    if !stripped.starts_with(keyword) {
        return None;
    }

    let rest = &stripped[keyword.len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() { None } else { Some(name) }
}

/// Collect original symbols from source files in a crate directory.
fn collect_original_symbols(crate_dir: &Path) -> Result<Vec<String>> {
    use walkdir::WalkDir;

    let mut all_symbols = Vec::new();

    for entry in WalkDir::new(crate_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read {}", entry.path().display()))?;
        let symbols = extract_rust_symbols_from_source(&content);
        all_symbols.extend(symbols);
    }

    all_symbols.sort();
    all_symbols.dedup();
    Ok(all_symbols)
}

/// Get the current Rust compiler version.
fn get_rust_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map_or_else(|| "unknown".to_string(), |v| v.trim().to_string())
}

/// Get current UTC timestamp as ISO 8601 string.
fn chrono_now_utc() -> String {
    // Use system time to avoid adding a chrono dependency
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", now.as_secs())
}

/// Print dry-run plan without actually expanding.
fn print_dry_run_plan(
    cli: &Cli,
    crates: &[(String, PathBuf)],
    cache_dir: &Path,
    refresh: bool,
) -> Result<()> {
    if cli.json {
        let plan = serde_json::json!({
            "action": "expand",
            "dry_run": true,
            "refresh": refresh,
            "cache_dir": cache_dir.display().to_string(),
            "crates": crates.iter().map(|(name, path)| {
                let hash = compute_source_hash(path).unwrap_or_default();
                let cache_file = cache_dir.join(format!("{name}.json"));
                let fresh = is_cache_fresh(&cache_file, &hash);
                serde_json::json!({
                    "name": name,
                    "path": path.display().to_string(),
                    "cache_fresh": fresh,
                    "would_expand": refresh || !fresh,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!("Macro Expansion Plan (Dry Run)");
        println!("==============================");
        println!();
        println!("Cache directory: {}", cache_dir.display());
        println!(
            "Refresh mode:   {}",
            if refresh { "force" } else { "incremental" }
        );
        println!();
        println!("Crates ({}):", crates.len());

        for (name, path) in crates {
            let hash = compute_source_hash(path).unwrap_or_default();
            let cache_file = cache_dir.join(format!("{name}.json"));
            let fresh = is_cache_fresh(&cache_file, &hash);

            let status = if fresh && !refresh {
                "skip (cache fresh)"
            } else if fresh && refresh {
                "expand (--refresh)"
            } else {
                "expand (no cache)"
            };

            println!("  {name:30} {status}");
        }

        println!();
        println!("Run without --dry-run to execute expansion.");
    }

    Ok(())
}

/// Print expand results summary.
fn print_expand_results(cli: &Cli, results: &[CrateExpandResult], cache_dir: &Path) -> Result<()> {
    if cli.json {
        let json = serde_json::json!({
            "cache_dir": cache_dir.display().to_string(),
            "results": results.iter().map(|r| {
                serde_json::json!({
                    "crate": r.crate_name,
                    "symbols_found": r.symbols_found,
                    "generated_symbols": r.generated_symbols,
                    "cached": r.cached,
                    "skipped_reason": r.skipped_reason,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("Macro Expansion Results");
        println!("=======================");
        println!();
        println!("Cache directory: {}", cache_dir.display());
        println!();

        let mut expanded = 0;
        let mut skipped = 0;
        let mut total_generated = 0;

        for r in results {
            if let Some(reason) = &r.skipped_reason {
                println!("  {}: skipped ({reason})", r.crate_name);
                skipped += 1;
            } else {
                println!(
                    "  {}: {} symbols ({} generated)",
                    r.crate_name, r.symbols_found, r.generated_symbols
                );
                expanded += 1;
                total_generated += r.generated_symbols;
            }
        }

        println!();
        println!("Summary:");
        println!("  Expanded: {expanded}");
        println!("  Skipped:  {skipped}");
        println!("  Total generated symbols: {total_generated}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_symbol_name() {
        assert!(is_valid_symbol_name("MyStruct"));
        assert!(is_valid_symbol_name("my_crate::MyStruct"));
        assert!(is_valid_symbol_name("my_crate::<MyStruct as Debug>::fmt"));
        assert!(is_valid_symbol_name("some_fn"));
        assert!(is_valid_symbol_name("CONSTANT_NAME"));
        // Control characters should be rejected
        assert!(!is_valid_symbol_name("bad\x00name"));
        assert!(!is_valid_symbol_name("bad\nname"));
        // Shell metacharacters should be rejected
        assert!(!is_valid_symbol_name("$(evil)"));
        assert!(!is_valid_symbol_name("`backtick`"));
        assert!(!is_valid_symbol_name("semi;colon"));
        assert!(!is_valid_symbol_name("pipe|char"));
    }

    #[test]
    fn test_extract_decl_name_fn() {
        assert_eq!(
            extract_decl_name("fn main() {", "fn "),
            Some("main".to_string())
        );
        assert_eq!(
            extract_decl_name("pub fn foo() {", "fn "),
            Some("foo".to_string())
        );
        assert_eq!(
            extract_decl_name("pub(crate) fn bar() {", "fn "),
            Some("bar".to_string())
        );
        assert_eq!(
            extract_decl_name("async fn baz() {", "fn "),
            Some("baz".to_string())
        );
        assert_eq!(
            extract_decl_name("pub async fn qux() {", "fn "),
            Some("qux".to_string())
        );
    }

    #[test]
    fn test_extract_decl_name_struct() {
        assert_eq!(
            extract_decl_name("struct Foo {", "struct "),
            Some("Foo".to_string())
        );
        assert_eq!(
            extract_decl_name("pub struct Bar;", "struct "),
            Some("Bar".to_string())
        );
    }

    #[test]
    fn test_extract_decl_name_no_match() {
        assert_eq!(extract_decl_name("let x = 5;", "fn "), None);
        assert_eq!(extract_decl_name("// fn foo", "fn "), None);
    }

    #[test]
    fn test_extract_rust_symbols_from_source() {
        let source = r"
pub fn hello() {}
struct MyStruct {
    field: i32,
}
enum Color { Red, Green, Blue }
const MAX: usize = 100;
mod inner {}
";
        let symbols = extract_rust_symbols_from_source(source);
        assert!(symbols.contains(&"hello".to_string()));
        assert!(symbols.contains(&"MyStruct".to_string()));
        assert!(symbols.contains(&"Color".to_string()));
        assert!(symbols.contains(&"MAX".to_string()));
        assert!(symbols.contains(&"inner".to_string()));
    }

    #[test]
    fn test_expand_cache_entry_roundtrip() {
        let entry = ExpandCacheEntry {
            crate_name: "test_crate".to_string(),
            rust_version: "rustc 1.94.0".to_string(),
            generated_at: "1234567890Z".to_string(),
            source_hash: "abc123".to_string(),
            files: {
                let mut map = HashMap::new();
                map.insert(
                    "src/lib.rs".to_string(),
                    ExpandCacheFileEntry {
                        original_symbols: vec!["Foo".to_string()],
                        expanded_symbols: vec!["Foo".to_string(), "Foo_fmt".to_string()],
                        generated_symbols: vec!["Foo_fmt".to_string()],
                        confidence: "heuristic".to_string(),
                    },
                );
                map
            },
        };

        let json = serde_json::to_string_pretty(&entry).unwrap();
        let parsed: ExpandCacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.crate_name, "test_crate");
        assert_eq!(parsed.source_hash, "abc123");
        assert_eq!(parsed.files.len(), 1);

        let file_entry = parsed.files.get("src/lib.rs").unwrap();
        assert_eq!(file_entry.generated_symbols, vec!["Foo_fmt"]);
    }

    #[test]
    fn test_is_cache_fresh_nonexistent() {
        assert!(!is_cache_fresh(
            Path::new("/nonexistent/cache.json"),
            "abc123"
        ));
    }

    #[test]
    fn test_is_cache_fresh_matching_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("test.json");
        let entry = ExpandCacheEntry {
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "0Z".to_string(),
            source_hash: "hash123".to_string(),
            files: HashMap::new(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        std::fs::write(&cache_path, json).unwrap();

        assert!(is_cache_fresh(&cache_path, "hash123"));
        assert!(!is_cache_fresh(&cache_path, "different_hash"));
    }

    #[test]
    fn test_compute_source_hash_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn main() {}").unwrap();
        std::fs::write(src_dir.join("helper.rs"), "fn helper() {}").unwrap();

        let hash1 = compute_source_hash(dir.path()).unwrap();
        let hash2 = compute_source_hash(dir.path()).unwrap();
        assert_eq!(hash1, hash2, "Hashes should be deterministic");
        assert!(!hash1.is_empty());
    }

    #[test]
    fn test_compute_source_hash_changes_on_modification() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn main() {}").unwrap();

        let hash1 = compute_source_hash(dir.path()).unwrap();

        std::fs::write(dir.path().join("lib.rs"), "fn main() { println!() }").unwrap();
        let hash2 = compute_source_hash(dir.path()).unwrap();

        assert_ne!(hash1, hash2, "Hash should change when source changes");
    }
}
