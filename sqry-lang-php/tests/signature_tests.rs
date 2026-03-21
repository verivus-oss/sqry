/// Tests for PHP signature metadata (return types and parameter types)
///
/// Task #17: Complete PHP signature metadata
/// This test suite validates that PHP functions and methods have their
/// return type annotations properly extracted and stored in signature metadata.
#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_php::PhpGraphBuilder;
use std::collections::HashMap;
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

/// Build a string lookup table from staging operations.
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

/// Find signature (return type) for a function/method by name.
/// The signature field contains the return type for PHP functions/methods.
fn find_return_type(staging: &StagingGraph, name_pattern: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Function | NodeKind::Method)
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name_pattern)) {
                return entry
                    .signature
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

// ============================================================================
// Function Return Type Tests
// ============================================================================

#[test]
fn test_function_with_string_return_type() {
    let source = r###"<?php
function greet(string $name): string {
    return "Hello, $name";
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_string");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "greet");
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected greet() to have string return type"
    );
}

#[test]
fn test_function_with_int_return_type() {
    let source = r###"<?php
function calculate(int $a, int $b): int {
    return $a + $b;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_int");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "calculate");
    assert_eq!(
        return_type,
        Some("int".to_string()),
        "Expected calculate() to have int return type"
    );
}

#[test]
fn test_function_with_float_return_type() {
    let source = r###"<?php
function divide(float $a, float $b): float {
    return $a / $b;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_float");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "divide");
    assert_eq!(
        return_type,
        Some("float".to_string()),
        "Expected divide() to have float return type"
    );
}

#[test]
fn test_function_with_bool_return_type() {
    let source = r###"<?php
function isValid(string $email): bool {
    return filter_var($email, FILTER_VALIDATE_EMAIL) !== false;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_bool");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "isValid");
    assert_eq!(
        return_type,
        Some("bool".to_string()),
        "Expected isValid() to have bool return type"
    );
}

#[test]
fn test_function_with_array_return_type() {
    let source = r###"<?php
function getItems(): array {
    return [];
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_array");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "getItems");
    assert_eq!(
        return_type,
        Some("array".to_string()),
        "Expected getItems() to have array return type"
    );
}

#[test]
fn test_function_with_class_return_type() {
    let source = r###"<?php
class User {}

function createUser(): User {
    return new User();
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_class");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "createUser");
    assert_eq!(
        return_type,
        Some("User".to_string()),
        "Expected createUser() to have User return type"
    );
}

#[test]
fn test_function_without_return_type() {
    let source = r###"<?php
function doSomething() {
    echo "Hello";
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_no_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "doSomething");
    assert_eq!(
        return_type, None,
        "Expected doSomething() to have no return type"
    );
}

// ============================================================================
// Nullable Return Type Tests (PHP 7.1+)
// ============================================================================

#[test]
fn test_function_with_nullable_string_return_type() {
    let source = r###"<?php
function findUser(int $id): ?string {
    return null;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_nullable_string");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "findUser");
    // Nullable types should be normalized to the base type (strip the ?)
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected findUser() to have string return type (nullable stripped)"
    );
}

#[test]
fn test_function_with_nullable_int_return_type() {
    let source = r###"<?php
function getCount(): ?int {
    return null;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_nullable_int");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "getCount");
    assert_eq!(
        return_type,
        Some("int".to_string()),
        "Expected getCount() to have int return type (nullable stripped)"
    );
}

// ============================================================================
// Union Return Type Tests (PHP 8.0+)
// ============================================================================

#[test]
fn test_function_with_union_return_type() {
    let source = r###"<?php
function process($value): string|int {
    return $value;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_union");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "process");
    // Union types should return the first type
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected process() to have string return type (first in union)"
    );
}

#[test]
fn test_function_with_nullable_union_return_type() {
    let source = r###"<?php
function getValue(): string|null {
    return null;
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("func_return_nullable_union");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "getValue");
    // Take first type in union
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected getValue() to have string return type (first in union)"
    );
}

// ============================================================================
// Method Return Type Tests
// ============================================================================

#[test]
fn test_method_with_return_type() {
    let source = r###"<?php
class User {
    public function getName(): string {
        return $this->name;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("method_return_type");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "getName");
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected getName() to have string return type"
    );
}

#[test]
fn test_method_with_nullable_return_type() {
    let source = r###"<?php
class User {
    public function getEmail(): ?string {
        return $this->email;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("method_return_nullable");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "getEmail");
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected getEmail() to have string return type (nullable stripped)"
    );
}

#[test]
fn test_method_without_return_type() {
    let source = r###"<?php
class Logger {
    public function log($message) {
        echo $message;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("method_no_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "log");
    assert_eq!(return_type, None, "Expected log() to have no return type");
}

#[test]
fn test_private_method_with_return_type() {
    let source = r###"<?php
class Calculator {
    private function add(int $a, int $b): int {
        return $a + $b;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("private_method_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "add");
    assert_eq!(
        return_type,
        Some("int".to_string()),
        "Expected add() to have int return type"
    );
}

#[test]
fn test_protected_method_with_return_type() {
    let source = r###"<?php
class Service {
    protected function process(array $data): bool {
        return true;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("protected_method_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "process");
    assert_eq!(
        return_type,
        Some("bool".to_string()),
        "Expected process() to have bool return type"
    );
}

#[test]
fn test_static_method_with_return_type() {
    let source = r###"<?php
class Config {
    public static function get(string $key): string {
        return "value";
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("static_method_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "get");
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected get() to have string return type"
    );
}

// ============================================================================
// Namespaced Function/Method Tests
// ============================================================================

#[test]
fn test_namespaced_function_with_return_type() {
    let source = r###"<?php
namespace App\Utils;

function slugify(string $text): string {
    return strtolower($text);
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("namespaced_func_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "slugify");
    assert_eq!(
        return_type,
        Some("string".to_string()),
        "Expected slugify() to have string return type"
    );
}

#[test]
fn test_namespaced_class_method_with_return_type() {
    let source = r###"<?php
namespace App\Services;

class UserService {
    public function findById(int $id): ?User {
        return null;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("namespaced_method_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let return_type = find_return_type(&staging, "findById");
    assert_eq!(
        return_type,
        Some("User".to_string()),
        "Expected findById() to have User return type (nullable stripped)"
    );
}

// ============================================================================
// Mixed Tests
// ============================================================================

#[test]
fn test_multiple_functions_with_different_return_types() {
    let source = r###"<?php
function getString(): string {
    return "hello";
}

function getInt(): int {
    return 42;
}

function getBool(): bool {
    return true;
}

function getNothing() {
    echo "nothing";
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("multiple_funcs_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(
        find_return_type(&staging, "getString"),
        Some("string".to_string())
    );
    assert_eq!(
        find_return_type(&staging, "getInt"),
        Some("int".to_string())
    );
    assert_eq!(
        find_return_type(&staging, "getBool"),
        Some("bool".to_string())
    );
    assert_eq!(find_return_type(&staging, "getNothing"), None);
}

#[test]
fn test_class_with_multiple_methods_with_return_types() {
    let source = r###"<?php
class Calculator {
    public function add(int $a, int $b): int {
        return $a + $b;
    }

    public function subtract(int $a, int $b): int {
        return $a - $b;
    }

    public function getName(): string {
        return "Calculator";
    }

    private function validate(): bool {
        return true;
    }
}
"###;
    let tree = parse_php(source);
    let file = unique_php_path("class_multiple_methods_return");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(find_return_type(&staging, "add"), Some("int".to_string()));
    assert_eq!(
        find_return_type(&staging, "subtract"),
        Some("int".to_string())
    );
    assert_eq!(
        find_return_type(&staging, "getName"),
        Some("string".to_string())
    );
    assert_eq!(
        find_return_type(&staging, "validate"),
        Some("bool".to_string())
    );
}
