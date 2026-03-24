//! Unused symbol detection for unified graph.
//!
//! This module provides unused/dead code detection using the unified graph API,
//! replacing the legacy index-based unused detection.

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::{NodeId, NodeKind};
use std::collections::HashSet;
use std::hash::BuildHasher;

/// Scope for unused analysis
///
/// Determines which symbols are candidates for unused detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusedScope {
    /// Public symbols with no external references
    Public,
    /// Private symbols with no references
    Private,
    /// Unused functions (any visibility)
    Function,
    /// Unused structs/types (any visibility)
    Struct,
    /// All unused symbols
    All,
}

impl UnusedScope {
    /// Parse scope from query value string
    #[must_use]
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            "function" => Some(Self::Function),
            "struct" => Some(Self::Struct),
            "all" | "" => Some(Self::All),
            _ => None,
        }
    }
}

/// Check if a node entry qualifies as an entry point for reachability analysis.
///
/// A node is an entry point if it is:
/// - Public (visibility = "public" or "pub")
/// - A main function (name = "main")
/// - A test function (name starts with "test_" or ends with "_test")
/// - A `Test` or `Export` node kind
fn is_entry_point_node(
    entry: &crate::graph::unified::storage::arena::NodeEntry,
    strings: &crate::graph::unified::storage::StringInterner,
) -> bool {
    let is_public = entry
        .visibility
        .and_then(|id| strings.resolve(id))
        .is_some_and(|v| v.as_ref() == "public" || v.as_ref() == "pub");

    let is_main_or_test = strings.resolve(entry.name).is_some_and(|name| {
        name.as_ref() == "main" || name.starts_with("test_") || name.ends_with("_test")
    });

    let is_export = matches!(entry.kind, NodeKind::Export);
    let is_test_node = matches!(entry.kind, NodeKind::Test);

    is_public || is_main_or_test || is_export || is_test_node
}

/// Check if an edge kind propagates reachability during BFS traversal.
fn is_reachability_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls { .. }
            | EdgeKind::References
            | EdgeKind::Imports { .. }
            | EdgeKind::Inherits
            | EdgeKind::Implements
            | EdgeKind::TypeOf { .. }
    )
}

/// Compute the set of reachable nodes from entry points via BFS traversal.
///
/// Entry points are automatically identified as:
/// - Public functions/methods (visibility = "public" or "pub")
/// - Main functions (name = "main")
/// - Test functions (name starts with "test_" or ends with "_test")
/// - Nodes of kind `Test` or `Export`
///
/// The traversal follows all outgoing edges to discover transitively reachable nodes.
///
/// # Arguments
///
/// * `graph` - The code graph to analyze
///
/// # Returns
///
/// A `HashSet` of `NodeId`s representing all nodes reachable from entry points.
#[must_use]
pub fn compute_reachable_set_graph(graph: &CodeGraph) -> HashSet<NodeId> {
    let mut reachable = HashSet::new();
    let mut worklist: Vec<NodeId> = Vec::new();

    let strings = graph.strings();

    // Find entry points
    for (node_id, entry) in graph.nodes().iter() {
        if is_entry_point_node(entry, strings) {
            worklist.push(node_id);
            reachable.insert(node_id);
        }
    }

    // BFS to find all reachable nodes
    while let Some(node_id) = worklist.pop() {
        for edge in graph.edges().edges_from(node_id) {
            if !reachable.contains(&edge.target) && is_reachability_edge(&edge.kind) {
                reachable.insert(edge.target);
                worklist.push(edge.target);
            }
        }
    }

    reachable
}

/// Check if a specific node is unused based on the scope filter.
///
/// A node is considered unused if:
/// 1. It matches the scope filter (All, Public, Private, Function, or Struct)
/// 2. It is not an entry point (main, test, or export)
/// 3. It is not reachable from any entry point
///
/// # Arguments
///
/// * `node_id` - The node to check
/// * `scope` - Filter for which types of nodes to check
/// * `graph` - The code graph to analyze
/// * `reachable` - Optional pre-computed reachable set (for efficiency when checking many nodes)
///
/// # Returns
///
/// `true` if the node is unused and matches the scope, `false` otherwise.
/// Returns `false` for invalid or non-existent nodes.
#[must_use]
pub fn is_node_unused<S: BuildHasher>(
    node_id: NodeId,
    scope: UnusedScope,
    graph: &CodeGraph,
    reachable: Option<&HashSet<NodeId, S>>,
) -> bool {
    let Some(entry) = graph.nodes().get(node_id) else {
        return false;
    };

    let strings = graph.strings();

    // Check scope filter
    let matches_scope = match scope {
        UnusedScope::All => true,
        UnusedScope::Public => entry
            .visibility
            .and_then(|id| strings.resolve(id))
            .is_some_and(|v| v.as_ref() == "public" || v.as_ref() == "pub"),
        UnusedScope::Private => {
            let vis = entry.visibility.and_then(|id| strings.resolve(id));
            vis.is_none() || vis.is_some_and(|v| v.as_ref() != "public" && v.as_ref() != "pub")
        }
        UnusedScope::Function => {
            matches!(entry.kind, NodeKind::Function | NodeKind::Method)
        }
        UnusedScope::Struct => {
            matches!(entry.kind, NodeKind::Struct | NodeKind::Class)
        }
    };

    if !matches_scope {
        return false;
    }

    // Skip entry points (they're always "used")
    let is_entry_point = {
        let is_main_or_test = strings.resolve(entry.name).is_some_and(|name| {
            name.as_ref() == "main" || name.starts_with("test_") || name.ends_with("_test")
        });

        let is_export = matches!(entry.kind, NodeKind::Export);
        let is_test_node = matches!(entry.kind, NodeKind::Test);

        is_main_or_test || is_export || is_test_node
    };

    if is_entry_point {
        return false;
    }

    // Check reachability
    if let Some(reachable_set) = reachable {
        return !reachable_set.contains(&node_id);
    }

    let reachable_set = compute_reachable_set_graph(graph);
    !reachable_set.contains(&node_id)
}

/// Find all unused nodes in the graph that match the given scope.
///
/// This function first computes the reachable set once, then iterates through
/// all nodes checking which are unused and match the scope filter.
///
/// # Arguments
///
/// * `scope` - Filter for which types of nodes to check (All, Public, Private, Function, Struct)
/// * `graph` - The code graph to analyze
/// * `max_results` - Maximum number of unused nodes to return
///
/// # Returns
///
/// A vector of `NodeId`s for unused nodes, limited by `max_results`.
#[must_use]
pub fn find_unused_nodes(scope: UnusedScope, graph: &CodeGraph, max_results: usize) -> Vec<NodeId> {
    let reachable = compute_reachable_set_graph(graph);
    let mut unused = Vec::new();

    for (node_id, _entry) in graph.nodes().iter() {
        if unused.len() >= max_results {
            break;
        }

        if is_node_unused(node_id, scope, graph, Some(&reachable)) {
            unused.push(node_id);
        }
    }

    unused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::collections::hash_map::RandomState;
    use std::path::Path;

    /// Options for creating test nodes.
    struct NodeOptions<'a> {
        name: &'a str,
        kind: NodeKind,
        visibility: Option<&'a str>,
    }

    impl<'a> NodeOptions<'a> {
        fn new(name: &'a str, kind: NodeKind) -> Self {
            Self {
                name,
                kind,
                visibility: None,
            }
        }

        fn with_visibility(mut self, vis: &'a str) -> Self {
            self.visibility = Some(vis);
            self
        }
    }

    /// Helper to create a test graph with configurable nodes and edges.
    fn create_test_graph(
        nodes: &[NodeOptions],
        edges: &[(usize, usize, EdgeKind)],
    ) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let mut node_ids = Vec::new();

        for opts in nodes {
            let name_id = graph.strings_mut().intern(opts.name).unwrap();
            let mut entry =
                NodeEntry::new(opts.kind, name_id, file_id).with_qualified_name(name_id);

            if let Some(vis) = opts.visibility {
                let vis_id = graph.strings_mut().intern(vis).unwrap();
                entry = entry.with_visibility(vis_id);
            }

            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        for (source_idx, target_idx, kind) in edges {
            let source = node_ids[*source_idx];
            let target = node_ids[*target_idx];
            graph
                .edges_mut()
                .add_edge(source, target, kind.clone(), file_id);
        }

        (graph, node_ids)
    }

    #[test]
    fn test_empty_graph() {
        let graph = CodeGraph::new();
        let reachable = compute_reachable_set_graph(&graph);
        assert!(reachable.is_empty());
    }

    #[test]
    fn test_find_unused_empty() {
        let graph = CodeGraph::new();
        let unused = find_unused_nodes(UnusedScope::All, &graph, 100);
        assert!(unused.is_empty());
    }

    #[test]
    fn test_public_function_is_entry_point() {
        // Public function should be reachable
        let nodes = [NodeOptions::new("public_func", NodeKind::Function).with_visibility("public")];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[0]),
            "Public function should be reachable"
        );
    }

    #[test]
    fn test_main_function_is_entry_point() {
        // main function should be reachable regardless of visibility
        let nodes = [NodeOptions::new("main", NodeKind::Function)];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[0]),
            "main function should be reachable"
        );
    }

    #[test]
    fn test_test_function_is_entry_point() {
        // test_ prefixed functions should be reachable
        let nodes = [
            NodeOptions::new("test_something", NodeKind::Function),
            NodeOptions::new("something_test", NodeKind::Function),
            NodeOptions::new("TestNode", NodeKind::Test),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[0]),
            "test_ prefixed function should be reachable"
        );
        assert!(
            reachable.contains(&node_ids[1]),
            "_test suffixed function should be reachable"
        );
        assert!(
            reachable.contains(&node_ids[2]),
            "Test node kind should be reachable"
        );
    }

    #[test]
    fn test_export_is_entry_point() {
        let nodes = [NodeOptions::new("exported_value", NodeKind::Export)];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[0]),
            "Export should be reachable"
        );
    }

    #[test]
    fn test_reachability_through_calls() {
        // Entry point -> A -> B (all should be reachable)
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("helper_a", NodeKind::Function),
            NodeOptions::new("helper_b", NodeKind::Function),
        ];
        let edges = [
            (
                0,
                1,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            ),
            (
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            ),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(reachable.contains(&node_ids[0]), "main should be reachable");
        assert!(
            reachable.contains(&node_ids[1]),
            "helper_a should be reachable via call"
        );
        assert!(
            reachable.contains(&node_ids[2]),
            "helper_b should be reachable via transitive call"
        );
    }

    #[test]
    fn test_reachability_through_imports() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("imported_module", NodeKind::Module),
        ];
        let edges = [(
            0,
            1,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
        )];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[1]),
            "Imported module should be reachable"
        );
    }

    #[test]
    fn test_reachability_through_references() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("referenced_var", NodeKind::Variable),
        ];
        let edges = [(0, 1, EdgeKind::References)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[1]),
            "Referenced variable should be reachable"
        );
    }

    #[test]
    fn test_reachability_through_inheritance() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("BaseClass", NodeKind::Class).with_visibility("public"),
            NodeOptions::new("DerivedClass", NodeKind::Class),
        ];
        let edges = [(2, 1, EdgeKind::Inherits)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        // BaseClass is public, so it's an entry point
        // DerivedClass inherits from BaseClass, making it reachable
        assert!(
            reachable.contains(&node_ids[1]),
            "BaseClass should be reachable (public)"
        );
        // Note: inheritance edge goes from derived -> base, so derived is not reachable
        // unless it's also called/used from an entry point
    }

    #[test]
    fn test_private_function_unreachable() {
        // Private function not called by anyone should be unused
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("private_helper", NodeKind::Function),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            !reachable.contains(&node_ids[1]),
            "Uncalled private function should be unreachable"
        );
    }

    #[test]
    fn test_is_node_unused_scope_all() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("unused_helper", NodeKind::Function),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        assert!(
            !is_node_unused::<RandomState>(node_ids[0], UnusedScope::All, &graph, None),
            "main should not be unused"
        );
        assert!(
            is_node_unused::<RandomState>(node_ids[1], UnusedScope::All, &graph, None),
            "unused_helper should be unused"
        );
    }

    #[test]
    fn test_is_node_unused_scope_public() {
        let nodes = [
            NodeOptions::new("public_unused", NodeKind::Function).with_visibility("public"),
            NodeOptions::new("private_unused", NodeKind::Function),
        ];
        // Neither is called, but we check scope filtering
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        // Public scope - only checks public symbols
        // Note: public symbols are entry points, so they're reachable but scope still matches
        assert!(
            !is_node_unused::<RandomState>(node_ids[0], UnusedScope::Public, &graph, None),
            "Public function is entry point, not unused"
        );
        assert!(
            !is_node_unused::<RandomState>(node_ids[1], UnusedScope::Public, &graph, None),
            "Private function doesn't match Public scope"
        );
    }

    #[test]
    fn test_is_node_unused_scope_private() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("public_unused", NodeKind::Function).with_visibility("public"),
            NodeOptions::new("private_unused", NodeKind::Function),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        // Private scope - only checks private symbols
        assert!(
            !is_node_unused::<RandomState>(node_ids[1], UnusedScope::Private, &graph, None),
            "Public function doesn't match Private scope"
        );
        assert!(
            is_node_unused::<RandomState>(node_ids[2], UnusedScope::Private, &graph, None),
            "Private unused function should be unused"
        );
    }

    #[test]
    fn test_is_node_unused_scope_function() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("unused_func", NodeKind::Function),
            NodeOptions::new("unused_struct", NodeKind::Struct),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        // Function scope - only checks functions/methods
        assert!(
            is_node_unused::<RandomState>(node_ids[1], UnusedScope::Function, &graph, None),
            "Unused function should match Function scope"
        );
        assert!(
            !is_node_unused::<RandomState>(node_ids[2], UnusedScope::Function, &graph, None),
            "Struct doesn't match Function scope"
        );
    }

    #[test]
    fn test_is_node_unused_scope_struct() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("unused_struct", NodeKind::Struct),
            NodeOptions::new("unused_class", NodeKind::Class),
            NodeOptions::new("unused_func", NodeKind::Function),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        // Struct scope - only checks structs/classes
        assert!(
            is_node_unused::<RandomState>(node_ids[1], UnusedScope::Struct, &graph, None),
            "Unused struct should match Struct scope"
        );
        assert!(
            is_node_unused::<RandomState>(node_ids[2], UnusedScope::Struct, &graph, None),
            "Unused class should match Struct scope"
        );
        assert!(
            !is_node_unused::<RandomState>(node_ids[3], UnusedScope::Struct, &graph, None),
            "Function doesn't match Struct scope"
        );
    }

    #[test]
    fn test_find_unused_nodes_basic() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("used_helper", NodeKind::Function),
            NodeOptions::new("unused_helper", NodeKind::Function),
        ];
        let edges = [(
            0,
            1,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
        )];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let unused = find_unused_nodes(UnusedScope::All, &graph, 100);
        assert_eq!(unused.len(), 1);
        assert!(unused.contains(&node_ids[2]));
    }

    #[test]
    fn test_find_unused_nodes_max_results() {
        // Create many unused nodes and verify limit is respected
        let mut nodes = vec![NodeOptions::new("main", NodeKind::Function)];
        for i in 0..10 {
            nodes.push(NodeOptions::new(
                Box::leak(format!("unused_{i}").into_boxed_str()),
                NodeKind::Function,
            ));
        }
        let (graph, _) = create_test_graph(&nodes, &[]);

        let unused = find_unused_nodes(UnusedScope::All, &graph, 3);
        assert_eq!(unused.len(), 3, "Should respect max_results limit");
    }

    #[test]
    fn test_entry_point_not_unused() {
        // Entry points should never be marked as unused
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("test_something", NodeKind::Function),
            NodeOptions::new("exported", NodeKind::Export),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        for &node_id in &node_ids {
            assert!(
                !is_node_unused::<RandomState>(node_id, UnusedScope::All, &graph, None),
                "Entry points should not be unused"
            );
        }
    }

    #[test]
    fn test_precomputed_reachable_set() {
        // Test using a pre-computed reachable set for efficiency
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("unused", NodeKind::Function),
        ];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            is_node_unused::<RandomState>(node_ids[1], UnusedScope::All, &graph, Some(&reachable)),
            "Should work with precomputed reachable set"
        );
    }

    #[test]
    fn test_invalid_node_not_unused() {
        let graph = CodeGraph::new();
        let invalid_node = NodeId::new(999, 0);

        assert!(
            !is_node_unused::<RandomState>(invalid_node, UnusedScope::All, &graph, None),
            "Invalid node should return false (not unused)"
        );
    }

    #[test]
    fn test_typeof_edge_reachability() {
        let nodes = [
            NodeOptions::new("main", NodeKind::Function),
            NodeOptions::new("MyType", NodeKind::Type),
        ];
        let edges = [(
            0,
            1,
            EdgeKind::TypeOf {
                context: None,
                index: None,
                name: None,
            },
        )];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[1]),
            "TypeOf edge should make type reachable"
        );
    }

    #[test]
    fn test_implements_edge_reachability() {
        let nodes = [
            NodeOptions::new("MyClass", NodeKind::Class).with_visibility("public"),
            NodeOptions::new("MyInterface", NodeKind::Interface),
        ];
        let edges = [(0, 1, EdgeKind::Implements)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let reachable = compute_reachable_set_graph(&graph);
        // MyClass is public (entry point), and implements MyInterface
        assert!(
            reachable.contains(&node_ids[1]),
            "Implements edge should make interface reachable"
        );
    }

    #[test]
    fn test_pub_visibility_variant() {
        // Test "pub" (Rust style) vs "public" visibility
        let nodes = [NodeOptions::new("rust_public", NodeKind::Function).with_visibility("pub")];
        let (graph, node_ids) = create_test_graph(&nodes, &[]);

        let reachable = compute_reachable_set_graph(&graph);
        assert!(
            reachable.contains(&node_ids[0]),
            "'pub' visibility should be recognized as public"
        );
    }
}
