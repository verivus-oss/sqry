use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_javascript::relations::JavaScriptGraphBuilder;
use std::hint::black_box;
use std::path::Path;
use tree_sitter::Parser;

fn parse_javascript_file(path: &Path) -> (tree_sitter::Tree, String) {
    let content = std::fs::read_to_string(path).expect("read file");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("set language");
    let tree = parser.parse(&content, None).expect("parse");
    (tree, content)
}

fn bench_react_files(c: &mut Criterion) {
    let test_files = vec![
        (
            "domEvents.js",
            "/mnt/sqry-test/-performance/repos/facebook_react/packages/dom-event-testing-library/domEvents.js",
            442, // LOC
        ),
        (
            "JestReact.js",
            "/mnt/sqry-test/-performance/repos/facebook_react/packages/jest-react/src/JestReact.js",
            137,
        ),
        (
            "ReactFabricComponentTree.js",
            "/mnt/sqry-test/-performance/repos/facebook_react/packages/react-native-renderer/src/ReactFabricComponentTree.js",
            55,
        ),
        (
            "ReactNativeInjection.js",
            "/mnt/sqry-test/-performance/repos/facebook_react/packages/react-native-renderer/src/ReactNativeInjection.js",
            41,
        ),
    ];

    let mut group = c.benchmark_group("javascript_graph_build");

    // Parse all files once for the aggregate benchmark (skip missing files)
    let parsed_files: Vec<_> = test_files
        .iter()
        .filter_map(|(name, path_str, _loc)| {
            let path = Path::new(path_str);
            if !path.exists() {
                eprintln!("Warning: Skipping {name} from aggregate - file not found");
                return None;
            }
            let (tree, content) = parse_javascript_file(path);
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

        let (tree, content) = parse_javascript_file(path);

        group.bench_with_input(
            BenchmarkId::new("graph_build", format!("{name} ({loc} LOC)")),
            &(&tree, &content, path),
            |b, &(tree, content, path)| {
                b.iter(|| {
                    let mut staging = StagingGraph::new();
                    let builder = JavaScriptGraphBuilder::default();
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

    // Aggregate benchmark for all available files
    if parsed_files.is_empty() {
        eprintln!("Warning: No test files available for aggregate benchmark");
    } else {
        group.bench_function(format!("all_{}_files", parsed_files.len()), |b| {
            b.iter(|| {
                for (_, tree, content, path) in &parsed_files {
                    let mut staging = StagingGraph::new();
                    let builder = JavaScriptGraphBuilder::default();
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

criterion_group!(benches, bench_react_files);
criterion_main!(benches);
