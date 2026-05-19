//! Generic BFS kernel with `TraversalConfig` and strategy callbacks.
//!
//! Provides a single `traverse()` function that implements standard BFS (global
//! visited set) and path-enumeration BFS (path-local cycle detection). All 14
//! BFS implementations in sqry migrate to this kernel.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;

use super::concurrent::GraphSnapshot;
use super::edge::kind::EdgeKind;
#[cfg(test)]
use super::edge::kind::ResolvedVia;
use super::edge::store::StoreEdgeRef;
use super::materialize::materialize_node;
use super::node::id::NodeId;
use super::traversal::{
    EdgeClassification, MaterializedEdge, TraversalMetadata, TraversalResult, TruncationReason,
};

// ──────────────────── Configuration types ────────────────────

/// Which direction to traverse edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDirection {
    /// Traverse outgoing edges (callees, dependencies).
    Outgoing,
    /// Traverse incoming edges (callers, reverse impact).
    Incoming,
    /// Traverse both directions (subgraph extraction).
    Both,
}

/// Filter controlling which edge types to include during traversal.
///
/// Each boolean corresponds to an `EdgeClassification` variant group.
/// Edges whose classification does not match any enabled flag are skipped.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // One bool per edge classification variant is the natural API
pub struct EdgeFilter {
    /// Include `Call` edges.
    pub include_calls: bool,
    /// Include `Import` and `Export` edges.
    pub include_imports: bool,
    /// Include `Reference` edges.
    pub include_references: bool,
    /// Include `Inherits` and `Implements` edges.
    pub include_inheritance: bool,
    /// Include `Contains` and `Defines` edges.
    pub include_structural: bool,
    /// Include `TypeOf` edges.
    pub include_type_edges: bool,
    /// Include `DatabaseAccess` edges.
    pub include_database: bool,
    /// Include `ServiceInteraction` edges.
    pub include_service: bool,
}

impl EdgeFilter {
    /// All edge types included.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            include_calls: true,
            include_imports: true,
            include_references: true,
            include_inheritance: true,
            include_structural: true,
            include_type_edges: true,
            include_database: true,
            include_service: true,
        }
    }

    /// Only call edges.
    #[must_use]
    pub const fn calls_only() -> Self {
        Self {
            include_calls: true,
            include_imports: false,
            include_references: false,
            include_inheritance: false,
            include_structural: false,
            include_type_edges: false,
            include_database: false,
            include_service: false,
        }
    }

    /// Calls and imports.
    #[must_use]
    pub const fn calls_and_imports() -> Self {
        Self {
            include_calls: true,
            include_imports: true,
            include_references: false,
            include_inheritance: false,
            include_structural: false,
            include_type_edges: false,
            include_database: false,
            include_service: false,
        }
    }

    /// Calls, imports, references, and inheritance (dependency impact).
    #[must_use]
    pub const fn dependency_edges() -> Self {
        Self {
            include_calls: true,
            include_imports: true,
            include_references: true,
            include_inheritance: true,
            include_structural: false,
            include_type_edges: false,
            include_database: false,
            include_service: false,
        }
    }

    /// Returns `true` if the given classification passes the filter.
    #[must_use]
    pub fn accepts(&self, classification: &EdgeClassification) -> bool {
        match classification {
            EdgeClassification::Call { .. } => self.include_calls,
            EdgeClassification::Import { .. } | EdgeClassification::Export { .. } => {
                self.include_imports
            }
            EdgeClassification::Reference => self.include_references,
            EdgeClassification::Inherits | EdgeClassification::Implements => {
                self.include_inheritance
            }
            EdgeClassification::Contains | EdgeClassification::Defines => self.include_structural,
            EdgeClassification::TypeOf => self.include_type_edges,
            EdgeClassification::DatabaseAccess => self.include_database,
            EdgeClassification::ServiceInteraction => self.include_service,
        }
    }
}

/// Resource limits for traversal.
#[derive(Debug, Clone)]
pub struct TraversalLimits {
    /// Maximum BFS depth.
    pub max_depth: u32,
    /// Maximum number of nodes to collect.
    pub max_nodes: Option<usize>,
    /// Maximum number of edges to collect.
    pub max_edges: Option<usize>,
    /// Maximum number of paths to collect (path enumeration only).
    pub max_paths: Option<usize>,
}

impl TraversalLimits {
    /// Default limits for subgraph extraction.
    #[must_use]
    pub const fn default_subgraph() -> Self {
        Self {
            max_depth: 2,
            max_nodes: Some(50),
            max_edges: None,
            max_paths: None,
        }
    }

    /// Default limits for graph export.
    #[must_use]
    pub const fn default_export() -> Self {
        Self {
            max_depth: 2,
            max_nodes: None,
            max_edges: Some(1000),
            max_paths: None,
        }
    }

    /// Default limits for dependency impact analysis.
    #[must_use]
    pub const fn default_impact() -> Self {
        Self {
            max_depth: 3,
            max_nodes: Some(500),
            max_edges: None,
            max_paths: None,
        }
    }

    /// Default limits for path tracing.
    #[must_use]
    pub const fn default_trace() -> Self {
        Self {
            max_depth: 5,
            max_nodes: None,
            max_edges: None,
            max_paths: Some(5),
        }
    }
}

/// Complete traversal configuration.
#[derive(Debug, Clone)]
pub struct TraversalConfig {
    /// Which direction to traverse edges.
    pub direction: TraversalDirection,
    /// Which edge types to include/traverse.
    pub edge_filter: EdgeFilter,
    /// Resource limits.
    pub limits: TraversalLimits,
}

// ──────────────────── Strategy trait ────────────────────

/// What the BFS frontier queue stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierMode {
    /// Queue stores `(NodeId, depth)`. Used by standard BFS consumers.
    Standard,
    /// Queue stores `(NodeId, Vec<NodeId> path, depth)`. Used by `trace_path`.
    PathEnumeration,
}

/// How dedup/cycle-detection works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitedPolicy {
    /// Global `HashSet<NodeId>`. Never re-enqueue visited nodes.
    Global,
    /// No global visited set. Cycle check against in-flight path only.
    /// Allows same node in multiple alternate paths.
    PathLocal,
}

/// Strategy callbacks for customizing traversal behavior.
///
/// Default implementations produce standard BFS with global visited set.
pub trait TraversalStrategy {
    /// Filter edges before processing. Access raw edge for confidence/followability.
    fn accept_edge(&mut self, _edge: &StoreEdgeRef, _depth: u32) -> bool {
        true
    }

    /// Control enqueueing with full context. Supports SCC pruning.
    fn should_enqueue(
        &mut self,
        _node_id: NodeId,
        _from: NodeId,
        _edge: &EdgeKind,
        _depth: u32,
    ) -> bool {
        true
    }

    /// Called when a path reaches target (`PathEnumeration` mode only).
    /// Return `Break` to stop collecting paths.
    fn on_path_complete(&mut self, _path: &[NodeId]) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    /// What the queue stores.
    fn frontier_mode(&self) -> FrontierMode {
        FrontierMode::Standard
    }

    /// How dedup/cycle-detection works.
    fn visited_policy(&self) -> VisitedPolicy {
        VisitedPolicy::Global
    }

    /// Path target for `PathEnumeration` mode. `None` = enumerate all paths to leaves.
    fn path_target(&self) -> Option<NodeId> {
        None
    }
}

/// Default no-op strategy for standard BFS.
struct DefaultStrategy;
impl TraversalStrategy for DefaultStrategy {}

// ──────────────────── Edge followability ────────────────────

/// Returns `true` if an edge kind should be followed during path traversal
/// at the given minimum confidence threshold.
///
/// Confidence mapping:
/// - `Calls`, `TraitMethodBinding`: 1.0 (highest confidence)
/// - `Inherits`, `Implements`: 0.9
/// - `Imports`, `Exports`: 0.8
/// - `References`: 0.7
/// - Cross-boundary calls (`FfiCall`, `HttpRequest`, `GrpcCall`, `WebAssemblyCall`): 0.6
/// - `MacroExpansion`: 0.5
/// - Everything else: 0.3
#[must_use]
pub fn is_followable_edge(kind: &EdgeKind, min_confidence: f64) -> bool {
    let confidence = match kind {
        EdgeKind::Calls { .. } | EdgeKind::TraitMethodBinding { .. } => 1.0,
        EdgeKind::Inherits | EdgeKind::Implements | EdgeKind::SealedPermit => 0.9,
        EdgeKind::Imports { .. } | EdgeKind::Exports { .. } => 0.8,
        EdgeKind::References => 0.7,
        EdgeKind::FfiCall { .. }
        | EdgeKind::HttpRequest { .. }
        | EdgeKind::GrpcCall { .. }
        | EdgeKind::WebAssemblyCall => 0.6,
        EdgeKind::MacroExpansion { .. } => 0.5,
        _ => 0.3,
    };
    confidence >= min_confidence
}

// ──────────────────── Built-in strategies ────────────────────

/// Simple path enumeration strategy for `trace_path` (non-optimized variant).
///
/// Traverses all paths from seeds to `target`, filtering edges by minimum
/// confidence and optionally restricting to same-language edges.
pub struct SimplePathStrategy {
    /// Target node to find paths to.
    target: NodeId,
    /// Minimum confidence threshold for edge followability.
    min_confidence: f64,
    /// Whether to allow cross-language edges.
    allow_cross_language: bool,
}

impl SimplePathStrategy {
    /// Creates a new simple path strategy.
    #[must_use]
    pub fn new(target: NodeId, min_confidence: f64, allow_cross_language: bool) -> Self {
        Self {
            target,
            min_confidence,
            allow_cross_language,
        }
    }
}

impl TraversalStrategy for SimplePathStrategy {
    fn accept_edge(&mut self, edge: &StoreEdgeRef, _depth: u32) -> bool {
        is_followable_edge(&edge.kind, self.min_confidence)
            && (self.allow_cross_language || !edge.kind.is_cross_boundary())
    }

    fn frontier_mode(&self) -> FrontierMode {
        FrontierMode::PathEnumeration
    }

    fn visited_policy(&self) -> VisitedPolicy {
        VisitedPolicy::PathLocal
    }

    fn path_target(&self) -> Option<NodeId> {
        Some(self.target)
    }
}

/// SCC-pruned path enumeration strategy for `trace_path` (optimized variant).
///
/// Uses precomputed SCC data and condensation DAG to prune branches that
/// cannot reach the target node's SCC component.
pub struct SccPathStrategy<'a> {
    /// Precomputed SCC data.
    scc_data: &'a super::analysis::scc::SccData,
    /// Precomputed condensation DAG for reachability queries.
    cond_dag: &'a super::analysis::condensation::CondensationDag,
    /// Target node.
    target: NodeId,
    /// SCC index of the target node (cached).
    target_scc: Option<u32>,
    /// Minimum confidence threshold for edge followability.
    min_confidence: f64,
    /// Whether to allow cross-language edges.
    allow_cross_language: bool,
}

impl<'a> SccPathStrategy<'a> {
    /// Creates a new SCC-pruned path strategy.
    #[must_use]
    pub fn new(
        scc_data: &'a super::analysis::scc::SccData,
        cond_dag: &'a super::analysis::condensation::CondensationDag,
        target: NodeId,
        min_confidence: f64,
        allow_cross_language: bool,
    ) -> Self {
        let target_scc = scc_data.scc_of(target);
        Self {
            scc_data,
            cond_dag,
            target,
            target_scc,
            min_confidence,
            allow_cross_language,
        }
    }
}

impl TraversalStrategy for SccPathStrategy<'_> {
    fn accept_edge(&mut self, edge: &StoreEdgeRef, _depth: u32) -> bool {
        is_followable_edge(&edge.kind, self.min_confidence)
            && (self.allow_cross_language || !edge.kind.is_cross_boundary())
    }

    fn should_enqueue(
        &mut self,
        node_id: NodeId,
        _from: NodeId,
        _edge: &EdgeKind,
        _depth: u32,
    ) -> bool {
        // If target SCC is unknown, allow all (conservative)
        let Some(target_scc) = self.target_scc else {
            return true;
        };

        // If node's SCC is unknown, allow (conservative)
        let Some(node_scc) = self.scc_data.scc_of(node_id) else {
            return true;
        };

        self.cond_dag.can_reach(node_scc, target_scc)
    }

    fn on_path_complete(&mut self, _path: &[NodeId]) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn frontier_mode(&self) -> FrontierMode {
        FrontierMode::PathEnumeration
    }

    fn visited_policy(&self) -> VisitedPolicy {
        VisitedPolicy::PathLocal
    }

    fn path_target(&self) -> Option<NodeId> {
        Some(self.target)
    }
}

// ──────────────────── Public API ────────────────────

/// Execute a graph traversal from the given seeds.
///
/// With `strategy: None`, performs standard BFS with global visited set.
/// With a strategy, behavior is customized via the trait methods.
#[must_use]
pub fn traverse(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    config: &TraversalConfig,
    strategy: Option<&mut dyn TraversalStrategy>,
) -> TraversalResult {
    let mut default_strategy = DefaultStrategy;
    let strategy = strategy.unwrap_or(&mut default_strategy);

    let frontier_mode = strategy.frontier_mode();
    let visited_policy = strategy.visited_policy();

    match (frontier_mode, visited_policy) {
        (FrontierMode::Standard, _) => {
            // Standard + PathLocal is invalid, treat as Standard + Global
            run_standard_bfs(snapshot, seeds, config, strategy)
        }
        (FrontierMode::PathEnumeration, VisitedPolicy::PathLocal) => {
            run_path_bfs(snapshot, seeds, config, strategy)
        }
        (FrontierMode::PathEnumeration, VisitedPolicy::Global) => {
            // Unusual but supported: path enumeration with global visited
            run_path_bfs(snapshot, seeds, config, strategy)
        }
    }
}

// ──────────────────── Standard BFS ────────────────────

/// Raw edge tuple collected during BFS.
struct RawEdge {
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    depth: u32,
}

/// Standard BFS with global visited set.
fn run_standard_bfs(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    config: &TraversalConfig,
    strategy: &mut dyn TraversalStrategy,
) -> TraversalResult {
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<(NodeId, u32)> = VecDeque::new();
    let mut raw_edges: Vec<RawEdge> = Vec::new();
    let mut discovered_order: Vec<NodeId> = Vec::new();
    let mut truncation: Option<TruncationReason> = None;
    let mut max_depth_reached = false;
    let mut nodes_visited: usize = 0;

    // Seed the queue
    for &seed in seeds {
        if visited.insert(seed) {
            discovered_order.push(seed);
            queue.push_back((seed, 0));
        }
    }

    'bfs: while let Some((current, depth)) = queue.pop_front() {
        nodes_visited += 1;

        if depth >= config.limits.max_depth {
            max_depth_reached = true;
            continue;
        }

        // Check node limit
        if let Some(max_nodes) = config.limits.max_nodes
            && discovered_order.len() >= max_nodes
        {
            truncation = Some(TruncationReason::NodeLimit);
            break;
        }

        let edges = collect_edges(snapshot, current, config.direction);

        for edge_ref in &edges {
            // Check edge limit
            if let Some(max_edges) = config.limits.max_edges
                && raw_edges.len() >= max_edges
            {
                truncation = Some(TruncationReason::EdgeLimit);
                break 'bfs;
            }

            let classification = EdgeClassification::from(&edge_ref.kind);
            if !config.edge_filter.accepts(&classification) {
                continue;
            }

            if !strategy.accept_edge(edge_ref, depth) {
                continue;
            }

            // Determine the "next" node depending on direction
            let next = neighbor_of(edge_ref, current);

            raw_edges.push(RawEdge {
                source: edge_ref.source,
                target: edge_ref.target,
                kind: edge_ref.kind.clone(),
                depth: depth + 1,
            });

            if visited.insert(next)
                && strategy.should_enqueue(next, current, &edge_ref.kind, depth + 1)
            {
                // Enforce node limit before adding
                if let Some(max_nodes) = config.limits.max_nodes
                    && discovered_order.len() >= max_nodes
                {
                    truncation = Some(TruncationReason::NodeLimit);
                    break 'bfs;
                }
                discovered_order.push(next);
                queue.push_back((next, depth + 1));
            }
        }
    }

    materialize_result(
        snapshot,
        &discovered_order,
        &raw_edges,
        None,
        truncation,
        max_depth_reached,
        seeds.len(),
        nodes_visited,
    )
}

// ──────────────────── Path enumeration BFS (stub for Task 4) ────────────────────

/// Path enumeration BFS with path-local cycle detection.
#[allow(clippy::too_many_lines)] // Kernel query dispatches across all edge kinds; complex path-tracking BFS state machine
fn run_path_bfs(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    config: &TraversalConfig,
    strategy: &mut dyn TraversalStrategy,
) -> TraversalResult {
    let target = strategy.path_target();
    let mut collected_paths: Vec<Vec<NodeId>> = Vec::new();
    let mut discovered_order: Vec<NodeId> = Vec::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut raw_edges: Vec<RawEdge> = Vec::new();
    let mut truncation: Option<TruncationReason> = None;
    let mut max_depth_reached = false;
    let mut nodes_visited: usize = 0;

    // Queue: (current_node, path_so_far, depth)
    let mut queue: VecDeque<(NodeId, Vec<NodeId>, u32)> = VecDeque::new();

    for &seed in seeds {
        queue.push_back((seed, vec![seed], 0));
        if seen.insert(seed) {
            discovered_order.push(seed);
        }
    }

    let use_global_visited = strategy.visited_policy() == VisitedPolicy::Global;
    let mut global_visited: HashSet<NodeId> = if use_global_visited {
        seeds.iter().copied().collect()
    } else {
        HashSet::new()
    };

    'bfs: while let Some((current, path, depth)) = queue.pop_front() {
        nodes_visited += 1;

        // Check if we reached the target
        if let Some(t) = target
            && current == t
            && path.len() > 1
        {
            // Record nodes along the path in discovery order
            for &node in &path {
                if seen.insert(node) {
                    discovered_order.push(node);
                }
            }

            let control = strategy.on_path_complete(&path);
            collected_paths.push(path);

            // Check path limit
            if let Some(max_paths) = config.limits.max_paths
                && collected_paths.len() >= max_paths
            {
                truncation = Some(TruncationReason::PathLimit);
                break 'bfs;
            }

            if control.is_break() {
                break 'bfs;
            }
            continue;
        }

        if depth >= config.limits.max_depth {
            max_depth_reached = true;
            continue;
        }

        // Check node limit
        if let Some(max_nodes) = config.limits.max_nodes
            && discovered_order.len() >= max_nodes
        {
            truncation = Some(TruncationReason::NodeLimit);
            break;
        }

        let edges = collect_edges(snapshot, current, config.direction);

        // If no target specified, leaf nodes (no followable outgoing edges) complete a path
        let mut has_followable_successor = false;

        for edge_ref in &edges {
            // Check edge limit
            if let Some(max_edges) = config.limits.max_edges
                && raw_edges.len() >= max_edges
            {
                truncation = Some(TruncationReason::EdgeLimit);
                break 'bfs;
            }

            let classification = EdgeClassification::from(&edge_ref.kind);
            if !config.edge_filter.accepts(&classification) {
                continue;
            }

            if !strategy.accept_edge(edge_ref, depth) {
                continue;
            }

            let next = neighbor_of(edge_ref, current);

            // Cycle detection: path-local or global
            if use_global_visited {
                if !global_visited.insert(next) {
                    continue;
                }
            } else if path.contains(&next) {
                continue;
            }

            if !strategy.should_enqueue(next, current, &edge_ref.kind, depth + 1) {
                continue;
            }

            has_followable_successor = true;

            raw_edges.push(RawEdge {
                source: edge_ref.source,
                target: edge_ref.target,
                kind: edge_ref.kind.clone(),
                depth: depth + 1,
            });

            // Enforce node limit before adding
            if let Some(max_nodes) = config.limits.max_nodes
                && discovered_order.len() >= max_nodes
            {
                truncation = Some(TruncationReason::NodeLimit);
                break 'bfs;
            }

            if seen.insert(next) {
                discovered_order.push(next);
            }

            let mut new_path = path.clone();
            new_path.push(next);
            queue.push_back((next, new_path, depth + 1));
        }

        // Leaf enumeration: if no target specified and node is a leaf, record path
        if target.is_none() && !has_followable_successor && path.len() > 1 {
            for &node in &path {
                if seen.insert(node) {
                    discovered_order.push(node);
                }
            }

            let control = strategy.on_path_complete(&path);
            collected_paths.push(path);

            if let Some(max_paths) = config.limits.max_paths
                && collected_paths.len() >= max_paths
            {
                truncation = Some(TruncationReason::PathLimit);
                break 'bfs;
            }

            if control.is_break() {
                break 'bfs;
            }
        }
    }

    let paths_for_result = Some(collected_paths);

    materialize_result(
        snapshot,
        &discovered_order,
        &raw_edges,
        paths_for_result,
        truncation,
        max_depth_reached,
        seeds.len(),
        nodes_visited,
    )
}

// ──────────────────── Shared helpers ────────────────────

/// Collect edges for a node in the given direction.
fn collect_edges(
    snapshot: &GraphSnapshot,
    node: NodeId,
    direction: TraversalDirection,
) -> Vec<StoreEdgeRef> {
    match direction {
        TraversalDirection::Outgoing => snapshot.edges().edges_from(node),
        TraversalDirection::Incoming => snapshot.edges().edges_to(node),
        TraversalDirection::Both => {
            let mut edges = snapshot.edges().edges_from(node);
            edges.extend(snapshot.edges().edges_to(node));
            edges
        }
    }
}

/// Given an edge and the current node, return the neighbor to traverse to.
fn neighbor_of(edge_ref: &StoreEdgeRef, current: NodeId) -> NodeId {
    if edge_ref.source == current {
        edge_ref.target
    } else {
        edge_ref.source
    }
}

/// Materialize collected BFS results into a `TraversalResult`.
///
/// Converts discovered `NodeId`s into `MaterializedNode`s, builds the index map,
/// converts raw edges to `MaterializedEdge`s, and optionally converts paths.
#[allow(clippy::too_many_arguments)]
fn materialize_result(
    snapshot: &GraphSnapshot,
    discovered_order: &[NodeId],
    raw_edges: &[RawEdge],
    raw_paths: Option<Vec<Vec<NodeId>>>,
    truncation: Option<TruncationReason>,
    max_depth_reached: bool,
    seed_count: usize,
    nodes_visited: usize,
) -> TraversalResult {
    // Materialize nodes and build index map
    let mut nodes = Vec::with_capacity(discovered_order.len());
    let mut node_index: HashMap<NodeId, usize> = HashMap::with_capacity(discovered_order.len());

    for &node_id in discovered_order {
        if node_index.contains_key(&node_id) {
            continue;
        }
        if let Some(materialized) = materialize_node(snapshot, node_id) {
            let idx = nodes.len();
            node_index.insert(node_id, idx);
            nodes.push(materialized);
        }
    }

    // Convert raw edges to MaterializedEdge, dropping any with missing indices
    let mut edges = Vec::with_capacity(raw_edges.len());
    for raw in raw_edges {
        if let (Some(&source_idx), Some(&target_idx)) =
            (node_index.get(&raw.source), node_index.get(&raw.target))
        {
            edges.push(MaterializedEdge {
                source_idx,
                target_idx,
                classification: EdgeClassification::from(&raw.kind),
                raw_kind: raw.kind.clone(),
                depth: raw.depth,
            });
        }
    }

    // Deduplicate edges (same source_idx, target_idx, classification)
    edges.dedup_by(|a, b| {
        a.source_idx == b.source_idx
            && a.target_idx == b.target_idx
            && a.classification == b.classification
    });

    // Convert paths
    let paths = raw_paths.map(|path_list| {
        path_list
            .iter()
            .filter_map(|path| {
                let converted: Vec<usize> = path
                    .iter()
                    .filter_map(|node_id| node_index.get(node_id).copied())
                    .collect();
                // Drop paths where any node couldn't be materialized
                if converted.len() == path.len() {
                    Some(converted)
                } else {
                    None
                }
            })
            .collect()
    });

    TraversalResult {
        metadata: TraversalMetadata {
            truncation,
            max_depth_reached,
            seed_count,
            nodes_visited,
            total_nodes: nodes.len(),
            total_edges: edges.len(),
        },
        nodes,
        edges,
        paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;

    use crate::graph::unified::file::FileId;

    /// Helper to create a test graph with nodes and edges.
    struct TestGraph {
        graph: CodeGraph,
        file_id: Option<FileId>,
    }

    impl TestGraph {
        fn new() -> Self {
            Self {
                graph: CodeGraph::new(),
                file_id: None,
            }
        }

        fn ensure_file_id(&mut self) -> FileId {
            if let Some(fid) = self.file_id {
                return fid;
            }
            let file_path = std::path::PathBuf::from("/kernel-tests/test.rs");
            let fid = self
                .graph
                .files_mut()
                .register_with_language(&file_path, Some(Language::Rust))
                .unwrap();
            self.file_id = Some(fid);
            fid
        }

        fn add_node(&mut self, name: &str) -> NodeId {
            self.add_node_with_kind(name, NodeKind::Function)
        }

        fn add_node_with_kind(&mut self, name: &str, kind: NodeKind) -> NodeId {
            let file_id = self.ensure_file_id();
            let name_id = self.graph.strings_mut().intern(name).unwrap();
            let qn_id = self
                .graph
                .strings_mut()
                .intern(&format!("test::{name}"))
                .unwrap();

            let entry = NodeEntry::new(kind, name_id, file_id)
                .with_qualified_name(qn_id)
                .with_location(1, 0, 10, 0);

            let node_id = self.graph.nodes_mut().alloc(entry).unwrap();
            self.graph
                .indices_mut()
                .add(node_id, kind, name_id, Some(qn_id), file_id);
            node_id
        }

        fn add_call_edge(&mut self, source: NodeId, target: NodeId) {
            let file_id = self.ensure_file_id();
            self.graph.edges_mut().add_edge(
                source,
                target,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file_id,
            );
        }

        fn add_edge(&mut self, source: NodeId, target: NodeId, kind: EdgeKind) {
            let file_id = self.ensure_file_id();
            self.graph
                .edges_mut()
                .add_edge(source, target, kind, file_id);
        }

        fn snapshot(&self) -> GraphSnapshot {
            self.graph.snapshot()
        }
    }

    fn calls_config(depth: u32) -> TraversalConfig {
        TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: depth,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        }
    }

    #[test]
    fn standard_outgoing_bfs() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);

        let snapshot = tg.snapshot();
        let result = traverse(&snapshot, &[a], &calls_config(3), None);

        assert_eq!(result.nodes.len(), 3);
        assert_eq!(result.edges.len(), 2);
        assert!(result.metadata.truncation.is_none());
        // First node should be the seed
        assert_eq!(result.nodes[0].node_id, a);
    }

    #[test]
    fn depth_limit() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);

        let snapshot = tg.snapshot();
        let result = traverse(&snapshot, &[a], &calls_config(1), None);

        // Depth 1: should discover a and b, but not c (b→c is at depth 2)
        assert_eq!(result.nodes.len(), 2);
        assert_eq!(result.edges.len(), 1);
        assert!(result.metadata.max_depth_reached);
    }

    #[test]
    fn node_limit_truncation() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        let d = tg.add_node("d");
        tg.add_call_edge(a, b);
        tg.add_call_edge(a, c);
        tg.add_call_edge(a, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: Some(2),
                max_edges: None,
                max_paths: None,
            },
        };

        let result = traverse(&snapshot, &[a], &config, None);

        assert_eq!(
            result.metadata.truncation,
            Some(TruncationReason::NodeLimit)
        );
    }

    #[test]
    fn empty_seeds() {
        let tg = TestGraph::new();
        let snapshot = tg.snapshot();
        let result = traverse(&snapshot, &[], &calls_config(3), None);

        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
        assert!(result.metadata.truncation.is_none());
        assert_eq!(result.metadata.seed_count, 0);
    }

    #[test]
    fn incoming_bfs() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_call_edge(c, b);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Incoming,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 3,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        };

        let result = traverse(&snapshot, &[b], &config, None);

        // b's incoming edges are from a and c
        assert_eq!(result.nodes.len(), 3);
    }

    #[test]
    fn bidirectional_bfs() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Both,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 3,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        };

        // Start from b — should find a (incoming) and c (outgoing)
        let result = traverse(&snapshot, &[b], &config, None);
        assert_eq!(result.nodes.len(), 3);
    }

    #[test]
    fn edge_filtering() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_edge(
            a,
            c,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
        );

        let snapshot = tg.snapshot();

        // calls_only should not include the import edge
        let result = traverse(&snapshot, &[a], &calls_config(3), None);
        assert_eq!(result.nodes.len(), 2); // a and b only

        // calls_and_imports should include both
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_and_imports(),
            limits: TraversalLimits {
                max_depth: 3,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        };
        let result = traverse(&snapshot, &[a], &config, None);
        assert_eq!(result.nodes.len(), 3); // a, b, and c
    }

    #[test]
    fn cycle_handling_standard() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, a);

        let snapshot = tg.snapshot();
        let result = traverse(&snapshot, &[a], &calls_config(10), None);

        // Global visited set prevents infinite loop
        assert_eq!(result.nodes.len(), 2);
    }

    #[test]
    fn edge_limit_truncation() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        let d = tg.add_node("d");
        tg.add_call_edge(a, b);
        tg.add_call_edge(a, c);
        tg.add_call_edge(a, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: None,
                max_edges: Some(2),
                max_paths: None,
            },
        };

        let result = traverse(&snapshot, &[a], &config, None);
        assert_eq!(
            result.metadata.truncation,
            Some(TruncationReason::EdgeLimit)
        );
    }

    #[test]
    fn index_invariants_hold() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        let c = tg.add_node("c");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);

        let snapshot = tg.snapshot();
        let result = traverse(&snapshot, &[a], &calls_config(5), None);

        // Every edge index must be < nodes.len()
        for edge in &result.edges {
            assert!(edge.source_idx < result.nodes.len());
            assert!(edge.target_idx < result.nodes.len());
        }

        // Metadata is consistent
        assert_eq!(result.metadata.total_nodes, result.nodes.len());
        assert_eq!(result.metadata.total_edges, result.edges.len());
    }

    #[test]
    fn all_filter_includes_structural() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("a");
        let b = tg.add_node("b");
        tg.add_edge(a, b, EdgeKind::Defines);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::all(),
            limits: TraversalLimits {
                max_depth: 3,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        };
        let result = traverse(&snapshot, &[a], &config, None);
        assert_eq!(result.nodes.len(), 2);
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].classification, EdgeClassification::Defines);
    }

    // ──────────────────── Path enumeration tests ────────────────────

    #[test]
    fn path_enumeration_finds_path() {
        let mut tg = TestGraph::new();
        let a = tg.add_node("pa");
        let b = tg.add_node("pb");
        let c = tg.add_node("pc");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(c, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        assert!(result.paths.is_some());
        let paths = result.paths.as_ref().unwrap();
        assert_eq!(paths.len(), 1, "expected 1 path from a→c");
        // Path should have 3 nodes: a → b → c
        assert_eq!(paths[0].len(), 3);
    }

    #[test]
    fn path_local_allows_shared_nodes_in_diamond() {
        // Diamond graph: a→b, a→c, b→d, c→d
        let mut tg = TestGraph::new();
        let a = tg.add_node("da");
        let b = tg.add_node("db");
        let c = tg.add_node("dc");
        let d = tg.add_node("dd");
        tg.add_call_edge(a, b);
        tg.add_call_edge(a, c);
        tg.add_call_edge(b, d);
        tg.add_call_edge(c, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(d, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        let paths = result.paths.as_ref().unwrap();
        // Two paths: a→b→d and a→c→d
        assert_eq!(paths.len(), 2, "expected 2 paths in diamond, got {paths:?}");
    }

    #[test]
    fn path_enumeration_handles_cycles() {
        // a→b→c→a (cycle), looking for path to c
        let mut tg = TestGraph::new();
        let a = tg.add_node("ca");
        let b = tg.add_node("cb");
        let c = tg.add_node("cc");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);
        tg.add_call_edge(c, a); // cycle back

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 10,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(c, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        let paths = result.paths.as_ref().unwrap();
        // Should find a→b→c without infinite loop
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].len(), 3);
    }

    #[test]
    fn path_enumeration_no_path() {
        // a→b, c is disconnected, looking for path from a to c
        let mut tg = TestGraph::new();
        let a = tg.add_node("na");
        let b = tg.add_node("nb");
        let c = tg.add_node("nc");
        tg.add_call_edge(a, b);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(c, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        let paths = result.paths.as_ref().unwrap();
        assert!(paths.is_empty(), "expected no paths, got {paths:?}");
    }

    #[test]
    fn path_limit_truncation() {
        // Create a graph with many paths: a→b1, a→b2, a→b3, b1→c, b2→c, b3→c
        let mut tg = TestGraph::new();
        let a = tg.add_node("la");
        let b1 = tg.add_node("lb1");
        let b2 = tg.add_node("lb2");
        let b3 = tg.add_node("lb3");
        let c = tg.add_node("lc");
        tg.add_call_edge(a, b1);
        tg.add_call_edge(a, b2);
        tg.add_call_edge(a, b3);
        tg.add_call_edge(b1, c);
        tg.add_call_edge(b2, c);
        tg.add_call_edge(b3, c);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 5,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(2),
            },
        };

        let mut strategy = SimplePathStrategy::new(c, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        let paths = result.paths.as_ref().unwrap();
        assert_eq!(paths.len(), 2, "expected exactly 2 paths (limited)");
        assert_eq!(
            result.metadata.truncation,
            Some(TruncationReason::PathLimit)
        );
    }

    #[test]
    fn path_bfs_node_limit_truncation() {
        // a→b→c→d, path to d, but max_nodes=2
        let mut tg = TestGraph::new();
        let a = tg.add_node("pna");
        let b = tg.add_node("pnb");
        let c = tg.add_node("pnc");
        let d = tg.add_node("pnd");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);
        tg.add_call_edge(c, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 10,
                max_nodes: Some(2),
                max_edges: None,
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(d, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        assert!(
            result.nodes.len() <= 2,
            "node limit violated: {} nodes",
            result.nodes.len()
        );
        assert_eq!(
            result.metadata.truncation,
            Some(TruncationReason::NodeLimit)
        );
    }

    #[test]
    fn path_bfs_edge_limit_truncation() {
        // a→b→c→d, path to d, but max_edges=1
        let mut tg = TestGraph::new();
        let a = tg.add_node("pea");
        let b = tg.add_node("peb");
        let c = tg.add_node("pec");
        let d = tg.add_node("ped");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);
        tg.add_call_edge(c, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 10,
                max_nodes: None,
                max_edges: Some(1),
                max_paths: Some(10),
            },
        };

        let mut strategy = SimplePathStrategy::new(d, 0.0, true);
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        assert_eq!(
            result.metadata.truncation,
            Some(TruncationReason::EdgeLimit)
        );
    }

    #[test]
    fn path_bfs_leaf_enumeration_no_target() {
        // a→b→c (leaf), a→d (leaf) — enumerate paths to all leaves
        let mut tg = TestGraph::new();
        let a = tg.add_node("la");
        let b = tg.add_node("lb");
        let c = tg.add_node("lc");
        let d = tg.add_node("ld");
        tg.add_call_edge(a, b);
        tg.add_call_edge(b, c);
        tg.add_call_edge(a, d);

        let snapshot = tg.snapshot();
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            edge_filter: EdgeFilter::calls_only(),
            limits: TraversalLimits {
                max_depth: 10,
                max_nodes: None,
                max_edges: None,
                max_paths: Some(10),
            },
        };

        // No target — should enumerate paths to leaves
        let mut strategy = LeafPathStrategy;
        let result = traverse(&snapshot, &[a], &config, Some(&mut strategy));

        let paths = result.paths.as_ref().unwrap();
        // Two leaf paths: a→b→c and a→d
        assert!(
            paths.len() >= 2,
            "expected at least 2 leaf paths, got {}",
            paths.len()
        );
    }

    /// Strategy that enumerates paths to all leaves (no specific target).
    struct LeafPathStrategy;

    impl TraversalStrategy for LeafPathStrategy {
        fn frontier_mode(&self) -> FrontierMode {
            FrontierMode::PathEnumeration
        }

        fn visited_policy(&self) -> VisitedPolicy {
            VisitedPolicy::PathLocal
        }

        fn path_target(&self) -> Option<NodeId> {
            None
        }
    }

    #[test]
    fn is_followable_edge_confidence_filtering() {
        // High confidence edges pass any threshold
        let calls = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        assert!(is_followable_edge(&calls, 0.0));
        assert!(is_followable_edge(&calls, 1.0));

        // FFI call has 0.6 confidence
        let ffi = EdgeKind::FfiCall {
            convention: crate::graph::unified::edge::kind::FfiConvention::C,
        };
        assert!(is_followable_edge(&ffi, 0.5));
        assert!(is_followable_edge(&ffi, 0.6));
        assert!(!is_followable_edge(&ffi, 0.7));

        // Defines has 0.3 confidence
        assert!(is_followable_edge(&EdgeKind::Defines, 0.3));
        assert!(!is_followable_edge(&EdgeKind::Defines, 0.4));
    }
}
