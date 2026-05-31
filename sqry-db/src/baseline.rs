//! Differential-test baseline oracle.
//!
//! One function per registered `DerivedQuery` in [`crate::QueryDb`]
//! (as listed in `register_builtin_queries`, currently 17). Each function
//! is intentionally a **dumb pure graph walk**:
//!
//! - No caching, no memoisation, no fusion.
//! - Operates exclusively on `GraphSnapshot::nodes()` / `edges()` /
//!   `macro_metadata()` / `strings()`.
//! - Output normalised to `BTreeSet` / sorted `Vec` for diff stability.
//!
//! The module is the oracle for the WS1 differential test harness
//! (see `docs/development/graph-fidelity-planner-correctness/02_DESIGN-…`
//! §2.1 — §2.3). The harness proves that the production cached
//! planner queries agree with these readable reference implementations
//! over thousands of proptest-generated graphs; the spot-check test in
//! `sqry-db/tests/baseline_spot_check.rs` hand-pins the expected output
//! for a 6-node fixture.
//!
//! # Coverage matrix (17 registered queries)
//!
//! | # | DerivedQuery | Baseline fn |
//! |---|--------------|-------------|
//! | 1 | `SccQuery` | [`scc`] |
//! | 2 | `CondensationQuery` | [`condensation`] |
//! | 3 | `ReachabilityQuery` | [`reachability`] |
//! | 4 | `CallersQuery` | [`callers`] |
//! | 5 | `CalleesQuery` | [`callees`] |
//! | 6 | `ImportsQuery` | [`imports`] |
//! | 7 | `ExportsQuery` | [`exports`] |
//! | 8 | `ReferencesQuery` | [`references`] |
//! | 9 | `ImplementsQuery` | [`implements`] |
//! | 10 | `CyclesQuery` | [`cycles`] |
//! | 11 | `IsInCycleQuery` | [`is_in_cycle`] |
//! | 12 | `EntryPointsQuery` | [`entry_points`] |
//! | 13 | `ReachableFromEntryPointsQuery` | [`reachable_from_entry_points`] |
//! | 14 | `UnusedQuery` | [`unused`] |
//! | 15 | `IsNodeUnusedQuery` | [`is_node_unused`] |
//! | 16 | `AddressTakenQuery` | [`address_taken`] |
//! | 17 | `CallsitePromiscuousQuery` | [`callsite_promiscuous`] |
//!
//! # Why NodeId-keyed instead of name-keyed?
//!
//! The relation queries in production (`callers:X`, `callees:X`, etc.) take
//! a `RelationKey` (string pattern with language-aware segment matching).
//! The baseline functions take a `NodeId` directly. The differential
//! harness bridges the two by resolving a name pattern back through
//! `snapshot.strings()` for each candidate node — see DESIGN §2.3 for
//! the planned `into_key()` adapter. NodeId-keyed signatures keep the
//! oracle as graph-mechanical as possible.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::{CircularType, UnusedScope};

use crate::queries::cycles::{CycleBounds, IsInCycleKey};
use crate::queries::scc::CachedSccData;
use crate::queries::unused::{IsNodeUnusedKey, UnusedKey};

// ============================================================================
// Helpers
// ============================================================================

/// Returns true iff two `EdgeKind` values share the same enum discriminant.
/// Matches the production convention (e.g. `SccQuery::execute`) of
/// treating `Calls{arg_count=0}` and `Calls{arg_count=3}` as the same
/// edge family for traversal purposes.
fn same_kind(a: &EdgeKind, b: &EdgeKind) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// Walks every live forward edge in the snapshot via the per-node
/// `edges_from` view. This yields the same set the production queries
/// see (CSR + delta unified, tombstones suppressed).
fn for_each_edge<F: FnMut(NodeId, NodeId, &EdgeKind)>(snapshot: &GraphSnapshot, mut f: F) {
    for (src, _entry) in snapshot.nodes().iter() {
        for edge in &snapshot.edges().edges_from(src) {
            f(src, edge.target, &edge.kind);
        }
    }
}

/// Returns true iff this node is a Phase 4c-prime unified loser. Mirrors
/// the `is_unified_loser()` skip the production queries apply so the
/// baseline doesn't report ghost IDs the planner already filters out.
fn skip_loser(entry: &NodeEntry) -> bool {
    entry.is_unified_loser()
}

// ============================================================================
// 1. SccQuery
// ============================================================================

/// Computes strongly connected components over edges matching `edge_kind`'s
/// discriminant. Returns the same shape `SccQuery::execute` produces, with
/// the addition that components and per-node mapping are constructed by a
/// recursive-free Tarjan walk over `BTreeSet`-ordered nodes for stable
/// component numbering.
///
/// Phase 4c-prime unified losers are filtered (see
/// `queries::scc::SccQuery::execute`).
#[must_use]
pub fn scc(snapshot: &GraphSnapshot, edge_kind: &EdgeKind) -> Arc<CachedSccData> {
    let nodes: Vec<NodeId> = snapshot
        .nodes()
        .iter()
        .filter(|(_, entry)| !skip_loser(entry))
        .map(|(nid, _)| nid)
        .collect();
    let node_set: HashSet<NodeId> = nodes.iter().copied().collect();

    let mut index_counter: u32 = 0;
    let mut indices: HashMap<NodeId, u32> = HashMap::new();
    let mut lowlinks: HashMap<NodeId, u32> = HashMap::new();
    let mut stack: Vec<NodeId> = Vec::new();
    let mut on_stack: HashSet<NodeId> = HashSet::new();
    let mut components: Vec<Vec<NodeId>> = Vec::new();

    // Compute the set of outgoing neighbours for `node` filtered to
    // `edge_kind`'s discriminant and to `node_set`, sorted by `NodeId` for
    // determinism. Called exactly once per node-push.
    //
    // Mirrors the determinism contract on `SccQuery::execute` (planner side):
    // `EdgeStore::edges_from` returns delta-buffer entries in `HashMap`
    // order, so re-querying `edges_from` on subsequent iterations of the
    // same work-stack frame produced different orderings on the same input
    // — which mis-targeted `lowlink` propagation and produced different SCC
    // results across runs. Cache the neighbour list at push time and read
    // the cached vector thereafter; sort by `NodeId` for canonical edge
    // order. Mirrors the production fix in `queries/scc.rs`; WS1 regression
    // seed `cc a8168cea9a32524e5950c6f414a84d72214577aa76365c73a86b4f0248002fbe`.
    let neighbours_of = |node: NodeId| -> Vec<NodeId> {
        let mut ns: Vec<NodeId> = snapshot
            .edges()
            .edges_from(node)
            .iter()
            .filter(|e| same_kind(&e.kind, edge_kind))
            .map(|e| e.target)
            .filter(|t| node_set.contains(t))
            .collect();
        ns.sort_unstable();
        ns
    };

    for &start in &nodes {
        if indices.contains_key(&start) {
            continue;
        }
        // Iterative Tarjan: (node, next-neighbour-index, cached-neighbours).
        let mut work: Vec<(NodeId, usize, Vec<NodeId>)> = vec![(start, 0, neighbours_of(start))];
        indices.insert(start, index_counter);
        lowlinks.insert(start, index_counter);
        index_counter += 1;
        stack.push(start);
        on_stack.insert(start);

        while let Some((node, pos, neighbours)) = work.last_mut() {
            if *pos < neighbours.len() {
                let next = neighbours[*pos];
                *pos += 1;
                if let std::collections::hash_map::Entry::Vacant(e) = indices.entry(next) {
                    e.insert(index_counter);
                    lowlinks.insert(next, index_counter);
                    index_counter += 1;
                    stack.push(next);
                    on_stack.insert(next);
                    work.push((next, 0, neighbours_of(next)));
                } else if on_stack.contains(&next) {
                    let node_copy = *node;
                    let next_idx = indices[&next];
                    let cur_low = lowlinks[&node_copy];
                    if next_idx < cur_low {
                        lowlinks.insert(node_copy, next_idx);
                    }
                }
            } else {
                let node_copy = *node;
                let node_idx = indices[&node_copy];
                let node_low = lowlinks[&node_copy];
                if node_low == node_idx {
                    let mut component = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack.remove(&w);
                        component.push(w);
                        if w == node_copy {
                            break;
                        }
                    }
                    components.push(component);
                }
                work.pop();
                if let Some((parent, _, _)) = work.last() {
                    let parent_copy = *parent;
                    let parent_low = lowlinks[&parent_copy];
                    if node_low < parent_low {
                        lowlinks.insert(parent_copy, node_low);
                    }
                }
            }
        }
    }

    let mut node_to_component: HashMap<NodeId, u32> = HashMap::with_capacity(nodes.len());
    for (idx, component) in components.iter().enumerate() {
        for &nid in component {
            node_to_component.insert(nid, idx as u32);
        }
    }
    Arc::new(CachedSccData {
        node_to_component,
        components,
        edge_kind: edge_kind.clone(),
    })
}

// ============================================================================
// 2. CondensationQuery
// ============================================================================

/// Returns the deduplicated DAG-edge adjacency keyed by `(src_component,
/// successor_component)` for all inter-component edges of `edge_kind`'s
/// discriminant. Mirrors `CondensationQuery::execute` but as an
/// independently-computed reference; the value is a sorted set of edges
/// for diff stability.
#[must_use]
pub fn condensation(snapshot: &GraphSnapshot, edge_kind: &EdgeKind) -> BTreeSet<(u32, u32)> {
    let scc_data = scc(snapshot, edge_kind);
    let mut dag_edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for_each_edge(snapshot, |src, tgt, kind| {
        if !same_kind(kind, edge_kind) {
            return;
        }
        if let (Some(s), Some(t)) = (scc_data.component_of(src), scc_data.component_of(tgt))
            && s != t
        {
            dag_edges.insert((s, t));
        }
    });
    dag_edges
}

// ============================================================================
// 3. ReachabilityQuery
// ============================================================================

/// BFS from `roots`, following edges whose discriminant matches `edge_kind`.
/// Returns the set of reachable nodes (including the roots themselves).
#[must_use]
pub fn reachability(
    snapshot: &GraphSnapshot,
    roots: &[NodeId],
    edge_kind: &EdgeKind,
) -> BTreeSet<NodeId> {
    let mut reached: BTreeSet<NodeId> = roots.iter().copied().collect();
    let mut worklist: Vec<NodeId> = roots.to_vec();
    while let Some(node) = worklist.pop() {
        for edge in &snapshot.edges().edges_from(node) {
            if !same_kind(&edge.kind, edge_kind) {
                continue;
            }
            if reached.insert(edge.target) {
                worklist.push(edge.target);
            }
        }
    }
    reached
}

// ============================================================================
// 4–9. Relation predicates (callers, callees, imports, exports,
// references, implements)
//
// The production queries are name-keyed (RelationKey carries a string
// pattern); the baselines are NodeId-keyed by design (see module docs).
// Each function returns the set of nodes whose relation set "contains"
// the target NodeId. The differential harness bridges name↔NodeId.
// ============================================================================

/// `callers:<target>` oracle: returns all nodes `N` such that the
/// production `callers:<name_of_target>` query, against a graph where
/// `name_of_target` uniquely identifies `target`, would include `N`.
/// Concretely: the set of nodes that have an outgoing `Calls` edge whose
/// source name matches the target's name — i.e. the set of nodes called
/// by `target`.
///
/// In planner terms, this is "for each candidate node `N`, does `N` have
/// an incoming `Calls` edge whose source is `target`?" — see
/// `RelationKind::Callers` traversal table in `queries/relation.rs`.
#[must_use]
pub fn callers(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        for edge in &snapshot.edges().edges_to(node_id) {
            if matches!(edge.kind, EdgeKind::Calls { .. }) && edge.source == target {
                out.insert(node_id);
                break;
            }
        }
    }
    out
}

/// `callees:<target>` oracle: nodes whose outgoing `Calls` edge target is
/// `target`. Reading: the set of nodes that call `target`.
#[must_use]
pub fn callees(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        for edge in &snapshot.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Calls { .. }) && edge.target == target {
                out.insert(node_id);
                break;
            }
        }
    }
    out
}

/// `imports:<target>` oracle: nodes whose outgoing `Imports` edge target
/// is `target`. (Per-node semantics; aliases and wildcards are not
/// modeled in the NodeId-keyed oracle — they require name-keyed lookups
/// the differential harness exercises separately.)
#[must_use]
pub fn imports(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        for edge in &snapshot.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Imports { .. }) && edge.target == target {
                out.insert(node_id);
                break;
            }
        }
    }
    out
}

/// `exports:<target>` oracle: nodes participating in an `Exports` edge
/// (either direction) whose OTHER endpoint is `target`. Self-loops are
/// skipped to mirror the `EndpointRole::Either` self-loop skip in
/// `queries::relation::node_has_matching_endpoint`.
#[must_use]
pub fn exports(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        if node_id == target {
            continue;
        }
        let outgoing = snapshot.edges().edges_from(node_id);
        let incoming = snapshot.edges().edges_to(node_id);
        let hit_out = outgoing.iter().any(|e| {
            matches!(e.kind, EdgeKind::Exports { .. }) && e.target == target && e.source != e.target
        });
        let hit_in = incoming.iter().any(|e| {
            matches!(e.kind, EdgeKind::Exports { .. }) && e.source == target && e.source != e.target
        });
        if hit_out || hit_in {
            out.insert(node_id);
        }
    }
    out
}

/// `references:<target>` oracle: nodes whose incoming reference edges
/// (`Calls`, `References`, `Imports`, `FfiCall`) come from `target`.
#[must_use]
pub fn references(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        for edge in &snapshot.edges().edges_to(node_id) {
            let is_ref = matches!(
                edge.kind,
                EdgeKind::Calls { .. }
                    | EdgeKind::References
                    | EdgeKind::Imports { .. }
                    | EdgeKind::FfiCall { .. }
            );
            if is_ref && edge.source == target {
                out.insert(node_id);
                break;
            }
        }
    }
    out
}

/// `implements:<target>` oracle: nodes whose outgoing `Implements` edge
/// reaches `target`.
#[must_use]
pub fn implements(snapshot: &GraphSnapshot, target: NodeId) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        for edge in &snapshot.edges().edges_from(node_id) {
            if matches!(edge.kind, EdgeKind::Implements) && edge.target == target {
                out.insert(node_id);
                break;
            }
        }
    }
    out
}

// ============================================================================
// 10–11. Cycle queries
// ============================================================================

/// Maps a [`CircularType`] to the canonical edge-kind discriminant used
/// for SCC traversal. Mirrors `queries::cycles::edge_probe_for`.
fn cycle_edge_probe(circular_type: CircularType) -> EdgeKind {
    use sqry_core::graph::unified::edge::kind::ResolvedVia;
    match circular_type {
        CircularType::Calls => EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        CircularType::Imports | CircularType::Modules => EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
    }
}

/// True iff `node` has a self-edge of `circular_type`'s discriminant.
fn node_has_self_loop(snapshot: &GraphSnapshot, node: NodeId, circular_type: CircularType) -> bool {
    for edge in &snapshot.edges().edges_from(node) {
        if edge.target != node {
            continue;
        }
        let probe = cycle_edge_probe(circular_type);
        if same_kind(&edge.kind, &probe) {
            return true;
        }
    }
    false
}

/// Returns every cycle in the graph: a `Vec<Vec<NodeId>>` where each
/// inner vector is an SCC satisfying [`CycleBounds`]. Size-1 SCCs are
/// reported only when the node carries a self-edge and
/// `bounds.should_include_self_loops` is true. Truncated to
/// `bounds.max_results`.
#[must_use]
pub fn cycles(
    snapshot: &GraphSnapshot,
    circular_type: CircularType,
    bounds: CycleBounds,
) -> Vec<Vec<NodeId>> {
    let probe = cycle_edge_probe(circular_type);
    let scc_data = scc(snapshot, &probe);
    let mut out: Vec<Vec<NodeId>> = Vec::new();
    for component in &scc_data.components {
        if out.len() >= bounds.max_results {
            break;
        }
        let size = component.len();
        let self_loop = size == 1 && node_has_self_loop(snapshot, component[0], circular_type);
        let include = if self_loop {
            bounds.should_include_self_loops
        } else {
            size >= 2 && size >= bounds.min_depth && bounds.max_depth.is_none_or(|m| size <= m)
        };
        if include {
            out.push(component.clone());
        }
    }
    out
}

/// True iff `node` participates in a cycle under `circular_type` and
/// `bounds`. Mirrors `IsInCycleQuery::execute`.
#[must_use]
pub fn is_in_cycle(snapshot: &GraphSnapshot, key: &IsInCycleKey) -> bool {
    let self_loop = node_has_self_loop(snapshot, key.node_id, key.circular_type);
    if self_loop && key.bounds.should_include_self_loops {
        return true;
    }
    let probe = cycle_edge_probe(key.circular_type);
    let scc_data = scc(snapshot, &probe);
    let Some(component_idx) = scc_data.component_of(key.node_id) else {
        return false;
    };
    let Some(component) = scc_data.components.get(component_idx as usize) else {
        return false;
    };
    let size = component.len();
    if size < 2 {
        return false;
    }
    if size < key.bounds.min_depth {
        return false;
    }
    if key.bounds.max_depth.is_some_and(|m| size > m) {
        return false;
    }
    true
}

// ============================================================================
// 12–13. Entry-point + reachable-from-entry-point
// ============================================================================

/// True iff this node qualifies as a reachability root. Mirrors
/// `queries::unused::is_entry_point`.
fn entry_point_predicate(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    let is_public = entry
        .visibility
        .and_then(|id| snapshot.strings().resolve(id))
        .is_some_and(|v| {
            let s = v.as_ref();
            s == "public" || s == "pub"
        });
    let is_main_or_test = snapshot.strings().resolve(entry.name).is_some_and(|name| {
        let n = name.as_ref();
        n == "main" || n.starts_with("test_") || n.ends_with("_test")
    });
    let is_export = matches!(entry.kind, NodeKind::Export);
    let is_test = matches!(entry.kind, NodeKind::Test);
    is_public || is_main_or_test || is_export || is_test
}

/// Set of entry-point nodes. Phase 4c-prime losers are filtered to
/// match `EntryPointsQuery::execute`.
#[must_use]
pub fn entry_points(snapshot: &GraphSnapshot) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        if entry_point_predicate(snapshot, entry) {
            out.insert(node_id);
        }
    }
    out
}

/// True iff this edge kind participates in reachability. Mirrors
/// `queries::unused::is_reachability_edge`.
fn is_reachability_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls { .. }
            | EdgeKind::References
            | EdgeKind::Imports { .. }
            | EdgeKind::Inherits
            | EdgeKind::Implements
            | EdgeKind::TypeOf { .. }
    )
}

/// Set of nodes reachable from the entry-point set via reachability
/// edges.
#[must_use]
pub fn reachable_from_entry_points(snapshot: &GraphSnapshot) -> BTreeSet<NodeId> {
    let entries = entry_points(snapshot);
    let mut reached: BTreeSet<NodeId> = entries.clone();
    let mut worklist: Vec<NodeId> = entries.into_iter().collect();
    while let Some(node) = worklist.pop() {
        for edge in &snapshot.edges().edges_from(node) {
            if !is_reachability_edge(&edge.kind) {
                continue;
            }
            if reached.insert(edge.target) {
                worklist.push(edge.target);
            }
        }
    }
    reached
}

// ============================================================================
// 14–15. Unused detection
// ============================================================================

/// True iff this entry passes the [`UnusedScope`] filter. Mirrors
/// `queries::unused::scope_matches`.
fn scope_matches(entry: &NodeEntry, snapshot: &GraphSnapshot, scope: UnusedScope) -> bool {
    match scope {
        UnusedScope::All => true,
        UnusedScope::Public => entry
            .visibility
            .and_then(|id| snapshot.strings().resolve(id))
            .is_some_and(|v| {
                let s = v.as_ref();
                s == "public" || s == "pub"
            }),
        UnusedScope::Private => {
            let vis = entry
                .visibility
                .and_then(|id| snapshot.strings().resolve(id));
            vis.is_none()
                || vis.is_some_and(|v| {
                    let s = v.as_ref();
                    s != "public" && s != "pub"
                })
        }
        UnusedScope::Function => matches!(entry.kind, NodeKind::Function | NodeKind::Method),
        UnusedScope::Struct => matches!(entry.kind, NodeKind::Struct | NodeKind::Class),
    }
}

/// True iff this entry is an "always entry point" (never marked unused
/// regardless of scope). Mirrors `queries::unused::is_always_entry_point`.
fn always_entry_point(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    let is_main_or_test = snapshot.strings().resolve(entry.name).is_some_and(|name| {
        let n = name.as_ref();
        n == "main" || n.starts_with("test_") || n.ends_with("_test")
    });
    let is_export = matches!(entry.kind, NodeKind::Export);
    let is_test = matches!(entry.kind, NodeKind::Test);
    is_main_or_test || is_export || is_test
}

/// Sorted list of unused nodes per the [`UnusedKey`] filter. Mirrors
/// `UnusedQuery::execute`.
#[must_use]
pub fn unused(snapshot: &GraphSnapshot, key: &UnusedKey) -> Vec<NodeId> {
    let reachable = reachable_from_entry_points(snapshot);
    let mut out: Vec<NodeId> = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if out.len() >= key.max_results {
            break;
        }
        if skip_loser(entry) {
            continue;
        }
        if !scope_matches(entry, snapshot, key.scope) {
            continue;
        }
        if always_entry_point(snapshot, entry) {
            continue;
        }
        if !reachable.contains(&node_id) {
            out.push(node_id);
        }
    }
    out.sort_unstable_by_key(|id| (id.index(), id.generation()));
    out
}

/// True iff this single node is unused per [`IsNodeUnusedKey`]. Mirrors
/// `IsNodeUnusedQuery::execute`.
#[must_use]
pub fn is_node_unused(snapshot: &GraphSnapshot, key: &IsNodeUnusedKey) -> bool {
    let Some(entry) = snapshot.nodes().get(key.node_id) else {
        return false;
    };
    if skip_loser(entry) {
        return false;
    }
    if !scope_matches(entry, snapshot, key.scope) {
        return false;
    }
    if always_entry_point(snapshot, entry) {
        return false;
    }
    let reachable = reachable_from_entry_points(snapshot);
    !reachable.contains(&key.node_id)
}

// ============================================================================
// 16–17. C indirect-call precision markers
// ============================================================================

/// Sorted set of nodes carrying the `ADDRESS_TAKEN` marker flag.
#[must_use]
pub fn address_taken(snapshot: &GraphSnapshot) -> Vec<NodeId> {
    let metadata = snapshot.macro_metadata();
    let mut out: Vec<NodeId> = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        if metadata.is_address_taken(node_id) {
            out.push(node_id);
        }
    }
    out.sort_unstable_by_key(|id| (id.index(), id.generation()));
    out
}

/// Sorted set of nodes carrying the `CALLSITE_PROMISCUOUS` marker flag.
#[must_use]
pub fn callsite_promiscuous(snapshot: &GraphSnapshot) -> Vec<NodeId> {
    let metadata = snapshot.macro_metadata();
    let mut out: Vec<NodeId> = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if skip_loser(entry) {
            continue;
        }
        if metadata.is_callsite_promiscuous(node_id) {
            out.push(node_id);
        }
    }
    out.sort_unstable_by_key(|id| (id.index(), id.generation()));
    out
}
