//! Alias derivation for the Phase 2 binding plane.
//!
//! An `AliasEntry` records a single alias declaration (`from_symbol` in the
//! scope resolves to the declaration at `to_symbol`). Wildcard imports
//! (`from module import *`) are represented with `is_wildcard = true` and a
//! sentinel `to_symbol` — the resolver forwards these through the target
//! module's exports rather than the alias table directly.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::unified::build::phase4e_binding::BindingEdgeIndex;
use crate::graph::unified::edge::kind::EdgeKind;
use crate::graph::unified::mutation_target::GraphMutationTarget;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::string::id::StringId;

use super::scope::{ScopeArena, ScopeId};

/// Dense handle into an `AliasTable`.
///
/// The id is the position of the entry in the sorted `entries` slice, so
/// `AliasEntryId(k)` is valid as long as the table hasn't been replaced.
/// `AliasTable::get(id)` performs a single bounds-checked slice index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AliasEntryId(pub u32);

/// One alias declaration.
///
/// Records the scope where the alias is declared, the local name the alias
/// introduces (`from_symbol`), the target name the alias points to
/// (`to_symbol`), the `EdgeId` of the `Imports` edge that produced this
/// entry, and whether this was a wildcard import.
///
/// The `id` field is the position of this entry in the owning `AliasTable`'s
/// sorted slice after `derive_aliases` has run. It is assigned by the sort
/// and must not be mutated by callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasEntry {
    /// Stable dense handle for this entry within its owning `AliasTable`.
    /// Assigned after sorting; equals `AliasEntryId(idx)` where `idx` is the
    /// position of this entry in the sorted entries slice.
    pub id: AliasEntryId,
    /// Scope that contains this alias declaration.
    pub scope: ScopeId,
    /// The local symbol name introduced by the alias (e.g., `baz` from
    /// `use foo::bar as baz`).
    pub from_symbol: StringId,
    /// The qualified target name the alias resolves to (e.g., `foo::bar`).
    ///
    /// Falls back to the local name if `qualified_name` is unset on the
    /// import node. In that case `to_symbol == from_symbol`.
    pub to_symbol: StringId,
    /// `NodeId` of the `Import` node that this entry was derived from.
    pub import_node: NodeId,
    /// Whether this is a wildcard import (`from module import *`).
    pub is_wildcard: bool,
}

/// Content-addressed alias lookup table.
///
/// Entries are sorted by `(scope, from_symbol)` for `O(log n)` binary-search
/// lookup via [`AliasTable::resolve_alias`]. Wildcard entries are included in
/// [`AliasTable::aliases_in`] but are skipped by `resolve_alias` — the
/// resolver dispatches wildcards through the target module's exports.
///
/// # Serialization
///
/// Only `entries` is persisted. The `by_scope` index is derived from
/// `entries` on deserialization and cannot drift out of sync with the entries
/// slice. This keeps the V9 snapshot footprint minimal and eliminates the
/// dual-write hazard.
#[derive(Debug, Clone, Default)]
pub struct AliasTable {
    /// All entries, sorted by `(scope, from_symbol)`.
    entries: Vec<AliasEntry>,
    /// Per-scope range index mapping a `ScopeId` to its `[start, end)` slice
    /// in `entries`. Populated after the entries are sorted; **not** persisted.
    by_scope: HashMap<ScopeId, (u32, u32)>,
}

impl AliasTable {
    /// Creates an empty `AliasTable`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the total number of alias entries.
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
    pub fn entries(&self) -> &[AliasEntry] {
        &self.entries
    }

    /// Looks up an alias entry by its dense `AliasEntryId`.
    ///
    /// Returns `None` if `id.0 >= self.len()`. The id is the position of the
    /// entry in the sorted slice, assigned during `derive_aliases`. It is
    /// stable for the lifetime of this table instance.
    ///
    /// Used by `ResolutionStep::FollowAlias { alias: AliasEntryId }` in the
    /// P2U07 resolver to recover the full `AliasEntry` for a recorded step.
    #[must_use]
    pub fn get(&self, id: AliasEntryId) -> Option<&AliasEntry> {
        self.entries.get(id.0 as usize)
    }

    /// Returns all alias entries declared in `scope`, including wildcard
    /// entries. Returns an empty slice when no aliases belong to `scope`.
    #[must_use]
    pub fn aliases_in(&self, scope: ScopeId) -> &[AliasEntry] {
        match self.by_scope.get(&scope) {
            Some(&(start, end)) => &self.entries[start as usize..end as usize],
            None => &[],
        }
    }

    /// Resolves `symbol` within `scope` to its target name.
    ///
    /// Returns `None` when no matching non-wildcard alias exists. Wildcard
    /// entries are deliberately skipped — the resolver dispatches wildcards
    /// through the target module's exports at query time.
    ///
    /// The lookup uses binary search on the sorted `(scope, from_symbol)`
    /// order, so it is `O(log n)` in the number of aliases in `scope`.
    #[must_use]
    pub fn resolve_alias(&self, scope: ScopeId, symbol: StringId) -> Option<StringId> {
        let slice = self.aliases_in(scope);
        // slice is already scope-filtered, so sorting by from_symbol alone is valid.
        slice
            .binary_search_by(|entry| entry.from_symbol.cmp(&symbol))
            .ok()
            .map(|idx| &slice[idx])
            .filter(|entry| !entry.is_wildcard)
            .map(|entry| entry.to_symbol)
    }

    /// Applies `keep` to every entry's `import_node`, dropping entries whose
    /// import-node fails the predicate.
    ///
    /// After filtering, the `id` of each surviving entry is reassigned to its
    /// new position in the sorted `entries` slice, and the `by_scope` range
    /// index is rebuilt from scratch so `resolve_alias` / `aliases_in` stay
    /// consistent. The relative order of surviving entries is preserved, so
    /// the table remains sorted by `(scope, from_symbol)`.
    ///
    /// This is the mutation entry point used by the Gate 0c `NodeIdBearing`
    /// impl (A2 §K row K.A12). Callers that hold the table behind an `Arc`
    /// must reach it through `Arc::make_mut` before invoking this method.
    ///
    /// `#[allow(dead_code)]` mirrors the NodeIdBearing trait itself: Gate 0b
    /// lands the scaffolding and unit tests, Gate 0c adds the production
    /// call site in `RebuildGraph::finalize()`.
    #[allow(dead_code)]
    pub(crate) fn retain_by_node(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.entries.retain(|entry| keep(entry.import_node));
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            entry.id = AliasEntryId(u32::try_from(idx).expect("alias count fits u32"));
        }
        self.rebuild_index();
    }

    /// Rewrite every `StringId` stored on every entry through `remap`,
    /// replacing any ID that appears as a key with its canonical value.
    ///
    /// This is the mutation entry point used by Gate 0c's finalize step 1
    /// (StringId canonicalisation). `from_symbol` is part of the
    /// `(scope, from_symbol)` sort key, so after rewrite the entries are
    /// re-sorted by `(scope, from_symbol)` and `by_scope` is rebuilt so
    /// `resolve_alias` / `aliases_in` keep returning consistent results.
    ///
    /// The relative order within a `(scope, from_symbol)` group is
    /// preserved by using a stable sort. Empty `remap` is a no-op.
    ///
    /// Callers that hold the table behind an `Arc` must reach it through
    /// `Arc::make_mut` before invoking this method.
    #[allow(dead_code)]
    pub(crate) fn rewrite_string_ids_through_remap(
        &mut self,
        remap: &std::collections::HashMap<StringId, StringId>,
    ) {
        if remap.is_empty() {
            return;
        }
        let mut changed = false;
        for entry in &mut self.entries {
            if let Some(&canon) = remap.get(&entry.from_symbol) {
                entry.from_symbol = canon;
                changed = true;
            }
            if let Some(&canon) = remap.get(&entry.to_symbol) {
                entry.to_symbol = canon;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        // `from_symbol` may have collapsed onto a different canonical ID,
        // so re-establish the (scope, from_symbol) sort order and rebuild
        // the scope range index. Use a stable sort to preserve the prior
        // order within each (scope, from_symbol) group.
        self.entries
            .sort_by(|a, b| (a.scope, a.from_symbol).cmp(&(b.scope, b.from_symbol)));
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            entry.id = AliasEntryId(u32::try_from(idx).expect("alias count fits u32"));
        }
        self.rebuild_index();
    }

    /// Rebuilds the `by_scope` range index from `self.entries`.
    ///
    /// Called after deserialization (where `by_scope` is not persisted) and
    /// after constructing an `AliasTable` from a raw `Vec<AliasEntry>`. The
    /// entries must already be sorted by `(scope, from_symbol)`.
    fn rebuild_index(&mut self) {
        debug_assert!(
            self.entries.windows(2).all(|w| w[0].scope <= w[1].scope),
            "rebuild_index requires entries sorted by scope ascending"
        );
        self.by_scope.clear();
        let mut cursor = 0u32;
        while (cursor as usize) < self.entries.len() {
            let scope = self.entries[cursor as usize].scope;
            let start = cursor;
            while (cursor as usize) < self.entries.len()
                && self.entries[cursor as usize].scope == scope
            {
                cursor += 1;
            }
            self.by_scope.insert(scope, (start, cursor));
        }
    }
}

// ---------------------------------------------------------------------------
// Serde: persist only `entries`; rebuild `by_scope` on deserialize.
// ---------------------------------------------------------------------------

impl Serialize for AliasTable {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize as a newtype wrapping just the entries Vec.
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AliasTable {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = Vec::<AliasEntry>::deserialize(deserializer)?;
        let mut table = AliasTable {
            entries,
            by_scope: HashMap::new(),
        };
        table.rebuild_index();
        Ok(table)
    }
}

/// Iterates every `Import` node via the `by_kind` index and, for each one,
/// looks up its incoming `Imports` edges from the pre-built
/// `BindingEdgeIndex::imports_by_target` map (O(1) per node). The source of
/// each such edge is the importing scope's node; the enclosing scope is
/// resolved via the precomputed `node_to_scope` map built from
/// `ScopeArena::iter()`.
///
/// For `use foo::bar as baz;` the `alias` field on the edge carries `baz` and
/// the import node's `qualified_name` carries `foo::bar`. For
/// `from module import *`, `alias` is `None`, `is_wildcard` is `true`, and the
/// import node's name is the sentinel `*`.
///
/// When the import node's `qualified_name` is `None`, `to_symbol` falls back
/// to the plain `name` field — i.e., `to_symbol == from_symbol`.
///
/// Returns `(AliasTable, aliases_with_invalid_scope)` where the second value
/// is the number of `Import` nodes whose enclosing scope could not be resolved.
///
/// # Performance
///
/// The `edge_index` parameter replaces the per-import `edges_to()` calls that
/// previously triggered O(E) delta scans, turning per-node edge lookup from
/// O(E) to O(1).
pub(crate) fn derive_aliases<G: GraphMutationTarget>(
    graph: &G,
    scopes: &ScopeArena,
    edge_index: &BindingEdgeIndex,
) -> (AliasTable, u64) {
    // Build a NodeId → ScopeId map for O(1) enclosing-scope lookup.
    // Uses ScopeArena::iter() so each slot's real generation is used; avoids
    // the generation=1 hardcode that a manual 0..slot_count loop would require.
    let node_to_scope = build_node_to_scope_map(scopes);

    let mut raw: Vec<AliasEntry> = Vec::new();
    let mut invalid_scope_count: u64 = 0;

    // Iterate every Import node and read its incoming Imports edges from
    // the pre-built index (O(1) per node). Imports edges point FROM the
    // importing scope/module TO the import node.
    for &import_node_id in graph.indices().by_kind(NodeKind::Import) {
        let Some(import_entry) = graph.nodes().get(import_node_id) else {
            continue;
        };

        // Look up pre-indexed incoming Imports edges for this import node.
        let Some(incoming) = edge_index.imports_by_target.get(&import_node_id) else {
            continue;
        };

        for (source_node_id, edge_kind) in incoming {
            let EdgeKind::Imports { alias, is_wildcard } = edge_kind else {
                continue;
            };

            // Resolve the enclosing scope.
            let scope = match node_to_scope.get(source_node_id).copied() {
                Some(id) => id,
                None => {
                    // The source node has no corresponding scope. Emit the
                    // entry with INVALID scope and count it for observability.
                    invalid_scope_count += 1;
                    ScopeId::INVALID
                }
            };

            // `alias` holds the local name (e.g., `baz` in `use foo::bar as baz`).
            // When no alias is given, use the import node's own name.
            let from_symbol = alias.unwrap_or(import_entry.name);

            // The target qualified name comes from the import node's qualified_name
            // field, falling back to the plain name if not set — i.e., to_symbol
            // equals from_symbol in the no-qualified-name case.
            let to_symbol = import_entry.qualified_name.unwrap_or(import_entry.name);

            // Placeholder id; will be overwritten after sorting.
            raw.push(AliasEntry {
                id: AliasEntryId(0),
                scope,
                from_symbol,
                to_symbol,
                import_node: import_node_id,
                is_wildcard: *is_wildcard,
            });
        }
    }

    // Sort by (scope, from_symbol) to enable binary-search lookup.
    raw.sort_by(|a, b| (a.scope, a.from_symbol).cmp(&(b.scope, b.from_symbol)));

    // Assign stable ids: the position in the sorted slice IS the AliasEntryId.
    for (idx, entry) in raw.iter_mut().enumerate() {
        entry.id = AliasEntryId(u32::try_from(idx).expect("alias count fits u32"));
    }

    // Build the per-scope range index via the shared helper so the logic lives
    // in exactly one place (deserialization also calls rebuild_index).
    let mut table = AliasTable {
        entries: raw,
        by_scope: HashMap::new(),
    };
    table.rebuild_index();

    (table, invalid_scope_count)
}

/// Builds a `HashMap<NodeId, ScopeId>` from the scope arena so that
/// `derive_aliases` can resolve the enclosing scope for any node in O(1).
///
/// Uses `ScopeArena::iter()` so each slot's real generation is used and the
/// generation=1 hardcode that a manual `0..slot_count` loop would require is
/// avoided.
fn build_node_to_scope_map(scopes: &ScopeArena) -> HashMap<NodeId, ScopeId> {
    scopes.iter().map(|(id, scope)| (scope.node, id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::string::id::StringId;

    fn make_scope(index: u32) -> ScopeId {
        ScopeId::new(index, 1)
    }

    fn make_node() -> NodeId {
        NodeId::new(0, 1)
    }

    fn make_entry(scope: ScopeId, from: u32, to: u32, wildcard: bool) -> AliasEntry {
        AliasEntry {
            id: AliasEntryId(0), // patched by derive_aliases; set manually in unit tests
            scope,
            from_symbol: StringId::new(from),
            to_symbol: StringId::new(to),
            import_node: make_node(),
            is_wildcard: wildcard,
        }
    }

    /// Build an `AliasTable` directly from a pre-sorted entries slice (for
    /// unit tests that don't want to go through `derive_aliases`).
    fn table_from_sorted(mut entries: Vec<AliasEntry>) -> AliasTable {
        for (idx, e) in entries.iter_mut().enumerate() {
            e.id = AliasEntryId(u32::try_from(idx).unwrap());
        }
        let mut table = AliasTable {
            entries,
            by_scope: HashMap::new(),
        };
        table.rebuild_index();
        table
    }

    #[test]
    fn empty_table_lookup_returns_none() {
        let table = AliasTable::new();
        assert!(table.is_empty());
        let scope = make_scope(0);
        assert!(table.aliases_in(scope).is_empty());
        assert_eq!(table.resolve_alias(scope, StringId::new(0)), None);
    }

    #[test]
    fn binary_search_resolve_alias() {
        let scope = make_scope(0);
        let entries = vec![
            make_entry(scope, 10, 100, false),
            make_entry(scope, 20, 200, false),
        ];
        let table = table_from_sorted(entries);
        assert_eq!(
            table.resolve_alias(scope, StringId::new(10)),
            Some(StringId::new(100))
        );
        assert_eq!(
            table.resolve_alias(scope, StringId::new(20)),
            Some(StringId::new(200))
        );
        assert_eq!(table.resolve_alias(scope, StringId::new(30)), None);
    }

    #[test]
    fn wildcard_skipped_by_resolve() {
        let scope = make_scope(0);
        let entries = vec![make_entry(scope, 5, 99, true)];
        let table = table_from_sorted(entries);
        assert_eq!(table.resolve_alias(scope, StringId::new(5)), None);
        assert!(
            table.aliases_in(scope).iter().any(|e| e.is_wildcard),
            "wildcard entry visible via aliases_in"
        );
    }

    #[test]
    fn get_by_alias_entry_id_round_trips() {
        let scope = make_scope(0);
        let entries = vec![
            make_entry(scope, 10, 100, false),
            make_entry(scope, 20, 200, false),
        ];
        let table = table_from_sorted(entries);

        // Ids are assigned by position after sort.
        let e0 = table.get(AliasEntryId(0)).expect("id 0 must exist");
        let e1 = table.get(AliasEntryId(1)).expect("id 1 must exist");

        assert_eq!(e0.from_symbol, StringId::new(10));
        assert_eq!(e0.to_symbol, StringId::new(100));
        assert_eq!(e0.id, AliasEntryId(0));

        assert_eq!(e1.from_symbol, StringId::new(20));
        assert_eq!(e1.to_symbol, StringId::new(200));
        assert_eq!(e1.id, AliasEntryId(1));

        assert!(table.get(AliasEntryId(2)).is_none());
    }

    #[test]
    fn entries_accessor_returns_full_slice() {
        let scope = make_scope(0);
        let entries = vec![
            make_entry(scope, 1, 10, false),
            make_entry(scope, 2, 20, false),
            make_entry(scope, 3, 30, true),
        ];
        let table = table_from_sorted(entries);
        assert_eq!(table.entries().len(), 3);
    }

    #[test]
    fn postcard_round_trip_preserves_entries_and_rebuilds_by_scope() {
        let s_a = make_scope(1);
        let s_b = make_scope(2);
        let entries = vec![
            make_entry(s_a, 10, 100, false),
            make_entry(s_a, 11, 101, false),
            make_entry(s_b, 20, 200, true),
        ];
        let original = table_from_sorted(entries);

        let bytes = postcard::to_allocvec(&original).expect("serialize");
        let restored: AliasTable = postcard::from_bytes(&bytes).expect("deserialize");

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

        // Verify by_scope was rebuilt correctly: resolve_alias must work.
        assert_eq!(
            restored.resolve_alias(s_a, StringId::new(10)),
            Some(StringId::new(100)),
            "resolve_alias on restored table must return correct target for s_a entry 0"
        );
        assert_eq!(
            restored.resolve_alias(s_a, StringId::new(11)),
            Some(StringId::new(101)),
            "resolve_alias on restored table must return correct target for s_a entry 1"
        );
        // Wildcard entry in s_b is not resolved by resolve_alias.
        assert_eq!(
            restored.resolve_alias(s_b, StringId::new(20)),
            None,
            "wildcard entry must not be returned by resolve_alias after round-trip"
        );
        assert!(
            restored.aliases_in(s_b).iter().any(|e| e.is_wildcard),
            "wildcard entry in s_b must survive round-trip"
        );
    }

    #[test]
    fn aliases_in_partition_by_scope() {
        let s_a = make_scope(1);
        let s_b = make_scope(2);
        let s_c = make_scope(3);

        let from_x = StringId::new(10);
        let from_y = StringId::new(11);
        let to_1 = StringId::new(20);
        let to_2 = StringId::new(21);

        // Six entries across three scopes, two entries each.
        // Build unsorted and let table_from_sorted sort them so the sort matters.
        let mut entries = vec![
            // s_b entries (will sort after s_a, before s_c by ScopeId ordering)
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_b,
                from_symbol: from_x,
                to_symbol: to_1,
                import_node: make_node(),
                is_wildcard: false,
            },
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_b,
                from_symbol: from_y,
                to_symbol: to_2,
                import_node: make_node(),
                is_wildcard: false,
            },
            // s_c entries
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_c,
                from_symbol: from_x,
                to_symbol: to_2,
                import_node: make_node(),
                is_wildcard: false,
            },
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_c,
                from_symbol: from_y,
                to_symbol: to_1,
                import_node: make_node(),
                is_wildcard: false,
            },
            // s_a entries (will sort first)
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_a,
                from_symbol: from_x,
                to_symbol: to_2,
                import_node: make_node(),
                is_wildcard: false,
            },
            AliasEntry {
                id: AliasEntryId(0),
                scope: s_a,
                from_symbol: from_y,
                to_symbol: to_1,
                import_node: make_node(),
                is_wildcard: false,
            },
        ];
        entries.sort_by(|a, b| (a.scope, a.from_symbol).cmp(&(b.scope, b.from_symbol)));
        let table = table_from_sorted(entries);

        // Each scope must see exactly its own 2 entries.
        let b_entries = table.aliases_in(s_b);
        assert_eq!(b_entries.len(), 2, "s_b must have exactly 2 entries");
        assert!(
            b_entries.iter().all(|e| e.scope == s_b),
            "s_b slice must only contain s_b entries"
        );

        let a_entries = table.aliases_in(s_a);
        assert_eq!(a_entries.len(), 2, "s_a must have exactly 2 entries");
        assert!(
            a_entries.iter().all(|e| e.scope == s_a),
            "s_a slice must only contain s_a entries"
        );

        let c_entries = table.aliases_in(s_c);
        assert_eq!(c_entries.len(), 2, "s_c must have exactly 2 entries");
        assert!(
            c_entries.iter().all(|e| e.scope == s_c),
            "s_c slice must only contain s_c entries"
        );

        // Scope isolation: same from_symbol in different scopes resolves to different targets.
        let resolved_a = table.resolve_alias(s_a, from_x);
        let resolved_b = table.resolve_alias(s_b, from_x);
        assert!(resolved_a.is_some(), "s_a must resolve from_x");
        assert!(resolved_b.is_some(), "s_b must resolve from_x");
        assert_ne!(
            resolved_a, resolved_b,
            "same from_symbol in different scopes must resolve to different targets"
        );
    }
}
