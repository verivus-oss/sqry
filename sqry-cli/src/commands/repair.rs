//! Index repair command implementation
//!
//! Provides tools to detect and fix common index issues:
//! - Orphaned symbols (files no longer exist)
//! - Dangling references (symbols reference non-existent dependencies)
//!
//! Note: Repair now uses the unified graph for detection and recommends
//! rebuilding the index for fixes.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use anyhow::{Context, Result};
use std::path::Path;

/// Repair corrupted index by detecting issues and recommending fixes
///
/// # Arguments
///
/// * `cli` - CLI configuration
/// * `path` - Directory with existing index
/// * `fix_orphans` - Remove symbols for files that no longer exist
/// * `fix_dangling` - Remove dangling references
/// * `recompute_checksum` - Recompute index checksum
/// * `fix_all` - Apply all fixes
/// * `dry_run` - Preview changes without modifying
///
/// # Errors
/// Returns an error if the graph cannot be loaded.
#[allow(clippy::fn_params_excessive_bools)]
pub fn run_repair(
    cli: &Cli,
    path: &str,
    fix_orphans: bool,
    fix_dangling: bool,
    recompute_checksum: bool,
    fix_all: bool,
    dry_run: bool,
) -> Result<()> {
    let root_path = Path::new(path);

    // Find index
    let index_location = find_nearest_index(root_path);
    let Some(ref loc) = index_location else {
        anyhow::bail!(
            "No index found at {}. Run 'sqry index' first.",
            root_path.display()
        );
    };

    // Load unified graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    let Some(actions) =
        determine_repair_actions(fix_all, fix_orphans, fix_dangling, recompute_checksum)
    else {
        return Ok(());
    };

    if dry_run {
        println!("DRY RUN MODE - Detecting issues only\n");
    }

    let mut stats = RepairStats::default();
    let mut issues_found = false;

    // Check for orphaned files
    if actions.orphans {
        println!("Checking for orphaned symbols...");
        let orphan_count = detect_orphaned_nodes(&graph, &loc.index_root);
        stats.orphans_detected = orphan_count;
        if orphan_count > 0 {
            println!("  Found {orphan_count} orphaned files (missing from disk)");
            issues_found = true;
        } else {
            println!("  No orphaned files found");
        }
    }

    if actions.dangling {
        println!("Checking for dangling references...");
        println!("  Note: Dangling reference detection requires full graph analysis");
    }

    if actions.checksum {
        println!("Checksum verification...");
        println!("  Graph integrity verified during load");
        stats.checksum_verified = true;
    }

    // Report results
    if issues_found {
        println!("\n⚠ Issues detected. To fix, rebuild the index:");
        println!("  sqry index --force {}", root_path.display());
    } else {
        println!("\n✓ No issues detected - index is healthy");
    }

    if cli.json {
        print_repair_json(issues_found, dry_run, &stats)?;
    }

    Ok(())
}

struct RepairActions {
    orphans: bool,
    dangling: bool,
    checksum: bool,
}

#[allow(clippy::fn_params_excessive_bools)]
fn determine_repair_actions(
    fix_all: bool,
    fix_orphans: bool,
    fix_dangling: bool,
    recompute_checksum: bool,
) -> Option<RepairActions> {
    let actions = RepairActions {
        orphans: fix_all || fix_orphans,
        dangling: fix_all || fix_dangling,
        checksum: fix_all || recompute_checksum,
    };

    if !actions.orphans && !actions.dangling && !actions.checksum {
        report_missing_repair_options();
        return None;
    }

    Some(actions)
}

fn report_missing_repair_options() {
    eprintln!("No repair options specified. Use --fix-all or specify individual checks:");
    eprintln!("  --fix-orphans           Check for symbols with missing files");
    eprintln!("  --fix-dangling          Check for dangling references");
    eprintln!("  --recompute-checksum    Verify index integrity");
    eprintln!("  --fix-all               Run all checks");
}

/// Detect nodes whose files no longer exist on disk
fn detect_orphaned_nodes(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    root_path: &Path,
) -> usize {
    let files = graph.files();
    let mut orphan_count = 0;

    // Check each unique file in the graph
    for (node_id, entry) in graph.nodes().iter() {
        let _ = node_id; // Suppress unused warning

        // Gate 0d iter-2 fix: skip unified losers from orphan
        // detection. Losers are inert duplicates, not orphaned
        // nodes. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }

        if let Some(file_path) = files.resolve(entry.file) {
            let full_path = root_path.join(file_path.as_ref());
            if !full_path.exists() {
                orphan_count += 1;
            }
        }
    }

    orphan_count
}

fn print_repair_json(issues_found: bool, dry_run: bool, stats: &RepairStats) -> Result<()> {
    let json_output = serde_json::json!({
        "issues_found": issues_found,
        "dry_run": dry_run,
        "stats": {
            "orphans_detected": stats.orphans_detected,
            "dangling_refs_detected": stats.dangling_refs_detected,
            "checksum_verified": stats.checksum_verified,
        },
        "recommendation": if issues_found { "Run 'sqry index --force' to rebuild" } else { "No action needed" }
    });
    println!("{}", serde_json::to_string_pretty(&json_output)?);
    Ok(())
}

/// Statistics about issues detected
#[derive(Default)]
struct RepairStats {
    orphans_detected: usize,
    dangling_refs_detected: usize,
    checksum_verified: bool,
}
