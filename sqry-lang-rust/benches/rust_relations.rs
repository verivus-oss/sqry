use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_rust::RustPlugin;
use sqry_lang_rust::relations::RustGraphBuilder;
use std::hint::black_box;
use std::path::PathBuf;

fn generate_rust_file_with_relations(num_functions: usize) -> String {
    use std::fmt::Write as _;
    let mut code = String::new();

    // Add imports to generate Import edges
    code.push_str("use std::collections::HashMap;\n");
    code.push_str("use std::sync::Arc;\n");
    code.push_str("use std::io::{Read, Write};\n\n");

    // Add structs with fields to generate FieldAccess edges
    for i in 0..num_functions / 10 {
        write!(
            code,
            r"pub struct Data{i} {{
    pub value: i32,
    pub name: String,
    count: usize,
}}

"
        )
        .expect("write struct fixture");
    }

    // Add impl blocks with methods to generate Call edges (method calls)
    for i in 0..num_functions / 10 {
        write!(
            code,
            r"impl Data{i} {{
    pub fn new(value: i32) -> Self {{
        Self {{
            value,
            name: String::new(),
            count: 0,
      }}
  }}

    pub fn get_value(&self) -> i32 {{
        self.value
  }}

    pub fn increment(&mut self) {{
        self.count += 1;
  }}
}}

"
        )
        .expect("write impl fixture");
    }

    // Add standalone functions with various call patterns
    for i in 0..num_functions {
        let struct_idx = i % (num_functions / 10).max(1);
        write!(
            code,
            r"pub fn function{i}(a: i32, b: i32) -> i32 {{
    let data = Data{struct_idx}::new(a);
    let val = data.get_value();
    helper_{i}(val + b)
}}

fn helper_{i}(x: i32) -> i32 {{
    x * 2
}}

"
        )
        .expect("write function fixture");
    }

    // Add unsafe functions to test unsafe call tracking
    for i in 0..num_functions / 20 {
        write!(
            code,
            r"pub unsafe fn unsafe_fn{i}(ptr: *const i32) -> i32 {{
    *ptr
}}

pub fn safe_caller{i}() -> i32 {{
    let x = 42;
    unsafe {{ unsafe_fn{i}(&x) }}
}}

"
        )
        .expect("write unsafe fixture");
    }

    // Add async functions to test async tracking
    for i in 0..num_functions / 20 {
        write!(
            code,
            r"pub async fn async_fn{i}() -> i32 {{
    let result = compute{i}().await;
    result
}}

async fn compute{i}() -> i32 {{
    42
}}

"
        )
        .expect("write async fixture");
    }

    // Add exports (pub items already count, but add explicit re-exports)
    code.push_str("\n// Re-exports for Export edge testing\n");
    code.push_str("pub use std::collections::HashSet;\n");

    code
}

fn bench_relation_extraction(c: &mut Criterion) {
    let plugin = RustPlugin::default();
    let builder = RustGraphBuilder::default();

    // Small file (~100 lines)
    let small_code = generate_rust_file_with_relations(10);
    c.bench_function("rust_relations_100_lines", |b| {
        b.iter(|| {
            let tree = plugin.parse_ast(black_box(small_code.as_bytes())).unwrap();
            let mut staging = StagingGraph::new();
            let file_path = PathBuf::from("bench.rs");
            builder
                .build_graph(&tree, small_code.as_bytes(), &file_path, &mut staging)
                .unwrap();
            black_box(staging)
        });
    });

    // Medium file (~500 lines)
    let medium_code = generate_rust_file_with_relations(50);
    c.bench_function("rust_relations_500_lines", |b| {
        b.iter(|| {
            let tree = plugin.parse_ast(black_box(medium_code.as_bytes())).unwrap();
            let mut staging = StagingGraph::new();
            let file_path = PathBuf::from("bench.rs");
            builder
                .build_graph(&tree, medium_code.as_bytes(), &file_path, &mut staging)
                .unwrap();
            black_box(staging)
        });
    });

    // Large file (~1000 lines) - FR-RUST performance target
    let large_code = generate_rust_file_with_relations(100);
    println!("\n=== FR-RUST Performance Test ===");
    println!("Generated code: {} lines", large_code.lines().count());
    println!("Target: <100ms for 1000-line file\n");

    c.bench_function("rust_relations_1000_lines", |b| {
        b.iter(|| {
            let tree = plugin.parse_ast(black_box(large_code.as_bytes())).unwrap();
            let mut staging = StagingGraph::new();
            let file_path = PathBuf::from("bench.rs");
            builder
                .build_graph(&tree, large_code.as_bytes(), &file_path, &mut staging)
                .unwrap();
            black_box(staging)
        });
    });
}

fn bench_graph_build(c: &mut Criterion) {
    let plugin = RustPlugin::default();
    let builder = plugin.graph_builder().expect("graph builder");

    // Small file (~100 lines)
    let small_code = generate_rust_file_with_relations(10);
    c.bench_function("rust_symbols_100_lines", |b| {
        b.iter(|| {
            let tree = plugin.parse_ast(black_box(small_code.as_bytes())).unwrap();
            let mut staging = StagingGraph::new();
            let file_path = PathBuf::from("bench.rs");
            builder
                .build_graph(&tree, small_code.as_bytes(), &file_path, &mut staging)
                .unwrap();
            black_box(staging)
        });
    });

    // Large file (~1000 lines)
    let large_code = generate_rust_file_with_relations(100);
    c.bench_function("rust_symbols_1000_lines", |b| {
        b.iter(|| {
            let tree = plugin.parse_ast(black_box(large_code.as_bytes())).unwrap();
            let mut staging = StagingGraph::new();
            let file_path = PathBuf::from("bench.rs");
            builder
                .build_graph(&tree, large_code.as_bytes(), &file_path, &mut staging)
                .unwrap();
            black_box(staging)
        });
    });
}

criterion_group!(benches, bench_relation_extraction, bench_graph_build);
criterion_main!(benches);
