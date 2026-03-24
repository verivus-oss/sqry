//! AST Operations Benchmark Suite (FT-D.2)
//!
//! Benchmarks for AST parsing, metadata extraction, and normalization operations.
//! These benchmarks establish performance baselines and detect regressions.
//!
//! **Categories:**
//! 1. **Parse AST**: Measure tree-sitter parsing performance across languages and file sizes
//! 2. **Build Graph**: Measure graph construction and metadata generation
//! 3. **Normalize Metadata**: Measure metadata normalization overhead
//! 4. **Query Rewrite**: Measure query field name normalization overhead
//!
//! **File Sizes Tested:**
//! - Small: 1KB (~30 lines)
//! - Medium: 10KB (~300 lines)
//! - Large: 100KB (~3000 lines)
//!
//! **Run benchmarks:**
//! ```bash
//! cargo bench -p sqry-core --bench ast_operations
//! ```

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::graph::unified::build::StagingGraph;
use sqry_core::normalizer::MetadataNormalizer;
use sqry_core::plugin::PluginManager;
use std::collections::HashMap;
use std::fmt::Write;
use std::hint::black_box;
use std::path::PathBuf;

// ============================================================================
// TEST DATA GENERATION
// ============================================================================

/// Generate Python source code with N functions
fn generate_python_source(num_functions: usize) -> String {
    let mut source = String::new();
    source.push_str("# Generated Python source for benchmarking\n\n");

    for i in 0..num_functions {
        let is_async = i % 2 == 0;
        if is_async {
            let _ = writeln!(source, "async def async_function_{i}():");
        } else {
            let _ = writeln!(source, "def sync_function_{i}():");
        }
        source.push_str("    \"\"\"Docstring\"\"\"\n");
        source.push_str("    pass\n\n");
    }

    source
}

/// Generate Rust source code with N functions
fn generate_rust_source(num_functions: usize) -> String {
    let mut source = String::new();
    source.push_str("// Generated Rust source for benchmarking\n\n");

    for i in 0..num_functions {
        let is_async = i % 2 == 0;
        let is_unsafe = i % 3 == 0;
        let visibility = if i % 4 == 0 { "pub " } else { "" };

        if is_unsafe {
            let _ = writeln!(source, "{visibility}unsafe fn unsafe_function_{i}() {{");
        } else if is_async {
            let _ = writeln!(source, "{visibility}async fn async_function_{i}() {{");
        } else {
            let _ = writeln!(source, "{visibility}fn sync_function_{i}() {{");
        }
        source.push_str("    // Function body\n");
        source.push_str("}\n\n");
    }

    source
}

/// Generate TypeScript source code with N functions
fn generate_typescript_source(num_functions: usize) -> String {
    let mut source = String::new();
    source.push_str("// Generated TypeScript source for benchmarking\n\n");

    for i in 0..num_functions {
        let is_async = i % 2 == 0;
        if is_async {
            let _ = writeln!(source, "async function asyncFunction{i}() {{");
        } else {
            let _ = writeln!(source, "function syncFunction{i}() {{");
        }
        source.push_str("  // Function body\n");
        source.push_str("}\n\n");
    }

    source
}

/// Generate metadata hashmap for benchmarking normalization
fn generate_metadata(num_entries: usize) -> HashMap<String, String> {
    let mut metadata = HashMap::new();

    for i in 0..num_entries {
        match i % 5 {
            0 => {
                metadata.insert("is_async".to_string(), "true".to_string());
            }
            1 => {
                metadata.insert("visibility".to_string(), "public".to_string());
            }
            2 => {
                metadata.insert("is_static".to_string(), "false".to_string());
            }
            3 => {
                metadata.insert("return_type".to_string(), "String".to_string());
            }
            4 => {
                metadata.insert(format!("custom_key_{i}"), format!("value_{i}"));
            }
            _ => unreachable!(),
        }
    }

    metadata
}

// ============================================================================
// BENCHMARK 1: PARSE AST (Multiple Languages × File Sizes)
// ============================================================================

fn benchmark_parse_ast(c: &mut Criterion) {
    let mut group = c.benchmark_group("ast_parse");

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));

    // Test different file sizes (small, medium, large)
    let sizes = vec![
        ("small", 10),   // ~10 functions ≈ 0.5KB
        ("medium", 100), // ~100 functions ≈ 5KB
        ("large", 500),  // ~500 functions ≈ 25KB
    ];

    for (size_name, num_functions) in sizes {
        // Python
        let python_source = generate_python_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("python", size_name),
            &python_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("py").unwrap();
                b.iter(|| {
                    let _ = plugin.parse_ast(black_box(source.as_bytes()));
                });
            },
        );

        // Rust
        let rust_source = generate_rust_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("rust", size_name),
            &rust_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("rs").unwrap();
                b.iter(|| {
                    let _ = plugin.parse_ast(black_box(source.as_bytes()));
                });
            },
        );

        // TypeScript
        let typescript_source = generate_typescript_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("typescript", size_name),
            &typescript_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("ts").unwrap();
                b.iter(|| {
                    let _ = plugin.parse_ast(black_box(source.as_bytes()));
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// BENCHMARK 2: BUILD GRAPH + METADATA
// ============================================================================

fn benchmark_extract_metadata(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_build");

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));

    // Test different symbol counts
    let sizes = vec![("small", 10), ("medium", 100), ("large", 500)];

    for (size_name, num_functions) in sizes {
        // Python
        let python_source = generate_python_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("python", size_name),
            &python_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("py").unwrap();
                let builder = plugin.graph_builder().expect("graph builder");
                let path = PathBuf::from("benchmark.py");
                b.iter(|| {
                    let tree = plugin
                        .parse_ast(black_box(source.as_bytes()))
                        .expect("parse ast");
                    let mut staging = StagingGraph::new();
                    builder
                        .build_graph(&tree, source.as_bytes(), &path, &mut staging)
                        .expect("build graph");
                });
            },
        );

        // Rust
        let rust_source = generate_rust_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("rust", size_name),
            &rust_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("rs").unwrap();
                let builder = plugin.graph_builder().expect("graph builder");
                let path = PathBuf::from("benchmark.rs");
                b.iter(|| {
                    let tree = plugin
                        .parse_ast(black_box(source.as_bytes()))
                        .expect("parse ast");
                    let mut staging = StagingGraph::new();
                    builder
                        .build_graph(&tree, source.as_bytes(), &path, &mut staging)
                        .expect("build graph");
                });
            },
        );

        // TypeScript
        let typescript_source = generate_typescript_source(num_functions);
        group.bench_with_input(
            BenchmarkId::new("typescript", size_name),
            &typescript_source,
            |b, source| {
                let plugin = manager.plugin_for_extension("ts").unwrap();
                let builder = plugin.graph_builder().expect("graph builder");
                let path = PathBuf::from("benchmark.ts");
                b.iter(|| {
                    let tree = plugin
                        .parse_ast(black_box(source.as_bytes()))
                        .expect("parse ast");
                    let mut staging = StagingGraph::new();
                    builder
                        .build_graph(&tree, source.as_bytes(), &path, &mut staging)
                        .expect("build graph");
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// BENCHMARK 3: METADATA NORMALIZATION
// ============================================================================

fn benchmark_normalize_metadata(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_normalization");

    let normalizer = MetadataNormalizer::new();

    // Test different metadata sizes
    let sizes = vec![
        ("small", 5),   // 5 metadata entries
        ("medium", 20), // 20 metadata entries
        ("large", 100), // 100 metadata entries
    ];

    for (size_name, num_entries) in sizes {
        let metadata = generate_metadata(num_entries);

        group.bench_with_input(
            BenchmarkId::from_parameter(size_name),
            &metadata,
            |b, meta| {
                b.iter(|| {
                    let _ = normalizer.normalize(black_box(meta.clone()));
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// BENCHMARK 4: QUERY FIELD NORMALIZATION
// ============================================================================

fn benchmark_query_field_normalization(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_field_normalization");

    let normalizer = MetadataNormalizer::new();

    // Test different field name lookups
    let fields = vec![
        ("async", "is_async"),
        ("static", "is_static"),
        ("unsafe", "is_unsafe"),
        ("const", "is_const"),
        ("mutable", "is_mutable"),
    ];

    for (short_form, _canonical) in fields {
        group.bench_with_input(
            BenchmarkId::from_parameter(short_form),
            short_form,
            |b, field| {
                b.iter(|| {
                    let _ = normalizer.get_canonical(black_box(field));
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// BENCHMARK 5: END-TO-END AST PIPELINE
// ============================================================================

fn benchmark_e2e_ast_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_ast_pipeline");

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));

    // Medium size file (100 functions)
    let python_source = generate_python_source(100);
    let path = PathBuf::from("benchmark.py");

    group.bench_function("parse_extract_normalize", |b| {
        let plugin = manager.plugin_for_extension("py").unwrap();
        let normalizer = MetadataNormalizer::new();

        let builder = plugin.graph_builder().expect("graph builder");

        b.iter(|| {
            // Full pipeline: parse → build graph → normalize metadata
            let tree = plugin
                .parse_ast(black_box(python_source.as_bytes()))
                .expect("parse ast");
            let mut staging = StagingGraph::new();
            builder
                .build_graph(&tree, python_source.as_bytes(), &path, &mut staging)
                .expect("build graph");

            for op in staging.operations() {
                if let sqry_core::graph::unified::build::staging::StagingOp::AddNode {
                    entry, ..
                } = op
                {
                    let mut raw = HashMap::new();
                    if entry.is_async {
                        raw.insert("async".to_string(), "true".to_string());
                    }
                    if entry.is_static {
                        raw.insert("static".to_string(), "true".to_string());
                    }
                    let _ = normalizer.normalize(black_box(raw));
                }
            }

            black_box(staging.operations().len());
        });
    });

    group.finish();
}

// ============================================================================
// CRITERION CONFIGURATION
// ============================================================================

criterion_group!(
    benches,
    benchmark_parse_ast,
    benchmark_extract_metadata,
    benchmark_normalize_metadata,
    benchmark_query_field_normalization,
    benchmark_e2e_ast_pipeline,
);

criterion_main!(benches);
