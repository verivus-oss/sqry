use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_cpp::relations::CppGraphBuilder;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use tree_sitter::Parser;

fn parse_cpp_file(path: &Path) -> (tree_sitter::Tree, Vec<u8>) {
    let content = fs::read(path).expect("read file");
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser
        .set_language(&language.into())
        .expect("load C++ grammar");
    let tree = parser.parse(&content, None).expect("parse C++");
    (tree, content)
}

fn bench_react_files(c: &mut Criterion) {
    let test_files = vec![
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/jsc-perf.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/thread-local.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/hardware-counter.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/perf-counters.cpp",
    ];

    let mut group = c.benchmark_group("cpp_graph_build");

    for file_path in test_files {
        let path = Path::new(file_path);
        if !path.exists() {
            eprintln!("Skipping missing file: {file_path}");
            continue;
        }

        let file_name = path.file_name().unwrap().to_str().unwrap();
        let (tree, content) = parse_cpp_file(path);

        group.bench_with_input(
            BenchmarkId::new("graph_build", file_name),
            &(&tree, &content, path),
            |b, &(tree, content, path)| {
                b.iter(|| {
                    let mut staging = StagingGraph::new();
                    let builder = CppGraphBuilder;
                    builder
                        .build_graph(tree, content, path, black_box(&mut staging))
                        .expect("build graph");
                    black_box(&staging);
                });
            },
        );
    }

    group.finish();
}

fn bench_all_react_files_together(c: &mut Criterion) {
    let test_files = [
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/jsc-perf.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/thread-local.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/hardware-counter.cpp",
        "/mnt/sqry-test/-performance/repos/facebook_react/scripts/perf-counters/src/perf-counters.cpp",
    ];

    // Pre-parse all files
    let parsed: Vec<_> = test_files
        .iter()
        .filter_map(|file_path| {
            let path = Path::new(file_path);
            if path.exists() {
                Some((parse_cpp_file(path), path))
            } else {
                None
            }
        })
        .collect();

    if parsed.is_empty() {
        eprintln!("No React files found for benchmarking");
        return;
    }

    c.bench_function("cpp_graph_build_all_4_files", |b| {
        b.iter(|| {
            for ((tree, content), path) in &parsed {
                let mut staging = StagingGraph::new();
                let builder = CppGraphBuilder;
                builder
                    .build_graph(tree, content, path, black_box(&mut staging))
                    .expect("build graph");
                black_box(&staging);
            }
        });
    });
}

criterion_group!(benches, bench_react_files, bench_all_react_files_together);
criterion_main!(benches);
