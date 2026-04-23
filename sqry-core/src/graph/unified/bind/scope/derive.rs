//! Scope derivation — walks `Contains` edges from per-file node sets and
//! emits one `Scope` record per `NodeKind::{Module, Function, Method, Class,
//! Struct, Enum, Interface, Trait}` node encountered.
//!
//! This function is the deriver called from
//! [`super::super::super::build::phase4e_binding::derive_binding_plane`]
//! during Phase 4e. It reads only `EdgeKind::Contains` edges — never
//! `FfiCall` / `HttpRequest` / cross-language edge kinds, because Phase 4e
//! runs before Pass 5 injects those.
//!
//! # Two-pass algorithm
//!
//! The derivation runs in two passes to eliminate an order-dependency bug
//! where inner-first plugin emission order caused children to receive
//! `parent = ScopeId::INVALID` because their parent was not yet allocated:
//!
//! **Pass 1** — Allocate one `Scope` per scope-worthy node with
//! `parent = ScopeId::INVALID` (placeholder). Build a
//! `HashMap<NodeId, ScopeId>` mapping each scope-worthy node to its
//! freshly allocated `ScopeId`. This pass is O(N) in the number of nodes.
//!
//! **Pass 2** — For every allocated scope, walk `Contains` edges upward
//! from the scope's node using the pre-built `BindingEdgeIndex::contains_parents`
//! map (O(1) per hop). Look each ancestor up in the `node_to_scope` map —
//! the first hit is the enclosing scope. Stamp the parent via
//! `ScopeArena::set_parent`. This pass is O(N · depth) in the worst case
//! but in practice bounded by tree depth (rarely > 10 levels).
//!
//! # Performance
//!
//! Pass 2 previously called `graph.edges().edges_to(node)` for each
//! scope-worthy node, triggering an O(E) delta buffer scan per call and
//! degrading to O(N × E) overall on large codebases. It now uses the
//! `contains_parents` map from `BindingEdgeIndex`, which was built by a
//! single O(E) delta scan in `derive_binding_plane`. Each hop is O(1).

use std::collections::HashMap;

use crate::graph::unified::build::phase4e_binding::BindingEdgeIndex;
use crate::graph::unified::mutation_target::GraphMutationTarget;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;

use super::arena::{Scope, ScopeArena, ScopeId, ScopeKind};

/// Builds a `ScopeArena` by walking `Contains` edges from every file's node
/// set. Top-level scopes (those whose containing-file has no scope-worthy
/// ancestor) get `parent = ScopeId::INVALID`; nested scopes carry their
/// enclosing scope's id.
///
/// Uses a two-pass algorithm that is iteration-order independent: whether a
/// plugin emits parent nodes before or after child nodes does not affect the
/// resulting scope tree.
///
/// The `edge_index` parameter provides O(1) `Contains`-parent lookups that
/// replace the per-node O(E) `edges_to()` calls from the old implementation.
///
/// # Generic mutation target
///
/// Task 4 Step 4 Phase 2 re-typed `graph` from `&CodeGraph` to
/// `&G: GraphMutationTarget` so the same deriver drives both the
/// full-build pipeline (backed by `CodeGraph`) and the incremental
/// rebuild pipeline (backed by `RebuildGraph`). The function only
/// reads `nodes()`, `files()`, and `indices()` via the trait; there
/// is no mutation on this side.
pub(crate) fn derive_scopes<G: GraphMutationTarget>(
    graph: &G,
    edge_index: &BindingEdgeIndex,
) -> ScopeArena {
    let mut arena = ScopeArena::new();
    let mut node_to_scope: HashMap<NodeId, ScopeId> = HashMap::default();

    // ---- Pass 1: allocate one scope record per scope-worthy node
    //              with a placeholder INVALID parent.
    for (file_id, _path) in graph.files().iter() {
        for &node_id in graph.indices().by_file(file_id) {
            let Some(entry) = graph.nodes().get(node_id) else {
                continue;
            };
            let Some(scope_kind) = node_kind_to_scope_kind(entry.kind) else {
                continue;
            };
            let scope = Scope {
                kind: scope_kind,
                parent: ScopeId::INVALID, // pass 2 will stamp this
                node: node_id,
                byte_span: (entry.start_byte, entry.end_byte),
                file: entry.file,
            };
            let scope_id = arena.allocate(scope);
            node_to_scope.insert(node_id, scope_id);
        }
    }

    // ---- Pass 2: walk parent chains via the pre-built contains_parents
    //              index (O(1) per hop), stamp parents into already-allocated
    //              scopes via set_parent.
    let scope_ids: Vec<ScopeId> = node_to_scope.values().copied().collect();
    for scope_id in scope_ids {
        let scope_node = arena
            .get(scope_id)
            .expect("freshly allocated scope must exist")
            .node;
        let parent_scope_id =
            enclosing_scope_id_via_index(scope_node, &node_to_scope, &edge_index.contains_parents);
        arena
            .set_parent(scope_id, parent_scope_id)
            .expect("freshly allocated scope must accept set_parent");
    }

    arena
}

/// Maps a `NodeKind` to the corresponding `ScopeKind`, or `None` if the
/// node kind is not scope-worthy.
fn node_kind_to_scope_kind(kind: NodeKind) -> Option<ScopeKind> {
    match kind {
        NodeKind::Module => Some(ScopeKind::Module),
        NodeKind::Function | NodeKind::Method => Some(ScopeKind::Function),
        NodeKind::Class | NodeKind::Struct | NodeKind::Enum => Some(ScopeKind::Class),
        NodeKind::Interface | NodeKind::Trait => Some(ScopeKind::Trait),
        // Other NodeKinds (Variable, Parameter, CallSite, Import, Export, etc.)
        // are not scope-worthy.
        _ => None,
    }
}

/// Walks the pre-built `contains_parents` map upward from `start` looking
/// for the nearest scope-worthy ancestor in `map`. Returns `ScopeId::INVALID`
/// if the walk reaches the file root (no more `Contains` edges) without
/// finding a hit.
///
/// Each hop is O(1) (hash map lookup), so the total cost is O(tree depth)
/// rather than the O(E) delta scan that the old `edges_to`-based
/// implementation incurred per call.
fn enclosing_scope_id_via_index(
    start: NodeId,
    map: &HashMap<NodeId, ScopeId>,
    contains_parents: &HashMap<NodeId, NodeId>,
) -> ScopeId {
    let mut current = start;
    loop {
        // O(1) parent lookup from the pre-built index.
        let Some(&parent) = contains_parents.get(&current) else {
            // Reached the root — no enclosing scope.
            return ScopeId::INVALID;
        };
        if let Some(&scope_id) = map.get(&parent) {
            return scope_id;
        }
        // Parent exists but is not scope-worthy; keep climbing.
        current = parent;
    }
}
