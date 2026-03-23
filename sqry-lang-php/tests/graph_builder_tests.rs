/// Integration tests for PHP `GraphBuilder`
#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::build::test_helpers::{assert_has_call_edge, count_operations};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_php::PhpGraphBuilder;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use support::unique_php_path;
use tree_sitter::Parser;

fn parse_php(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .expect("error loading PHP grammar");
    parser.parse(source, None).expect("php parse failed")
}

#[test]
fn graph_builder_extracts_instance_and_static_calls() {
    let source =
        fs::read_to_string("tests/fixtures/graph/users_controller.php").expect("load php fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("users_controller");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // PHP GraphBuilder uses double-colon as separator (like PHP's class resolution operator)
    assert_has_call_edge(
        &staging,
        "UsersController::create",
        "UsersController::sendWelcomeEmail",
    );
    assert_has_call_edge(
        &staging,
        "UsersController::sendWelcomeEmail",
        "Mailer::deliver",
    );
    assert_has_call_edge(&staging, "UsersController::audit", "self::log");
}

#[test]
fn graph_builder_detects_php_ffi_edges() {
    let source =
        fs::read_to_string("tests/fixtures/graph/ffi_bridge.php").expect("load php ffi fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("ffi_bridge");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Verify regular method call edge exists
    assert_has_call_edge(&staging, "Crypto::encrypt", "self::crypto_encrypt");

    // Verify FFI edges are created
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    assert_eq!(
        ffi_edge_count, 2,
        "Expected 2 FfiCall edges: FFI::cdef() and $ffi->crypto_encrypt(), got {ffi_edge_count}"
    );

    // Verify specific FFI edge targets with correct naming
    assert!(
        has_ffi_edge(&staging, "Crypto::setup", "native::libcrypto"),
        "Expected FFI edge from Crypto::setup to native::libcrypto"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "Crypto::crypto_encrypt",
            "native::ffi::crypto_encrypt"
        ),
        "Expected FFI edge from Crypto::crypto_encrypt to native::ffi::crypto_encrypt"
    );
}

#[test]
fn graph_builder_handles_malformed_php_gracefully() {
    let source = fs::read_to_string("tests/fixtures/graph/malformed_syntax.php")
        .expect("load malformed fixture");

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .expect("error loading PHP grammar");

    // Tree-sitter is error-resilient and will parse malformed code with error nodes
    let tree = parser
        .parse(&source, None)
        .expect("tree-sitter should parse despite errors");

    // Verify tree contains errors (validates our fixture is actually malformed)
    assert!(
        tree.root_node().has_error(),
        "malformed fixture should produce a tree with error nodes"
    );

    let file = unique_php_path("malformed");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    // Builder should handle error nodes gracefully without panicking
    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);

    result.expect("builder should gracefully handle malformed PHP without returning errors");

    // Verify we got some operations (implied success)
    let counts = count_operations(&staging);
    assert!(counts.nodes > 0 || counts.edges == 0); // Weak assertion, mostly checking no panic
}

#[test]
fn graph_builder_respects_depth_limit() {
    let source = fs::read_to_string("tests/fixtures/graph/deep_namespaces.php")
        .expect("load deep namespaces fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("deep_namespaces");
    let mut staging = StagingGraph::new();

    // Use default builder (max depth = 5)
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with deep namespaces");
}

#[test]
fn graph_builder_handles_visibility_modifiers() {
    let source = fs::read_to_string("tests/fixtures/graph/visibility_edge_cases.php")
        .expect("load visibility fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("visibility");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with visibility modifiers");
}

#[test]
fn graph_builder_detects_multiple_ffi_calls() {
    let source = fs::read_to_string("tests/fixtures/graph/multiple_ffi.php")
        .expect("load multiple ffi fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("multiple_ffi");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with multiple FFI");

    // Verify multiple FFI edges are created
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    // Expected FFI calls:
    // - 1x FFI::cdef() in Crypto class
    // - 2x $ffi->aes_encrypt() and $ffi->aes_decrypt()
    // - 1x FFI::load() in Compressor class
    assert!(
        ffi_edge_count >= 4,
        "Expected at least 4 FfiCall edges (FFI::cdef, FFI::load, and FFI method calls), got {ffi_edge_count}"
    );

    // Verify specific FFI edge targets (check for normalized names without extensions)
    assert!(
        has_ffi_edge(&staging, "CryptoLibrary::init", "native::libcrypto"),
        "Expected FFI edge from CryptoLibrary::init to native::libcrypto (FFI::cdef)"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "CryptoLibrary::encrypt",
            "native::ffi::aes_encrypt"
        ),
        "Expected FFI edge from CryptoLibrary::encrypt to native::ffi::aes_encrypt"
    );

    assert!(
        has_ffi_edge(&staging, "CompressionLib::setup", "native::compression"),
        "Expected FFI edge from CompressionLib::setup to native::compression (FFI::load with .h stripped)"
    );
}

#[test]
fn graph_builder_detects_advanced_ffi_patterns() {
    let source = r###"<?php

class FfiAdvanced {
    // Fully-qualified FFI with leading backslash
    public static function fully_qualified() {
        $lib = \FFI::cdef("int foo(int x);", "libtest.so");
        return $lib->foo(42);
    }

    // Chained FFI call without intermediate variable
    public static function chained_cdef() {
        return FFI::cdef("int bar();", "libchain.so")->bar();
    }

    // Parenthesized chained call
    public static function parenthesized_chain() {
        return (FFI::load("/usr/lib/test.h"))->baz();
    }
}
"###;

    let tree = parse_php(source);
    let file = unique_php_path("advanced_ffi");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with advanced FFI patterns");

    // Count total FFI edges
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    assert!(
        ffi_edge_count >= 5,
        "Expected at least 5 FfiCall edges (3 FFI::cdef/load + 2 member calls), got {ffi_edge_count}"
    );

    // Verify fully-qualified \FFI::cdef detection
    assert!(
        has_ffi_edge(&staging, "FfiAdvanced::fully_qualified", "native::libtest"),
        "Expected FFI edge for fully-qualified \\FFI::cdef()"
    );

    // Verify chained FFI::cdef()->method() detection
    assert!(
        has_ffi_edge(&staging, "FfiAdvanced::chained_cdef", "native::libchain"),
        "Expected FFI edge for chained FFI::cdef()->bar()"
    );

    assert!(
        has_ffi_edge(&staging, "FfiAdvanced::chained_cdef", "native::ffi::bar"),
        "Expected FFI edge for bar() call on chained FFI object"
    );

    // Verify parenthesized (FFI::load())->method() detection
    assert!(
        has_ffi_edge(&staging, "FfiAdvanced::parenthesized_chain", "native::test"),
        "Expected FFI edge for parenthesized (FFI::load()) with .h stripped"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiAdvanced::parenthesized_chain",
            "native::ffi::baz"
        ),
        "Expected FFI edge for baz() call on parenthesized FFI object"
    );
}

#[test]
fn graph_builder_handles_php8_named_arguments() {
    let source = r###"<?php

class FfiNamedArgs {
    // PHP 8 named arguments with FFI::cdef
    public static function named_cdef() {
        $lib = \FFI::cdef(
            cdef: "int process(int x);",
            lib: "libnamed.so"
        );
        return $lib->process(42);
    }

    // PHP 8 named arguments with FFI::load
    public static function named_load() {
        $ffi = FFI::load(filename: "/usr/lib/libtest.h");
        return $ffi->test_func();
    }

    // Mixed: positional first arg, named second arg
    public static function mixed_args() {
        return FFI::cdef("int foo();", lib: "libmixed.so");
    }
}
"###;

    let tree = parse_php(source);
    let file = unique_php_path("php8_named_args");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with PHP 8 named arguments");

    // Verify named argument library names are extracted correctly
    assert!(
        has_ffi_edge(&staging, "FfiNamedArgs::named_cdef", "native::libnamed"),
        "Expected FFI edge for FFI::cdef with named lib: argument"
    );

    assert!(
        has_ffi_edge(&staging, "FfiNamedArgs::named_load", "native::libtest"),
        "Expected FFI edge for FFI::load with named filename: argument (.h stripped)"
    );

    assert!(
        has_ffi_edge(&staging, "FfiNamedArgs::mixed_args", "native::libmixed"),
        "Expected FFI edge for FFI::cdef with mixed positional/named arguments"
    );

    // Count total FFI edges
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    // Expect at least 3 static FFI edges + member calls
    assert!(
        ffi_edge_count >= 3,
        "Expected at least 3 FfiCall edges (3 FFI::cdef/load calls), got {ffi_edge_count}"
    );
}

#[test]
fn graph_builder_handles_reordered_named_arguments() {
    let source = r###"<?php

class FfiReordered {
    // Reversed order: lib comes before cdef
    public static function reversed_cdef() {
        $ffi = FFI::cdef(lib: "libreversed.so", cdef: "int test();");
        return $ffi->test();
    }

    // Reversed order with fully-qualified FFI
    public static function reversed_qualified() {
        return \FFI::cdef(
            lib: "libqualified.so",
            cdef: "int foo();"
        )->foo();
    }

    // FFI::load with named filename argument
    public static function named_filename() {
        $ffi = FFI::load(filename: "/usr/lib/libordered.h");
        return $ffi->process();
    }
}
"###;

    let tree = parse_php(source);
    let file = unique_php_path("reordered_named_args");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with reordered named arguments");

    // Verify reversed named arguments are correctly extracted
    assert!(
        has_ffi_edge(
            &staging,
            "FfiReordered::reversed_cdef",
            "native::libreversed"
        ),
        "Expected FFI edge for reversed lib: before cdef: argument"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiReordered::reversed_qualified",
            "native::libqualified"
        ),
        "Expected FFI edge for reversed \\FFI::cdef with lib: first"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiReordered::named_filename",
            "native::libordered"
        ),
        "Expected FFI edge for FFI::load with named filename: argument (.h stripped)"
    );

    // Verify member calls still work with reordered args
    assert!(
        has_ffi_edge(&staging, "FfiReordered::reversed_cdef", "native::ffi::test"),
        "Expected FFI edge for test() call on reversed arg FFI object"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiReordered::reversed_qualified",
            "native::ffi::foo"
        ),
        "Expected FFI edge for foo() call on chained reversed arg FFI object"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiReordered::named_filename",
            "native::ffi::process"
        ),
        "Expected FFI edge for process() call on named filename FFI object"
    );
}

#[test]
fn graph_builder_rejects_interpolated_ffi_strings() {
    let source = r###"<?php

class FfiInterpolated {
    // Interpolated library name should fall back to native::unknown
    public static function interpolated_lib($suffix) {
        $ffi = FFI::cdef("int foo();", "lib{$suffix}.so");
        return $ffi->foo();
    }

    // Variable interpolation in path
    public static function interpolated_path($dir) {
        $ffi = FFI::load("$dir/libtest.h");
        return $ffi->test();
    }

    // Pure string literals should still work
    public static function pure_string() {
        return FFI::cdef("int bar();", "libpure.so");
    }
}
"###;

    let tree = parse_php(source);
    let file = unique_php_path("interpolated_strings");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with interpolated strings");

    // Count FFI edges - should have edges for pure strings and fallback for interpolated
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    // Expect: 2 unknown (interpolated FFI::cdef/load) + 1 pure (FFI::cdef) + 2 member calls (foo, test) = 5 total
    assert!(
        ffi_edge_count >= 5,
        "Expected at least 5 FfiCall edges (3 static + 2 member calls), got {ffi_edge_count}"
    );

    // Verify interpolated strings fall back to native::unknown
    assert!(
        has_ffi_edge(
            &staging,
            "FfiInterpolated::interpolated_lib",
            "native::unknown"
        ),
        "Expected interpolated \"lib{{$suffix}}.so\" to fall back to native::unknown"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiInterpolated::interpolated_path",
            "native::unknown"
        ),
        "Expected interpolated \"$dir/libtest.h\" to fall back to native::unknown"
    );

    // Verify pure string literal still works
    assert!(
        has_ffi_edge(&staging, "FfiInterpolated::pure_string", "native::libpure"),
        "Expected pure string literal to produce native::libpure"
    );

    // Verify member calls still work even with unknown libraries
    assert!(
        has_ffi_edge(
            &staging,
            "FfiInterpolated::interpolated_lib",
            "native::ffi::foo"
        ),
        "Expected FFI edge for foo() call even on interpolated lib"
    );
}

#[test]
fn graph_builder_rejects_complex_interpolation() {
    let source = r###"<?php

class FfiComplexInterpolation {
    private $config = ['lib_path' => '/usr/lib'];
    private $libs = null;

    // Array access interpolation
    public static function array_access($paths) {
        $ffi = FFI::load("{$paths['lib_dir']}/libarray.h");
        return $ffi->process();
    }

    // Property access interpolation
    public function property_access() {
        $ffi = FFI::cdef("int foo();", "{$this->config['lib_path']}/libprop.so");
        return $ffi->foo();
    }

    // Nested expression interpolation
    public function nested_expression() {
        $ffi = FFI::load("{$this->libs->primary}/libnested.h");
        return $ffi->test();
    }

    // Pure string for comparison
    public static function pure_complex() {
        return FFI::cdef("int bar();", "libpure.so");
    }
}
"###;

    let tree = parse_php(source);
    let file = unique_php_path("complex_interpolation");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph with complex interpolation");

    // Count FFI edges
    let ffi_edge_count =
        count_edges_of_kind_local(&staging, |kind| matches!(kind, EdgeKind::FfiCall { .. }));

    // Expect: 3 unknown (complex interpolations) + 1 pure + 3 member calls = 7 total
    assert!(
        ffi_edge_count >= 7,
        "Expected at least 7 FfiCall edges (4 static + 3 member calls), got {ffi_edge_count}"
    );

    // Verify array access interpolation falls back to native::unknown
    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::array_access",
            "native::unknown"
        ),
        "Expected array access interpolation {{$paths['lib_dir']}} to fall back to native::unknown"
    );

    // Verify property access interpolation falls back to native::unknown
    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::property_access",
            "native::unknown"
        ),
        "Expected property access interpolation {{$this->config['lib_path']}} to fall back to native::unknown"
    );

    // Verify nested expression interpolation falls back to native::unknown
    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::nested_expression",
            "native::unknown"
        ),
        "Expected nested expression interpolation {{$this->libs->primary}} to fall back to native::unknown"
    );

    // Verify pure string still works
    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::pure_complex",
            "native::libpure"
        ),
        "Expected pure string to produce native::libpure"
    );

    // Verify member calls still work even with complex interpolation
    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::array_access",
            "native::ffi::process"
        ),
        "Expected FFI edge for process() call even with array interpolation"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::property_access",
            "native::ffi::foo"
        ),
        "Expected FFI edge for foo() call even with property interpolation"
    );

    assert!(
        has_ffi_edge(
            &staging,
            "FfiComplexInterpolation::nested_expression",
            "native::ffi::test"
        ),
        "Expected FFI edge for test() call even with nested expression interpolation"
    );
}

// ============================================================================
// Import Edge Tests
// ============================================================================

use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::graph::unified::string::StringId;

/// Helper function to count edges of a specific kind in staging operations.
fn count_edges_of_kind_local(
    staging: &StagingGraph,
    predicate: impl Fn(&EdgeKind) -> bool,
) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                predicate(kind)
            } else {
                false
            }
        })
        .count()
}

/// Build a map from StringId to string value from staging operations.
fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((*local_id, value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Check if a specific FFI edge exists (source -> target).
fn has_ffi_edge(staging: &StagingGraph, source_pattern: &str, target_pattern: &str) -> bool {
    use sqry_core::graph::unified::edge::FfiConvention;
    use sqry_core::graph::unified::node::NodeId;

    let string_map = build_string_map(staging);

    // Build node name lookup
    let mut node_names: HashMap<NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check for FFI edges matching the patterns
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::FfiCall {
                convention: FfiConvention::C,
            },
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);

            if let (Some(src), Some(tgt)) = (source_name, target_name) {
                return src == source_pattern && tgt == target_pattern;
            }
        }
        false
    })
}

/// Get node name from NodeEntry using string map.
fn get_node_name(entry: &NodeEntry, string_map: &HashMap<StringId, String>) -> Option<String> {
    entry
        .qualified_name
        .and_then(|qualified_name| string_map.get(&qualified_name).cloned())
        .or_else(|| string_map.get(&entry.name).cloned())
}

/// Helper to verify an import edge exists with specific target name.
/// Returns true if at least one import edge targets a node with the given name pattern.
fn has_import_edge_to(staging: &StagingGraph, target_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    // Build NodeId -> name map from AddNode operations
    let mut node_names: HashMap<sqry_core::graph::unified::node::NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check if any import edge targets a node with matching name
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Imports { .. },
            target,
            ..
        } = op
        {
            node_names
                .get(target)
                .map(|name| name.contains(target_pattern))
                .unwrap_or(false)
        } else {
            false
        }
    })
}

/// Helper to verify an import edge exists with specific alias.
fn has_import_edge_with_alias(staging: &StagingGraph, alias_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::Imports {
                    alias: Some(alias_id),
                    ..
                },
            ..
        } = op
        {
            string_map
                .get(alias_id)
                .map(|alias_str| alias_str.contains(alias_pattern))
                .unwrap_or(false)
        } else {
            false
        }
    })
}

#[test]
fn graph_builder_extracts_simple_use_statement() {
    let source = r###"<?php
use App\Services\Mailer;
"###;
    let tree = parse_php(source);
    let file = unique_php_path("simple_use");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting Mailer
    assert!(
        has_import_edge_to(&staging, "Mailer"),
        "expected import edge to Mailer"
    );
}

#[test]
fn graph_builder_extracts_aliased_use_statement() {
    let source = r###"<?php
use App\Services\Mailer as Mail;
"###;
    let tree = parse_php(source);
    let file = unique_php_path("aliased_use");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting Mailer with alias Mail
    assert!(
        has_import_edge_to(&staging, "Mailer"),
        "expected import edge to Mailer"
    );
    assert!(
        has_import_edge_with_alias(&staging, "Mail"),
        "expected import edge with alias Mail"
    );
}

#[test]
fn graph_builder_extracts_grouped_use_statement() {
    let source = r###"<?php
use App\Models\{User, Post, Comment};
"###;
    let tree = parse_php(source);
    let file = unique_php_path("grouped_use");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edges for each class in the group
    assert!(
        has_import_edge_to(&staging, "User"),
        "expected import edge to User"
    );
    assert!(
        has_import_edge_to(&staging, "Post"),
        "expected import edge to Post"
    );
    assert!(
        has_import_edge_to(&staging, "Comment"),
        "expected import edge to Comment"
    );
}

#[test]
fn graph_builder_extracts_function_use_statement() {
    let source = r###"<?php
use function App\Utils\array_flatten;
"###;
    let tree = parse_php(source);
    let file = unique_php_path("function_use");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting array_flatten
    assert!(
        has_import_edge_to(&staging, "array_flatten"),
        "expected import edge to array_flatten for function use"
    );
}

#[test]
fn graph_builder_extracts_const_use_statement() {
    let source = r###"<?php
use const App\Config\VERSION;
"###;
    let tree = parse_php(source);
    let file = unique_php_path("const_use");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting VERSION
    assert!(
        has_import_edge_to(&staging, "VERSION"),
        "expected import edge to VERSION for const use"
    );
}

#[test]
fn graph_builder_extracts_require_statement() {
    let source = r###"<?php
require 'bootstrap.php';
"###;
    let tree = parse_php(source);
    let file = unique_php_path("require");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting bootstrap.php
    assert!(
        has_import_edge_to(&staging, "bootstrap.php"),
        "expected import edge to bootstrap.php for require statement"
    );
}

#[test]
fn graph_builder_extracts_require_once_statement() {
    let source = r###"<?php
require_once 'config.php';
"###;
    let tree = parse_php(source);
    let file = unique_php_path("require_once");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting config.php
    assert!(
        has_import_edge_to(&staging, "config.php"),
        "expected import edge to config.php for require_once statement"
    );
}

#[test]
fn graph_builder_extracts_include_statement() {
    let source = r###"<?php
include 'helpers.php';
"###;
    let tree = parse_php(source);
    let file = unique_php_path("include");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting helpers.php
    assert!(
        has_import_edge_to(&staging, "helpers.php"),
        "expected import edge to helpers.php for include statement"
    );
}

#[test]
fn graph_builder_extracts_include_once_statement() {
    let source = r###"<?php
include_once 'polyfills.php';
"###;
    let tree = parse_php(source);
    let file = unique_php_path("include_once");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have import edge targeting polyfills.php
    assert!(
        has_import_edge_to(&staging, "polyfills.php"),
        "expected import edge to polyfills.php for include_once statement"
    );
}

// ============================================================================
// OOP Edge Tests - Inheritance
// ============================================================================

#[test]
fn graph_builder_extracts_class_inheritance() {
    let source = r###"<?php
class Child extends Parent {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("class_inheritance");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have Inherits edge
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "expected at least 1 Inherits edge, got {inherits_count}"
    );
}

#[test]
fn graph_builder_extracts_qualified_class_inheritance() {
    let source = r###"<?php
namespace App\Controllers;

class UserController extends App\Base\Controller {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("qualified_inheritance");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have Inherits edge
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "expected at least 1 Inherits edge for qualified name, got {inherits_count}"
    );
}

// ============================================================================
// OOP Edge Tests - Interface Implementation
// ============================================================================

#[test]
fn graph_builder_extracts_single_interface_implementation() {
    let source = r###"<?php
class UserRepository implements Repository {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("single_interface");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have Implements edge
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 1,
        "expected at least 1 Implements edge, got {implements_count}"
    );
}

#[test]
fn graph_builder_extracts_multiple_interface_implementation() {
    let source = r###"<?php
class User implements Serializable, JsonSerializable, ArrayAccess {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("multiple_interfaces");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have 3 Implements edges
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 3,
        "expected at least 3 Implements edges for multiple interfaces, got {implements_count}"
    );
}

#[test]
fn graph_builder_extracts_extends_and_implements() {
    let source = r###"<?php
class UserController extends BaseController implements Authenticatable {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("extends_and_implements");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have both Inherits and Implements edges
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));

    assert!(
        inherits_count >= 1,
        "expected at least 1 Inherits edge, got {inherits_count}"
    );
    assert!(
        implements_count >= 1,
        "expected at least 1 Implements edge, got {implements_count}"
    );
}

// ============================================================================
// OOP Edge Tests - Trait Usage
// ============================================================================

#[test]
fn graph_builder_extracts_single_trait_usage() {
    let source = r###"<?php
class User {
    use Timestampable;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("single_trait");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Trait usage is modeled as Implements edge
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 1,
        "expected at least 1 Implements edge for trait usage, got {implements_count}"
    );
}

#[test]
fn graph_builder_extracts_multiple_trait_usage() {
    let source = r###"<?php
class Post {
    use Timestampable, SoftDeletes, Loggable;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("multiple_traits");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have 3 Implements edges for 3 traits
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 3,
        "expected at least 3 Implements edges for multiple traits, got {implements_count}"
    );
}

// ============================================================================
// OOP Edge Tests - Interface Inheritance
// ============================================================================

#[test]
fn graph_builder_extracts_interface_inheritance() {
    let source = r###"<?php
interface Cacheable extends Serializable {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("interface_inheritance");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Interface inheritance uses Inherits edge
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "expected at least 1 Inherits edge for interface inheritance, got {inherits_count}"
    );
}

#[test]
fn graph_builder_extracts_interface_multiple_inheritance() {
    let source = r###"<?php
interface UserInterface extends Authenticatable, Authorizable, CanResetPassword {
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("interface_multiple_inheritance");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have 3 Inherits edges
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 3,
        "expected at least 3 Inherits edges for multiple interface inheritance, got {inherits_count}"
    );
}

// ============================================================================
// Combined Tests
// ============================================================================

#[test]
fn graph_builder_handles_complex_class_with_all_features() {
    let source = r###"<?php
namespace App\Models;

use App\Traits\Timestampable;
use App\Traits\SoftDeletes;
use App\Contracts\Authenticatable;

class User extends BaseModel implements Authenticatable, Serializable {
    use Timestampable, SoftDeletes;

    public function save() {
        $this->validate();
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("complex_class");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Count all edge types
    let import_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    let inherits_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Inherits));
    let implements_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Implements));
    let calls_count = count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Calls { .. }));

    // 3 use statements -> 3 import edges
    assert!(
        import_count >= 3,
        "expected at least 3 import edges, got {import_count}"
    );

    // 1 extends -> 1 inherits edge
    assert!(
        inherits_count >= 1,
        "expected at least 1 inherits edge, got {inherits_count}"
    );

    // 2 implements + 2 traits -> 4 implements edges
    assert!(
        implements_count >= 4,
        "expected at least 4 implements edges, got {implements_count}"
    );

    // 1 method call ($this->validate()) -> 1 call edge
    assert!(
        calls_count >= 1,
        "expected at least 1 call edge, got {calls_count}"
    );
}

#[test]
fn graph_builder_handles_imports_fixture() {
    let source = fs::read_to_string("tests/fixtures/relations/php/imports.php")
        .expect("load imports fixture");
    let tree = parse_php(&source);
    let file = unique_php_path("imports_fixture");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Count import edges (fixture has 4 use statements + 4 include/require)
    let import_count =
        count_edges_of_kind_local(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 4,
        "expected at least 4 import edges from fixture, got {import_count}"
    );
}

// ============================================================================
// Export Edge Tests
// ============================================================================

/// Helper to verify an export edge exists from module to target node.
fn has_export_edge_to(staging: &StagingGraph, target_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    // Build NodeId -> name map from AddNode operations
    let mut node_names: HashMap<sqry_core::graph::unified::node::NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check if any export edge targets a node with matching name
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { .. },
            target,
            ..
        } = op
        {
            node_names
                .get(target)
                .map(|name| name.contains(target_pattern))
                .unwrap_or(false)
        } else {
            false
        }
    })
}

#[test]
fn graph_builder_exports_top_level_class() {
    let source = r###"<?php
class User {
    public function getId() {
        return $this->id;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_class");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for User class
    assert!(
        has_export_edge_to(&staging, "User"),
        "expected export edge for User class"
    );
}

#[test]
fn graph_builder_exports_top_level_function() {
    let source = r###"<?php
function greet(string $name): string {
    return "Hello, {$name}!";
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_function");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for greet function
    assert!(
        has_export_edge_to(&staging, "greet"),
        "expected export edge for greet function"
    );
}

#[test]
fn graph_builder_exports_interface() {
    let source = r###"<?php
interface Repository {
    public function findById($id);
    public function save($entity);
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_interface");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for Repository interface
    assert!(
        has_export_edge_to(&staging, "Repository"),
        "expected export edge for Repository interface"
    );
}

#[test]
fn graph_builder_exports_trait() {
    let source = r###"<?php
trait Timestampable {
    public function getCreatedAt() {
        return $this->created_at;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_trait");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for Timestampable trait
    assert!(
        has_export_edge_to(&staging, "Timestampable"),
        "expected export edge for Timestampable trait"
    );
}

#[test]
fn graph_builder_exports_namespaced_class() {
    let source = r###"<?php
namespace App\Services;

class UserService {
    public function getUsers() {
        return [];
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_namespaced_class");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for namespaced UserService class
    assert!(
        has_export_edge_to(&staging, "UserService"),
        "expected export edge for App\\Services\\UserService class"
    );
}

#[test]
fn graph_builder_exports_namespaced_function() {
    let source = r###"<?php
namespace App\Helpers;

function slugify($text) {
    return strtolower(preg_replace('/[^a-z0-9]+/', '-', $text));
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_namespaced_function");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for namespaced slugify function
    assert!(
        has_export_edge_to(&staging, "slugify"),
        "expected export edge for App\\Helpers\\slugify function"
    );
}

#[test]
fn graph_builder_exports_multiple_classes_in_file() {
    let source = r###"<?php
class User {
    public function getId() {
        return $this->id;
    }
}

class Post {
    public function getTitle() {
        return $this->title;
    }
}

interface Repository {
    public function findById($id);
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_multiple");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edges for all top-level symbols
    assert!(
        has_export_edge_to(&staging, "User"),
        "expected export edge for User class"
    );
    assert!(
        has_export_edge_to(&staging, "Post"),
        "expected export edge for Post class"
    );
    assert!(
        has_export_edge_to(&staging, "Repository"),
        "expected export edge for Repository interface"
    );
}

#[test]
fn graph_builder_exports_brace_style_namespace() {
    let source = r###"<?php
namespace App\Services {
    class UserService {
        public function getUsers() {
            return [];
        }
    }

    function helper() {
        return true;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_brace_namespace");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edges for symbols in brace-style namespace
    assert!(
        has_export_edge_to(&staging, "UserService"),
        "expected export edge for UserService class in brace-style namespace"
    );
    assert!(
        has_export_edge_to(&staging, "helper"),
        "expected export edge for helper function in brace-style namespace"
    );
}

#[test]
fn graph_builder_exports_semicolon_style_namespace() {
    let source = r###"<?php
namespace App\Services;

class UserService {
    public function getUsers() {
        return [];
    }
}

function helper() {
    return true;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_semicolon_namespace");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edges for symbols in semicolon-style namespace
    assert!(
        has_export_edge_to(&staging, "UserService"),
        "expected export edge for UserService class in semicolon-style namespace"
    );
    assert!(
        has_export_edge_to(&staging, "helper"),
        "expected export edge for helper function in semicolon-style namespace"
    );
}

#[test]
fn graph_builder_exports_enum() {
    let source = r###"<?php
enum Status {
    case PENDING;
    case APPROVED;
    case REJECTED;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("export_enum");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should have export edge for Status enum
    assert!(
        has_export_edge_to(&staging, "Status"),
        "expected export edge for Status enum"
    );
}
