//! TypeOf and Reference edge tests for Scala language plugin.
//!
//! Tests comprehensive coverage of:
//! - Variables and fields (val/var)
//! - Function parameters
//! - Function return types
//! - Complex generic types
//! - Tuple types
//! - Function types
//! - Compound types (intersection types)
//! - Edge cases (type inference, wildcards, etc.)

use sqry_core::graph::{
    GraphBuilder, Language,
    unified::{
        StagingGraph,
        build::staging::StagingOp,
        edge::{EdgeKind, kind::TypeOfContext},
    },
};
use sqry_lang_scala::ScalaGraphBuilder;
use std::{collections::HashMap, path::Path};
use tree_sitter::Parser;

// ============================================================================
// Test Helpers
// ============================================================================

fn parse_scala(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .expect("Failed to set Scala language");
    parser.parse(source, None).expect("Failed to parse Scala")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_scala(source);
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(filename), &mut staging)
        .expect("Failed to build graph");

    staging
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
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

fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
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

fn build_node_display_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = staging
                    .resolve_node_display_name(Language::Scala, entry)?
                    .to_string();
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Collect TypeOf edges with a specific context.
/// Returns vec of (source_name, target_name) pairs.
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    let node_names = build_node_name_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind:
                    EdgeKind::TypeOf {
                        context: ctx,
                        index: _,
                        name: _,
                    },
                ..
            } = op
            {
                if *ctx == Some(context) {
                    let source_name = node_names.get(&source.index())?;
                    let target_name = node_names.get(&target.index())?;
                    Some((source_name.clone(), target_name.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

/// Collect all Reference edges.
/// Returns vec of (source_name, target_name) pairs.
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
            {
                if matches!(kind, EdgeKind::References) {
                    let source_name = node_names.get(&source.index())?;
                    let target_name = node_names.get(&target.index())?;
                    Some((source_name.clone(), target_name.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

fn collect_typeof_edges_by_context_for_display(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    let node_names = build_node_display_name_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind:
                    EdgeKind::TypeOf {
                        context: ctx,
                        index: _,
                        name: _,
                    },
                ..
            } = op
            {
                if *ctx == Some(context) {
                    let source_name = node_names.get(&source.index())?;
                    let target_name = node_names.get(&target.index())?;
                    Some((source_name.clone(), target_name.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

// ============================================================================
// Category 1: Variables and Fields
// ============================================================================

#[test]
fn test_val_simple_type() {
    let source = r#"
class User {
  val name: String = "test"
}
"#;
    let staging = build_test_graph(source, "User.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::name") && tgt == "String"),
        "Expected TypeOf edge for val name: String, got: {typeof_edges:?}"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::name") && tgt == "String"),
        "Expected Reference edge for String, got: {ref_edges:?}"
    );

    let display_typeof_edges =
        collect_typeof_edges_by_context_for_display(&staging, TypeOfContext::Field);
    assert!(
        display_typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with(".name") && tgt == "String"),
        "Expected Scala display TypeOf edge for val name: String, got: {display_typeof_edges:?}"
    );
}

#[test]
fn test_var_simple_type() {
    let source = r#"
class Counter {
  var count: Int = 0
}
"#;
    let staging = build_test_graph(source, "Counter.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::count") && tgt == "Int"),
        "Expected TypeOf edge for var count: Int, got: {typeof_edges:?}"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::count") && tgt == "Int"),
        "Expected Reference edge for Int, got: {ref_edges:?}"
    );
}

#[test]
fn test_private_field_typeof() {
    let source = r#"
class Service {
  private val config: String = "prod"
}
"#;
    let staging = build_test_graph(source, "Service.scala");

    // Private fields should still have TypeOf edges
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::config") && tgt == "String"),
        "Expected TypeOf edge for private field, got: {typeof_edges:?}"
    );
}

// ============================================================================
// Category 2: Function Parameters
// ============================================================================

#[test]
fn test_function_parameter_simple() {
    let source = r#"
def greet(name: String): Unit = println(name)
"#;
    let staging = build_test_graph(source, "Test.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("greet") && tgt == "String"),
        "Expected TypeOf edge for parameter, got: {typeof_edges:?}"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.contains("greet") && tgt == "String"),
        "Expected Reference edge for String, got: {ref_edges:?}"
    );
}

#[test]
fn test_function_multiple_parameters() {
    let source = r#"
def add(x: Int, y: Int): Int = x + y
"#;
    let staging = build_test_graph(source, "Test.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have 2 parameter TypeOf edges
    let param_edges: Vec<_> = typeof_edges
        .iter()
        .filter(|(src, _)| src.contains("add"))
        .collect();
    assert!(
        param_edges.len() >= 2,
        "Expected 2 parameter TypeOf edges, got: {param_edges:?}"
    );
}

#[test]
fn test_method_parameter() {
    let source = r#"
class Calculator {
  def multiply(a: Int, b: Int): Int = a * b
}
"#;
    let staging = build_test_graph(source, "Calculator.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::multiply") && tgt == "Int"),
        "Expected TypeOf edges for method parameters, got: {typeof_edges:?}"
    );
}

#[test]
fn test_implicit_parameter() {
    let source = r#"
def execute(implicit ctx: Context): Unit = ()
"#;
    let staging = build_test_graph(source, "Test.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("execute") && tgt == "Context"),
        "Expected TypeOf edge for implicit parameter, got: {typeof_edges:?}"
    );
}

// ============================================================================
// Category 3: Return Types
// ============================================================================

#[test]
fn test_function_return_simple() {
    let source = r#"
def getCount(): Int = 42
"#;
    let staging = build_test_graph(source, "Test.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("getCount") && tgt == "Int"),
        "Expected TypeOf edge for return type, got: {typeof_edges:?}"
    );

    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.contains("getCount") && tgt == "Int"),
        "Expected Reference edge for Int, got: {ref_edges:?}"
    );
}

#[test]
fn test_function_return_unit() {
    let source = r#"
def doSomething(): Unit = ()
"#;
    let staging = build_test_graph(source, "Test.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("doSomething") && tgt == "Unit"),
        "Expected TypeOf edge for Unit return type, got: {typeof_edges:?}"
    );
}

#[test]
fn test_inferred_return_type() {
    let source = r#"
def getValue() = 42
"#;
    let staging = build_test_graph(source, "Test.scala");

    // Type inference - no explicit return type, so no TypeOf edge expected
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        !typeof_edges.iter().any(|(src, _)| src.contains("getValue")),
        "Should not have TypeOf edge for inferred type"
    );
}

// ============================================================================
// Category 4: Complex Types
// ============================================================================

#[test]
fn test_generic_list_type() {
    let source = r#"
class Container {
  val items: List[String] = List.empty
}
"#;
    let staging = build_test_graph(source, "Container.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::items") && tgt.contains("List")),
        "Expected TypeOf edge for List[String], got: {typeof_edges:?}"
    );

    let ref_edges = collect_reference_edges(&staging);
    // Should have references to both List and String
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::items") && tgt == "List"),
        "Expected Reference to List, got: {ref_edges:?}"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::items") && tgt == "String"),
        "Expected Reference to String, got: {ref_edges:?}"
    );
}

#[test]
fn test_nested_generic() {
    let source = r#"
class Store {
  val cache: Map[String, List[User]] = Map.empty
}
"#;
    let staging = build_test_graph(source, "Store.scala");

    let ref_edges = collect_reference_edges(&staging);
    // Should have references to Map, String, List, and User
    let types: Vec<&str> = ref_edges
        .iter()
        .filter(|(src, _)| src.ends_with("::cache"))
        .map(|(_, tgt)| tgt.as_str())
        .collect();

    assert!(
        types.contains(&"Map"),
        "Expected Reference to Map, got: {types:?}"
    );
    assert!(
        types.contains(&"String"),
        "Expected Reference to String, got: {types:?}"
    );
    assert!(
        types.contains(&"List"),
        "Expected Reference to List, got: {types:?}"
    );
    assert!(
        types.contains(&"User"),
        "Expected Reference to User, got: {types:?}"
    );
}

#[test]
fn test_tuple_type() {
    let source = r#"
class Pair {
  val coords: (Int, Int) = (0, 0)
}
"#;
    let staging = build_test_graph(source, "Pair.scala");

    let ref_edges = collect_reference_edges(&staging);
    // Should have reference to Int (possibly multiple times)
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::coords") && tgt == "Int"),
        "Expected Reference to Int from tuple, got: {ref_edges:?}"
    );
}

#[test]
fn test_function_type() {
    let source = r#"
class Handler {
  val callback: (String, Int) => Boolean = null
}
"#;
    let staging = build_test_graph(source, "Handler.scala");

    let ref_edges = collect_reference_edges(&staging);
    let types: Vec<&str> = ref_edges
        .iter()
        .filter(|(src, _)| src.ends_with("::callback"))
        .map(|(_, tgt)| tgt.as_str())
        .collect();

    // Should have references to String, Int, and Boolean
    assert!(
        types.contains(&"String"),
        "Expected Reference to String, got: {types:?}"
    );
    assert!(
        types.contains(&"Int"),
        "Expected Reference to Int, got: {types:?}"
    );
    assert!(
        types.contains(&"Boolean"),
        "Expected Reference to Boolean, got: {types:?}"
    );
}

#[test]
fn test_compound_type() {
    let source = r#"
class Mixed {
  val obj: Serializable with Cloneable = null
}
"#;
    let staging = build_test_graph(source, "Mixed.scala");

    let ref_edges = collect_reference_edges(&staging);
    let types: Vec<&str> = ref_edges
        .iter()
        .filter(|(src, _)| src.ends_with("::obj"))
        .map(|(_, tgt)| tgt.as_str())
        .collect();

    // Should have references to both Serializable and Cloneable
    assert!(
        types.contains(&"Serializable"),
        "Expected Reference to Serializable, got: {types:?}"
    );
    assert!(
        types.contains(&"Cloneable"),
        "Expected Reference to Cloneable, got: {types:?}"
    );
}

// ============================================================================
// Category 5: Integration Tests
// ============================================================================

#[test]
fn test_class_with_mixed_members() {
    let source = r#"
class UserService {
  val users: List[User] = List.empty
  var count: Int = 0

  def findUser(id: Long): Option[User] = None
}
"#;
    let staging = build_test_graph(source, "UserService.scala");

    // Check field TypeOf edges
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_edges.len() >= 2,
        "Expected at least 2 field TypeOf edges, got: {field_edges:?}"
    );

    // Check parameter TypeOf edges
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        param_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::findUser") && tgt == "Long"),
        "Expected parameter TypeOf edge, got: {param_edges:?}"
    );

    // Check return TypeOf edges
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::findUser") && tgt.contains("Option")),
        "Expected return TypeOf edge, got: {return_edges:?}"
    );
}

#[test]
fn test_multiple_type_references() {
    let source = r#"
def process(data: Map[String, Result[User, Error]]): Boolean = true
"#;
    let staging = build_test_graph(source, "Test.scala");

    let ref_edges = collect_reference_edges(&staging);
    let types: Vec<&str> = ref_edges
        .iter()
        .filter(|(src, _)| src.contains("process"))
        .map(|(_, tgt)| tgt.as_str())
        .collect();

    // Should have references to: Map, String, Result, User, Error, Boolean
    for expected_type in &["Map", "String", "Result", "User", "Error", "Boolean"] {
        assert!(
            types.contains(expected_type),
            "Expected Reference to {expected_type}, got: {types:?}"
        );
    }
}

#[test]
fn test_constructor_parameters() {
    let source = r#"
case class User(name: String, age: Int, email: String)
"#;
    let staging = build_test_graph(source, "User.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    // Case class constructor parameters should have TypeOf edges
    assert!(
        typeof_edges.len() >= 3,
        "Expected at least 3 parameter TypeOf edges for case class, got: {typeof_edges:?}"
    );
}

// ============================================================================
// Category 6: Edge Cases
// ============================================================================

#[test]
fn test_type_alias() {
    let source = r#"
class Handler {
  type UserMap = Map[String, User]
  val cache: UserMap = Map.empty
}
"#;
    let staging = build_test_graph(source, "Handler.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    // Should have TypeOf edge for the alias usage
    assert!(
        typeof_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::cache") && tgt == "UserMap"),
        "Expected TypeOf edge for type alias, got: {typeof_edges:?}"
    );
}

#[test]
fn test_existential_type() {
    let source = r#"
class Wildcards {
  val anyList: List[_] = List.empty
}
"#;
    let staging = build_test_graph(source, "Wildcards.scala");

    let ref_edges = collect_reference_edges(&staging);
    // Should have reference to List, but not to _ (wildcard)
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.ends_with("::anyList") && tgt == "List"),
        "Expected Reference to List, got: {ref_edges:?}"
    );

    // Should NOT have reference to "_"
    assert!(
        !ref_edges.iter().any(|(_, tgt)| tgt == "_"),
        "Should not have Reference to wildcard _"
    );
}

#[test]
fn test_missing_type_annotation() {
    let source = r#"
class Inferred {
  val value = 42
}
"#;
    let staging = build_test_graph(source, "Inferred.scala");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    // Type inference - no explicit type annotation, so no TypeOf edge
    assert!(
        !typeof_edges.iter().any(|(src, _)| src.ends_with(".value")),
        "Should not have TypeOf edge for inferred type"
    );
}
