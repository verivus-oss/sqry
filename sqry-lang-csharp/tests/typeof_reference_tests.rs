//! Test suite for TypeOf and Reference edges in C# plugin
//!
//! Validates that the C# plugin correctly creates:
//! - TypeOf edges for variables, fields, properties, and parameters
//! - Reference edges for type usage
//! - Proper handling of generics, nullables, and arrays

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_csharp::relations::CSharpGraphBuilder;
use sqry_test_support::graph_helpers::build_string_lookup;
use std::path::Path;
use tree_sitter::Parser;

// ========== Test Helper Functions ==========

/// Parse C# source code into a tree-sitter Tree
fn parse_csharp(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_c_sharp::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load C# grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse C# code")
}

/// Build staging graph from C# source and return it for assertions
fn build_test_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_csharp(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file_path = Path::new(filename);

    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Helper to collect edges of a specific kind with resolved names
fn collect_edges_by_kind(
    staging: &StagingGraph,
    edge_kind_filter: impl Fn(&EdgeKind) -> bool,
) -> Vec<(String, String)> {
    let strings = build_string_lookup(staging);

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
                && edge_kind_filter(kind)
            {
                let from_name = staging.operations().iter().find_map(|op| {
                    if let StagingOp::AddNode {
                        expected_id: Some(id),
                        entry,
                        ..
                    } = op
                        && id == source
                    {
                        return strings.get(&entry.name.index()).cloned();
                    }
                    None
                });
                let to_name = staging.operations().iter().find_map(|op| {
                    if let StagingOp::AddNode {
                        expected_id: Some(id),
                        entry,
                        ..
                    } = op
                        && id == target
                    {
                        return strings.get(&entry.name.index()).cloned();
                    }
                    None
                });
                if let (Some(from), Some(to)) = (from_name, to_name) {
                    return Some((from, to));
                }
            }
            None
        })
        .collect()
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

// ========== Simple Type TypeOf Edge Tests ==========

#[test]
fn test_local_variable_simple_type() {
    let content = r#"
class Test {
    void Method() {
        int x = 5;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "x" && typ == "int"),
        "Expected TypeOf edge from x to int, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_multiple_local_variables_different_types() {
    let content = r#"
class Test {
    void Method() {
        int count = 42;
        string name = "test";
        bool flag = true;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "int"),
        "Expected TypeOf edge from count to int"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "name" && typ == "string"),
        "Expected TypeOf edge from name to string"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "flag" && typ == "bool"),
        "Expected TypeOf edge from flag to bool"
    );
}

#[test]
fn test_reference_edge_created_with_typeof() {
    let content = r#"
class Test {
    void Method() {
        int x = 5;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    // Check both TypeOf and Reference edges exist
    assert!(
        has_edge_kind(&staging, "type_of"),
        "Expected TypeOf edge for variable x"
    );
    assert!(
        has_edge_kind(&staging, "references"),
        "Expected Reference edge for variable x"
    );

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "x" && typ == "int"),
        "Expected Reference edge from x to int"
    );
}

// ========== Generic Type TypeOf Edge Tests ==========

#[test]
fn test_generic_type_extracts_base() {
    let content = r#"
using System.Collections.Generic;

class Test {
    void Method() {
        List<User> users = new List<User>();
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "List<User>"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "List<User>"),
        "Expected TypeOf edge from users to List<User> (full type), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_generic_dictionary() {
    let content = r#"
using System.Collections.Generic;

class Test {
    void Method() {
        Dictionary<int, string> map = new Dictionary<int, string>();
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "Dictionary<int, string>"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "map" && typ == "Dictionary<int, string>"),
        "Expected TypeOf edge from map to Dictionary<int, string> (full type)"
    );
}

// ========== Nullable Type TypeOf Edge Tests ==========

#[test]
fn test_nullable_value_type() {
    let content = r#"
class Test {
    void Method() {
        int? count = null;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "int?"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "int?"),
        "Expected TypeOf edge from count to int? (full type), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_nullable_reference_type() {
    let content = r#"
class Test {
    void Method() {
        string? name = null;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "string?"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "name" && typ == "string?"),
        "Expected TypeOf edge from name to string? (full type)"
    );
}

// ========== Field Declaration TypeOf Edge Tests ==========

#[test]
fn test_field_declaration_typeof() {
    let content = r#"
class Service {
    private UserRepository repository;
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should have TypeOf edge from Service.repository to UserRepository
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "Service.repository" && typ == "UserRepository"),
        "Expected TypeOf edge from Service.repository to UserRepository, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_multiple_fields_typeof() {
    let content = r#"
class User {
    private int age;
    private string name;
    public bool isActive;
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "User.age" && typ == "int"),
        "Expected TypeOf edge for User.age"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "User.name" && typ == "string"),
        "Expected TypeOf edge for User.name"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "User.isActive" && typ == "bool"),
        "Expected TypeOf edge for User.isActive"
    );
}

#[test]
fn test_field_with_generic_type() {
    let content = r#"
using System.Collections.Generic;

class Service {
    private List<User> users;
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "List<User>"
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "Service.users" && typ == "List<User>"),
        "Expected TypeOf edge from Service.users to List<User> (full type)"
    );
}

// ========== Property Declaration TypeOf Edge Tests ==========

#[test]
fn test_auto_property_typeof() {
    let content = r#"
class User {
    public int Age { get; set; }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.Age" && typ == "int"),
        "Expected TypeOf edge from User.Age to int, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_readonly_property_typeof() {
    let content = r#"
class User {
    public string Name { get; }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.Name" && typ == "string"),
        "Expected TypeOf edge from User.Name to string"
    );
}

#[test]
fn test_computed_property_typeof() {
    let content = r#"
class User {
    private int age;
    public bool IsAdult { get { return age >= 18; } }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.IsAdult" && typ == "bool"),
        "Expected TypeOf edge from User.IsAdult to bool"
    );
}

// ========== Array Type Tests ==========

#[test]
fn test_array_type_extracts_base() {
    let content = r#"
class Test {
    void Method() {
        int[] numbers = new int[5];
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full type signature "int[]"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "numbers" && typ == "int[]"),
        "Expected TypeOf edge from numbers to int[] (full type)"
    );
}

// ========== Multiple Declarators Test ==========

#[test]
fn test_multiple_declarators_in_one_statement() {
    let content = r#"
class Test {
    void Method() {
        int a = 1, b = 2, c = 3;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should have TypeOf edges for all 3 variables
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "a" && typ == "int"),
        "Expected TypeOf edge for variable a"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "b" && typ == "int"),
        "Expected TypeOf edge for variable b"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "c" && typ == "int"),
        "Expected TypeOf edge for variable c"
    );
}

// ========== Integration Tests ==========

#[test]
fn test_typeof_does_not_break_existing_call_edges() {
    let content = r#"
class Service {
    private UserRepository repository;

    public void Test() {
        int count = repository.Count();
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    // Should still have call edges (existing functionality)
    assert!(
        has_edge_kind(&staging, "calls"),
        "TypeOf edges should not break existing call edges"
    );

    // And should have TypeOf edges (new functionality)
    assert!(has_edge_kind(&staging, "type_of"), "Expected TypeOf edges");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should have TypeOf edges for both field and local variable
    assert!(
        typeof_edges
            .iter()
            .any(|(node, _)| node.contains("repository")),
        "Expected TypeOf edge for repository field"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "int"),
        "Expected TypeOf edge for count variable"
    );
}

#[test]
fn test_visibility_and_typeof_work_together() {
    let content = r#"
class Widget {
    private int count;

    public string Process() {
        string result = "";
        return result;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    // Should have TypeOf edges
    assert!(has_edge_kind(&staging, "type_of"), "Expected TypeOf edges");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Field typeof
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "Widget.count" && typ == "int"),
        "Expected TypeOf edge for field count"
    );

    // Local variable typeof
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "string"),
        "Expected TypeOf edge for local variable result"
    );

    // Verify method still exists (visibility work from Task #6)
    let strings = build_string_lookup(&staging);
    let has_method = staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            strings
                .get(&entry.name.index())
                .is_some_and(|name| name.contains("Widget.Process"))
        } else {
            false
        }
    });
    assert!(has_method, "Method Widget.Process should exist");
}

#[test]
fn test_qualified_type_name_preserved() {
    let content = r#"
class Test {
    void Method() {
        System.Text.StringBuilder builder = new System.Text.StringBuilder();
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Qualified type name should be preserved (no generics to strip)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "builder" && typ == "System.Text.StringBuilder"),
        "Expected TypeOf edge with qualified type name preserved, got: {:?}",
        typeof_edges
    );
}

// ========== Count Tests ==========

#[test]
fn test_combined_field_and_local_variable_typeof() {
    let content = r#"
class Service {
    private UserRepository repository;

    public void Process() {
        string name = "test";
        int count = 42;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_count = count_edge_kind(&staging, "type_of");

    // Should have at least 3 TypeOf edges:
    // - 1 field (repository)
    // - 2 local variables (name, count)
    assert!(
        typeof_count >= 3,
        "Expected at least 3 TypeOf edges (1 field + 2 local vars), got {}",
        typeof_count
    );
}

// ========== Method Return Type TypeOf Edge Tests ==========

#[test]
fn test_method_return_type_simple() {
    let content = r#"
class Service {
    public string GetName() {
        return "test";
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method.contains("GetName") && typ == "string"),
        "Expected TypeOf edge from GetName method to string return type, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_method_return_type_generic() {
    let content = r#"
class Service {
    public List<User> GetUsers() {
        return null;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method.contains("GetUsers") && typ == "List<User>"),
        "Expected TypeOf edge from GetUsers to List<User> (full type), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_method_return_type_nullable() {
    let content = r#"
class Service {
    public string? TryGetName() {
        return null;
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method.contains("TryGetName") && typ == "string?"),
        "Expected TypeOf edge from TryGetName to string? (full type), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_method_void_return_no_typeof() {
    let content = r#"
class Service {
    public void Process() {
    }
}
"#;

    let staging = build_test_graph(content, "test.cs");

    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should not create TypeOf edge for void return type
    let void_edges: Vec<_> = typeof_edges
        .iter()
        .filter(|(method, typ)| method.contains("Process") && typ == "void")
        .collect();

    assert!(
        void_edges.is_empty(),
        "Should not create TypeOf edge for void return type"
    );
}
