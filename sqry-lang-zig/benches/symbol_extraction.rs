use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_zig::ZigPlugin;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::PathBuf;

fn generate_large_zig_file(num_functions: usize) -> String {
    let mut code = String::new();
    code.push_str("const std = @import(\"std\");\n\n");

    // Add structs
    for i in 0..num_functions / 10 {
        write!(
            code,
            "pub const Struct{i} = struct {{\n    x: i32,\n    y: i32,\n}};\n\n"
        )
        .expect("write struct fixture");
    }

    // Add functions
    for i in 0..num_functions {
        write!(
            code,
            "pub fn function{i}(a: i32, b: i32) i32 {{\n    return a + b + {i};\n}}\n\n"
        )
        .expect("write function fixture");
    }

    // Add tests
    for i in 0..num_functions / 10 {
        write!(
            code,
            "test \"test {i}\" {{\n    try std.testing.expect(true);\n}}\n\n"
        )
        .expect("write test fixture");
    }

    // Add enums
    for i in 0..num_functions / 10 {
        write!(
            code,
            "pub const Enum{i} = enum {{\n    variant_a,\n    variant_b,\n}};\n\n"
        )
        .expect("write enum fixture");
    }

    code
}

fn bench_graph_build(c: &mut Criterion) {
    let plugin = ZigPlugin::default();
    let builder = plugin.graph_builder().expect("graph builder");

    // Small file (~100 lines)
    let small_code = generate_large_zig_file(10);
    c.bench_function("zig_graph_build_100_lines", |b| {
        b.iter(|| {
            let tree = plugin
                .parse_ast(black_box(small_code.as_bytes()))
                .expect("parse AST");
            let mut staging = StagingGraph::new();
            let result = builder.build_graph(
                &tree,
                black_box(small_code.as_bytes()),
                black_box(&PathBuf::from("bench.zig")),
                &mut staging,
            );
            black_box(result)
        });
    });

    // Medium file (~500 lines)
    let medium_code = generate_large_zig_file(50);
    c.bench_function("zig_graph_build_500_lines", |b| {
        b.iter(|| {
            let tree = plugin
                .parse_ast(black_box(medium_code.as_bytes()))
                .expect("parse AST");
            let mut staging = StagingGraph::new();
            let result = builder.build_graph(
                &tree,
                black_box(medium_code.as_bytes()),
                black_box(&PathBuf::from("bench.zig")),
                &mut staging,
            );
            black_box(result)
        });
    });

    // Large file (~1000 lines) - AC-ZIG-7 target
    let large_code = generate_large_zig_file(100);
    println!("\n=== AC-ZIG-7 Performance Test ===");
    println!("Generated code: {} lines", large_code.lines().count());
    println!("Target: <100ms for 1000-line file\n");

    c.bench_function("zig_graph_build_1000_lines", |b| {
        b.iter(|| {
            let tree = plugin
                .parse_ast(black_box(large_code.as_bytes()))
                .expect("parse AST");
            let mut staging = StagingGraph::new();
            let result = builder.build_graph(
                &tree,
                black_box(large_code.as_bytes()),
                black_box(&PathBuf::from("bench.zig")),
                &mut staging,
            );
            black_box(result)
        });
    });
}

criterion_group!(benches, bench_graph_build);
criterion_main!(benches);
