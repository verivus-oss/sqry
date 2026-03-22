//! Performance benchmarks for Java local variable scope resolution.
//!
//! Run with: `cargo test -p sqry-lang-java --test perf_local_scopes -- --ignored --nocapture`

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

fn load_fixture(path: &str) -> String {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("java")
        .join(path);

    std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!("Failed to load fixture {}: {e}", fixture_path.display());
    })
}

fn build_graph(content: &str, filename: &str) -> StagingGraph {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_java::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Java grammar");
    let tree = parser
        .parse(content, None)
        .expect("Failed to parse Java code");

    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();
    let file_path = Path::new(filename);

    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn run_benchmark(fixture_path: &str, filename: &str, warmup_count: usize, run_count: usize) {
    let content = load_fixture(fixture_path);
    let loc = content.lines().count();

    println!("--- Benchmark: {filename} ({loc} LOC) ---");

    // Warmup
    for _ in 0..warmup_count {
        let _ = build_graph(&content, filename);
    }

    // Timed runs
    let mut durations_us = Vec::with_capacity(run_count);
    for _ in 0..run_count {
        let start = Instant::now();
        let _ = build_graph(&content, filename);
        durations_us.push(start.elapsed().as_micros());
    }

    durations_us.sort_unstable();
    let median_us = durations_us[run_count / 2];
    let p95_idx = (run_count as f64 * 0.95).ceil() as usize - 1;
    let p95_us = durations_us[p95_idx.min(run_count - 1)];
    let min_us = durations_us[0];
    let max_us = durations_us[run_count - 1];

    println!("  fixture: {fixture_path}");
    println!("  loc: {loc}");
    println!("  warmup: {warmup_count}, runs: {run_count}");
    println!(
        "  median: {:.3} ms, p95: {:.3} ms",
        median_us as f64 / 1000.0,
        p95_us as f64 / 1000.0
    );
    println!(
        "  min: {:.3} ms, max: {:.3} ms",
        min_us as f64 / 1000.0,
        max_us as f64 / 1000.0
    );
    println!();
}

#[test]
#[ignore = "perf benchmark, not for CI"]
fn perf_small_100() {
    run_benchmark("perf/PerfSmall100.java", "PerfSmall100.java", 3, 10);
}

#[test]
#[ignore = "perf benchmark, not for CI"]
fn perf_medium_1k() {
    run_benchmark("perf/PerfMedium1k.java", "PerfMedium1k.java", 3, 10);
}

#[test]
#[ignore = "perf benchmark, not for CI"]
fn perf_large_10k() {
    run_benchmark("perf/PerfLarge10k.java", "PerfLarge10k.java", 3, 10);
}
