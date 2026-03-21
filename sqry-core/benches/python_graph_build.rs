use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_python::relations::PythonGraphBuilder;
use std::hint::black_box;
use std::path::Path;
use tree_sitter::Parser;

fn parse_python_file(path: &Path) -> (tree_sitter::Tree, String) {
    let content = std::fs::read_to_string(path).expect("read file");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("set language");
    let tree = parser.parse(&content, None).expect("parse");
    (tree, content)
}

fn bench_python_files(c: &mut Criterion) {
    // Use standard library files for benchmarking
    let test_files = vec![
        (
            "pathlib.py",
            "/usr/lib/python3.10/pathlib.py",
            1177, // Approximate LOC
        ),
        (
            "collections.py",
            "/usr/lib/python3.10/collections/__init__.py",
            1217,
        ),
        ("functools.py", "/usr/lib/python3.10/functools.py", 936),
        ("typing.py", "/usr/lib/python3.10/typing.py", 2811),
    ];

    let mut group = c.benchmark_group("python_graph_build");

    // Parse all files once for the aggregate benchmark (skip missing files)
    let parsed_files: Vec<_> = test_files
        .iter()
        .filter_map(|(name, path_str, _loc)| {
            let path = Path::new(path_str);
            if !path.exists() {
                eprintln!("Warning: Skipping {name} from aggregate - file not found");
                return None;
            }
            let (tree, content) = parse_python_file(path);
            Some((*name, tree, content, path))
        })
        .collect();

    // Benchmark individual files
    for (name, path_str, loc) in &test_files {
        let path = Path::new(path_str);

        // Check if file exists, skip if not
        if !path.exists() {
            eprintln!("Warning: Skipping {name} - file not found");
            continue;
        }

        let (tree, content) = parse_python_file(path);

        group.bench_with_input(
            BenchmarkId::new("graph_build", format!("{name} ({loc} LOC)")),
            &(&tree, &content, path),
            |b, &(tree, content, path)| {
                b.iter(|| {
                    let mut staging = StagingGraph::new();
                    let builder = PythonGraphBuilder::default();
                    builder
                        .build_graph(
                            tree,
                            content.as_bytes(),
                            black_box(path),
                            black_box(&mut staging),
                        )
                        .expect("build graph");
                });
            },
        );
    }

    // Aggregate benchmark across all files
    if parsed_files.is_empty() {
        eprintln!("Warning: No test files available for aggregate benchmark");
    } else {
        group.bench_function(format!("all_{}_files", parsed_files.len()), |b| {
            b.iter(|| {
                for (_, tree, content, path) in &parsed_files {
                    let mut staging = StagingGraph::new();
                    let builder = PythonGraphBuilder::default();
                    builder
                        .build_graph(
                            tree,
                            content.as_bytes(),
                            black_box(*path),
                            black_box(&mut staging),
                        )
                        .expect("build graph");
                }
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_python_files);
criterion_main!(benches);
