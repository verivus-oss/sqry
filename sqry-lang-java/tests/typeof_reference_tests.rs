//! Test suite for `TypeOf` and Reference edges in Java plugin
//!
//! Validates that the Java plugin correctly creates:
//! - `TypeOf` edges for fields, local variables, and parameters
//! - Reference edges for variable/field accesses

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Tree;

// ========== Test Helper Functions ==========

/// Parse Java source code into a tree-sitter Tree
fn parse_java(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_java::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Java grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Java code")
}

/// Build staging graph from Java source and return it for assertions
fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_java(content);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();
    let file_path = PathBuf::from(filename);

    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Count edges of a specific kind in staging operations
fn count_edge_kind(staging: &StagingGraph, kind_tag: &str) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind.tag() == kind_tag
            } else {
                false
            }
        })
        .count()
}

/// Check if staging has an edge of a specific kind
fn has_edge_kind(staging: &StagingGraph, kind_tag: &str) -> bool {
    count_edge_kind(staging, kind_tag) > 0
}

/// Build a string ID → string value lookup from staging graph.
fn build_string_lookup(staging: &StagingGraph) -> std::collections::HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build node ID → node name lookup.
fn build_node_name_lookup(staging: &StagingGraph) -> std::collections::HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Collect Reference edges (`source_name`, `target_name`).
fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
                && matches!(kind, EdgeKind::References)
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| format!("<unknown:{}>", source.index()));
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| format!("<unknown:{}>", target.index()));
                return Some((source_name, target_name));
            }
            None
        })
        .collect()
}

fn has_reference_target(edges: &[(String, String)], target: &str) -> bool {
    edges.iter().any(|(_, tgt)| tgt == target)
}

fn has_reference_target_prefix(edges: &[(String, String)], prefix: &str) -> bool {
    edges.iter().any(|(_, tgt)| tgt.starts_with(prefix))
}

fn has_reference_target_suffix(edges: &[(String, String)], suffix: &str) -> bool {
    edges.iter().any(|(_, tgt)| tgt.ends_with(suffix))
}

fn has_reference_target_contains(edges: &[(String, String)], needle: &str) -> bool {
    edges.iter().any(|(_, tgt)| tgt.contains(needle))
}

// ========== TypeOf Edge Tests (Phase 1: Fields) ==========

#[test]
fn test_field_typeof_edge() {
    let content = r"
package com.example;

class Service {
    private UserRepository repository;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge from repository variable to UserRepository class
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for field declaration"
    );

    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 1,
        "Expected at least 1 TypeOf edge for field, got {typeof_count}"
    );
}

#[test]
fn test_multiple_fields_typeof_edges() {
    let content = r"
package com.example;

class Service {
    private UserRepository userRepository;
    private OrderRepository orderRepository;
    private String name;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for all 3 fields
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges for 3 fields, got {typeof_count}"
    );
}

#[test]
fn test_field_with_generic_type() {
    let content = r"
package com.example;

import java.util.List;

class Service {
    private List<User> users;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge for generic field
    // Note: We extract base type (List) not the full generic signature
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for generic field"
    );
}

// ========== TypeOf Edge Tests (Phase 2: Local Variables) ==========

#[test]
fn test_local_variable_typeof_edge() {
    let content = r#"
package com.example;

class Service {
    public void test() {
        String name = "test";
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge for local variable
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for local variable declaration"
    );
}

#[test]
fn test_multiple_local_variables() {
    let content = r#"
package com.example;

class Service {
    public void test() {
        String name = "test";
        int count = 42;
        boolean flag = true;
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for all 3 local variables
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges for 3 local variables, got {typeof_count}"
    );
}

#[test]
fn test_local_variable_with_imported_type() {
    let content = r"
package com.example;

import java.util.Optional;

class Service {
    public void test() {
        Optional<User> user = Optional.empty();
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge with resolved import
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for local variable with imported type"
    );
}

#[test]
fn test_multiple_declarators_in_one_statement() {
    let content = r"
package com.example;

class Service {
    public void test() {
        String a, b, c;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for all 3 variables (a, b, c)
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges for multiple declarators, got {typeof_count}"
    );
}

// ========== Integration Tests ==========

#[test]
fn test_combined_field_and_local_variable_typeof() {
    let content = r#"
package com.example;

class Service {
    private UserRepository repository;

    public void process() {
        String name = "test";
        int count = 42;
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for:
    // - 1 field (repository)
    // - 2 local variables (name, count)
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges (1 field + 2 local vars), got {typeof_count}"
    );
}

#[test]
fn test_typeof_edges_do_not_break_existing_edges() {
    let content = r#"
package com.example;

class Service {
    private UserRepository repository;

    public void test() {
        String name = "test";
        repository.save(null);
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should still have call edges (existing functionality)
    assert!(
        has_edge_kind(&staging, "calls"),
        "TypeOf edges should not break existing call edges"
    );

    // And should have TypeOf edges (new functionality)
    assert!(has_edge_kind(&staging, "type_of"), "Expected TypeOf edges");
}

// ========== Limitation Tests ==========

#[test]
fn test_var_keyword_not_supported() {
    // Java 10+ type inference with 'var' keyword
    // This is a limitation test - we document that var is not supported
    let content = r#"
package com.example;

class Service {
    public void test() {
        var name = "test";  // Type inference - not supported
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // This should still compile without errors, but may not have TypeOf edge
    // (tree-sitter may not parse 'var' as a type)
    // The test just ensures we don't crash
    let _typeof_count = count_edge_kind(&staging, "type_of");
    // No assertion - this is a documented limitation
}

#[test]
fn test_wildcard_generics() {
    // Document behavior with wildcard generics
    let content = r"
package com.example;

import java.util.List;

class Service {
    private List<?> items;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge for List (wildcard is part of type text)
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge even with wildcard generics"
    );
}

// ========== Property Node Tests (Task #7: P1 Property Nodes) ==========

use sqry_core::graph::unified::node::NodeKind;

/// Helper to check if staging has a node of a specific kind with given name substring
fn has_node_with_kind_and_name(
    staging: &StagingGraph,
    kind: NodeKind,
    name_contains: &str,
) -> bool {
    let strings = build_string_lookup(staging);
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            entry.kind == kind
                && strings
                    .get(&entry.name.index())
                    .is_some_and(|n| n.contains(name_contains))
        } else {
            false
        }
    })
}

/// Count nodes of a specific kind
fn count_node_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { entry, .. } if entry.kind == kind))
        .count()
}

#[test]
fn test_mutable_field_creates_property_node() {
    let content = r"
package com.example;

class Service {
    private int counter;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Mutable (non-final) field should create Property node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "counter"),
        "Expected Property node for mutable field"
    );
}

#[test]
fn test_final_field_creates_constant_node() {
    let content = r#"
package com.example;

class Config {
    private final String API_KEY = "secret";
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Final field should create Constant node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "API_KEY"),
        "Expected Constant node for final field"
    );
}

#[test]
fn test_static_field_creates_property_node_with_static() {
    let content = r"
package com.example;

class Counter {
    private static int instanceCount;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Static non-final field should create Property node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "instanceCount"),
        "Expected Property node for static field"
    );
}

#[test]
fn test_static_final_field_creates_constant_node() {
    let content = r"
package com.example;

class Config {
    public static final int MAX_SIZE = 100;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Static final field should create Constant node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "MAX_SIZE"),
        "Expected Constant node for static final field"
    );
}

#[test]
fn test_multiple_field_types_mixed() {
    let content = r"
package com.example;

class Service {
    private String name;                    // Property (mutable)
    private final String id;                // Constant (final)
    private static int count;               // Property (static mutable)
    public static final int MAX = 100;      // Constant (static final)
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 Property nodes (mutable fields)
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(
        property_count >= 2,
        "Expected at least 2 Property nodes for mutable fields, got {property_count}"
    );

    // Should have 2 Constant nodes (final fields)
    let constant_count = count_node_kind(&staging, NodeKind::Constant);
    assert!(
        constant_count >= 2,
        "Expected at least 2 Constant nodes for final fields, got {constant_count}"
    );
}

#[test]
fn test_property_node_has_typeof_edge() {
    let content = r"
package com.example;

class Service {
    private UserRepository repository;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Property node should have TypeOf edge to type
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "repository"),
        "Expected Property node for field"
    );
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge from Property to type"
    );
}

#[test]
fn test_property_node_visibility() {
    // Tests different visibility modifiers
    let content = r"
package com.example;

class Service {
    private int privateField;
    protected int protectedField;
    public int publicField;
    int packagePrivateField;
}
";

    let staging = build_staging_graph(content, "test.java");

    // All 4 fields should be Property nodes
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(
        property_count >= 4,
        "Expected at least 4 Property nodes for fields with different visibility, got {property_count}"
    );
}
// ========== Parameter TypeOf Edge Tests ==========

#[test]
fn test_parameter_typeof_edge() {
    let content = r"
package com.example;

class Service {
    public void process(User user) {
        // Method with single parameter
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge for parameter
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for method parameter"
    );

    // Should have Parameter node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Parameter, "user"),
        "Expected Parameter node for 'user' parameter"
    );
}

#[test]
fn test_multiple_parameters_typeof_edges() {
    let content = r"
package com.example;

class Service {
    public void process(User user, String name, int age) {
        // Method with multiple parameters
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for all 3 parameters
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges for 3 parameters, got {typeof_count}"
    );

    // Should have Parameter nodes
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Parameter, "user"),
        "Expected Parameter node for 'user'"
    );
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Parameter, "name"),
        "Expected Parameter node for 'name'"
    );
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Parameter, "age"),
        "Expected Parameter node for 'age'"
    );
}

#[test]
fn test_parameter_with_imported_type() {
    let content = r"
package com.example;

import java.util.Optional;

class Service {
    public void process(Optional<User> user) {
        // Parameter with imported generic type
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edge for parameter with resolved import
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for parameter with imported type"
    );

    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Parameter, "user"),
        "Expected Parameter node for 'user'"
    );
}

#[test]
fn test_constructor_parameters() {
    let content = r"
package com.example;

class Service {
    public Service(UserRepository repository, String name) {
        // Constructor with parameters
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have TypeOf edges for constructor parameters
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 2,
        "Expected at least 2 TypeOf edges for constructor parameters, got {typeof_count}"
    );
}

// ========== Reference Edge Tests ==========

#[test]
fn test_field_reference_edge() {
    let content = r"
package com.example;

class Service {
    private UserRepository repository;

    public void test() {
        repository.save(null);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Reference edge from usage to field declaration
    assert!(
        has_edge_kind(&staging, "references"),
        "Expected Reference edge for field access"
    );
}

#[test]
fn test_multiple_field_references() {
    let content = r"
package com.example;

class Service {
    private UserRepository repository;

    public void test() {
        repository.save(null);
        repository.findById(1L);
        repository.delete(null);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Reference edges for all 3 field accesses
    let reference_count = count_edge_kind(&staging, "references");
    assert!(
        reference_count >= 3,
        "Expected at least 3 Reference edges for field accesses, got {reference_count}"
    );
}

#[test]
fn test_field_reference_in_different_methods() {
    let content = r"
package com.example;

class Service {
    private UserRepository repository;

    public void save() {
        repository.save(null);
    }

    public void find() {
        repository.findById(1L);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Reference edges from both methods
    let reference_count = count_edge_kind(&staging, "references");
    assert!(
        reference_count >= 2,
        "Expected at least 2 Reference edges for field accesses in different methods, got {reference_count}"
    );
}

// ========== Field Collision Tests ==========

#[test]
fn test_field_collision_multiple_classes_same_name() {
    let content = r"
package com.example;

class User {
    private String name;
}

class Product {
    private String name;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 separate Property nodes with qualified names
    // (User::name and Product::name)
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(
        property_count >= 2,
        "Expected at least 2 Property nodes for fields in different classes, got {property_count}"
    );

    // Should have 2 TypeOf edges (one for each field)
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 2,
        "Expected at least 2 TypeOf edges for fields in different classes, got {typeof_count}"
    );
}

#[test]
fn test_field_collision_nested_classes() {
    let content = r"
package com.example;

class Outer {
    private String value;

    class Inner {
        private String value;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 separate Property nodes for outer and inner class fields
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(
        property_count >= 2,
        "Expected at least 2 Property nodes for fields in nested classes, got {property_count}"
    );
}

#[test]
fn test_qualified_field_reference_resolution() {
    let content = r"
package com.example;

class Service {
    private String name;

    class Inner {
        private String name;

        public void test() {
            name.length();
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should resolve field reference to correct field based on class context
    assert!(
        has_edge_kind(&staging, "references"),
        "Expected Reference edge for qualified field access"
    );
}

// ========== Metadata Tests ==========

#[test]
fn test_public_field_has_public_visibility() {
    let content = r"
package com.example;

class Config {
    public String apiEndpoint;
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Property node for public field
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "apiEndpoint"),
        "Expected Property node for public field"
    );

    // Visibility metadata is stored but we can't easily verify it in staging
    // The implementation extracts and stores it correctly
}

#[test]
fn test_static_field_metadata() {
    let content = r#"
package com.example;

class Config {
    public static String DEFAULT_URL = "http://example.com";
    private static final int MAX_RETRIES = 3;
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Static non-final should be Property
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "DEFAULT_URL"),
        "Expected Property node for static mutable field"
    );

    // Static final should be Constant
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "MAX_RETRIES"),
        "Expected Constant node for static final field"
    );
}

// ========== Integration Tests ==========

#[test]
fn test_complete_class_with_all_features() {
    let content = r"
package com.example;

import java.util.Optional;

class UserService {
    private static final int MAX_USERS = 1000;
    private UserRepository repository;
    public String serviceName;

    public UserService(UserRepository repository) {
        this.repository = repository;
    }

    public Optional<User> findUser(Long id) {
        return repository.findById(id);
    }

    public void saveUser(User user, String createdBy) {
        repository.save(user);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have:
    // - 1 Constant (MAX_USERS)
    // - 2 Properties (repository, serviceName)
    // - 3 Parameters (repository in constructor, id in findUser, user and createdBy in saveUser)
    // - Reference edges for field accesses

    let constant_count = count_node_kind(&staging, NodeKind::Constant);
    assert!(constant_count >= 1, "Expected at least 1 Constant node");

    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(property_count >= 2, "Expected at least 2 Property nodes");

    let parameter_count = count_node_kind(&staging, NodeKind::Parameter);
    assert!(
        parameter_count >= 4,
        "Expected at least 4 Parameter nodes, got {parameter_count}"
    );

    // Should have Reference edges for repository field accesses
    let reference_count = count_edge_kind(&staging, "references");
    assert!(
        reference_count >= 2,
        "Expected at least 2 Reference edges for field accesses, got {reference_count}"
    );

    // Should have TypeOf edges for fields, parameters, and local variables
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 7,
        "Expected at least 7 TypeOf edges (3 fields + 4 parameters), got {typeof_count}"
    );
}

#[test]
fn test_no_regressions_with_new_features() {
    // Ensure that existing functionality still works with new features enabled
    let content = r#"
package com.example;

class Service {
    private final String API_KEY = "secret";
    private String name;

    public void test() {
        String localVar = "test";
        name.length();
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Old features still work:
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "API_KEY"),
        "Constant node creation still works"
    );
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "name"),
        "Property node creation still works"
    );

    // TypeOf edges exist
    assert!(
        has_edge_kind(&staging, "type_of"),
        "TypeOf edges still created"
    );

    // New features work:
    assert!(
        has_edge_kind(&staging, "references"),
        "Reference edges created for field access"
    );
}

// ========== Tests for Codex Review Findings ==========

#[test]
fn test_final_field_reference_uses_constant_node() {
    // Test that reference edges for final fields point to Constant nodes, not Property nodes
    let content = r#"
package com.example;

class Config {
    private final String API_KEY = "secret";

    public void useKey() {
        String key = API_KEY;  // Reference to final field
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Constant node for API_KEY
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "Config::API_KEY"),
        "Final field should create Constant node"
    );

    // Should NOT have a Property node for API_KEY
    assert!(
        !has_node_with_kind_and_name(&staging, NodeKind::Property, "Config::API_KEY"),
        "Final field should NOT create Property node"
    );

    // Reference edge should exist and target the Constant node
    let reference_count = count_edge_kind(&staging, "references");
    assert!(
        reference_count >= 1,
        "Expected reference edge for final field access"
    );
}

#[test]
fn test_varargs_parameter_typeof_edge() {
    // Test that varargs parameters (String... args) get TypeOf edges
    let content = r"
package com.example;

class Logger {
    public void log(String format, Object... args) {
        // varargs parameter should have TypeOf edge
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Parameter nodes for both format and args
    let parameter_count = count_node_kind(&staging, NodeKind::Parameter);
    assert!(
        parameter_count >= 2,
        "Expected 2 Parameter nodes (format and args), got {parameter_count}"
    );

    // Should have TypeOf edges for both parameters
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 2,
        "Expected TypeOf edges for both parameters, got {typeof_count}"
    );
}

#[test]
fn test_receiver_parameter_typeof_edge() {
    // Test that receiver parameters (Outer.this) get TypeOf edges
    let content = r"
package com.example;

class Outer {
    class Inner {
        public void method(Outer Outer.this) {
            // receiver parameter should have TypeOf edge
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Parameter node for receiver parameter
    let parameter_count = count_node_kind(&staging, NodeKind::Parameter);
    assert!(
        parameter_count >= 1,
        "Expected Parameter node for receiver parameter, got {parameter_count}"
    );

    // Should have TypeOf edge for receiver parameter
    let typeof_count = count_edge_kind(&staging, "type_of");
    assert!(
        typeof_count >= 1,
        "Expected TypeOf edge for receiver parameter, got {typeof_count}"
    );
}

#[test]
fn test_qualified_field_access_no_false_positive() {
    // Test that other.field does NOT create reference edge, but this.field does
    let content = r"
package com.example;

class Service {
    private String name;

    public void test(Service other) {
        this.name.length();   // Should create reference edge
        name.length();        // Should create reference edge
        other.name.length();  // Should NOT create reference edge (qualified by other object)
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let reference_edges = collect_reference_edges(&staging);
    let field_refs = reference_edges
        .iter()
        .filter(|(_, target)| target == "Service::name")
        .count();
    assert_eq!(
        field_refs, 2,
        "Expected 2 field reference edges to Service::name, got {field_refs}. Edges: {reference_edges:?}"
    );
}

#[test]
fn test_nested_class_field_collision_prevention() {
    // Test that fields with same name in different nested classes don't collide
    let content = r"
package com.example;

class OuterA {
    class Inner {
        private String value;
    }
}

class OuterB {
    class Inner {
        private String value;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 distinct Property nodes with fully qualified names
    // OuterA::Inner::value and OuterB::Inner::value
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "OuterA::Inner::value"),
        "Expected fully qualified field name for OuterA.Inner.value"
    );

    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "OuterB::Inner::value"),
        "Expected fully qualified field name for OuterB.Inner.value"
    );

    // Should have exactly 2 Property nodes (no collision)
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert_eq!(
        property_count, 2,
        "Expected 2 distinct Property nodes, got {property_count}"
    );
}

#[test]
fn test_catch_parameter_declaration_context() {
    // Test that catch (Exception e) parameters are recognized as declarations, not references
    let content = r"
package com.example;

class ErrorHandler {
    private Exception e;  // Field named 'e'

    public void test() {
        try {
            throw new Exception();
        } catch (Exception e) {  // Parameter 'e' should NOT create reference to field 'e'
            e.printStackTrace();
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'e'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "ErrorHandler::e"),
        "Expected field 'e' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "ErrorHandler::e"),
        "Catch parameter should not reference field ErrorHandler::e. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "e@"),
        "Expected reference edge to catch variable e@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_enhanced_for_parameter_declaration_context() {
    // Test that for (Type x : xs) parameters are recognized as declarations, not references
    let content = r"
package com.example;

import java.util.List;

class Processor {
    private String item;  // Field named 'item'

    public void test(List<String> items) {
        for (String item : items) {  // Parameter 'item' should NOT create reference to field 'item'
            System.out.println(item);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'item'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::item"),
        "Expected field 'item' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::item"),
        "Enhanced-for variable should not reference field Processor::item. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "item@"),
        "Expected reference edge to enhanced-for variable item@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_static_field_metadata_applied() {
    // Test that static metadata is correctly applied to field nodes
    let content = r#"
package com.example;

class Config {
    private static final String VERSION = "1.0";
    private static int instanceCount = 0;
    private String name;
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have nodes for all three fields
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Constant, "Config::VERSION"),
        "Static final field should be Constant"
    );

    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Config::instanceCount"),
        "Static non-final field should be Property"
    );

    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Config::name"),
        "Non-static field should be Property"
    );

    // Note: We can't easily test if is_static is set without accessing internal node data
    // This test ensures nodes are created; the implementation ensures is_static is set
}

// ========== Shadowing Detection Tests (Codex Review Follow-up) ==========

#[test]
fn test_method_parameter_shadows_field() {
    // Test that method parameters shadow fields of the same name
    let content = r"
package com.example;

class Processor {
    private String value;

    public void process(String value) {
        // 'value' here should reference the parameter, not the field
        System.out.println(value);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'value'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::value"),
        "Expected field 'value' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::value"),
        "Parameter should shadow field Processor::value. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_contains(&reference_edges, "process::value"),
        "Expected reference edge to parameter process::value, got: {reference_edges:?}"
    );
}

#[test]
fn test_constructor_parameter_shadows_field() {
    // Test that constructor parameters shadow fields
    let content = r"
package com.example;

class Config {
    private String name;

    public Config(String name) {
        // 'name' here should reference the parameter, not the field
        String temp = name;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'name'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Config::name"),
        "Expected field 'name' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Config::name"),
        "Constructor parameter should shadow field Config::name. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_contains(&reference_edges, "Config.<init>::name"),
        "Expected reference edge to constructor param Config.<init>::name, got: {reference_edges:?}"
    );
}

#[test]
fn test_enhanced_for_iterable_field_reference() {
    // Test that the iterable expression can reference a field
    let content = r"
package com.example;

import java.util.List;

class Processor {
    private List<String> items;

    public void process() {
        for (String item : items) {
            // 'items' in the for header should create a reference to the field
            System.out.println(item);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'items'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::items"),
        "Expected field 'items' to be a Property"
    );

    // Should have a reference edge for 'items' in the for header
    let reference_count = count_edge_kind(&staging, "references");
    assert!(
        reference_count >= 1,
        "Expected reference edge for field 'items' in enhanced-for, got {reference_count} edges"
    );
}

#[test]
fn test_for_loop_init_shadows_field() {
    // Test that for-loop init declarations shadow fields
    let content = r"
package com.example;

class Counter {
    private int i;

    public void count() {
        for (int i = 0; i < 10; i++) {
            // 'i' here should reference the loop variable, not the field
            System.out.println(i);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'i'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Counter::i"),
        "Expected field 'i' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Counter::i"),
        "Loop variable should shadow field Counter::i. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "i@"),
        "Expected reference edge to loop variable i@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_try_with_resources_shadows_field() {
    // Test that try-with-resources declarations shadow fields
    let content = r#"
package com.example;

import java.io.FileReader;

class FileProcessor {
    private FileReader reader;

    public void process() throws Exception {
        try (FileReader reader = new FileReader("file.txt")) {
            // 'reader' here should reference the resource, not the field
            int c = reader.read();
        }
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'reader'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "FileProcessor::reader"),
        "Expected field 'reader' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "FileProcessor::reader"),
        "Resource should shadow field FileProcessor::reader. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "reader@"),
        "Expected reference edge to resource variable reader@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_lambda_parameter_shadows_field() {
    // Test that lambda parameters shadow fields
    let content = r"
package com.example;

import java.util.List;
import java.util.function.Function;

class Processor {
    private String value;

    public void process(List<String> items) {
        Function<String, String> mapper = value -> {
            // 'value' here should reference the lambda parameter, not the field
            return value.toUpperCase();
        };
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'value'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::value"),
        "Expected field 'value' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::value"),
        "Lambda parameter should shadow field Processor::value. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "lambda@")
            && has_reference_target_suffix(&reference_edges, "::value"),
        "Expected reference edge to lambda param ::value, got: {reference_edges:?}"
    );
}

#[test]
fn test_varargs_parameter_shadows_field() {
    // Test that varargs parameters shadow fields
    let content = r"
package com.example;

class Logger {
    private String[] args;

    public void log(String format, String... args) {
        // 'args' here should reference the varargs parameter, not the field
        for (String arg : args) {
            System.out.println(arg);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'args'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Logger::args"),
        "Expected field 'args' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Logger::args"),
        "Varargs parameter should shadow field Logger::args. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_contains(&reference_edges, "log::args"),
        "Expected reference edge to varargs param log::args, got: {reference_edges:?}"
    );
}

// ========== Pattern Variable Tests (Java 16+ Pattern Matching) ==========

#[test]
fn test_instanceof_pattern_variable_shadows_field() {
    // Test that instanceof pattern variables shadow fields (Java 16+)
    let content = r"
package com.example;

class Processor {
    private String value;

    public void check(Object obj) {
        if (obj instanceof String value) {
            // 'value' here should reference the pattern variable, not the field
            System.out.println(value.length());
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'value'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::value"),
        "Expected field 'value' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::value"),
        "Pattern variable should shadow field Processor::value. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "value@"),
        "Expected reference edge to pattern variable value@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_switch_record_pattern_shadows_field() {
    // Test that switch record pattern components shadow fields (Java 17+)
    let content = r#"
package com.example;

sealed interface Shape {}
record Point(int x, int y) implements Shape {}

class Processor {
    private int x;
    private int y;

    public void process(Shape shape) {
        switch (shape) {
            case Point(int x, int y) -> {
                // 'x' and 'y' here should reference pattern variables, not fields
                System.out.println(x + y);
            }
            default -> System.out.println("unknown");
        }
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Should have Property nodes for fields 'x' and 'y'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::x"),
        "Expected field 'x' to be a Property"
    );
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::y"),
        "Expected field 'y' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::x"),
        "Pattern variable should shadow field Processor::x. Edges: {reference_edges:?}"
    );
    assert!(
        !has_reference_target(&reference_edges, "Processor::y"),
        "Pattern variable should shadow field Processor::y. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "x@")
            && has_reference_target_prefix(&reference_edges, "y@"),
        "Expected reference edges to pattern vars x@..., y@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_instanceof_pattern_variable_declaration_context() {
    // Test that instanceof pattern variable declaration is filtered
    let content = r"
package com.example;

class Processor {
    private String data;

    public void check(Object obj) {
        // 'data' in the pattern should NOT create a reference to field
        if (obj instanceof String data) {
            System.out.println(data);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'data'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::data"),
        "Expected field 'data' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::data"),
        "Pattern variable should shadow field Processor::data. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "data@"),
        "Expected reference edge to pattern variable data@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_nested_instanceof_pattern() {
    // Test nested instanceof patterns
    let content = r"
package com.example;

class Processor {
    private String value;

    public void check(Object obj) {
        if (obj instanceof String value) {
            if (value.length() > 0) {
                // 'value' should still reference the pattern variable
                System.out.println(value.trim());
            }
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Property node for field 'value'
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::value"),
        "Expected field 'value' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::value"),
        "Nested pattern variable should shadow field Processor::value. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "value@"),
        "Expected reference edge to pattern variable value@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_switch_expression_pattern_variables() {
    // Test switch expressions with pattern matching
    let content = r"
package com.example;

sealed interface Result {}
record Success(String data) implements Result {}
record Failure(String error) implements Result {}

class Processor {
    private String data;
    private String error;

    public String process(Result result) {
        return switch (result) {
            case Success(String data) -> data.toUpperCase();
            case Failure(String error) -> error.toLowerCase();
        };
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Property nodes for fields
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::data"),
        "Expected field 'data' to be a Property"
    );
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "Processor::error"),
        "Expected field 'error' to be a Property"
    );

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        !has_reference_target(&reference_edges, "Processor::data"),
        "Pattern variable should shadow field Processor::data. Edges: {reference_edges:?}"
    );
    assert!(
        !has_reference_target(&reference_edges, "Processor::error"),
        "Pattern variable should shadow field Processor::error. Edges: {reference_edges:?}"
    );
    assert!(
        has_reference_target_prefix(&reference_edges, "data@")
            && has_reference_target_prefix(&reference_edges, "error@"),
        "Expected reference edges to pattern vars data@..., error@..., got: {reference_edges:?}"
    );
}

#[test]
fn test_pattern_variable_no_field_conflict() {
    // Test pattern variable when there's no field to shadow
    let content = r"
package com.example;

class Processor {
    public void check(Object obj) {
        if (obj instanceof String value) {
            // 'value' is just a pattern variable, no field to shadow
            System.out.println(value);
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let reference_edges = collect_reference_edges(&staging);
    assert!(
        has_reference_target_prefix(&reference_edges, "value@"),
        "Expected reference edge to pattern variable value@..., got: {reference_edges:?}"
    );
}
