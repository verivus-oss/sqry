use crate::Result;
use crate::graph::unified::GraphSnapshot;
use crate::graph::unified::node::NodeId;
use crate::output::diagram::{
    Diagram, DiagramEdge, DiagramFormat, DiagramFormatter, DiagramOptions, GraphBuilder, Node,
    escape_label_mermaid,
};
use std::fmt::Write;

/// Formatter that emits Mermaid graph syntax.
#[derive(Debug, Clone, Default)]
pub struct MermaidFormatter;

impl MermaidFormatter {
    /// Create a new formatter instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn render_graph(graph: &GraphBuilder, options: &DiagramOptions) -> Result<String> {
        let mut output = String::with_capacity(graph.node_count() * 64 + graph.edge_count() * 24);
        Self::write_metadata_comment(&mut output, graph)?;
        Self::write_header(&mut output, options)?;
        for node in graph.nodes() {
            Self::write_node(&mut output, node, options)?;
        }
        for edge in graph.edges() {
            Self::write_edge(&mut output, edge)?;
        }
        if graph.is_truncated() {
            writeln!(
                output,
                "%% truncated to {} nodes (increase --max-nodes to see more)",
                graph.node_count()
            )?;
        }
        Ok(output)
    }

    fn write_metadata_comment(output: &mut String, graph: &GraphBuilder) -> Result<()> {
        writeln!(
            output,
            "%% sqry diagram | nodes: {} edges: {}",
            graph.node_count(),
            graph.edge_count()
        )?;
        Ok(())
    }

    fn write_header(output: &mut String, options: &DiagramOptions) -> Result<()> {
        let direction = match options.direction {
            crate::output::diagram::Direction::TopDown => "TB",
            crate::output::diagram::Direction::BottomUp => "BT",
            crate::output::diagram::Direction::LeftRight => "LR",
            crate::output::diagram::Direction::RightLeft => "RL",
        };
        writeln!(output, "graph {direction}")?;
        Ok(())
    }

    fn write_node(
        output: &mut String,
        node: &crate::output::diagram::Node,
        options: &DiagramOptions,
    ) -> Result<()> {
        let mut label = escape_label_mermaid(&node.label);
        if options.include_file_paths
            && let Some(path) = &node.file_path
        {
            label.push_str("\\n");
            let location = if let Some(line) = node.line {
                format!("{}:{}", path.display(), line)
            } else {
                path.display().to_string()
            };
            label.push_str(&escape_label_mermaid(&location));
        }
        writeln!(output, "  {}[\"{}\"]", node.id, label)?;
        Ok(())
    }

    fn write_edge(output: &mut String, edge: &crate::output::diagram::Edge) -> Result<()> {
        if let Some(label) = &edge.label {
            writeln!(
                output,
                "  {} -- {} --> {}",
                edge.from,
                escape_label_mermaid(label),
                edge.to
            )?;
        } else {
            writeln!(output, "  {} --> {}", edge.from, edge.to)?;
        }
        Ok(())
    }
}

impl DiagramFormatter for MermaidFormatter {
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
            format: DiagramFormat::Mermaid,
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

// Integration tests for MermaidFormatter are in sqry-cli/tests/visualize_integration.rs
// since they require a real CodeGraph fixture.
