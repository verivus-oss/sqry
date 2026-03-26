//! Criterion benchmarks for the pooled lexer path.
//!
//! Mirrors the baseline harness but routes tokenization through `with_lexer`
//! so we can measure improvements delivered by the thread-local pool.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::query::lexer::tokenize_with_pool;
use std::hint::black_box;
use std::sync::OnceLock;

fn long_query() -> &'static str {
    static LONG: OnceLock<String> = OnceLock::new();
    LONG.get_or_init(|| {
        (0..128)
            .map(|i| format!("name:value{i}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    })
    .as_str()
}

fn bench_pooled_simple_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_pooled_simple");
    group.bench_function(BenchmarkId::new("pooled", "simple"), |b| {
        b.iter(|| tokenize_with_pool(black_box("kind:function AND async:true")).unwrap());
    });
    group.finish();
}

fn bench_pooled_complex_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_pooled_complex");
    group.bench_function(BenchmarkId::new("pooled", "complex"), |b| {
        b.iter(|| {
            tokenize_with_pool(black_box(
                "(kind:function OR kind:method) AND (lang:rust OR lang:python) AND NOT name~=/test/",
            ))
            .unwrap()
      });
  });
    group.finish();
}

fn bench_pooled_repeated_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_pooled_repeated_100x");
    group.bench_function(BenchmarkId::new("pooled", "repeated_100x"), |b| {
        b.iter(|| {
            for _ in 0..100 {
                tokenize_with_pool(black_box("kind:function")).unwrap();
            }
        });
    });
    group.finish();
}

fn bench_pooled_long_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_pooled_long");
    group.bench_function(BenchmarkId::new("pooled", "long"), |b| {
        b.iter(|| tokenize_with_pool(black_box(long_query())).unwrap());
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_pooled_simple_query,
    bench_pooled_complex_query,
    bench_pooled_repeated_query,
    bench_pooled_long_query
);
criterion_main!(benches);
