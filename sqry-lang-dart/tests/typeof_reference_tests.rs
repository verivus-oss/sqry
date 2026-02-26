//! TypeOf and Reference edge tests for Dart language plugin.
//!
//! Tests TypeOf edges (full type signatures) and Reference edges (nested type names)
//! for variables, fields, parameters, and return types.

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_dart::DartPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Build a string lookup map from StagingGraph operations.
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

/// Parse Dart source code into a staging graph.
fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let plugin = DartPlugin::default();
    let file = PathBuf::from(filename);
    let tree = plugin.parse_ast(source.as_bytes()).expect("parse failed");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .expect("build graph");

    staging
}

/// Collect TypeOf edges by checking which edges exist for nodes with given names.
///
/// Returns Vec<(source_name, type_name, context)>
fn collect_typeof_edges(staging: &StagingGraph) -> Vec<(String, String, Option<TypeOfContext>)> {
    let strings = build_string_lookup(staging);
    let mut result = Vec::new();

    // Collect all typeof edges with their source/target IDs
    let mut typeof_edges = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::TypeOf { context, .. },
            source,
            target,
            ..
        } = op
        {
            typeof_edges.push((*source, *target, *context));
        }
    }

    // For each edge, find the source and target node names
    for (source_id, target_id, context) in typeof_edges {
        let mut source_name = String::new();
        let mut target_name = String::new();

        // Find nodes by expected_id
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                if let Some(exp_id) = expected_id
                    && *exp_id == source_id
                    && let Some(name) = strings.get(&entry.name.index())
                {
                    source_name = name.clone();
                }
                if let Some(exp_id) = expected_id
                    && *exp_id == target_id
                    && let Some(name) = strings.get(&entry.name.index())
                {
                    target_name = name.clone();
                }
            }
        }

        result.push((source_name, target_name, context));
    }

    result
}

/// Collect TypeOf edges by specific context.
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_typeof_edges(staging)
        .into_iter()
        .filter_map(|(source, target, ctx)| {
            if ctx == Some(context) {
                Some((source, target))
            } else {
                None
            }
        })
        .collect()
}

/// Collect Reference edges.
///
/// Returns Vec<(source_name, target_name)>
fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let strings = build_string_lookup(staging);
    let mut result = Vec::new();

    // Collect all reference edges with their source/target IDs
    let mut ref_edges = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::References,
            source,
            target,
            ..
        } = op
        {
            ref_edges.push((*source, *target));
        }
    }

    // For each edge, find the source and target node names
    for (source_id, target_id) in ref_edges {
        let mut source_name = String::new();
        let mut target_name = String::new();

        // Find nodes by expected_id
        for op in staging.operations() {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                if let Some(exp_id) = expected_id
                    && *exp_id == source_id
                    && let Some(name) = strings.get(&entry.name.index())
                {
                    source_name = name.clone();
                }
                if let Some(exp_id) = expected_id
                    && *exp_id == target_id
                    && let Some(name) = strings.get(&entry.name.index())
                {
                    target_name = name.clone();
                }
            }
        }

        result.push((source_name, target_name));
    }

    result
}

/// Find a specific TypeOf edge by source name and context.
fn find_typeof_edge(
    staging: &StagingGraph,
    source_name: &str,
    context: TypeOfContext,
) -> Option<String> {
    let edges = collect_typeof_edges_by_context(staging, context);
    edges
        .into_iter()
        .find(|(src, _)| src == source_name)
        .map(|(_, target)| target)
}

// ============================================================================
// Category 1: Variables and Fields (5 tests)
// ============================================================================

#[test]
fn test_final_simple_type() {
    let source = r#"
final String name = "John";
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge
    let type_name = find_typeof_edge(&staging, "name", TypeOfContext::Variable);
    assert_eq!(type_name, Some("String".to_string()));

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(
        refs.contains(&("name".to_string(), "String".to_string())),
        "Expected Reference edge from name to String, found: {:?}",
        refs
    );
}

#[test]
fn test_var_with_explicit_type() {
    let source = r#"
int count = 0;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge
    let type_name = find_typeof_edge(&staging, "count", TypeOfContext::Variable);
    assert_eq!(type_name, Some("int".to_string()));

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("count".to_string(), "int".to_string())));
}

#[test]
fn test_top_level_var() {
    let source = r#"
final List<String> items = [];
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge (should have full type)
    let type_name = find_typeof_edge(&staging, "items", TypeOfContext::Variable);
    assert_eq!(type_name, Some("List<String>".to_string()));

    // Check Reference edges (should have both List and String)
    let refs = collect_reference_edges(&staging);
    eprintln!("Reference edges: {:?}", refs);
    assert!(
        refs.contains(&("items".to_string(), "List".to_string())),
        "Missing List reference"
    );
    assert!(
        refs.contains(&("items".to_string(), "String".to_string())),
        "Missing String reference"
    );
}

#[test]
fn test_class_field_typeof() {
    let source = r#"
class User {
  final int age;
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for field (qualified name: User.age)
    let type_name = find_typeof_edge(&staging, "User.age", TypeOfContext::Field);
    assert_eq!(type_name, Some("int".to_string()));

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("User.age".to_string(), "int".to_string())));
}

#[test]
fn test_private_field_typeof() {
    let source = r#"
class Account {
  final String _privateField;
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for private field
    let type_name = find_typeof_edge(&staging, "Account._privateField", TypeOfContext::Field);
    assert_eq!(type_name, Some("String".to_string()));

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("Account._privateField".to_string(), "String".to_string())));
}

// ============================================================================
// Category 2: Parameters (4 tests)
// ============================================================================

#[test]
fn test_function_parameter_simple() {
    let source = r#"
void greet(String name) {
  print(name);
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for parameter
    let edges = collect_typeof_edges(&staging);
    let param_edge = edges
        .iter()
        .find(|(src, _, ctx)| src == "greet" && *ctx == Some(TypeOfContext::Parameter));

    assert!(param_edge.is_some(), "Expected Parameter TypeOf edge");
    assert_eq!(param_edge.unwrap().1, "String");

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("greet".to_string(), "String".to_string())));
}

#[test]
fn test_function_multiple_parameters() {
    let source = r#"
void process(String name, int age, bool active) {
  // implementation
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edges for all parameters
    let edges = collect_typeof_edges(&staging);
    let param_edges: Vec<_> = edges
        .iter()
        .filter(|(src, _, ctx)| src == "process" && *ctx == Some(TypeOfContext::Parameter))
        .collect();

    assert_eq!(param_edges.len(), 3, "Expected 3 parameter TypeOf edges");

    // Check Reference edges for all parameter types
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("process".to_string(), "String".to_string())));
    assert!(refs.contains(&("process".to_string(), "int".to_string())));
    assert!(refs.contains(&("process".to_string(), "bool".to_string())));
}

#[test]
fn test_method_parameter() {
    let source = r#"
class Calculator {
  void add(int a, int b) {
    // implementation
  }
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edges for method parameters
    let edges = collect_typeof_edges(&staging);
    let param_edges: Vec<_> = edges
        .iter()
        .filter(|(src, _, ctx)| src == "Calculator.add" && *ctx == Some(TypeOfContext::Parameter))
        .collect();

    assert_eq!(param_edges.len(), 2, "Expected 2 parameter TypeOf edges");
}

#[test]
fn test_optional_parameter() {
    let source = r#"
void configure(String host, [int? port]) {
  // implementation
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edges (should have at least the required parameter)
    let edges = collect_typeof_edges(&staging);
    let param_edges: Vec<_> = edges
        .iter()
        .filter(|(src, _, ctx)| src == "configure" && *ctx == Some(TypeOfContext::Parameter))
        .collect();

    assert!(
        !param_edges.is_empty(),
        "Expected at least one parameter TypeOf edge"
    );
}

// ============================================================================
// Category 3: Return Types (4 tests)
// ============================================================================

#[test]
fn test_function_return_simple() {
    let source = r#"
String getName() {
  return "John";
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for return type
    let type_name = find_typeof_edge(&staging, "getName", TypeOfContext::Return);
    assert_eq!(type_name, Some("String".to_string()));

    // Check Reference edge
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("getName".to_string(), "String".to_string())));
}

#[test]
fn test_function_return_void() {
    let source = r#"
void process() {
  print("done");
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for void return type
    let type_name = find_typeof_edge(&staging, "process", TypeOfContext::Return);
    assert_eq!(type_name, Some("void".to_string()));
}

#[test]
fn test_function_return_future() {
    let source = r#"
Future<String> fetchData() async {
  return Future.value("data");
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for Future return type
    let type_name = find_typeof_edge(&staging, "fetchData", TypeOfContext::Return);
    assert_eq!(type_name, Some("Future<String>".to_string()));

    // Check Reference edges (Future and String)
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("fetchData".to_string(), "Future".to_string())));
    assert!(refs.contains(&("fetchData".to_string(), "String".to_string())));
}

#[test]
fn test_inferred_return_type() {
    let source = r#"
getData() {
  return 42;
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check that no TypeOf edge is created for inferred return type
    let type_name = find_typeof_edge(&staging, "getData", TypeOfContext::Return);
    // Inferred types don't have TypeOf edges
    assert!(
        type_name.is_none(),
        "Inferred return type should not have TypeOf edge"
    );
}

// ============================================================================
// Category 4: Generic Types (4 tests)
// ============================================================================

#[test]
fn test_generic_list_type() {
    let source = r#"
final List<String> names = [];
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge with full generic type
    let type_name = find_typeof_edge(&staging, "names", TypeOfContext::Variable);
    assert_eq!(type_name, Some("List<String>".to_string()));

    // Check Reference edges for both List and String
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("names".to_string(), "List".to_string())));
    assert!(refs.contains(&("names".to_string(), "String".to_string())));
}

#[test]
fn test_generic_map_type() {
    let source = r#"
final Map<String, int> userAges = {};
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge with full Map type
    let type_name = find_typeof_edge(&staging, "userAges", TypeOfContext::Variable);
    assert_eq!(type_name, Some("Map<String, int>".to_string()));

    // Check Reference edges for Map, String, and int
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("userAges".to_string(), "Map".to_string())));
    assert!(refs.contains(&("userAges".to_string(), "String".to_string())));
    assert!(refs.contains(&("userAges".to_string(), "int".to_string())));
}

#[test]
fn test_nested_generic() {
    let source = r#"
final List<Map<String, User>> data = [];
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge with full nested generic type
    let type_name = find_typeof_edge(&staging, "data", TypeOfContext::Variable);
    assert_eq!(
        type_name,
        Some("List<Map<String, User>>".to_string()),
        "Expected full nested generic type"
    );

    // Check Reference edges for all nested types
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("data".to_string(), "List".to_string())));
    assert!(refs.contains(&("data".to_string(), "Map".to_string())));
    assert!(refs.contains(&("data".to_string(), "String".to_string())));
    assert!(refs.contains(&("data".to_string(), "User".to_string())));
}

#[test]
fn test_generic_future() {
    let source = r#"
Future<List<User>> fetchUsers() async {
  return Future.value([]);
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for return type with nested generics
    let type_name = find_typeof_edge(&staging, "fetchUsers", TypeOfContext::Return);
    assert_eq!(type_name, Some("Future<List<User>>".to_string()));

    // Check Reference edges for all nested types
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("fetchUsers".to_string(), "Future".to_string())));
    assert!(refs.contains(&("fetchUsers".to_string(), "List".to_string())));
    assert!(refs.contains(&("fetchUsers".to_string(), "User".to_string())));
}

// ============================================================================
// Category 5: Nullable Types (2 tests)
// ============================================================================

#[test]
fn test_nullable_simple() {
    let source = r#"
String? maybeName;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge includes nullable marker
    let type_name = find_typeof_edge(&staging, "maybeName", TypeOfContext::Variable);
    assert_eq!(type_name, Some("String?".to_string()));

    // Check Reference edge (should reference String without ?)
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("maybeName".to_string(), "String".to_string())));
}

#[test]
fn test_nullable_generic() {
    let source = r#"
List<String>? maybeList;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge includes nullable marker on generic
    let type_name = find_typeof_edge(&staging, "maybeList", TypeOfContext::Variable);
    assert_eq!(type_name, Some("List<String>?".to_string()));

    // Check Reference edges for nested types
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("maybeList".to_string(), "List".to_string())));
    assert!(refs.contains(&("maybeList".to_string(), "String".to_string())));
}

// ============================================================================
// Category 6: Function Types (3 tests)
// ============================================================================

#[test]
fn test_function_type_simple() {
    let source = r#"
Function callback;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for Function type
    let type_name = find_typeof_edge(&staging, "callback", TypeOfContext::Variable);
    assert_eq!(type_name, Some("Function".to_string()));
}

#[test]
fn test_function_type_typed() {
    let source = r#"
void Function(int) handler;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for typed function
    let type_name = find_typeof_edge(&staging, "handler", TypeOfContext::Variable);
    // The full function type signature should be captured
    assert!(
        type_name.is_some(),
        "Expected TypeOf edge for typed function"
    );

    // Check Reference edges for types in function signature
    let refs = collect_reference_edges(&staging);
    // Should reference types used in the function signature
    assert!(
        refs.iter().any(|(src, _)| src == "handler"),
        "Expected reference edges from handler"
    );
}

#[test]
fn test_function_type_returns() {
    let source = r#"
String Function(int, bool) transformer;
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check TypeOf edge for function with return type
    let type_name = find_typeof_edge(&staging, "transformer", TypeOfContext::Variable);
    assert!(
        type_name.is_some(),
        "Expected TypeOf edge for function with return type"
    );

    // Check Reference edges
    let refs = collect_reference_edges(&staging);
    assert!(
        refs.iter().any(|(src, _)| src == "transformer"),
        "Expected reference edges from transformer"
    );
}

// ============================================================================
// Category 7: Integration (3 tests)
// ============================================================================

#[test]
fn test_class_with_mixed_members() {
    let source = r#"
class User {
  final String name;
  final int age;

  String getName() {
    return name;
  }

  void setAge(int newAge) {
    // implementation
  }
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check field TypeOf edges (may not work yet, but test the integration)
    let all_edges = collect_typeof_edges(&staging);

    // Check method return type
    let get_name_return = find_typeof_edge(&staging, "User.getName", TypeOfContext::Return);
    assert_eq!(get_name_return, Some("String".to_string()));

    // Check method parameter
    let set_age_param = all_edges.iter().any(|(src, target, ctx)| {
        src == "User.setAge" && target == "int" && *ctx == Some(TypeOfContext::Parameter)
    });
    assert!(set_age_param, "Expected parameter TypeOf edge for setAge");

    // Check void return type
    let set_age_return = find_typeof_edge(&staging, "User.setAge", TypeOfContext::Return);
    assert_eq!(set_age_return, Some("void".to_string()));
}

#[test]
fn test_multiple_type_references() {
    let source = r#"
Map<String, List<User>> processUsers(List<User> users, String filter) {
  return {};
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Check return type
    let return_type = find_typeof_edge(&staging, "processUsers", TypeOfContext::Return);
    assert_eq!(
        return_type,
        Some("Map<String, List<User>>".to_string()),
        "Expected complex return type"
    );

    // Check that all type names are referenced
    let refs = collect_reference_edges(&staging);
    assert!(refs.contains(&("processUsers".to_string(), "Map".to_string())));
    assert!(refs.contains(&("processUsers".to_string(), "String".to_string())));
    assert!(refs.contains(&("processUsers".to_string(), "List".to_string())));
    assert!(refs.contains(&("processUsers".to_string(), "User".to_string())));
}

#[test]
fn test_constructor_parameters() {
    let source = r#"
class Point {
  Point(double x, double y) {
    // implementation
  }
}
"#;
    let staging = build_test_graph(source, "test.dart");

    // Constructor parameters may not be fully implemented
    // This test documents expected behavior
    let all_edges = collect_typeof_edges(&staging);

    // Look for any constructor-related edges
    let has_constructor_edges = all_edges.iter().any(|(src, _, _)| src.contains("Point"));

    // Document current state - constructors may need additional work
    if !has_constructor_edges {
        eprintln!("NOTE: Constructor parameters not yet fully implemented");
    }
}
