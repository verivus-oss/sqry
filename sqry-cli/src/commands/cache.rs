//! Cache command implementation

use crate::args::{CacheAction, Cli};
use anyhow::Result;
use sqry_core::cache::{CacheConfig, CacheManager, PruneOptions, PruneOutputMode, PruneReport};
use std::path::PathBuf;
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
