//! Performance benchmarks for plugin loading strategies.
//!
//! Measures the runtime impact of different plugin loading approaches:
//! - Loading all 13 built-in plugins upfront
//! - Loading plugins on-demand (baseline)
//! - Selective plugin loading
//!
//! This validates the Stream 1 goal: production-like test loading with built-in plugins.

mod plugin_factory_helpers;

use plugin_factory_helpers::{try_with_builtin_plugins, with_builtin_plugins};
use sqry_core::plugin::PluginManager;
use std::time::Instant;

fn ms_parts(ns: u128) -> (u128, u128) {
    let ms = ns / 1_000_000;
    let micros = (ns % 1_000_000) / 1_000;
    (ms, micros)
}

fn ratio_parts(ratio_scaled: u128) -> (u128, u128) {
    let ratio_int = ratio_scaled / 100;
    let ratio_frac = ratio_scaled % 100;
    (ratio_int, ratio_frac)
}

/// Measures time to create `PluginManager` with all built-in plugins
#[test]
fn bench_with_builtin_plugins() {
    let iterations = 10;
    let mut times_ns: Vec<u128> = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let _manager = with_builtin_plugins();
        let elapsed = start.elapsed();
        times_ns.push(elapsed.as_nanos());
    }

    let avg_ns = times_ns.iter().sum::<u128>() / iterations as u128;
    let min_ns = *times_ns.iter().min().unwrap();
    let max_ns = *times_ns.iter().max().unwrap();

    println!("\n=== Plugin Loading Performance ===");
    println!("with_builtin_plugins() (13 plugins):");
    let (avg_millis, avg_micros) = ms_parts(avg_ns);
    let (min_millis, min_micros) = ms_parts(min_ns);
    let (max_millis, max_micros) = ms_parts(max_ns);
    println!("  Average: {avg_millis}.{avg_micros:03} ms");
    println!("  Min:     {min_millis}.{min_micros:03} ms");
    println!("  Max:     {max_millis}.{max_micros:03} ms");
    println!("  Plugins: 13 (7 Tier 1 + 6 Tier 2)");

    // Sanity check: plugin loading should be fast (< 10ms)
    assert!(
        avg_ns < 10_000_000,
        "Plugin loading too slow: {avg_millis}.{avg_micros:03} ms (expected < 10ms)"
    );
}

/// Measures time to create empty `PluginManager` (baseline)
#[test]
fn bench_empty_manager() {
    let iterations = 10;
    let mut times_ns: Vec<u128> = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let _manager = PluginManager::new();
        let elapsed = start.elapsed();
        times_ns.push(elapsed.as_nanos());
    }

    let avg_ns = times_ns.iter().sum::<u128>() / iterations as u128;

    println!("\n=== Baseline Performance ===");
    println!("PluginManager::new() (0 plugins):");
    let (avg_millis, avg_micros) = ms_parts(avg_ns);
    println!("  Average: {avg_millis}.{avg_micros:03} ms");

    // Baseline should be very fast (< 5ms; relaxed for CI runner variability,
    // especially macOS VMs where system overhead can spike)
    assert!(
        avg_ns < 5_000_000,
        "Empty manager creation too slow: {avg_millis}.{avg_micros:03} ms (expected < 5ms)"
    );
}

/// Measures time to manually register a single plugin (on-demand loading)
#[test]
fn bench_on_demand_loading() {
    let iterations = 10;
    let mut times_ns: Vec<u128> = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
        let elapsed = start.elapsed();
        times_ns.push(elapsed.as_nanos());
    }

    let avg_ns = times_ns.iter().sum::<u128>() / iterations as u128;

    println!("\n=== On-Demand Loading ===");
    println!("Manual register_builtin() (1 plugin):");
    let (avg_millis, avg_micros) = ms_parts(avg_ns);
    println!("  Average: {avg_millis}.{avg_micros:03} ms");

    // On-demand loading should be very fast (< 5ms; relaxed for CI runner variability)
    assert!(
        avg_ns < 5_000_000,
        "On-demand loading too slow: {avg_millis}.{avg_micros:03} ms"
    );
}

/// Validates that `try_with_builtin_plugins()` returns Ok
#[test]
fn test_fallible_factory_succeeds() {
    let result = try_with_builtin_plugins();
    assert!(result.is_ok(), "try_with_builtin_plugins() should succeed");

    let manager = result.unwrap();
    assert_eq!(
        manager.plugins().len(),
        13,
        "Should load exactly 13 plugins"
    );
}

/// Validates plugin coverage across all expected extensions
#[test]
fn test_plugin_coverage() {
    let manager = with_builtin_plugins();

    // Tier 1: Core languages
    let tier1_extensions = ["rs", "js", "py", "ts", "go", "java", "swift"];
    for ext in &tier1_extensions {
        assert!(
            manager.plugin_for_extension(ext).is_some(),
            "Missing Tier 1 plugin for extension: {ext}"
        );
    }

    // Tier 2: Additional languages
    let tier2_extensions = ["kt", "lua", "php", "r", "rb", "scala"];
    for ext in &tier2_extensions {
        assert!(
            manager.plugin_for_extension(ext).is_some(),
            "Missing Tier 2 plugin for extension: {ext}"
        );
    }

    // Verify total count
    assert_eq!(
        manager.plugins().len(),
        13,
        "Expected 13 total plugins (7 Tier 1 + 6 Tier 2)"
    );
}

/// Compares upfront loading vs on-demand loading overhead
#[test]
fn bench_loading_strategy_comparison() {
    let iterations = 5;

    // Measure upfront loading (13 plugins)
    let mut upfront_times: Vec<u128> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let _manager = with_builtin_plugins();
        upfront_times.push(start.elapsed().as_nanos());
    }
    let upfront_avg = upfront_times.iter().sum::<u128>() / iterations as u128;

    // Measure on-demand loading (1 plugin)
    let mut on_demand_times: Vec<u128> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
        on_demand_times.push(start.elapsed().as_nanos());
    }
    let on_demand_avg = on_demand_times.iter().sum::<u128>() / iterations as u128;

    let on_demand_floor_ns = 50_000; // 0.05ms floor to avoid inflated ratios on ultra-fast baseline
    let effective_on_demand = on_demand_avg.max(on_demand_floor_ns);
    let overhead_ratio_scaled = upfront_avg.saturating_mul(100) / effective_on_demand;
    let (ratio_int, ratio_frac) = ratio_parts(overhead_ratio_scaled);

    println!("\n=== Loading Strategy Comparison ===");
    let (upfront_millis, upfront_micros) = ms_parts(upfront_avg);
    let (on_demand_millis, on_demand_micros) = ms_parts(on_demand_avg);
    println!("Upfront (13 plugins):  {upfront_millis}.{upfront_micros:03} ms");
    println!("On-demand (1 plugin):  {on_demand_millis}.{on_demand_micros:03} ms");
    println!("Overhead ratio:        {ratio_int}.{ratio_frac:02}x");

    // Document findings
    println!("\n=== Analysis ===");
    if overhead_ratio_scaled < 500 {
        println!("✓ Upfront loading overhead is acceptable (<5x on-demand)");
    } else if overhead_ratio_scaled < 1000 {
        println!("⚠ Upfront loading has moderate overhead (5-10x on-demand)");
    } else {
        println!("⚠ Upfront loading has high overhead (>10x on-demand)");
    }

    // Performance target: upfront loading should be < 300x slower than on-demand
    // (Relaxed from 150x for CI runner variability, especially macOS VMs)
    // Absolute time is more important: upfront loading should be <50ms total
    assert!(
        overhead_ratio_scaled < 30_000,
        "Upfront loading overhead too high: {ratio_int}.{ratio_frac:02}x (expected <300x; using floor {on_demand_floor_ns}ns)"
    );
    assert!(
        upfront_avg < 50_000_000,
        "Upfront loading too slow: {upfront_millis}.{upfront_micros:03} ms (expected <50ms)"
    );
}

/// Validates deterministic plugin ordering
#[test]
fn test_deterministic_ordering() {
    let manager1 = with_builtin_plugins();
    let manager2 = with_builtin_plugins();

    let ids1: Vec<String> = manager1
        .plugins()
        .iter()
        .map(|p| p.metadata().id.to_string())
        .collect();

    let ids2: Vec<String> = manager2
        .plugins()
        .iter()
        .map(|p| p.metadata().id.to_string())
        .collect();

    assert_eq!(
        ids1, ids2,
        "Plugin ordering should be deterministic across invocations"
    );

    println!("\n=== Plugin Roster (Deterministic Order) ===");
    for (i, id) in ids1.iter().enumerate() {
        println!("  {}. {}", i + 1, id);
    }
}
