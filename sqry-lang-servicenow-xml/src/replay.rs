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
        //
        // Body extents are deliberately NOT carried across (issue #748).
        //
        // The delegation staging hashed bodies against the delegated SCRIPT
        // bytes, so every extent it holds is in script-relative coordinates.
        // The outer pipeline then runs `attach_body_hashes` again over the
        // whole XML document. Copying a script-relative extent into that pass
        // would have it read XML bytes at script line numbers, which is the
        // same mis-hash this issue is about.
        //
        // Leaving the parent with no body extent for a replayed id is exactly
        // right in both directions. A node the delegation hashed carries its
        // hash on the cloned entry and the second pass skips it. A node the
        // delegation declined, a call-site stub or a body under the minimum
        // length, has no extent for the parent to hash and stays declined.
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

#[cfg(test)]
mod tests {
    use super::*;

    use sqry_core::graph::node::{Language, Span};
    use sqry_core::graph::unified::build::helper::{CalleeKindHint, GraphBuildHelper};

    /// Issue #748: the parent's second `attach_body_hashes` pass, over XML
    /// bytes, must not fingerprint anything the delegated pass declined.
    ///
    /// Both halves matter. A real declaration keeps the hash the delegated
    /// pass computed against SCRIPT bytes, unchanged. A call-site stub, which
    /// the delegated pass declined, stays declined, rather than being hashed
    /// against XML bytes at script-relative line numbers.
    #[test]
    fn the_parent_xml_pass_rehashes_nothing_the_script_pass_declined() {
        let script = "function outer() {\n  helper(1);\n}\n";

        let mut del_staging = StagingGraph::new();
        let (declared_id, stub_id) = {
            let mut helper = GraphBuildHelper::new(
                &mut del_staging,
                std::path::Path::new("record.js"),
                Language::JavaScript,
            );
            let declaration = Span::new(
                sqry_core::graph::node::Position { line: 0, column: 0 },
                sqry_core::graph::node::Position { line: 2, column: 1 },
            );
            let declared_id = helper.add_function("outer", Some(declaration), false, false);
            let call_site = Span::new(
                sqry_core::graph::node::Position { line: 1, column: 2 },
                sqry_core::graph::node::Position {
                    line: 1,
                    column: 11,
                },
            );
            let stub_id = helper.ensure_callee("helper", call_site, CalleeKindHint::Function);
            (declared_id, stub_id)
        };
        assert!(del_staging.declaration_span(declared_id).is_some());
        assert!(
            del_staging.declaration_span(stub_id).is_none(),
            "a call-site stub owns no body extent"
        );
        del_staging.attach_body_hashes(script.as_bytes(), None);
        let script_hash = del_staging
            .nodes()
            .find(|n| n.expected_id == Some(declared_id))
            .expect("declaration is staged")
            .entry
            .body_hash;
        assert!(
            script_hash.is_some(),
            "the declaration hashes over the script"
        );

        let mut main_staging = StagingGraph::new();
        // Stage a filler node FIRST so the parent's id space is already advanced.
        // Without it the replayed nodes reuse indices 0 and 1, the same ids the
        // delegated nodes had, and the remap would not be under test.
        {
            let mut helper = GraphBuildHelper::new(
                &mut main_staging,
                std::path::Path::new("record.xml"),
                Language::JavaScript,
            );
            helper.add_module("<record>", None);
        }
        let mut state = ReplayState::new(&main_staging);
        state
            .replay(&mut main_staging, &mut del_staging)
            .expect("replay succeeds");

        let replayed: Vec<_> = main_staging.nodes().collect();
        assert_eq!(replayed.len(), 3, "the filler plus both replayed nodes");
        let new_declared_id = replayed[1].expected_id.expect("replayed node has an id");
        let new_stub_id = replayed[2].expected_id.expect("replayed node has an id");
        assert_ne!(
            new_declared_id, declared_id,
            "the replayed ids must differ from the delegated ones, or the remap \
             is not under test"
        );
        assert!(
            main_staging.declaration_span(new_declared_id).is_none()
                && main_staging.declaration_span(new_stub_id).is_none(),
            "script-relative extents must not follow the nodes into a graph that \
             hashes XML bytes"
        );

        // The parent's whole-file pass, over XML bytes this time.
        let xml = "<record><script>function outer() {\n  helper(1);\n}</script></record>\n";
        main_staging.attach_body_hashes(xml.as_bytes(), None);

        let after: Vec<_> = main_staging.nodes().collect();
        assert_eq!(
            after[1].entry.body_hash, script_hash,
            "the declaration keeps the hash computed over the script bytes"
        );
        assert!(
            after[2].entry.body_hash.is_none(),
            "the stub is still declined; hashing it here would read XML bytes at \
             the script's line numbers"
        );
    }
}
