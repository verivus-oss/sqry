//! `callers:X` derived query.
//!
//! Under the planner's relation convention (shared with the DB12 inline
//! `relation_matches` path), `callers:X` filters a node set to those nodes
//! whose **incoming** `Calls` edges carry a source whose name matches `X`.
//! Reading it right-to-left: `X` appears in each result node's `callers`
//! list — i.e. the nodes returned are ones that `X` calls.
//!
//! The real computation lives in
//! [`super::relation::compute_relation_source_set`]. This module only pins
//! the [`DerivedQuery`] identity so the sharded cache routes the result to
//! its own slot.

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::query::DerivedQuery;

use super::relation::{RelationKey, RelationKind, compute_relation_source_set};

/// `callers:X` — filter to nodes where `X` is one of the callers.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: any change in the global `Calls`
/// topology can introduce or remove callers of a given name.
pub struct CallersQuery;

impl DerivedQuery for CallersQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CALLERS;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::Callers, key, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
    use sqry_core::graph::unified::node::kind::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn callers_query_matches_planner_semantics() {
        // main --Calls--> target, unrelated has no edges.
        // `callers:main` = {target} — target's callers list includes main.
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("lib.rs")).unwrap();
        let main_name = graph.strings_mut().intern("main").unwrap();
        let target_name = graph.strings_mut().intern("target").unwrap();
        let unrelated_name = graph.strings_mut().intern("unrelated").unwrap();

        let main_fn = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file).with_qualified_name(main_name),
            )
            .unwrap();
        let target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, target_name, file)
                    .with_qualified_name(target_name),
            )
            .unwrap();
        let unrelated = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, unrelated_name, file)
                    .with_qualified_name(unrelated_name),
            )
            .unwrap();

        graph.edges_mut().add_edge(
            main_fn,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        db.register::<CallersQuery>();

        let matches = db.get::<CallersQuery>(&RelationKey::exact("main"));
        assert!(matches.contains(&target));
        assert!(!matches.contains(&main_fn));
        assert!(!matches.contains(&unrelated));
    }

    #[test]
    fn callers_query_dynamic_language_method_segment_fallback() {
        // Dynamic-language fallback: the pattern `Player::takeDamage` has
        // `takeDamage` as its method segment. When the actual callee is
        // `Enemy::takeDamage`, the method-segment fallback keeps the match
        // alive so cross-receiver Ruby/Python dispatch stays covered.
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("game.rb")).unwrap();
        assert!(
            graph
                .files_mut()
                .set_language(file, sqry_core::graph::node::Language::Ruby)
        );

        let caller_name = graph.strings_mut().intern("Game::update").unwrap();
        let callee_name = graph.strings_mut().intern("Enemy::takeDamage").unwrap();

        let caller = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Method, caller_name, file)
                    .with_qualified_name(caller_name),
            )
            .unwrap();
        let callee = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Method, callee_name, file)
                    .with_qualified_name(callee_name),
            )
            .unwrap();
        graph.edges_mut().add_edge(
            caller,
            callee,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        let snapshot = Arc::new(graph.snapshot());
        // `callers:Enemy::takeDamage` — the set of nodes whose callers
        // include `Enemy::takeDamage`. Under Callers semantics this is the
        // set of nodes that `Enemy::takeDamage` calls. (Our fixture only
        // has one Calls edge going *into* Enemy::takeDamage, so that set is
        // empty.) The interesting assertion is that the trailing-segment
        // fallback is what makes the *graph_eval* convention test
        // (`callers:Player::takeDamage` matching Game::update as a caller
        // of Enemy::takeDamage) still work when it's wired in that
        // direction — the helper lives in
        // [`super::relation::method_segment_matches`] and is exercised here
        // by the parallel `callees:Player::takeDamage` assertion below.
        let matches = compute_relation_source_set(
            RelationKind::Callees,
            &RelationKey::exact("Player::takeDamage"),
            &snapshot,
        );
        assert!(
            matches.contains(&caller),
            "Game::update calls Enemy::takeDamage, whose trailing segment \
             `takeDamage` matches `Player::takeDamage` under the dynamic \
             fallback."
        );
    }
}
