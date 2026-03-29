use crate::Result;
use crate::graph::unified::GraphSnapshot;
use crate::graph::unified::node::NodeId;
use crate::output::diagram::{DiagramEdge, DiagramOptions};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Graph data structure for diagram generation.
#[derive(Debug, Clone)]
pub struct GraphBuilder {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    max_nodes: usize,
    is_truncated: bool,
    node_ids: HashSet<String>,
}

impl GraphBuilder {
    /// Create a new graph builder with the provided node cap.
    #[must_use]
    pub fn new(max_nodes: usize) -> Self {
        let clamped = clamp_max_nodes(max_nodes);
        Self {
            nodes: Vec::with_capacity(clamped.min(64)),
            edges: Vec::new(),
            max_nodes: clamped,
            is_truncated: false,
            node_ids: HashSet::with_capacity(clamped),
        }
    }

    /// Add a node to the builder (ignored if capacity reached).
    ///
    /// # Errors
    ///
    /// Currently always returns `Ok(())`; the `Result` is reserved for future
    /// validations so callers can propagate builder errors without API churn.
    pub fn add_node(&mut self, node: Node) -> Result<()> {
        self.insert_node(node);
        Ok(())
    }

    /// Add a directed edge.
    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// Total number of nodes retained.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Total number of edges retained.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Whether nodes were dropped due to `max_nodes`.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.is_truncated
    }

    /// Access all nodes.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Access all edges.
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Construct a graph builder from graph data.
    ///
    /// # Arguments
    ///
    /// * `snapshot` - Graph snapshot for resolving node data
    /// * `nodes` - Node IDs to include in the diagram
    /// * `edges` - Edges between nodes (using `DiagramEdge`)
    /// * `extra_nodes` - Additional nodes not in the graph (e.g., placeholders)
    /// * `options` - Diagram generation options
    ///
    /// # Errors
    ///
    /// Returns an error if future graph builder validations fail. The current
    /// implementation is infallible and always returns `Ok`.
    pub fn build_from_graph(
        snapshot: &GraphSnapshot,
        nodes: &[NodeId],
        edges: &[DiagramEdge],
        extra_nodes: &[Node],
        options: &DiagramOptions,
    ) -> Result<Self> {
        let mut builder = GraphBuilder::new(options.max_nodes);
        let mut id_lookup: HashMap<NodeId, String> = HashMap::with_capacity(nodes.len() * 2);
        let strings = snapshot.strings();
        let files = snapshot.files();

        // Add graph nodes
        for &node_id in nodes {
            if let Some(node) = node_to_diagram_node(snapshot, node_id) {
                let label = node.label.clone();
                if let Some(assigned_id) = builder.insert_node(node) {
                    id_lookup.insert(node_id, assigned_id.clone());
                    // Also map by label for edge resolution
                    if assigned_id != label {
                        // Label differs from assigned ID (due to sanitization)
                    }
                }
            }
            // Stale NodeIds are silently skipped
        }

        // Add extra nodes (e.g., placeholders for no-match queries)
        for extra in extra_nodes {
            let _ = builder.insert_node(extra.clone());
        }

        // Add edges
        for edge in edges {
            let Some(from_id) = resolve_or_create_diagram_node(
                &mut builder,
                &mut id_lookup,
                snapshot,
                edge.source,
                strings,
                files,
            ) else {
                continue;
            };

            let Some(to_id) = resolve_or_create_diagram_node(
                &mut builder,
                &mut id_lookup,
                snapshot,
                edge.target,
                strings,
                files,
            ) else {
                continue;
            };

            builder.add_edge(Edge {
                from: from_id,
                to: to_id,
                label: edge.label.clone(),
            });
        }

        Ok(builder)
    }

    fn insert_node(&mut self, mut node: Node) -> Option<String> {
        if self.nodes.len() >= self.max_nodes {
            self.is_truncated = true;
            return None;
        }

        let sanitized = sanitize_node_id(&node.id);
        let unique = self.reserve_id(&sanitized);
        node.id.clone_from(&unique);
        self.nodes.push(node);
        Some(unique)
    }

    fn reserve_id(&mut self, base: &str) -> String {
        let mut candidate = if base.is_empty() {
            "_".to_string()
        } else {
            base.to_string()
        };
        let mut suffix = 1usize;

        while self.node_ids.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }

        self.node_ids.insert(candidate.clone());
        candidate
    }
}

/// Convert a graph node to a diagram node.
///
/// Returns `None` if the node ID is stale (refers to a removed/replaced node).
fn node_to_diagram_node(snapshot: &GraphSnapshot, node_id: NodeId) -> Option<Node> {
    let entry = snapshot.get_node(node_id)?;
    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    let label = if qualified_name.is_empty() {
        name
    } else {
        qualified_name
    };

    let file_path = files.resolve(entry.file).map(|p| p.to_path_buf());

    Some(Node {
        id: label.clone(),
        label,
        file_path,
        line: Some(entry.start_line as usize),
    })
}

/// Resolve or create a diagram node for an edge endpoint.
///
/// Returns the assigned node ID if found or created, `None` if the `NodeId` is stale.
/// Caches the result in `id_lookup` for future lookups.
fn resolve_or_create_diagram_node(
    builder: &mut GraphBuilder,
    id_lookup: &mut HashMap<NodeId, String>,
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    strings: &crate::graph::unified::storage::StringInterner,
    files: &crate::graph::unified::storage::FileRegistry,
) -> Option<String> {
    if let Some(id) = id_lookup.get(&node_id) {
        return Some(id.clone());
    }

    let assigned_id = ensure_node_for_edge_graph(builder, snapshot, node_id, strings, files)?;
    id_lookup.insert(node_id, assigned_id.clone());
    Some(assigned_id)
}

/// Ensure a node exists for an edge endpoint, creating it if needed.
///
/// Returns the assigned node ID, or `None` if the `NodeId` is stale.
fn ensure_node_for_edge_graph(
    builder: &mut GraphBuilder,
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    strings: &crate::graph::unified::storage::StringInterner,
    files: &crate::graph::unified::storage::FileRegistry,
) -> Option<String> {
    let entry = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    let label = if qualified_name.is_empty() {
        if name.is_empty() {
            "(unknown)".to_string()
        } else {
            name
        }
    } else {
        qualified_name
    };

    let file_path = files.resolve(entry.file).map(|p| p.to_path_buf());
    let line = Some(entry.start_line as usize);

    let node = Node {
        id: label.clone(),
        label,
        file_path,
        line,
    };

    builder.insert_node(node)
}

fn clamp_max_nodes(value: usize) -> usize {
    value.clamp(1, 500)
}

/// Convert a symbol name into a valid diagram node identifier.
#[must_use]
pub fn sanitize_node_id(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    let mut last_was_separator = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cleaned.push(ch);
            last_was_separator = false;
        } else if matches!(ch, ':' | '.' | ' ' | '-') {
            if !last_was_separator {
                cleaned.push('_');
                last_was_separator = true;
            }
        } else {
            last_was_separator = false;
        }
    }

    if cleaned.is_empty() {
        return "_".to_string();
    }

    if cleaned.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// Escape labels for Mermaid syntax.
#[must_use]
pub fn escape_label_mermaid(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// Escape labels for `GraphViz` DOT syntax.
#[must_use]
pub fn escape_label_graphviz(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// A single node in the generated diagram.
#[derive(Debug, Clone)]
pub struct Node {
    /// Identifier referenced by edges (sanitized, unique).
    pub id: String,
    /// Human-friendly label rendered in the diagram.
    ///
    /// Usually the symbol's qualified name.
    pub label: String,
    /// Optional file path for the symbol represented by the node.
    pub file_path: Option<PathBuf>,
    /// Optional line number for the symbol (1-based).
    pub line: Option<usize>,
}

/// Directed edge between two nodes.
#[derive(Debug, Clone)]
pub struct Edge {
    /// Source node identifier.
    pub from: String,
    /// Target node identifier.
    pub to: String,
    /// Optional label shown alongside the edge (call metadata, etc.).
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_sanitize_node_id_basic() {
        assert_eq!(sanitize_node_id("simple"), "simple");
        assert_eq!(sanitize_node_id("with_underscore"), "with_underscore");
        assert_eq!(sanitize_node_id("MyClass"), "MyClass");
    }

    #[test]
    fn test_sanitize_node_id_special_chars() {
        assert_eq!(sanitize_node_id("std::vec::Vec"), "std_vec_Vec");
        assert_eq!(sanitize_node_id("foo::bar::baz"), "foo_bar_baz");
        assert_eq!(sanitize_node_id("Some<T>"), "SomeT");
        assert_eq!(sanitize_node_id("a.b.c"), "a_b_c");
    }

    #[test]
    fn test_sanitize_node_id_starts_with_number() {
        assert_eq!(sanitize_node_id("123func"), "_123func");
        assert_eq!(sanitize_node_id("0"), "_0");
    }

    #[test]
    fn test_sanitize_node_id_unicode() {
        assert_eq!(sanitize_node_id("函数名"), "_");
        assert_eq!(sanitize_node_id("func_µ"), "func_");
        let _ = sanitize_node_id("🚀🔥💯");
    }

    #[test]
    fn test_sanitize_node_id_empty() {
        assert_eq!(sanitize_node_id(""), "_");
        assert_eq!(sanitize_node_id("!!!"), "_");
    }

    #[test]
    fn test_escape_label_mermaid_behavior() {
        let escaped = escape_label_mermaid("with \"quotes\"");
        assert!(escaped.contains("\\\""));

        let escaped = escape_label_mermaid("line\nbreak");
        assert!(escaped.contains("\\n"));

        let escaped = escape_label_mermaid("path\\segment");
        assert!(escaped.contains("\\\\"));
    }

    #[test]
    fn test_escape_label_graphviz_behavior() {
        let escaped = escape_label_graphviz("with \"quotes\"");
        assert!(escaped.contains("\\\""));

        let escaped = escape_label_graphviz("line\nbreak");
        assert!(escaped.contains("\\n"));
    }

    #[test]
    fn test_graph_builder_empty() {
        let builder = GraphBuilder::new(100);
        assert_eq!(builder.node_count(), 0);
        assert_eq!(builder.edge_count(), 0);
        assert!(!builder.is_truncated());
    }

    #[test]
    fn test_graph_builder_add_nodes() {
        let mut builder = GraphBuilder::new(100);
        let node = Node {
            id: "node1".into(),
            label: "Function 1".into(),
            file_path: None,
            line: None,
        };
        builder.add_node(node).unwrap();

        assert_eq!(builder.node_count(), 1);
    }

    #[test]
    fn test_graph_builder_max_nodes() {
        let mut builder = GraphBuilder::new(5);

        for i in 0..10 {
            let node = Node {
                id: format!("node{i}"),
                label: format!("Function {i}"),
                file_path: None,
                line: None,
            };
            let _ = builder.add_node(node);
        }

        assert_eq!(builder.node_count(), 5);
        assert!(builder.is_truncated());
    }

    #[test]
    fn test_duplicate_node_names_produce_unique_ids() {
        let mut builder = GraphBuilder::new(100);
        for i in 0..3 {
            let node = Node {
                id: "dup".to_string(),
                label: format!("Function {i}"),
                file_path: None,
                line: Some(i + 1),
            };
            let _ = builder.add_node(node);
        }
        let ids: std::collections::HashSet<_> =
            builder.nodes().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids.len(), 3);
    }

    proptest! {
        #[test]
        fn test_sanitize_node_id_never_panics(input in ".*") {
            let result = sanitize_node_id(&input);
            assert!(!result.is_empty());
        }

        #[test]
        fn test_escape_label_graphviz_never_panics(input in ".*") {
            let _ = escape_label_graphviz(&input);
        }
    }
}
