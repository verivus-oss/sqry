//! Criterion benchmarks for the query language feature
//!
//! This benchmark suite measures performance of query parsing, validation, optimization,
//! and caching. Target performance:
//! - Query parsing: < 1ms for typical queries
//! - Validation: < 500μs for typical queries
//! - Optimization: < 200μs for typical queries
//! - Cache hit: < 100ns (Arc clone)
//! - Cache miss + store: < 2ms

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::query::cache::AstParseCache;
use sqry_core::query::lexer::tokenize_with_pool;
use sqry_core::query::optimizer::Optimizer;
use sqry_core::query::validator::{ValidationOptions, Validator};
use sqry_core::query::{FieldRegistry, ParsedQuery, QueryParser};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::OnceLock;

// ============================================================================
// Test Queries
// ============================================================================

/// Simple query: "kind:function"
fn simple_query() -> &'static str {
    "kind:function"
}

/// Complex query with multiple AND clauses
fn complex_query() -> &'static str {
    "kind:function AND name~=/^test_/ AND NOT path:*test*"
}

/// Query with deeply nested parentheses (5 levels)
fn nested_query() -> &'static str {
    "((((kind:function AND lang:rust) OR (kind:method AND lang:python)) AND name:main) OR path:*src*)"
}

/// Large query with many clauses
fn large_query() -> &'static str {
    static LARGE: OnceLock<String> = OnceLock::new();
    LARGE
        .get_or_init(|| {
            [
                "kind:function",
                "kind:method",
                "kind:class",
                "kind:interface",
                "kind:trait",
                "lang:rust",
                "lang:python",
                "lang:go",
                "path:*src*",
                "path:*test*",
            ]
            .iter()
            .map(|q| format!("({q})"))
            .collect::<Vec<_>>()
            .join(" OR ")
        })
        .as_str()
}

/// Query with regex pattern
fn regex_query() -> &'static str {
    "kind:function AND name~=/^test_[a-z]+_\\d+$/ AND lang:python"
}

/// Query with lookaround assertions (FT-C.1)
fn lookaround_query() -> &'static str {
    "name~=/(?=test_)\\w+/ AND kind:function"
}

// ============================================================================
// Parsing Benchmarks
// ============================================================================

fn bench_parse_simple(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "simple"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(simple_query())));
    });
    group.finish();
}

fn bench_parse_complex(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "complex"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(complex_query())));
    });
    group.finish();
}

fn bench_parse_nested(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "nested"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(nested_query())));
    });
    group.finish();
}

fn bench_parse_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "large"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(large_query())));
    });
    group.finish();
}

fn bench_parse_regex(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "regex"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(regex_query())));
    });
    group.finish();
}

fn bench_parse_lookaround(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");
    group.bench_function(BenchmarkId::new("parse", "lookaround"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(lookaround_query())));
    });
    group.finish();
}

// ============================================================================
// Lexing Benchmarks
// ============================================================================

fn bench_lex_simple(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_lex");
    group.bench_function(BenchmarkId::new("lex", "simple"), |b| {
        b.iter(|| tokenize_with_pool(black_box(simple_query())));
    });
    group.finish();
}

fn bench_lex_complex(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_lex");
    group.bench_function(BenchmarkId::new("lex", "complex"), |b| {
        b.iter(|| tokenize_with_pool(black_box(complex_query())));
    });
    group.finish();
}

// ============================================================================
// Validation Benchmarks
// ============================================================================

fn bench_validate_simple_valid(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);
    let query = QueryParser::parse_query(simple_query()).unwrap();

    let mut group = c.benchmark_group("query_validate");
    group.bench_function(BenchmarkId::new("validate", "simple_valid"), |b| {
        b.iter(|| validator.validate(&query.root));
    });
    group.finish();
}

fn bench_validate_complex_valid(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);
    let query = QueryParser::parse_query(complex_query()).unwrap();

    let mut group = c.benchmark_group("query_validate");
    group.bench_function(BenchmarkId::new("validate", "complex_valid"), |b| {
        b.iter(|| validator.validate(&query.root));
    });
    group.finish();
}

fn bench_validate_with_fuzzy(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let options = ValidationOptions {
        fuzzy_fields: true,
        fuzzy_field_distance: 2,
    };
    let validator = Validator::with_options(registry, options);
    // Query with slightly misspelled field to trigger Levenshtein distance calculation
    let query = QueryParser::parse_query("knd:function").unwrap();

    let mut group = c.benchmark_group("query_validate");
    group.bench_function(BenchmarkId::new("validate", "fuzzy_correction"), |b| {
        b.iter(|| validator.validate(&query.root));
    });
    group.finish();
}

fn bench_validate_regex_pattern(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);
    let query = QueryParser::parse_query(regex_query()).unwrap();

    let mut group = c.benchmark_group("query_validate");
    group.bench_function(BenchmarkId::new("validate", "regex_pattern"), |b| {
        b.iter(|| validator.validate(&query.root));
    });
    group.finish();
}

fn bench_validate_lookaround_pattern(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);
    let query = QueryParser::parse_query(lookaround_query()).unwrap();

    let mut group = c.benchmark_group("query_validate");
    group.bench_function(BenchmarkId::new("validate", "lookaround_pattern"), |b| {
        b.iter(|| validator.validate(&query.root));
    });
    group.finish();
}

// ============================================================================
// Optimization Benchmarks
// ============================================================================

fn bench_optimize_simple(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let optimizer = Optimizer::new(registry);
    let query = QueryParser::parse_query(simple_query()).unwrap();

    let mut group = c.benchmark_group("query_optimize");
    group.bench_function(BenchmarkId::new("optimize", "simple"), |b| {
        b.iter(|| optimizer.optimize_query(black_box(query.clone())));
    });
    group.finish();
}

fn bench_optimize_complex(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let optimizer = Optimizer::new(registry);
    let query = QueryParser::parse_query(complex_query()).unwrap();

    let mut group = c.benchmark_group("query_optimize");
    group.bench_function(BenchmarkId::new("optimize", "complex"), |b| {
        b.iter(|| optimizer.optimize_query(black_box(query.clone())));
    });
    group.finish();
}

fn bench_optimize_nested(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let optimizer = Optimizer::new(registry);
    let query = QueryParser::parse_query(nested_query()).unwrap();

    let mut group = c.benchmark_group("query_optimize");
    group.bench_function(BenchmarkId::new("optimize", "nested"), |b| {
        b.iter(|| optimizer.optimize_query(black_box(query.clone())));
    });
    group.finish();
}

fn bench_optimize_large(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let optimizer = Optimizer::new(registry);
    let query = QueryParser::parse_query(large_query()).unwrap();

    let mut group = c.benchmark_group("query_optimize");
    group.bench_function(BenchmarkId::new("optimize", "large"), |b| {
        b.iter(|| optimizer.optimize_query(black_box(query.clone())));
    });
    group.finish();
}

// ============================================================================
// Cache Benchmarks
// ============================================================================

fn bench_cache_hit(c: &mut Criterion) {
    let cache = AstParseCache::new(1000);
    let ast = QueryParser::parse_query(simple_query()).unwrap();
    let parsed = ParsedQuery::from_ast(Arc::new(ast)).unwrap();
    let query_str = simple_query().to_string();

    // Pre-populate cache
    cache.insert(query_str.clone(), parsed);

    let mut group = c.benchmark_group("query_cache");
    group.bench_function(BenchmarkId::new("cache", "hit"), |b| {
        b.iter(|| cache.get(black_box(&query_str)));
    });
    group.finish();
}

fn bench_cache_miss_and_store(c: &mut Criterion) {
    let cache = AstParseCache::new(1000);

    let mut group = c.benchmark_group("query_cache");
    group.bench_function(BenchmarkId::new("cache", "miss_and_store"), |b| {
        b.iter(|| {
            let query_str = black_box(simple_query().to_string());
            if let Some(cached) = cache.get(&query_str) {
                cached
            } else {
                let ast = QueryParser::parse_query(&query_str).unwrap();
                let parsed = ParsedQuery::from_ast(Arc::new(ast)).unwrap();
                cache.insert(query_str.clone(), parsed.clone());
                cache.get(&query_str).unwrap()
            }
        });
    });
    group.finish();
}

fn bench_cache_hit_complex(c: &mut Criterion) {
    let cache = AstParseCache::new(1000);
    let ast = QueryParser::parse_query(complex_query()).unwrap();
    let parsed = ParsedQuery::from_ast(Arc::new(ast)).unwrap();
    let query_str = complex_query().to_string();

    // Pre-populate cache
    cache.insert(query_str.clone(), parsed);

    let mut group = c.benchmark_group("query_cache_complex");
    group.bench_function(BenchmarkId::new("cache", "hit_complex"), |b| {
        b.iter(|| cache.get(black_box(&query_str)));
    });
    group.finish();
}

fn bench_cache_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_cache_cold");
    group.bench_function(BenchmarkId::new("cache", "cold_start"), |b| {
        b.iter(|| {
            let cache = AstParseCache::new(100);
            let query_str = simple_query().to_string();
            let ast = QueryParser::parse_query(&query_str).unwrap();
            let parsed = ParsedQuery::from_ast(Arc::new(ast)).unwrap();
            cache.insert(query_str.clone(), parsed);
            cache.get(&query_str)
        });
    });
    group.finish();
}

// ============================================================================
// Full Pipeline Benchmarks (parse -> validate -> optimize)
// ============================================================================

fn bench_full_pipeline_simple(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry.clone());
    let optimizer = Optimizer::new(registry);

    let mut group = c.benchmark_group("query_pipeline");
    group.bench_function(BenchmarkId::new("pipeline", "simple"), |b| {
        b.iter(|| {
            let query = QueryParser::parse_query(black_box(simple_query())).unwrap();
            validator.validate(&query.root).unwrap();
            let _optimized = optimizer.optimize_query(query);
        });
    });
    group.finish();
}

fn bench_full_pipeline_complex(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry.clone());
    let optimizer = Optimizer::new(registry);

    let mut group = c.benchmark_group("query_pipeline");
    group.bench_function(BenchmarkId::new("pipeline", "complex"), |b| {
        b.iter(|| {
            let query = QueryParser::parse_query(black_box(complex_query())).unwrap();
            validator.validate(&query.root).unwrap();
            let _optimized = optimizer.optimize_query(query);
        });
    });
    group.finish();
}

fn bench_full_pipeline_nested(c: &mut Criterion) {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry.clone());
    let optimizer = Optimizer::new(registry);

    let mut group = c.benchmark_group("query_pipeline");
    group.bench_function(BenchmarkId::new("pipeline", "nested"), |b| {
        b.iter(|| {
            let query = QueryParser::parse_query(black_box(nested_query())).unwrap();
            validator.validate(&query.root).unwrap();
            let _optimized = optimizer.optimize_query(query);
        });
    });
    group.finish();
}

// ============================================================================
// Comparative Benchmarks
// ============================================================================

fn bench_cache_vs_parse(c: &mut Criterion) {
    let cache = AstParseCache::new(1000);
    let ast = QueryParser::parse_query(simple_query()).unwrap();
    let parsed = ParsedQuery::from_ast(Arc::new(ast)).unwrap();
    let query_str = simple_query().to_string();
    cache.insert(query_str.clone(), parsed);

    let mut group = c.benchmark_group("query_cache_vs_parse");

    group.bench_function(BenchmarkId::new("comparison", "cache_hit"), |b| {
        b.iter(|| cache.get(black_box(&query_str)));
    });

    group.bench_function(BenchmarkId::new("comparison", "fresh_parse"), |b| {
        b.iter(|| QueryParser::parse_query(black_box(simple_query())));
    });

    group.finish();
}

// ============================================================================
// Criterion Group Setup
// ============================================================================

criterion_group!(
    benches,
    // Parsing
    bench_parse_simple,
    bench_parse_complex,
    bench_parse_nested,
    bench_parse_large,
    bench_parse_regex,
    bench_parse_lookaround,
    // Lexing
    bench_lex_simple,
    bench_lex_complex,
    // Validation
    bench_validate_simple_valid,
    bench_validate_complex_valid,
    bench_validate_with_fuzzy,
    bench_validate_regex_pattern,
    bench_validate_lookaround_pattern,
    // Optimization
    bench_optimize_simple,
    bench_optimize_complex,
    bench_optimize_nested,
    bench_optimize_large,
    // Caching
    bench_cache_hit,
    bench_cache_miss_and_store,
    bench_cache_hit_complex,
    bench_cache_cold_start,
    // Pipeline
    bench_full_pipeline_simple,
    bench_full_pipeline_complex,
    bench_full_pipeline_nested,
    // Comparisons
    bench_cache_vs_parse,
);

criterion_main!(benches);
