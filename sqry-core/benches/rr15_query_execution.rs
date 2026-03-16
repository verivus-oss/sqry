//! Query Execution Benchmarks on Real Repositories
//!
//! This benchmark measures query execution performance using the unified CodeGraph
//! on real repositories. It tests various query patterns including simple predicates,
//! OR expressions, AND combinations, and regex patterns.
//!
//! **Prerequisites**: Repositories must have pre-built graphs via `sqry index`.
//! If graphs don't exist, benchmarks will be skipped with a warning.

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use sqry_core::plugin::PluginManager;
use sqry_core::query::QueryExecutor;
use std::hint::black_box;
use std::path::Path;

/// Real repository paths from ./benchmark-repos
fn get_benchmark_repos() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ripgrep", "./benchmark-repos"),
        ("fd", "./benchmark-repos"),
    ]
}

/// Query scenarios covering various query patterns
///
/// NOTE: Word patterns for `name:` must start with an alphanumeric character or underscore
/// due to lexer design. If you need a leading wildcard, use a regex literal with `~=`
/// (e.g., `name~=/.*test.*/`).
fn get_query_scenarios() -> Vec<(&'static str, &'static str)> {
    vec![
        // Simple predicate
        ("simple_kind", "kind:function"),
        // Simple 2-way OR (baseline)
        ("simple_or", "kind:function OR kind:struct"),
        // Complex 5-way OR
        (
            "complex_or",
            "kind:function OR kind:struct OR kind:class OR kind:interface OR kind:enum",
        ),
        // Large 3-way OR
        ("triple_or", "kind:function OR kind:method OR kind:class"),
        // Nested OR with AND and regex patterns (prefix matching)
        (
            "nested_or_and",
            "(kind:function OR kind:method) AND (name~=/^test_/ OR name~=/^spec_/)",
        ),
        // Large OR with regex patterns (matches common function prefixes)
        (
            "large_or_wildcards",
            "name~=/^get/ OR name~=/^set/ OR name~=/^is/ OR name~=/^has/ OR name~=/^create/ OR name~=/^delete/",
        ),
    ]
}

/// Check if a repository has a pre-built graph
fn has_graph(path: &Path) -> bool {
    path.join(".sqry/graph/snapshot.sqry").exists()
}

fn bench_query_execution(c: &mut Criterion) {
    let repos = get_benchmark_repos();
    let queries = get_query_scenarios();

    // Check which repos are available
    let available_repos: Vec<_> = repos
        .iter()
        .filter(|(name, path)| {
            let path = Path::new(path);
            if !path.exists() {
                eprintln!("⚠️  Skipping {name}: repository not found at {path:?}");
                return false;
            }
            if !has_graph(path) {
                eprintln!("⚠️  Skipping {name}: no graph found. Run `sqry index {path:?}` first.");
                return false;
            }
            true
        })
        .collect();

    if available_repos.is_empty() {
        eprintln!("⚠️  No repositories available for benchmarking.");
        eprintln!("    To enable benchmarks, ensure repositories exist and have graphs built.");
        return;
    }

    let mut group = c.benchmark_group("query_execution");
    group.sample_size(50); // 50 samples for stable results
    group.sampling_mode(SamplingMode::Flat);

    for (repo_name, repo_path) in available_repos {
        let plugin_manager = PluginManager::new();
        let executor = QueryExecutor::with_plugin_manager(plugin_manager);
        let path = Path::new(repo_path);

        for (query_name, query_str) in &queries {
            let bench_id = BenchmarkId::new(*repo_name, *query_name);

            group.bench_with_input(bench_id, query_str, |b, &query| {
                b.iter(|| {
                    let results = executor
                        .execute_on_graph(black_box(query), black_box(path))
                        .unwrap_or_else(|e| panic!("Query failed on {repo_path}: {e}"));
                    black_box(results)
                });
            });
        }
    }

    group.finish();
}

criterion_group!(benches, bench_query_execution);
criterion_main!(benches);
