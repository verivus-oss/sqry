use sqry_core::graph::GraphBuilder;
use sqry_core::graph::Language;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeId, StagingGraph};
use sqry_lang_elixir::ElixirGraphBuilder;
use std::path::Path;
use tree_sitter::Tree;

/// Parse Elixir source code
fn parse_elixir(source: &str) -> (Tree, Vec<u8>) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_elixir_sqry::language())
        .expect("Failed to load Elixir grammar");

    let content = source.as_bytes().to_vec();
    let tree = parser.parse(&content, None).expect("Failed to parse");
    (tree, content)
}

/// Extract Interface nodes from staging operations (protocols)
fn extract_interfaces(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Interface
            {
                let name_str = staging
                    .resolve_node_display_name(Language::Elixir, entry)
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Struct nodes from staging operations (protocol implementations)
fn extract_structs(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Struct
            {
                let name_str = staging
                    .resolve_node_display_name(Language::Elixir, entry)
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Implements edges from staging operations
fn extract_implements_edges(staging: &StagingGraph) -> Vec<(NodeId, NodeId)> {
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
                && matches!(kind, EdgeKind::Implements)
            {
                Some((*source, *target))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn test_simple_protocol_definition() {
    let source = r#"
        defprotocol Stringify do
          def to_string(data)
        end
    "#;

    let (tree, content) = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
        .unwrap();

    let interfaces = extract_interfaces(&staging);
    assert_eq!(interfaces.len(), 1, "Expected 1 Interface node (protocol)");

    let (_, name) = &interfaces[0];
    assert_eq!(name, "Stringify", "Protocol should be named 'Stringify'");
}

#[test]
fn test_protocol_implementation() {
    let source = r#"
        defprotocol Stringify do
          def to_string(data)
        end

        defimpl Stringify, for: Integer do
          def to_string(data) do
            Integer.to_string(data)
          end
        end
    "#;

    let (tree, content) = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
        .unwrap();

    let interfaces = extract_interfaces(&staging);
    let structs = extract_structs(&staging);
    let implements_edges = extract_implements_edges(&staging);

    assert_eq!(
        interfaces.len(),
        1,
        "Expected 1 Interface node (protocol definition)"
    );
    assert_eq!(
        structs.len(),
        1,
        "Expected 1 Struct node (protocol implementation)"
    );
    assert_eq!(
        implements_edges.len(),
        1,
        "Expected 1 Implements edge linking implementation to protocol"
    );

    let (_, struct_name) = &structs[0];
    assert_eq!(
        struct_name, "Stringify.Integer",
        "Implementation should be named 'Stringify.Integer'"
    );
}

#[test]
fn test_multiple_protocol_implementations() {
    let source = r#"
        defprotocol Stringify do
          def to_string(data)
        end

        defimpl Stringify, for: Integer do
          def to_string(data), do: Integer.to_string(data)
        end

        defimpl Stringify, for: List do
          def to_string(data), do: inspect(data)
        end

        defimpl Stringify, for: Map do
          def to_string(data), do: inspect(data)
        end
    "#;

    let (tree, content) = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
        .unwrap();

    let interfaces = extract_interfaces(&staging);
    let structs = extract_structs(&staging);
    let implements_edges = extract_implements_edges(&staging);

    assert_eq!(interfaces.len(), 1, "Expected 1 Interface node (protocol)");
    assert_eq!(
        structs.len(),
        3,
        "Expected 3 Struct nodes (implementations)"
    );
    assert_eq!(implements_edges.len(), 3, "Expected 3 Implements edges");

    let struct_names: Vec<String> = structs.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        struct_names.contains(&"Stringify.Integer".to_string()),
        "Expected implementation for Integer"
    );
    assert!(
        struct_names.contains(&"Stringify.List".to_string()),
        "Expected implementation for List"
    );
    assert!(
        struct_names.contains(&"Stringify.Map".to_string()),
        "Expected implementation for Map"
    );
}

#[test]
fn test_multiple_protocols() {
    let source = r#"
        defprotocol Stringify do
          def to_string(data)
        end

        defprotocol Enumerable do
          def count(data)
          def member?(data, value)
        end

        defimpl Stringify, for: Integer do
          def to_string(data), do: Integer.to_string(data)
        end

        defimpl Enumerable, for: List do
          def count(list), do: length(list)
          def member?(list, value), do: value in list
        end
    "#;

    let (tree, content) = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
        .unwrap();

    let interfaces = extract_interfaces(&staging);
    let structs = extract_structs(&staging);
    let implements_edges = extract_implements_edges(&staging);

    assert_eq!(
        interfaces.len(),
        2,
        "Expected 2 Interface nodes (protocols)"
    );
    assert_eq!(
        structs.len(),
        2,
        "Expected 2 Struct nodes (implementations)"
    );
    assert_eq!(implements_edges.len(), 2, "Expected 2 Implements edges");

    let protocol_names: Vec<String> = interfaces.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        protocol_names.contains(&"Stringify".to_string()),
        "Expected Stringify protocol"
    );
    assert!(
        protocol_names.contains(&"Enumerable".to_string()),
        "Expected Enumerable protocol"
    );
}

#[test]
fn test_impl_without_protocol_definition() {
    // Test that implementation works even if protocol is not defined in same file
    let source = r#"
        defimpl String.Chars, for: MyStruct do
          def to_string(_), do: "MyStruct"
        end
    "#;

    let (tree, content) = parse_elixir(source);
    let mut staging = StagingGraph::new();
    let builder = ElixirGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ex"), &mut staging)
        .unwrap();

    let interfaces = extract_interfaces(&staging);
    let structs = extract_structs(&staging);
    let implements_edges = extract_implements_edges(&staging);

    // Should create the protocol interface even though it's not explicitly defined
    assert_eq!(
        interfaces.len(),
        1,
        "Expected 1 Interface node (external protocol)"
    );
    assert_eq!(structs.len(), 1, "Expected 1 Struct node (implementation)");
    assert_eq!(implements_edges.len(), 1, "Expected 1 Implements edge");
}
