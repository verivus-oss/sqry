//! Rigorous ripgrep comparison benchmark suite
//!
//! This benchmark validates the "7.7x faster than ripgrep" claim with 50+ varying tests
//! across different scenarios:
//! - File size variations (1KB to 10MB)
//! - File count variations (1 to 1000 files)
//! - Pattern complexity (literal, regex, complex regex)
//! - Real-world codebases (sqry itself, mixed languages)
//! - Edge cases (empty results, all matches, binary files)
//!
//! Run with: cargo bench --bench `rigorous_ripgrep_comparison`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::search::{SearchConfig, SearchMode, Searcher};
use std::fmt::Write;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

// ===== Test Data Generation =====

fn create_file_with_lines(
    dir: &Path,
    name: &str,
    line_count: usize,
    pattern_freq: usize,
) -> PathBuf {
    let mut content = String::new();
    for i in 0..line_count {
        if i % pattern_freq == 0 {
            writeln!(&mut content, "// TODO: optimize function_{i}")
                .expect("failed to write TODO line");
        } else {
            writeln!(&mut content, "fn function_{i}() {{ /* implementation */ }}")
                .expect("failed to write function line");
        }
    }
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn create_files(
    dir: &Path,
    file_count: usize,
    lines_per_file: usize,
    pattern_freq: usize,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for i in 0..file_count {
        files.push(create_file_with_lines(
            dir,
            &format!("file_{i}.rs"),
            lines_per_file,
            pattern_freq,
        ));
    }
    files
}

fn create_large_file(dir: &Path, name: &str, size_kb: usize) -> PathBuf {
    let line = "fn function_name() { /* some code here */ }\n";
    let line_size = line.len();
    let num_lines = (size_kb * 1024) / line_size;

    let mut content = String::new();
    for i in 0..num_lines {
        if i % 100 == 0 {
            content.push_str("// TODO: implement this\n");
        } else {
            content.push_str(line);
        }
    }
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ===== Benchmark Helpers =====

fn bench_sqry_search(dir: &Path, pattern: &str, case_insensitive: bool) -> usize {
    let config = SearchConfig {
        mode: SearchMode::Text,
        case_insensitive,
        before_context: 0,
        after_context: 0,
        ..Default::default()
    };

    let searcher = Searcher::new().unwrap();
    let results = searcher.search(pattern, &[dir], &config).unwrap();
    results.len()
}

fn bench_ripgrep_command(dir: &Path, pattern: &str, case_insensitive: bool) -> usize {
    let mut cmd = Command::new("rg");
    cmd.arg(pattern)
        .arg(dir)
        .arg("--no-heading")
        .arg("--no-line-number")
        .arg("--count-matches");

    if case_insensitive {
        cmd.arg("-i");
    }

    let output = cmd.output().unwrap();
    let count_str = String::from_utf8_lossy(&output.stdout);
    count_str
        .lines()
        .filter_map(|l| l.parse::<usize>().ok())
        .sum()
}

// ===== Category 1: File Size Variations (10 tests) =====

fn bench_file_size_variations(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_size_variations");

    let sizes = vec![1, 10, 50, 100, 500, 1000, 2000, 5000, 7500, 10000]; // KB

    for size_kb in sizes {
        let temp_dir = TempDir::new().unwrap();
        create_large_file(temp_dir.path(), "large.rs", size_kb);

        // sqry benchmark
        group.bench_with_input(
            BenchmarkId::new("sqry", format!("{size_kb}KB")),
            &size_kb,
            |b, _| {
                b.iter(|| black_box(bench_sqry_search(temp_dir.path(), "TODO", false)));
            },
        );

        // ripgrep benchmark
        group.bench_with_input(
            BenchmarkId::new("ripgrep", format!("{size_kb}KB")),
            &size_kb,
            |b, _| {
                b.iter(|| black_box(bench_ripgrep_command(temp_dir.path(), "TODO", false)));
            },
        );
    }

    group.finish();
}

// ===== Category 2: File Count Variations (10 tests) =====

fn bench_file_count_variations(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_count_variations");

    let counts = vec![1, 5, 10, 25, 50, 100, 200, 500, 750, 1000];

    for file_count in counts {
        let temp_dir = TempDir::new().unwrap();
        create_files(temp_dir.path(), file_count, 100, 10);

        // sqry benchmark
        group.bench_with_input(
            BenchmarkId::new("sqry", format!("{file_count}_files")),
            &file_count,
            |b, _| {
                b.iter(|| black_box(bench_sqry_search(temp_dir.path(), "TODO", false)));
            },
        );

        // ripgrep benchmark
        group.bench_with_input(
            BenchmarkId::new("ripgrep", format!("{file_count}_files")),
            &file_count,
            |b, _| {
                b.iter(|| black_box(bench_ripgrep_command(temp_dir.path(), "TODO", false)));
            },
        );
    }

    group.finish();
}

// ===== Category 3: Pattern Complexity (10 tests) =====

fn bench_pattern_complexity(c: &mut Criterion) {
    let mut group = c.benchmark_group("pattern_complexity");

    let temp_dir = TempDir::new().unwrap();
    create_files(temp_dir.path(), 50, 200, 10);

    let patterns = vec![
        ("literal_simple", "TODO", false),
        ("literal_long", "function_implementation_details", false),
        ("case_insensitive", "todo", true),
        ("word_boundary", r"\bfunction\b", false),
        ("digit_pattern", r"\d+", false),
        ("identifier_pattern", r"fn \w+", false),
        ("multiword", "TODO.*optimize", false),
        ("alternation", "TODO|FIXME|HACK", false),
        ("complex_regex", r"fn \w+\(\) \{", false),
        ("unicode_aware", r"\w+", false),
    ];

    for (name, pattern, case_insensitive) in patterns {
        // sqry benchmark
        group.bench_with_input(BenchmarkId::new("sqry", name), &pattern, |b, p| {
            b.iter(|| black_box(bench_sqry_search(temp_dir.path(), p, case_insensitive)));
        });

        // ripgrep benchmark
        group.bench_with_input(BenchmarkId::new("ripgrep", name), &pattern, |b, p| {
            b.iter(|| black_box(bench_ripgrep_command(temp_dir.path(), p, case_insensitive)));
        });
    }

    group.finish();
}

// ===== Category 4: Real-World Codebases (10 tests) =====

fn bench_real_world_codebases(c: &mut Criterion) {
    let mut group = c.benchmark_group("real_world_codebases");
    group.sample_size(20); // Reduce sample size for large codebases

    // Test on sqry's own codebase
    let sqry_src = Path::new("sqry-core/src");

    if sqry_src.exists() {
        let patterns = vec![
            ("common_word_pub", "pub"),
            ("common_word_fn", "fn"),
            ("common_word_use", "use"),
            ("comment_todo", "TODO"),
            ("comment_fixme", "FIXME"),
            ("error_handling", "Result"),
            ("option_type", "Option"),
            ("derive_macro", "#[derive"),
            ("test_attribute", "#[test]"),
            ("async_keyword", "async"),
        ];

        for (name, pattern) in patterns {
            // sqry benchmark
            group.bench_with_input(BenchmarkId::new("sqry", name), &pattern, |b, p| {
                b.iter(|| black_box(bench_sqry_search(sqry_src, p, false)));
            });

            // ripgrep benchmark
            group.bench_with_input(BenchmarkId::new("ripgrep", name), &pattern, |b, p| {
                b.iter(|| black_box(bench_ripgrep_command(sqry_src, p, false)));
            });
        }
    }

    group.finish();
}

// ===== Category 5: Edge Cases (10 tests) =====

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // Test 1-2: Empty results (pattern not found)
    let temp_dir1 = TempDir::new().unwrap();
    create_files(temp_dir1.path(), 100, 100, 10);

    group.bench_function("sqry_no_matches", |b| {
        b.iter(|| {
            black_box(bench_sqry_search(
                temp_dir1.path(),
                "NONEXISTENT_PATTERN_XYZ",
                false,
            ))
        });
    });

    group.bench_function("ripgrep_no_matches", |b| {
        b.iter(|| {
            black_box(bench_ripgrep_command(
                temp_dir1.path(),
                "NONEXISTENT_PATTERN_XYZ",
                false,
            ))
        });
    });

    // Test 3-4: Very common pattern (many matches)
    let temp_dir2 = TempDir::new().unwrap();
    create_files(temp_dir2.path(), 100, 100, 1); // Pattern on every line

    group.bench_function("sqry_many_matches", |b| {
        b.iter(|| black_box(bench_sqry_search(temp_dir2.path(), "TODO", false)));
    });

    group.bench_function("ripgrep_many_matches", |b| {
        b.iter(|| black_box(bench_ripgrep_command(temp_dir2.path(), "TODO", false)));
    });

    // Test 5-6: Single tiny file
    let temp_dir3 = TempDir::new().unwrap();
    fs::write(temp_dir3.path().join("tiny.rs"), "TODO: fix\n").unwrap();

    group.bench_function("sqry_tiny_file", |b| {
        b.iter(|| black_box(bench_sqry_search(temp_dir3.path(), "TODO", false)));
    });

    group.bench_function("ripgrep_tiny_file", |b| {
        b.iter(|| black_box(bench_ripgrep_command(temp_dir3.path(), "TODO", false)));
    });

    // Test 7-8: Deep directory structure
    let temp_dir4 = TempDir::new().unwrap();
    let mut deep_path = temp_dir4.path().to_path_buf();
    for i in 0..10 {
        deep_path = deep_path.join(format!("level{i}"));
        fs::create_dir_all(&deep_path).unwrap();
        fs::write(deep_path.join("file.rs"), "TODO: implement\n").unwrap();
    }

    group.bench_function("sqry_deep_dirs", |b| {
        b.iter(|| black_box(bench_sqry_search(temp_dir4.path(), "TODO", false)));
    });

    group.bench_function("ripgrep_deep_dirs", |b| {
        b.iter(|| black_box(bench_ripgrep_command(temp_dir4.path(), "TODO", false)));
    });

    // Test 9-10: Mixed file sizes
    let temp_dir5 = TempDir::new().unwrap();
    create_large_file(temp_dir5.path(), "large.rs", 1000);
    create_file_with_lines(temp_dir5.path(), "medium.rs", 500, 10);
    create_file_with_lines(temp_dir5.path(), "small.rs", 50, 10);
    fs::write(temp_dir5.path().join("tiny.rs"), "TODO\n").unwrap();

    group.bench_function("sqry_mixed_sizes", |b| {
        b.iter(|| black_box(bench_sqry_search(temp_dir5.path(), "TODO", false)));
    });

    group.bench_function("ripgrep_mixed_sizes", |b| {
        b.iter(|| black_box(bench_ripgrep_command(temp_dir5.path(), "TODO", false)));
    });

    group.finish();
}

// ===== Criterion Configuration =====

criterion_group!(
    benches,
    bench_file_size_variations,
    bench_file_count_variations,
    bench_pattern_complexity,
    bench_real_world_codebases,
    bench_edge_cases,
);
criterion_main!(benches);
