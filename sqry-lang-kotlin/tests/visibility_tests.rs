use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_kotlin::relations::KotlinGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_kotlin(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin::LANGUAGE.into())
        .expect("Failed to set Kotlin language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Kotlin code")
}

// ========== Property Node Tests (Task #7: P1 Property Nodes) ==========

fn count_node_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { entry, .. } if entry.kind == kind))
        .count()
}

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

#[test]
fn test_val_property_creates_property_node() {
    let source = r#"
class User {
    val id: Int = 1
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // val property (without const) should create Property node
    // Kotlin: val is immutable, const val is compile-time constant
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "id"),
        "Expected Property node for val property 'id'"
    );
}

#[test]
fn test_var_property_creates_property_node() {
    let source = r#"
class User {
    var name: String = ""
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // var property should create Property node
    assert!(
        has_node_with_kind_and_name(&staging, NodeKind::Property, "name"),
        "Expected Property node for var property 'name'"
    );
}

#[test]
fn test_mixed_val_var_properties() {
    let source = r#"
class User {
    val id: Int = 1
    var name: String = ""
    val email: String = ""
    var age: Int = 0
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Both val and var properties → Property nodes (4 total)
    // Kotlin: val is immutable property, var is mutable property
    // Only const val would be a Constant node
    let property_count = count_node_kind(&staging, NodeKind::Property);
    assert!(
        property_count >= 4,
        "Expected at least 4 Property nodes (2 val + 2 var), got {}",
        property_count
    );
}

#[test]
fn test_property_with_visibility() {
    let source = r#"
class User {
    private val secret: String = "hidden"
    protected var count: Int = 0
    internal val config: String = ""
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Check that nodes are created with proper visibility
    let strings = build_string_lookup(&staging);
    let mut found_private = false;
    let mut found_protected = false;
    let mut found_internal = false;

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            let vis = entry
                .visibility
                .and_then(|id| strings.get(&id.index()).cloned());

            if name.contains("secret") {
                assert_eq!(vis, Some("private".to_string()), "secret should be private");
                found_private = true;
            }
            if name.contains("count") {
                assert_eq!(
                    vis,
                    Some("protected".to_string()),
                    "count should be protected"
                );
                found_protected = true;
            }
            if name.contains("config") {
                assert_eq!(
                    vis,
                    Some("internal".to_string()),
                    "config should be internal"
                );
                found_internal = true;
            }
        }
    }

    assert!(found_private, "Should have found private property");
    assert!(found_protected, "Should have found protected property");
    assert!(found_internal, "Should have found internal property");
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && (entry.kind == NodeKind::Function || entry.kind == NodeKind::Method)
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_function_visibility_public_explicit() {
    let source = r#"
public fun publicFunction() {
    println("public")
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "publicFunction");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "public function should have public visibility"
    );
}

#[test]
fn test_function_visibility_public_default() {
    let source = r#"
fun defaultPublicFunction() {
    println("default public")
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "defaultPublicFunction");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "function without modifier should have public visibility (Kotlin default)"
    );
}

#[test]
fn test_function_visibility_private() {
    let source = r#"
private fun privateFunction() {
    println("private")
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "privateFunction");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "private function should have private visibility"
    );
}

#[test]
fn test_function_visibility_protected() {
    let source = r#"
class MyClass {
    protected fun protectedFunction() {
        println("protected")
    }
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "protectedFunction");
    assert_eq!(
        visibility,
        Some("protected".to_string()),
        "protected function should have protected visibility"
    );
}

#[test]
fn test_function_visibility_internal() {
    let source = r#"
internal fun internalFunction() {
    println("internal")
}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "internalFunction");
    assert_eq!(
        visibility,
        Some("internal".to_string()),
        "internal function should have internal visibility"
    );
}

#[test]
fn test_function_visibility_mixed() {
    let source = r#"
public fun publicFunc() {}
private fun privateFunc() {}
protected fun protectedFunc() {}
internal fun internalFunc() {}
fun defaultPublic() {}
"#;
    let tree = parse_kotlin(source);
    let mut staging = StagingGraph::new();
    let builder = KotlinGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("test.kt"), &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(
        find_function_visibility(&staging, "publicFunc"),
        Some("public".to_string()),
        "publicFunc should be public"
    );
    assert_eq!(
        find_function_visibility(&staging, "privateFunc"),
        Some("private".to_string()),
        "privateFunc should be private"
    );
    assert_eq!(
        find_function_visibility(&staging, "protectedFunc"),
        Some("protected".to_string()),
        "protectedFunc should be protected"
    );
    assert_eq!(
        find_function_visibility(&staging, "internalFunc"),
        Some("internal".to_string()),
        "internalFunc should be internal"
    );
    assert_eq!(
        find_function_visibility(&staging, "defaultPublic"),
        Some("public".to_string()),
        "defaultPublic should be public (default)"
    );
}
