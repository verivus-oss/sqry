//! Implements the `sqry visualize` command for generating diagrams.

use crate::args::{Cli, DiagramFormatArg, DirectionArg, VisualizeCommand};
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use anyhow::{Context, Result, anyhow, bail};
use sqry_core::graph::unified::GraphSnapshot;
use sqry_core::graph::unified::edge::{EdgeKind, ExportKind};
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::graph::unified::resolution::{FileScope, ResolvedFileScope};
use sqry_core::graph::unified::{
    EdgeFilter, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};
use sqry_core::output::diagram::{
    D2Formatter, Diagram, DiagramEdge, DiagramFormat, DiagramFormatter, DiagramOptions, Direction,
    GraphType, GraphVizFormatter, MermaidFormatter, Node,
};
use sqry_core::query::executor::graph_eval::{
    entry_query_texts, extract_method_name, language_aware_segments_match,
};
use std::collections::HashSet;
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
    let graph = load_unified_graph_for_cli(root_path, &config, cli, no_op_reporter())
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

    let graph_data = collect_graph_data_unified(
        &relation,
        &snapshot,
        max_depth,
        capped_nodes,
        cmd.in_file.as_deref(),
        cmd.line,
    );

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

/// Create a placeholder node for no-match queries.
fn placeholder_node(name: &str) -> Node {
    Node {
        id: name.to_string(),
        label: name.to_string(),
        file_path: None,
        line: None,
    }
}

/// Collect graph data for visualization using the traversal kernel.
///
/// Uses the kernel for BFS traversal, then converts the result into
/// diagram-specific `GraphData` with sorted edges and labeled edges.
///
/// # Frontier invariant (DB19)
///
/// Root nodes are resolved once at the handler boundary via
/// [`resolve_nodes`] and passed as a `&[NodeId]` into [`traverse`]. The
/// kernel walks the NodeId-keyed graph and never re-resolves names at
/// depth ≥ 1. This is the same invariant DB17/DB18 locked for
/// `trace_path`, `subgraph`, `call-chain-depth`, and `dependency-tree`
/// — a same-simple-name node at depth 1 never broadens the frontier
/// because the kernel operates on generational-indexed `NodeId`s, not
/// names.
///
/// Regressions that reintroduced DB15-class same-name broadening would
/// surface in the CLI `migration_golden_cli_test` golden tests.
fn collect_graph_data_unified(
    relation: &RelationQuery,
    snapshot: &GraphSnapshot,
    max_depth: usize,
    max_nodes: usize,
    in_file: Option<&str>,
    line: Option<u32>,
) -> GraphData {
    // Resolve root nodes
    let root_nodes = resolve_nodes(snapshot, &relation.target, relation.kind, in_file, line);

    if root_nodes.is_empty() {
        let placeholder = placeholder_node(&relation.target);
        return GraphData {
            nodes: Vec::new(),
            edges: Vec::new(),
            extra_nodes: vec![placeholder],
        };
    }

    // Map relation kind to traversal direction and edge filter
    let (direction, edge_filter) = match relation.kind {
        RelationKind::Callers => (TraversalDirection::Incoming, EdgeFilter::calls_only()),
        RelationKind::Callees => (TraversalDirection::Outgoing, EdgeFilter::calls_only()),
        RelationKind::Imports => (TraversalDirection::Incoming, edge_filter_imports_only()),
        RelationKind::Exports => (TraversalDirection::Incoming, edge_filter_exports_only()),
    };

    let max_edges = max_nodes.saturating_mul(max_depth.max(1)).max(32);

    let config = TraversalConfig {
        direction,
        edge_filter,
        limits: TraversalLimits {
            max_depth: u32::try_from(max_depth).unwrap_or(u32::MAX),
            max_nodes: Some(max_nodes),
            max_edges: Some(max_edges),
            max_paths: None,
        },
    };

    let result = traverse(snapshot, &root_nodes, &config, None);

    // Convert TraversalResult into GraphData
    let mut nodes = NodeSet::default();
    let mut edges: Vec<DiagramEdge> = Vec::new();

    // Add root nodes first to preserve ordering
    for &root in &root_nodes {
        nodes.add_node(snapshot, root);
    }

    // Build sorted diagram edges from kernel result
    let mut diagram_edges: Vec<(DiagramEdge, (String, String, &'static str))> = result
        .edges
        .iter()
        .map(|mat_edge| {
            let source_id = result.nodes[mat_edge.source_idx].node_id;
            let target_id = result.nodes[mat_edge.target_idx].node_id;

            let label = edge_label_for_kind(snapshot, &mat_edge.raw_kind);

            let source_key = node_sort_key(snapshot, source_id);
            let target_key = node_sort_key(snapshot, target_id);
            let sort_key = (
                source_key.0.clone(),
                target_key.0.clone(),
                mat_edge.raw_kind.tag(),
            );

            (
                DiagramEdge {
                    source: source_id,
                    target: target_id,
                    label,
                },
                sort_key,
            )
        })
        .collect();

    diagram_edges.sort_by(|a, b| a.1.cmp(&b.1));

    for (edge, _sort_key) in diagram_edges {
        nodes.add_node(snapshot, edge.source);
        nodes.add_node(snapshot, edge.target);
        edges.push(edge);
    }

    GraphData {
        nodes: nodes.into_vec(),
        edges,
        extra_nodes: Vec::new(),
    }
}

/// Edge filter that only includes import edges.
fn edge_filter_imports_only() -> EdgeFilter {
    EdgeFilter {
        include_calls: false,
        include_imports: true,
        include_references: false,
        include_inheritance: false,
        include_structural: false,
        include_type_edges: false,
        include_database: false,
        include_service: false,
    }
}

/// Edge filter that only includes export edges.
fn edge_filter_exports_only() -> EdgeFilter {
    // Exports are classified under `include_imports` in EdgeFilter (both import+export).
    // We include imports flag which covers both Import and Export classifications.
    EdgeFilter {
        include_calls: false,
        include_imports: true,
        include_references: false,
        include_inheritance: false,
        include_structural: false,
        include_type_edges: false,
        include_database: false,
        include_service: false,
    }
}

fn resolve_nodes(
    snapshot: &GraphSnapshot,
    name: &str,
    relation_kind: RelationKind,
    in_file: Option<&str>,
    line: Option<u32>,
) -> Vec<NodeId> {
    // Resolve the relation roots to the SAME node set `sqry graph
    // direct-callers` / `direct-callees` and `sqry query callers:`/`callees:`
    // resolve (verivus-oss/sqry#516). For `Calls`-edge relations that means
    // the segment-aware matcher: a query for `Alpha::helper` matches every
    // node whose trailing method segment is `helper` (so `Beta::helper` too),
    // exactly as `graph_eval` / sqry-db reconcile it. Import / export
    // relations keep the bucket matcher, which honours the `Import` node kind
    // and module-name buckets those surfaces use.
    let mut candidates = match relation_kind {
        RelationKind::Callers | RelationKind::Callees => {
            resolve_call_relation_roots(snapshot, name)
        }
        RelationKind::Imports | RelationKind::Exports => {
            let required_kind = required_node_kind(relation_kind);
            let matches = collect_node_matches(snapshot, name, required_kind);
            select_node_candidates(relation_kind, &matches)
        }
    };

    // Narrow by the optional `--in <file>` / `--line <N>` disambiguators.
    candidates = apply_disambiguators(snapshot, candidates, in_file, line);

    if candidates.is_empty() {
        return Vec::new();
    }

    candidates.sort_by_key(|node_id| node_sort_key(snapshot, *node_id));
    candidates.dedup();

    // Traverse from every matching definition (union), matching the
    // resolution used by `sqry graph direct-callees` / `sqry query`. The
    // previous `candidates.truncate(1)` picked a single arbitrary node by
    // sort order, so an ambiguous name whose first-sorted match had no
    // outgoing edges rendered an edge-less node-only diagram even though the
    // relation existed on a sibling definition (verivus-oss/sqry#516). The
    // traversal kernel takes a `&[NodeId]` seed set and never re-resolves the
    // name at depth >= 1, so multiple roots do not broaden the frontier.
    candidates
}

/// Resolve the root nodes for a `callers:` / `callees:` relation with the same
/// segment-aware name matching `sqry graph direct-call*` and `sqry query`
/// use.
///
/// A node is a root when any of its query texts (interned name, qualified
/// name, display-qualified name) segment-matches `name` under
/// [`language_aware_segments_match`], the shared predicate `graph_eval` and
/// sqry-db reconcile on. So a qualified `Type::method` query resolves to the
/// identical node set as `graph direct-callees Type::method` (every node whose
/// trailing method segment matches), not just the exact-qualified node.
/// Unified losers are skipped, matching [`collect_node_matches`].
///
/// The trailing-method-segment fallback mirrors sqry-db's
/// `method_segment_matches` (relation.rs): some plugins (JS / TS via
/// `semantic_name_for_node_input`, which short-circuits on a `/`) intern a
/// slash-containing callable as its full name with no bare-method query text,
/// so `Foo::fetchUsers` would not segment-match `frontend/api.js::fetchUsers`.
/// sqry-db still unions those through the method-name fallback, so `graph
/// direct-callees` finds them; replicate it here for identical resolution.
fn resolve_call_relation_roots(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    let query_method = extract_method_name(name);
    snapshot
        .iter_nodes()
        .filter_map(|(node_id, entry)| {
            if entry.is_unified_loser() {
                return None;
            }
            let texts = entry_query_texts(snapshot, entry);
            let segment_match = texts
                .iter()
                .any(|text| language_aware_segments_match(snapshot, entry.file, text, name));
            let method_match = query_method.as_deref().is_some_and(|method| {
                texts
                    .iter()
                    .filter_map(|text| extract_method_name(text))
                    .any(|candidate| candidate == method)
            });
            (segment_match || method_match).then_some(node_id)
        })
        .collect()
}

/// Narrow a candidate node set by the optional `--in <file>` and
/// `--line <N>` disambiguators.
///
/// `--in` is resolved against the graph's file registry via the shared
/// [`GraphSnapshot::resolve_file_scope`] so its path-normalization matches
/// `sqry impact --in`. A file that is not indexed yields an empty set (the
/// caller then renders the no-match placeholder). `--line` keeps only nodes
/// whose definition starts on that one-based line.
fn apply_disambiguators(
    snapshot: &GraphSnapshot,
    candidates: Vec<NodeId>,
    in_file: Option<&str>,
    line: Option<u32>,
) -> Vec<NodeId> {
    let mut result = candidates;

    if let Some(file) = in_file {
        match snapshot.resolve_file_scope(&FileScope::Path(Path::new(file))) {
            Ok(ResolvedFileScope::File(file_id)) => {
                result.retain(|node_id| {
                    snapshot
                        .get_node(*node_id)
                        .is_some_and(|entry| entry.file == file_id)
                });
            }
            // `ResolvedFileScope::Any` cannot arise from `FileScope::Path`;
            // an unindexed file resolves the whole set to empty.
            _ => result.clear(),
        }
    }

    if let Some(target_line) = line {
        result.retain(|node_id| {
            snapshot
                .get_node(*node_id)
                .is_some_and(|entry| entry.start_line == target_line)
        });
    }

    result
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
        // Gate 0d iter-2 fix: skip unified losers from CLI
        // `visualize` matching. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
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

    #[test]
    fn resolve_call_roots_matches_path_qualified_name_via_method_segment() {
        use sqry_core::graph::node::Language;
        use sqry_core::graph::unified::concurrent::CodeGraph;
        use sqry_core::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        // A callable whose interned name carries a '/', the shape
        // `semantic_name_for_node_input` produces for slash-qualified inputs
        // (JS / TS): the full name is interned with no bare-method query text.
        // `language_aware_segments_match` alone cannot line up a suffix
        // boundary against `Api::fetchUsers`, so parity with `graph
        // direct-callees` depends on the trailing-method-segment fallback this
        // test locks (verivus-oss/sqry#516).
        let slash_name = "frontend/api.js::fetchUsers";

        let mut graph = CodeGraph::new();
        let name_id = graph.strings_mut().intern(slash_name).unwrap();
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("frontend/api.js"), Some(Language::JavaScript))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
            .with_qualified_name(name_id)
            .with_location(1, 0, 1, 1);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, name_id, Some(name_id), file_id);

        let snapshot = graph.snapshot();

        // The segment matcher alone misses, so the resolution below exercises
        // the method-segment fallback rather than the plain segment path.
        assert!(
            !language_aware_segments_match(&snapshot, file_id, slash_name, "Api::fetchUsers"),
            "segment matcher must miss the slash-qualified name (fallback needed)"
        );

        let roots = resolve_call_relation_roots(&snapshot, "Api::fetchUsers");
        assert!(
            roots.contains(&node_id),
            "slash-qualified callable must resolve via the trailing-method-segment \
             fallback, matching graph direct-callees"
        );
    }
}
