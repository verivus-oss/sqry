//! Hybrid search performance benchmarks
//!
//! Measures performance of hybrid search engine components:
//! - Text search performance
//! - Query classification overhead
//! - Semantic vs text mode selection
//! - Large file handling
//! - Multi-file searching
//!
//! Run with: cargo bench --bench `hybrid_search_benchmark`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::search::classifier::QueryClassifier;
use sqry_core::search::fallback::{FallbackConfig, FallbackSearchEngine};
use sqry_core::search::{SearchConfig, SearchMode, Searcher};
use std::fmt::Write;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use tempfile::TempDir;

// ===== Test Data Generation =====

fn create_test_file_with_content(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.join(name);
    fs::write(&file_path, content).unwrap();
    file_path
}

fn create_large_test_file(dir: &Path, lines: usize) -> std::path::PathBuf {
    let mut content = String::new();
    for i in 0..lines {
        let _ = writeln!(content, "fn function_{i}() {{ /* code */ }} // Line {i}");
        if i % 100 == 50 {
            content.push_str("// TODO: optimize this section\n");
        }
    }
    create_test_file_with_content(dir, "large_file.rs", &content)
}

fn create_multi_file_project(dir: &Path, file_count: usize) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for i in 0..file_count {
        let content = format!(
            r#"
pub fn function_{i}() {{
    // TODO: implement
    let x = {i};
    println!("Processing {{}}",x);
}}

pub struct Data{i} {{
    field: i32,
}}
"#
        );
        files.push(create_test_file_with_content(
            dir,
            &format!("file_{i}.rs"),
            &content,
        ));
    }
    files
}

// ===== Query Classification Benchmarks =====

fn bench_query_classification(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_classification");

    group.bench_function("semantic_query", |b| {
        let query = "kind:function AND name:foo AND visibility:public";
        b.iter(|| {
            black_box(QueryClassifier::classify(query));
        });
    });

    group.bench_function("text_query", |b| {
        let query = "TODO: fix this bug";
        b.iter(|| {
            black_box(QueryClassifier::classify(query));
        });
    });

    group.bench_function("hybrid_query", |b| {
        let query = "find_user";
        b.iter(|| {
            black_box(QueryClassifier::classify(query));
        });
    });

    group.finish();
}

// ===== Text Search Benchmarks =====

fn bench_text_search_by_file_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_search_file_size");

    for line_count in &[100, 1_000, 10_000] {
        let temp_dir = TempDir::new().unwrap();
        create_large_test_file(temp_dir.path(), *line_count);

        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: false,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("lines", line_count), line_count, |b, _| {
            b.iter(|| {
                let searcher = Searcher::new().unwrap();
                black_box(
                    searcher
                        .search("TODO", &[temp_dir.path()], &config)
                        .unwrap(),
                )
            });
        });
    }

    group.finish();
}

fn bench_text_search_by_file_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_search_file_count");

    for file_count in &[10, 50, 100] {
        let temp_dir = TempDir::new().unwrap();
        create_multi_file_project(temp_dir.path(), *file_count);

        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: false,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("files", file_count), file_count, |b, _| {
            b.iter(|| {
                let searcher = Searcher::new().unwrap();
                black_box(
                    searcher
                        .search("TODO", &[temp_dir.path()], &config)
                        .unwrap(),
                )
            });
        });
    }

    group.finish();
}

// ===== Hybrid Search Benchmarks =====

fn bench_hybrid_search_modes(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_search_modes");

    let temp_dir = TempDir::new().unwrap();
    create_multi_file_project(temp_dir.path(), 10);

    let config = FallbackConfig {
        show_search_mode: false, // Disable logging for benchmarks
        ..Default::default()
    };

    group.bench_function("semantic_only", |b| {
        b.iter(|| {
            let mut engine = FallbackSearchEngine::with_config(config.clone()).unwrap();
            black_box(engine.search("kind:function", temp_dir.path()).unwrap())
        });
    });

    group.bench_function("text_only", |b| {
        b.iter(|| {
            let mut engine = FallbackSearchEngine::with_config(config.clone()).unwrap();
            black_box(engine.search_text_only("TODO", temp_dir.path()).unwrap())
        });
    });

    group.bench_function("fallback_scenario", |b| {
        b.iter(|| {
            let mut engine = FallbackSearchEngine::with_config(config.clone()).unwrap();
            // Ambiguous query that triggers fallback
            black_box(
                engine
                    .search("nonexistent_pattern", temp_dir.path())
                    .unwrap(),
            )
        });
    });

    group.finish();
}

fn bench_hybrid_engine_overhead(c: &mut Criterion) {
    c.bench_function("hybrid_engine_creation", |b| {
        b.iter(|| {
            let config = FallbackConfig::default();
            black_box(FallbackSearchEngine::with_config(config).unwrap())
        });
    });
}

// ===== Search Configuration Benchmarks =====

fn bench_text_search_options(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_search_options");

    let temp_dir = TempDir::new().unwrap();
    create_large_test_file(temp_dir.path(), 1000);

    // Baseline: no context, case-sensitive
    group.bench_function("baseline", |b| {
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: false,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };
        b.iter(|| {
            let searcher = Searcher::new().unwrap();
            black_box(
                searcher
                    .search("TODO", &[temp_dir.path()], &config)
                    .unwrap(),
            )
        });
    });

    // With context lines
    group.bench_function("with_context_2_lines", |b| {
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: false,
            before_context: 2,
            after_context: 2,
            ..Default::default()
        };
        b.iter(|| {
            let searcher = Searcher::new().unwrap();
            black_box(
                searcher
                    .search("TODO", &[temp_dir.path()], &config)
                    .unwrap(),
            )
        });
    });

    // Case-insensitive
    group.bench_function("case_insensitive", |b| {
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: true,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };
        b.iter(|| {
            let searcher = Searcher::new().unwrap();
            black_box(
                searcher
                    .search("todo", &[temp_dir.path()], &config)
                    .unwrap(),
            )
        });
    });

    // Regex pattern
    group.bench_function("regex_pattern", |b| {
        let config = SearchConfig {
            mode: SearchMode::Regex,
            case_insensitive: false,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };
        b.iter(|| {
            let searcher = Searcher::new().unwrap();
            black_box(
                searcher
                    .search("TODO.*optimize", &[temp_dir.path()], &config)
                    .unwrap(),
            )
        });
    });

    group.finish();
}

// ===== Ripgrep Comparison Benchmarks =====

fn bench_sqry_vs_ripgrep(c: &mut Criterion) {
    use std::process::Command;

    let mut group = c.benchmark_group("sqry_vs_ripgrep");

    // Create test data
    let temp_dir = TempDir::new().unwrap();
    create_multi_file_project(temp_dir.path(), 100);

    // Benchmark sqry text search
    group.bench_function("sqry_text_search_100_files", |b| {
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: false,
            before_context: 0,
            after_context: 0,
            ..Default::default()
        };
        b.iter(|| {
            let searcher = Searcher::new().unwrap();
            black_box(
                searcher
                    .search("TODO", &[temp_dir.path()], &config)
                    .unwrap(),
            )
        });
    });

    // Benchmark raw ripgrep command
    group.bench_function("ripgrep_raw_100_files", |b| {
        b.iter(|| {
            black_box(
                Command::new("rg")
                    .arg("TODO")
                    .arg(temp_dir.path())
                    .arg("--no-heading")
                    .arg("--no-line-number")
                    .output()
                    .unwrap(),
            )
        });
    });

    group.finish();
}

// ===== Criterion Configuration =====

criterion_group!(
    benches,
    bench_query_classification,
    bench_text_search_by_file_size,
    bench_text_search_by_file_count,
    bench_hybrid_search_modes,
    bench_hybrid_engine_overhead,
    bench_text_search_options,
    bench_sqry_vs_ripgrep,
);
criterion_main!(benches);
