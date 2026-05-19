//! Reachability derived query.
//!
//! Computes the set of nodes reachable from a given root set by following
//! edges of a specified kind. Used by `find_unused` to identify entry points
//! and compute the reachable set.

use std::collections::HashSet;
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
#[cfg(test)]
use sqry_core::graph::unified::edge::kind::ResolvedVia;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

// PN3 cold-start persistence: ReachabilityKey and ReachableSet are serialized
// via postcard at cache-insert time. EdgeKind and NodeId already derive
// Serialize/Deserialize from sqry-core.

/// Key for a reachability query: a set of root nodes + edge kind.
#[derive(Debug, Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReachabilityKey {
    /// Root nodes to start BFS from.
    pub roots: Vec<NodeId>,
    /// Edge kind to follow.
    pub edge_kind: EdgeKind,
}

/// Result of a reachability query: the set of reachable nodes.
// HashSet is serde-able because NodeId derives Serialize/Deserialize.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReachableSet {
    /// All nodes reachable from the root set (includes roots themselves).
    pub reachable: HashSet<NodeId>,
}

/// Computes the set of all nodes reachable from a root set via BFS.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: invalidated when any edge changes, because
/// a new edge could make previously-unreachable nodes reachable.
pub struct ReachabilityQuery;

impl DerivedQuery for ReachabilityQuery {
    type Key = ReachabilityKey;
    type Value = Arc<ReachableSet>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::REACHABILITY;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(
        key: &ReachabilityKey,
        _db: &QueryDb,
        snapshot: &GraphSnapshot,
    ) -> Arc<ReachableSet> {
        // Record all files as deps (global topology query)
        for (fid, _seg) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        // BFS from roots
        let mut reachable = HashSet::new();
        let mut queue: std::collections::VecDeque<NodeId> = key.roots.iter().copied().collect();

        for &root in &key.roots {
            reachable.insert(root);
        }

        while let Some(node) = queue.pop_front() {
            for edge_ref in &snapshot.edges().edges_from(node) {
                if std::mem::discriminant(&edge_ref.kind) == std::mem::discriminant(&key.edge_kind)
                    && reachable.insert(edge_ref.target)
                {
                    queue.push_back(edge_ref.target);
                }
            }
        }

        Arc::new(ReachableSet { reachable })
    }
}

// ============================================================================
// PN3 serde roundtrip tests
// ============================================================================

#[cfg(test)]
mod serde_roundtrip {
    use super::*;
    use postcard::{from_bytes, to_allocvec};

    #[test]
    fn reachability_key_roundtrip() {
        let original = ReachabilityKey {
            roots: vec![NodeId::new(1, 1), NodeId::new(5, 2)],
            edge_kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: ReachabilityKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn reachable_set_roundtrip() {
        let mut reachable = HashSet::new();
        reachable.insert(NodeId::new(10, 1));
        reachable.insert(NodeId::new(20, 1));
        let original = ReachableSet { reachable };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: ReachableSet = from_bytes(&bytes).expect("deserialize failed");
        // HashSet order is not deterministic, so compare by re-serializing.
        // Both sets must contain the same elements.
        assert_eq!(decoded.reachable.len(), original.reachable.len());
        for node in &original.reachable {
            assert!(
                decoded.reachable.contains(node),
                "node {node:?} missing from decoded ReachableSet"
            );
        }
    }
}
