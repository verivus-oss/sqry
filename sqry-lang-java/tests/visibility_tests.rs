use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_java::relations::JavaGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_java(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("Failed to set Java language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse Java code")
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

#[test]
fn test_method_visibility_public() {
    let source = r"
public class TestClass {
    public void publicMethod() {
        // body
    }
}
";
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Test.java"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_method_visibility(&staging, "publicMethod");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "public method should have public visibility"
    );
}

#[test]
fn test_method_visibility_private() {
    let source = r"
public class TestClass {
    private void privateMethod() {
        // body
    }
}
";
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Test.java"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_method_visibility(&staging, "privateMethod");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "private method should have private visibility"
    );
}

#[test]
fn test_method_visibility_protected() {
    let source = r"
public class TestClass {
    protected void protectedMethod() {
        // body
    }
}
";
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Test.java"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_method_visibility(&staging, "protectedMethod");
    assert_eq!(
        visibility,
        Some("protected".to_string()),
        "protected method should have protected visibility"
    );
}

#[test]
fn test_method_visibility_package_private() {
    let source = r"
public class TestClass {
    void packagePrivateMethod() {
        // body
    }
}
";
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Test.java"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_method_visibility(&staging, "packagePrivateMethod");
    assert_eq!(
        visibility,
        Some("package-private".to_string()),
        "method without modifier should have package-private visibility"
    );
}

#[test]
fn test_method_visibility_mixed() {
    let source = r"
public class TestClass {
    public void publicMethod() {}
    private void privateMethod() {}
    protected void protectedMethod() {}
    void packagePrivateMethod() {}
}
";
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("Test.java"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    assert_eq!(
        find_method_visibility(&staging, "publicMethod"),
        Some("public".to_string()),
        "publicMethod should be public"
    );
    assert_eq!(
        find_method_visibility(&staging, "privateMethod"),
        Some("private".to_string()),
        "privateMethod should be private"
    );
    assert_eq!(
        find_method_visibility(&staging, "protectedMethod"),
        Some("protected".to_string()),
        "protectedMethod should be protected"
    );
    assert_eq!(
        find_method_visibility(&staging, "packagePrivateMethod"),
        Some("package-private".to_string()),
        "packagePrivateMethod should be package-private"
    );
}
