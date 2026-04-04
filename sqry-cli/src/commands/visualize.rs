//! Implements the `sqry visualize` command for generating diagrams.

use crate::args::{Cli, DiagramFormatArg, DirectionArg, VisualizeCommand};
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use anyhow::{Context, Result, anyhow, bail};
use sqry_core::graph::unified::GraphSnapshot;
use sqry_core::graph::unified::edge::{EdgeKind, ExportKind, StoreEdgeRef};
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::output::diagram::{
    D2Formatter, Diagram, DiagramEdge, DiagramFormat, DiagramFormatter, DiagramOptions, Direction,
    GraphType, GraphVizFormatter, MermaidFormatter, Node,
};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// Run the visualize command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded, rendered, or written.
pub fn run_visualize(cli: &Cli, cmd: &VisualizeCommand) -> Result<()> {
    validate_command(cli, cmd)?;

    let relation = RelationQuery::parse(&cmd.query)?;
    let search_path = cmd.path.as_deref().unwrap_or(cli.search_path());
    let root_path = Path::new(search_path);

    // Load unified graph snapshot
    // We use default config (no hidden, no follow symlinks) matching CLI defaults unless specified
    // For visualization, we generally want what's in the snapshot.
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(root_path, &config, cli)
        .context("Failed to load unified graph. Run `sqry index` first.")?;

    let snapshot = graph.snapshot();

    if snapshot.nodes().is_empty() {
        bail!(
            "Graph is empty. Run `sqry index {}` to populate it.",
            root_path.display()
        );
    }

    let max_depth = cmd.depth.max(1);
    let capped_nodes = cmd.max_nodes.clamp(1, 500);

    let graph_data = collect_graph_data_unified(&relation, &snapshot, max_depth, capped_nodes);

    let has_placeholder_root = !graph_data.extra_nodes.is_empty();

    if has_placeholder_root {
        eprintln!(
            "No nodes matched '{}'. Rendering placeholder context only.",
            relation.target
        );
    }

    if graph_data.edges.is_empty() {
        eprintln!(
            "No relations found for query '{}'. Rendering node context only.",
            cmd.query
        );
    }

    let options = DiagramOptions {
        format: cmd.format.into(),
        graph_type: relation.kind.graph_type(),
        max_depth: Some(max_depth),
        max_nodes: capped_nodes,
        direction: cmd.direction.into(),
        ..Default::default()
    };

    let node_count = graph_data.nodes.len() + graph_data.extra_nodes.len();
    if node_count >= capped_nodes {
        eprintln!(
            "⚠️  Graph contains {node_count} nodes but visualization is limited to {capped_nodes}. \
Use --max-nodes (up to 500) or refine your relation query to include more detail."
        );
    }

    let formatter = create_formatter(cmd.format);
    let diagram = match relation.kind {
        RelationKind::Imports | RelationKind::Exports => formatter.format_dependency_graph(
            &snapshot,
            &graph_data.nodes,
            &graph_data.edges,
            &graph_data.extra_nodes,
            &options,
        )?,
        _ => formatter.format_call_graph(
            &snapshot,
            &graph_data.nodes,
            &graph_data.edges,
            &graph_data.extra_nodes,
            &options,
        )?,
    };

    if diagram.is_truncated {
        eprintln!(
            "⚠️  Graph truncated to {capped_nodes} nodes (adjust --max-nodes to include more, max 500)."
        );
    }

    write_text_output(&diagram, cmd.output_file.as_ref())?;
    Ok(())
}

fn validate_command(cli: &Cli, cmd: &VisualizeCommand) -> Result<()> {
    if cli.json {
        bail!("--json output is not supported for the visualize command.");
    }
    if cmd.max_nodes == 0 {
        bail!("--max-nodes must be at least 1.");
    }
    if cmd.depth == 0 {
        bail!("--depth must be at least 1.");
    }
    Ok(())
}

/// Context for graph traversal.
struct TraversalContext<'a> {
    snapshot: &'a GraphSnapshot,
    relation_kind: RelationKind,
    max_depth: usize,
    max_edges: usize,
}

/// Initialize root nodes for traversal, returning placeholder if none found.
fn init_root_nodes(
    relation: &RelationQuery,
    snapshot: &GraphSnapshot,
    nodes: &mut NodeSet,
    queue: &mut VecDeque<(NodeId, usize)>,
    visited: &mut HashSet<NodeId>,
) -> Option<GraphData> {
    let root_nodes = resolve_nodes(snapshot, &relation.target, relation.kind);

    if root_nodes.is_empty() {
        let placeholder = placeholder_node(&relation.target);
        return Some(GraphData {
            nodes: Vec::new(),
            edges: Vec::new(),
            extra_nodes: vec![placeholder],
        });
    }

    for start_node in root_nodes {
        nodes.add_node(snapshot, start_node);
        if visited.insert(start_node) {
            queue.push_back((start_node, 0usize));
        }
    }

    None
}

/// Create a placeholder node for no-match queries.
fn placeholder_node(name: &str) -> Node {
    Node {
        id: name.to_string(),
        label: name.to_string(),
        file_path: None,
        line: None,
    }
}

/// Convert a graph edge to a `DiagramEdge`, adding nodes to the set.
fn edge_to_diagram_edge(
    edge: &StoreEdgeRef,
    snapshot: &GraphSnapshot,
    nodes: &mut NodeSet,
) -> DiagramEdge {
    nodes.add_node(snapshot, edge.source);
    nodes.add_node(snapshot, edge.target);

    let label = edge_label_for_kind(snapshot, &edge.kind);

    DiagramEdge {
        source: edge.source,
        target: edge.target,
        label,
    }
}

/// Determine the next node to visit based on relation kind.
fn next_node_for_relation(edge: &StoreEdgeRef, relation_kind: RelationKind) -> NodeId {
    match relation_kind {
        RelationKind::Callers | RelationKind::Imports | RelationKind::Exports => edge.source,
        RelationKind::Callees => edge.target,
    }
}

fn collect_graph_data_unified(
    relation: &RelationQuery,
    snapshot: &GraphSnapshot,
    max_depth: usize,
    max_nodes: usize,
) -> GraphData {
    let mut edges = Vec::new();
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut nodes = NodeSet::default();

    // Initialize roots - return early if placeholder
    if let Some(placeholder_data) =
        init_root_nodes(relation, snapshot, &mut nodes, &mut queue, &mut visited)
    {
        return placeholder_data;
    }

    let ctx = TraversalContext {
        snapshot,
        relation_kind: relation.kind,
        max_depth,
        max_edges: max_nodes.saturating_mul(max_depth.max(1)).max(32),
    };

    while let Some((current_node, depth)) = queue.pop_front() {
        if depth >= ctx.max_depth || edges.len() >= ctx.max_edges {
            continue;
        }

        let mut outgoing_edges =
            collect_relation_edges(ctx.snapshot, ctx.relation_kind, current_node);
        outgoing_edges.sort_by_key(|edge| {
            let source_key = node_sort_key(ctx.snapshot, edge.source);
            let target_key = node_sort_key(ctx.snapshot, edge.target);
            (source_key, target_key, edge.kind.tag())
        });

        for edge in outgoing_edges {
            if edges.len() >= ctx.max_edges {
                break;
            }

            edges.push(edge_to_diagram_edge(&edge, ctx.snapshot, &mut nodes));

            let next_node = next_node_for_relation(&edge, ctx.relation_kind);
            if visited.insert(next_node) && depth + 1 < ctx.max_depth {
                queue.push_back((next_node, depth + 1));
            }
        }
    }

    GraphData {
        nodes: nodes.into_vec(),
        edges,
        extra_nodes: Vec::new(),
    }
}

fn collect_relation_edges(
    snapshot: &GraphSnapshot,
    relation_kind: RelationKind,
    current_node: NodeId,
) -> Vec<StoreEdgeRef> {
    match relation_kind {
        RelationKind::Callers => snapshot
            .edges()
            .edges_to(current_node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .collect(),
        RelationKind::Callees => snapshot
            .edges()
            .edges_from(current_node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .collect(),
        RelationKind::Imports => snapshot
            .edges()
            .edges_to(current_node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Imports { .. }))
            .collect(),
        RelationKind::Exports => snapshot
            .edges()
            .edges_to(current_node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Exports { .. }))
            .collect(),
    }
}

fn resolve_nodes(snapshot: &GraphSnapshot, name: &str, relation_kind: RelationKind) -> Vec<NodeId> {
    let required_kind = required_node_kind(relation_kind);
    let matches = collect_node_matches(snapshot, name, required_kind);
    let mut candidates = select_node_candidates(relation_kind, &matches);

    if candidates.is_empty() {
        return Vec::new();
    }

    candidates.sort_by_key(|node_id| node_sort_key(snapshot, *node_id));
    if relation_kind == RelationKind::Imports {
        candidates
    } else {
        candidates.truncate(1);
        candidates
    }
}

struct NodeMatches {
    qualified: Vec<NodeId>,
    name: Vec<NodeId>,
    pattern: Vec<NodeId>,
}

fn required_node_kind(relation_kind: RelationKind) -> Option<NodeKind> {
    match relation_kind {
        RelationKind::Imports => Some(NodeKind::Import),
        _ => None,
    }
}

fn collect_node_matches(
    snapshot: &GraphSnapshot,
    name: &str,
    required_kind: Option<NodeKind>,
) -> NodeMatches {
    let mut qualified = Vec::new();
    let mut name_matches = Vec::new();
    let mut pattern = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        if required_kind.is_some_and(|kind| entry.kind != kind) {
            continue;
        }
        let name_str = snapshot.strings().resolve(entry.name);
        let qualified_str = entry
            .qualified_name
            .and_then(|id| snapshot.strings().resolve(id));
        let name_ref = name_str.as_ref().map(AsRef::as_ref);
        let qualified_ref = qualified_str.as_ref().map(AsRef::as_ref);

        if matches!(qualified_ref, Some(candidate) if candidate == name) {
            qualified.push(node_id);
            continue;
        }

        if matches!(name_ref, Some(candidate) if candidate == name) {
            name_matches.push(node_id);
            continue;
        }

        if matches!(qualified_ref, Some(candidate) if candidate.contains(name))
            || matches!(name_ref, Some(candidate) if candidate.contains(name))
        {
            pattern.push(node_id);
        }
    }

    NodeMatches {
        qualified,
        name: name_matches,
        pattern,
    }
}

fn select_node_candidates(relation_kind: RelationKind, matches: &NodeMatches) -> Vec<NodeId> {
    if relation_kind == RelationKind::Imports {
        return merge_node_candidates(&matches.qualified, &matches.name, &matches.pattern);
    }

    if !matches.qualified.is_empty() {
        return matches.qualified.clone();
    }
    if !matches.name.is_empty() {
        return matches.name.clone();
    }

    matches.pattern.clone()
}

fn merge_node_candidates(
    qualified: &[NodeId],
    name_matches: &[NodeId],
    pattern: &[NodeId],
) -> Vec<NodeId> {
    let mut merged = Vec::new();
    let mut seen = HashSet::new();

    for node_id in qualified.iter().chain(name_matches).chain(pattern) {
        if seen.insert(*node_id) {
            merged.push(*node_id);
        }
    }

    merged
}

fn node_sort_key(snapshot: &GraphSnapshot, id: NodeId) -> (String, String, u32, u64) {
    if let Some(entry) = snapshot.get_node(id) {
        let name = node_display_name(snapshot, entry);
        let file = snapshot
            .files()
            .resolve(entry.file)
            .map(|p| p.as_ref().to_string_lossy().to_string())
            .unwrap_or_default();
        (file, name, id.index(), id.generation())
    } else {
        (String::new(), String::new(), id.index(), id.generation())
    }
}

fn node_display_name(
    snapshot: &GraphSnapshot,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
) -> String {
    entry
        .qualified_name
        .and_then(|sid| snapshot.strings().resolve(sid))
        .or_else(|| snapshot.strings().resolve(entry.name))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Extract a diagram label from edge kind (for import/export/call edges).
fn edge_label_for_kind(snapshot: &GraphSnapshot, kind: &EdgeKind) -> Option<String> {
    match kind {
        EdgeKind::Calls { is_async, .. } => {
            if *is_async {
                Some("async".to_string())
            } else {
                None
            }
        }
        EdgeKind::Imports { alias, is_wildcard } => {
            let alias_name = alias
                .and_then(|id| snapshot.strings().resolve(id))
                .map(|value| value.to_string());
            import_edge_label(alias_name.as_deref(), *is_wildcard)
        }
        EdgeKind::Exports { kind, alias } => {
            let alias_name = alias
                .and_then(|id| snapshot.strings().resolve(id))
                .map(|value| value.to_string());
            export_edge_label(*kind, alias_name.as_deref())
        }
        _ => None,
    }
}

fn import_edge_label(alias: Option<&str>, is_wildcard: bool) -> Option<String> {
    match (alias, is_wildcard) {
        (None, false) => None,
        (Some(alias), false) => Some(format!("as {alias}")),
        (None, true) => Some("*".to_string()),
        (Some(alias), true) => Some(format!("* as {alias}")),
    }
}

fn export_edge_label(kind: ExportKind, alias: Option<&str>) -> Option<String> {
    let kind_label = match kind {
        ExportKind::Direct => None,
        ExportKind::Reexport => Some("reexport"),
        ExportKind::Default => Some("default"),
        ExportKind::Namespace => Some("namespace"),
    };

    match (kind_label, alias) {
        (None, None) => None,
        (Some(kind), None) => Some(kind.to_string()),
        (None, Some(alias)) => Some(format!("as {alias}")),
        (Some(kind), Some(alias)) => Some(format!("{kind} as {alias}")),
    }
}

fn create_formatter(format: DiagramFormatArg) -> Box<dyn DiagramFormatter> {
    match format {
        DiagramFormatArg::Mermaid => Box::new(MermaidFormatter::new()),
        DiagramFormatArg::Graphviz => Box::new(GraphVizFormatter::new()),
        DiagramFormatArg::D2 => Box::new(D2Formatter::new()),
    }
}

fn write_text_output(diagram: &Diagram, path: Option<&PathBuf>) -> Result<()> {
    if let Some(path) = path {
        fs::write(path, &diagram.content)
            .with_context(|| format!("Failed to write diagram to {}", path.display()))?;
        println!("Diagram saved to {}", path.display());
    } else {
        println!("{}", diagram.content);
    }
    Ok(())
}

fn render_default_direction(dir: DirectionArg) -> Direction {
    match dir {
        DirectionArg::TopDown => Direction::TopDown,
        DirectionArg::BottomUp => Direction::BottomUp,
        DirectionArg::LeftRight => Direction::LeftRight,
        DirectionArg::RightLeft => Direction::RightLeft,
    }
}

#[derive(Debug)]
struct GraphData {
    nodes: Vec<NodeId>,
    edges: Vec<DiagramEdge>,
    /// Placeholder nodes for no-match queries (not in graph).
    extra_nodes: Vec<Node>,
}

/// Tracks visited nodes by key (`qualified_name` or name), maintaining insertion order.
#[derive(Default)]
struct NodeSet {
    seen: HashSet<String>,
    ordered: Vec<NodeId>,
}

impl NodeSet {
    fn add_node(&mut self, snapshot: &GraphSnapshot, node_id: NodeId) {
        let key = node_key(snapshot, node_id);
        if self.seen.insert(key) {
            self.ordered.push(node_id);
        }
    }

    fn into_vec(self) -> Vec<NodeId> {
        self.ordered
    }
}

/// Get a unique key for a node (`qualified_name` if present, else name).
fn node_key(snapshot: &GraphSnapshot, node_id: NodeId) -> String {
    if let Some(entry) = snapshot.get_node(node_id) {
        entry
            .qualified_name
            .and_then(|sid| snapshot.strings().resolve(sid))
            .or_else(|| snapshot.strings().resolve(entry.name))
            .map(|s| s.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationKind {
    Callers,
    Callees,
    Imports,
    Exports,
}

impl RelationKind {
    fn from_str(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "callers" => Some(Self::Callers),
            "callees" => Some(Self::Callees),
            "imports" => Some(Self::Imports),
            "exports" => Some(Self::Exports),
            _ => None,
        }
    }

    fn graph_type(self) -> GraphType {
        match self {
            RelationKind::Callers | RelationKind::Callees => GraphType::CallGraph,
            RelationKind::Imports | RelationKind::Exports => GraphType::DependencyGraph,
        }
    }
}

#[derive(Debug)]
struct RelationQuery {
    kind: RelationKind,
    target: String,
}

impl RelationQuery {
    fn parse(input: &str) -> Result<Self> {
        let (prefix, target) = input.split_once(':').ok_or_else(|| {
            anyhow!("Relation query must use the form kind:symbol. Example: callers:main")
        })?;

        let kind = RelationKind::from_str(prefix.trim()).ok_or_else(|| {
            anyhow!("Unsupported relation '{prefix}'. Use callers, callees, imports, or exports.")
        })?;

        let target = target.trim();
        if target.is_empty() {
            bail!("Relation target cannot be empty.");
        }

        Ok(Self {
            kind,
            target: target.to_string(),
        })
    }
}

impl From<DiagramFormatArg> for DiagramFormat {
    fn from(value: DiagramFormatArg) -> Self {
        match value {
            DiagramFormatArg::Mermaid => DiagramFormat::Mermaid,
            DiagramFormatArg::Graphviz => DiagramFormat::GraphViz,
            DiagramFormatArg::D2 => DiagramFormat::D2,
        }
    }
}

impl From<DirectionArg> for Direction {
    fn from(value: DirectionArg) -> Self {
        render_default_direction(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relation_query() {
        let query = RelationQuery::parse("callers:main").unwrap();
        assert_eq!(query.kind, RelationKind::Callers);
        assert_eq!(query.target, "main");
    }

    #[test]
    fn rejects_unknown_relation() {
        assert!(RelationQuery::parse("unknown:foo").is_err());
    }
}
