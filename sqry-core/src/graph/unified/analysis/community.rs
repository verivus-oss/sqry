//! Coarsened, exact-integer Louvain community detection (optional power-user
//! primitive backing `sqry graph communities`).
//!
//! This finds dependency-density clusters that can cut across the directory
//! layout. It is deliberately NOT on the default `overview` report path (that is
//! [`super::subsystems`], a deterministic path/package aggregation). A cross-LLM
//! design review imposed four hardening must-fixes, all implemented here:
//!
//! 1. **Coarsened graph.** Louvain runs on FILE nodes (symbol edges aggregated
//!    to their files), never raw symbol nodes, so the modularity resolution
//!    limit operates at a meaningful granularity.
//! 2. **Exact-integer modularity.** Edge weights are integers and the delta-Q
//!    accept test is evaluated in `i128`, multiplied through by `(2m)^2 * den`
//!    (with the Reichardt-Bornholdt resolution as an exact rational `num/den`),
//!    so accept/reject and tie-breaks are exact and architecture-independent.
//!    The local-move phase is sequential (no rayon).
//! 3. **Deterministic STRUCTURAL gate, not wall-clock.** The algorithm runs iff
//!    `(file-node count, coarsened-edge count, estimated work units)` are under
//!    fixed integer thresholds. Over threshold it returns a deterministic
//!    "too large" verdict. Wall-clock never decides content (mirrors the
//!    structural budgets in [`super::condensation`], not `Instant::now()`).
//! 4. **Connected-components post-pass.** After convergence each community is
//!    split into the connected components of its induced subgraph, removing
//!    Louvain's one quality defect over Leiden.
//!
//! Communities are identified by their representative hub (reusing
//! [`rank_hubs`]), not a numeric id, so two reports diff sanely. The whole path
//! is integer, so the same snapshot + same gate verdict + same resolution +
//! same [`ALGORITHM_VERSION`] yields an identical assignment on any machine.
//!
//! Undirected note: the graph is symmetrised over `Calls` + `References`
//! (call direction is discarded here, unlike [`super::subsystems`]); intra-file
//! edges become a node's implicit self-loop and never affect a move decision, so
//! they are folded into node degree rather than materialized. Files with no
//! cross-file coupling are not part of the coarsened graph and so never form a
//! community.

use std::collections::BTreeMap;

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::NodeId;

use super::centrality::{HubMetric, HubOpts, KindMask, node_is_symbol, rank_hubs};

/// Algorithm identity for the determinism contract. Bump on any change to the
/// coarsening, accept test, aggregation, or post-pass.
pub const ALGORITHM_VERSION: u32 = 1;

/// Default structural-gate ceiling on coarsened file-node count.
pub const DEFAULT_MAX_FILE_NODES: u64 = 50_000;
/// Default structural-gate ceiling on coarsened (undirected) edge count.
pub const DEFAULT_MAX_COARSENED_EDGES: u64 = 500_000;
/// Default structural-gate ceiling on estimated work units.
pub const DEFAULT_MAX_WORK_UNITS: u128 = 50_000_000;
/// Multiplier turning `(V + E)` into an estimated work-unit budget.
const WORK_UNIT_FACTOR: u128 = 64;
/// Safety cap on local-move passes per level (prevents a pathological
/// non-convergence loop; real inputs converge in a handful of passes).
const MAX_PASSES_PER_LEVEL: usize = 100;

/// Reichardt-Bornholdt resolution parameter, held as an exact rational so the
/// accept test never touches `f64`. Higher gamma yields more, smaller
/// communities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Resolution {
    num: u64,
    den: u64,
}

impl Resolution {
    /// Gamma = 1 (the default).
    pub const ONE: Resolution = Resolution { num: 1, den: 1 };

    /// Builds `num/den`. Returns `None` if `den == 0`.
    #[must_use]
    pub const fn new(num: u64, den: u64) -> Option<Self> {
        if den == 0 {
            return None;
        }
        Some(Self { num, den })
    }

    /// The numerator.
    #[must_use]
    pub const fn num(self) -> u64 {
        self.num
    }

    /// The denominator (always non-zero).
    #[must_use]
    pub const fn den(self) -> u64 {
        self.den
    }
}

impl Default for Resolution {
    fn default() -> Self {
        Self::ONE
    }
}

/// Structural gate thresholds. Defaults are the fixed production ceilings; they
/// are exposed on [`CommunityOpts`] so tests and power users can tighten them,
/// but wall-clock is never a factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CommunityGate {
    /// Maximum coarsened file-node count.
    pub max_file_nodes: u64,
    /// Maximum coarsened (undirected) edge count.
    pub max_coarsened_edges: u64,
    /// Maximum estimated work units.
    pub max_work_units: u128,
}

impl Default for CommunityGate {
    fn default() -> Self {
        Self {
            max_file_nodes: DEFAULT_MAX_FILE_NODES,
            max_coarsened_edges: DEFAULT_MAX_COARSENED_EDGES,
            max_work_units: DEFAULT_MAX_WORK_UNITS,
        }
    }
}

/// Options for [`detect_communities`].
#[derive(Debug, Clone, Copy)]
pub struct CommunityOpts {
    /// Maximum communities to return. `0` means unbounded.
    pub top: usize,
    /// The resolution parameter.
    pub resolution: Resolution,
    /// The structural gate thresholds.
    pub gate: CommunityGate,
}

impl Default for CommunityOpts {
    fn default() -> Self {
        Self {
            top: 10,
            resolution: Resolution::ONE,
            gate: CommunityGate::default(),
        }
    }
}

/// One detected community (a cluster of coupled files).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Community {
    /// The community's top hub (its identity for diffing); `None` only when the
    /// member files hold no rankable symbol.
    pub representative: Option<NodeId>,
    /// Member files, sorted ascending by file index.
    pub files: Vec<FileId>,
    /// Number of member files.
    pub size: u32,
    /// Stub-free symbols across the member files.
    pub symbol_count: u32,
    /// Total coarsened edge weight internal to the community.
    pub internal_edges: u64,
}

/// The deterministic structural-gate verdict when the graph is too large.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GateVerdict {
    /// Coarsened file-node count.
    pub file_nodes: u64,
    /// Coarsened (undirected) edge count.
    pub coarsened_edges: u64,
    /// Estimated work units.
    pub work_units: u128,
    /// The thresholds that were exceeded.
    pub gate: CommunityGate,
}

/// Result of a community-detection run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum CommunityOutcome {
    /// A community partition (possibly empty).
    Partition(Vec<Community>),
    /// The coarsened graph exceeded the structural gate.
    TooLarge(GateVerdict),
}

/// Full report from [`detect_communities`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommunityReport {
    /// The outcome (partition or too-large verdict).
    pub outcome: CommunityOutcome,
    /// The resolution used.
    pub resolution: Resolution,
    /// The algorithm version that produced this report.
    pub algorithm_version: u32,
}

/// An undirected, integer-weighted graph with no materialized self-loops.
///
/// `degree[i]` is node `i`'s total incident weight *including* the implicit
/// self-loop from folded intra-community edges, so `sum(degree) == two_m` is
/// invariant across aggregation levels.
#[derive(Debug, Clone)]
struct WeightedGraph {
    /// `adj[i]` = neighbors `(node, weight)`, no self-loop entries.
    adj: Vec<Vec<(usize, u64)>>,
    /// Total incident weight per node (includes implicit self-loops).
    degree: Vec<u64>,
    /// `2m` = sum of all degrees; invariant across levels.
    two_m: u128,
}

impl WeightedGraph {
    fn node_count(&self) -> usize {
        self.adj.len()
    }

    /// Count of undirected edges (each `{i, j}` once).
    fn edge_count(&self) -> u64 {
        let total: usize = self.adj.iter().map(Vec::len).sum();
        (total / 2) as u64
    }
}

/// Coarsens the symbol graph in `snapshot` to a file-level undirected weighted
/// graph over `Calls` + `References` edges.
///
/// Returns the graph plus `compact_to_file[i]` = the [`FileId`] of coarsened
/// node `i` (nodes are the files that carry at least one cross-file coupling,
/// numbered ascending by file index for determinism).
fn coarsen_to_files(snapshot: &GraphSnapshot) -> (WeightedGraph, Vec<FileId>) {
    let node_slots = snapshot.nodes().slot_count();

    // node index -> owning file index (None for vacant slots).
    let mut file_of_node: Vec<Option<u32>> = vec![None; node_slots];
    for (id, entry) in snapshot.iter_nodes() {
        if let Some(slot) = file_of_node.get_mut(id.index() as usize) {
            *slot = Some(entry.file.index());
        }
    }

    // Accumulate undirected cross-file weights keyed (min_file, max_file), plus
    // intra-file weight per file (an implicit self-loop on that file node).
    let mut weights: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    let mut self_weights: BTreeMap<u32, u64> = BTreeMap::new();
    for edge in snapshot.edges().all_live_forward_edges() {
        if !matches!(edge.kind, EdgeKind::Calls { .. } | EdgeKind::References) {
            continue;
        }
        let (Some(Some(fu)), Some(Some(fv))) = (
            file_of_node.get(edge.source.index() as usize),
            file_of_node.get(edge.target.index() as usize),
        ) else {
            continue;
        };
        if fu == fv {
            // Intra-file edge: a self-loop on this file node. Its weight must be
            // folded into the file's degree (below), or the coarsened modularity
            // null model is computed on the wrong graph and can mis-partition.
            *self_weights.entry(*fu).or_insert(0) += 1;
            continue;
        }
        let key = if fu < fv { (*fu, *fv) } else { (*fv, *fu) };
        *weights.entry(key).or_insert(0) += 1;
    }

    // Compact the participating files to 0..F, ascending by file index.
    let mut file_to_compact: BTreeMap<u32, usize> = BTreeMap::new();
    for &(a, b) in weights.keys() {
        let next = file_to_compact.len();
        file_to_compact.entry(a).or_insert(next);
        let next = file_to_compact.len();
        file_to_compact.entry(b).or_insert(next);
    }
    let file_count = file_to_compact.len();
    let mut compact_to_file: Vec<FileId> = vec![FileId::new(0); file_count];
    for (&file_index, &compact) in &file_to_compact {
        compact_to_file[compact] = FileId::new(file_index);
    }

    let mut adj: Vec<Vec<(usize, u64)>> = vec![Vec::new(); file_count];
    let mut degree: Vec<u64> = vec![0u64; file_count];
    for (&(a, b), &w) in &weights {
        let ca = file_to_compact[&a];
        let cb = file_to_compact[&b];
        adj[ca].push((cb, w));
        adj[cb].push((ca, w));
        degree[ca] = degree[ca].saturating_add(w);
        degree[cb] = degree[cb].saturating_add(w);
    }
    // Fold each included file's intra-file self-loop into its degree. A self-loop
    // of weight s contributes 2s to the node's degree (both endpoints are the
    // file), matching aggregate()'s convention so sum(degree) == two_m stays
    // invariant across levels. Files with no cross-file coupling are not nodes
    // here, so their intra-file edges correctly do not enter this subgraph's 2m.
    for (&file_index, &compact) in &file_to_compact {
        if let Some(&s) = self_weights.get(&file_index) {
            degree[compact] = degree[compact].saturating_add(s.saturating_mul(2));
        }
    }

    // Sort each adjacency list by neighbor id for a fixed iteration order.
    for list in &mut adj {
        list.sort_unstable_by_key(|&(n, _)| n);
    }
    let two_m: u128 = degree.iter().map(|&d| u128::from(d)).sum();

    (WeightedGraph { adj, degree, two_m }, compact_to_file)
}

/// The exact-integer modularity move score for inserting node `i` (with degree
/// `ki`) into a community that has total degree `sum_tot_c` and to which `i` has
/// weight `kin`.
///
/// `score = kin * two_m * den - num * sum_tot_c * ki`, i.e. `delta-Q` multiplied
/// by the positive constant `(2m)^2 * den`, so `argmax score == argmax delta-Q`
/// and `score > 0 <=> delta-Q > 0`. Returns `None` on `i128` overflow (which the
/// structural gate makes unreachable in practice; treated defensively as
/// too-large).
fn move_score(
    kin: u128,
    two_m: u128,
    den: u128,
    num: u128,
    sum_tot_c: u128,
    ki: u128,
) -> Option<i128> {
    let pos = i128::try_from(kin.checked_mul(two_m)?.checked_mul(den)?).ok()?;
    let neg = i128::try_from(num.checked_mul(sum_tot_c)?.checked_mul(ki)?).ok()?;
    Some(pos - neg)
}

/// Relabels a raw community assignment to canonical labels `0..K`, ordered by
/// the minimum member node id (nodes scanned ascending).
fn canonicalize(raw: &[usize]) -> Vec<usize> {
    let mut remap: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out = vec![0usize; raw.len()];
    for (node, &label) in raw.iter().enumerate() {
        let next = remap.len();
        let canonical = *remap.entry(label).or_insert(next);
        out[node] = canonical;
    }
    out
}

/// One Louvain level: sequential local-move optimization to convergence.
///
/// Returns the canonical community assignment (`0..K`) for `graph`, or `None` on
/// arithmetic overflow.
fn local_move(graph: &WeightedGraph, res: Resolution) -> Option<Vec<usize>> {
    let n = graph.node_count();
    let two_m = graph.two_m;
    let num = u128::from(res.num());
    let den = u128::from(res.den());

    // Community label per node (start each node in its own community).
    let mut comm: Vec<usize> = (0..n).collect();
    // Total degree per community (indexed by community label, which is a node id).
    let mut sum_tot: Vec<u128> = graph.degree.iter().map(|&d| u128::from(d)).collect();

    for _pass in 0..MAX_PASSES_PER_LEVEL {
        let mut moved = false;
        for i in 0..n {
            let ci = comm[i];
            let ki = u128::from(graph.degree[i]);

            // Remove i from its community.
            sum_tot[ci] -= ki;

            // Weight from i to each neighboring community (BTreeMap => ascending
            // community id => deterministic tie-break toward the smallest id).
            let mut kin: BTreeMap<usize, u128> = BTreeMap::new();
            for &(j, w) in &graph.adj[i] {
                if j == i {
                    continue;
                }
                *kin.entry(comm[j]).or_insert(0) += u128::from(w);
            }

            // Baseline: i alone in its own singleton (community id `i`), score 0.
            let mut best_comm = i;
            let mut best_score: i128 = 0;
            for (&c, &weight_in) in &kin {
                let score = move_score(weight_in, two_m, den, num, sum_tot[c], ki)?;
                if score > best_score {
                    best_score = score;
                    best_comm = c;
                }
            }

            comm[i] = best_comm;
            sum_tot[best_comm] += ki;
            if best_comm != ci {
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }

    Some(canonicalize(&comm))
}

/// Aggregates `graph` by `comm` (with `k` communities) into the next-level
/// graph: one super-node per community, cross-community weights summed, and
/// intra-community weight folded into super-node degree (no self-loop edges).
fn aggregate(graph: &WeightedGraph, comm: &[usize], k: usize) -> WeightedGraph {
    let mut degree = vec![0u64; k];
    for (i, &c) in comm.iter().enumerate() {
        degree[c] = degree[c].saturating_add(graph.degree[i]);
    }

    // Sum cross-community weights once per undirected edge (guard with i < j).
    let mut weights: BTreeMap<(usize, usize), u64> = BTreeMap::new();
    for i in 0..graph.node_count() {
        for &(j, w) in &graph.adj[i] {
            if i >= j {
                continue;
            }
            let ci = comm[i];
            let cj = comm[j];
            if ci == cj {
                continue; // intra-community: folded into degree as a self-loop
            }
            let key = if ci < cj { (ci, cj) } else { (cj, ci) };
            *weights.entry(key).or_insert(0) += w;
        }
    }

    let mut adj: Vec<Vec<(usize, u64)>> = vec![Vec::new(); k];
    for (&(ca, cb), &w) in &weights {
        adj[ca].push((cb, w));
        adj[cb].push((ca, w));
    }
    for list in &mut adj {
        list.sort_unstable_by_key(|&(n, _)| n);
    }
    let two_m: u128 = degree.iter().map(|&d| u128::from(d)).sum();

    WeightedGraph { adj, degree, two_m }
}

/// Runs multi-level Louvain, returning the canonical community assignment for
/// each base node, or `None` on arithmetic overflow.
fn louvain(base: WeightedGraph, res: Resolution) -> Option<Vec<usize>> {
    let base_count = base.node_count();
    if base_count == 0 {
        return Some(Vec::new());
    }

    let mut current = base;
    // base node -> id in the current (aggregated) level.
    let mut base_to_level: Vec<usize> = (0..base_count).collect();

    loop {
        let comm = local_move(&current, res)?;
        let k = comm.iter().copied().max().map_or(0, |m| m + 1);

        // Compose: base node -> new-level community.
        for x in &mut base_to_level {
            *x = comm[*x];
        }

        if k >= current.node_count() {
            break; // no merging happened at this level
        }
        current = aggregate(&current, &comm, k);
    }

    Some(canonicalize(&base_to_level))
}

/// Splits any internally-disconnected community into its connected components.
///
/// For each community, computes the connected components of the induced
/// subgraph (edges of `graph` whose endpoints share the community) and gives
/// each component a fresh label. Labels are canonical (assigned by ascending
/// start node, i.e. by minimum member node id). O(V + E).
fn split_disconnected_communities(graph: &WeightedGraph, comm: &[usize]) -> Vec<usize> {
    let n = graph.node_count();
    let mut out = vec![usize::MAX; n];
    let mut next_label = 0usize;

    for start in 0..n {
        if out[start] != usize::MAX {
            continue;
        }
        let community = comm[start];
        let label = next_label;
        next_label += 1;

        // DFS over same-community, edge-connected nodes.
        let mut stack = vec![start];
        out[start] = label;
        while let Some(u) = stack.pop() {
            for &(v, _w) in &graph.adj[u] {
                if comm[v] == community && out[v] == usize::MAX {
                    out[v] = label;
                    stack.push(v);
                }
            }
        }
    }

    out
}

/// Computes the total internal (intra-community) coarsened edge weight per
/// community.
fn internal_weights(graph: &WeightedGraph, comm: &[usize], community_count: usize) -> Vec<u64> {
    let mut internal = vec![0u64; community_count];
    for i in 0..graph.node_count() {
        for &(j, w) in &graph.adj[i] {
            if i < j && comm[i] == comm[j] {
                internal[comm[i]] = internal[comm[i]].saturating_add(w);
            }
        }
    }
    internal
}

/// Detects dependency-density communities in `snapshot`.
///
/// Returns a [`CommunityReport`] whose outcome is either a ranked
/// [`CommunityOutcome::Partition`] (bounded by `opts.top`) or a deterministic
/// [`CommunityOutcome::TooLarge`] verdict when the coarsened graph exceeds the
/// structural gate. Communities rank by `(symbol_count desc, size desc, min
/// file index asc)`.
#[must_use]
pub fn detect_communities(snapshot: &GraphSnapshot, opts: &CommunityOpts) -> CommunityReport {
    let (graph, compact_to_file) = coarsen_to_files(snapshot);

    // --- Structural gate (integer only, no wall-clock). ---------------------
    let file_nodes = graph.node_count() as u64;
    let coarsened_edges = graph.edge_count();
    let work_units =
        (u128::from(file_nodes) + u128::from(coarsened_edges)).saturating_mul(WORK_UNIT_FACTOR);
    let gate = opts.gate;
    let over_gate = file_nodes > gate.max_file_nodes
        || coarsened_edges > gate.max_coarsened_edges
        || work_units > gate.max_work_units;
    if over_gate {
        return CommunityReport {
            outcome: CommunityOutcome::TooLarge(GateVerdict {
                file_nodes,
                coarsened_edges,
                work_units,
                gate,
            }),
            resolution: opts.resolution,
            algorithm_version: ALGORITHM_VERSION,
        };
    }

    // --- Louvain + connected-components post-pass. --------------------------
    // `louvain` only returns None on i128 overflow, which the gate makes
    // unreachable; treat it defensively as a too-large verdict.
    let Some(raw_comm) = louvain(graph.clone(), opts.resolution) else {
        return CommunityReport {
            outcome: CommunityOutcome::TooLarge(GateVerdict {
                file_nodes,
                coarsened_edges,
                work_units,
                gate,
            }),
            resolution: opts.resolution,
            algorithm_version: ALGORITHM_VERSION,
        };
    };
    let comm = split_disconnected_communities(&graph, &raw_comm);
    let community_count = comm.iter().copied().max().map_or(0, |m| m + 1);

    // --- Build per-community aggregates. ------------------------------------
    let internal = internal_weights(&graph, &comm, community_count);

    // file index -> community label (only for coarsened files).
    let mut file_to_community: BTreeMap<u32, usize> = BTreeMap::new();
    let mut members: Vec<Vec<FileId>> = vec![Vec::new(); community_count];
    for (compact, &file) in compact_to_file.iter().enumerate() {
        let label = comm[compact];
        file_to_community.insert(file.index(), label);
        members[label].push(file);
    }
    for list in &mut members {
        list.sort_unstable_by_key(|f| f.index());
    }

    // Symbol counts per community over stub-free symbols.
    let mut symbol_count = vec![0u32; community_count];
    for (_id, entry) in snapshot.iter_nodes() {
        if !node_is_symbol(snapshot, entry) {
            continue;
        }
        if let Some(&label) = file_to_community.get(&entry.file.index()) {
            symbol_count[label] = symbol_count[label].saturating_add(1);
        }
    }

    // Representatives: first ranked hub whose file is in each community.
    let hub_opts = HubOpts {
        top: 0,
        by: HubMetric::FanIn,
        kinds: KindMask::default(),
    };
    let mut representative: Vec<Option<NodeId>> = vec![None; community_count];
    for hub in rank_hubs(snapshot, &hub_opts) {
        if let Some(entry) = snapshot.get_node(hub.node)
            && let Some(&label) = file_to_community.get(&entry.file.index())
            && representative[label].is_none()
        {
            representative[label] = Some(hub.node);
        }
    }
    // Fallback: lowest-index stub-free symbol in the community.
    for (id, entry) in snapshot.iter_nodes() {
        if !node_is_symbol(snapshot, entry) {
            continue;
        }
        if let Some(&label) = file_to_community.get(&entry.file.index())
            && representative[label].is_none()
        {
            representative[label] = Some(id);
        }
    }

    let mut communities: Vec<Community> = (0..community_count)
        .map(|label| {
            let files = std::mem::take(&mut members[label]);
            let size = u32::try_from(files.len()).unwrap_or(u32::MAX);
            Community {
                representative: representative[label],
                files,
                size,
                symbol_count: symbol_count[label],
                internal_edges: internal[label],
            }
        })
        .collect();
    communities.sort_by(|a, b| {
        b.symbol_count
            .cmp(&a.symbol_count) // symbol count descending
            .then_with(|| b.size.cmp(&a.size)) // file count descending
            .then_with(|| {
                // min file index ascending (files are already sorted ascending)
                let a_min = a.files.first().map_or(u32::MAX, |f| f.index());
                let b_min = b.files.first().map_or(u32::MAX, |f| f.index());
                a_min.cmp(&b_min)
            })
    });
    if opts.top != 0 && communities.len() > opts.top {
        communities.truncate(opts.top);
    }

    CommunityReport {
        outcome: CommunityOutcome::Partition(communities),
        resolution: opts.resolution,
        algorithm_version: ALGORITHM_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    /// Registers a file that holds exactly one function, returning both ids.
    fn file_with_fn(graph: &mut CodeGraph, path: &str, name: &str) -> (FileId, NodeId) {
        let file = graph
            .files_mut()
            .register_with_language(&PathBuf::from(path), Some(Language::Rust))
            .expect("register file");
        let sid = graph.strings_mut().intern(name).expect("intern name");
        let entry = NodeEntry::new(NodeKind::Function, sid, file)
            .with_definition(true)
            .with_byte_range(0, 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph
            .indices_mut()
            .add(id, NodeKind::Function, sid, Some(sid), file);
        (file, id)
    }

    fn edge(graph: &CodeGraph, from: NodeId, to: NodeId, file: FileId) {
        graph.edges().add_edge(from, to, calls(), file);
    }

    /// Builds three dense file clusters (3 files each, one function per file)
    /// lightly bridged. Returns the graph and the planted cluster file sets.
    fn build_three_clusters() -> (CodeGraph, Vec<BTreeSet<u32>>) {
        let mut graph = CodeGraph::new();
        let mut cluster_files: Vec<BTreeSet<u32>> = Vec::new();
        let mut cluster_reps: Vec<NodeId> = Vec::new();

        for c in 0..3 {
            let mut files = Vec::new();
            let mut fns = Vec::new();
            for i in 0..3 {
                let (f, n) = file_with_fn(
                    &mut graph,
                    &format!("cluster{c}/f{i}.rs"),
                    &format!("c{c}f{i}"),
                );
                files.push(f);
                fns.push(n);
            }
            // Dense intra-cluster triangle, both directions (undirected weight 2).
            for &(a, b) in &[(0usize, 1usize), (1, 2), (0, 2)] {
                edge(&graph, fns[a], fns[b], files[a]);
                edge(&graph, fns[b], fns[a], files[b]);
            }
            cluster_files.push(files.iter().map(|f| f.index()).collect());
            cluster_reps.push(fns[0]);
        }

        // Sparse inter-cluster bridges (single edge each).
        edge(&graph, cluster_reps[0], cluster_reps[1], FileId::new(0));
        edge(&graph, cluster_reps[1], cluster_reps[2], FileId::new(0));
        edge(&graph, cluster_reps[0], cluster_reps[2], FileId::new(0));

        (graph, cluster_files)
    }

    fn partition(report: &CommunityReport) -> &[Community] {
        match &report.outcome {
            CommunityOutcome::Partition(v) => v,
            CommunityOutcome::TooLarge(_) => panic!("expected a partition, got TooLarge"),
        }
    }

    #[test]
    fn three_dense_clusters_resolve_to_three_communities() {
        let (graph, planted) = build_three_clusters();
        let snapshot = graph.snapshot();
        let report = detect_communities(&snapshot, &CommunityOpts::default());
        let communities = partition(&report);
        assert_eq!(communities.len(), 3, "one community per planted cluster");

        let planted_sets: BTreeSet<BTreeSet<u32>> = planted.into_iter().collect();
        let found_sets: BTreeSet<BTreeSet<u32>> = communities
            .iter()
            .map(|c| c.files.iter().map(|f| f.index()).collect())
            .collect();
        assert_eq!(
            found_sets, planted_sets,
            "communities match planted clusters"
        );

        // Each community is identified by a representative hub, not a numeric id.
        for c in communities {
            assert!(
                c.representative.is_some(),
                "community has a representative hub"
            );
            assert_eq!(c.size, 3);
            assert_eq!(c.symbol_count, 3);
        }
    }

    #[test]
    fn partition_is_deterministic_across_runs() {
        let (graph, _) = build_three_clusters();
        let snapshot = graph.snapshot();
        let first = detect_communities(&snapshot, &CommunityOpts::default());
        for _ in 0..10 {
            let again = detect_communities(&snapshot, &CommunityOpts::default());
            assert_eq!(
                again, first,
                "exact-integer path is byte-stable across runs"
            );
        }
    }

    #[test]
    fn connected_components_post_pass_splits_disconnected_community() {
        // Two disconnected edge-pairs {0-1} and {2-3} forced into one community.
        let graph = WeightedGraph {
            adj: vec![vec![(1, 1)], vec![(0, 1)], vec![(3, 1)], vec![(2, 1)]],
            degree: vec![1, 1, 1, 1],
            two_m: 4,
        };
        let split = split_disconnected_communities(&graph, &[0, 0, 0, 0]);
        assert_eq!(split[0], split[1], "{{0,1}} stay together");
        assert_eq!(split[2], split[3], "{{2,3}} stay together");
        assert_ne!(
            split[0], split[2],
            "the disconnected halves are split apart"
        );

        // A connected community is left intact (single component).
        let connected = WeightedGraph {
            adj: vec![vec![(1, 1)], vec![(0, 1), (2, 1)], vec![(1, 1)]],
            degree: vec![1, 2, 1],
            two_m: 4,
        };
        let intact = split_disconnected_communities(&connected, &[0, 0, 0]);
        assert_eq!(intact, vec![0, 0, 0], "a connected community is not split");
    }

    #[test]
    fn higher_resolution_yields_more_communities() {
        let (graph, _) = build_three_clusters();
        let snapshot = graph.snapshot();

        let low = detect_communities(
            &snapshot,
            &CommunityOpts {
                top: 0,
                resolution: Resolution::ONE,
                gate: CommunityGate::default(),
            },
        );
        let high = detect_communities(
            &snapshot,
            &CommunityOpts {
                top: 0,
                resolution: Resolution::new(1000, 1).unwrap(),
                gate: CommunityGate::default(),
            },
        );
        let low_n = partition(&low).len();
        let high_n = partition(&high).len();
        assert!(
            high_n >= low_n,
            "higher gamma yields at least as many (smaller) communities: {high_n} >= {low_n}"
        );
        assert!(high_n > low_n, "gamma 1000 fragments the clusters further");
    }

    #[test]
    fn structural_gate_returns_too_large_deterministically() {
        let (graph, _) = build_three_clusters();
        let snapshot = graph.snapshot();
        // 9 coarsened file nodes; a ceiling of 1 must trip the gate.
        let opts = CommunityOpts {
            top: 0,
            resolution: Resolution::ONE,
            gate: CommunityGate {
                max_file_nodes: 1,
                max_coarsened_edges: DEFAULT_MAX_COARSENED_EDGES,
                max_work_units: DEFAULT_MAX_WORK_UNITS,
            },
        };
        let report = detect_communities(&snapshot, &opts);
        match report.outcome {
            CommunityOutcome::TooLarge(verdict) => {
                assert_eq!(verdict.file_nodes, 9);
                assert!(verdict.file_nodes > verdict.gate.max_file_nodes);
            }
            CommunityOutcome::Partition(_) => panic!("expected TooLarge over the gate"),
        }
        // Deterministic: the verdict never depends on wall-clock.
        assert_eq!(detect_communities(&snapshot, &opts), report);
    }

    #[test]
    fn empty_graph_yields_empty_partition() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let report = detect_communities(&snapshot, &CommunityOpts::default());
        assert!(partition(&report).is_empty());
    }

    #[test]
    fn move_score_is_exact_integer() {
        // kin=2, two_m=10, gamma=1 (den=1,num=1), sum_tot=4, ki=3
        // score = 2*10*1 - 1*4*3 = 20 - 12 = 8
        assert_eq!(move_score(2, 10, 1, 1, 4, 3), Some(8));
        // Fractional gamma 1/2: score = 2*10*2 - 1*4*3 = 40 - 12 = 28
        assert_eq!(move_score(2, 10, 2, 1, 4, 3), Some(28));
    }

    #[test]
    fn intra_file_edges_fold_into_file_node_degree() {
        // Two functions in a.rs with an intra-file call (a self-loop on file A),
        // plus a cross-file call A->B so both files are nodes. The self-loop must
        // add 2 to A's coarsened degree and to two_m. Regression guard for the
        // bug where intra-file edges were dropped instead of folded into degree.
        let mut graph = CodeGraph::new();
        let file_a = graph
            .files_mut()
            .register_with_language(&PathBuf::from("a.rs"), Some(Language::Rust))
            .expect("register a");
        let file_b = graph
            .files_mut()
            .register_with_language(&PathBuf::from("b.rs"), Some(Language::Rust))
            .expect("register b");
        let mk = |graph: &mut CodeGraph, name: &str, file: FileId| {
            let sid = graph.strings_mut().intern(name).expect("intern");
            let entry = NodeEntry::new(NodeKind::Function, sid, file).with_definition(true);
            graph.nodes_mut().alloc(entry).expect("alloc")
        };
        let a1 = mk(&mut graph, "a1", file_a);
        let a2 = mk(&mut graph, "a2", file_a);
        let b1 = mk(&mut graph, "b1", file_b);
        graph.edges().add_edge(a1, a2, calls(), file_a); // intra-file self-loop
        graph.edges().add_edge(a1, b1, calls(), file_a); // cross-file A->B
        let snapshot = graph.snapshot();
        let (wg, files) = coarsen_to_files(&snapshot);
        assert_eq!(wg.node_count(), 2, "both files participate");
        let idx_a = files.iter().position(|f| *f == file_a).expect("A present");
        let idx_b = files.iter().position(|f| *f == file_b).expect("B present");
        assert_eq!(wg.degree[idx_a], 3, "A degree = 1 cross + 2 self-loop");
        assert_eq!(wg.degree[idx_b], 1, "B degree = 1 cross");
        assert_eq!(wg.two_m, 4, "self-loop must count in 2m");
    }
}
