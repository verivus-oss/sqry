//! Shared seeded, depth-limited subgraph builder.
//!
//! This is the reusable traversal primitive behind seeded exports (today the
//! Archify architecture exporter). It is deliberately **entry-point-agnostic**:
//! it takes a set of concrete [`NodeId`] seeds and walks the graph. Resolving
//! which nodes are seeds (by symbol name, file, or entry-point heuristic) is a
//! higher-layer concern and stays out of `sqry-core` (entry-point detection
//! lives in `sqry-db`). The builder never invents seeds.
//!
//! It is also **format-agnostic**: the caller supplies an edge classifier that
//! decides, per [`EdgeKind`], whether an edge is retained (and under what
//! caller-defined class) and whether the traversal follows it. This keeps each
//! export format's edge-retention policy fully independent. In particular the
//! Archify path classifies (and retains) semantically rich cross-service edges
//! (`HttpRequest`, `DbQuery`, `MessageQueue`, ...) that the raw dot / d2 /
//! mermaid / json exporters intentionally drop, and it does so through its own
//! classifier so those existing formats are provably unchanged.
//!
//! # Determinism
//!
//! Every ordering decision is explicit and stable so identical input yields
//! byte-identical output, including across parallel (rayon) graph rebuilds
//! where the underlying arena/edge iteration order can differ:
//!
//! - Seeds are sorted by `NodeId` index and de-duplicated before the walk.
//! - Outgoing edges of each node are sorted by
//!   `(target index, edge-kind discriminant, sequence)` before they are
//!   recorded or enqueued.
//! - BFS uses a per-node visited set keyed on `NodeId` (never on qualified
//!   name), so same-named nodes in unrelated files never fuse.
//!
//! The resulting [`SeededSubgraph::nodes`] is in stable BFS discovery order and
//! [`SeededSubgraph::edges`] is in stable emission order.

use std::collections::{HashSet, VecDeque};

use crate::graph::node::Language;
use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;

/// Default BFS depth (hops from the seed) for a seeded subgraph.
pub const DEFAULT_MAX_DEPTH: usize = 2;

/// Hard cap on BFS depth. Anything deeper is already too dense to read.
pub const MAX_DEPTH_CAP: usize = 5;

/// Default cap on the number of raw nodes visited during BFS.
pub const DEFAULT_MAX_RESULTS: usize = 1000;

/// Configuration for a seeded subgraph walk.
#[derive(Debug, Clone)]
pub struct SeededSubgraphConfig {
    /// Maximum BFS hops from the seed set. Clamped to [`MAX_DEPTH_CAP`].
    pub max_depth: usize,
    /// Maximum number of raw nodes to visit before the walk stops.
    pub max_results: usize,
    /// Optional language allow-list. Empty means "any language". A node whose
    /// file language is not in the list is skipped (not visited, not expanded).
    pub languages: Vec<Language>,
}

impl Default for SeededSubgraphConfig {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_results: DEFAULT_MAX_RESULTS,
            languages: Vec::new(),
        }
    }
}

impl SeededSubgraphConfig {
    /// Clamp `max_depth` to the hard cap and guarantee a sane `max_results`.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.max_depth = self.max_depth.min(MAX_DEPTH_CAP);
        if self.max_results == 0 {
            self.max_results = DEFAULT_MAX_RESULTS;
        }
        self
    }
}

/// Decision returned by a caller-supplied edge classifier.
#[derive(Debug, Clone)]
pub struct EdgeRetention<C> {
    /// Caller-defined classification of the retained edge (e.g. a connection
    /// category for the Archify exporter).
    pub class: C,
    /// Whether the BFS should follow this edge to expand the frontier.
    pub traverse: bool,
}

/// A retained edge between two visited nodes, carrying the caller's class.
#[derive(Debug, Clone)]
pub struct RetainedEdge<C> {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// Caller-defined classification of this edge.
    pub class: C,
}

/// The result of a seeded subgraph walk.
#[derive(Debug, Clone)]
pub struct SeededSubgraph<C> {
    /// Visited nodes in stable BFS discovery order.
    pub nodes: Vec<NodeId>,
    /// Retained edges (both endpoints visited) in stable emission order.
    pub edges: Vec<RetainedEdge<C>>,
    /// `true` if the walk stopped early because `max_results` was reached.
    pub truncated: bool,
}

/// Build a seeded, depth-limited subgraph.
///
/// `classify_edge` is invoked for every outgoing edge of a visited node. It
/// returns `Some(EdgeRetention { class, traverse })` to retain the edge under
/// `class` (and, if `traverse`, to expand into the target), or `None` to drop
/// the edge entirely. The classifier is the sole edge-retention policy: this
/// function embeds no format-specific edge knowledge.
///
/// A retained edge is only recorded once both endpoints are visited nodes, so
/// the returned edge list never dangles.
pub fn build_seeded_subgraph<C, F>(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    config: &SeededSubgraphConfig,
    classify_edge: F,
) -> SeededSubgraph<C>
where
    F: Fn(&EdgeKind) -> Option<EdgeRetention<C>>,
    C: Clone,
{
    let language_ok = |node: NodeId| -> bool {
        if config.languages.is_empty() {
            return true;
        }
        snapshot
            .get_node(node)
            .and_then(|entry| snapshot.files().language_for_file(entry.file))
            .is_some_and(|lang| config.languages.contains(&lang))
    };

    // Deterministic seed order: sort by index, dedup, drop unified losers and
    // language-filtered seeds.
    let mut ordered_seeds: Vec<NodeId> = seeds
        .iter()
        .copied()
        .filter(|&id| {
            snapshot.get_node(id).is_some_and(|e| !e.is_unified_loser()) && language_ok(id)
        })
        .collect();
    ordered_seeds.sort_by_key(|id| id.index());
    ordered_seeds.dedup();

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut discovery: Vec<NodeId> = Vec::new();
    let mut pending_edges: Vec<(NodeId, NodeId, C)> = Vec::new();
    let mut queue: VecDeque<(NodeId, usize)> =
        ordered_seeds.iter().map(|&id| (id, 0usize)).collect();
    let mut truncated = false;

    while let Some((current, depth)) = queue.pop_front() {
        if depth > config.max_depth || visited.contains(&current) {
            continue;
        }
        if !language_ok(current) {
            continue;
        }
        if discovery.len() >= config.max_results {
            truncated = true;
            break;
        }
        visited.insert(current);
        discovery.push(current);

        // Collect + deterministically sort this node's outgoing retained edges.
        let mut outgoing: Vec<(NodeId, EdgeKind, u64, C, bool)> = Vec::new();
        for edge in snapshot.edges().edges_from(current) {
            let Some(retention) = classify_edge(&edge.kind) else {
                continue;
            };
            // Skip edges into losers or language-filtered targets up front so
            // the frontier stays clean.
            if snapshot
                .get_node(edge.target)
                .is_none_or(|e| e.is_unified_loser())
                || !language_ok(edge.target)
            {
                continue;
            }
            outgoing.push((
                edge.target,
                edge.kind.clone(),
                edge.seq,
                retention.class,
                retention.traverse,
            ));
        }
        outgoing.sort_by(|a, b| {
            a.0.index()
                .cmp(&b.0.index())
                .then_with(|| edge_kind_rank(&a.1).cmp(&edge_kind_rank(&b.1)))
                .then_with(|| a.2.cmp(&b.2))
        });

        for (target, _kind, _seq, class, traverse) in outgoing {
            pending_edges.push((current, target, class));
            if traverse && depth < config.max_depth && !visited.contains(&target) {
                queue.push_back((target, depth + 1));
            }
        }
    }

    // Retain only edges whose endpoints were both visited, preserving emission
    // order. `pending_edges` is already in per-node sorted order.
    let edges: Vec<RetainedEdge<C>> = pending_edges
        .into_iter()
        .filter(|(from, to, _)| visited.contains(from) && visited.contains(to))
        .map(|(from, to, class)| RetainedEdge { from, to, class })
        .collect();

    SeededSubgraph {
        nodes: discovery,
        edges,
        truncated,
    }
}

/// Stable, discriminant-based rank for an `EdgeKind`.
///
/// `std::mem::discriminant` is not `Ord`, so we derive a stable integer from a
/// fixed match. Two edges of the same variant compare equal here and fall
/// through to the sequence tie-break, which keeps ordering total and stable
/// without depending on metadata payloads (arg counts, async flags, string
/// ids) that can differ harmlessly.
fn edge_kind_rank(kind: &EdgeKind) -> u16 {
    // Grouped by architectural weight so the dominant-kind tie-break in the
    // Archify exporter is stable and intention-revealing.
    match kind {
        EdgeKind::HttpRequest { .. } => 0,
        EdgeKind::GrpcCall { .. } => 1,
        EdgeKind::DbQuery { .. } => 2,
        EdgeKind::TableRead { .. } => 3,
        EdgeKind::TableWrite { .. } => 4,
        EdgeKind::TriggeredBy { .. } => 5,
        EdgeKind::MessageQueue { .. } => 6,
        EdgeKind::WebSocket { .. } => 7,
        EdgeKind::GraphQLOperation { .. } => 8,
        EdgeKind::FfiCall { .. } => 9,
        EdgeKind::WebAssemblyCall => 10,
        EdgeKind::Calls { .. } => 11,
        EdgeKind::Imports { .. } => 12,
        EdgeKind::Exports { .. } => 13,
        EdgeKind::TypeOf { .. } => 14,
        EdgeKind::References => 15,
        EdgeKind::Inherits => 16,
        EdgeKind::Implements => 17,
        EdgeKind::Defines => 18,
        EdgeKind::Contains => 19,
        // Any remaining variant sorts last but stays deterministic via the
        // discriminant hash fallback so the ordering is still total.
        other => 20u16.wrapping_add(discriminant_index(other)),
    }
}

/// Deterministic fallback index for edge variants not explicitly ranked.
///
/// Only the long tail of JVM/Rust-specific edges reaches this arm, and the
/// Archify classifier never retains them, so it never affects real output; it
/// exists purely to keep [`edge_kind_rank`] total. The debug label is stable
/// within a build and identical across rebuilds of the same source, so folding
/// it keeps ordering deterministic.
fn discriminant_index(kind: &EdgeKind) -> u16 {
    let label = format!("{kind:?}");
    let mut acc: u16 = 0;
    for b in label.bytes().take(8) {
        acc = acc.wrapping_mul(31).wrapping_add(u16::from(b));
    }
    acc
}
