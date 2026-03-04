//! Criterion benchmarks for plugin registration and loading (Stream 1, Task 1.4)
//!
//! **IMPORTANT**: This file contains TWO types of benchmarks:
//!
//! 1. **In-Memory Registration** (fast, ~nanoseconds):
//!    - Measures registration of already-instantiated unit structs
//!    - Does NOT include tree-sitter initialization or grammar loading
//!    - Useful for understanding `PluginManager` overhead only
//!
//! 2. **Cold-Start Loading** (slow, ~milliseconds):
//!    - Measures plugin initialization including:
//!      * `PluginManager` creation and registration
//!      * Tree-sitter Parser creation
//!      * Language grammar loading via `set_language()`
//!      * Actual parsing with per-language appropriate code
//!    - NOTE: Tree-sitter grammars are statically linked (compiled into binary)
//!    - Does NOT include: Plugin discovery from disk, dynamic library loading, or I/O operations
//!    - Represents realistic startup cost for statically-linked plugin architecture
//!
//! Performance targets:
//! - In-memory registration: <10μs for 13 plugins
//! - Cold-start loading: <100ms for 13 plugins
//!
//! Methodology:
//! - Release build optimizations
//! - Criterion default: 100 iterations warm-up + measurement
//! - Statistical analysis with confidence intervals
//! - Black-box optimization barriers to prevent compiler elision
//!
//! RKG: implements TASK:STREAM-1-TASK-1.4-PERFORMANCE-BENCHMARKS

#![allow(deprecated)] // Benchmark code uses simplified plugin instantiation

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_core::plugin::{LanguagePlugin, PluginManager};
use std::hint::black_box;
use tree_sitter::Parser;

// ============================================================================
// Benchmark: Empty PluginManager (Baseline)
// ============================================================================

/// Measures time to create an empty `PluginManager` (0 plugins)
///
/// This establishes the baseline overhead of `PluginManager` struct allocation
/// and initialization without any plugins.
fn bench_empty_manager(c: &mut Criterion) {
    c.bench_function("plugin_manager_empty", |b| {
        b.iter(|| {
            let manager = black_box(PluginManager::new());
            black_box(manager)
        });
    });
}

// ============================================================================
// Benchmark: Single Plugin Registration
// ============================================================================

/// Measures time to create `PluginManager` + register 1 plugin
///
/// This measures the per-plugin registration overhead in isolation.
fn bench_single_plugin_registration(c: &mut Criterion) {
    c.bench_function("plugin_register_single_rust", |b| {
        b.iter(|| {
            let mut manager = black_box(PluginManager::new());
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            black_box(manager)
        });
    });
}

// ============================================================================
// Benchmark: Individual Plugin Loading (Per-Language)
// ============================================================================

/// Measures per-plugin overhead for each Tier 1 language plugin
///
/// This helps identify if any specific plugin has unusually high initialization cost.
fn bench_individual_plugins(c: &mut Criterion) {
    let mut group = c.benchmark_group("plugin_register_individual");

    // Tier 1: Core languages
    group.bench_function("go", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_go::GoPlugin::default())));
            black_box(manager)
        });
    });

    group.bench_function("java", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_java::JavaPlugin::default())));
            black_box(manager)
        });
    });

    group.bench_function("javascript", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            black_box(manager)
        });
    });

    group.bench_function("python", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            black_box(manager)
        });
    });

    group.bench_function("rust", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            black_box(manager)
        });
    });

    group.bench_function("swift", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_swift::SwiftPlugin::default())));
            black_box(manager)
        });
    });

    group.bench_function("typescript", |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(
                sqry_lang_typescript::TypeScriptPlugin::default(),
            )));
            black_box(manager)
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Upfront Loading (All 13 Plugins)
// ============================================================================

/// Measures time to load all 13 built-in plugins upfront
///
/// This is the production-like loading strategy used in tests via
/// `with_builtin_plugins()` from the test helper module.
fn bench_upfront_loading_all_plugins(c: &mut Criterion) {
    c.bench_function("plugin_load_all_13_upfront", |b| {
        b.iter(|| {
            let mut manager = black_box(PluginManager::new());

            // Tier 1: Core languages (alphabetical order)
            manager.register_builtin(black_box(Box::new(sqry_lang_go::GoPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_java::JavaPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_swift::SwiftPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_typescript::TypeScriptPlugin::default(),
            )));

            // Tier 2: Additional languages (alphabetical order)
            manager.register_builtin(black_box(Box::new(
                sqry_lang_kotlin::KotlinPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_lua::LuaPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_php::PhpPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_r::RPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_ruby::RubyPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_scala::ScalaPlugin::default())));

            black_box(manager)
        });
    });
}

// ============================================================================
// Benchmark: Incremental Loading (Scaling Analysis)
// ============================================================================

/// Measures how plugin loading time scales with plugin count
///
/// Tests loading 1, 3, 7, and 13 plugins to understand scaling characteristics.
fn bench_incremental_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("plugin_load_incremental");

    // 1 plugin
    group.bench_function(BenchmarkId::from_parameter("1_plugin"), |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            black_box(manager)
        });
    });

    // 3 plugins
    group.bench_function(BenchmarkId::from_parameter("3_plugins"), |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            black_box(manager)
        });
    });

    // 7 plugins (all Tier 1)
    group.bench_function(BenchmarkId::from_parameter("7_plugins_tier1"), |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            manager.register_builtin(black_box(Box::new(sqry_lang_go::GoPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_java::JavaPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_swift::SwiftPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_typescript::TypeScriptPlugin::default(),
            )));
            black_box(manager)
        });
    });

    // 13 plugins (all)
    group.bench_function(BenchmarkId::from_parameter("13_plugins_all"), |b| {
        b.iter(|| {
            let mut manager = PluginManager::new();
            // Tier 1
            manager.register_builtin(black_box(Box::new(sqry_lang_go::GoPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_java::JavaPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_swift::SwiftPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_typescript::TypeScriptPlugin::default(),
            )));
            // Tier 2
            manager.register_builtin(black_box(Box::new(
                sqry_lang_kotlin::KotlinPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_lua::LuaPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_php::PhpPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_r::RPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_ruby::RubyPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_scala::ScalaPlugin::default())));
            black_box(manager)
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Cold-Start Plugin Loading (Tree-sitter Initialization)
// ============================================================================

/// Measures cold-start loading time for a single plugin
///
/// This benchmark measures the FULL initialization overhead including:
/// - `PluginManager` creation
/// - Plugin registration
/// - Tree-sitter Parser creation
/// - Language grammar loading via `set_language()`
/// - First parse (triggers actual grammar loading)
///
/// Expected timing: ~microseconds (slower than registration due to parsing overhead)
fn bench_cold_start_single_plugin(c: &mut Criterion) {
    const SAMPLE_CODE: &[u8] = b"fn main() { println!(\"Hello\"); }";

    c.bench_function("cold_start_single_rust", |b| {
        b.iter(|| {
            // Include PluginManager registration
            let mut manager = black_box(PluginManager::new());
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));

            // Get plugin and perform cold-start parse
            let plugin = manager.plugin_for_extension("rs").unwrap();
            let mut parser = black_box(Parser::new());
            let language = black_box(plugin.language());
            parser.set_language(&language).unwrap();
            // Actual parse triggers grammar loading
            let tree = parser.parse(SAMPLE_CODE, None);
            black_box((manager, tree))
        });
    });
}

/// Measures cold-start loading time for each Tier 1 plugin individually
///
/// This helps identify if any specific plugin has unusually high
/// tree-sitter initialization overhead. Includes actual parsing to
/// trigger grammar loading.
fn bench_cold_start_individual_plugins(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_start_individual");

    group.bench_function("go", |b| {
        const GO_CODE: &[u8] = b"package main\nfunc main() {}";
        b.iter(|| {
            let plugin = sqry_lang_go::GoPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(GO_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("java", |b| {
        const JAVA_CODE: &[u8] = b"class Main { public static void main(String[] args) {} }";
        b.iter(|| {
            let plugin = sqry_lang_java::JavaPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(JAVA_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("javascript", |b| {
        const JS_CODE: &[u8] = b"function main() { console.log('hello'); }";
        b.iter(|| {
            let plugin = sqry_lang_javascript::JavaScriptPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(JS_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("python", |b| {
        const PY_CODE: &[u8] = b"def main():\n    print('hello')";
        b.iter(|| {
            let plugin = sqry_lang_python::PythonPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(PY_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("rust", |b| {
        const RUST_CODE: &[u8] = b"fn main() { println!(\"hello\"); }";
        b.iter(|| {
            let plugin = sqry_lang_rust::RustPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(RUST_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("swift", |b| {
        const SWIFT_CODE: &[u8] = b"func main() { print(\"hello\") }";
        b.iter(|| {
            let plugin = sqry_lang_swift::SwiftPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(SWIFT_CODE, None);
            black_box(tree)
        });
    });

    group.bench_function("typescript", |b| {
        const TS_CODE: &[u8] = b"function main(): void { console.log('hello'); }";
        b.iter(|| {
            let plugin = sqry_lang_typescript::TypeScriptPlugin::default();
            let mut parser = Parser::new();
            let language = plugin.language();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(TS_CODE, None);
            black_box(tree)
        });
    });

    group.finish();
}

/// Measures cold-start loading for all 13 plugins
///
/// This represents the realistic startup cost when loading all built-in
/// plugins from scratch, including:
/// - `PluginManager` creation and all 13 plugin registrations
/// - Full tree-sitter initialization for each plugin
/// - Per-language appropriate sample code parsing
///
/// Expected timing: ~hundreds of microseconds (13 plugins × per-plugin overhead)
fn bench_cold_start_all_13_plugins(c: &mut Criterion) {
    // Per-language sample code
    let samples: Vec<(&str, &[u8])> = vec![
        ("go", b"package main\nfunc main() {}"),
        (
            "java",
            b"class Main { public static void main(String[] args) {} }",
        ),
        ("js", b"function main() { console.log('hello'); }"),
        ("py", b"def main():\n    print('hello')"),
        ("rs", b"fn main() { println!(\"hello\"); }"),
        ("swift", b"func main() { print(\"hello\") }"),
        ("ts", b"function main(): void { console.log('hello'); }"),
        ("kt", b"fun main() { println(\"hello\") }"),
        ("lua", b"function main() print('hello') end"),
        ("php", b"<?php function main() { echo 'hello'; } ?>"),
        ("r", b"main <- function() { print('hello') }"),
        ("rb", b"def main\n  puts 'hello'\nend"),
        (
            "scala",
            b"object Main { def main(args: Array[String]): Unit = println(\"hello\") }",
        ),
    ];

    c.bench_function("cold_start_all_13", |b| {
        b.iter(|| {
            // Step 1: Create PluginManager and register all 13 plugins
            let mut manager = black_box(PluginManager::new());

            // Tier 1 plugins
            manager.register_builtin(black_box(Box::new(sqry_lang_go::GoPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_java::JavaPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_javascript::JavaScriptPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_python::PythonPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_rust::RustPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_swift::SwiftPlugin::default())));
            manager.register_builtin(black_box(Box::new(
                sqry_lang_typescript::TypeScriptPlugin::default(),
            )));

            // Tier 2 plugins
            manager.register_builtin(black_box(Box::new(
                sqry_lang_kotlin::KotlinPlugin::default(),
            )));
            manager.register_builtin(black_box(Box::new(sqry_lang_lua::LuaPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_php::PhpPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_r::RPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_ruby::RubyPlugin::default())));
            manager.register_builtin(black_box(Box::new(sqry_lang_scala::ScalaPlugin::default())));

            // Step 2: Initialize each plugin's parser with language-appropriate code
            for (ext, code) in black_box(&samples) {
                if let Some(plugin) = manager.plugin_for_extension(ext) {
                    let mut parser = Parser::new();
                    let language = plugin.language();
                    parser.set_language(&language).unwrap();
                    // Trigger grammar loading with actual parse
                    let _ = parser.parse(code, None);
                }
            }

            black_box(manager)
        });
    });
}

// ============================================================================
// Benchmark: Plugin Lookup Performance
// ============================================================================

/// Measures plugin lookup performance after loading
///
/// This tests the overhead of `plugin_for_extension()` calls with varying
/// numbers of registered plugins.
fn bench_plugin_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("plugin_lookup");

    // Lookup with 1 plugin registered
    group.bench_function("lookup_1_plugin", |b| {
        b.iter_batched(
            || {
                let mut manager = PluginManager::new();
                manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
                manager
            },
            |manager| {
                let result = manager.plugin_for_extension("rs").is_some();
                black_box(result)
            },
            BatchSize::SmallInput,
        );
    });

    // Lookup with 7 plugins registered (Tier 1)
    group.bench_function("lookup_7_plugins", |b| {
        b.iter_batched(
            || {
                let mut manager = PluginManager::new();
                manager.register_builtin(Box::new(sqry_lang_go::GoPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_java::JavaPlugin::default()));
                manager
                    .register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_swift::SwiftPlugin::default()));
                manager
                    .register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));
                manager
            },
            |manager| {
                let result = manager.plugin_for_extension("rs").is_some();
                black_box(result)
            },
            BatchSize::SmallInput,
        );
    });

    // Lookup with all 13 plugins registered
    group.bench_function("lookup_13_plugins", |b| {
        b.iter_batched(
            || {
                let mut manager = PluginManager::new();
                // Tier 1
                manager.register_builtin(Box::new(sqry_lang_go::GoPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_java::JavaPlugin::default()));
                manager
                    .register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_swift::SwiftPlugin::default()));
                manager
                    .register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::default()));
                // Tier 2
                manager.register_builtin(Box::new(sqry_lang_kotlin::KotlinPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_lua::LuaPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_php::PhpPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_r::RPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_ruby::RubyPlugin::default()));
                manager.register_builtin(Box::new(sqry_lang_scala::ScalaPlugin::default()));
                manager
            },
            |manager| {
                let result = manager.plugin_for_extension("rs").is_some();
                black_box(result)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

criterion_group!(
    benches,
    // In-Memory Registration Benchmarks (fast, ~nanoseconds)
    bench_empty_manager,
    bench_single_plugin_registration,
    bench_individual_plugins,
    bench_upfront_loading_all_plugins,
    bench_incremental_loading,
    bench_plugin_lookup,
    // Cold-Start Loading Benchmarks (slow, ~milliseconds)
    bench_cold_start_single_plugin,
    bench_cold_start_individual_plugins,
    bench_cold_start_all_13_plugins,
);

criterion_main!(benches);
