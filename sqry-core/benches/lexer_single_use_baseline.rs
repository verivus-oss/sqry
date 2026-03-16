//! Baseline benchmarks for the query lexer.
//!
//! These benches capture the performance profile of the pre-optimization
//! implementation (fresh lexer per call, no buffer reuse). The results serve
//! as the control group for future buffer pooling experiments.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
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
use sqry_core::query::lexer::Lexer;

fn bench_simple_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_baseline_simple");
    group.bench_function(BenchmarkId::new("fresh", "simple"), |b| {
        b.iter(|| {
            let input = black_box("kind:function AND async:true");
            let mut lexer = Lexer::new(input);
            lexer.tokenize().unwrap()
        });
    });
    group.finish();
}

fn bench_complex_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_baseline_complex");
    group.bench_function(BenchmarkId::new("fresh", "complex"), |b| {
        b.iter(|| {
            let input = black_box("(kind:function OR kind:method) AND (lang:rust OR lang:python) AND NOT name~=/test/");
            let mut lexer = Lexer::new(input);
            lexer.tokenize().unwrap()
      });
  });
    group.finish();
}

fn bench_repeated_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_baseline_repeated_5x");
    group.bench_function(BenchmarkId::new("fresh", "repeated_100x"), |b| {
        b.iter(|| {
            for _ in 0..100 {
                let input = black_box("kind:function");
                let mut lexer = Lexer::new(input);
                lexer.tokenize().unwrap();
            }
        });
    });
    group.finish();
}

fn bench_long_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer_baseline_long");
    group.bench_function(BenchmarkId::new("fresh", "long"), |b| {
        b.iter(|| {
            let input = black_box(long_query());
            let mut lexer = Lexer::new(input);
            lexer.tokenize().unwrap();
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_simple_query,
    bench_complex_query,
    bench_repeated_query,
    bench_long_query
);
criterion_main!(benches);
