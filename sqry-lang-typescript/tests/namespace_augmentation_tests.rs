use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeId, StagingGraph};
use sqry_lang_typescript::TypeScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Tree;

/// Parse TypeScript source code
fn parse_typescript(source: &str) -> (Tree, Vec<u8>) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("Failed to load TypeScript grammar");

    let content = source.as_bytes().to_vec();
    let tree = parser.parse(&content, None).expect("Failed to parse");
    (tree, content)
}

/// Build string map from staging operations
fn build_string_map(staging: &StagingGraph) -> HashMap<u32, String> {
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

/// Extract Module nodes from staging operations
fn extract_modules(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    let string_map = build_string_map(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Module
            {
                let name_str = string_map
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Function nodes
fn extract_functions(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    let string_map = build_string_map(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Function
            {
                let name_str = string_map
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Class nodes
fn extract_classes(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    let string_map = build_string_map(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Class
            {
                let name_str = string_map
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Interface nodes
fn extract_interfaces(staging: &StagingGraph) -> Vec<(NodeId, String)> {
    let string_map = build_string_map(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == NodeKind::Interface
            {
                let name_str = string_map
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default();
                let node_id = expected_id.unwrap_or(NodeId::new(0, 0));
                Some((node_id, name_str))
            } else {
                None
            }
        })
        .collect()
}

/// Extract Contains edges
fn extract_contains_edges(staging: &StagingGraph) -> Vec<(NodeId, NodeId)> {
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
                && matches!(kind, EdgeKind::Contains)
            {
                Some((*source, *target))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn test_simple_namespace_declaration() {
    let source = r"
        namespace MyApp {
          export function init() {
            return true;
          }
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Assertions
    // Note: We'll have 2 modules - MyApp and test.ts (from export processing)
    let namespace_modules: Vec<_> = modules.iter().filter(|(_, name)| name == "MyApp").collect();
    assert_eq!(namespace_modules.len(), 1, "Expected 1 MyApp Module node");
    assert!(
        modules.iter().any(|(_, name)| name == "MyApp"),
        "Expected Module named 'MyApp'"
    );

    assert!(
        functions.iter().any(|(_, name)| name == "init"),
        "Expected Function named 'init'"
    );

    assert_eq!(contains_edges.len(), 1, "Expected 1 Contains edge");
}

#[test]
fn test_namespace_augmentation() {
    let source = r"
        namespace MyApp {
          export function init() {
            return true;
          }
        }

        namespace MyApp {
          export function shutdown() {
            return false;
          }
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Should have exactly 1 Module (not 2) - augmentation reuses the same namespace
    let namespace_modules: Vec<_> = modules.iter().filter(|(_, name)| name == "MyApp").collect();
    assert_eq!(
        namespace_modules.len(),
        1,
        "Expected exactly 1 MyApp Module node (namespace augmentation should reuse)"
    );

    // Should have 2 functions: init and shutdown
    let func_names: Vec<String> = functions.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        func_names.contains(&"init".to_string()),
        "Expected 'init' function"
    );
    assert!(
        func_names.contains(&"shutdown".to_string()),
        "Expected 'shutdown' function"
    );

    // Should have 2 Contains edges (both functions linked to same namespace)
    assert_eq!(contains_edges.len(), 2, "Expected 2 Contains edges");

    // Verify both edges point from the same namespace NodeId
    let namespace_id = modules[0].0;
    assert!(
        contains_edges.iter().all(|(from, _)| *from == namespace_id),
        "All Contains edges should originate from the same namespace"
    );
}

#[test]
fn test_nested_namespaces() {
    let source = r"
        namespace Outer {
          export namespace Inner {
            export function foo() {}
          }
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Should have 2 Module nodes: Outer and Inner (plus possibly test.ts from exports)
    let namespace_modules: Vec<_> = modules
        .iter()
        .filter(|(_, name)| name == "Outer" || name == "Inner")
        .collect();
    assert_eq!(
        namespace_modules.len(),
        2,
        "Expected 2 namespace Module nodes for nested namespaces"
    );

    let module_names: Vec<String> = modules.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        module_names.contains(&"Outer".to_string()),
        "Expected 'Outer' namespace"
    );
    assert!(
        module_names.contains(&"Inner".to_string()),
        "Expected 'Inner' namespace"
    );

    // Should have 1 function
    assert_eq!(functions.len(), 1, "Expected 1 Function node");

    // Should have at least 1 Contains edge (Inner -> foo)
    assert!(!contains_edges.is_empty(), "Expected Contains edges");
}

#[test]
fn test_nested_namespace_augmentation() {
    let source = r"
        namespace Outer {
          export namespace Inner {
            export function foo() {}
          }
        }

        namespace Outer {
          export namespace Inner {
            export function bar() {}
          }
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);

    // Should have exactly 2 Module nodes (Outer and Inner, not duplicated)
    let namespace_modules: Vec<_> = modules
        .iter()
        .filter(|(_, name)| name == "Outer" || name == "Inner")
        .collect();
    assert_eq!(
        namespace_modules.len(),
        2,
        "Expected 2 namespace Module nodes (augmentation should reuse namespaces)"
    );

    // Should have 2 functions: foo and bar
    assert_eq!(functions.len(), 2, "Expected 2 Function nodes");

    let func_names: Vec<String> = functions.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        func_names.contains(&"foo".to_string()),
        "Expected 'foo' function"
    );
    assert!(
        func_names.contains(&"bar".to_string()),
        "Expected 'bar' function"
    );
}

#[test]
fn test_mixed_member_types() {
    let source = r#"
        namespace Utils {
          export class Helper {}
          export interface Config {}
          export function run() {}
          export const VERSION = "1.0";
        }
    "#;

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let classes = extract_classes(&staging);
    let interfaces = extract_interfaces(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Should have 1 Module (Utils namespace)
    let namespace_modules: Vec<_> = modules.iter().filter(|(_, name)| name == "Utils").collect();
    assert_eq!(namespace_modules.len(), 1, "Expected 1 Utils Module node");

    // Should have 1 Class
    assert!(
        classes.iter().any(|(_, name)| name == "Helper"),
        "Expected Class named 'Helper'"
    );

    // Should have 1 Interface
    assert!(
        interfaces.iter().any(|(_, name)| name == "Config"),
        "Expected Interface named 'Config'"
    );

    // Should have 1 Function
    assert!(
        functions.iter().any(|(_, name)| name == "run"),
        "Expected Function named 'run'"
    );

    // Should have multiple Contains edges (one per member)
    assert!(
        contains_edges.len() >= 3,
        "Expected at least 3 Contains edges"
    );
}

#[test]
fn test_module_keyword() {
    let source = r"
        module LegacyApp {
          export function old() {}
        }

        module LegacyApp {
          export function new() {}
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // module keyword should work identically to namespace
    let namespace_modules: Vec<_> = modules
        .iter()
        .filter(|(_, name)| name == "LegacyApp")
        .collect();
    assert_eq!(
        namespace_modules.len(),
        1,
        "Expected 1 LegacyApp Module node (module augmentation)"
    );

    assert_eq!(functions.len(), 2, "Expected 2 Function nodes");

    let func_names: Vec<String> = functions.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        func_names.contains(&"old".to_string()),
        "Expected 'old' function"
    );
    assert!(
        func_names.contains(&"new".to_string()),
        "Expected 'new' function"
    );

    assert_eq!(contains_edges.len(), 2, "Expected 2 Contains edges");
}

#[test]
fn test_empty_namespace() {
    let source = r"
        namespace Empty {}
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Should have 1 Module
    assert_eq!(
        modules.len(),
        1,
        "Expected 1 Module node for empty namespace"
    );

    // Should have 0 Contains edges (no members)
    assert_eq!(
        contains_edges
            .iter()
            .filter(|(from, _)| *from == modules[0].0)
            .count(),
        0,
        "Expected 0 Contains edges for empty namespace"
    );
}

#[test]
fn test_multiple_augmentations() {
    let source = r"
        namespace App {
          export function a() {}
        }
        namespace App {
          export function b() {}
        }
        namespace App {
          export function c() {}
        }
    ";

    let (tree, content) = parse_typescript(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, &content, Path::new("test.ts"), &mut staging)
        .unwrap();

    // Extract nodes
    let modules = extract_modules(&staging);
    let functions = extract_functions(&staging);
    let contains_edges = extract_contains_edges(&staging);

    // Should have 1 Module (3 augmentations all reuse the same namespace)
    let namespace_modules: Vec<_> = modules.iter().filter(|(_, name)| name == "App").collect();
    assert_eq!(
        namespace_modules.len(),
        1,
        "Expected 1 App Module node (multiple augmentations)"
    );

    // Should have 3 functions
    assert_eq!(functions.len(), 3, "Expected 3 Function nodes");

    let func_names: Vec<String> = functions.iter().map(|(_, name)| name.clone()).collect();
    assert!(
        func_names.contains(&"a".to_string()),
        "Expected 'a' function"
    );
    assert!(
        func_names.contains(&"b".to_string()),
        "Expected 'b' function"
    );
    assert!(
        func_names.contains(&"c".to_string()),
        "Expected 'c' function"
    );

    // Should have 3 Contains edges
    assert_eq!(contains_edges.len(), 3, "Expected 3 Contains edges");
}
