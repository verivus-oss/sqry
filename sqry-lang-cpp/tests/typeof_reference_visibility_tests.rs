//! C++ `GraphBuilder` tests for `TypeOf` edges, Reference edges, and visibility metadata.
//!
//! Tests the unified graph builder implementation for:
//! - `TypeOf` edges (variable -> type relationships)
//! - Reference edges (variable references type)
//! - Visibility metadata (public/private/protected for class members, static for file scope)

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_cpp::relations::CppGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_cpp(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .expect("Failed to set C++ language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse C++ source")
}

use sqry_core::graph::unified::{NodeEntry, NodeId, StringId};

/// Helper to count typeof edges in staged operations.
///
/// Counts every `EdgeKind::TypeOf` regardless of the `(context, index, name)`
/// metadata triple. Free-variable typings emit `(None, None, None)` while the
/// post-U07 class/struct field migration emits
/// `(Some(TypeOfContext::Field), None, Some(<bare-name>))`. Both shapes are
/// part of the U07 cross-language field-emission contract and both must be
/// counted by visibility/typeof regression tests.
fn count_typeof_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TypeOf { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper to count reference edges in staged operations
fn count_reference_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::References,
                    ..
                }
            )
        })
        .count()
}

/// Build a map from `StringId` to string value from staging operations
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

/// Get node name from `NodeEntry` using string map
fn get_node_name(entry: &NodeEntry, string_map: &HashMap<StringId, String>) -> Option<String> {
    string_map.get(&entry.name).cloned()
}

/// Collect (source, target) pairs for `TypeOf` edges.
///
/// Captures every `EdgeKind::TypeOf` regardless of the
/// `(context, index, name)` triple so that both free-variable typings
/// (`(None, None, None)`) and post-U07 class/struct field typings
/// (`(Some(TypeOfContext::Field), None, Some(<bare-name>))`) are returned.
fn collect_typeof_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let string_map = build_string_map(staging);
    let mut node_names: HashMap<NodeId, String> = HashMap::new();

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::TypeOf { .. },
                source,
                target,
                ..
            } = op
                && let (Some(src_name), Some(tgt_name)) =
                    (node_names.get(source), node_names.get(target))
            {
                return Some((src_name.clone(), tgt_name.clone()));
            }
            None
        })
        .collect()
}

/// Find function/method visibility
fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_map(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Function | NodeKind::Method)
        {
            let node_name = strings.get(&entry.name);
            if node_name.is_some_and(|n| n.contains(name) || n.ends_with(name)) {
                return entry.visibility.and_then(|id| strings.get(&id).cloned());
            }
        }
    }
    None
}

/// Find variable / field visibility.
///
/// Accepts `NodeKind::Variable`, `NodeKind::Property`, and `NodeKind::Constant`.
/// Per the U07 cross-language field-emission contract
/// (`docs/development/cross-language-field-emission/02_DESIGN` §3.1.1, §4.1),
/// C++ class/struct fields now emit `Property` (mutable fields) or `Constant`
/// (`const` / `constexpr` fields) instead of `Variable`. Free-variable
/// declarations at file scope still emit `Variable`. This helper inspects
/// all three node kinds so visibility regressions can be asserted across the
/// migrated surface.
fn find_variable_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_map(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(
                entry.kind,
                NodeKind::Variable | NodeKind::Property | NodeKind::Constant
            )
        {
            let node_name = strings.get(&entry.name);
            if node_name.is_some_and(|n| n.contains(name) || n.ends_with(name)) {
                return entry.visibility.and_then(|id| strings.get(&id).cloned());
            }
        }
    }
    None
}

// ==================== TypeOf Edge Tests ====================

#[test]
fn test_simple_variable_typeof() {
    let content = r"
int x;
double y;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should have TypeOf edges for x and y
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with('x') && tgt == "int"),
        "Expected TypeOf edge from x to int"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with('y') && tgt == "double"),
        "Expected TypeOf edge from y to double"
    );
}

#[test]
fn test_qualified_type_typeof() {
    let content = r"
#include <string>
std::string name;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should extract simple type name from qualified name
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("name") && (tgt == "string" || tgt.contains("string"))),
        "Expected TypeOf edge from name to string (from std::string)"
    );
}

#[test]
fn test_template_type_typeof() {
    let content = r"
#include <vector>
std::vector<int> numbers;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should extract base type from template: vector<int> -> vector
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("numbers") && tgt.contains("vector")),
        "Expected TypeOf edge from numbers to vector (base type from vector<int>)"
    );
}

#[test]
fn test_pointer_type_typeof() {
    let content = r"
int* ptr;
const char* str;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should strip pointer modifier
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("ptr") && tgt == "int"),
        "Expected TypeOf edge from ptr to int (stripped *)"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("str") && tgt == "char"),
        "Expected TypeOf edge from str to char (stripped const and *)"
    );
}

#[test]
fn test_reference_type_typeof() {
    let content = r"
int value = 42;
int& ref = value;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should strip reference modifier
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("ref") && tgt == "int"),
        "Expected TypeOf edge from ref to int (stripped &)"
    );
}

#[test]
fn test_class_member_typeof() {
    let content = r"
class User {
public:
    int id;
    std::string name;
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_edges = collect_typeof_edges(&staging);

    // Should have TypeOf edges for class members
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("id") && tgt == "int"),
        "Expected TypeOf edge from User::id to int"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("name") && tgt.contains("string")),
        "Expected TypeOf edge from User::name to string"
    );
}

// ==================== Reference Edge Tests ====================

#[test]
fn test_typeof_and_reference_paired() {
    let content = r"
int x;
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let typeof_count = count_typeof_edges(&staging);
    let reference_count = count_reference_edges(&staging);

    // TypeOf and Reference edges should be created together
    assert_eq!(
        typeof_count, reference_count,
        "TypeOf and Reference edge counts should match"
    );
    assert!(typeof_count > 0, "Should have at least one TypeOf edge");
}

// ==================== Visibility Tests ====================

#[test]
fn test_public_class_member_visibility() {
    let content = r"
class MyClass {
public:
    int publicField;
    void publicMethod() {}
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check field visibility
    let field_visibility = find_variable_visibility(&staging, "publicField");
    assert_eq!(
        field_visibility,
        Some("public".to_string()),
        "publicField should have public visibility"
    );

    // Check method visibility
    let method_visibility = find_function_visibility(&staging, "publicMethod");
    assert_eq!(
        method_visibility,
        Some("public".to_string()),
        "publicMethod should have public visibility"
    );
}

#[test]
fn test_private_class_member_visibility() {
    let content = r"
class MyClass {
private:
    int privateField;
    void privateMethod() {}
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check field visibility
    let field_visibility = find_variable_visibility(&staging, "privateField");
    assert_eq!(
        field_visibility,
        Some("private".to_string()),
        "privateField should have private visibility"
    );

    // Check method visibility
    let method_visibility = find_function_visibility(&staging, "privateMethod");
    assert_eq!(
        method_visibility,
        Some("private".to_string()),
        "privateMethod should have private visibility"
    );
}

#[test]
fn test_protected_class_member_visibility() {
    let content = r"
class Base {
protected:
    int protectedField;
    void protectedMethod() {}
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check field visibility
    let field_visibility = find_variable_visibility(&staging, "protectedField");
    assert_eq!(
        field_visibility,
        Some("protected".to_string()),
        "protectedField should have protected visibility"
    );

    // Check method visibility
    let method_visibility = find_function_visibility(&staging, "protectedMethod");
    assert_eq!(
        method_visibility,
        Some("protected".to_string()),
        "protectedMethod should have protected visibility"
    );
}

#[test]
fn test_mixed_visibility_sections() {
    let content = r"
class MyClass {
public:
    int publicField;
    void publicMethod() {}
private:
    int privateField;
    void privateMethod() {}
public:
    int anotherPublicField;
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify visibility transitions work correctly
    assert_eq!(
        find_variable_visibility(&staging, "publicField"),
        Some("public".to_string()),
        "publicField should be public"
    );
    assert_eq!(
        find_function_visibility(&staging, "publicMethod"),
        Some("public".to_string()),
        "publicMethod should be public"
    );
    assert_eq!(
        find_variable_visibility(&staging, "privateField"),
        Some("private".to_string()),
        "privateField should be private"
    );
    assert_eq!(
        find_function_visibility(&staging, "privateMethod"),
        Some("private".to_string()),
        "privateMethod should be private"
    );
    assert_eq!(
        find_variable_visibility(&staging, "anotherPublicField"),
        Some("public".to_string()),
        "anotherPublicField should be public (reverted from private)"
    );
}

#[test]
fn test_class_default_private_visibility() {
    let content = r"
class MyClass {
    int defaultField;
    void defaultMethod() {}
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Class members default to private
    assert_eq!(
        find_variable_visibility(&staging, "defaultField"),
        Some("private".to_string()),
        "Class members should default to private"
    );
    assert_eq!(
        find_function_visibility(&staging, "defaultMethod"),
        Some("private".to_string()),
        "Class methods should default to private"
    );
}

#[test]
fn test_struct_default_public_visibility() {
    let content = r"
struct MyStruct {
    int defaultField;
    void defaultMethod() {}
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Struct members default to public
    assert_eq!(
        find_variable_visibility(&staging, "defaultField"),
        Some("public".to_string()),
        "Struct members should default to public"
    );
    assert_eq!(
        find_function_visibility(&staging, "defaultMethod"),
        Some("public".to_string()),
        "Struct methods should default to public"
    );
}

#[test]
fn test_static_function_private_visibility() {
    let content = r"
static int helper() {
    return 42;
}
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Static functions have internal linkage (private)
    let visibility = find_function_visibility(&staging, "helper");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Static functions should have private visibility (internal linkage)"
    );
}

#[test]
fn test_non_static_function_public_visibility() {
    let content = r"
int publicFunction() {
    return 42;
}
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Non-static functions have external linkage (public)
    let visibility = find_function_visibility(&staging, "publicFunction");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Non-static functions should have public visibility (external linkage)"
    );
}

// ==================== Combined Tests ====================

#[test]
fn test_full_example_with_typeof_reference_visibility() {
    let content = r"
class User {
public:
    int id;
    std::string name;

    void setName(const std::string& newName) {
        name = newName;
    }

private:
    std::string password;

    bool verify(const std::string& pwd) {
        return password == pwd;
    }
};
";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify TypeOf edges exist
    let typeof_edges = collect_typeof_edges(&staging);
    assert!(
        typeof_edges.iter().any(|(src, _)| src.contains("id")),
        "Should have TypeOf edge for id field"
    );
    assert!(
        typeof_edges.iter().any(|(src, _)| src.contains("name")),
        "Should have TypeOf edge for name field"
    );
    assert!(
        typeof_edges.iter().any(|(src, _)| src.contains("password")),
        "Should have TypeOf edge for password field"
    );

    // Verify visibility is correct
    assert_eq!(
        find_variable_visibility(&staging, "id"),
        Some("public".to_string()),
        "id should be public"
    );
    assert_eq!(
        find_variable_visibility(&staging, "name"),
        Some("public".to_string()),
        "name should be public"
    );
    assert_eq!(
        find_function_visibility(&staging, "setName"),
        Some("public".to_string()),
        "setName should be public"
    );
    assert_eq!(
        find_variable_visibility(&staging, "password"),
        Some("private".to_string()),
        "password should be private"
    );
    assert_eq!(
        find_function_visibility(&staging, "verify"),
        Some("private".to_string()),
        "verify should be private"
    );
}
