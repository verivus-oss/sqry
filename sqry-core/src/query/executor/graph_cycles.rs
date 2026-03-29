//! Cycle detection for unified graph.
//!
//! This module provides cycle detection using the unified graph API,
//! replacing the legacy index-based cycle detection.

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use std::collections::{HashMap, HashSet};

/// Type of circular dependency to detect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircularType {
    /// Function/method call cycles (A calls B calls C calls A)
    Calls,
    /// File import cycles (a.rs imports b.rs imports a.rs)
    Imports,
    /// Module-level cycles (aggregated from import graph)
    Modules,
}

impl CircularType {
    /// Parse circular type from query value string
    #[must_use]
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "calls" | "call" => Some(Self::Calls),
            "imports" | "import" => Some(Self::Imports),
            "modules" | "module" => Some(Self::Modules),
            _ => None,
        }
    }
}

/// Configuration for circular dependency detection
#[derive(Debug, Clone)]
pub struct CircularConfig {
    /// Minimum cycle depth to report (default: 2)
    pub min_depth: usize,
    /// Maximum cycle depth to report (default: unbounded)
    pub max_depth: Option<usize>,
    /// Maximum results to return (default: 100)
    pub max_results: usize,
    /// Include self-loops (A -> A) in results (default: false)
    pub should_include_self_loops: bool,
}

impl Default for CircularConfig {
    fn default() -> Self {
        Self {
            min_depth: 2,
            max_depth: None,
            max_results: 100,
            should_include_self_loops: false,
        }
    }
}

/// Frame for iterative Tarjan's algorithm (simulates recursion stack)
struct TarjanFrame {
    /// Current node being processed
    node: NodeId,
    /// Index into successor list (tracks which successors have been visited)
    successor_idx: usize,
    /// Phase: false = pre-visit, true = post-visit
    is_post_visit: bool,
}

/// Tarjan's SCC algorithm state for graph nodes
struct GraphTarjanState {
    /// Current index counter
    index: usize,
    /// Stack of nodes in current SCC discovery
    stack: Vec<NodeId>,
    /// Set of nodes on stack (for O(1) lookup)
    on_stack: HashSet<NodeId>,
    /// Index assigned to each node
    indices: HashMap<NodeId, usize>,
    /// Lowlink value for each node
    lowlinks: HashMap<NodeId, usize>,
    /// Discovered SCCs
    sccs: Vec<Vec<NodeId>>,
}

impl GraphTarjanState {
    fn new() -> Self {
        Self {
            index: 0,
            stack: Vec::new(),
            on_stack: HashSet::new(),
            indices: HashMap::new(),
            lowlinks: HashMap::new(),
            sccs: Vec::new(),
        }
    }

    fn init_node(&mut self, v: NodeId) {
        self.indices.insert(v, self.index);
        self.lowlinks.insert(v, self.index);
        self.index += 1;
        self.stack.push(v);
        self.on_stack.insert(v);
    }

    fn handle_post_visit(&mut self, v: NodeId, work_stack: &[TarjanFrame]) {
        if let Some(parent_frame) = work_stack.last() {
            let v_lowlink = *self.lowlinks.get(&v).unwrap();
            let parent_lowlink = self.lowlinks.get_mut(&parent_frame.node).unwrap();
            *parent_lowlink = (*parent_lowlink).min(v_lowlink);
        }

        let v_index = *self.indices.get(&v).unwrap();
        let v_lowlink = *self.lowlinks.get(&v).unwrap();
        if v_lowlink == v_index {
            self.extract_scc(v);
        }
    }

    fn extract_scc(&mut self, root: NodeId) {
        let mut scc = Vec::new();
        loop {
            let w = self.stack.pop().unwrap();
            self.on_stack.remove(&w);
            scc.push(w);
            if w == root {
                break;
            }
        }
        self.sccs.push(scc);
    }
}

/// Collect call graph adjacency from unified graph edges
fn collect_call_adjacency(
    graph: &CodeGraph,
) -> (
    HashSet<NodeId>,
    HashMap<NodeId, Vec<NodeId>>,
    HashSet<NodeId>,
) {
    let mut all_nodes: HashSet<NodeId> = HashSet::new();
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut self_loops: HashSet<NodeId> = HashSet::new();

    // Iterate over all nodes
    for (node_id, _entry) in graph.nodes().iter() {
        all_nodes.insert(node_id);

        // Get outgoing call edges
        for edge in graph.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Calls { .. }) {
                adjacency.entry(node_id).or_default().push(edge.target);

                if node_id == edge.target {
                    self_loops.insert(node_id);
                }
            }
        }
    }

    (all_nodes, adjacency, self_loops)
}

/// Collect import graph adjacency from unified graph edges
fn collect_import_adjacency(
    graph: &CodeGraph,
) -> (
    HashSet<NodeId>,
    HashMap<NodeId, Vec<NodeId>>,
    HashSet<NodeId>,
) {
    let mut all_nodes: HashSet<NodeId> = HashSet::new();
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut self_loops: HashSet<NodeId> = HashSet::new();

    // Iterate over all nodes
    for (node_id, _entry) in graph.nodes().iter() {
        all_nodes.insert(node_id);

        // Get outgoing import edges
        for edge in graph.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Imports { .. }) {
                adjacency.entry(node_id).or_default().push(edge.target);

                if node_id == edge.target {
                    self_loops.insert(node_id);
                }
            }
        }
    }

    (all_nodes, adjacency, self_loops)
}

/// Iterative Tarjan's strongconnect for graph nodes
fn tarjan_strongconnect_iterative(
    start: NodeId,
    adjacency: &HashMap<NodeId, Vec<NodeId>>,
    state: &mut GraphTarjanState,
) {
    let mut work_stack: Vec<TarjanFrame> = vec![TarjanFrame {
        node: start,
        successor_idx: 0,
        is_post_visit: false,
    }];

    while let Some(frame) = work_stack.last_mut() {
        let v = frame.node;

        if frame.is_post_visit {
            work_stack.pop();
            state.handle_post_visit(v, &work_stack);
            continue;
        }

        if !state.indices.contains_key(&v) {
            state.init_node(v);
        }

        let mut successor_idx = frame.successor_idx;
        let has_pushed_child =
            process_successors(v, adjacency, &mut successor_idx, &mut work_stack, state);

        if let Some(current_frame) = work_stack.iter_mut().rev().find(|f| f.node == v) {
            current_frame.successor_idx = successor_idx;
            if !has_pushed_child {
                current_frame.is_post_visit = true;
            }
        }
    }
}

fn process_successors(
    v: NodeId,
    adjacency: &HashMap<NodeId, Vec<NodeId>>,
    successor_idx: &mut usize,
    work_stack: &mut Vec<TarjanFrame>,
    state: &mut GraphTarjanState,
) -> bool {
    let Some(successors) = adjacency.get(&v) else {
        return false;
    };

    while *successor_idx < successors.len() {
        let w = successors[*successor_idx];
        *successor_idx += 1;

        if !state.indices.contains_key(&w) {
            work_stack.push(TarjanFrame {
                node: w,
                successor_idx: 0,
                is_post_visit: false,
            });
            return true;
        }

        if state.on_stack.contains(&w) {
            let w_index = *state.indices.get(&w).unwrap();
            let v_lowlink = state.lowlinks.get_mut(&v).unwrap();
            *v_lowlink = (*v_lowlink).min(w_index);
        }
    }

    false
}

fn should_include_scc(size: usize, is_self_loop: bool, config: &CircularConfig) -> bool {
    // Self-loops (size 1) need explicit inclusion via config
    if is_self_loop {
        return config.should_include_self_loops;
    }

    // Single-node non-self-loop SCCs aren't cycles
    if size == 1 {
        return false;
    }

    // Check min_depth bound
    if size < config.min_depth {
        return false;
    }

    // Check max_depth bound
    if config.max_depth.is_some_and(|max| size > max) {
        return false;
    }

    true
}

/// Find all cycles in the graph using call or import edges.
///
/// Uses Tarjan's algorithm to find strongly connected components (SCCs),
/// then filters to those with more than one node or self-loops.
///
/// # Arguments
///
/// * `circular_type` - The type of edges to follow (Calls, Imports, or Modules)
/// * `graph` - The code graph to analyze
/// * `config` - Configuration for min/max depth, self-loop inclusion, and result limits
///
/// # Returns
///
/// A vector of cycles, where each cycle is a vector of qualified symbol names.
/// Cycles are sorted by size (largest first) and limited by `config.max_results`.
#[must_use]
pub fn find_all_cycles_graph(
    circular_type: CircularType,
    graph: &CodeGraph,
    config: &CircularConfig,
) -> Vec<Vec<String>> {
    let (all_nodes, adjacency, self_loops) = match circular_type {
        CircularType::Calls => collect_call_adjacency(graph),
        CircularType::Imports | CircularType::Modules => collect_import_adjacency(graph),
    };

    let mut state = GraphTarjanState::new();

    // Run Tarjan's algorithm from each unvisited node
    for node in &all_nodes {
        if !state.indices.contains_key(node) {
            tarjan_strongconnect_iterative(*node, &adjacency, &mut state);
        }
    }

    // Filter and convert SCCs to qualified names
    let strings = graph.strings();
    let mut result = Vec::new();

    for scc in state.sccs {
        let size = scc.len();
        let is_self_loop = size == 1 && self_loops.contains(&scc[0]);

        if !should_include_scc(size, is_self_loop, config) {
            continue;
        }

        // Convert NodeIds to qualified names
        let names: Vec<String> = scc
            .iter()
            .filter_map(|&node_id| {
                let entry = graph.nodes().get(node_id)?;
                let name = entry
                    .qualified_name
                    .and_then(|id| strings.resolve(id))
                    .or_else(|| strings.resolve(entry.name))
                    .map(|s| s.to_string())?;
                Some(name)
            })
            .collect();

        if !names.is_empty() {
            result.push(names);
        }

        if result.len() >= config.max_results {
            break;
        }
    }

    result
}

/// Check if a specific node is part of a cycle.
///
/// Uses SCC-based semantics consistent with `find_all_cycles_graph`. The node
/// is considered in a cycle if it belongs to a strongly connected component (SCC)
/// whose size meets the `min_depth`/`max_depth` criteria, or if it has a self-loop
/// and `include_self_loops` is true.
///
/// # Arguments
///
/// * `node_id` - The node to check for cycle membership
/// * `circular_type` - The type of edges to follow (Calls, Imports, or Modules)
/// * `graph` - The code graph to analyze
/// * `config` - Configuration for min/max depth and self-loop inclusion
///
/// # Returns
///
/// `true` if the node is part of a cycle that meets the config criteria, `false` otherwise.
/// Returns `false` for invalid or non-existent nodes.
#[must_use]
pub fn is_node_in_cycle(
    node_id: NodeId,
    circular_type: CircularType,
    graph: &CodeGraph,
    config: &CircularConfig,
) -> bool {
    // Early return if the node doesn't exist
    if graph.nodes().get(node_id).is_none() {
        return false;
    }

    // Build adjacency list and collect self-loops based on cycle type
    let (adjacency, self_loops) = match circular_type {
        CircularType::Calls => collect_call_adjacency_with_self_loops(graph),
        CircularType::Imports | CircularType::Modules => {
            collect_import_adjacency_with_self_loops(graph)
        }
    };

    // Check for self-loop using same semantics as find_all_cycles_graph:
    // Self-loops are included when `should_include_self_loops` is true,
    // regardless of min_depth (they're treated as a special case)
    if self_loops.contains(&node_id) && config.should_include_self_loops {
        return true;
    }

    // Find the SCC containing this node using targeted Tarjan's algorithm
    find_scc_containing_node(node_id, &adjacency, config)
}

/// Collect call graph adjacency with self-loop tracking.
fn collect_call_adjacency_with_self_loops(
    graph: &CodeGraph,
) -> (HashMap<NodeId, Vec<NodeId>>, HashSet<NodeId>) {
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut self_loops: HashSet<NodeId> = HashSet::new();

    for (node_id, _entry) in graph.nodes().iter() {
        for edge in graph.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Calls { .. }) {
                adjacency.entry(node_id).or_default().push(edge.target);
                if node_id == edge.target {
                    self_loops.insert(node_id);
                }
            }
        }
    }

    (adjacency, self_loops)
}

/// Collect import graph adjacency with self-loop tracking.
fn collect_import_adjacency_with_self_loops(
    graph: &CodeGraph,
) -> (HashMap<NodeId, Vec<NodeId>>, HashSet<NodeId>) {
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut self_loops: HashSet<NodeId> = HashSet::new();

    for (node_id, _entry) in graph.nodes().iter() {
        for edge in graph.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Imports { .. }) {
                adjacency.entry(node_id).or_default().push(edge.target);
                if node_id == edge.target {
                    self_loops.insert(node_id);
                }
            }
        }
    }

    (adjacency, self_loops)
}

/// State for the targeted Tarjan's algorithm used by `find_scc_containing_node`.
struct TargetedTarjanState {
    index_counter: usize,
    indices: HashMap<NodeId, usize>,
    lowlinks: HashMap<NodeId, usize>,
    on_stack: HashSet<NodeId>,
    stack: Vec<NodeId>,
}

impl TargetedTarjanState {
    fn new() -> Self {
        Self {
            index_counter: 0,
            indices: HashMap::new(),
            lowlinks: HashMap::new(),
            on_stack: HashSet::new(),
            stack: Vec::new(),
        }
    }

    fn init_node(&mut self, node: NodeId) {
        if let std::collections::hash_map::Entry::Vacant(e) = self.indices.entry(node) {
            e.insert(self.index_counter);
            self.lowlinks.insert(node, self.index_counter);
            self.index_counter += 1;
            self.stack.push(node);
            self.on_stack.insert(node);
        }
    }

    /// Handle post-visit: update parent lowlink and check for SCC root.
    ///
    /// Returns `true` if the target node was found in an SCC that meets the config criteria.
    fn handle_post_visit(
        &mut self,
        node: NodeId,
        target: NodeId,
        work_stack: &[(NodeId, usize, bool)],
        config: &CircularConfig,
    ) -> bool {
        // Update parent's lowlink
        if let Some(&(parent_node, _, _)) = work_stack.last() {
            let node_lowlink = *self.lowlinks.get(&node).unwrap_or(&usize::MAX);
            if let Some(parent_lowlink) = self.lowlinks.get_mut(&parent_node) {
                *parent_lowlink = (*parent_lowlink).min(node_lowlink);
            }
        }

        // Check if this node is the root of an SCC
        let node_index = *self.indices.get(&node).unwrap_or(&usize::MAX);
        let node_lowlink = *self.lowlinks.get(&node).unwrap_or(&usize::MAX);

        if node_lowlink == node_index {
            return self.extract_and_check_scc(node, target, config);
        }

        false
    }

    /// Extract the SCC rooted at `root` and check if it contains `target` and meets size criteria.
    fn extract_and_check_scc(
        &mut self,
        root: NodeId,
        target: NodeId,
        config: &CircularConfig,
    ) -> bool {
        let mut scc_size = 0;
        let mut contains_target = false;
        loop {
            let w = self.stack.pop().unwrap();
            self.on_stack.remove(&w);
            scc_size += 1;
            if w == target {
                contains_target = true;
            }
            if w == root {
                break;
            }
        }

        // Single-node SCCs (self-loops) are handled separately in is_node_in_cycle,
        // so we only consider multi-node cycles here (size >= 2)
        contains_target
            && scc_size >= 2
            && scc_size >= config.min_depth
            && config.max_depth.is_none_or(|max| scc_size <= max)
    }

    /// Process successors for a node during iterative Tarjan traversal.
    ///
    /// Returns `true` if a child was pushed onto the work stack (need to recurse).
    fn process_node_successors(
        &mut self,
        node: NodeId,
        succ_idx: &mut usize,
        adjacency: &HashMap<NodeId, Vec<NodeId>>,
        work_stack: &mut Vec<(NodeId, usize, bool)>,
    ) -> bool {
        let Some(succs) = adjacency.get(&node) else {
            return false;
        };

        while *succ_idx < succs.len() {
            let w = succs[*succ_idx];
            *succ_idx += 1;

            if !self.indices.contains_key(&w) {
                work_stack.push((node, *succ_idx, false));
                work_stack.push((w, 0, false));
                return true;
            } else if self.on_stack.contains(&w) {
                let w_index = *self.indices.get(&w).unwrap();
                let node_lowlink = self.lowlinks.get_mut(&node).unwrap();
                *node_lowlink = (*node_lowlink).min(w_index);
            }
        }

        false
    }
}

/// Find if a node is in an SCC that meets the size criteria.
///
/// Uses a targeted version of Tarjan's algorithm that starts from the target node
/// and only explores nodes reachable from it. This is more efficient than running
/// full SCC detection when checking a single node.
fn find_scc_containing_node(
    target: NodeId,
    adjacency: &HashMap<NodeId, Vec<NodeId>>,
    config: &CircularConfig,
) -> bool {
    let mut state = TargetedTarjanState::new();
    let mut work_stack: Vec<(NodeId, usize, bool)> = vec![(target, 0, false)];

    while let Some((node, mut succ_idx, is_post)) = work_stack.pop() {
        if is_post {
            if state.handle_post_visit(node, target, &work_stack, config) {
                return true;
            }
            continue;
        }

        state.init_node(node);

        let pushed_child =
            state.process_node_successors(node, &mut succ_idx, adjacency, &mut work_stack);

        if !pushed_child {
            work_stack.push((node, succ_idx, true));
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;

    /// Helper to create a test graph with nodes and call edges.
    fn create_test_graph(
        nodes: &[(&str, NodeKind)],
        edges: &[(usize, usize)],
    ) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let mut node_ids = Vec::new();

        // Create nodes
        for (name, kind) in nodes {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(*kind, name_id, file_id).with_qualified_name(name_id);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        // Create call edges
        for (source_idx, target_idx) in edges {
            let source = node_ids[*source_idx];
            let target = node_ids[*target_idx];
            graph.edges_mut().add_edge(
                source,
                target,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file_id,
            );
        }

        (graph, node_ids)
    }

    /// Helper to create a graph with import edges.
    fn create_import_graph(
        nodes: &[(&str, NodeKind)],
        edges: &[(usize, usize)],
    ) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let mut node_ids = Vec::new();

        for (name, kind) in nodes {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(*kind, name_id, file_id).with_qualified_name(name_id);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        for (source_idx, target_idx) in edges {
            let source = node_ids[*source_idx];
            let target = node_ids[*target_idx];
            graph.edges_mut().add_edge(
                source,
                target,
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                file_id,
            );
        }

        (graph, node_ids)
    }

    #[test]
    fn test_empty_graph() {
        let graph = CodeGraph::new();
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_no_cycles_linear_chain() {
        // A -> B -> C (no cycles)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_c", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 2)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert!(cycles.is_empty(), "Linear chain should have no cycles");
    }

    #[test]
    fn test_simple_two_node_cycle() {
        // A -> B -> A (cycle)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 0)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find exactly one cycle");
        assert_eq!(cycles[0].len(), 2, "Cycle should have 2 nodes");
        assert!(
            cycles[0].contains(&"func_a".to_string()) && cycles[0].contains(&"func_b".to_string()),
            "Cycle should contain func_a and func_b"
        );
    }

    #[test]
    fn test_three_node_cycle() {
        // A -> B -> C -> A (cycle)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_c", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 2), (2, 0)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find exactly one cycle");
        assert_eq!(cycles[0].len(), 3, "Cycle should have 3 nodes");
    }

    #[test]
    fn test_self_loop_excluded_by_default() {
        // A -> A (self-loop)
        let nodes = [("func_a", NodeKind::Function)];
        let edges = [(0, 0)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert!(
            cycles.is_empty(),
            "Self-loops should be excluded by default"
        );
    }

    #[test]
    fn test_self_loop_included_when_enabled() {
        // A -> A (self-loop)
        let nodes = [("func_a", NodeKind::Function)];
        let edges = [(0, 0)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig {
            should_include_self_loops: true,
            min_depth: 1, // Must be 1 to include self-loops
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find self-loop when enabled");
        assert_eq!(cycles[0].len(), 1, "Self-loop has 1 node");
        assert_eq!(cycles[0][0], "func_a");
    }

    #[test]
    fn test_min_depth_filter() {
        // Two cycles: A -> B -> A (size 2) and X -> Y -> Z -> X (size 3)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_x", NodeKind::Function),
            ("func_y", NodeKind::Function),
            ("func_z", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 0), (2, 3), (3, 4), (4, 2)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig {
            min_depth: 3, // Only cycles with 3+ nodes
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should only find the 3-node cycle");
        assert_eq!(cycles[0].len(), 3);
    }

    #[test]
    fn test_max_depth_filter() {
        // Two cycles: A -> B -> A (size 2) and X -> Y -> Z -> W -> X (size 4)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_x", NodeKind::Function),
            ("func_y", NodeKind::Function),
            ("func_z", NodeKind::Function),
            ("func_w", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 0), (2, 3), (3, 4), (4, 5), (5, 2)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig {
            max_depth: Some(3), // Only cycles with 3 or fewer nodes
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should only find the 2-node cycle");
        assert_eq!(cycles[0].len(), 2);
    }

    #[test]
    fn test_max_results_limit() {
        // Create 10 independent 2-node cycles
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for i in 0..10 {
            nodes.push((format!("func_a{i}"), NodeKind::Function));
            nodes.push((format!("func_b{i}"), NodeKind::Function));
            edges.push((i * 2, i * 2 + 1));
            edges.push((i * 2 + 1, i * 2));
        }

        let nodes_ref: Vec<(&str, NodeKind)> =
            nodes.iter().map(|(s, k)| (s.as_str(), *k)).collect();
        let (graph, _) = create_test_graph(&nodes_ref, &edges);
        let config = CircularConfig {
            max_results: 3,
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 3, "Should respect max_results limit");
    }

    #[test]
    fn test_import_cycles() {
        // Module A imports Module B imports Module A
        let nodes = [
            ("module_a", NodeKind::Module),
            ("module_b", NodeKind::Module),
        ];
        let edges = [(0, 1), (1, 0)];
        let (graph, _) = create_import_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Imports, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find import cycle");
    }

    #[test]
    fn test_is_node_in_cycle_positive() {
        // A -> B -> A (A is in a cycle)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 0)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        assert!(
            is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config),
            "func_a should be in a cycle"
        );
        assert!(
            is_node_in_cycle(node_ids[1], CircularType::Calls, &graph, &config),
            "func_b should be in a cycle"
        );
    }

    #[test]
    fn test_is_node_in_cycle_negative() {
        // A -> B -> C (no cycles, C is not in a cycle)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_c", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 2)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        assert!(
            !is_node_in_cycle(node_ids[2], CircularType::Calls, &graph, &config),
            "func_c should not be in a cycle"
        );
    }

    #[test]
    fn test_is_node_in_cycle_invalid_node() {
        let graph = CodeGraph::new();
        let config = CircularConfig::default();
        let invalid_node = NodeId::new(999, 0);

        assert!(
            !is_node_in_cycle(invalid_node, CircularType::Calls, &graph, &config),
            "Invalid node should return false"
        );
    }

    #[test]
    fn test_multiple_independent_cycles() {
        // Cycle 1: A -> B -> A
        // Cycle 2: X -> Y -> Z -> X (separate component)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_x", NodeKind::Function),
            ("func_y", NodeKind::Function),
            ("func_z", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 0), (2, 3), (3, 4), (4, 2)];
        let (graph, _) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 2, "Should find both independent cycles");
    }

    #[test]
    fn test_cycle_with_branches() {
        // A -> B -> C -> A (cycle) with D -> B (branch into cycle)
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_c", NodeKind::Function),
            ("func_d", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 2), (2, 0), (3, 1)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);
        let config = CircularConfig::default();

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find the main cycle");

        // D is not in the cycle, it just points into it
        assert!(
            !is_node_in_cycle(node_ids[3], CircularType::Calls, &graph, &config),
            "func_d should not be in the cycle"
        );
    }

    #[test]
    fn test_is_node_in_cycle_min_depth_3_scc_semantics() {
        // Codex review scenario: T->A, T->B, B->A, A->T
        // This forms an SCC of size 3: {T, A, B}
        // With min_depth=3, both find_all_cycles_graph and is_node_in_cycle
        // should agree that T is in a cycle (SCC size = 3 >= min_depth)
        let nodes = [
            ("func_t", NodeKind::Function), // 0
            ("func_a", NodeKind::Function), // 1
            ("func_b", NodeKind::Function), // 2
        ];
        // T->A, T->B, B->A, A->T forms SCC {T, A, B}
        let edges = [(0, 1), (0, 2), (2, 1), (1, 0)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        let config = CircularConfig {
            min_depth: 3,
            ..Default::default()
        };

        // find_all_cycles_graph should find the SCC
        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert_eq!(cycles.len(), 1, "Should find the 3-node SCC");
        assert_eq!(cycles[0].len(), 3, "SCC should have 3 nodes");

        // is_node_in_cycle should agree for all nodes in the SCC
        assert!(
            is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config),
            "func_t should be in cycle (SCC size 3 >= min_depth 3)"
        );
        assert!(
            is_node_in_cycle(node_ids[1], CircularType::Calls, &graph, &config),
            "func_a should be in cycle (SCC size 3 >= min_depth 3)"
        );
        assert!(
            is_node_in_cycle(node_ids[2], CircularType::Calls, &graph, &config),
            "func_b should be in cycle (SCC size 3 >= min_depth 3)"
        );
    }

    #[test]
    fn test_self_loop_semantics_consistency() {
        // Self-loop: A -> A
        // With include_self_loops=true and min_depth>1, both functions should agree
        let nodes = [("func_a", NodeKind::Function)];
        let edges = [(0, 0)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        // Case 1: include_self_loops=true, min_depth=2
        // find_all_cycles_graph includes self-loops even when min_depth > 1
        // is_node_in_cycle should do the same
        let config = CircularConfig {
            should_include_self_loops: true,
            min_depth: 2,
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        let is_in_cycle = is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config);

        // Both should agree (self-loops are special-cased when include_self_loops is true)
        assert_eq!(
            !cycles.is_empty(),
            is_in_cycle,
            "find_all_cycles_graph and is_node_in_cycle should agree on self-loop inclusion"
        );

        // Case 2: include_self_loops=false
        let config_no_self = CircularConfig {
            should_include_self_loops: false,
            min_depth: 1,
            ..Default::default()
        };

        let cycles_no_self = find_all_cycles_graph(CircularType::Calls, &graph, &config_no_self);
        let is_in_cycle_no_self =
            is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config_no_self);

        assert!(cycles_no_self.is_empty(), "Self-loop should be excluded");
        assert!(
            !is_in_cycle_no_self,
            "is_node_in_cycle should also exclude self-loop"
        );
    }

    #[test]
    fn test_is_node_in_cycle_with_max_depth() {
        // 4-node cycle: A -> B -> C -> D -> A
        let nodes = [
            ("func_a", NodeKind::Function),
            ("func_b", NodeKind::Function),
            ("func_c", NodeKind::Function),
            ("func_d", NodeKind::Function),
        ];
        let edges = [(0, 1), (1, 2), (2, 3), (3, 0)];
        let (graph, node_ids) = create_test_graph(&nodes, &edges);

        // max_depth=3 should exclude this 4-node cycle
        let config = CircularConfig {
            max_depth: Some(3),
            ..Default::default()
        };

        let cycles = find_all_cycles_graph(CircularType::Calls, &graph, &config);
        assert!(cycles.is_empty(), "4-node cycle exceeds max_depth=3");

        assert!(
            !is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config),
            "is_node_in_cycle should also respect max_depth"
        );

        // max_depth=4 should include it
        let config_larger = CircularConfig {
            max_depth: Some(4),
            ..Default::default()
        };

        let cycles_larger = find_all_cycles_graph(CircularType::Calls, &graph, &config_larger);
        assert_eq!(cycles_larger.len(), 1, "4-node cycle fits max_depth=4");

        assert!(
            is_node_in_cycle(node_ids[0], CircularType::Calls, &graph, &config_larger),
            "is_node_in_cycle should find node in 4-node cycle with max_depth=4"
        );
    }
}
