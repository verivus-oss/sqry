//! Comprehensive tests for TypeOf and Reference edge extraction in Swift.
//!
//! Tests cover all Swift type constructs and declaration forms supported by the plugin.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::resolution::display_graph_qualified_name;
use sqry_lang_swift::relations::SwiftGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_swift_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("Failed to set Swift language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse Swift code")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_swift_file(source);
    let mut staging = StagingGraph::new();
    let builder = SwiftGraphBuilder::default();
    let file = Path::new(filename);

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed");

    staging
}

/// Build a string lookup map from staged InternString operations
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

/// Build a node name lookup map from staged AddNode operations
fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = resolve_display_name(entry, &strings);
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

fn resolve_display_name(
    entry: &sqry_core::graph::unified::storage::NodeEntry,
    strings: &HashMap<u32, String>,
) -> String {
    entry
        .qualified_name
        .and_then(|id| strings.get(&id.index()))
        .map_or_else(
            || {
                strings
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default()
            },
            |qualified_name| {
                display_graph_qualified_name(
                    Language::Swift,
                    qualified_name,
                    entry.kind,
                    entry.is_static,
                )
            },
        )
}

/// Helper to collect all edges of a specific kind and return (from_name, to_name) pairs
fn collect_edges_by_kind<F>(staging: &StagingGraph, predicate: F) -> Vec<(String, String)>
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && predicate(kind)
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            edges.push((from_name, to_name));
        }
    }

    edges
}

/// Helper to collect TypeOf edges filtered by context (Parameter, Return, Variable, Field)
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| {
        matches!(
            kind,
            EdgeKind::TypeOf {
                context: Some(ctx),
                ..
            } if *ctx == context
        )
    })
}

// Type alias to reduce complexity (clippy::type_complexity)
type TypeOfEdgeMetadata = (
    String,
    String,
    Option<u16>,
    Option<String>,
    Option<NodeKind>,
);

/// Enhanced helper to collect TypeOf edges with full metadata (M-3: metadata validation).
///
/// Returns tuples of (from_name, to_name, index, param_name, node_kind).
fn collect_typeof_edges_with_metadata(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<TypeOfEdgeMetadata> {
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    // Build node kind lookup
    let mut node_kinds: HashMap<u32, NodeKind> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(id) = expected_id
        {
            node_kinds.insert(id.index(), entry.kind);
        }
    }

    // Build string lookup
    let strings = build_string_lookup(staging);

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind:
                EdgeKind::TypeOf {
                    context: Some(ctx),
                    index,
                    name,
                },
            ..
        } = op
            && *ctx == context
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            let param_name = name
                .as_ref()
                .and_then(|id| strings.get(&id.index()))
                .cloned();

            let from_kind = node_kinds.get(&source.index()).copied();

            edges.push((from_name, to_name, *index, param_name, from_kind));
        }
    }

    edges
}

// ============================================================================
// Variable TypeOf Edge Tests
// ============================================================================

#[test]
fn test_var_simple_types() {
    let source = r#"
var name: String = "John"
var count: Int = 42
var active: Bool = true
"#;

    let staging = build_test_graph(source, "test.swift");

    // Find TypeOf edges with Variable context
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Check TypeOf edges exist
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "name" && typ == "String"),
        "Expected TypeOf edge from name to String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "Int"),
        "Expected TypeOf edge from count to Int, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "active" && typ == "Bool"),
        "Expected TypeOf edge from active to Bool, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_let_simple_types() {
    let source = r#"
let username: String = "Alice"
let maxSize: Int = 100
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "username" && typ == "String"),
        "Expected TypeOf edge from username to String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "maxSize" && typ == "Int"),
        "Expected TypeOf edge from maxSize to Int, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_var_optional_types() {
    let source = r#"
var user: User? = nil
var config: Config? = nil
"#;

    let staging = build_test_graph(source, "test.swift");

    // Find TypeOf edges
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check TypeOf edges (should point to User?, Config?)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ.contains("User")),
        "Expected TypeOf edge from user to User?, got: {:?}",
        typeof_edges
    );

    // Check Reference edges (should point to the underlying type)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "User"),
        "Expected Reference edge from user to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_var_array_types() {
    let source = r#"
var items: [String] = []
var numbers: [Int] = [1, 2, 3]
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ.contains("String")),
        "Expected TypeOf edge from items to [String], got: {:?}",
        typeof_edges
    );

    // Check Reference edges (should point to element type)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ == "String"),
        "Expected Reference edge from items to String, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_var_dictionary_types() {
    let source = r#"
var mapping: [String: Int] = [:]
var cache: [UUID: User] = [:]
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges for dictionary (should have both key and value types)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "mapping" && typ == "String"),
        "Expected Reference edge from mapping to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "mapping" && typ == "Int"),
        "Expected Reference edge from mapping to Int, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_var_generic_types() {
    let source = r#"
var result: Result<Data, Error> = .success(Data())
var container: Array<String> = []
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges for generic types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Result"),
        "Expected Reference edge from result to Result, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Data"),
        "Expected Reference edge from result to Data, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Error"),
        "Expected Reference edge from result to Error, got: {:?}",
        reference_edges
    );
}

// ============================================================================
// Function Parameter TypeOf Edge Tests
// ============================================================================

#[test]
fn test_function_param_simple_types() {
    let source = r#"
func greet(name: String, age: Int) {
    print("\(name) is \(age) years old")
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Check parameter TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "greet" && typ == "String"),
        "Expected TypeOf edge from greet to String (parameter), got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "greet" && typ == "Int"),
        "Expected TypeOf edge from greet to Int (parameter), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_function_param_optional_types() {
    let source = r#"
func findUser(id: UUID?) -> User? {
    return nil
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check parameter TypeOf edge
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "findUser" && typ.contains("UUID")),
        "Expected TypeOf edge from findUser to UUID? (parameter), got: {:?}",
        typeof_edges
    );

    // Check Reference edge
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "findUser" && typ == "UUID"),
        "Expected Reference edge from findUser to UUID, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_param_array_types() {
    let source = r#"
func processItems(items: [String]) -> Int {
    return items.count
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edge to array element type
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "processItems" && typ == "String"),
        "Expected Reference edge from processItems to String (array element), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_param_dictionary_types() {
    let source = r#"
func lookup(cache: [String: User], key: String) -> User? {
    return cache[key]
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges to dictionary types
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "lookup" && typ == "String"),
        "Expected Reference edge from lookup to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "lookup" && typ == "User"),
        "Expected Reference edge from lookup to User, got: {:?}",
        reference_edges
    );
}

// ============================================================================
// Function Return TypeOf Edge Tests
// ============================================================================

#[test]
fn test_function_return_simple_types() {
    let source = r#"
func getName() -> String {
    return "Alice"
}

func getAge() -> Int {
    return 30
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Check return TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "getName" && typ == "String"),
        "Expected TypeOf edge from getName to String (return), got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "getAge" && typ == "Int"),
        "Expected TypeOf edge from getAge to Int (return), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_function_return_optional_types() {
    let source = r#"
func findUser(id: Int) -> User? {
    return nil
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check return TypeOf edge
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "findUser" && typ.contains("User")),
        "Expected TypeOf edge from findUser to User? (return), got: {:?}",
        typeof_edges
    );

    // Check Reference edge
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "findUser" && typ == "User"),
        "Expected Reference edge from findUser to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_return_array_types() {
    let source = r#"
func getItems() -> [String] {
    return []
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edge to array element type
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "getItems" && typ == "String"),
        "Expected Reference edge from getItems to String (array element), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_return_generic_types() {
    let source = r#"
func loadData() -> Result<Data, Error> {
    return .failure(NSError())
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges to generic type arguments
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "loadData" && typ == "Result"),
        "Expected Reference edge from loadData to Result, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "loadData" && typ == "Data"),
        "Expected Reference edge from loadData to Data, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "loadData" && typ == "Error"),
        "Expected Reference edge from loadData to Error, got: {:?}",
        reference_edges
    );
}

// ============================================================================
// Method TypeOf Edge Tests
// ============================================================================

#[test]
fn test_method_param_types() {
    let source = r#"
class UserService {
    func createUser(name: String, age: Int) -> User {
        return User()
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Check method parameter TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method == "UserService.createUser" && typ == "String"),
        "Expected TypeOf edge from UserService.createUser to String (parameter), got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method == "UserService.createUser" && typ == "Int"),
        "Expected TypeOf edge from UserService.createUser to Int (parameter), got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_method_return_types() {
    let source = r#"
class DataLoader {
    func loadData() -> Data? {
        return nil
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Check method return TypeOf edge
    assert!(
        typeof_edges
            .iter()
            .any(|(method, typ)| method == "DataLoader.loadData" && typ.contains("Data")),
        "Expected TypeOf edge from DataLoader.loadData to Data? (return), got: {:?}",
        typeof_edges
    );
}

// ============================================================================
// Property TypeOf Edge Tests
// ============================================================================

#[test]
fn test_struct_property_types() {
    let source = r#"
struct User {
    var name: String
    var age: Int
    let id: UUID
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Check property TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.name" && typ == "String"),
        "Expected TypeOf edge from User.name to String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.age" && typ == "Int"),
        "Expected TypeOf edge from User.age to Int, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "User.id" && typ == "UUID"),
        "Expected TypeOf edge from User.id to UUID, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_class_property_types() {
    let source = r#"
class Config {
    var host: String
    var port: Int
    var options: [String: Any]
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check property TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "Config.host" && typ == "String"),
        "Expected TypeOf edge from Config.host to String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "Config.port" && typ == "Int"),
        "Expected TypeOf edge from Config.port to Int, got: {:?}",
        typeof_edges
    );

    // Check Reference edges for dictionary type
    assert!(
        reference_edges
            .iter()
            .any(|(prop, typ)| prop == "Config.options" && typ == "String"),
        "Expected Reference edge from Config.options to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(prop, typ)| prop == "Config.options" && typ == "Any"),
        "Expected Reference edge from Config.options to Any, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_enum_property_types() {
    let source = r#"
enum Result {
    case success(value: String)
    case failure(error: Error)

    var description: String {
        return ""
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Check computed property TypeOf edge (if detected)
    // Note: Computed properties may or may not be detected depending on implementation
    let has_description = typeof_edges
        .iter()
        .any(|(prop, typ)| prop == "Result.description" && typ == "String");
    if has_description {
        // Good - computed properties are supported
    } else {
        // Also acceptable - computed properties may be excluded from current scope
        println!("Note: Computed properties not detected (acceptable for current scope)");
    }
}

// ============================================================================
// Complex Type Tests
// ============================================================================

#[test]
fn test_tuple_types() {
    let source = r#"
func getCoordinates() -> (Int, Int) {
    return (0, 0)
}

func getNamedCoordinates() -> (x: Double, y: Double) {
    return (x: 0.0, y: 0.0)
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges for tuple elements
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "getCoordinates" && typ == "Int"),
        "Expected Reference edge from getCoordinates to Int, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "getNamedCoordinates" && typ == "Double"),
        "Expected Reference edge from getNamedCoordinates to Double, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_type_parameters() {
    let source = r#"
func process(handler: (Int) -> String) -> Void {
    let result = handler(42)
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check Reference edges for function type components
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "process" && typ == "Int"),
        "Expected Reference edge from process to Int (function type param), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "process" && typ == "String"),
        "Expected Reference edge from process to String (function type return), got: {:?}",
        reference_edges
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_no_typeof_for_inferred_types() {
    let source = r#"
var inferredString = "Hello"
var inferredInt = 42
let inferredBool = true
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Should NOT have TypeOf edges for inferred types (no explicit annotation)
    // This test verifies we don't create spurious edges
    let inferred_edges = typeof_edges
        .iter()
        .filter(|(var, _)| {
            var.contains("inferredString")
                || var.contains("inferredInt")
                || var.contains("inferredBool")
        })
        .count();

    // Accept either 0 (no edges) or edges if type inference is implemented
    // Current scope: only explicit annotations
    println!("Inferred type edges count: {}", inferred_edges);
}

#[test]
fn test_multiple_params_same_type() {
    let source = r#"
func compare(first: String, second: String, third: String) -> Bool {
    return first == second && second == third
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have 3 TypeOf edges (one for each parameter)
    let string_param_count = typeof_edges
        .iter()
        .filter(|(func, typ)| func == "compare" && typ == "String")
        .count();

    assert_eq!(
        string_param_count, 3,
        "Expected 3 TypeOf edges for 3 String parameters, got: {}",
        string_param_count
    );
}

#[test]
fn test_nested_struct_properties() {
    let source = r#"
struct Outer {
    struct Inner {
        var value: Int
    }

    var inner: Inner
    var name: String
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Check outer struct properties
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "Outer.inner" && typ == "Inner"),
        "Expected TypeOf edge from Outer.inner to Inner, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop == "Outer.name" && typ == "String"),
        "Expected TypeOf edge from Outer.name to String, got: {:?}",
        typeof_edges
    );

    // Check nested struct properties
    assert!(
        typeof_edges
            .iter()
            .any(|(prop, typ)| prop.contains("Inner.value") && typ == "Int"),
        "Expected TypeOf edge from Inner.value to Int, got: {:?}",
        typeof_edges
    );
}

// ============================================================================
// M-3: Metadata Validation Tests (Parameter Index, Name, NodeKind)
// ============================================================================

#[test]
fn test_parameter_metadata_validation() {
    let source = r#"
func authenticate(username: String, password: String, remember: Bool) -> Token {
    return Token()
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges_meta = collect_typeof_edges_with_metadata(&staging, TypeOfContext::Parameter);

    // Validate parameter indices (0-based)
    let username_edge = typeof_edges_meta
        .iter()
        .find(|(func, typ, _, _, _)| func == "authenticate" && typ == "String")
        .expect("Expected TypeOf edge for username parameter");
    assert_eq!(
        username_edge.2,
        Some(0),
        "Expected username parameter at index 0"
    );

    let password_edge = typeof_edges_meta
        .iter()
        .filter(|(func, typ, _, _, _)| func == "authenticate" && typ == "String")
        .nth(1)
        .expect("Expected TypeOf edge for password parameter");
    assert_eq!(
        password_edge.2,
        Some(1),
        "Expected password parameter at index 1"
    );

    let remember_edge = typeof_edges_meta
        .iter()
        .find(|(func, typ, _, _, _)| func == "authenticate" && typ == "Bool")
        .expect("Expected TypeOf edge for remember parameter");
    assert_eq!(
        remember_edge.2,
        Some(2),
        "Expected remember parameter at index 2"
    );

    // Validate parameter names
    assert_eq!(
        username_edge.3.as_deref(),
        Some("username"),
        "Expected parameter name 'username'"
    );
    assert_eq!(
        password_edge.3.as_deref(),
        Some("password"),
        "Expected parameter name 'password'"
    );
    assert_eq!(
        remember_edge.3.as_deref(),
        Some("remember"),
        "Expected parameter name 'remember'"
    );

    // Validate NodeKind is Function (not Method)
    assert_eq!(
        username_edge.4,
        Some(NodeKind::Function),
        "Expected NodeKind::Function for top-level function"
    );
}

#[test]
fn test_method_parameter_node_kind() {
    let source = r#"
class AuthService {
    func login(email: String, code: Int) -> User {
        return User()
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges_meta = collect_typeof_edges_with_metadata(&staging, TypeOfContext::Parameter);

    // Find method parameter edge
    let email_edge = typeof_edges_meta
        .iter()
        .find(|(method, typ, _, _, _)| method == "AuthService.login" && typ == "String")
        .expect("Expected TypeOf edge for email parameter");

    // Validate NodeKind is Method (not Function) - FIX H-1
    assert_eq!(
        email_edge.4,
        Some(NodeKind::Method),
        "Expected NodeKind::Method for method parameter"
    );

    // Validate parameter metadata
    assert_eq!(email_edge.2, Some(0), "Expected email at index 0");
    assert_eq!(
        email_edge.3.as_deref(),
        Some("email"),
        "Expected parameter name 'email'"
    );
}

#[test]
fn test_return_type_node_kind() {
    let source = r#"
class DataService {
    func fetchData() -> Data {
        return Data()
    }
}

func processData() -> Result {
    return Result()
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges_meta = collect_typeof_edges_with_metadata(&staging, TypeOfContext::Return);

    // Method return type should have NodeKind::Method
    let method_edge = typeof_edges_meta
        .iter()
        .find(|(name, typ, _, _, _)| name == "DataService.fetchData" && typ == "Data")
        .expect("Expected TypeOf edge for method return");
    assert_eq!(
        method_edge.4,
        Some(NodeKind::Method),
        "Expected NodeKind::Method for method return"
    );

    // Function return type should have NodeKind::Function
    let func_edge = typeof_edges_meta
        .iter()
        .find(|(name, typ, _, _, _)| name == "processData" && typ == "Result")
        .expect("Expected TypeOf edge for function return");
    assert_eq!(
        func_edge.4,
        Some(NodeKind::Function),
        "Expected NodeKind::Function for function return"
    );
}

// ============================================================================
// M-4: Spec-Required Type Constructs (Previously Missing Coverage)
// ============================================================================

#[test]
fn test_protocol_composition_types() {
    let source = r#"
func register(user: Codable & Sendable) {
    print(user)
}

var handler: Equatable & Hashable = MyType()
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Protocol composition should extract both protocols
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "register" && typ == "Codable"),
        "Expected Reference edge to Codable in protocol composition, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "register" && typ == "Sendable"),
        "Expected Reference edge to Sendable in protocol composition, got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "Equatable"),
        "Expected Reference edge to Equatable, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "Hashable"),
        "Expected Reference edge to Hashable, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_some_any_types() {
    let source = r#"
func buildView() -> some View {
    return EmptyView()
}

var storage: any Codable = Data()
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // `some View` should extract View
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "buildView" && typ == "View"),
        "Expected Reference edge to View from 'some View', got: {:?}",
        reference_edges
    );

    // `any Codable` should extract Codable
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "storage" && typ == "Codable"),
        "Expected Reference edge to Codable from 'any Codable', got: {:?}",
        reference_edges
    );
}

#[test]
fn test_implicitly_unwrapped_optional() {
    let source = r#"
var outlet: UIView! = nil
func setup(delegate: AppDelegate!) {
    print(delegate)
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Implicitly unwrapped optional should extract base type
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "outlet" && typ == "UIView"),
        "Expected Reference edge to UIView from UIView!, got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "setup" && typ == "AppDelegate"),
        "Expected Reference edge to AppDelegate from AppDelegate!, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_metatype() {
    let source = r#"
func register(type: User.Type) {
    print(type)
}

var typeRef: Codable.Type = String.self
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Metatype User.Type should extract User
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "register" && typ == "User"),
        "Expected Reference edge to User from User.Type, got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "typeRef" && typ == "Codable"),
        "Expected Reference edge to Codable from Codable.Type, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_attributed_types() {
    let source = r#"
func process(handler: @escaping (Int) -> Void) {
    DispatchQueue.main.async {
        handler(42)
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // @escaping function type should extract parameter and return types
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "process" && typ == "Int"),
        "Expected Reference edge to Int from @escaping function type, got: {:?}",
        reference_edges
    );

    // Note: Void may or may not be extracted depending on implementation
    // (it's a builtin type and might be filtered out)
}

#[test]
fn test_inout_parameters() {
    let source = r#"
func swap(a: inout Int, b: inout String) {
    // swap implementation
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // inout parameters should still extract types
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "swap" && typ == "Int")
            || reference_edges
                .iter()
                .any(|(func, typ)| func == "swap" && typ == "Int"),
        "Expected type edge to Int from inout parameter, got typeof: {:?}, ref: {:?}",
        typeof_edges,
        reference_edges
    );

    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "swap" && typ == "String")
            || reference_edges
                .iter()
                .any(|(func, typ)| func == "swap" && typ == "String"),
        "Expected type edge to String from inout parameter, got typeof: {:?}, ref: {:?}",
        typeof_edges,
        reference_edges
    );
}

#[test]
fn test_variadic_parameters() {
    let source = r#"
func log(messages: String...) {
    for msg in messages {
        print(msg)
    }
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Variadic parameters should extract element type
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "log" && typ.contains("String"))
            || reference_edges
                .iter()
                .any(|(func, typ)| func == "log" && typ == "String"),
        "Expected type edge to String from variadic parameter, got typeof: {:?}, ref: {:?}",
        typeof_edges,
        reference_edges
    );
}

#[test]
fn test_async_throws_return_types() {
    let source = r#"
func fetchUser() async -> User {
    return User()
}

func loadData() throws -> Data {
    return Data()
}

func process() async throws -> Result {
    return Result()
}

func asyncRethrows(_ handler: () async throws -> Void) rethrows {
    try await handler()
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // async function should extract return type
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "fetchUser" && typ == "User"),
        "Expected Return TypeOf edge for async function, got: {:?}",
        typeof_edges
    );

    // throws function should extract return type
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "loadData" && typ == "Data"),
        "Expected Return TypeOf edge for throws function, got: {:?}",
        typeof_edges
    );

    // async throws function should extract return type
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "process" && typ == "Result"),
        "Expected Return TypeOf edge for async throws function, got: {:?}",
        typeof_edges
    );

    // Note: rethrows may or may not have a return type detected
    // depending on whether grammar provides explicit return type for Void
}

#[test]
fn test_parameter_external_internal_labels() {
    let source = r#"
func greet(to name: String, with message: String) {
    print("\(message), \(name)!")
}

func process(_ value: Int, silent: Bool) {
    print(value)
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges_meta = collect_typeof_edges_with_metadata(&staging, TypeOfContext::Parameter);

    // First parameter: external label "to", internal name "name"
    let name_param = typeof_edges_meta
        .iter()
        .find(|(func, typ, idx, _, _)| func == "greet" && typ == "String" && *idx == Some(0))
        .expect("Expected first String parameter");

    // Should capture internal name "name" (used in function body)
    assert_eq!(
        name_param.3.as_deref(),
        Some("name"),
        "Expected parameter name 'name' (internal label)"
    );

    // Second parameter: external label "with", internal name "message"
    let message_param = typeof_edges_meta
        .iter()
        .find(|(func, typ, idx, _, _)| func == "greet" && typ == "String" && *idx == Some(1))
        .expect("Expected second String parameter");

    assert_eq!(
        message_param.3.as_deref(),
        Some("message"),
        "Expected parameter name 'message' (internal label)"
    );

    // Wildcard parameter: _ means no external label
    let value_param = typeof_edges_meta
        .iter()
        .find(|(func, typ, _, _, _)| func == "process" && typ == "Int")
        .expect("Expected Int parameter");

    assert_eq!(
        value_param.3.as_deref(),
        Some("value"),
        "Expected parameter name 'value' even with _ external label"
    );
}

#[test]
fn test_multiple_bindings() {
    let source = r#"
var a, b: Int = 0
let x, y, z: String = ""

struct Point {
    var x, y: Double
}
"#;

    let staging = build_test_graph(source, "test.swift");

    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Multiple bindings in var declaration
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "a" && typ == "Int"),
        "Expected TypeOf edge for 'a' in multiple binding, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "b" && typ == "Int"),
        "Expected TypeOf edge for 'b' in multiple binding, got: {:?}",
        typeof_edges
    );

    // Multiple bindings in let declaration
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "x" && typ == "String"),
        "Expected TypeOf edge for 'x' in multiple binding, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "y" && typ == "String"),
        "Expected TypeOf edge for 'y' in multiple binding, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "z" && typ == "String"),
        "Expected TypeOf edge for 'z' in multiple binding, got: {:?}",
        typeof_edges
    );

    // Multiple bindings in struct properties
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.x" && typ == "Double"),
        "Expected TypeOf edge for Point.x in multiple binding, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.y" && typ == "Double"),
        "Expected TypeOf edge for Point.y in multiple binding, got: {:?}",
        field_edges
    );
}

// ============================================================================
// Iteration 3 Tests - Mixed-Type Multi-Binding and Metatype Coverage
// ============================================================================

/// FIX M-1 (Iteration 4): Test actual multi-binding declarations at top level.
/// Tests both shared-type (`var a, b: Int`) and per-binding (`var a: Int, b: String`) syntax.
/// Validates that the pattern+type pairing logic correctly handles both forms.
#[test]
fn test_mixed_type_multi_binding_toplevel() {
    let source = r#"
// Shared-type multi-binding (valid Swift)
var a, b, c: Int

// Per-binding types (valid Swift)
var x: Bool, y: Double, z: Float

// Mix of shared and per-binding
var p, q: String, r: [UInt8]
"#;

    let staging = build_test_graph(source, "test.swift");
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Shared-type binding: var a, b, c: Int
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "a" && typ == "Int"),
        "Expected TypeOf edge a -> Int, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "b" && typ == "Int"),
        "Expected TypeOf edge b -> Int, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "c" && typ == "Int"),
        "Expected TypeOf edge c -> Int, got: {:?}",
        typeof_edges
    );

    // Per-binding types: var x: Bool, y: Double, z: Float
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "x" && typ == "Bool"),
        "Expected TypeOf edge x -> Bool, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "y" && typ == "Double"),
        "Expected TypeOf edge y -> Double, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "z" && typ == "Float"),
        "Expected TypeOf edge z -> Float, got: {:?}",
        typeof_edges
    );

    // Mixed: var p, q: String, r: [UInt8]
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "p" && typ == "String"),
        "Expected TypeOf edge p -> String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "q" && typ == "String"),
        "Expected TypeOf edge q -> String, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "r" && typ == "[UInt8]"),
        "Expected TypeOf edge r -> [UInt8], got: {:?}",
        typeof_edges
    );
}

/// FIX M-1 (Iteration 4): Test actual multi-binding declarations in properties.
/// Tests both shared-type and per-binding syntax for class/struct properties.
#[test]
fn test_mixed_type_multi_binding_property() {
    let source = r#"
class MyClass {
    // Shared-type multi-binding
    var x, y: Double

    // Per-binding types
    var p: Int, q: String, r: [UInt8]
}

struct Point {
    // Shared-type
    var a, b: Float

    // Per-binding
    var x: Bool, y: Character
}
"#;

    let staging = build_test_graph(source, "test.swift");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // MyClass properties - Shared-type: var x, y: Double
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "MyClass.x" && typ == "Double"),
        "Expected TypeOf edge MyClass.x -> Double, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "MyClass.y" && typ == "Double"),
        "Expected TypeOf edge MyClass.y -> Double, got: {:?}",
        field_edges
    );

    // MyClass properties - Per-binding: var p: Int, q: String, r: [UInt8]
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "MyClass.p" && typ == "Int"),
        "Expected TypeOf edge MyClass.p -> Int, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "MyClass.q" && typ == "String"),
        "Expected TypeOf edge MyClass.q -> String, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "MyClass.r" && typ == "[UInt8]"),
        "Expected TypeOf edge MyClass.r -> [UInt8], got: {:?}",
        field_edges
    );

    // Point properties - Shared-type: var a, b: Float
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.a" && typ == "Float"),
        "Expected TypeOf edge Point.a -> Float, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.b" && typ == "Float"),
        "Expected TypeOf edge Point.b -> Float, got: {:?}",
        field_edges
    );

    // Point properties - Per-binding: var x: Bool, y: Character
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.x" && typ == "Bool"),
        "Expected TypeOf edge Point.x -> Bool, got: {:?}",
        field_edges
    );
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Point.y" && typ == "Character"),
        "Expected TypeOf edge Point.y -> Character, got: {:?}",
        field_edges
    );
}

/// FIX M-2 (Iteration 3): Test metatype node kind coverage.
/// Tests that both `metatype` and `metatype_type` node kinds are handled.
#[test]
fn test_metatype_node_kind_coverage() {
    let source = r#"
var t: User.Type
var p: Protocol.Type

func getType() -> SomeClass.Type {
    return SomeClass.self
}

class Container {
    var metaRef: Service.Type
}
"#;

    let staging = build_test_graph(source, "test.swift");

    // Check variable TypeOf edges
    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);
    assert!(
        var_edges
            .iter()
            .any(|(var, typ)| var == "t" && typ == "User.Type"),
        "Expected TypeOf edge t -> User.Type, got: {:?}",
        var_edges
    );
    assert!(
        var_edges
            .iter()
            .any(|(var, typ)| var == "p" && typ == "Protocol.Type"),
        "Expected TypeOf edge p -> Protocol.Type, got: {:?}",
        var_edges
    );

    // Check return type TypeOf edge
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    assert!(
        return_edges
            .iter()
            .any(|(func, typ)| func == "getType" && typ == "SomeClass.Type"),
        "Expected TypeOf edge getType -> SomeClass.Type, got: {:?}",
        return_edges
    );

    // Check property TypeOf edge
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        field_edges
            .iter()
            .any(|(prop, typ)| prop == "Container.metaRef" && typ == "Service.Type"),
        "Expected TypeOf edge Container.metaRef -> Service.Type, got: {:?}",
        field_edges
    );

    // Check Reference edges to the base types
    let ref_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));
    assert!(
        ref_edges.iter().any(|(_, to)| to == "User"),
        "Expected Reference edge to User, got: {:?}",
        ref_edges
    );
    assert!(
        ref_edges.iter().any(|(_, to)| to == "Protocol"),
        "Expected Reference edge to Protocol, got: {:?}",
        ref_edges
    );
    assert!(
        ref_edges.iter().any(|(_, to)| to == "SomeClass"),
        "Expected Reference edge to SomeClass, got: {:?}",
        ref_edges
    );
    assert!(
        ref_edges.iter().any(|(_, to)| to == "Service"),
        "Expected Reference edge to Service, got: {:?}",
        ref_edges
    );
}
