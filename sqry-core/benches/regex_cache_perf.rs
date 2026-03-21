use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::query::regex_cache::get_or_compile_regex;
use std::hint::black_box;

#[allow(clippy::regex_creation_in_loops)] // Intentional: benchmark repeated regex compilation
fn bench_regex_uncached(c: &mut Criterion) {
    c.bench_function("regex_uncached_1000_matches", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let re = regex::Regex::new("process.*").unwrap();
                black_box(re.is_match("process_data"));
            }
        });
    });
}

fn bench_regex_cached(c: &mut Criterion) {
    c.bench_function("regex_cached_1000_matches", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let re = get_or_compile_regex("process.*", false, false, false).unwrap();
                black_box(re.is_match("process_data"));
            }
        });
    });
}

criterion_group!(benches, bench_regex_uncached, bench_regex_cached);
criterion_main!(benches);
