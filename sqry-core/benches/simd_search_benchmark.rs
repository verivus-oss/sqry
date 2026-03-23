//! SIMD text search benchmarks (P2-5)
//!
//! Measures AVX2 vs scalar performance for:
//! - Substring search (Boyer-Moore-Horspool)
//! - Trigram extraction
//! - ASCII lowercase conversion
//!
//! Target: 2-4x speedup with AVX2 on `x86_64`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use sqry_core::search::simd;
use std::hint::black_box;

// ============================================================================
// Benchmark Data Generators
// ============================================================================

/// Generate haystack for substring search benchmarks
fn generate_haystack(size: usize) -> Vec<u8> {
    // Realistic code-like text with mixed ASCII
    let pattern = b"fn search_symbol(name: &str, path: &Path) -> Option<Symbol> {\n    ";
    pattern.iter().cycle().take(size).copied().collect()
}

/// Generate needle patterns of various sizes
fn generate_needles() -> Vec<(String, Vec<u8>)> {
    vec![
        ("single_byte".to_string(), b"s".to_vec()),
        ("short_4b".to_string(), b"name".to_vec()),
        ("medium_12b".to_string(), b"search_symbol".to_vec()),
        (
            "long_32b".to_string(),
            b"fn search_symbol(name: &str, path: &Path)".to_vec(),
        ),
        (
            "very_long_64b".to_string(),
            b"fn search_symbol(name: &str, path: &Path) -> Option<Symbol> {".to_vec(),
        ),
    ]
}

/// Generate text for trigram extraction
fn generate_text_for_trigrams(size: usize) -> String {
    // Realistic identifier-like text
    let pattern = "createCompilerHost_parseSourceFile_getSymbolAtLocation_";
    pattern.chars().cycle().take(size).collect()
}

/// Generate text for lowercase benchmarks
fn generate_text_for_lowercase(size: usize) -> String {
    // Mixed case ASCII text (realistic code)
    let pattern = "CreateCompilerHost_ParseSourceFile_GetSymbolAtLocation_";
    pattern.chars().cycle().take(size).collect()
}

// ============================================================================
// Substring Search Benchmarks
// ============================================================================

fn bench_search_scalar_vs_avx2(c: &mut Criterion) {
    let mut group = c.benchmark_group("search/substring");

    let haystack_sizes = vec![100, 1_000, 10_000, 100_000];
    let needles = generate_needles();

    for size in haystack_sizes {
        let haystack = generate_haystack(size);
        group.throughput(Throughput::Bytes(size as u64));

        for (needle_name, needle) in &needles {
            let bench_name = format!("hay_{size}_needle_{needle_name}");

            // Benchmark scalar implementation
            group.bench_with_input(
                BenchmarkId::new("scalar", &bench_name),
                &(&haystack, needle),
                |b, (hay, ndl)| {
                    b.iter(|| simd::scalar::search(black_box(hay), black_box(ndl)));
                },
            );

            // Benchmark AVX2 implementation (dispatches automatically)
            group.bench_with_input(
                BenchmarkId::new("avx2_dispatch", &bench_name),
                &(&haystack, needle),
                |b, (hay, ndl)| {
                    b.iter(|| simd::search(black_box(hay), black_box(ndl)));
                },
            );
        }
    }

    group.finish();
}

/// Benchmark single-byte search (memchr-like)
fn bench_search_single_byte(c: &mut Criterion) {
    let mut group = c.benchmark_group("search/single_byte");

    let haystack_sizes = vec![100, 1_000, 10_000, 100_000];

    for size in haystack_sizes {
        let haystack = generate_haystack(size);
        let needle: &[u8] = b"x"; // Character unlikely to be found (worst case)

        group.throughput(Throughput::Bytes(size as u64));

        // Scalar
        group.bench_with_input(
            BenchmarkId::new("scalar", size),
            &(&haystack, needle),
            |b, (hay, ndl)| {
                b.iter(|| simd::scalar::search(black_box(hay), black_box(*ndl)));
            },
        );

        // AVX2 dispatch
        group.bench_with_input(
            BenchmarkId::new("avx2_dispatch", size),
            &(&haystack, needle),
            |b, (hay, ndl)| {
                b.iter(|| simd::search(black_box(hay), black_box(*ndl)));
            },
        );
    }

    group.finish();
}

// ============================================================================
// Trigram Extraction Benchmarks
// ============================================================================

fn bench_trigram_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("trigram/extraction");

    let text_sizes = vec![10, 50, 100, 500, 1_000];

    for size in text_sizes {
        let text = generate_text_for_trigrams(size);
        group.throughput(Throughput::Bytes(size as u64));

        // Scalar
        group.bench_with_input(BenchmarkId::new("scalar", size), &text, |b, txt| {
            b.iter(|| simd::scalar::extract_trigrams(black_box(txt)));
        });

        // AVX2 dispatch
        group.bench_with_input(BenchmarkId::new("avx2_dispatch", size), &text, |b, txt| {
            b.iter(|| simd::extract_trigrams(black_box(txt)));
        });
    }

    group.finish();
}

// ============================================================================
// ASCII Lowercase Benchmarks
// ============================================================================

fn bench_lowercase_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("lowercase/ascii");

    let text_sizes = vec![32, 64, 128, 256, 512, 1_024, 10_000];

    for size in text_sizes {
        let text = generate_text_for_lowercase(size);
        group.throughput(Throughput::Bytes(size as u64));

        // Scalar
        group.bench_with_input(BenchmarkId::new("scalar", size), &text, |b, txt| {
            b.iter(|| simd::scalar::to_lowercase_ascii(black_box(txt)));
        });

        // AVX2 dispatch
        group.bench_with_input(BenchmarkId::new("avx2_dispatch", size), &text, |b, txt| {
            b.iter(|| simd::to_lowercase_ascii(black_box(txt)));
        });
    }

    group.finish();
}

// ============================================================================
// Realistic E2E Scenarios
// ============================================================================

/// Benchmark realistic search scenario: finding function names in code
fn bench_realistic_function_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e/function_search");

    // Simulate searching a large source file (10KB)
    let source_code = generate_haystack(10_000);
    let function_name = b"search_symbol";

    group.throughput(Throughput::Bytes(10_000));

    // Scalar
    group.bench_function("scalar", |b| {
        b.iter(|| simd::scalar::search(black_box(&source_code), black_box(function_name)));
    });

    // AVX2 dispatch
    group.bench_function("avx2_dispatch", |b| {
        b.iter(|| simd::search(black_box(&source_code), black_box(function_name)));
    });

    group.finish();
}

/// Benchmark realistic trigram scenario: fuzzy search index building
fn bench_realistic_trigram_indexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e/trigram_indexing");

    // Simulate indexing 1000 symbol names (realistic codebase)
    let symbol_names: Vec<String> = (0..1000)
        .map(|i| format!("createCompilerHost_{i}_parseSourceFile"))
        .collect();

    let total_bytes: usize = symbol_names.iter().map(String::len).sum();
    group.throughput(Throughput::Bytes(total_bytes as u64));

    // Scalar
    group.bench_function("scalar", |b| {
        b.iter(|| {
            for name in &symbol_names {
                black_box(simd::scalar::extract_trigrams(black_box(name)));
            }
        });
    });

    // AVX2 dispatch
    group.bench_function("avx2_dispatch", |b| {
        b.iter(|| {
            for name in &symbol_names {
                black_box(simd::extract_trigrams(black_box(name)));
            }
        });
    });

    group.finish();
}

/// Benchmark realistic lowercase scenario: case-insensitive query processing
fn bench_realistic_case_normalization(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e/case_normalization");

    // Simulate normalizing 1000 queries (realistic search workload)
    let queries: Vec<String> = (0..1000)
        .map(|i| format!("SearchSymbolByName_Query_{i}_WithFilter"))
        .collect();

    let total_bytes: usize = queries.iter().map(String::len).sum();
    group.throughput(Throughput::Bytes(total_bytes as u64));

    // Scalar
    group.bench_function("scalar", |b| {
        b.iter(|| {
            for query in &queries {
                black_box(simd::scalar::to_lowercase_ascii(black_box(query)));
            }
        });
    });

    // AVX2 dispatch
    group.bench_function("avx2_dispatch", |b| {
        b.iter(|| {
            for query in &queries {
                black_box(simd::to_lowercase_ascii(black_box(query)));
            }
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

criterion_group!(
    simd_benchmarks,
    bench_search_scalar_vs_avx2,
    bench_search_single_byte,
    bench_trigram_extraction,
    bench_lowercase_conversion,
    bench_realistic_function_search,
    bench_realistic_trigram_indexing,
    bench_realistic_case_normalization,
);

criterion_main!(simd_benchmarks);
