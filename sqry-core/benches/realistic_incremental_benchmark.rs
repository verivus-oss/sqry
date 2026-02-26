use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::ast::IncrementalParser;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;

// Use the real JavaScript plugin
use sqry_lang_javascript::JavaScriptPlugin;

/// Test with real React source files from facebook/react repository
fn bench_realistic_files(c: &mut Criterion) {
    let parser = IncrementalParser::with_default_capacity();
    let plugin = JavaScriptPlugin::default();

    // Test cases: Real files from React codebase at different sizes
    let test_cases = vec![
        (
            "small_200_lines",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react/src/ReactContext.js",
        ),
        (
            "medium_1000_lines",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react-reconciler/src/ReactFiberHooks.js",
        ),
        (
            "large_5000_lines",
            "/mnt/sqry-test/FR-2025-006-performance/repos/facebook_react/packages/react-reconciler/src/ReactFiberWorkLoop.js",
        ),
    ];

    for (name, file_path) in test_cases {
        // Check if file exists
        if !std::path::Path::new(file_path).exists() {
            eprintln!("Skipping benchmark {name}: file not found at {file_path}");
            continue;
        }

        let original_content = match fs::read(file_path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Failed to read {file_path}: {e}");
                continue;
            }
        };

        // Simulate a realistic edit: change one character in the middle
        let mut modified_content = original_content.clone();
        let mid = modified_content.len() / 2;
        if mid < modified_content.len() {
            // Change a character (e.g., 'a' -> 'b')
            if modified_content[mid] == b'a' {
                modified_content[mid] = b'b';
            } else {
                modified_content[mid] = b'a';
            }
        }

        let path = PathBuf::from(file_path);

        // Warm up cache with original parse
        parser.parse(&plugin, &path, &original_content, None).ok();

        let mut group = c.benchmark_group(format!("realistic_{name}"));

        // Benchmark full parse (cache cleared)
        group.bench_with_input(
            BenchmarkId::new("full_parse", name),
            &modified_content,
            |b, content| {
                b.iter(|| {
                    parser.clear_cache();
                    black_box(parser.parse(&plugin, &path, content, None).ok())
                });
            },
        );

        // Benchmark incremental parse (with cache already warm)
        // IMPORTANT: Cache is warmed ONCE before the benchmark loop, not per iteration
        group.bench_with_input(
            BenchmarkId::new("incremental_parse_warm_cache", name),
            &modified_content,
            |b, content| {
                b.iter_with_setup(
                    || {
                        // Setup: Ensure cache is warm (happens before timing starts)
                        parser.parse(&plugin, &path, &original_content, None).ok();
                    },
                    |()| {
                        // This is what gets timed: just the incremental parse
                        black_box(
                            parser
                                .parse(&plugin, &path, content, Some(&original_content))
                                .ok(),
                        )
                    },
                );
            },
        );

        group.finish();
    }
}

criterion_group!(benches, bench_realistic_files);
criterion_main!(benches);
