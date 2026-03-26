use crate::Result;
use crate::graph::unified::GraphSnapshot;
use crate::graph::unified::node::NodeId;
use crate::output::diagram::{
    Diagram, DiagramEdge, DiagramFormat, DiagramFormatter, DiagramOptions, Direction, GraphBuilder,
    Node,
};
use std::fmt::Write;

/// Formatter that emits D2 diagrams.
#[derive(Debug, Clone, Default)]
pub struct D2Formatter;

impl D2Formatter {
    /// Create a new formatter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn render_graph(graph: &GraphBuilder, options: &DiagramOptions) -> Result<String> {
        let mut output =
            String::with_capacity(graph.node_count() * 72 + graph.edge_count() * 28 + 64);
        writeln!(output, "direction: {}", d2_direction(options.direction))?;
        writeln!(
            output,
            "# sqry diagram | nodes: {} edges: {}",
            graph.node_count(),
            graph.edge_count()
        )?;
        writeln!(output)?;

        for node in graph.nodes() {
            Self::write_node(&mut output, node, options)?;
        }
        writeln!(output)?;
        for edge in graph.edges() {
            Self::write_edge(&mut output, edge)?;
        }

        if graph.is_truncated() {
            writeln!(
                output,
                "# truncated to {} nodes (raise --max-nodes for more detail)",
                graph.node_count()
            )?;
        }

        Ok(output)
    }

    fn write_node(
        output: &mut String,
        node: &crate::output::diagram::Node,
        options: &DiagramOptions,
    ) -> Result<()> {
        writeln!(output, "{}: {{", node.id)?;
        writeln!(output, "  label: \"{}\"", escape_label_d2(&node.label))?;
        if options.include_file_paths
            && let Some(path) = &node.file_path
        {
            let tooltip = if let Some(line) = node.line {
                format!("{}:{}", path.display(), line)
            } else {
                path.display().to_string()
            };
            writeln!(output, "  tooltip: \"{}\"", escape_label_d2(&tooltip))?;
        }
        writeln!(output, "}}")?;
        Ok(())
    }

    fn write_edge(output: &mut String, edge: &crate::output::diagram::Edge) -> Result<()> {
        if let Some(label) = &edge.label {
            writeln!(
                output,
                "{} -> {}: \"{}\"",
                edge.from,
                edge.to,
                escape_label_d2(label)
            )?;
        } else {
            writeln!(output, "{} -> {}", edge.from, edge.to)?;
        }
        Ok(())
    }
}

impl DiagramFormatter for D2Formatter {
    fn format_call_graph(
        &self,
        snapshot: &GraphSnapshot,
        nodes: &[NodeId],
        edges: &[DiagramEdge],
        extra_nodes: &[Node],
        options: &DiagramOptions,
    ) -> Result<Diagram> {
        let graph = GraphBuilder::build_from_graph(snapshot, nodes, edges, extra_nodes, options)?;
        let content = Self::render_graph(&graph, options)?;

        Ok(Diagram {
            format: DiagramFormat::D2,
            content,
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
            is_truncated: graph.is_truncated(),
        })
    }

    fn format_dependency_graph(
        &self,
        snapshot: &GraphSnapshot,
        nodes: &[NodeId],
        edges: &[DiagramEdge],
        extra_nodes: &[Node],
        options: &DiagramOptions,
    ) -> Result<Diagram> {
        self.format_call_graph(snapshot, nodes, edges, extra_nodes, options)
    }
}

fn d2_direction(direction: Direction) -> &'static str {
    match direction {
        Direction::TopDown => "down",
        Direction::BottomUp => "up",
        Direction::LeftRight => "right",
        Direction::RightLeft => "left",
    }
}

fn escape_label_d2(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\\n")
}

// Integration tests for D2Formatter are in sqry-cli/tests/visualize_integration.rs
// since they require a real CodeGraph fixture.
