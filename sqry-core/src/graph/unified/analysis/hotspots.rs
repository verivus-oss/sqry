//! Fan-out complexity hotspot ranking (the `hotspots` primitive).
//!
//! Ranks the most structurally complex functions/methods in a graph by a
//! deterministic fan-out complexity score: the count of outgoing `Calls`
//! edges plus the deepest call-chain reachable from the node (capped to keep
//! the walk bounded on cyclic graphs). A high score marks an orchestration
//! hotspot: a function that both calls broadly and drives a deep call tree,
//! which is where a newcomer's reading effort concentrates.
//!
//! # Reuse, not reinvention
//!
//! This is the single shared implementation behind both the CLI
//! `sqry overview` hotspots section and the MCP `generate_overview` tool.
//! Lifting it here (beside [`super::centrality::rank_hubs`] and
//! [`super::subsystems::aggregate_subsystems`], which the overview report also
//! composes) keeps the two surfaces byte-identical on the same snapshot and
//! removes the duplicate complexity walk the CLI `complexity` command grew.
//!
//! The whole path is integer and float-free, so the ranking is byte-stable
//! across runs and architectures: ties break deterministically by node index
//! ascending.

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::string::StringId;

/// Recursion cap for the call-chain-depth walk. Bounds the traversal on cyclic
/// or deep call graphs so the score stays computable in constant stack.
const MAX_CHAIN_DEPTH: usize = 20;

/// One ranked complexity hotspot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotspotRank {
    /// The ranked node.
    pub node: NodeId,
    /// Its interned simple name.
    pub name: StringId,
    /// Its node kind (always `Function` or `Method`).
    pub kind: NodeKind,
    /// Fan-out complexity: outgoing `Calls` count + deepest call-chain depth.
    pub score: usize,
}

/// Longest `Calls` chain (in edges) reachable from each node, cycle-broken and
/// capped at [`MAX_CHAIN_DEPTH`], indexed by node arena slot.
///
/// Computed once over a prebuilt `Calls` adjacency (`calls_out[slot]` = the
/// callee slots) with an ITERATIVE, memoized post-order DFS. This fixes two
/// problems in the original per-node recursive walk: it enumerated every path
/// and exploded exponentially on cyclic call graphs (a dense mutual recursion
/// hung), and it called `edges_from` per node, which is O(|delta|) on a
/// delta-backed (first-run) snapshot and made ranking `Theta(V*|delta|)`. The
/// adjacency is built once from a single `all_live_forward_edges` sweep (the
/// pattern the other primitives use) and the DP visits each node once, in
/// constant Rust stack. A back-edge (callee still on the stack) is a cycle and
/// does not extend a chain, so the longest simple chain is measured.
/// Array-indexed, so it never influences output order.
///
/// Takes `calls_out` by `&mut` and sorts each node's callee list in place
/// FIRST. `all_live_forward_edges` emits delta Adds in unordered `HashMap`
/// order, and on a cyclic graph the back-edge decision (which callee is still
/// on the stack when reached) depends on traversal order, so an unsorted
/// adjacency would make the reach vector, and therefore every hotspot score,
/// vary run-to-run. Canonicalizing here makes the DP order-independent by
/// construction, and the caller reuses the now-sorted adjacency for scoring
/// (its `len`/`max` uses are order-invariant anyway). Sorting in place avoids
/// cloning the whole edge set.
///
/// Cost: the sweep and the DP are each O(V+E); the per-node canonical sort adds
/// O(E log D) where D is the maximum out-degree (worst case O(E log E) on a
/// single-source star), so the primitive is O(V + E log D), effectively linear
/// for call graphs with bounded fan-out. This is the same comparison-sort idiom
/// the analysis layer already uses to canonicalize adjacency: the shared
/// `CsrAdjacency::build_from_snapshot` sorts its entire merged edge set by
/// `(src, tgt)` (see `analysis/csr.rs`), and `community.rs` sorts each level's
/// adjacency. A per-node sort here is never worse than that shared builder.
fn compute_call_chain_reach(calls_out: &mut [Vec<u32>]) -> Vec<usize> {
    for list in calls_out.iter_mut() {
        list.sort_unstable();
    }
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let n = calls_out.len();
    let mut reach = vec![0usize; n];
    let mut computed = vec![false; n];
    let mut on_stack = vec![false; n];
    for start in 0..n {
        if computed[start] {
            continue;
        }
        let mut stack = vec![Step::Enter(start)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(node) => {
                    if computed[node] {
                        continue;
                    }
                    on_stack[node] = true;
                    stack.push(Step::Exit(node));
                    for &callee in &calls_out[node] {
                        let c = callee as usize;
                        if c < n && !computed[c] && !on_stack[c] {
                            stack.push(Step::Enter(c));
                        }
                    }
                }
                Step::Exit(node) => {
                    let mut best = 0usize;
                    for &callee in &calls_out[node] {
                        let c = callee as usize;
                        // Forward/cross callees are fully computed by post-order;
                        // a back-edge callee is still on the stack (cycle) and is
                        // skipped so it does not extend the chain.
                        if c < n && computed[c] {
                            best = best.max(1 + reach[c]);
                        }
                    }
                    on_stack[node] = false;
                    reach[node] = best.min(MAX_CHAIN_DEPTH);
                    computed[node] = true;
                }
            }
        }
    }
    reach
}

/// Fan-out complexity score for one node: outgoing `Calls` count plus the
/// deepest reachable call chain. `callees` are the node's `Calls` target slots;
/// `reach` is the precomputed per-slot chain depth.
fn node_complexity(callees: &[u32], reach: &[usize]) -> usize {
    let mut max_depth = 0usize;
    for &callee in callees {
        // `1 + reach[callee]` is the deepest chain through this callee; a callee
        // slot outside `reach` counts as a single-edge leaf.
        let depth = reach
            .get(callee as usize)
            .map_or(1, |&r| (1 + r).min(MAX_CHAIN_DEPTH));
        max_depth = max_depth.max(depth);
    }
    callees.len() + max_depth
}

/// Ranks the most fan-out-complex functions/methods in `snapshot`.
///
/// Considers only `Function` / `Method` nodes, skips unified losers, and keeps
/// only nodes with a non-zero score. Results are sorted by score descending,
/// tie-broken by node index ascending, then truncated to `top` (`0` means
/// unbounded). Deterministic and byte-stable across runs.
#[must_use]
pub fn rank_hotspots(snapshot: &GraphSnapshot, top: usize) -> Vec<HotspotRank> {
    // Build the `Calls` forward adjacency once (a single O(V+E) edge sweep, the
    // same pattern the other primitives use) so neither the depth DP nor scoring
    // calls the per-node, delta-scanning `edges_from`. Then compute each node's
    // deepest call chain once, cycle-safe.
    let slots = snapshot.nodes().slot_count();
    let mut calls_out: Vec<Vec<u32>> = vec![Vec::new(); slots];
    for edge_ref in snapshot.edges().all_live_forward_edges() {
        if !matches!(edge_ref.kind, EdgeKind::Calls { .. }) {
            continue;
        }
        if let Some(list) = calls_out.get_mut(edge_ref.source.index() as usize) {
            list.push(edge_ref.target.index());
        }
    }
    // `compute_call_chain_reach` canonicalizes callee order in place (sorts each
    // list) before the cycle-breaking DFS, so `reach` and every downstream score
    // are deterministic regardless of the delta's HashMap emission order.
    let reach = compute_call_chain_reach(&mut calls_out);
    let mut ranked: Vec<HotspotRank> = Vec::new();
    for (id, entry) in snapshot.iter_nodes() {
        // Skip unification losers: their edges were rewritten into the winner
        // (Phase 4c-prime), so they carry no genuine fan-out.
        if entry.is_unified_loser() {
            continue;
        }
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        let callees = calls_out
            .get(id.index() as usize)
            .map_or(&[][..], Vec::as_slice);
        let score = node_complexity(callees, &reach);
        if score == 0 {
            continue;
        }
        ranked.push(HotspotRank {
            node: id,
            name: entry.name,
            kind: entry.kind,
            score,
        });
    }

    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score) // score descending
            .then_with(|| a.node.index().cmp(&b.node.index())) // then node index ascending
    });

    if top != 0 && ranked.len() > top {
        ranked.truncate(top);
    }
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::PathBuf;

    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    fn register_file(graph: &mut CodeGraph, path: &str) -> FileId {
        graph
            .files_mut()
            .register_with_language(&PathBuf::from(path), Some(Language::Rust))
            .expect("register file")
    }

    fn add_fn(graph: &mut CodeGraph, name: &str, file: FileId) -> NodeId {
        let sid = graph.strings_mut().intern(name).expect("intern name");
        let entry = NodeEntry::new(NodeKind::Function, sid, file)
            .with_definition(true)
            .with_byte_range(0, 1);
        let id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph
            .indices_mut()
            .add(id, NodeKind::Function, sid, Some(sid), file);
        id
    }

    fn edge(graph: &CodeGraph, from: NodeId, to: NodeId, file: FileId) {
        graph.edges().add_edge(from, to, calls(), file);
    }

    #[test]
    fn ranks_by_fan_out_complexity_descending() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let driver = add_fn(&mut graph, "driver", f);
        let a = add_fn(&mut graph, "a", f);
        let b = add_fn(&mut graph, "b", f);
        let leaf = add_fn(&mut graph, "leaf", f);

        // driver -> a -> b -> leaf : 1 call + depth 3 = score 4.
        edge(&graph, driver, a, f);
        edge(&graph, a, b, f);
        edge(&graph, b, leaf, f);

        let snapshot = graph.snapshot();
        let hotspots = rank_hotspots(&snapshot, 10);

        // driver: 1 call + depth 3 = 4; a: 1 call + depth 2 = 3; b: 1 call + depth 1 = 2.
        // leaf has no outgoing calls -> score 0 -> excluded.
        assert_eq!(hotspots.len(), 3, "leaf (score 0) is excluded");
        assert_eq!(hotspots[0].node, driver, "highest score ranks first");
        assert_eq!(hotspots[0].score, 4);
        assert!(
            hotspots[0].score >= hotspots[1].score && hotspots[1].score >= hotspots[2].score,
            "scores are non-increasing"
        );
    }

    #[test]
    fn top_bound_truncates() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let a = add_fn(&mut graph, "a", f);
        let b = add_fn(&mut graph, "b", f);
        let c = add_fn(&mut graph, "c", f);
        edge(&graph, a, b, f);
        edge(&graph, b, c, f);

        let snapshot = graph.snapshot();
        let hotspots = rank_hotspots(&snapshot, 1);
        assert_eq!(
            hotspots.len(),
            1,
            "top=1 keeps only the highest-scoring node"
        );
    }

    #[test]
    fn cyclic_call_graph_terminates() {
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let a = add_fn(&mut graph, "a", f);
        let b = add_fn(&mut graph, "b", f);
        // Mutual recursion: a -> b -> a.
        edge(&graph, a, b, f);
        edge(&graph, b, a, f);

        let snapshot = graph.snapshot();
        // Must terminate (depth cap) and rank both nodes.
        let hotspots = rank_hotspots(&snapshot, 0);
        assert_eq!(hotspots.len(), 2);
    }

    #[test]
    fn densely_cyclic_call_graph_terminates_fast() {
        // Every function calls every other: dense mutual recursion with high
        // branching. A naive depth-capped path walk enumerates ~b^depth paths
        // and hangs here (the case that timed out); the memoized O(V+E) reach
        // must finish immediately and bound every score. Reaching the asserts at
        // all is the regression signal.
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let fns: Vec<NodeId> = (0..6)
            .map(|i| add_fn(&mut graph, &format!("f{i}"), f))
            .collect();
        for &from in &fns {
            for &to in &fns {
                if from != to {
                    edge(&graph, from, to, f);
                }
            }
        }
        let snapshot = graph.snapshot();
        let hotspots = rank_hotspots(&snapshot, 0);
        assert_eq!(hotspots.len(), 6);
        for h in &hotspots {
            // 5 outgoing calls + a bounded chain depth; never explodes.
            assert!(h.score >= 5 && h.score <= 5 + MAX_CHAIN_DEPTH);
        }
        // Deterministic across runs even on the cyclic graph.
        assert_eq!(hotspots, rank_hotspots(&snapshot, 0));
    }

    #[test]
    fn reach_is_order_independent_deterministic() {
        // The determinism guard, deterministic by construction: feed
        // `compute_call_chain_reach` the SAME logical branching cycle
        // (0 -> {1, 2}, 1 -> 2, 2 -> 1) with node 0's callees in two different
        // orders. Because the function canonicalizes (sorts) each adjacency list
        // before the cycle-breaking DFS, both MUST yield the identical reach
        // vector [2, 0, 1]. If the in-place sort were removed, the unsorted
        // [2, 1] input would instead yield [2, 1, 0] and this assert would fail
        // regardless of any HashMap iteration order, which is exactly the
        // regression a snapshot-level test could only catch probabilistically.
        let mut sorted_first = vec![vec![1u32, 2], vec![2], vec![1]];
        let mut reversed_first = vec![vec![2u32, 1], vec![2], vec![1]];
        let reach_a = compute_call_chain_reach(&mut sorted_first);
        let reach_b = compute_call_chain_reach(&mut reversed_first);
        assert_eq!(
            reach_a,
            vec![2, 0, 1],
            "canonical reach for the branching cycle"
        );
        assert_eq!(
            reach_a, reach_b,
            "reach must be identical regardless of input callee order"
        );
    }

    #[test]
    fn branching_cycle_scores_are_canonical() {
        // End-to-end pin over a real snapshot: the same branching cycle
        // v0 -> {v1, v2}, v1 -> v2, v2 -> v1 built with edges inserted in reverse.
        // reach = [2, 0, 1] gives canonical scores v0=4, v1=3, v2=2 and ranking
        // [v0, v1, v2]. (Determinism itself is guarded deterministically by
        // `reach_is_order_independent_deterministic`; this pins the full scoring
        // and ranking wiring end to end.)
        let mut graph = CodeGraph::new();
        let f = register_file(&mut graph, "crate/src/lib.rs");
        let v0 = add_fn(&mut graph, "v0", f);
        let v1 = add_fn(&mut graph, "v1", f);
        let v2 = add_fn(&mut graph, "v2", f);
        // Reverse insertion order (the back-edges first).
        edge(&graph, v2, v1, f);
        edge(&graph, v1, v2, f);
        edge(&graph, v0, v2, f);
        edge(&graph, v0, v1, f);

        let snapshot = graph.snapshot();
        let hotspots = rank_hotspots(&snapshot, 0);

        let score_of = |node: NodeId| {
            hotspots
                .iter()
                .find(|h| h.node == node)
                .map(|h| h.score)
                .unwrap_or_else(|| panic!("node {node:?} missing from ranking"))
        };
        // v0: 2 calls + deepest chain v0->v1->v2 (depth 2) = 4.
        assert_eq!(score_of(v0), 4, "v0 canonical score");
        // v1: 1 call + chain v1->v2->v1(back-edge stops) depth 2 = 3.
        assert_eq!(score_of(v1), 3, "v1 canonical score");
        // v2: 1 call + chain v2->v1->v2(back-edge stops) depth 1 = 2.
        assert_eq!(score_of(v2), 2, "v2 canonical score");
        // Ranking order is fixed: v0, v1, v2.
        assert_eq!(
            hotspots.iter().map(|h| h.node).collect::<Vec<_>>(),
            vec![v0, v1, v2],
            "canonical ranking order independent of edge emission order"
        );
    }
}
