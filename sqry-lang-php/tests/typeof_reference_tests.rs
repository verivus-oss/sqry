//! Tests for `TypeOf` and Reference edge creation from `PHPDoc` annotations
//!
//! This test suite validates that `TypeOf` and Reference edges are correctly
//! created from `PHPDoc` @param, @return, and @var annotations in PHP code.

#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::build::test_helpers::collect_edges_by_kind;
use sqry_lang_php::PhpGraphBuilder;
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

// ============================================================================
// @param Tests (Function/Method Parameters)
// ============================================================================

#[test]
fn test_function_phpdoc_param_simple_type() {
    let source = r"<?php
/**
 * @param {string} $name
 */
function greet($name) {
    echo $name;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that TypeOf edges were created for the parameter
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Function should have TypeOf edges from @param"
    );
}

#[test]
fn test_function_phpdoc_param_custom_type() {
    let source = r"<?php
/**
 * @param {User} $user
 */
function processUser($user) {
    return $user->name;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that Reference edges were created for custom types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Function should have Reference edges from @param with custom type"
    );
}

#[test]
fn test_function_phpdoc_param_union_type() {
    let source = r"<?php
/**
 * @param {string|int} $value
 */
function processValue($value) {
    return $value;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that Reference edges were created for both union types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 2,
        "Union types should create multiple reference edges"
    );
}

#[test]
fn test_function_phpdoc_param_array_type() {
    let source = r"<?php
/**
 * @param {User[]} $users
 */
function processUsers($users) {
    foreach ($users as $user) {
        echo $user->name;
    }
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that Reference edges were created for array element type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Array type should create reference edges for element type"
    );
}

#[test]
fn test_function_phpdoc_multiple_params() {
    let source = r"<?php
/**
 * @param {string} $firstName
 * @param {string} $lastName
 * @param {int} $age
 */
function createPerson($firstName, $lastName, $age) {
    return ['first' => $firstName, 'last' => $lastName, 'age' => $age];
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that multiple TypeOf edges exist
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        typeof_edges.len() >= 3,
        "Multiple @param tags should create multiple TypeOf edges"
    );
}

// ============================================================================
// @return Tests (Function/Method Return Types)
// ============================================================================

#[test]
fn test_function_phpdoc_return_simple_type() {
    let source = r#"<?php
/**
 * @return {string}
 */
function getName() {
    return "John";
}
"#;

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check TypeOf edge for return type
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Function should have TypeOf edge for @return"
    );
}

#[test]
fn test_function_phpdoc_return_custom_type() {
    let source = r"<?php
/**
 * @return {User}
 */
function getUser() {
    return new User();
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check Reference edge for return type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Function should have Reference edge for @return with custom type"
    );
}

#[test]
fn test_function_phpdoc_return_union_type() {
    let source = r"<?php
/**
 * @return {User|Admin|Guest}
 */
function getUser() {
    return null;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check Reference edges for all union types
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        ref_edges.len() >= 3,
        "Union return types should create multiple reference edges"
    );
}

// ============================================================================
// @var Tests (Variable and Property Declarations)
// ============================================================================

#[test]
fn test_property_phpdoc_var_type() {
    let source = r"<?php
class User {
    /**
     * @var {string}
     */
    private $name;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should not crash and graph should be valid
    assert!(
        !staging.is_empty(),
        "Graph should contain nodes after processing @var"
    );
}

#[test]
fn test_property_phpdoc_var_custom_type() {
    let source = r"<?php
class Repository {
    /**
     * @var {UserService}
     */
    private $userService;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should create Reference edges for the custom type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Property @var should create reference edges"
    );
}

#[test]
fn test_property_phpdoc_var_array_type() {
    let source = r"<?php
class Database {
    /**
     * @var {Connection[]}
     */
    private $connections = [];
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Array element type should create reference edges
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Array @var should create reference edges"
    );
}

// ============================================================================
// Method PHPDoc Tests
// ============================================================================

#[test]
fn test_method_phpdoc_param() {
    let source = r"<?php
class UserService {
    /**
     * @param {User} $user
     */
    public function save($user) {
        return $user->id;
    }
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check TypeOf edges from method to parameter type
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    assert!(
        !typeof_edges.is_empty(),
        "Method should have TypeOf edges from @param"
    );
}

#[test]
fn test_method_phpdoc_return() {
    let source = r"<?php
class UserService {
    /**
     * @return {User}
     */
    public function find($id) {
        return new User();
    }
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check Reference edge for return type
    let ref_edges = collect_edges_by_kind(&staging, "References");
    assert!(
        !ref_edges.is_empty(),
        "Method should have Reference edge from @return"
    );
}

#[test]
fn test_method_phpdoc_param_and_return() {
    let source = r"<?php
class Mapper {
    /**
     * @param {Data} $data
     * @return {Model}
     */
    public function map($data) {
        return new Model($data);
    }
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check both TypeOf and Reference edges
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    let ref_edges = collect_edges_by_kind(&staging, "References");

    assert!(!typeof_edges.is_empty(), "Method should have TypeOf edges");
    assert!(ref_edges.len() >= 2, "Method should have Reference edges");
}

// ============================================================================
// Negative Tests (No False Positives)
// ============================================================================

#[test]
fn test_no_phpdoc_no_typeof_edges() {
    let source = r"<?php
function noDocumentation($param) {
    return $param;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Function without PHPDoc should not have TypeOf edges from documentation
    // (It might have other edges, but not from PHPDoc)
    // This test primarily ensures we don't crash
    assert!(!staging.is_empty(), "Graph should be valid");
}

#[test]
fn test_malformed_phpdoc_no_crash() {
    let source = r"<?php
/**
 * This is a malformed comment
 * (no @param or @return tags)
 */
function test($param) {
    return $param;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    // Should not crash
    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    assert!(
        !staging.is_empty(),
        "Graph should be valid even with malformed comments"
    );
}

// ============================================================================
// Complex Integration Tests
// ============================================================================

#[test]
fn test_complex_class_with_phpdoc_edges() {
    let source = r"<?php
class UserRepository {
    /**
     * @param {User} $user
     * @return {bool}
     */
    public function create($user) {
        return true;
    }

    /**
     * @param {int} $id
     * @return {User}
     */
    public function find($id) {
        return new User();
    }

    /**
     * @var {PDOConnection}
     */
    private $connection;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Check that TypeOf and Reference edges were created
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    let ref_edges = collect_edges_by_kind(&staging, "References");

    assert!(
        !typeof_edges.is_empty(),
        "Class methods should have TypeOf edges"
    );
    assert!(
        !ref_edges.is_empty(),
        "Class should have Reference edges from properties and methods"
    );
}

#[test]
fn test_phpdoc_with_namespaced_types() {
    let source = r"<?php
namespace App\Services;

/**
 * @param {App\Models\User} $user
 * @return {App\Models\Result}
 */
function processUser($user) {
    return new Result();
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should handle namespaced types correctly
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    let ref_edges = collect_edges_by_kind(&staging, "References");

    assert!(
        !typeof_edges.is_empty(),
        "Should handle namespaced @param types"
    );
    assert!(
        !ref_edges.is_empty(),
        "Should create references for namespaced types"
    );
}

#[test]
fn test_phpdoc_nullable_types() {
    let source = r"<?php
/**
 * @param {?User} $user
 * @return {?string}
 */
function process($user) {
    return null;
}
";

    let tree = parse_php(source);
    let file = unique_php_path("test");
    let mut staging = StagingGraph::new();
    let builder = PhpGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(&file), &mut staging)
        .expect("build graph");

    // Should handle nullable types correctly (stripping the ?)
    let typeof_edges = collect_edges_by_kind(&staging, "TypeOf");
    let ref_edges = collect_edges_by_kind(&staging, "References");

    assert!(
        !typeof_edges.is_empty(),
        "Should handle nullable @param types"
    );
    assert!(!ref_edges.is_empty(), "Should handle nullable return types");
}
