//! Diagram output formatters (Mermaid, GraphViz, D2).
//!
//! The types in this module convert relation query results into
//! diagram-friendly representations for offline text generation.

mod builder;
mod d2;
mod graphviz;
mod mermaid;

pub use builder::{
    Edge, GraphBuilder, Node, escape_label_graphviz, escape_label_mermaid, sanitize_node_id,
};
pub use d2::D2Formatter;
pub use graphviz::GraphVizFormatter;
pub use mermaid::MermaidFormatter;

use crate::Result;
use crate::graph::unified::GraphSnapshot;
use crate::graph::unified::node::NodeId;
use anyhow::anyhow;
use std::fmt;
use std::str::FromStr;

/// Supported text-based diagram formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramFormat {
    /// Mermaid `graph` syntax.
    Mermaid,
    /// `GraphViz` DOT graphs.
    GraphViz,
    /// D2 diagrams.
    D2,
}

impl DiagramFormat {
    /// Parse a diagram format from user input (e.g., CLI flag).
    ///
    /// # Errors
    ///
    /// Returns an error when the provided string does not match any supported
    /// format (Mermaid, GraphViz/DOT, or D2).
    pub fn parse_format(value: &str) -> Result<Self> {
        value.parse()
    }

    /// File extension typically used for this diagram format.
    #[must_use]
    pub fn file_extension(&self) -> &'static str {
        match self {
            DiagramFormat::Mermaid => "mmd",
            DiagramFormat::GraphViz => "dot",
            DiagramFormat::D2 => "d2",
        }
    }
}

impl fmt::Display for DiagramFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiagramFormat::Mermaid => write!(f, "mermaid"),
            DiagramFormat::GraphViz => write!(f, "graphviz"),
            DiagramFormat::D2 => write!(f, "d2"),
        }
    }
}

impl FromStr for DiagramFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "mermaid" | "mmd" => Ok(DiagramFormat::Mermaid),
            "graphviz" | "dot" => Ok(DiagramFormat::GraphViz),
            "d2" => Ok(DiagramFormat::D2),
            other => Err(anyhow!("unknown diagram format: {other}")),
        }
    }
}

/// Graph intent that determines which relations are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphType {
    /// Function callers/callees graph.
    CallGraph,
    /// File/module dependency graph (imports/exports).
    DependencyGraph,
    /// Type hierarchy graph (inheritance/implementation).
    TypeHierarchy,
}

/// Layout direction for diagram rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Top-to-bottom layout (Mermaid `TB`, DOT `TB`).
    #[default]
    TopDown,
    /// Bottom-to-top layout (Mermaid `BT`).
    BottomUp,
    /// Left-to-right layout (Mermaid/DOT `LR`).
    LeftRight,
    /// Right-to-left layout (Mermaid `RL`).
    RightLeft,
}

/// Options that control diagram generation.
#[derive(Debug, Clone)]
pub struct DiagramOptions {
    /// Desired output format.
    pub format: DiagramFormat,
    /// Relation type to visualize.
    pub graph_type: GraphType,
    /// Optional maximum traversal depth (None = unlimited).
    pub max_depth: Option<usize>,
    /// Maximum number of nodes allowed in generated graph.
    pub max_nodes: usize,
    /// Whether to include file paths/line numbers in node labels.
    pub include_file_paths: bool,
    /// Edge layout direction preference.
    pub direction: Direction,
}

impl Default for DiagramOptions {
    fn default() -> Self {
        Self {
            format: DiagramFormat::Mermaid,
            graph_type: GraphType::CallGraph,
            max_depth: Some(3),
            max_nodes: 100,
            include_file_paths: true,
            direction: Direction::TopDown,
        }
    }
}

/// Edge input for diagram generation.
///
/// Uses `NodeId` references directly from the graph.
#[derive(Debug, Clone)]
pub struct DiagramEdge {
    /// Source node ID from the graph.
    pub source: NodeId,
    /// Target node ID from the graph.
    pub target: NodeId,
    /// Optional edge label (e.g., "async", "as alias", "*").
    pub label: Option<String>,
}

impl DiagramEdge {
    /// Create a new edge without a label.
    #[must_use]
    pub fn new(source: NodeId, target: NodeId) -> Self {
        Self {
            source,
            target,
            label: None,
        }
    }

    /// Create a new edge with a label.
    #[must_use]
    pub fn with_label(source: NodeId, target: NodeId, label: impl Into<String>) -> Self {
        Self {
            source,
            target,
            label: Some(label.into()),
        }
    }
}

/// Result of formatting a diagram.
#[derive(Debug, Clone)]
pub struct Diagram {
    /// Format used to render the content.
    pub format: DiagramFormat,
    /// Raw text content (Mermaid/DOT/D2).
    pub content: String,
    /// Number of nodes included in the diagram.
    pub node_count: usize,
    /// Number of edges included in the diagram.
    pub edge_count: usize,
    /// True if nodes/edges were truncated to satisfy [`DiagramOptions::max_nodes`].
    pub is_truncated: bool,
}

impl Diagram {
    /// Convenience accessor for whether the diagram contains any nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.node_count == 0
    }
}

/// Trait implemented by all diagram formatters.
pub trait DiagramFormatter {
    /// Format call graph relations into the formatter's syntax.
    ///
    /// # Arguments
    ///
    /// * `snapshot` - Graph snapshot for node lookups
    /// * `nodes` - Node IDs to include in the diagram
    /// * `edges` - Edges between nodes
    /// * `extra_nodes` - Additional nodes not in the graph (e.g., placeholders)
    /// * `options` - Diagram generation options
    ///
    /// # Errors
    ///
    /// Returns an error when graph construction fails.
    fn format_call_graph(
        &self,
        snapshot: &GraphSnapshot,
        nodes: &[NodeId],
        edges: &[DiagramEdge],
        extra_nodes: &[Node],
        options: &DiagramOptions,
    ) -> Result<Diagram>;

    /// Format dependency graphs (imports/exports).
    ///
    /// # Arguments
    ///
    /// * `snapshot` - Graph snapshot for node lookups
    /// * `nodes` - Node IDs to include in the diagram
    /// * `edges` - Edges between nodes
    /// * `extra_nodes` - Additional nodes not in the graph (e.g., placeholders)
    /// * `options` - Diagram generation options
    ///
    /// # Errors
    ///
    /// Returns an error when graph construction fails.
    fn format_dependency_graph(
        &self,
        snapshot: &GraphSnapshot,
        nodes: &[NodeId],
        edges: &[DiagramEdge],
        extra_nodes: &[Node],
        options: &DiagramOptions,
    ) -> Result<Diagram>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagram_format_from_str() {
        assert_eq!(
            DiagramFormat::parse_format("mermaid").unwrap(),
            DiagramFormat::Mermaid
        );
        assert_eq!(
            DiagramFormat::parse_format("MMD").unwrap(),
            DiagramFormat::Mermaid
        );
        assert_eq!(
            DiagramFormat::parse_format("graphviz").unwrap(),
            DiagramFormat::GraphViz
        );
        assert_eq!(
            DiagramFormat::parse_format("dot").unwrap(),
            DiagramFormat::GraphViz
        );
        assert_eq!(
            DiagramFormat::parse_format("d2").unwrap(),
            DiagramFormat::D2
        );
        assert!(DiagramFormat::parse_format("invalid").is_err());
    }

    #[test]
    fn diagram_format_file_extensions() {
        assert_eq!(DiagramFormat::Mermaid.file_extension(), "mmd");
        assert_eq!(DiagramFormat::GraphViz.file_extension(), "dot");
        assert_eq!(DiagramFormat::D2.file_extension(), "d2");
    }

    #[test]
    fn diagram_options_defaults() {
        let opts = DiagramOptions::default();
        assert_eq!(opts.format, DiagramFormat::Mermaid);
        assert_eq!(opts.graph_type, GraphType::CallGraph);
        assert_eq!(opts.max_depth, Some(3));
        assert_eq!(opts.max_nodes, 100);
        assert!(opts.include_file_paths);
        assert_eq!(opts.direction, Direction::TopDown);
    }
}
