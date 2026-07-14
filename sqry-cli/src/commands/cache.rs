//! Cache command implementation

use crate::args::{CacheAction, Cli};
use anyhow::{Context, Result};
use sqry_core::cache::{CacheConfig, CacheManager, PruneOptions, PruneOutputMode, PruneReport};
use sqry_lang_rust::macro_boundaries::expand_cache::{
    EXPAND_CACHE_SCHEMA_VERSION, ExpandCache, ExpandCacheEntry, GeneratedSymbol,
    compute_crate_source_hash,
};
use sqry_lang_rust::macro_boundaries::generated_symbols::{
    SymbolInfo, diff_symbols, extract_symbols_from_source, extract_symbols_from_source_in_module,
};
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

/// Result of expanding a single crate.
#[derive(Debug)]
struct CrateExpandResult {
    crate_name: String,
    symbols_found: usize,
    generated_symbols: usize,
    cached: bool,
    skipped_reason: Option<String>,
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

/// Check if a cached entry is fresh: it exists, parses, matches the current
/// schema version, and its recorded `source_hash` matches `current_hash`.
///
/// The source hash itself is computed by the shared
/// [`compute_crate_source_hash`], so the writer and the index-side consumer
/// agree byte-for-byte.
fn is_cache_fresh(cache_path: &Path, current_hash: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(cache_path) else {
        return false;
    };
    let Ok(entry) = serde_json::from_str::<ExpandCacheEntry>(&content) else {
        return false;
    };
    entry.schema_version == EXPAND_CACHE_SCHEMA_VERSION && entry.source_hash == current_hash
}

/// Expand a single crate and write the cache entry.
///
/// The output is a per-crate, qualified, kinded `generated_symbols` list, built
/// by tree-sitter parsing (not a line heuristic) and routed through
/// [`ExpandCache::write`] so writer and index-side reader share one path/key
/// scheme. `workspace_root` is retained for signature compatibility with the
/// caller; per-file attribution is not recoverable from `cargo expand` output,
/// so ownership is carried structurally via each symbol's module scope chain.
fn expand_single_crate(
    crate_name: &str,
    crate_dir: &Path,
    _workspace_root: &Path,
    cache_dir: &Path,
    refresh: bool,
) -> Result<CrateExpandResult> {
    // Compute the source hash via the SHARED hasher so `is_fresh` is comparable
    // with the index-side consumer.
    let source_hash = compute_crate_source_hash(crate_dir)
        .with_context(|| format!("Failed to compute source hash for {crate_name}"))?;

    // Check freshness.
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

    // Run cargo expand (the only subprocess; confined to this writer command).
    let (expand_output, expand_target) = run_cargo_expand(crate_name, crate_dir)?;

    // Check size limit.
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

    // Tree-sitter qualified extraction of the expanded output.
    let expanded_symbols = extract_symbols_from_source(&expand_output, crate_name);
    let total_expanded = expanded_symbols.len();

    // Tree-sitter qualified extraction of every original `.rs` file in the crate,
    // each qualified with its crate-relative module path so both sides of the
    // diff share one namespace.
    let original_symbols = collect_original_symbols(crate_dir, crate_name, expand_target)?;

    // Diff (keyed on the crate-prefixed qualified_name) to find generated names.
    let diff = diff_symbols(&original_symbols, &expanded_symbols);
    let generated_names: std::collections::HashSet<&str> =
        diff.generated.iter().map(String::as_str).collect();

    // Materialise a `GeneratedSymbol` (structured pieces) for each generated
    // qualified name, deduped by the consumer-facing
    // `(scope_segments, impl_type, simple_name)` tuple. The crate-prefixed
    // qualified_name is the diff key ONLY and is not persisted.
    let mut seen: std::collections::HashSet<(String, Option<String>, String)> =
        std::collections::HashSet::new();
    let mut generated_symbols: Vec<GeneratedSymbol> = Vec::new();
    for sym in &expanded_symbols {
        if !generated_names.contains(sym.qualified_name.as_str()) {
            continue;
        }
        let scope_key = sym
            .scope_segments
            .iter()
            .map(|seg| format!("{}:{}", u8::from(seg.is_module), seg.name))
            .collect::<Vec<_>>()
            .join("|");
        let dedup_key = (scope_key, sym.impl_type.clone(), sym.simple_name.clone());
        if !seen.insert(dedup_key) {
            continue;
        }
        generated_symbols.push(sym_to_generated(sym));
    }

    let generated_count = generated_symbols.len();

    // Build and write the cache entry through ExpandCache (shared path/key).
    let entry = ExpandCacheEntry {
        schema_version: EXPAND_CACHE_SCHEMA_VERSION,
        crate_name: crate_name.to_string(),
        rust_version: get_rust_version(),
        generated_at: chrono_now_utc(),
        source_hash,
        confidence: "heuristic".to_string(),
        generated_symbols,
    };

    let cache = ExpandCache::new(cache_dir.to_path_buf())
        .with_context(|| format!("Failed to open expand cache dir {}", cache_dir.display()))?;
    cache
        .write(crate_name, &entry)
        .with_context(|| format!("Failed to write expand cache for {crate_name}"))?;

    Ok(CrateExpandResult {
        crate_name: crate_name.to_string(),
        symbols_found: total_expanded,
        generated_symbols: generated_count,
        cached: false,
        skipped_reason: None,
    })
}

/// Convert an extracted [`SymbolInfo`] into a persisted [`GeneratedSymbol`],
/// carrying only the structured pieces (never the crate-prefixed diff key).
fn sym_to_generated(sym: &SymbolInfo) -> GeneratedSymbol {
    GeneratedSymbol {
        simple_name: sym.simple_name.clone(),
        scope_segments: sym.scope_segments.clone(),
        impl_type: sym.impl_type.clone(),
        kind: sym.kind,
    }
}

/// Which `cargo expand` target produced the output.
///
/// Drives which crate-root file the original-symbol collector must EXCLUDE so
/// the original set matches the expanded set (a lib+bin crate has two crate
/// roots, `lib.rs` and `main.rs`, both mapping to the crate-root module path;
/// only the expanded target's root belongs in the diff).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpandTarget {
    /// `cargo expand --lib` (the library target).
    Lib,
    /// `cargo expand` with no explicit target (a binary crate).
    Default,
}

/// Run `cargo expand` for a specific crate, reporting which target was expanded.
fn run_cargo_expand(crate_name: &str, crate_dir: &Path) -> Result<(String, ExpandTarget)> {
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
        return Ok((
            String::from_utf8_lossy(&output2.stdout).to_string(),
            ExpandTarget::Default,
        ));
    }

    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        ExpandTarget::Lib,
    ))
}

/// Collect original (unexpanded) qualified symbols from every `.rs` file in a
/// crate directory via tree-sitter, matching the extraction used on the expanded
/// output so `diff_symbols` compares like with like.
///
/// Each file's symbols are qualified with that file's crate-relative module path
/// (via [`ModuleResolver::compute_file_module_path`]) so a `src/foo.rs` item is
/// keyed `crate::foo::bar`, exactly as `cargo expand` inlines it. Without this,
/// an ordinary file-backed-module item would look "generated" to `diff_symbols`.
///
/// A lib+bin crate has two crate roots (`src/lib.rs` and `src/main.rs`), both
/// mapping to the crate-root module path. Only the EXPANDED target's root (plus
/// its module tree) belongs in the diff, so when the lib was expanded the binary
/// roots (`src/main.rs` and `src/bin/*.rs`) are skipped, and when a binary was
/// expanded the library root (`src/lib.rs`) is skipped. Otherwise a same-named
/// crate-root item from the other target could mask a genuinely macro-generated
/// symbol (false negative).
fn collect_original_symbols(
    crate_dir: &Path,
    crate_name: &str,
    expand_target: ExpandTarget,
) -> Result<Vec<SymbolInfo>> {
    use sqry_lang_rust::module_resolver::ModuleResolver;
    use walkdir::WalkDir;

    let resolver = ModuleResolver::new(crate_dir.to_path_buf());
    let mut all_symbols = Vec::new();

    for entry in WalkDir::new(crate_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let path = entry.path();
        if excluded_crate_root(path, crate_dir, expand_target) {
            continue;
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let file_module_path = resolver.compute_file_module_path(path);
        all_symbols.extend(extract_symbols_from_source_in_module(
            &content,
            crate_name,
            file_module_path.as_deref(),
        ));
    }

    Ok(all_symbols)
}

/// Whether an original `.rs` file belongs to a crate target OTHER than the one
/// `cargo expand` expanded, and must therefore be excluded from the diff.
///
/// When the library was expanded, the binary roots (`src/main.rs`, `src/bin/*`)
/// are foreign; when a binary was expanded, the library root (`src/lib.rs`) is.
fn excluded_crate_root(path: &Path, crate_dir: &Path, expand_target: ExpandTarget) -> bool {
    // Match Cargo's target roots CRATE-RELATIVE. A previous version scanned the
    // absolute path for any component named `bin`, which wrongly excluded a crate
    // living under a directory named `bin` (dropping every file) and legitimate
    // library modules like `src/foo/bin/bar.rs`. `Path::{==, starts_with}` here
    // match whole components, so `src/bin/x.rs` is caught but `src/bingo.rs` and
    // `src/foo/bin/bar.rs` are not.
    let Ok(rel) = path.strip_prefix(crate_dir) else {
        return false;
    };
    let is_src_main = rel == Path::new("src/main.rs");
    let in_src_bin = rel.starts_with("src/bin");
    let is_src_lib = rel == Path::new("src/lib.rs");

    match expand_target {
        // The lib was expanded: exclude the binary targets (`src/main.rs` and
        // any `src/bin/*.rs`), which are not part of the library crate.
        ExpandTarget::Lib => is_src_main || in_src_bin,
        // A binary was expanded: exclude the library root (`src/lib.rs`).
        ExpandTarget::Default => is_src_lib,
    }
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
                let hash = compute_crate_source_hash(path).unwrap_or_default();
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
            let hash = compute_crate_source_hash(path).unwrap_or_default();
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
    use sqry_lang_rust::macro_boundaries::expand_cache::GeneratedSymbolKind;

    #[test]
    fn test_sym_to_generated_carries_structured_pieces_not_diff_key() {
        // Extract a derive-style method and confirm the persisted GeneratedSymbol
        // carries structured pieces (never the crate-prefixed diff key).
        let source = r"
mod widgets {
    pub struct Gizmo;
    impl Clone for Gizmo {
        fn clone(&self) -> Self { Gizmo }
    }
}
";
        let symbols = extract_symbols_from_source(source, "my_crate");
        let clone = symbols
            .iter()
            .find(|s| s.qualified_name == "my_crate::widgets::<Gizmo as Clone>::clone")
            .expect("trait-impl method must be extracted");

        let generated = sym_to_generated(clone);
        assert_eq!(generated.simple_name, "clone");
        // Plain impl type, trait dropped (matches the live graph naming).
        assert_eq!(generated.impl_type.as_deref(), Some("Gizmo"));
        assert_eq!(generated.kind, GeneratedSymbolKind::Method);
        // Only the module scope survives (the impl pushes no segment).
        assert_eq!(generated.scope_segments.len(), 1);
        assert_eq!(generated.scope_segments[0].name, "widgets");
        assert!(generated.scope_segments[0].is_module);
    }

    #[test]
    fn test_collect_original_symbols_reads_all_rs_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn root_fn() {}\n").unwrap();
        std::fs::write(src.join("helper.rs"), "pub fn helper_fn() {}\n").unwrap();

        let symbols = collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Lib).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.simple_name.as_str()).collect();
        assert!(names.contains(&"root_fn"));
        assert!(names.contains(&"helper_fn"));
    }

    #[test]
    fn test_collect_original_symbols_qualifies_file_module_path() {
        // A file-backed module (`src/foo.rs`) item must be keyed
        // `crate::foo::bar`, exactly as `cargo expand` inlines it, so the diff
        // does NOT misclassify it as macro-generated. The crate root (`lib.rs`)
        // stays at the crate root.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn root_fn() {}\n").unwrap();
        std::fs::write(src.join("foo.rs"), "pub fn bar() {}\n").unwrap();
        std::fs::write(src.join("nested/deep.rs"), "pub fn deep_fn() {}\n").unwrap();

        let symbols = collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Lib).unwrap();
        let qnames: Vec<&str> = symbols.iter().map(|s| s.qualified_name.as_str()).collect();

        // Crate root: no module prefix.
        assert!(
            qnames.contains(&"my_crate::root_fn"),
            "crate-root fn must stay at the crate root, got {qnames:?}"
        );
        // `src/foo.rs` -> module `foo`.
        assert!(
            qnames.contains(&"my_crate::foo::bar"),
            "src/foo.rs item must be qualified with its file module path, got {qnames:?}"
        );
        // `src/nested/deep.rs` -> module `nested::deep`.
        assert!(
            qnames.contains(&"my_crate::nested::deep::deep_fn"),
            "nested file module path must be reflected, got {qnames:?}"
        );

        // The `foo::bar` original symbol carries the leading `foo` module
        // segment, matching what `cargo expand` produces (so `scope_segments`
        // compares equal in the dedup key too).
        let bar = symbols
            .iter()
            .find(|s| s.simple_name == "bar")
            .expect("bar extracted");
        assert_eq!(bar.scope_segments.len(), 1);
        assert_eq!(bar.scope_segments[0].name, "foo");
        assert!(bar.scope_segments[0].is_module);
    }

    #[test]
    fn test_file_backed_module_item_not_flagged_generated() {
        // The Codex blocker end-to-end (writer/diff side): an ordinary item in a
        // file-backed module must NOT appear in `diff.generated`. We simulate
        // `cargo expand` output (everything inlined into `mod foo { .. }`) and
        // the original file tree, and prove the diff is clean.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod foo;\npub fn root_fn() {}\n").unwrap();
        std::fs::write(src.join("foo.rs"), "pub fn bar() {}\n").unwrap();

        // Expanded output: cargo expand inlines `src/foo.rs` as `mod foo { .. }`.
        let expanded_output = "pub mod foo { pub fn bar() {} }\npub fn root_fn() {}\n";
        let expanded = extract_symbols_from_source(expanded_output, "my_crate");
        let original = collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Lib).unwrap();

        let diff = diff_symbols(&original, &expanded);
        assert!(
            !diff.generated.iter().any(|g| g.contains("bar")),
            "ordinary file-backed-module item `bar` must NOT be flagged generated; got {:?}",
            diff.generated
        );
        assert!(
            !diff.generated.iter().any(|g| g.contains("root_fn")),
            "crate-root item must not be flagged generated; got {:?}",
            diff.generated
        );
    }

    #[test]
    fn test_lib_expand_excludes_binary_roots() {
        // A lib+bin crate: `cargo expand --lib` expands the library, so the
        // binary roots (`src/main.rs`, `src/bin/tool.rs`) must be EXCLUDED from
        // the original set. Otherwise a same-named crate-root item in main.rs
        // could mask a genuinely macro-generated lib crate-root symbol.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        std::fs::write(src.join("main.rs"), "fn main_only() {}\n").unwrap();
        std::fs::write(src.join("bin/tool.rs"), "fn tool_only() {}\n").unwrap();

        let symbols = collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Lib).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.simple_name.as_str()).collect();
        assert!(names.contains(&"lib_fn"), "lib root must be included");
        assert!(
            !names.contains(&"main_only"),
            "src/main.rs must be excluded when the lib is expanded"
        );
        assert!(
            !names.contains(&"tool_only"),
            "src/bin/*.rs must be excluded when the lib is expanded"
        );
    }

    #[test]
    fn test_lib_module_named_bin_not_excluded() {
        // Regression: a legitimate library module tree under a directory named
        // `bin` (e.g. src/foo/bin/bar.rs) must NOT be excluded on --lib expand.
        // The prior check scanned for ANY `bin` path component and wrongly
        // dropped these files, so their real symbols were misclassified as
        // macro-generated. The exclusion is crate-relative to `src/bin/*` only.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("foo/bin")).unwrap();
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        std::fs::write(src.join("foo/bin/bar.rs"), "pub fn deep_lib_fn() {}\n").unwrap();
        // A genuine foreign binary root, for contrast.
        std::fs::write(src.join("bin/tool.rs"), "fn tool_only() {}\n").unwrap();

        let symbols = collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Lib).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.simple_name.as_str()).collect();
        assert!(
            names.contains(&"deep_lib_fn"),
            "src/foo/bin/bar.rs is a library module and must NOT be excluded"
        );
        assert!(names.contains(&"lib_fn"), "lib root must be included");
        assert!(
            !names.contains(&"tool_only"),
            "real src/bin/*.rs must still be excluded on --lib expand"
        );
    }

    #[test]
    fn test_default_expand_excludes_lib_root() {
        // A binary was expanded (no lib target found): the library root
        // (`src/lib.rs`) is foreign and must be excluded.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        std::fs::write(src.join("main.rs"), "fn main_fn() {}\n").unwrap();

        let symbols =
            collect_original_symbols(dir.path(), "my_crate", ExpandTarget::Default).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.simple_name.as_str()).collect();
        assert!(names.contains(&"main_fn"), "binary root must be included");
        assert!(
            !names.contains(&"lib_fn"),
            "src/lib.rs must be excluded when a binary is expanded"
        );
    }

    #[test]
    fn test_is_cache_fresh_nonexistent() {
        assert!(!is_cache_fresh(
            Path::new("/nonexistent/cache.json"),
            "abc123"
        ));
    }

    #[test]
    fn test_is_cache_fresh_matching_hash_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("test.json");
        let entry = ExpandCacheEntry {
            schema_version: EXPAND_CACHE_SCHEMA_VERSION,
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "0Z".to_string(),
            source_hash: "hash123".to_string(),
            confidence: "heuristic".to_string(),
            generated_symbols: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        std::fs::write(&cache_path, &json).unwrap();

        assert!(is_cache_fresh(&cache_path, "hash123"));
        assert!(!is_cache_fresh(&cache_path, "different_hash"));

        // A matching hash but stale schema version is NOT fresh.
        let mut stale = entry;
        stale.schema_version = EXPAND_CACHE_SCHEMA_VERSION - 1;
        std::fs::write(&cache_path, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(!is_cache_fresh(&cache_path, "hash123"));
    }

    #[test]
    fn test_writer_round_trip_through_expand_cache() {
        // End-to-end (minus the cargo-expand subprocess): a written entry is
        // readable back through ExpandCache with qualified, kinded symbols.
        let source = r"
pub struct Foo;
impl Foo {
    pub fn generated_new() -> Self { Foo }
}
";
        let symbols = extract_symbols_from_source(source, "round_trip");
        let generated: Vec<GeneratedSymbol> = symbols
            .iter()
            .filter(|s| s.simple_name == "generated_new")
            .map(sym_to_generated)
            .collect();
        assert_eq!(generated.len(), 1);

        let entry = ExpandCacheEntry {
            schema_version: EXPAND_CACHE_SCHEMA_VERSION,
            crate_name: "round_trip".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "0Z".to_string(),
            source_hash: "hash".to_string(),
            confidence: "heuristic".to_string(),
            generated_symbols: generated,
        };

        let dir = tempfile::tempdir().unwrap();
        let cache = ExpandCache::new(dir.path().to_path_buf()).unwrap();
        cache.write("round_trip", &entry).unwrap();

        let read_back = cache.read("round_trip").unwrap().unwrap();
        assert_eq!(read_back.schema_version, EXPAND_CACHE_SCHEMA_VERSION);
        assert_eq!(read_back.generated_symbols.len(), 1);
        let sym = &read_back.generated_symbols[0];
        assert_eq!(sym.simple_name, "generated_new");
        assert_eq!(sym.impl_type.as_deref(), Some("Foo"));
        assert_eq!(sym.kind, GeneratedSymbolKind::Method);
    }
}
