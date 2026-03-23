use crate::Result;
use crate::graph::unified::GraphSnapshot;
use crate::graph::unified::node::NodeId;
use crate::output::diagram::{
    Diagram, DiagramEdge, DiagramFormat, DiagramFormatter, DiagramOptions, GraphBuilder, GraphType,
    Node, escape_label_graphviz,
};
use std::fmt::Write;

/// Formatter that emits `GraphViz` DOT diagrams.
#[derive(Debug, Clone, Default)]
pub struct GraphVizFormatter;

impl GraphVizFormatter {
    /// Create a new formatter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn render_graph(graph: &GraphBuilder, options: &DiagramOptions) -> Result<String> {
        let mut output =
            String::with_capacity(graph.node_count() * 80 + graph.edge_count() * 32 + 128);
        writeln!(output, "digraph {} {{", graph_name(options.graph_type))?;
        writeln!(output, "  rankdir={};", direction_to_rankdir(options))?;
        writeln!(
            output,
            "  node [shape=box, style=\"rounded,filled\", fillcolor=\"#f8fafc\", fontname=\"JetBrains Mono\", fontsize=10];"
        )?;
        writeln!(
            output,
            "  edge [color=\"#94a3b8\", fontname=\"JetBrains Mono\", fontsize=9];"
        )?;
        writeln!(
            output,
            "  // sqry diagram | nodes: {} edges: {}",
            graph.node_count(),
            graph.edge_count()
        )?;

        for node in graph.nodes() {
            Self::write_node(&mut output, node, options)?;
        }
        for edge in graph.edges() {
            Self::write_edge(&mut output, edge)?;
        }

        if graph.is_truncated() {
            writeln!(
                output,
                "  // truncated to {} nodes (use --max-nodes to increase)",
                graph.node_count()
            )?;
        }
        writeln!(output, "}}")?;
        Ok(output)
    }

    fn write_node(
        output: &mut String,
        node: &crate::output::diagram::Node,
        options: &DiagramOptions,
    ) -> Result<()> {
        let mut label = escape_label_graphviz(&node.label);
        if options.include_file_paths
            && let Some(path) = &node.file_path
        {
            label.push_str("\\n");
            let location = if let Some(line) = node.line {
                format!("{}:{}", path.display(), line)
            } else {
                path.display().to_string()
            };
            label.push_str(&escape_label_graphviz(&location));
        }

        writeln!(output, "  {} [label=\"{}\"];", node.id, label)?;
        Ok(())
    }

    fn write_edge(output: &mut String, edge: &crate::output::diagram::Edge) -> Result<()> {
        if let Some(label) = &edge.label {
            writeln!(
                output,
                "  {} -> {} [label=\"{}\"]",
                edge.from,
                edge.to,
                escape_label_graphviz(label)
            )?;
        } else {
            writeln!(output, "  {} -> {}", edge.from, edge.to)?;
        }
        Ok(())
    }
}

impl DiagramFormatter for GraphVizFormatter {
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
            format: DiagramFormat::GraphViz,
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

fn direction_to_rankdir(options: &DiagramOptions) -> &'static str {
    match options.direction {
        crate::output::diagram::Direction::TopDown => "TB",
        crate::output::diagram::Direction::BottomUp => "BT",
        crate::output::diagram::Direction::LeftRight => "LR",
        crate::output::diagram::Direction::RightLeft => "RL",
    }
}

fn graph_name(graph_type: GraphType) -> &'static str {
    match graph_type {
        GraphType::CallGraph => "CallGraph",
        GraphType::DependencyGraph => "DependencyGraph",
        GraphType::TypeHierarchy => "TypeHierarchy",
    }
}

// Integration tests for GraphVizFormatter are in sqry-cli/tests/visualize_integration.rs
// since they require a real CodeGraph fixture.
