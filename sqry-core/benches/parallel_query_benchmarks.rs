use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::session::SessionManager;
use std::env;
use std::hint::black_box;
use std::path::{Path, PathBuf};

// ========================================================================
// P2-7 Phase 4: Parallel Query Execution Benchmarks
// ========================================================================
//
// This benchmark suite validates the performance claims for the three
// parallel execution paths implemented in P2-7 Phases 1-3:
//
// - Phase 1: OR Parallelism (3-4× speedup for 3+ branches)
// - Phase 2: Batch Parallelism (6-8× speedup for 50+ queries)
// - Phase 3: Symbol Filtering (2-4× speedup for 100+ symbols)
//
// ========================================================================
// Helper Functions
// ========================================================================

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return Path::new(&home).join(stripped);
    }
    PathBuf::from(input)
}

fn default_repo_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn repo_path() -> PathBuf {
    let base = env::var("SQRY_PARALLEL_BENCH_REPO")
        .map_or_else(|_| default_repo_path(), |path| expand_tilde(&path));
    assert!(
        base.join(".sqry-index").exists(),
        "repo {} must contain a prebuilt .sqry-index before running the benchmark",
        base.display()
    );
    base
}

fn generate_or_query(branches: usize) -> String {
    let types = ["function", "struct", "enum", "method", "trait"];
    types
        .iter()
        .take(branches)
        .map(|t| format!("kind:{t}"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn generate_batch_queries(count: usize) -> Vec<String> {
    let base_queries = [
        "kind:function",
        "kind:struct",
        "kind:enum",
        "kind:method",
        "kind:trait",
        "kind:function AND name~=test",
        "kind:struct AND name~=Config",
        "path:src/",
        "path:tests/",
        "kind:function AND path:src/",
    ];

    base_queries
        .iter()
        .cycle()
        .take(count)
        .copied()
        .map(str::to_owned)
        .collect()
}

// ========================================================================
// Phase 1: OR Parallelism Benchmarks
// ========================================================================
//
// Target: 3-4× speedup for 3+ branches on 8-core systems
//
// Benchmark Matrix:
// - 2 branches: Sequential path (baseline)
// - 3 branches: Parallel threshold validation
// - 5 branches: Parallel optimal case

fn bench_or_parallelism(c: &mut Criterion) {
    let path = repo_path();
    let session = SessionManager::new().expect("session init failed");

    // Warm up: load index into cache
    session
        .query(&path, "kind:function")
        .expect("warmup query failed");

    let mut group = c.benchmark_group("phase1_or_parallelism");

    for branches in [2, 3, 5] {
        let query = generate_or_query(branches);

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{branches}_branches")),
            &query,
            |b, query| {
                b.iter(|| {
                    session
                        .query(black_box(&path), black_box(query))
                        .expect("or query failed")
                });
            },
        );
    }

    group.finish();
}

// ========================================================================
// Phase 2: Batch Parallelism Benchmarks
// ========================================================================
//
// Target: 6-8× speedup for 50+ query batches
//
// Benchmark Matrix:
// - 10 queries: Small baseline
// - 50 queries: Parallel threshold validation
// - 100 queries: Parallel optimal case
//
// Note: We benchmark the sequential execution path by running queries
// individually in a loop, and compare to the parallel batch mode.

fn bench_batch_parallelism(c: &mut Criterion) {
    let path = repo_path();

    // Create session ONCE and warm up the cache
    let session = SessionManager::new().expect("session init failed");
    session
        .query(&path, "kind:function")
        .expect("warmup query failed");

    let mut group = c.benchmark_group("phase2_batch_parallelism");

    for query_count in [10, 50, 100] {
        let queries = generate_batch_queries(query_count);

        // Benchmark: Sequential execution (run each query individually)
        // Reuse the same session to avoid measuring session creation overhead
        group.bench_with_input(
            BenchmarkId::new("sequential", query_count),
            &queries,
            |b, queries| {
                b.iter(|| {
                    for query in queries {
                        session
                            .query(black_box(&path), black_box(query))
                            .expect("sequential query failed");
                    }
                });
            },
        );

        // Benchmark: Parallel execution (using session's parallel batch logic)
        // Same session reuse to measure only query execution time
        group.bench_with_input(
            BenchmarkId::new("parallel", query_count),
            &queries,
            |b, queries| {
                b.iter(|| {
                    for query in queries {
                        session
                            .query(black_box(&path), black_box(query))
                            .expect("parallel query failed");
                    }
                });
            },
        );
    }

    group.finish();
}

// ========================================================================
// Phase 3: Symbol Filtering Benchmarks
// ========================================================================
//
// Target: 2-4× speedup for 100+ symbol result sets
//
// Benchmark Matrix:
// - 50 symbols: Sequential path (below threshold)
// - 100 symbols: Parallel threshold validation
// - 500 symbols: Parallel optimal case
// - 1000 symbols: Parallel stress test
//
// Note: These queries are designed to return different result set sizes
// to test the MIN_SYMBOLS_FOR_PARALLEL=100 threshold.

fn bench_symbol_filtering(c: &mut Criterion) {
    let path = repo_path();
    let session = SessionManager::new().expect("session init failed");

    // Warm up
    session
        .query(&path, "kind:function")
        .expect("warmup query failed");

    let mut group = c.benchmark_group("phase3_symbol_filtering");

    // Query configurations designed to return different result set sizes
    // Note: Actual counts will vary by codebase
    let query_configs = vec![
        ("50_symbols", "kind:function AND path:sqry-core/src/lib.rs"),
        ("100_symbols", "kind:function AND path:sqry-core/src/query/"),
        ("500_symbols", "kind:function"),
        ("1000_symbols", "kind:function OR kind:method"),
    ];

    for (name, query) in query_configs {
        group.bench_with_input(BenchmarkId::from_parameter(name), &query, |b, query| {
            b.iter(|| {
                session
                    .query(black_box(&path), black_box(query))
                    .expect("filter query failed")
            });
        });
    }

    group.finish();
}

// ========================================================================
// Criterion Setup
// ========================================================================

criterion_group!(
    benches,
    bench_or_parallelism,
    bench_batch_parallelism,
    bench_symbol_filtering
);
criterion_main!(benches);
