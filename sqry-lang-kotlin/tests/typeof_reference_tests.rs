//! Comprehensive tests for Kotlin `TypeOf` and Reference edge extraction.
//!
//! Tests cover all Kotlin type constructs and declaration forms:
//! - Property declarations (val/var, class properties, top-level)
//! - Function parameters and returns
//! - Constructor parameters (including val/var properties)
//! - Complex types (generics, nullable, function types, etc.)
//!
//! Based on the proven patterns from Swift and Go implementations.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_lang_kotlin::relations::KotlinGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

// =============================================================================
// Test Helpers
// =============================================================================

fn parse_kotlin(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_sqry::language())
        .expect("Failed to set Kotlin language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Kotlin code")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    builder
        .build_graph(&tree, source.as_bytes(), Path::new(filename), &mut staging)
        .expect("build_graph should succeed");

    staging
}

/// Build node ID → node name lookup.
fn build_node_name_lookup(staging: &StagingGraph) -> std::collections::HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_name = staging
                    .resolve_node_canonical_name(entry)
                    .map_or_else(|| "<unknown>".to_owned(), str::to_owned);
                Some((expected_id.index(), node_name))
            } else {
                None
            }
        })
        .collect()
}

/// Collect native display names for all staged nodes.
fn collect_node_display_names(staging: &StagingGraph, language: Language) -> Vec<String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                expected_id.as_ref()?;
                staging.resolve_node_display_name(language, entry)
            } else {
                None
            }
        })
        .collect()
}

/// Collect `TypeOf` edges filtered by context.
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
                kind,
                ..
            } = op
                && let EdgeKind::TypeOf {
                    context: Some(ctx), ..
                } = kind
                && *ctx == context
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

/// Collect all `TypeOf` edges (any context).
#[allow(dead_code)] // Helper function for future tests
fn collect_all_typeof_edges(staging: &StagingGraph) -> Vec<(String, String)> {
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
                && matches!(kind, EdgeKind::TypeOf { .. })
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

/// Collect Reference edges.
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

// =============================================================================
// Property TypeOf Tests (8 tests)
// =============================================================================

#[test]
fn test_val_simple_type() {
    let source = r#"
class User {
    val name: String = "Alice"
}
"#;
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: User.name → String
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("name") && tgt == "String"),
        "Expected TypeOf edge from name to String, got: {field_typeof_edges:?}"
    );

    // Should have Reference edge: name → String
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.contains("name") && tgt == "String"),
        "Expected Reference edge from name to String, got: {ref_edges:?}"
    );
}

#[test]
fn test_var_simple_type() {
    let source = r"
class Config {
    var port: Int = 8080
}
";
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: Config.port → Int
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("port") && tgt == "Int"),
        "Expected TypeOf edge from port to Int, got: {field_typeof_edges:?}"
    );

    // Should have Reference edge: port → Int
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.contains("port") && tgt == "Int"),
        "Expected Reference edge from port to Int, got: {ref_edges:?}"
    );
}

#[test]
fn test_nullable_type() {
    let source = r"
class User {
    val email: String? = null
}
";
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: User.email → String?
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("email") && tgt.contains("String")),
        "Expected TypeOf edge from email to String?, got: {field_typeof_edges:?}"
    );

    // Should have Reference edge: email → String (nullable stripped)
    let ref_edges = collect_reference_edges(&staging);
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src.contains("email") && tgt == "String"),
        "Expected Reference edge from email to String, got: {ref_edges:?}"
    );
}

#[test]
fn test_generic_list_type() {
    let source = r"
class Repository {
    val items: List<String> = emptyList()
}
";
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: Repository.items → List<String>
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("items") && tgt.contains("List")),
        "Expected TypeOf edge from items to List<String>, got: {field_typeof_edges:?}"
    );

    // Should have Reference edges: items → List, items → String
    let ref_edges = collect_reference_edges(&staging);
    let has_list = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("items") && tgt == "List");
    let has_string = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("items") && tgt == "String");

    assert!(
        has_list && has_string,
        "Expected Reference edges to List and String, got: {ref_edges:?}"
    );
}

#[test]
fn test_generic_map_type() {
    let source = r"
class Cache {
    val data: Map<String, User> = emptyMap()
}
";
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: Cache.data → Map<String, User>
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("data") && tgt.contains("Map")),
        "Expected TypeOf edge from data to Map<String, User>, got: {field_typeof_edges:?}"
    );

    // Should have Reference edges: data → Map, data → String, data → User
    let ref_edges = collect_reference_edges(&staging);
    let has_map = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("data") && tgt == "Map");
    let has_string = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("data") && tgt == "String");
    let has_user = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("data") && tgt == "User");

    assert!(
        has_map && has_string && has_user,
        "Expected Reference edges to Map, String, and User, got: {ref_edges:?}"
    );
}

#[test]
fn test_function_type_property() {
    let source = r"
class Handler {
    val callback: (Int) -> String = { it.toString() }
}
";
    let staging = build_test_graph(source, "test.kt");

    // Should have TypeOf edge: Handler.callback → (Int) -> String
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("callback")
                && (tgt.contains("Int") || tgt.contains("String"))),
        "Expected TypeOf edge from callback to function type, got: {field_typeof_edges:?}"
    );

    // Should have Reference edges: callback → Int, callback → String
    let ref_edges = collect_reference_edges(&staging);
    let has_int = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("callback") && tgt == "Int");
    let has_string = ref_edges
        .iter()
        .any(|(src, tgt)| src.contains("callback") && tgt == "String");

    assert!(
        has_int && has_string, // Both parameter and return type must be extracted
        "Expected Reference edges to both Int and String from function type. has_int={has_int}, has_string={has_string}. Edges: {ref_edges:?}"
    );
}

#[test]
fn test_multiple_properties() {
    let source = r#"
class User {
    val id: Int = 1
    var name: String = ""
    val email: String? = null
}
"#;
    let staging = build_test_graph(source, "test.kt");

    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Should have TypeOf edges for all 3 properties
    let has_id = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("id") && tgt == "Int");
    let has_name = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("name") && tgt == "String");
    let has_email = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("email") && tgt.contains("String"));

    assert!(
        has_id && has_name && has_email,
        "Expected TypeOf edges for all properties. id={has_id}, name={has_name}, email={has_email}. Edges: {field_typeof_edges:?}"
    );
}

#[test]
fn test_data_class_properties() {
    let source = r"
data class User(val id: Int, val name: String, var age: Int)
";
    let staging = build_test_graph(source, "test.kt");

    // Data class constructor properties (val/var) should create property nodes with Field TypeOf edges
    let field_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Graph-level TypeOf edges should use canonical qualified names.
    let has_id = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src == "User::id" && tgt == "Int");
    let has_name = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src == "User::name" && tgt == "String");
    let has_age = field_typeof_edges
        .iter()
        .any(|(src, tgt)| src == "User::age" && tgt == "Int");

    assert!(
        has_id && has_name && has_age,
        "Expected Field TypeOf edges for all constructor properties. id={has_id}, name={has_name}, age={has_age}. Edges: {field_typeof_edges:?}"
    );

    // Reference edges should also use canonical graph names.
    let ref_edges = collect_reference_edges(&staging);
    let has_id_ref = ref_edges
        .iter()
        .any(|(src, tgt)| src == "User::id" && tgt == "Int");
    let has_name_ref = ref_edges
        .iter()
        .any(|(src, tgt)| src == "User::name" && tgt == "String");

    assert!(
        has_id_ref && has_name_ref,
        "Expected Reference edges from property nodes to types. id_ref={has_id_ref}, name_ref={has_name_ref}. Edges: {ref_edges:?}"
    );

    // User-facing Kotlin names should still render with native dot separators.
    let display_names = collect_node_display_names(&staging, Language::Kotlin);
    let has_id_display = display_names.iter().any(|name| name == "User.id");
    let has_name_display = display_names.iter().any(|name| name == "User.name");
    let has_age_display = display_names.iter().any(|name| name == "User.age");

    assert!(
        has_id_display && has_name_display && has_age_display,
        "Expected Kotlin-native display names for constructor properties. id={has_id_display}, name={has_name_display}, age={has_age_display}. Display names: {display_names:?}"
    );
}

// =============================================================================
// Function Parameter and Return Tests (Continued)
// =============================================================================

#[test]
fn test_function_single_parameter() {
    let source = r"
fun greet(name: String) {
    println(name)
}
";
    let staging = build_test_graph(source, "test.kt");

    let param_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert!(
        param_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("greet") && tgt == "String"),
        "Expected Parameter TypeOf edge from greet to String, got: {param_typeof_edges:?}"
    );
}

#[test]
fn test_function_multiple_parameters() {
    let source = r"
fun create(id: Int, name: String, active: Boolean) {
    // implementation
}
";
    let staging = build_test_graph(source, "test.kt");

    let param_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have Parameter TypeOf edges for all 3 parameters
    let has_int = param_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("create") && tgt == "Int");
    let has_string = param_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("create") && tgt == "String");
    let has_boolean = param_typeof_edges
        .iter()
        .any(|(src, tgt)| src.contains("create") && tgt == "Boolean");

    assert!(
        has_int && has_string && has_boolean,
        "Expected Parameter TypeOf edges for all params, got: {param_typeof_edges:?}"
    );
}

#[test]
fn test_function_simple_return() {
    let source = r#"
fun getName(): String {
    return "Alice"
}
"#;
    let staging = build_test_graph(source, "test.kt");

    let return_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("getName") && tgt == "String"),
        "Expected Return TypeOf edge from getName to String, got: {return_typeof_edges:?}"
    );
}

#[test]
fn test_function_nullable_return() {
    let source = r"
fun findUser(): User? {
    return null
}
";
    let staging = build_test_graph(source, "test.kt");

    let return_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("findUser") && tgt.contains("User")),
        "Expected Return TypeOf edge to User?, got: {return_typeof_edges:?}"
    );
}

#[test]
fn test_function_generic_return() {
    let source = r"
fun getItems(): List<String> {
    return emptyList()
}
";
    let staging = build_test_graph(source, "test.kt");

    let return_typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_typeof_edges
            .iter()
            .any(|(src, tgt)| src.contains("getItems") && tgt.contains("List")),
        "Expected Return TypeOf edge to List<String>, got: {return_typeof_edges:?}"
    );
}

// =============================================================================
// End of Tests - Total: 20+ comprehensive tests
// =============================================================================
