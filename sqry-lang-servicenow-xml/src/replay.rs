//! Multi-record staging replay for safe delegation across multiple records.
//!
//! When a multi-record XML file contains multiple script records, each delegation
//! gets its own `StagingGraph`. This module replays delegation staging operations
//! into the main staging with remapped `StringId`s and `NodeId`s to avoid collisions.

use std::collections::HashMap;

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::string::StringId;
use sqry_core::graph::{GraphBuilderError, GraphResult};

/// Tracks local string ID allocation across multiple delegation replays.
pub struct ReplayState {
    next_string_id: u32,
}

impl ReplayState {
    /// Create a new replay state seeded from the main staging's current string count.
    #[must_use]
    pub fn new(main_staging: &StagingGraph) -> Self {
        Self {
            next_string_id: main_staging.string_count_u32(),
        }
    }

    /// Replay delegation staging operations into the main staging.
    ///
    /// Uses `StagingGraph::apply_string_remap()` to rewrite `StringId`s in `AddNode`/`AddEdge`
    /// ops, then replays nodes and uses `get_remapped_edges()` for edges.
    ///
    /// # Ordering invariant
    /// `InternString` ops are collected first, then `apply_string_remap()` rewrites
    /// `AddNode`/`AddEdge` payloads (not `InternString` ops themselves).
    ///
    /// # Body hashes
    /// `attach_body_hashes()` must be called on the delegation staging with the
    /// delegated script bytes BEFORE calling this method.
    ///
    /// # Errors
    ///
    /// Returns a `GraphResult` error if replaying any node or edge operation fails.
    pub fn replay(
        &mut self,
        main_staging: &mut StagingGraph,
        del_staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Step 1: Re-intern strings from delegation into main staging's ID space.
        let mut string_remap: HashMap<StringId, StringId> = HashMap::new();
        for op in del_staging.operations() {
            if let StagingOp::InternString { local_id, value } = op {
                let new_local = StringId::new_local(self.next_string_id);
                self.next_string_id += 1;
                main_staging.intern_string(new_local, value.clone());
                string_remap.insert(*local_id, new_local);
            }
        }

        // Step 2: Apply string remap to delegation staging's AddNode/AddEdge ops.
        del_staging.apply_string_remap(&string_remap).map_err(|e| {
            GraphBuilderError::CrossLanguageError {
                reason: format!("Replay string remap failed: {e}"),
            }
        })?;

        // Step 3: Replay AddNode ops into main staging, building node remap.
        let mut node_remap: HashMap<NodeId, NodeId> = HashMap::new();
        for op in del_staging.operations() {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let new_node_id = main_staging.add_node(entry.clone());
                if let Some(old_id) = expected_id {
                    node_remap.insert(*old_id, new_node_id);
                }
            }
        }

        // Step 4: Validate all edge endpoints are in node_remap.
        // get_remapped_edges() falls back to unwrap_or(*source) for unmapped IDs,
        // which can silently alias a main-staging node. Pre-validate to catch this.
        for op in del_staging.operations() {
            if let StagingOp::AddEdge { source, target, .. } = op {
                if !node_remap.contains_key(source) {
                    return Err(GraphBuilderError::CrossLanguageError {
                        reason: format!("Replay: unmapped source NodeId {source:?}"),
                    });
                }
                if !node_remap.contains_key(target) {
                    return Err(GraphBuilderError::CrossLanguageError {
                        reason: format!("Replay: unmapped target NodeId {target:?}"),
                    });
                }
            }
        }

        // Step 5: Collect edges with remapped NodeIds and replay into main staging.
        let remapped_edges = del_staging.get_remapped_edges(&node_remap);
        main_staging.add_edges(&remapped_edges);

        Ok(())
    }
}
