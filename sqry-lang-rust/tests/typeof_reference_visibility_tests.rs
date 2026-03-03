//! Comprehensive tests for Rust TypeOf edges, Reference edges, and type extraction.
//!
//! These tests validate that the Rust plugin creates TypeOf and Reference edges
//! for all typed declarations and extracts all type names from complex type annotations.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_rust::relations::RustGraphBuilder;
use sqry_test_support::graph_helpers::build_string_lookup;
use std::path::Path;
use tree_sitter::Parser;

fn parse_rust(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Error loading Rust grammar");
    parser.parse(source, None).expect("Error parsing Rust code")
}

fn build_test_graph(source: &str, file_name: &str) -> StagingGraph {
    let tree = parse_rust(source);
    let file = Path::new(file_name);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

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
                return Some((from_name?, to_name?));
            }
            None
        })
        .collect()
}

// ========== TypeOf Edges - Let Declarations ==========

#[test]
fn test_typeof_edge_simple_let_binding() {
    let source = r"
fn main() {
    let user: User = get_user();
    let count: usize = 0;
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "User"),
        "Expected TypeOf edge from user to User, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "usize"),
        "Expected TypeOf edge from count to usize, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_generic_type_let_binding() {
    let source = r"
fn main() {
    let users: Vec<User> = vec![];
    let map: HashMap<String, i32> = HashMap::new();
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // TypeOf edge to primary (base) type
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "Vec"),
        "Expected TypeOf edge from users to Vec, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "map" && typ == "HashMap"),
        "Expected TypeOf edge from map to HashMap, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_reference_edges_generic_type_arguments() {
    let source = r"
fn main() {
    let users: Vec<User> = vec![];
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Reference edges to ALL extracted types (Vec and User)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "Vec"),
        "Expected Reference edge from users to Vec, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "User"),
        "Expected Reference edge from users to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_typeof_edge_reference_type_let_binding() {
    let source = r"
fn main() {
    let data: &DataFrame = get_data();
    let mut_data: &mut DataFrame = get_mut_data();
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Reference types should extract base type
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "data" && typ == "DataFrame"),
        "Expected TypeOf edge from data to DataFrame, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "mut_data" && typ == "DataFrame"),
        "Expected TypeOf edge from mut_data to DataFrame, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_complex_generic_let_binding() {
    let source = r"
fn main() {
    let cache: Arc<RwLock<HashMap<String, User>>> = Arc::new(RwLock::new(HashMap::new()));
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edge to primary type (Arc)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "Arc"),
        "Expected TypeOf edge from cache to Arc, got: {:?}",
        typeof_edges
    );

    // Reference edges to ALL extracted types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "Arc"),
        "Expected Reference edge to Arc"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "RwLock"),
        "Expected Reference edge to RwLock, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "HashMap"),
        "Expected Reference edge to HashMap, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "String"),
        "Expected Reference edge to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "cache" && typ == "User"),
        "Expected Reference edge to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_no_typeof_edge_for_untyped_let() {
    let source = r"
fn main() {
    let x = 42;
    let y = vec![1, 2, 3];
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // No type annotations, so no TypeOf edges for x or y
    assert!(
        !typeof_edges.iter().any(|(var, _)| var == "x"),
        "Should not create TypeOf edge for untyped let binding x"
    );
    assert!(
        !typeof_edges.iter().any(|(var, _)| var == "y"),
        "Should not create TypeOf edge for untyped let binding y"
    );
}

#[test]
fn test_typeof_edge_tuple_type() {
    let source = r#"
fn main() {
    let tuple: (i32, String, User) = (42, String::from("test"), get_user());
}
"#;

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // All tuple element types should be extracted
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "tuple" && typ == "i32"),
        "Expected Reference edge to i32, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "tuple" && typ == "String"),
        "Expected Reference edge to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "tuple" && typ == "User"),
        "Expected Reference edge to User, got: {:?}",
        reference_edges
    );
}

// ========== TypeOf Edges - Parameters ==========

#[test]
fn test_typeof_edge_simple_parameter() {
    let source = r"
fn process(user: User, count: usize) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "User"),
        "Expected TypeOf edge from user param to User, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "usize"),
        "Expected TypeOf edge from count param to usize, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_reference_parameter() {
    let source = r"
fn process(data: &DataFrame, mut_data: &mut DataFrame) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Reference types extract base type
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "data" && typ == "DataFrame"),
        "Expected TypeOf edge from data param to DataFrame, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "mut_data" && typ == "DataFrame"),
        "Expected TypeOf edge from mut_data param to DataFrame, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_generic_parameter() {
    let source = r"
fn process_items(items: Vec<Item>) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf to primary type
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ == "Vec"),
        "Expected TypeOf edge from items to Vec, got: {:?}",
        typeof_edges
    );

    // Reference edges to all types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ == "Vec"),
        "Expected Reference edge to Vec"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ == "Item"),
        "Expected Reference edge to Item, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_no_typeof_edge_for_self_parameter() {
    let source = r"
impl MyStruct {
    fn process(&self) {
        // body
    }

    fn process_mut(&mut self) {
        // body
    }
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // self parameters should be skipped
    assert!(
        !typeof_edges.iter().any(|(var, _)| var == "self"),
        "Should not create TypeOf edge for self parameter"
    );
}

// ========== TypeOf Edges - Struct Fields ==========

#[test]
fn test_typeof_edge_struct_fields() {
    let source = r"
struct Service {
    repository: UserRepository,
    cache: Cache,
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "repository" && typ == "UserRepository"),
        "Expected TypeOf edge from repository field to UserRepository, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "Cache"),
        "Expected TypeOf edge from cache field to Cache, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_struct_fields_generic() {
    let source = r"
struct Container {
    items: Vec<Item>,
    map: HashMap<String, User>,
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf to primary types
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "items" && typ == "Vec"),
        "Expected TypeOf edge from items to Vec, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "map" && typ == "HashMap"),
        "Expected TypeOf edge from map to HashMap, got: {:?}",
        typeof_edges
    );

    // Reference edges to all types
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "items" && typ == "Item"),
        "Expected Reference edge from items to Item, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "map" && typ == "String"),
        "Expected Reference edge from map to String"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "map" && typ == "User"),
        "Expected Reference edge from map to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_typeof_edge_struct_fields_complex() {
    let source = r"
struct Service {
    cache: Arc<RwLock<HashMap<String, Vec<User>>>>,
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // All nested types should be extracted
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "Arc"),
        "Expected Reference edge to Arc"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "RwLock"),
        "Expected Reference edge to RwLock"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "HashMap"),
        "Expected Reference edge to HashMap"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "String"),
        "Expected Reference edge to String"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "Vec"),
        "Expected Reference edge to Vec"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(field, typ)| field == "cache" && typ == "User"),
        "Expected Reference edge to User, got: {:?}",
        reference_edges
    );
}

// ========== Tuple Struct/Enum Fields ==========

#[test]
#[ignore = "Debug helper test - run manually when debugging AST structure"]
fn debug_trait_bounds_ast() {
    use tree_sitter::Parser;

    let source = r#"
fn process<T: Display + Clone>(value: T) where T: Iterator {
    println!("{}", value);
}
"#;
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();

    fn print_node(node: tree_sitter::Node, source: &str, depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = node.utf8_text(source.as_bytes()).unwrap_or("");
        let text_preview = if text.len() > 60 { &text[..60] } else { text };
        println!("{}{}  '{}'", indent, kind, text_preview);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_node(child, source, depth + 1);
        }
    }

    print_node(root, source, 0);
}

#[test]
#[ignore = "Debug helper test - run manually when debugging AST structure"]
fn debug_tuple_struct_ast() {
    use tree_sitter::Parser;

    let source = "struct Point(i32, i32);";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();

    fn print_node(node: tree_sitter::Node, source: &str, depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = node.utf8_text(source.as_bytes()).unwrap_or("");
        let text_preview = if text.len() > 40 { &text[..40] } else { text };
        println!("{}{}  '{}'", indent, kind, text_preview);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_node(child, source, depth + 1);
        }
    }

    print_node(root, source, 0);
}

#[test]
fn test_typeof_edge_tuple_struct() {
    let source = r"
struct Point(i32, i32);
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Tuple fields are named by index: 0, 1
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "0" && typ == "i32"),
        "Expected TypeOf edge from field 0 to i32, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "1" && typ == "i32"),
        "Expected TypeOf edge from field 1 to i32, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_tuple_struct_generic() {
    let source = r"
struct Pair<T, U>(T, U);
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "0" && typ == "T"),
        "Expected TypeOf edge from field 0 to T, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "1" && typ == "U"),
        "Expected TypeOf edge from field 1 to U, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_tuple_struct_complex() {
    let source = r"
struct Wrapper(Arc<RwLock<HashMap<String, User>>>);
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edge to primary type
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "0" && typ == "Arc"),
        "Expected TypeOf edge from field 0 to Arc, got: {:?}",
        typeof_edges
    );

    // Reference edges to all nested types
    let expected_types = vec!["Arc", "RwLock", "HashMap", "String", "User"];
    for expected_type in expected_types {
        assert!(
            reference_edges
                .iter()
                .any(|(field, typ)| field == "0" && typ == expected_type),
            "Expected Reference edge from field 0 to {}, got: {:?}",
            expected_type,
            reference_edges
        );
    }
}

#[test]
fn test_typeof_edge_enum_variant_tuple_fields() {
    let source = r"
enum Result<T, E> {
    Ok(T),
    Err(E),
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Each variant has a tuple field at index 0
    // Note: Both variants have field "0", so we just check that T and E are extracted
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "0" && typ == "T"),
        "Expected TypeOf edge from field 0 to T (Ok variant), got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(field, typ)| field == "0" && typ == "E"),
        "Expected TypeOf edge from field 0 to E (Err variant), got: {:?}",
        typeof_edges
    );
}

// ========== Reference Edges - Type Aliases ==========

#[test]
fn test_reference_edge_simple_type_alias() {
    let source = r"
type UserId = u64;
type UserName = String;
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "UserId" && typ == "u64"),
        "Expected Reference edge from UserId to u64, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "UserName" && typ == "String"),
        "Expected Reference edge from UserName to String, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_reference_edge_generic_type_alias() {
    let source = r"
type MyResult<T> = Result<T, MyError>;
type MyVec<T> = Vec<T>;
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // MyResult<T> references: Result, T, MyError
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "MyResult" && typ == "Result"),
        "Expected Reference edge from MyResult to Result, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "MyResult" && typ == "T"),
        "Expected Reference edge from MyResult to T, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "MyResult" && typ == "MyError"),
        "Expected Reference edge from MyResult to MyError, got: {:?}",
        reference_edges
    );

    // MyVec<T> references: Vec, T
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "MyVec" && typ == "Vec"),
        "Expected Reference edge from MyVec to Vec"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "MyVec" && typ == "T"),
        "Expected Reference edge from MyVec to T, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_reference_edge_complex_type_alias() {
    let source = r"
type BoxFn<T> = Box<dyn Fn(T) -> String>;
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // BoxFn references: Box, Fn, T, String
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "BoxFn" && typ == "Box"),
        "Expected Reference edge from BoxFn to Box, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "BoxFn" && typ == "Fn"),
        "Expected Reference edge from BoxFn to Fn, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "BoxFn" && typ == "T"),
        "Expected Reference edge from BoxFn to T, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "BoxFn" && typ == "String"),
        "Expected Reference edge from BoxFn to String, got: {:?}",
        reference_edges
    );
}

// ========== Const/Static TypeOf Edges ==========

#[test]
fn test_typeof_edge_simple_const() {
    let source = r"
const MAX_SIZE: usize = 100;
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "MAX_SIZE" && typ == "usize"),
        "Expected TypeOf edge from MAX_SIZE to usize, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_simple_static() {
    let source = r"
static INSTANCE_COUNT: i32 = 0;
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "INSTANCE_COUNT" && typ == "i32"),
        "Expected TypeOf edge from INSTANCE_COUNT to i32, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_typeof_edge_generic_const() {
    let source = r"
const USERS: Vec<User> = Vec::new();
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edge to primary type (Vec)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "USERS" && typ == "Vec"),
        "Expected TypeOf edge from USERS to Vec, got: {:?}",
        typeof_edges
    );

    // Reference edges to all types (Vec, User)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "USERS" && typ == "Vec"),
        "Expected Reference edge from USERS to Vec, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "USERS" && typ == "User"),
        "Expected Reference edge from USERS to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_typeof_edge_complex_static() {
    let source = r"
static CACHE: Arc<RwLock<HashMap<String, User>>> = Arc::new(RwLock::new(HashMap::new()));
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edge to primary type (Arc)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "CACHE" && typ == "Arc"),
        "Expected TypeOf edge from CACHE to Arc, got: {:?}",
        typeof_edges
    );

    // Reference edges to all types
    let expected_types = vec!["Arc", "RwLock", "HashMap", "String", "User"];
    for expected_type in expected_types {
        assert!(
            reference_edges
                .iter()
                .any(|(var, typ)| var == "CACHE" && typ == expected_type),
            "Expected Reference edge from CACHE to {}, got: {:?}",
            expected_type,
            reference_edges
        );
    }
}

#[test]
fn test_no_typeof_edge_for_untyped_const() {
    let source = r"
const PI = 3.14;
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // No TypeOf edge because type is inferred, not explicitly annotated
    assert!(
        !typeof_edges.iter().any(|(var, _)| var == "PI"),
        "Expected no TypeOf edge for untyped const, got: {:?}",
        typeof_edges
    );
}

// ========== Complex Type Extraction ==========

#[test]
fn test_trait_bounds_extraction() {
    let source = r"
fn compare<T: Ord + Display>(a: T, b: T) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Parameters should reference T
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "a" && typ == "T"),
        "Expected Reference edge from parameter a to T, got: {:?}",
        reference_edges
    );

    // Function should reference trait bounds: Ord, Display
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "compare" && typ == "Ord"),
        "Expected Reference edge from function compare to Ord, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "compare" && typ == "Display"),
        "Expected Reference edge from function compare to Display, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_where_clause_trait_bounds() {
    let source = r"
fn process<T>(value: T) where T: Iterator + Clone {
    let x = value;
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Parameter should reference T
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "value" && typ == "T"),
        "Expected Reference edge from parameter to T, got: {:?}",
        reference_edges
    );

    // Function should reference where clause trait bounds: Iterator, Clone
    // The function name may be module-qualified, so we check if any edge exists with the right target
    let has_iterator = reference_edges.iter().any(|(_, typ)| typ == "Iterator");
    let has_clone = reference_edges.iter().any(|(_, typ)| typ == "Clone");

    assert!(
        has_iterator,
        "Expected Reference edge to Iterator (from where clause), got: {:?}",
        reference_edges
    );
    assert!(
        has_clone,
        "Expected Reference edge to Clone (from where clause), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_struct_trait_bounds() {
    let source = r"
struct Container<T: Display + Debug> {
    value: T,
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Struct should reference trait bounds: Display, Debug
    assert!(
        reference_edges
            .iter()
            .any(|(s, typ)| s == "Container" && typ == "Display"),
        "Expected Reference edge from Container to Display, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(s, typ)| s == "Container" && typ == "Debug"),
        "Expected Reference edge from Container to Debug, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_impl_trait_bounds() {
    let source = r"
impl<T: Display> MyStruct<T> {
    fn new(value: T) -> Self {
        MyStruct { value }
    }
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Impl type should reference trait bounds: Display
    // Note: The type name includes type parameters, so it's "MyStruct<T>"
    assert!(
        reference_edges
            .iter()
            .any(|(s, typ)| s.starts_with("MyStruct") && typ == "Display"),
        "Expected Reference edge from MyStruct to Display, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_associated_type_extraction() {
    use sqry_lang_rust::relations::graph_builder::extract_all_type_names_from_rust_type;

    // Test the extraction function directly with associated type syntax
    // Need to wrap in a valid Rust construct for parsing
    let source = "fn test() -> <T as Iterator>::Item { unimplemented!() }";
    let tree = parse_rust(source);
    let root = tree.root_node();

    // Find the qualified_type node in the return type
    fn find_qualified_type(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
        if node.kind() == "qualified_type" || node.kind() == "scoped_type_identifier" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_qualified_type(child) {
                return Some(found);
            }
        }
        None
    }

    let type_node = find_qualified_type(root)
        .expect("Should find qualified_type or scoped_type_identifier node");

    let extracted = extract_all_type_names_from_rust_type(type_node, source.as_bytes());

    // Should extract T, Iterator, and Item
    assert!(
        extracted.contains(&"T".to_string()),
        "Expected to extract 'T' from '<T as Iterator>::Item', got: {:?}",
        extracted
    );
    assert!(
        extracted.contains(&"Iterator".to_string()),
        "Expected to extract 'Iterator' from '<T as Iterator>::Item', got: {:?}",
        extracted
    );
    assert!(
        extracted.contains(&"Item".to_string()),
        "Expected to extract 'Item' from '<T as Iterator>::Item', got: {:?}",
        extracted
    );

    assert_eq!(
        extracted.len(),
        3,
        "Expected exactly 3 types (T, Iterator, Item), got: {:?}",
        extracted
    );
}

#[test]
fn test_impl_trait_extraction() {
    let source = r"
fn create_display(x: impl Display) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // impl Display should extract Display
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "x" && typ == "Display"),
        "Expected Reference edge to Display from impl trait, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_function_type_extraction() {
    let source = r"
type Callback = fn(i32, String) -> Result<User, Error>;
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // fn(i32, String) -> Result<User, Error> should extract all types
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Callback" && typ == "i32"),
        "Expected Reference edge to i32, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Callback" && typ == "String"),
        "Expected Reference edge to String, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Callback" && typ == "Result"),
        "Expected Reference edge to Result, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Callback" && typ == "User"),
        "Expected Reference edge to User, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Callback" && typ == "Error"),
        "Expected Reference edge to Error, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_scoped_type_identifier_extraction() {
    let source = r"
fn process(data: std::vec::Vec<User>) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // std::vec::Vec should extract just "Vec" (last component)
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "data" && typ == "Vec"),
        "Expected Reference edge to Vec from scoped identifier, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "data" && typ == "User"),
        "Expected Reference edge to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_array_type_extraction() {
    let source = r"
fn process(buffer: [u8; 1024]) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // [u8; 1024] should extract u8, skip 1024
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "buffer" && typ == "u8"),
        "Expected Reference edge to u8 from array type, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_pointer_type_extraction() {
    let source = r"
fn process(ptr: *const User, mut_ptr: *mut User) {
    // body
}
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // *const User and *mut User should extract User
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "ptr" && typ == "User"),
        "Expected Reference edge to User from *const, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(param, typ)| param == "mut_ptr" && typ == "User"),
        "Expected Reference edge to User from *mut, got: {:?}",
        reference_edges
    );
}

// ========== Edge Cases ==========

#[test]
fn test_no_reference_to_unit_type() {
    let source = r"
fn noop() -> () {
    // body
}

type UnitAlias = ();
";

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Unit type () should not create references
    // Filter for any edges that might reference "()" or "unit"
    let unit_refs: Vec<_> = reference_edges
        .iter()
        .filter(|(_, typ)| typ.contains("()") || typ.contains("unit"))
        .collect();

    assert!(
        unit_refs.is_empty(),
        "Should not create references to unit type, found: {:?}",
        unit_refs
    );
}

#[test]
fn test_no_reference_to_never_type() {
    let source = r#"
fn diverge() -> ! {
    panic!("never returns");
}
"#;

    let staging = build_test_graph(source, "test.rs");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Never type ! should not create references
    let never_refs: Vec<_> = reference_edges
        .iter()
        .filter(|(_, typ)| typ == "!")
        .collect();

    assert!(
        never_refs.is_empty(),
        "Should not create references to never type, found: {:?}",
        never_refs
    );
}

#[test]
fn test_integration_all_features() {
    let source = r"
// Type alias with generics and bounds
type MyResult<T: Display> = Result<T, MyError>;

// Struct with complex fields
struct Service {
    cache: Arc<RwLock<HashMap<String, Vec<User>>>>,
    callback: Box<dyn Fn(&User) -> Result<(), Error>>,
}

// Function with various parameter types
fn process<T: Iterator<Item = User>>(
    iter: T,
    data: &DataFrame,
    buffer: [u8; 256],
    callback: impl Fn(User),
) {
    let result: MyResult<String> = Ok(String::new());
    let items: Vec<Item> = vec![];
}
";

    let staging = build_test_graph(source, "test.rs");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Verify we have TypeOf edges for variables and parameters
    assert!(!typeof_edges.is_empty(), "Should have TypeOf edges");

    // Verify we have Reference edges for type aliases and complex types
    assert!(!reference_edges.is_empty(), "Should have Reference edges");

    // Spot checks for complex types
    assert!(
        reference_edges.iter().any(|(_, typ)| typ == "HashMap"),
        "Should extract HashMap from nested generic, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges.iter().any(|(_, typ)| typ == "User"),
        "Should extract User from various contexts, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges.iter().any(|(_, typ)| typ == "Result"),
        "Should extract Result from type alias and function types, got: {:?}",
        reference_edges
    );
}

// ============================================================================
// TypeOf Edges - Function Pointer Types
// ============================================================================

/// Test that function pointer types create TypeOf edge to "fn" marker, not first parameter.
///
/// For `let cb: fn(i32) -> i32`, the TypeOf edge should point to "fn",
/// and Reference edges should point to all parameter and return types.
///
/// This ensures function-typed variables are semantically marked as functions,
/// not as whatever their first parameter type happens to be.
#[test]
fn test_typeof_edge_function_pointer() {
    let source = r"
fn main() {
    let callback: fn(i32) -> i32 = |x| x + 1;
}
";
    let staging = build_test_graph(source, "test.rs");

    // Get TypeOf edges (Variable -> Type)
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Get Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Verify TypeOf edge points to "fn" marker, NOT "i32"
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "callback" && typ == "fn"),
        "TypeOf edge should point to 'fn' marker for function pointer type, got: {:?}",
        typeof_edges
    );

    // Verify we DON'T have TypeOf edge to i32
    assert!(
        !typeof_edges
            .iter()
            .any(|(var, typ)| var == "callback" && typ == "i32"),
        "TypeOf edge should NOT point to first parameter type 'i32', got: {:?}",
        typeof_edges
    );

    // Verify Reference edges include both parameter and return types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "callback" && typ == "i32"),
        "Should have Reference edges to parameter/return types, got: {:?}",
        reference_edges
    );
}

/// Test that trait-based function types (Fn, FnMut, FnOnce) use trait name as primary type.
#[test]
fn test_typeof_edge_fn_trait() {
    let source = r"
fn main() {
    let closure: Box<dyn Fn(i32) -> String> = Box::new(|x| x.to_string());
}
";
    let staging = build_test_graph(source, "test.rs");

    // Get TypeOf and Reference edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edge should point to Box (outermost type)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "closure" && typ == "Box"),
        "TypeOf edge should point to 'Box', got: {:?}",
        typeof_edges
    );

    // Reference edges should include Fn trait and all parameter/return types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "closure" && typ == "Fn"),
        "Should have Reference edge to 'Fn' trait, got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "closure" && typ == "i32"),
        "Should have Reference edge to parameter type 'i32', got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "closure" && typ == "String"),
        "Should have Reference edge to return type 'String', got: {:?}",
        reference_edges
    );
}

// ============================================================================
// Reference Edges - Type Alias Bounds
// ============================================================================

/// Test that type alias bounds create Reference edges to bound traits.
///
/// For `type Alias<T: Trait> = ...`, should create Reference edges from
/// the alias to both the RHS types AND the bound traits from type parameters.
#[test]
fn test_reference_edge_type_alias_with_bounds() {
    let source = r"
type Serializable<T: Serialize + Clone> = Result<T, Error>;
";
    let staging = build_test_graph(source, "test.rs");

    // Get Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Should have Reference edges to RHS types
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Serializable" && typ == "Result"),
        "Should have Reference edge to RHS type 'Result', got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Serializable" && typ == "T"),
        "Should have Reference edge to type parameter 'T', got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Serializable" && typ == "Error"),
        "Should have Reference edge to RHS type 'Error', got: {:?}",
        reference_edges
    );

    // Should have Reference edges to bound traits from type parameters
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Serializable" && typ == "Serialize"),
        "Should have Reference edge to bound trait 'Serialize', got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Serializable" && typ == "Clone"),
        "Should have Reference edge to bound trait 'Clone', got: {:?}",
        reference_edges
    );
}

/// Test type alias with multiple type parameters and bounds.
#[test]
fn test_reference_edge_type_alias_multiple_params_bounds() {
    let source = r"
type Mapper<K: Hash + Eq, V: Display> = HashMap<K, V>;
";
    let staging = build_test_graph(source, "test.rs");

    // Get Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // RHS types
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "HashMap"),
        "Should have Reference edge to 'HashMap', got: {:?}",
        reference_edges
    );

    // Type parameters
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "K"),
        "Should have Reference edge to 'K', got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "V"),
        "Should have Reference edge to 'V', got: {:?}",
        reference_edges
    );

    // Bound traits for K
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "Hash"),
        "Should have Reference edge to bound trait 'Hash' from K, got: {:?}",
        reference_edges
    );

    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "Eq"),
        "Should have Reference edge to bound trait 'Eq' from K, got: {:?}",
        reference_edges
    );

    // Bound trait for V
    assert!(
        reference_edges
            .iter()
            .any(|(alias, typ)| alias == "Mapper" && typ == "Display"),
        "Should have Reference edge to bound trait 'Display' from V, got: {:?}",
        reference_edges
    );
}
