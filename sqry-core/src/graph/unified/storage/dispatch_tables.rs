//! Dispatch tables — Phase β joint-stubs.
//!
//! This module defines [`DispatchTables`], the per-snapshot side-table store
//! that Plan B (graph-fidelity planner correctness, WS2 — see
//! `docs/superpowers/plans/2026-05-25-graph-fidelity-planner-correctness-dag.toml`)
//! will populate with virtual / interface / duck-typed / structural /
//! promiscuous-elided dispatch resolutions.
//!
//! The type lands as part of the V12 schema bump alongside Plan A's
//! [`super::framework_routes::FrameworkRouteMetadata`] so a single V11 → V12
//! upconvert covers both surfaces.
//!
//! # Population frontier
//!
//! The MCP `resolved_via` filter param and the `Predicate::ResolvedViaEq`
//! planner predicate that ship in this same PR are **complete capabilities**
//! — `Predicate::ResolvedViaEq` walks outgoing `Calls` edges and accepts
//! the node iff at least one edge carries a `resolved_via` value in the
//! requested set (see `sqry-db/src/planner/execute.rs`
//! `CompiledPredicate::ResolvedViaEq`). All 8 `ResolvedVia` variants
//! evaluate today. The 3 pre-existing variants
//! (`Direct` / `TypeMatch` / `BindingPlane`) are populated in production
//! by `pass5b_c_indirect`; the 5 new variants
//! (`VirtualDispatch` / `InterfaceDispatch` / `DuckTyped` / `Structural`
//! / `PromiscuousElided`) light up as Plan B's downstream resolver PRs
//! (`U_WS2_2` … `U_WS2_7`) emit edges carrying them. Coverage in
//! `sqry-db/tests/phase_beta_predicate_evaluation.rs` exercises empty /
//! match / non-match / multi-target / AND-composition paths against
//! fixture graphs that emit the new variants directly.
//!
//! What is *not* in this PR is the **aggregate per-node bookkeeping**
//! captured by [`DispatchTables`] — no resolver pass in `sqry-core`
//! populates it yet. The aggregate store is reserved for the dispatch
//! resolvers' lookup tables, fan-out cap hits, and ambiguity sets.
//!
//! # Why a separate side-table, not new `EdgeKind` variants
//!
//! Plan B SPEC §3.2 carries the new resolution provenances on the existing
//! `EdgeKind::Calls.resolved_via` field — the dispatch *resolver* writes its
//! per-edge resolution into the edge metadata, while [`DispatchTables`] holds
//! the *aggregate* per-node bookkeeping (lookup tables, fan-out cap hits,
//! ambiguity sets) that the resolver needs across edges. Storing both in the
//! same place keeps planner index reuse intact (see Plan B U_WS2_7
//! critical decision: "per-edge filter, not separate edge kind; preserves
//! planner index reuse").

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::node::id::NodeId;

/// Per-node dispatch-resolution side tables — Plan B 02_DESIGN §3.7
/// (line 279) shape.
///
/// Populated by Plan B's WS2 resolvers (JVM virtual / interface dispatch, Go
/// interface dispatch, Python duck-typed dispatch, TypeScript structural
/// dispatch, promiscuous-cap elision). Empty in the joint-stubs PR.
///
/// # Per-language plane
///
/// Each plane is a `BTreeMap` keyed by the call-site `NodeId`. The
/// per-plane entry types are minimal in the joint-stubs PR; downstream
/// WS2 resolver PRs extend them with concrete fields (vtable index,
/// interface witness set, structural shape hash, etc. — DESIGN §3.7
/// line 287). The wire envelope is V12-stable; resolver PRs only add
/// fields to the entry structs, not new top-level keys.
///
/// # Determinism
///
/// All collection fields are `BTreeMap` (not `HashMap`) so postcard-encoded
/// snapshots are bit-for-bit deterministic across `HashMap` iteration order
/// — the same discipline used elsewhere in this codebase (cf. the iterative
/// Tarjan SCC determinism fix, commit `b6bec9fc0`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchTables {
    /// JVM virtual / abstract method dispatch entries.
    /// Plan B `pass5c_jvm_virtual` populates this map; resolved targets
    /// emit `Calls { resolved_via: VirtualDispatch }` edges in addition.
    pub jvm_virtual: BTreeMap<NodeId, JvmDispatchEntry>,
    /// Go interface dispatch entries (structural method-set superset).
    /// Plan B `pass5d_go_interface` populates this map; resolved targets
    /// emit `Calls { resolved_via: InterfaceDispatch }` edges in addition.
    pub go_interface: BTreeMap<NodeId, GoDispatchEntry>,
    /// Python duck-typed dispatch entries.
    /// Plan B `pass5e_python_duck` populates this map; resolved targets
    /// emit `Calls { resolved_via: DuckTyped }` edges in addition.
    pub python_duck: BTreeMap<NodeId, PythonDispatchEntry>,
    /// TypeScript structural dispatch entries.
    /// Plan B `pass5f_ts_structural` populates this map; resolved targets
    /// emit `Calls { resolved_via: Structural }` edges in addition.
    pub ts_structural: BTreeMap<NodeId, TsDispatchEntry>,
    /// Fan-out cap-hit log (`CALLSITE_PROMISCUOUS` exceeded).
    /// Each entry records the call-site that tripped the cap, which
    /// resolver tripped it, and the candidate count seen. Surfaces to
    /// `resolved_via=promiscuous_elided` MCP filter consumers as a
    /// diagnostic.
    pub cap_hits: Vec<CapHit>,
}

impl DispatchTables {
    /// Returns `true` if no resolver has populated any plane.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jvm_virtual.is_empty()
            && self.go_interface.is_empty()
            && self.python_duck.is_empty()
            && self.ts_structural.is_empty()
            && self.cap_hits.is_empty()
    }
}

/// JVM virtual / abstract method dispatch entry.
///
/// Joint-stub shape: empty. Plan B `pass5c_jvm_virtual` populates with
/// concrete fields (vtable index, candidate `NodeId` set, fan-out,
/// was-elided). DESIGN §3.3 line 247-253.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JvmDispatchEntry {
    /// Reserved for Plan B `pass5c_jvm_virtual`. Empty in this stub.
    ///
    /// Modelled as `Option<()>` to give postcard a stable wire shape
    /// the resolver PR can replace with a concrete struct without
    /// breaking V12 backward compatibility (postcard `None` and
    /// `Some(())` both encode as a single discriminant byte).
    #[serde(default)]
    pub resolution: Option<()>,
}

/// Go interface dispatch entry.
///
/// Joint-stub shape: empty. Plan B `pass5d_go_interface` populates with
/// concrete fields (interface signature, candidate method-set superset
/// types, fan-out). DESIGN §3.4 line 256-260.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoDispatchEntry {
    /// Reserved for Plan B `pass5d_go_interface`. Empty in this stub.
    #[serde(default)]
    pub resolution: Option<()>,
}

/// Python duck-typed dispatch entry.
///
/// Joint-stub shape: empty. Plan B `pass5e_python_duck` populates with
/// concrete fields (method name, arity, candidate `NodeId` set,
/// fan-out). DESIGN §3.5 line 263-270.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonDispatchEntry {
    /// Reserved for Plan B `pass5e_python_duck`. Empty in this stub.
    #[serde(default)]
    pub resolution: Option<()>,
}

/// TypeScript structural dispatch entry.
///
/// Joint-stub shape: empty. Plan B `pass5f_ts_structural` populates with
/// concrete fields (declared interface superset, structural shape hash,
/// candidate set, fan-out). DESIGN §3.6 line 272-275.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TsDispatchEntry {
    /// Reserved for Plan B `pass5f_ts_structural`. Empty in this stub.
    #[serde(default)]
    pub resolution: Option<()>,
}

/// Fan-out cap-hit diagnostic — Plan B DESIGN §3.7 (`cap_hits`).
///
/// Recorded by every resolver when `CALLSITE_PROMISCUOUS` is exceeded.
/// The resolver still emits a `Calls { resolved_via: PromiscuousElided }`
/// self-edge for the call-site so MCP filters that look for elided edges
/// see a uniform surface; the [`CapHit`] entry carries the structured
/// per-cap-hit context (which resolver, candidate count, contributing
/// receiver shape).
///
/// Joint-stub shape: minimal. Plan B's resolver PRs (`U_WS2_2_*` ...)
/// extend this with concrete fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapHit {
    /// Reserved for Plan B WS2 resolvers. Empty in this stub.
    #[serde(default)]
    pub diagnostic: Option<()>,
}

/// Back-compat alias for the joint-stubs PR.
///
/// The previous joint-stub shape exposed a single `entries: BTreeMap<NodeId,
/// DispatchEntry>` field; the V12 surface lifts the per-plane maps to
/// top-level fields matching DESIGN §3.7. This alias preserves the
/// `DispatchEntry` type name (re-exported through `sqry_core::schema`) so
/// downstream PRs that pre-dated the rewrite can continue to refer to a
/// per-node opaque entry type. Plan B's resolver PRs will switch their
/// imports to the per-plane entry types (`JvmDispatchEntry`,
/// `GoDispatchEntry`, ...) over time.
pub type DispatchEntry = JvmDispatchEntry;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_tables_default_is_empty() {
        let dt = DispatchTables::default();
        assert!(dt.is_empty());
        assert_eq!(dt.jvm_virtual.len(), 0);
        assert_eq!(dt.go_interface.len(), 0);
        assert_eq!(dt.python_duck.len(), 0);
        assert_eq!(dt.ts_structural.len(), 0);
        assert_eq!(dt.cap_hits.len(), 0);
    }

    #[test]
    fn dispatch_tables_round_trip_through_postcard() {
        let dt = DispatchTables::default();
        let bytes = postcard::to_allocvec(&dt).expect("serialize");
        let round: DispatchTables = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(dt, round);
    }

    #[test]
    fn dispatch_entry_default_is_empty() {
        let entry = DispatchEntry::default();
        assert!(entry.resolution.is_none());
    }

    #[test]
    fn cap_hit_round_trip_through_postcard() {
        let hit = CapHit::default();
        let bytes = postcard::to_allocvec(&hit).expect("serialize");
        let round: CapHit = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(hit, round);
    }
}
