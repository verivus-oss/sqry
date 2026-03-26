use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::session::SessionManager;
use std::env;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

const DEFAULT_REPO: &str = "/tmp/example_repo";
const DEFAULT_COLD_QUERY: &str = "kind:function";
const DEFAULT_WARM_QUERIES: [&str; 3] =
    ["kind:class", "kind:method", "kind:function AND lang:rust"];

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return Path::new(&home).join(stripped);
    }
    PathBuf::from(input)
}

/// Returns the repository path if it has a prebuilt index, or None if not available.
/// When None, benchmarks will be skipped with a warning rather than panicking.
fn repo_path() -> Option<PathBuf> {
    let path = env::var("SQRY_SESSION_BENCH_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let base = expand_tilde(&path);

    if base.join(".sqry-index").exists() {
        Some(base)
    } else {
        eprintln!(
            "⚠️  Skipping session_performance benchmarks: no prebuilt .sqry-index found at {}",
            base.display()
        );
        eprintln!(
            "   Set SQRY_SESSION_BENCH_REPO to a repository with a prebuilt index to run these benchmarks."
        );
        None
    }
}

fn bench_session_cold_start(c: &mut Criterion) {
    let Some(path) = repo_path() else {
        return; // Skip benchmark if no indexed repo available
    };

    c.bench_function("session_cold_start", |b| {
        b.iter(|| {
            let session = SessionManager::new().expect("session init failed");
            session
                .query(black_box(&path), black_box(DEFAULT_COLD_QUERY))
                .expect("cold query failed");
        });
    });
}

fn bench_session_warm_queries(c: &mut Criterion) {
    let Some(path) = repo_path() else {
        return; // Skip benchmark if no indexed repo available
    };

    let session = SessionManager::new().expect("session init failed");
    session
        .query(&path, DEFAULT_COLD_QUERY)
        .expect("initial warm-up query failed");

    let mut group = c.benchmark_group("session_warm_queries");
    for query in DEFAULT_WARM_QUERIES {
        group.bench_function(query, |b| {
            b.iter(|| {
                session
                    .query(black_box(&path), black_box(query))
                    .expect("warm query failed");
            });
        });
    }
    group.finish();
}

fn bench_session_amortized_run(c: &mut Criterion) {
    let Some(path) = repo_path() else {
        return; // Skip benchmark if no indexed repo available
    };

    c.bench_function("session_amortized_10_queries", |b| {
        b.iter(|| {
            let session = SessionManager::new().expect("session init failed");
            session
                .query(&path, DEFAULT_COLD_QUERY)
                .expect("amortized cold query failed");
            for idx in 0..9 {
                let query = format!("kind:function AND name:test{idx}");
                session
                    .query(&path, &query)
                    .expect("amortized warm query failed");
            }
        });
    });
}

fn bench_session_concurrent_queries(c: &mut Criterion) {
    let Some(path) = repo_path() else {
        return; // Skip benchmark if no indexed repo available
    };

    let path = Arc::new(path);
    let session = Arc::new(SessionManager::new().expect("session init failed"));
    session
        .query(&path, DEFAULT_COLD_QUERY)
        .expect("concurrency warm-up failed");

    c.bench_function("session_concurrent_8_queries", |b| {
        b.iter(|| {
            let handles: Vec<_> = (0..8)
                .map(|idx| {
                    let session = Arc::clone(&session);
                    let path = Arc::clone(&path);
                    thread::spawn(move || {
                        let query = format!("kind:class AND name:test{idx}");
                        session
                            .query(&path, &query)
                            .expect("concurrent query failed");
                    })
                })
                .collect();

            for handle in handles {
                handle.join().expect("worker thread panicked");
            }
        });
    });
}

criterion_group!(
    benches,
    bench_session_cold_start,
    bench_session_warm_queries,
    bench_session_amortized_run,
    bench_session_concurrent_queries
);
criterion_main!(benches);
