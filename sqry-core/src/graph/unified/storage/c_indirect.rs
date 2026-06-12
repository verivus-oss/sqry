//! C indirect-call precision side tables (Phase A, U09).
//!
//! This module hosts [`CIndirectSideTables`], the consolidated side-table
//! substrate consumed by:
//!
//! * **U10** — C plugin Phase 1 instrumentation (populates the staging
//!   payloads),
//! * **U11** — Phase 3/4 plumbing (merges per-file staging into the
//!   workspace-global tables),
//! * **U12** — `pass5b_c_indirect_resolve` (consumes the side tables to
//!   rewrite synthetic `Calls` edges into precise binding-plane / type-match
//!   candidates), and
//! * **U13** — sqry-db derived queries over address-taken + cap-exceeded
//!   callsite metadata.
//!
//! # Design contract
//!
//! These are **side tables**, not edges. The hot edge arena stays
//! cache-friendly; query consumers (resolver, derived queries) live at
//! side-table O(1) lookup cost.
//!
//! The DESIGN contract for the storage shape lives in
//! `docs/development/c-semantic-phase-a-icall-precision/02_DESIGN-...`
//! §3.7 (top-level tables), §4.2 (`IndirectCallsite` / `IndirectShape`),
//! §7.1 (binding plane), and §10.2 (wire format).
//!
//! # Wire-format invariants (DESIGN §10.2)
//!
//! The `c_indirect_tables` slot inside the V11 snapshot envelope is typed
//! as `Option<CIndirectSideTables>`. The 1-byte minimum applies to the
//! `Option`'s `None` discriminant, **not** to an allocated-but-empty
//! `CIndirectSideTables` whose inner maps and vecs each contribute their
//! own length prefix. V10 → V11 upconvert sets the slot to `None`.
//!
//! # `LocalScopeIndex` location
//!
//! The block-scope arena originally landed in `sqry-lang-c::relations::scope_index`
//! (U08). Because `CIndirectSideTables` lives in `sqry-core` and U12's
//! resolver (also in `sqry-core`) needs to perform `resolve_type` lookups,
//! the *storage shape + lookup* moved to this module as the
//! [`scope_index`] submodule. The tree-sitter Builder remains in
//! `sqry-lang-c` and constructs the sqry-core type via
//! [`LocalScopeIndex::from_parts`]. The `sqry-lang-c` symbol is now a
//! re-export of this module's type.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::string::StringId;

pub mod scope_index;

pub use scope_index::{LocalDeclaration, LocalScopeIndex, ScopeEntry};

// ---------------------------------------------------------------------------
// Top-level side-tables container (DESIGN §3.7)
// ---------------------------------------------------------------------------

/// Workspace-global side tables consumed by the C indirect-call resolver.
///
/// Populated incrementally by the C plugin's Phase 1 walkers (U10), drained
/// into this container during Phase 3 commit + Phase 4d edge insertion (U11),
/// and consumed by `pass5b_c_indirect_resolve` (U12).
///
/// All six fields are independently populated; non-C workspaces never
/// allocate this struct (the parent slot on `CodeGraph` is
/// `Option<CIndirectSideTables>` and stays `None`).
///
/// # Fields
///
/// * `fn_signature` — every C `Function`/`Method` node's canonical
///   signature token id (§3.1 grammar). Population point: U10's
///   `process_struct_fields` / function-definition walker.
/// * `struct_field_fnptr` — function-pointer field signatures keyed by
///   `(struct_qn, field_name)`. Required for `obj->field(...)` resolution
///   (§4.2 case B).
/// * `local_var_type` — every local `Variable` node's canonical type
///   token. Required for `(*fp)(...)` / `fp(...)` resolution where `fp`
///   is a function-pointer-typed local.
/// * `local_scope_indices` — per-`FileId` block-scope arena (DESIGN §4.1).
///   The resolver looks up an identifier at a use-site byte offset and
///   walks the parent-scope chain innermost-out.
/// * `bindings_by_field` — `(struct_qn, field_name) → Vec<BindingEntry>`
///   recording every initializer site that assigns a function to that
///   slot of any instance. The binding plane (§7.1) is the resolver's
///   first-tier evidence stream.
/// * `pending_callsites` — every indirect callsite captured during Phase 1.
///   The resolver iterates this vec in `pass5b_c_indirect_resolve` and
///   emits new precise `Calls` edges per resolved candidate.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CIndirectSideTables {
    /// `fn_signature[node_id] = canonical signature StringId`.
    ///
    /// Populated for every C `Function`/`Method` node. Empty for non-C.
    pub fn_signature: HashMap<NodeId, StringId>,

    /// `struct_field_fnptr[(struct_qn, field_name)] = canonical signature StringId`.
    ///
    /// Populated only for fields whose declared type is a function pointer.
    /// `struct_qn` is the canonical struct tag (e.g. `struct file_operations`)
    /// produced by §3.1 grammar.
    pub struct_field_fnptr: HashMap<(StringId, StringId), StringId>,

    /// `local_var_type[NodeId of the local Variable node] = canonical type StringId`.
    ///
    /// "canonical type" here is *not* a function signature — it's the type
    /// token (e.g. `"struct file_operations*"`, `"int (*)(int)"`) needed
    /// to resolve the receiver of an indirect call. Function-pointer-typed
    /// locals get the same signature token they'd produce as a function
    /// (re-used via §3.1 grammar).
    pub local_var_type: HashMap<NodeId, StringId>,

    /// Per-file block-scope arenas (DESIGN §4.1).
    ///
    /// Keyed by `FileId` because C local scope is intra-procedural — no
    /// cross-file lookups. The resolver uses
    /// [`LocalScopeIndex::resolve_type`] to map an identifier-at-offset
    /// back to its source-level type token, which then routes through
    /// §3.1's canonical signature grammar.
    pub local_scope_indices: HashMap<FileId, LocalScopeIndex>,

    /// `(struct_qn, field_name) → Vec<BindingEntry>` — binding plane (§7.1).
    ///
    /// Every designated-initializer or positional-initializer site that
    /// assigns a function to a struct slot contributes one entry. The
    /// resolver (§4.2 step 2) looks up this map first; only if the result
    /// is empty or exceeds the cardinality cap does it fall back to the
    /// type-signature path.
    pub bindings_by_field: HashMap<(StringId, StringId), Vec<BindingEntry>>,

    /// Every indirect callsite captured during Phase 1 parse.
    ///
    /// The resolver iterates this vec in `pass5b_c_indirect_resolve` and
    /// rewrites each callsite's synthetic `Calls` edge into a precise
    /// candidate set. Retained on the side tables (DESIGN §4.3) so that
    /// `CALLSITE_PROMISCUOUS` callers can have their cap-exceeded
    /// candidate count re-derived on demand.
    pub pending_callsites: Vec<IndirectCallsite>,
}

impl CIndirectSideTables {
    /// Returns a freshly-allocated, empty side-table container.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when every inner table is empty.
    ///
    /// Useful for build-pipeline assertions and for keeping non-C workspaces
    /// from ever allocating the parent `Option<CIndirectSideTables>` slot
    /// on `CodeGraph` as `Some(_)`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fn_signature.is_empty()
            && self.struct_field_fnptr.is_empty()
            && self.local_var_type.is_empty()
            && self.local_scope_indices.is_empty()
            && self.bindings_by_field.is_empty()
            && self.pending_callsites.is_empty()
    }

    /// Look up the block-scope arena for `file_id`.
    ///
    /// Returns `None` for non-C files or for C files that produced no
    /// scopes (empty translation units). The resolver (§4.2) treats a
    /// `None` here identically to a `resolve_type` miss — it falls back
    /// through the synthetic-stub edge.
    #[inline]
    #[must_use]
    pub fn scope_index_for(&self, file_id: FileId) -> Option<&LocalScopeIndex> {
        self.local_scope_indices.get(&file_id)
    }
}

// ---------------------------------------------------------------------------
// Binding-plane entries (DESIGN §7.1)
// ---------------------------------------------------------------------------

/// One binding-plane entry: a function-valued initializer slot inside a
/// struct instance.
///
/// Stored under the key `(struct_qn, field_name)` in
/// [`CIndirectSideTables::bindings_by_field`]. Cross-file queries work
/// because every file's bindings are merged into the same table during
/// Phase 4d (DESIGN §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    /// The ops-table variable that holds this binding
    /// (e.g. `ext4_file_operations`).
    pub instance_node: NodeId,
    /// The address-taken function that the binding targets
    /// (e.g. `ext4_file_read_iter`).
    pub target_fn: NodeId,
    /// Whether the binding was introduced via a designated or positional
    /// initializer (DESIGN §2.5 / §7.1).
    pub site_kind: BindingSiteKind,
}

/// Kind of initializer site that introduced a [`BindingEntry`].
///
/// Designated initializers (`.field = fn`) and positional initializers
/// (`{ fn1, fn2, ... }`) are both captured. The kind is preserved for
/// downstream tooling (e.g. provenance / future planner predicates) — the
/// resolver itself treats them uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindingSiteKind {
    /// `.field_name = function_name` — designated initializer.
    DesignatedInitializer,
    /// Positional initializer: the function appears at a known offset in
    /// a positional aggregate initializer (e.g. `{ fn1, fn2 }`).
    PositionalInitializer,
}

// ---------------------------------------------------------------------------
// Indirect callsites (DESIGN §4.2)
// ---------------------------------------------------------------------------

/// One captured indirect call site, queued for resolution by
/// `pass5b_c_indirect_resolve`.
///
/// `caller` is the `NodeId` of the enclosing C function / method (the
/// caller-side anchor of the synthetic `Calls` edge captured in Phase 1).
/// `file_id` and `use_span` locate the callsite for `LocalScopeIndex`
/// lookups; the resolver uses `shape` to dispatch between the
/// pointer-expression and field-expression resolution paths.
///
/// `argument_count` and `is_async` are preserved from the original Phase 1
/// `Calls` edge so the resolver can re-stamp them on the rewritten precise
/// edges (DESIGN §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndirectCallsite {
    /// The enclosing function/method node that issued this call.
    pub caller: NodeId,
    /// The file in which the callsite appears.
    pub file_id: FileId,
    /// Byte range of the callsite expression. The `.0` byte is used as
    /// the use-site offset by [`LocalScopeIndex::resolve_type`].
    pub use_span: (usize, usize),
    /// Whether the syntactic shape is a pointer-expression call
    /// (`(*fp)(...)` / `fp(...)`) or a field-expression call
    /// (`obj.field(...)` / `obj->field(...)`).
    pub shape: IndirectShape,
    /// Argument count carried from the original Phase 1 `Calls` edge —
    /// re-stamped on each rewritten precise edge.
    pub argument_count: u32,
    /// `is_async` carried from the original Phase 1 `Calls` edge —
    /// re-stamped on each rewritten precise edge. C calls are typically
    /// synchronous; the field exists for parity with `EdgeKind::Calls`.
    pub is_async: bool,
}

/// Syntactic shape of an indirect callsite (DESIGN §4.2).
///
/// Two cases are captured during Phase 1:
///
/// * `PointerExpr` — `(*fp)(...)` or `fp(...)` where `fp` is a
///   function-pointer-typed variable. The resolver uses
///   `LocalScopeIndex::resolve_type(var_name, use_span.0)` to obtain
///   the canonical signature, then looks up `functions_by_signature`.
/// * `FieldExpr` — `obj.field(...)` or `obj->field(...)`. The resolver
///   resolves `receiver_name` to a type token, strips pointer depth to
///   obtain the canonical `struct_qn`, then keys
///   `struct_field_fnptr` and `bindings_by_field` on
///   `(struct_qn, field_name)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndirectShape {
    /// Pointer-expression call: `(*fp)(...)` / `fp(...)`.
    PointerExpr {
        /// The identifier of the function-pointer variable.
        var_name: String,
    },
    /// Field-expression call: `obj.field(...)` / `obj->field(...)`.
    FieldExpr {
        /// The receiver identifier (`obj`).
        receiver_name: String,
        /// The field name (`field`).
        field_name: String,
    },
}

// ---------------------------------------------------------------------------
// Tests — TEST:c-icall-precision-021
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::file::id::FileId;
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::string::StringId;

    /// DESIGN §10.2 wire-shape contract: a `None: Option<CIndirectSideTables>`
    /// must postcard-serialise to exactly one byte (the `Option`
    /// discriminant). This is load-bearing for V10 → V11 upconvert: legacy
    /// snapshots that lack the new slot decode the discriminant as
    /// `Some/None` and stop, with no further bytes consumed.
    #[test]
    fn option_none_serializes_to_one_byte() {
        let none: Option<CIndirectSideTables> = None;
        let bytes = postcard::to_stdvec(&none).expect("serialize None");
        assert_eq!(
            bytes.len(),
            1,
            "Option<CIndirectSideTables>::None must encode as a single \
             discriminant byte (got {} bytes: {:?})",
            bytes.len(),
            bytes
        );
        assert_eq!(
            bytes[0], 0x00,
            "postcard encodes Option::None as the 0x00 discriminant"
        );

        // Round-trip the single-byte payload back into None.
        let decoded: Option<CIndirectSideTables> =
            postcard::from_bytes(&bytes).expect("deserialize None");
        assert_eq!(decoded, None);
    }

    /// DESIGN §3.7 / §4.2 / §7.1 — populated round-trip preserves every
    /// inner map and vec deep-equally. Exercises:
    ///
    /// * `fn_signature` — one entry,
    /// * `struct_field_fnptr` — one entry (composite `(StringId, StringId)`
    ///   key),
    /// * `local_var_type` — one entry,
    /// * `local_scope_indices` — one entry with a synthetic
    ///   `LocalScopeIndex` (two scopes, three declarations, exercising
    ///   parent-scope chain and `resolve_type` round-trip),
    /// * `bindings_by_field` — one key with TWO `BindingEntry` values
    ///   covering both `BindingSiteKind` variants,
    /// * `pending_callsites` — two entries exercising both
    ///   `IndirectShape::PointerExpr` and `IndirectShape::FieldExpr`.
    #[test]
    fn populated_table_roundtrip_preserves_every_map() {
        let node_a = NodeId::new(1, 1);
        let node_b = NodeId::new(2, 1);
        let node_caller = NodeId::new(3, 1);
        let node_instance = NodeId::new(4, 1);
        let node_target_fn = NodeId::new(5, 1);
        let node_local_var = NodeId::new(6, 1);
        let sig_a = StringId::new(101);
        let struct_qn = StringId::new(200);
        let field_name = StringId::new(201);
        let field_sig = StringId::new(202);
        let local_var_type_tok = StringId::new(300);
        let file_id_a = FileId::new(7);

        // Build a small synthetic LocalScopeIndex by hand: two scopes
        // (outer + inner), three declarations across them.
        let scope_index = LocalScopeIndex::from_parts(
            vec![
                ScopeEntry::new((0, 100), None),    // outer file scope
                ScopeEntry::new((20, 60), Some(0)), // inner block scope, child of outer
            ],
            vec![
                vec![
                    LocalDeclaration::new("x".into(), "int".into(), (5, 12), 0),
                    LocalDeclaration::new("fp".into(), "void (*)(int)".into(), (13, 30), 0),
                ],
                vec![LocalDeclaration::new(
                    "y".into(),
                    "char".into(),
                    (25, 35),
                    1,
                )],
            ],
        );

        // Sanity: the synthetic LocalScopeIndex resolves both an outer and
        // an inner declaration. We re-check this post-roundtrip below.
        assert_eq!(scope_index.resolve_type("x", 40), Some("int"));
        assert_eq!(scope_index.resolve_type("y", 40), Some("char"));
        assert_eq!(scope_index.resolve_type("fp", 40), Some("void (*)(int)"));
        assert_eq!(scope_index.resolve_type("x", 1), None);

        let mut tables = CIndirectSideTables::new();
        tables.fn_signature.insert(node_a, sig_a);
        tables
            .struct_field_fnptr
            .insert((struct_qn, field_name), field_sig);
        tables
            .local_var_type
            .insert(node_local_var, local_var_type_tok);
        tables
            .local_scope_indices
            .insert(file_id_a, scope_index.clone());

        // Two BindingEntry values under one key — exercises both
        // BindingSiteKind variants.
        tables.bindings_by_field.insert(
            (struct_qn, field_name),
            vec![
                BindingEntry {
                    instance_node: node_instance,
                    target_fn: node_target_fn,
                    site_kind: BindingSiteKind::DesignatedInitializer,
                },
                BindingEntry {
                    instance_node: node_b,
                    target_fn: node_a,
                    site_kind: BindingSiteKind::PositionalInitializer,
                },
            ],
        );

        // Two pending callsites — one of each IndirectShape variant.
        tables.pending_callsites.push(IndirectCallsite {
            caller: node_caller,
            file_id: file_id_a,
            use_span: (40, 48),
            shape: IndirectShape::PointerExpr {
                var_name: "fp".into(),
            },
            argument_count: 2,
            is_async: false,
        });
        tables.pending_callsites.push(IndirectCallsite {
            caller: node_caller,
            file_id: file_id_a,
            use_span: (50, 70),
            shape: IndirectShape::FieldExpr {
                receiver_name: "ops".into(),
                field_name: "read".into(),
            },
            argument_count: 3,
            is_async: false,
        });

        assert!(
            !tables.is_empty(),
            "constructed tables must report non-empty"
        );

        // Round-trip the populated tables through postcard.
        let some_tables = Some(tables.clone());
        let bytes = postcard::to_stdvec(&some_tables).expect("serialize Some");
        assert!(
            bytes.len() > 1,
            "populated Some(...) must encode more than the bare 0x01 \
             discriminant; got {} bytes",
            bytes.len()
        );
        assert_eq!(
            bytes[0], 0x01,
            "postcard encodes Option::Some as the 0x01 discriminant"
        );

        let decoded: Option<CIndirectSideTables> =
            postcard::from_bytes(&bytes).expect("deserialize Some");

        // Deep-equal the round-trip.
        assert_eq!(
            decoded,
            Some(tables.clone()),
            "populated CIndirectSideTables must deep-roundtrip via postcard"
        );

        // Spot-check inner shape (independent of the deep-equality assertion):
        // every inner map preserved its single populated entry.
        let decoded_tables = decoded.expect("Some after roundtrip");
        assert_eq!(decoded_tables.fn_signature.get(&node_a), Some(&sig_a));
        assert_eq!(
            decoded_tables
                .struct_field_fnptr
                .get(&(struct_qn, field_name)),
            Some(&field_sig)
        );
        assert_eq!(
            decoded_tables.local_var_type.get(&node_local_var),
            Some(&local_var_type_tok)
        );
        let restored_scope = decoded_tables
            .local_scope_indices
            .get(&file_id_a)
            .expect("scope index restored under file_id_a");
        assert_eq!(restored_scope.resolve_type("x", 40), Some("int"));
        assert_eq!(restored_scope.resolve_type("y", 40), Some("char"));
        let bindings = decoded_tables
            .bindings_by_field
            .get(&(struct_qn, field_name))
            .expect("binding entries restored under (struct_qn, field_name)");
        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings[0].site_kind,
            BindingSiteKind::DesignatedInitializer
        );
        assert_eq!(
            bindings[1].site_kind,
            BindingSiteKind::PositionalInitializer
        );
        assert_eq!(decoded_tables.pending_callsites.len(), 2);
        assert!(matches!(
            &decoded_tables.pending_callsites[0].shape,
            IndirectShape::PointerExpr { var_name } if var_name == "fp"
        ));
        assert!(matches!(
            &decoded_tables.pending_callsites[1].shape,
            IndirectShape::FieldExpr { receiver_name, field_name }
                if receiver_name == "ops" && field_name == "read"
        ));

        // Scope_index_for accessor reaches the restored value.
        assert!(decoded_tables.scope_index_for(file_id_a).is_some());
        assert!(decoded_tables.scope_index_for(FileId::new(9999)).is_none());
    }

    /// A freshly-constructed `CIndirectSideTables` is empty by every
    /// inner-table criterion.
    #[test]
    fn new_tables_are_empty() {
        let tables = CIndirectSideTables::new();
        assert!(tables.is_empty());
        assert!(tables.fn_signature.is_empty());
        assert!(tables.struct_field_fnptr.is_empty());
        assert!(tables.local_var_type.is_empty());
        assert!(tables.local_scope_indices.is_empty());
        assert!(tables.bindings_by_field.is_empty());
        assert!(tables.pending_callsites.is_empty());
    }
}
