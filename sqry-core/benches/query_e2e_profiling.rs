// FR-2025-005 Query Pipeline Profiling Benchmark
// Measures the query processing pipeline: classification → parsing → optimization
//
// Use with: cargo bench --bench query_e2e_profiling
// Or flamegraph: cargo flamegraph --bench query_e2e_profiling -- --bench

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::query::QueryParser;
use sqry_core::query::optimizer::Optimizer;
use sqry_core::query::registry::FieldRegistry;
use sqry_core::search::classifier::QueryClassifier;
use std::hint::black_box;

fn bench_classification_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("01_classification");

    let queries = vec![
        ("semantic_simple", "kind:function"),
        (
            "semantic_complex",
            "kind:function AND async:true OR visibility:public",
        ),
        (
            "semantic_nested",
            "(kind:function OR kind:method) AND (async:true OR static:true)",
        ),
        ("text_marker", "TODO: fix this"),
        ("text_regex", "^fn main"),
        ("hybrid_simple", "process_data"),
        ("hybrid_identifier", "UserModel"),
    ];

    for (name, query) in queries {
        group.bench_with_input(BenchmarkId::from_parameter(name), &query, |b, q| {
            b.iter(|| {
                let result = QueryClassifier::classify(black_box(q));
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_query_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("02_parsing");

    let queries = vec![
        ("simple_field", "kind:function"),
        ("and_query", "kind:function AND name:process"),
        ("or_query", "kind:function OR kind:struct"),
        ("not_query", "kind:function AND NOT name:test"),
        (
            "complex_nested",
            "(kind:function AND async:true) OR (kind:struct AND name:User)",
        ),
        (
            "very_complex",
            "((kind:function OR kind:method) AND (async:true OR static:true)) AND NOT (name:test OR name:mock)",
        ),
    ];

    for (name, query) in queries {
        group.bench_with_input(BenchmarkId::from_parameter(name), &query, |b, q| {
            b.iter(|| {
                let result = QueryParser::parse_query(black_box(q));
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_query_optimization(c: &mut Criterion) {
    let mut group = c.benchmark_group("03_optimization");

    // Parse queries once for optimization benchmarks
    let test_queries = vec![
        ("simple", "kind:function"),
        ("and_chain", "kind:function AND name:process AND async:true"),
        ("or_branches", "kind:function OR kind:struct OR kind:class"),
        (
            "nested_and_or",
            "(kind:function AND async:true) OR (kind:struct AND name:User)",
        ),
        ("double_not", "NOT NOT kind:function"),
        (
            "complex",
            "((kind:function OR kind:method) AND async:true) AND NOT (name:test OR name:mock)",
        ),
    ];

    let registry = FieldRegistry::default();
    let optimizer = Optimizer::new(registry);

    for (name, query_str) in test_queries {
        let query = QueryParser::parse_query(query_str).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(name), &query, |b, q| {
            b.iter(|| {
                let result = optimizer.optimize(black_box(q.root.clone()));
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_end_to_end_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("04_e2e_pipeline");
    group.sample_size(50); // Reduce samples for E2E

    let queries = vec![
        ("simple", "kind:function"),
        ("realistic_1", "kind:function AND async:true"),
        ("realistic_2", "name:process AND kind:function"),
        (
            "realistic_3",
            "(kind:function OR kind:method) AND async:true",
        ),
        (
            "complex",
            "((kind:function AND async:true) OR (kind:struct AND name:User)) AND NOT name:test",
        ),
    ];

    let registry = FieldRegistry::default();
    let optimizer = Optimizer::new(registry);

    for (name, query_str) in queries {
        group.bench_with_input(BenchmarkId::from_parameter(name), &query_str, |b, q| {
            b.iter(|| {
                // Full pipeline: classify → parse → optimize
                let _classification = QueryClassifier::classify(black_box(q));
                let parsed = QueryParser::parse_query(black_box(q)).unwrap();
                let optimized_query = optimizer.optimize(black_box(parsed.root));
                black_box(optimized_query)
            });
        });
    }

    group.finish();
}

fn bench_pathological_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("05_pathological");

    // Deep nesting
    let deep_nested = "((((kind:function AND async:true) OR (kind:struct AND name:User)) OR (kind:class AND name:Model)) OR (kind:interface AND name:Service))";
    group.bench_function("deep_nesting", |b| {
        b.iter(|| {
            let result = QueryParser::parse_query(black_box(deep_nested));
            black_box(result)
        });
    });

    // Many OR branches (potential optimization challenge)
    let many_or = "kind:function OR kind:struct OR kind:class OR kind:interface OR kind:enum OR kind:type OR kind:variable OR kind:constant";
    group.bench_function("many_or_branches", |b| {
        b.iter(|| {
            let result = QueryParser::parse_query(black_box(many_or));
            black_box(result)
        });
    });

    // Long AND chain
    let long_and = "kind:function AND async:true AND static:false AND visibility:public AND name:process AND lang:rust";
    group.bench_function("long_and_chain", |b| {
        b.iter(|| {
            let result = QueryParser::parse_query(black_box(long_and));
            black_box(result)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_classification_overhead,
    bench_query_parsing,
    bench_query_optimization,
    bench_end_to_end_pipeline,
    bench_pathological_cases
);
criterion_main!(benches);
