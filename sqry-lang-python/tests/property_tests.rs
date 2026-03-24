use sqry_core::graph::GraphBuilder;
/// Property node tests for Python
/// Tests @property decorator detection and property node creation
use sqry_core::graph::Language;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_python::relations::PythonGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_python(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("Error loading Python grammar");
    parser.parse(source, None).expect("Error parsing")
}

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

/// Count property nodes in staging graph.
fn count_property_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddNode {
                    entry,
                    ..
                } if entry.kind == NodeKind::Property
            )
        })
        .count()
}

/// Find a property node by name pattern.
fn find_property_node(staging: &StagingGraph, name_pattern: &str) -> Option<String> {
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Property
            && let Some(node_name) = staging.resolve_node_canonical_name(entry)
            && node_name.contains(name_pattern)
        {
            return Some(node_name.to_string());
        }
    }
    None
}

/// Find a property display/native name by name pattern.
fn find_property_display_name(staging: &StagingGraph, name_pattern: &str) -> Option<String> {
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Property
            && let Some(node_name) = staging.resolve_node_display_name(Language::Python, entry)
            && node_name.contains(name_pattern)
        {
            return Some(node_name);
        }
    }
    None
}

/// Find the visibility of a property by name.
fn find_property_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Property
        {
            let node_name = staging.resolve_node_canonical_name(entry);
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
fn test_property_simple() {
    let source = r"
class User:
    @property
    def name(self):
        return self._name
";
    let tree = parse_python(source);
    let file = Path::new("test_property_simple.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 1,
        "Expected 1 property node for @property decorated method, got {property_count}"
    );

    let property_name = find_property_node(&staging, "name");
    assert!(
        property_name.is_some(),
        "Expected to find property node 'name'"
    );
    assert!(property_name.unwrap().contains("User::name"));

    let display_name = find_property_display_name(&staging, "name");
    assert!(
        display_name.is_some(),
        "Expected to find property display name 'name'"
    );
    assert!(display_name.unwrap().contains("User.name"));
}

#[test]
fn test_property_with_setter() {
    let source = r"
class User:
    @property
    def email(self):
        return self._email

    @email.setter
    def email(self, value):
        self._email = value
";
    let tree = parse_python(source);
    let file = Path::new("test_property_setter.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have 1 property node for getter, 1 method for setter
    // (setter is technically a method, not a property in our model)
    let property_count = count_property_nodes(&staging);
    assert!(
        property_count >= 1,
        "Expected at least 1 property node for @property getter, got {property_count}"
    );
}

#[test]
fn test_property_with_type_annotation() {
    let source = r"
class Person:
    @property
    def age(self) -> int:
        return self._age
";
    let tree = parse_python(source);
    let file = Path::new("test_property_typed.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 1,
        "Expected 1 property node with type annotation, got {property_count}"
    );

    let property_name = find_property_node(&staging, "age");
    assert!(property_name.is_some(), "Expected to find property 'age'");
}

#[test]
fn test_multiple_properties() {
    let source = r"
class Rectangle:
    @property
    def width(self):
        return self._width

    @property
    def height(self):
        return self._height

    @property
    def area(self):
        return self.width * self.height
";
    let tree = parse_python(source);
    let file = Path::new("test_multiple_properties.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 3,
        "Expected 3 property nodes (width, height, area), got {property_count}"
    );

    assert!(find_property_node(&staging, "width").is_some());
    assert!(find_property_node(&staging, "height").is_some());
    assert!(find_property_node(&staging, "area").is_some());
}

#[test]
fn test_property_visibility_public() {
    let source = r"
class Config:
    @property
    def value(self):
        return self._value
";
    let tree = parse_python(source);
    let file = Path::new("test_property_public.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let visibility = find_property_visibility(&staging, "value");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Expected public visibility for property 'value'"
    );
}

#[test]
fn test_property_visibility_protected() {
    let source = r"
class Internal:
    @property
    def _config(self):
        return self.__config
";
    let tree = parse_python(source);
    let file = Path::new("test_property_protected.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let visibility = find_property_visibility(&staging, "_config");
    assert_eq!(
        visibility,
        Some("protected".to_string()),
        "Expected protected visibility for property '_config'"
    );
}

#[test]
fn test_property_mixed_with_methods() {
    let source = r"
class Account:
    @property
    def balance(self):
        return self._balance

    def deposit(self, amount):
        self._balance += amount

    @property
    def status(self):
        return 'active' if self.balance > 0 else 'inactive'
";
    let tree = parse_python(source);
    let file = Path::new("test_mixed_property_methods.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 2,
        "Expected 2 property nodes (balance, status), got {property_count}"
    );

    // Verify method is not counted as property
    let strings = build_string_lookup(&staging);
    let method_count = staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && entry.kind == NodeKind::Method
                && let Some(name) = strings.get(&entry.name.index())
            {
                return name.contains("deposit");
            }
            false
        })
        .count();
    assert_eq!(
        method_count, 1,
        "Expected 1 method node (deposit), got {method_count}"
    );
}

#[test]
fn test_property_in_nested_class() {
    let source = r"
class Outer:
    class Inner:
        @property
        def nested_prop(self):
            return 'nested'
";
    let tree = parse_python(source);
    let file = Path::new("test_nested_property.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 1,
        "Expected 1 property node in nested class, got {property_count}"
    );

    let property_name = find_property_node(&staging, "nested_prop");
    assert!(
        property_name.is_some(),
        "Expected to find nested property 'nested_prop'"
    );
}

#[test]
fn test_regular_method_not_property() {
    let source = r"
class Service:
    def get_data(self):
        return self.data

    def set_data(self, value):
        self.data = value
";
    let tree = parse_python(source);
    let file = Path::new("test_not_property.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 0,
        "Expected 0 property nodes (no @property decorator), got {property_count}"
    );
}

#[test]
fn test_property_with_deleter() {
    let source = r"
class Resource:
    @property
    def handle(self):
        return self._handle

    @handle.setter
    def handle(self, value):
        self._handle = value

    @handle.deleter
    def handle(self):
        del self._handle
";
    let tree = parse_python(source);
    let file = Path::new("test_property_deleter.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert!(
        property_count >= 1,
        "Expected at least 1 property node (getter), got {property_count}"
    );
}

#[test]
fn test_property_with_complex_return_type() {
    let source = r"
from typing import Optional, List

class DataStore:
    @property
    def items(self) -> Optional[List[str]]:
        return self._items
";
    let tree = parse_python(source);
    let file = Path::new("test_property_complex_type.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let property_count = count_property_nodes(&staging);
    assert_eq!(
        property_count, 1,
        "Expected 1 property node with complex type, got {property_count}"
    );
}
