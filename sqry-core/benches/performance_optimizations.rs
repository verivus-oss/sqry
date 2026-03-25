// Performance Optimization Benchmarks
// Phase 1: Classifier optimization benchmarks

use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::search::classifier::QueryClassifier;
use std::hint::black_box;

fn bench_classifier(c: &mut Criterion) {
    let mut group = c.benchmark_group("classifier");

    // Semantic queries with different patterns
    group.bench_function("semantic_single_field", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("kind:function"));
            black_box(ty)
        });
    });

    group.bench_function("semantic_relation_parentheses", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("callers(MyFunction)"));
            black_box(ty)
        });
    });

    group.bench_function("semantic_relation_colon", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("callees:bar"));
            black_box(ty)
        });
    });

    group.bench_function("semantic_complex", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box(
                "kind:function AND async:true OR visibility:public",
            ));
            black_box(ty)
        });
    });

    group.bench_function("semantic_ast_node", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("@function.def"));
            black_box(ty)
        });
    });

    group.bench_function("semantic_symbol_path", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("foo::bar::baz"));
            black_box(ty)
        });
    });

    // Text queries with different patterns
    group.bench_function("text_simple_marker", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("TODO: fix this"));
            black_box(ty)
        });
    });

    group.bench_function("text_regex_anchor", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("^fn main"));
            black_box(ty)
        });
    });

    group.bench_function("text_character_class", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("fn \\w+"));
            black_box(ty)
        });
    });

    // Hybrid/ambiguous queries
    group.bench_function("hybrid_simple_identifier", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("find_user"));
            black_box(ty)
        });
    });

    group.bench_function("hybrid_short_query", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("error"));
            black_box(ty)
        });
    });

    // Edge cases
    group.bench_function("empty_query", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box(""));
            black_box(ty)
        });
    });

    group.bench_function("long_query", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box(
                "kind:function AND async:true OR (callers(foo) AND callees(bar)) OR visibility:public AND scope:module::MyClass",
            ));
            black_box(ty)
      });
  });

    group.bench_function("unicode_query", |b| {
        b.iter(|| {
            let ty = QueryClassifier::classify(black_box("kind:函数 AND foo::バー::baz"));
            black_box(ty)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_classifier);
criterion_main!(benches);
