//! Shadow derivation for the Phase 2 binding plane.
//!
//! A `ShadowEntry` names one Variable/Parameter node inside a function-scope
//! chain, ordered by its byte offset. Multiple entries for the same
//! `(scope, symbol)` pair form a shadow chain: the innermost definition
//! whose byte offset is strictly less than a query offset is the effective
//! binding at that offset.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::unified::build::phase4e_binding::BindingEdgeIndex;
use crate::graph::unified::edge::kind::EdgeKind;
use crate::graph::unified::mutation_target::GraphMutationTarget;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::string::id::StringId;

use super::scope::{ScopeArena, ScopeId, ScopeKind};

/// Dense handle into a `ShadowTable`.
///
/// The id is the position of the entry in the sorted `entries` slice, so
/// `ShadowEntryId(k)` is valid as long as the table hasn't been replaced.
/// `ShadowTable::get(id)` performs a single bounds-checked slice index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShadowEntryId(pub u32);

/// One shadow-chain entry.
///
/// Records the function scope that owns the variable, the interned symbol
/// name, the `NodeId` of the defining `Variable` or `Parameter` node, the
/// byte offset of that definition, and a stable dense handle into the owning
/// `ShadowTable`.
///
/// The `id` field is the position of this entry in the sorted entries slice
/// after `derive_shadows` has run. It is assigned by the sort and must not
/// be mutated by callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowEntry {
    /// Stable dense handle for this entry within its owning `ShadowTable`.
    /// Assigned after sorting; equals `ShadowEntryId(idx)` where `idx` is
    /// the position of this entry in the sorted entries slice.
    pub id: ShadowEntryId,
    /// The function scope that contains this definition.
    pub scope: ScopeId,
    /// The interned symbol name of the variable or parameter.
    pub symbol: StringId,
    /// `NodeId` of the `Variable` or `Parameter` node that introduces the
    /// binding at this offset.
    pub node: NodeId,
    /// Byte offset of the definition site within its source file.
    ///
    /// Entries within the same `(scope, symbol)` chain are sorted by this
    /// field in ascending order; `effective_binding` performs a reverse-scan
    /// to find the last entry whose offset is strictly less than the query
    /// offset.
    pub byte_offset: u32,
}

/// Content-addressed shadow lookup table.
///
/// Entries are sorted by `(scope, symbol, byte_offset)`. The `chains` index
/// maps each `(ScopeId, StringId)` key to a half-open range `[start, end)`
/// into `entries`. `effective_binding` does a backward linear scan through
/// the relevant range to find the innermost definition before a query offset.
///
/// # Serialization
///
/// Only `entries` is persisted. The `chains` index is derived from `entries`
/// on deserialization and cannot drift out of sync. This keeps the V9 snapshot
/// footprint minimal and eliminates the dual-write hazard.
#[derive(Debug, Clone, Default)]
pub struct ShadowTable {
    /// All entries, sorted by `(scope, symbol, byte_offset)`.
    entries: Vec<ShadowEntry>,
    /// Per-`(scope, symbol)` range index mapping a `(ScopeId, StringId)` key
    /// to its `[start, end)` slice in `entries`. **Not** persisted.
    chains: HashMap<(ScopeId, StringId), (u32, u32)>,
}

impl ShadowTable {
    /// Creates an empty `ShadowTable`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the total number of shadow entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the table contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the full sorted entries slice.
    ///
    /// Useful for V9 persistence walkers, debug dumps, stats collection, and
    /// any future consumer that needs to iterate the entire table.
    #[must_use]
    pub fn entries(&self) -> &[ShadowEntry] {
        &self.entries
    }

    /// Looks up a shadow entry by its dense `ShadowEntryId`.
    ///
    /// Returns `None` if `id.0 >= self.len()`. The id is the position of the
    /// entry in the sorted slice, assigned during `derive_shadows`. It is
    /// stable for the lifetime of this table instance.
    ///
    /// Used by resolution steps that record a shadow entry by id and later
    /// need to recover the full `ShadowEntry`.
    #[must_use]
    pub fn get(&self, id: ShadowEntryId) -> Option<&ShadowEntry> {
        self.entries.get(id.0 as usize)
    }

    /// Returns all shadow entries declared in `scope`, sorted by
    /// `(symbol, byte_offset)`.
    ///
    /// Returns an empty `Vec` when no entries belong to `scope`. The returned
    /// entries span all symbols in the scope, not a single symbol chain.
    #[must_use]
    pub fn shadows_in(&self, scope: ScopeId) -> Vec<&ShadowEntry> {
        self.entries.iter().filter(|e| e.scope == scope).collect()
    }

    /// Returns the innermost shadow entry for `(scope, symbol)` whose byte
    /// offset is strictly less than `query_byte_offset`.
    ///
    /// Returns `None` when no such entry exists — i.e., the query position is
    /// before the first definition of `symbol` in `scope`, or the `(scope,
    /// symbol)` pair has no entries at all.
    ///
    /// # Shadow semantics
    ///
    /// This implements the Rust/JS/Python shadow semantic: a re-binding at
    /// offset B is visible only to references at offsets > B. The last
    /// definition strictly before the query offset wins.
    #[must_use]
    pub fn effective_binding(
        &self,
        scope: ScopeId,
        symbol: StringId,
        query_byte_offset: u32,
    ) -> Option<NodeId> {
        let (start, end) = self.chains.get(&(scope, symbol)).copied()?;
        let slice = &self.entries[start as usize..end as usize];
        // Reverse scan: the last entry whose byte_offset < query_byte_offset wins.
        slice
            .iter()
            .rev()
            .find(|e| e.byte_offset < query_byte_offset)
            .map(|e| e.node)
    }

    /// Applies `keep` to every entry's `node`, dropping entries whose defining
    /// `Variable`/`Parameter` node fails the predicate.
    ///
    /// After filtering, the `id` of each surviving entry is reassigned to its
    /// new position in the sorted `entries` slice, and the `chains` range
    /// index is rebuilt from scratch so `effective_binding` / `shadows_in`
    /// stay consistent. The relative order of surviving entries is preserved,
    /// so the table remains sorted by `(scope, symbol, byte_offset)`.
    ///
    /// This is the mutation entry point used by the Gate 0c `NodeIdBearing`
    /// impl (A2 §K row K.A13). Callers that hold the table behind an `Arc`
    /// must reach it through `Arc::make_mut` before invoking this method.
    ///
    /// `#[allow(dead_code)]` mirrors the NodeIdBearing trait itself: Gate 0b
    /// lands the scaffolding and unit tests, Gate 0c adds the production
    /// call site in `RebuildGraph::finalize()`.
    #[allow(dead_code)]
    pub(crate) fn retain_by_node(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.entries.retain(|entry| keep(entry.node));
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            entry.id = ShadowEntryId(u32::try_from(idx).expect("shadow count fits u32"));
        }
        self.rebuild_index();
    }

    /// Rewrite every `StringId` stored on every entry through `remap`,
    /// replacing any ID that appears as a key with its canonical value.
    ///
    /// Used by Gate 0c's finalize step 1 (StringId canonicalisation).
    /// `symbol` participates in the `(scope, symbol, byte_offset)` sort
    /// key AND in the `chains: HashMap<(ScopeId, StringId), _>` index, so
    /// after rewrite the entries are re-sorted and the `chains` map is
    /// rebuilt from scratch. A stable sort preserves the prior order
    /// within each `(scope, symbol, byte_offset)` group (only relevant if
    /// two entries collide on all three — legal after dedup but rare).
    ///
    /// Empty `remap` is a no-op. Callers that hold the table behind an
    /// `Arc` must reach it through `Arc::make_mut` first.
    #[allow(dead_code)]
    pub(crate) fn rewrite_string_ids_through_remap(&mut self, remap: &HashMap<StringId, StringId>) {
        if remap.is_empty() {
            return;
        }
        let mut changed = false;
        for entry in &mut self.entries {
            if let Some(&canon) = remap.get(&entry.symbol) {
                entry.symbol = canon;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.entries.sort_by(|a, b| {
            (a.scope, a.symbol, a.byte_offset).cmp(&(b.scope, b.symbol, b.byte_offset))
        });
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            entry.id = ShadowEntryId(u32::try_from(idx).expect("shadow count fits u32"));
        }
        self.rebuild_index();
    }

    /// Rebuilds the `chains` range index from `self.entries`.
    ///
    /// Called after deserialization (where `chains` is not persisted) and
    /// after constructing a `ShadowTable` from a raw `Vec<ShadowEntry>`. The
    /// entries must already be sorted by `(scope, symbol, byte_offset)`.
    fn rebuild_index(&mut self) {
        debug_assert!(
            self.entries
                .windows(2)
                .all(|w| { (w[0].scope, w[0].symbol) <= (w[1].scope, w[1].symbol) }),
            "rebuild_index requires entries sorted by (scope, symbol) ascending"
        );
        self.chains.clear();
        let mut cursor = 0u32;
        while (cursor as usize) < self.entries.len() {
            let key = (
                self.entries[cursor as usize].scope,
                self.entries[cursor as usize].symbol,
            );
            let start = cursor;
            while (cursor as usize) < self.entries.len()
                && (
                    self.entries[cursor as usize].scope,
                    self.entries[cursor as usize].symbol,
                ) == key
            {
                cursor += 1;
            }
            self.chains.insert(key, (start, cursor));
        }
    }
}

// ---------------------------------------------------------------------------
// Serde: persist only `entries`; rebuild `chains` on deserialize.
// ---------------------------------------------------------------------------

impl Serialize for ShadowTable {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize as a newtype wrapping just the entries Vec.
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ShadowTable {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = Vec::<ShadowEntry>::deserialize(deserializer)?;
        let mut table = ShadowTable {
            entries,
            chains: HashMap::default(),
        };
        table.rebuild_index();
        Ok(table)
    }
}

/// Walks `NodeKind::Variable` and `NodeKind::Parameter` nodes whose enclosing
/// scope is a `ScopeKind::Function`, groups them by `(scope, symbol)`, sorts
/// each group by `byte_offset`, assigns stable `ShadowEntryId`s, and builds
/// the `chains` range index.
///
/// Uses `ScopeArena::iter()` to build the node → scope map, avoiding the
/// generation-1 hardcode. Only `Contains`, `Defines` edges feed into scope
/// derivation; `derive_shadows` operates on the already-finalized scope arena
/// and never walks cross-language edges.
///
/// Returns an empty `ShadowTable` when no Variable/Parameter nodes belong to
/// any Function scope.
///
/// # Performance
///
/// The `edge_index` parameter replaces the per-function `edges_from()` calls
/// that previously triggered O(E) delta scans, turning per-node edge lookup
/// from O(E) to O(1).
pub(crate) fn derive_shadows<G: GraphMutationTarget>(
    graph: &G,
    scopes: &ScopeArena,
    edge_index: &BindingEdgeIndex,
) -> ShadowTable {
    // Build NodeId → ScopeId for every scope whose kind is Function.
    // Reuse the same node_to_scope approach as derive_aliases but then filter
    // to Function-kind scopes.
    let node_to_function_scope: HashMap<NodeId, ScopeId> = scopes
        .iter()
        .filter(|(_, scope)| scope.kind == ScopeKind::Function)
        .map(|(id, scope)| (scope.node, id))
        .collect();

    // For nodes that are contained within a function (i.e., their direct
    // parent is the function node), we need to know which scope corresponds
    // to which function node. We already have that in node_to_function_scope
    // (function_node → scope_id). We need to find which function scope
    // contains each Variable/Parameter.
    //
    // Strategy: for each function node find its `Defines`/`Contains` children
    // that are Variable/Parameter nodes using the pre-built forward index.
    // This is correct for single-level nesting (typical for let-bindings and
    // parameters) and avoids the O(E) delta scan that edges_from() would
    // trigger.

    let mut raw: Vec<ShadowEntry> = Vec::new();

    for (&func_node, &scope_id) in &node_to_function_scope {
        // Look up pre-indexed outgoing Defines/Contains edges from the function node.
        let Some(children) = edge_index.defines_contains_by_source.get(&func_node) else {
            continue;
        };

        for (child, edge_kind) in children {
            if !matches!(edge_kind, EdgeKind::Defines | EdgeKind::Contains) {
                continue;
            }
            let Some(child_entry) = graph.nodes().get(*child) else {
                continue;
            };
            if !matches!(child_entry.kind, NodeKind::Variable | NodeKind::Parameter) {
                continue;
            }
            raw.push(ShadowEntry {
                // Placeholder id; overwritten after sorting.
                id: ShadowEntryId(0),
                scope: scope_id,
                symbol: child_entry.name,
                node: *child,
                byte_offset: child_entry.start_byte,
            });
        }
    }

    if raw.is_empty() {
        log::debug!("derive_shadows: no Variable/Parameter nodes under Function scopes");
        return ShadowTable::new();
    }

    // Sort by (scope, symbol, byte_offset) for stable chain partitioning.
    raw.sort_by(|a, b| (a.scope, a.symbol, a.byte_offset).cmp(&(b.scope, b.symbol, b.byte_offset)));

    // Assign stable ids: position in the sorted slice IS the ShadowEntryId.
    for (idx, entry) in raw.iter_mut().enumerate() {
        entry.id = ShadowEntryId(u32::try_from(idx).expect("shadow count fits u32"));
    }

    // Build the per-(scope, symbol) range index via rebuild_index so the
    // logic lives in exactly one place (deserialization also calls it).
    let mut table = ShadowTable {
        entries: raw,
        chains: HashMap::default(),
    };
    table.rebuild_index();

    log::debug!(
        "derive_shadows: derived {} shadow entries across {} chains",
        table.len(),
        table.chains.len(),
    );

    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::string::id::StringId;

    fn make_scope(index: u32) -> ScopeId {
        ScopeId::new(index, 1)
    }

    /// Build a `ShadowTable` directly from a pre-sorted entries slice (for
    /// unit tests that don't want to go through `derive_shadows`).
    fn table_from_sorted(mut entries: Vec<ShadowEntry>) -> ShadowTable {
        for (idx, e) in entries.iter_mut().enumerate() {
            e.id = ShadowEntryId(u32::try_from(idx).unwrap());
        }
        let mut table = ShadowTable {
            entries,
            chains: HashMap::default(),
        };
        table.rebuild_index();
        table
    }

    fn make_entry(scope: ScopeId, symbol: StringId, node: NodeId, byte_offset: u32) -> ShadowEntry {
        ShadowEntry {
            id: ShadowEntryId(0), // patched by table_from_sorted
            scope,
            symbol,
            node,
            byte_offset,
        }
    }

    #[test]
    fn t12_effective_binding_innermost_before_offset() {
        let scope = make_scope(0);
        let symbol = StringId::new(1);
        let entries = vec![
            make_entry(scope, symbol, NodeId::new(1, 1), 10),
            make_entry(scope, symbol, NodeId::new(2, 1), 30),
        ];
        let table = table_from_sorted(entries);

        // Query at offset 40 — both definitions are before, innermost is node 2.
        assert_eq!(
            table.effective_binding(scope, symbol, 40),
            Some(NodeId::new(2, 1)),
            "innermost definition before offset 40 must be node 2 (at offset 30)"
        );
        // Query at offset 20 — only first definition is before.
        assert_eq!(
            table.effective_binding(scope, symbol, 20),
            Some(NodeId::new(1, 1)),
            "only node 1 (offset 10) is before offset 20"
        );
    }

    #[test]
    fn t13_effective_binding_before_first_returns_none() {
        let scope = make_scope(0);
        let symbol = StringId::new(1);
        let entries = vec![make_entry(scope, symbol, NodeId::new(1, 1), 50)];
        let table = table_from_sorted(entries);

        // Query at offset 10 — before the first definition.
        assert_eq!(
            table.effective_binding(scope, symbol, 10),
            None,
            "query before first definition must return None"
        );
    }

    #[test]
    fn empty_table_behavior() {
        let table = ShadowTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        let scope = make_scope(0);
        let symbol = StringId::new(1);
        assert!(table.shadows_in(scope).is_empty());
        assert_eq!(table.effective_binding(scope, symbol, 100), None);
        assert!(table.get(ShadowEntryId(0)).is_none());
    }

    #[test]
    fn get_by_shadow_entry_id_round_trips() {
        let scope = make_scope(0);
        let sym_a = StringId::new(1);
        let sym_b = StringId::new(2);
        let entries = vec![
            make_entry(scope, sym_a, NodeId::new(1, 1), 10),
            make_entry(scope, sym_b, NodeId::new(2, 1), 20),
        ];
        let table = table_from_sorted(entries);

        let e0 = table.get(ShadowEntryId(0)).expect("id 0 must exist");
        let e1 = table.get(ShadowEntryId(1)).expect("id 1 must exist");
        assert_eq!(e0.id, ShadowEntryId(0));
        assert_eq!(e0.symbol, sym_a);
        assert_eq!(e1.id, ShadowEntryId(1));
        assert_eq!(e1.symbol, sym_b);
        assert!(table.get(ShadowEntryId(2)).is_none());
    }

    #[test]
    fn entries_accessor_returns_full_slice() {
        let scope = make_scope(0);
        let symbol = StringId::new(1);
        let entries = vec![
            make_entry(scope, symbol, NodeId::new(1, 1), 5),
            make_entry(scope, symbol, NodeId::new(2, 1), 15),
            make_entry(scope, symbol, NodeId::new(3, 1), 25),
        ];
        let table = table_from_sorted(entries);
        assert_eq!(table.entries().len(), 3);
    }

    #[test]
    fn scope_partitioning_no_collision() {
        // Two scopes each declaring the same symbol name must not collide.
        let s_a = make_scope(0);
        let s_b = make_scope(1);
        let sym = StringId::new(42);

        let entries = vec![
            make_entry(s_a, sym, NodeId::new(10, 1), 5),
            make_entry(s_a, sym, NodeId::new(11, 1), 20),
            make_entry(s_b, sym, NodeId::new(20, 1), 5),
            make_entry(s_b, sym, NodeId::new(21, 1), 30),
        ];
        let table = table_from_sorted(entries);

        // s_a at offset 25 → should return node 11 (offset 20 < 25).
        assert_eq!(
            table.effective_binding(s_a, sym, 25),
            Some(NodeId::new(11, 1)),
            "s_a effective binding at 25 must be node 11"
        );
        // s_b at offset 25 → should return node 20 (offset 5 < 25, 30 ≥ 25).
        assert_eq!(
            table.effective_binding(s_b, sym, 25),
            Some(NodeId::new(20, 1)),
            "s_b effective binding at 25 must be node 20"
        );
        // s_a and s_b resolve to different nodes for the same symbol and offset
        assert_ne!(
            table.effective_binding(s_a, sym, 25),
            table.effective_binding(s_b, sym, 25),
            "same symbol in different scopes must resolve independently"
        );
    }

    #[test]
    fn postcard_round_trip_preserves_entries_and_rebuilds_chains() {
        let s_a = make_scope(1);
        let s_b = make_scope(2);
        let sym_x = StringId::new(10);
        let sym_y = StringId::new(11);

        let entries = vec![
            make_entry(s_a, sym_x, NodeId::new(1, 1), 10),
            make_entry(s_a, sym_x, NodeId::new(2, 1), 30),
            make_entry(s_b, sym_y, NodeId::new(3, 1), 5),
        ];
        let original = table_from_sorted(entries);

        let bytes = postcard::to_allocvec(&original).expect("serialize");
        let restored: ShadowTable = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(
            restored.entries().len(),
            original.entries().len(),
            "entry count must survive round-trip"
        );
        assert_eq!(
            restored.entries(),
            original.entries(),
            "entries must be byte-equal after round-trip"
        );

        // Verify chains were rebuilt correctly.
        assert_eq!(
            restored.effective_binding(s_a, sym_x, 40),
            Some(NodeId::new(2, 1)),
            "effective_binding on restored table must find node 2 for s_a/sym_x at 40"
        );
        assert_eq!(
            restored.effective_binding(s_a, sym_x, 20),
            Some(NodeId::new(1, 1)),
            "effective_binding on restored table must find node 1 for s_a/sym_x at 20"
        );
        assert_eq!(
            restored.effective_binding(s_b, sym_y, 10),
            Some(NodeId::new(3, 1)),
            "effective_binding on restored table must find node 3 for s_b/sym_y at 10"
        );
        assert_eq!(
            restored.effective_binding(s_b, sym_y, 3),
            None,
            "query before first def must return None after round-trip"
        );
    }
}
