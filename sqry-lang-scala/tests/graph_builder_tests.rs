/// Integration tests for Scala `GraphBuilder`
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_scala::ScalaGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_scala(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .expect("Error loading Scala grammar");
    parser.parse(source, None).expect("Error parsing")
}

// ============================================================================
// Visibility Metadata Tests
// ============================================================================

/// Build a string lookup table from staging operations.
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

/// Find the visibility of a method by name.
fn find_method_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Method
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

/// Find the visibility of a class by name.
fn find_class_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Class
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
fn test_method_visibility_public() {
    let source = r"
class Foo {
  def bar(): Unit = {}
}
";
    let tree = parse_scala(source);
    let file = Path::new("test_visibility_public.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_method_visibility(&staging, "bar");
    assert_eq!(visibility, Some("public".to_string()));
}

#[test]
fn test_method_visibility_private() {
    let source = r"
class Foo {
  private def bar(): Unit = {}
}
";
    let tree = parse_scala(source);
    let file = Path::new("test_visibility_private.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_method_visibility(&staging, "bar");
    assert_eq!(visibility, Some("private".to_string()));
}

#[test]
fn test_method_visibility_protected() {
    let source = r"
class Foo {
  protected def bar(): Unit = {}
}
";
    let tree = parse_scala(source);
    let file = Path::new("test_visibility_protected.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_method_visibility(&staging, "bar");
    assert_eq!(visibility, Some("protected".to_string()));
}

#[test]
fn test_class_visibility_public() {
    let source = r"
class Foo {}
";
    let tree = parse_scala(source);
    let file = Path::new("test_class_public.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_class_visibility(&staging, "Foo");
    assert_eq!(visibility, Some("public".to_string()));
}

#[test]
fn test_class_visibility_private() {
    let source = r"
private class Foo {}
";
    let tree = parse_scala(source);
    let file = Path::new("test_class_private.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_class_visibility(&staging, "Foo");
    assert_eq!(visibility, Some("private".to_string()));
}

#[test]
fn test_mixed_visibility() {
    let source = r#"
class User {
  def getName(): String = ""
  private def validateEmail(): Boolean = true
  protected def hashPassword(): String = ""
}
"#;
    let tree = parse_scala(source);
    let file = Path::new("test_mixed_visibility.scala");
    let mut staging = StagingGraph::new();
    let builder = ScalaGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert_eq!(
        find_method_visibility(&staging, "getName"),
        Some("public".to_string())
    );
    assert_eq!(
        find_method_visibility(&staging, "validateEmail"),
        Some("private".to_string())
    );
    assert_eq!(
        find_method_visibility(&staging, "hashPassword"),
        Some("protected".to_string())
    );
}
