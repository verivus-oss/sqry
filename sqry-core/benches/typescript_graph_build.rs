use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_typescript::relations::TypeScriptGraphBuilder;
use std::hint::black_box;
use std::path::Path;
use tree_sitter::Parser;

fn parse_typescript_file(path: &Path) -> (tree_sitter::Tree, String) {
    let content = std::fs::read_to_string(path).expect("read file");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("set language");
    let tree = parser.parse(&content, None).expect("parse");
    (tree, content)
}

fn bench_typescript_files(c: &mut Criterion) {
    // Use real-world TypeScript library files for benchmarking
    let test_files = vec![
        (
            "react-reconciler.ts",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react-reconciler/src/ReactFiberBeginWork.js",
            3500, // Approximate LOC
        ),
        (
            "react-hooks.ts",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react-reconciler/src/ReactFiberHooks.js",
            3800,
        ),
        (
            "react-commit.ts",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react-reconciler/src/ReactFiberCommitWork.js",
            3200,
        ),
    ];

    let mut group = c.benchmark_group("typescript_graph_build");

    // Parse all files once for the aggregate benchmark (skip missing files)
    let parsed_files: Vec<_> = test_files
        .iter()
        .filter_map(|(name, path_str, _loc)| {
            let path = Path::new(path_str);
            if !path.exists() {
                eprintln!("Warning: Skipping {name} from aggregate - file not found");
                return None;
            }
            let (tree, content) = parse_typescript_file(path);
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

        let (tree, content) = parse_typescript_file(path);

        group.bench_with_input(
            BenchmarkId::new("graph_build", format!("{name} ({loc} LOC)")),
            &(&tree, &content, path),
            |b, &(tree, content, path)| {
                b.iter(|| {
                    let mut staging = StagingGraph::new();
                    let builder = TypeScriptGraphBuilder::default();
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
                    let builder = TypeScriptGraphBuilder::default();
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

criterion_group!(benches, bench_typescript_files);
criterion_main!(benches);
