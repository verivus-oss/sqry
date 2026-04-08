//! Shared graph handler helpers for seed lookup.
//!
//! Seed lookup delegates to [`sqry_core::graph::unified::materialize`].

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::materialize;
use sqry_core::graph::unified::node::NodeId;

/// Resolve one symbol query into ordered candidate seeds.
///
/// Delegates to [`materialize::find_nodes_by_name`].
#[must_use]
pub(crate) fn find_nodes_by_name(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    materialize::find_nodes_by_name(snapshot, name)
}

/// Resolve several symbol queries into a stable, deduplicated seed set.
///
/// Delegates to [`materialize::collect_symbol_seeds`].
#[must_use]
pub(crate) fn collect_symbol_seeds(snapshot: &GraphSnapshot, symbols: &[String]) -> Vec<NodeId> {
    materialize::collect_symbol_seeds(snapshot, symbols)
}
