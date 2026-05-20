//! Pass 5b — C indirect-call resolution (Phase A, U12).
//!
//! This pass runs after Phase 4e (binding-plane derivation) and BEFORE
//! Pass 5 (cross-language linking). It consumes the C indirect side
//! tables populated during Phase 1 + Phase 3/4 (U10/U11) and rewrites
//! each captured indirect callsite's synthetic `Calls` stub into a
//! precise candidate set:
//!
//! 1. **Binding-plane lookup** (DESIGN §4.2 step 2 / §7): for
//!    `FieldExpr` callsites, the receiver's source-level type token is
//!    resolved via [`LocalScopeIndex::resolve_type`], stripped of the
//!    `struct ` keyword and any pointer depth, interned, and used to
//!    key into `bindings_by_field`. Each [`BindingEntry::target_fn`]
//!    becomes a candidate. Result count > cap → skip to type-match.
//!    For `PointerExpr` callsites, no `bindings_by_var` side table
//!    exists at HEAD (DESIGN §7's intra-file pointer-var binding is
//!    reserved for a later cluster); binding-plane lookup is
//!    intentionally empty and the resolver falls through.
//! 2. **Type-match fallback** (DESIGN §4.2 step 3): the expected
//!    signature is recovered from `struct_field_fnptr` (FieldExpr) or
//!    from `LocalScopeIndex::resolve_type` (PointerExpr). The candidate
//!    set is every C function whose `fn_signature` matches AND whose
//!    `is_address_taken` flag is set. The `fn_signature` table is
//!    transitively seeded from `bindings_by_field` × `struct_field_fnptr`
//!    in this module's prelude (see [`seed_fn_signature_from_bindings`])
//!    because the C plugin does not stage `fn_signature` directly at
//!    HEAD — every binding establishes that the bound function's
//!    canonical signature matches the field's canonical signature.
//! 3. **Cap enforcement** (DESIGN §5.2): cap = 4 (calibrated 2026-05-14
//!    against `test-fixtures/c-icall-precision/linux-driver-subset/` —
//!    see `measurements/2026-05-14-cap-histogram.txt`). On cap exceeded,
//!    the synthetic stub is preserved and
//!    [`NodeMetadataStore::mark_callsite_promiscuous`] is invoked on the
//!    caller node.
//! 4. **Fallback**: on every miss the synthetic stub edge is left
//!    untouched — zero regression vs HEAD's pre-Phase-A behaviour
//!    (DESIGN §4.3).
//!
//! See DESIGN sections 4, 7, and 8.4 for the canonical algorithm. See
//! IMPL_PLAN §"U12 — `pass5b_c_indirect_resolve`" for the wire-in
//! contract.

use std::collections::HashMap;

use crate::graph::node::Span;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::store::StoreEdgeRef;
use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::storage::c_indirect::{IndirectCallsite, IndirectShape};
use crate::graph::unified::string::StringId;

/// Cap on indirect-callsite candidate set size (DESIGN §5.2).
///
/// Phase A lands a deliberately-conservative value. U17 calibrates the
/// final number against the kernel-scale fixtures committed in
/// `test-fixtures/c-icall-precision/`.
// Calibrated 2026-05-14, see measurements/2026-05-14-cap-histogram.txt
// (linux-driver-subset: p99=3, max=3, count=11; tightened per IMPL PLAN step 4
//  rule "min(16, measured p99 + 25%)" — ceil(3 * 1.25) = 4)
const CAP: usize = 4;

/// Statistics produced by [`resolve_c_indirect_calls`].
///
/// Surfaced via `log::info!` at `sqry_core::build` from
/// `entrypoint.rs`. Each counter increments per [`IndirectCallsite`]
/// processed, not per emitted edge — a single binding-plane resolution
/// that produces three candidate edges contributes `+1` to
/// `binding_resolved`, not `+3`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass5bStats {
    /// Number of callsites resolved via the binding plane (§7).
    pub binding_resolved: u64,
    /// Number of callsites resolved via the type-match fallback
    /// (§4.2 step 3).
    pub typematch_resolved: u64,
    /// Number of callsites whose candidate set exceeded the cap
    /// (DESIGN §5.2). Caller is flagged `CALLSITE_PROMISCUOUS` and the
    /// synthetic stub is preserved.
    pub cap_exceeded: u64,
    /// Number of callsites that produced no candidates on either path.
    /// The synthetic stub is preserved — zero regression vs HEAD.
    pub stub_fallback: u64,
}

impl Pass5bStats {
    /// Total number of callsites that produced a precise edge rewrite
    /// (binding or type-match).
    #[must_use]
    pub fn total_resolved(&self) -> u64 {
        self.binding_resolved + self.typematch_resolved
    }
}

/// Run Pass 5b — C indirect-call resolution.
///
/// Non-C workspaces have `graph.c_indirect_tables() == None`; the pass
/// short-circuits with an all-zero [`Pass5bStats`]. Build-pipeline
/// errors are intentionally swallowed at this layer: the pass either
/// resolves a callsite or leaves the synthetic stub intact. The only
/// caller-visible signal is the returned stats.
///
/// See DESIGN §4.2 for the algorithm and §8.4 for entry-point wiring.
pub fn resolve_c_indirect_calls(graph: &mut CodeGraph) -> Pass5bStats {
    let mut stats = Pass5bStats::default();

    // Non-C workspaces: zero-allocation fast path.
    if graph.c_indirect_tables().is_none() {
        return stats;
    }

    // Seed fn_signature transitively from bindings_by_field ×
    // struct_field_fnptr. The C plugin does not populate fn_signature
    // directly at HEAD (U10's signature_builder fires only for struct
    // fields), so without this seeding the type-match fallback would
    // be inert. Every binding entry (target_fn) bound to (struct_qn,
    // field_name) carries the implicit declaration that
    // fn_signature[target_fn] == struct_field_fnptr[(struct_qn,
    // field_name)] — the C type system enforces this at the binding
    // site. See module-doc point 2.
    seed_fn_signature_from_bindings(graph);

    // Snapshot the callsite list — we mutate the graph (edge store,
    // metadata store, fn_signature) while iterating, so the pending
    // vector must be detached first.
    let callsites: Vec<IndirectCallsite> = graph
        .c_indirect_tables()
        .map(|t| t.pending_callsites.clone())
        .unwrap_or_default();

    log::info!(
        target: "sqry_core::build",
        "Pass 5b start: {} indirect callsite(s) pending",
        callsites.len(),
    );

    // Build the type-match reverse index (canonical signature StringId
    // → Vec<NodeId of address-taken fns with that sig>) once. Held by
    // value to avoid borrowing graph during the per-callsite loop.
    let signature_index = build_address_taken_signature_index(graph);

    // ------------------------------------------------------------------
    // Two-phase structure (perf fix, 2026-05-20).
    //
    // The pre-refactor loop called `graph.edges().edges_from(callsite.caller)`
    // per callsite inside `rewrite_synthetic_edge`. `edges_from` is
    // O(|delta|) per call (it rebuilds a per-source LWW map by scanning
    // the full delta — see store.rs:567 / 677). For corpora the size of
    // the Linux kernel (≈24M raw edges in delta × hundreds of callsites)
    // this is O(callsites × |delta|) and dominates the build (1392s of
    // 1582s total wall on a fresh-clone Linux kernel index).
    //
    // The fix splits the loop into:
    //
    //   * Phase 5b-a (read): build ONE per-source outgoing-edge map from
    //     `EdgeStore::all_live_forward_edges()` (O(|csr| + |delta|)
    //     total), then look each callsite's caller up in the map
    //     (O(1) amortised per callsite). Stub-finding stays here.
    //   * Phase 5b-b (write): drain the planned actions and mutate the
    //     graph. No reads of `graph.edges()` happen in this phase, so
    //     the precomputed map can never go stale.
    //
    // See `bbnty/research/sqry-perf/linux-2026-05-20-v16.0.2-rootcause.md`
    // for the full root-cause analysis (Codex iter-2 APPROVED).
    // ------------------------------------------------------------------

    // Phase 5b-a: build the per-source outgoing-edge map once.
    let outgoing_by_caller: HashMap<NodeId, Vec<StoreEdgeRef>> = graph
        .edges()
        .all_live_forward_edges()
        .into_iter()
        .fold(HashMap::new(), |mut m, e| {
            m.entry(e.source).or_default().push(e);
            m
        });

    // Phase 5b-a (continued): per-callsite read + decide.
    let empty_outgoing: Vec<StoreEdgeRef> = Vec::new();
    let mut planned: Vec<PlannedAction> = Vec::with_capacity(callsites.len());
    for callsite in &callsites {
        let outgoing = outgoing_by_caller
            .get(&callsite.caller)
            .unwrap_or(&empty_outgoing);
        let action = match resolve_one(graph, callsite, &signature_index) {
            ResolutionOutcome::BindingPlane(targets) => {
                stats.binding_resolved += 1;
                build_rewrite_action(
                    graph,
                    callsite,
                    &targets,
                    ResolvedVia::BindingPlane,
                    outgoing,
                )
            }
            ResolutionOutcome::TypeMatch(targets) => {
                stats.typematch_resolved += 1;
                build_rewrite_action(graph, callsite, &targets, ResolvedVia::TypeMatch, outgoing)
            }
            ResolutionOutcome::CapExceeded => {
                stats.cap_exceeded += 1;
                PlannedAction::MarkPromiscuous {
                    caller: callsite.caller,
                }
            }
            ResolutionOutcome::FallbackToStub => {
                stats.stub_fallback += 1;
                PlannedAction::Noop
            }
        };
        planned.push(action);
    }

    // Drop the map before the write phase to release ~|csr|+|delta|
    // bytes of working memory.
    drop(outgoing_by_caller);

    // Phase 5b-b: drain planned actions and apply graph mutations.
    for action in planned {
        apply_planned_action(graph, action);
    }

    log::info!(
        target: "sqry_core::build",
        "Pass 5b end: binding={}, typematch={}, cap_exceeded={}, fallback={}",
        stats.binding_resolved,
        stats.typematch_resolved,
        stats.cap_exceeded,
        stats.stub_fallback,
    );

    stats
}

/// A planned mutation to apply in Phase 5b-b.
///
/// Built during the read phase (5b-a) so the write phase can run with
/// no reads of `graph.edges()` — eliminating any need to invalidate the
/// precomputed per-source outgoing-edge map.
enum PlannedAction {
    /// Remove the stub edge identified by these coordinates and emit
    /// one precise `Calls` edge per candidate.
    Rewrite {
        caller: NodeId,
        stub_target: NodeId,
        stub_kind: EdgeKind,
        stub_file: FileId,
        stub_spans: Vec<Span>,
        new_kind: EdgeKind,
        candidates: Vec<NodeId>,
    },
    /// Mark the caller node as `CALLSITE_PROMISCUOUS`. No edge mutation.
    MarkPromiscuous { caller: NodeId },
    /// No graph mutation; only the stats counter advanced in 5b-a.
    Noop,
}

fn apply_planned_action(graph: &mut CodeGraph, action: PlannedAction) {
    match action {
        PlannedAction::Rewrite {
            caller,
            stub_target,
            stub_kind,
            stub_file,
            stub_spans,
            new_kind,
            candidates,
        } => {
            // Step 2 (DESIGN §4.3): remove the stub.
            graph
                .edges_mut()
                .remove_edge(caller, stub_target, stub_kind, stub_file);
            // Step 3: emit one precise edge per candidate.
            for candidate in candidates {
                graph.edges_mut().add_edge_with_spans(
                    caller,
                    candidate,
                    new_kind.clone(),
                    stub_file,
                    stub_spans.clone(),
                );
            }
        }
        PlannedAction::MarkPromiscuous { caller } => {
            graph.macro_metadata_mut().mark_callsite_promiscuous(caller);
        }
        PlannedAction::Noop => {}
    }
}

// ---------------------------------------------------------------------------
// Internal: per-callsite resolution dispatch
// ---------------------------------------------------------------------------

enum ResolutionOutcome {
    BindingPlane(Vec<NodeId>),
    TypeMatch(Vec<NodeId>),
    CapExceeded,
    FallbackToStub,
}

fn resolve_one(
    graph: &CodeGraph,
    callsite: &IndirectCallsite,
    signature_index: &HashMap<StringId, Vec<NodeId>>,
) -> ResolutionOutcome {
    // Recover the merged side tables. Cloning bindings_by_field /
    // struct_field_fnptr is avoided — we only read these.
    let Some(tables) = graph.c_indirect_tables() else {
        return ResolutionOutcome::FallbackToStub;
    };

    // Derive (struct_qn_id_opt, field_name_id_opt, expected_sig_opt)
    // per callsite shape. PointerExpr's binding plane lookup is empty
    // until a `bindings_by_var` table lands — DESIGN §7 §3.3.2 line.
    let (binding_targets, expected_sig): (Vec<NodeId>, Option<StringId>) = match &callsite.shape {
        IndirectShape::FieldExpr {
            receiver_name,
            field_name,
        } => {
            // Resolve receiver's source-level type token via
            // LocalScopeIndex (intra-procedural).
            let Some(scope) = tables.scope_index_for(callsite.file_id) else {
                return ResolutionOutcome::FallbackToStub;
            };
            let Some(receiver_type) = scope.resolve_type(receiver_name, callsite.use_span.0) else {
                return ResolutionOutcome::FallbackToStub;
            };
            // Strip `struct ` keyword + leading whitespace to align
            // with the bare-tag form stored in struct_field_fnptr /
            // bindings_by_field (see test
            // `struct_field_fnptr_merge_across_files`).
            let struct_tag = strip_struct_keyword_and_pointer(receiver_type);
            // Both legs need to be interned to look up the side
            // tables. Use immutable `get` — these strings were
            // interned during Phase 3/4 staging, so failing to find
            // them here means the receiver / field is simply not
            // tracked by the C plugin and we fall back to stub.
            let strings = graph.strings();
            let Some(struct_id) = strings.get(struct_tag) else {
                return ResolutionOutcome::FallbackToStub;
            };
            let Some(field_id) = strings.get(field_name) else {
                return ResolutionOutcome::FallbackToStub;
            };
            let key = (struct_id, field_id);

            let binding_targets: Vec<NodeId> = tables
                .bindings_by_field
                .get(&key)
                .map(|entries| entries.iter().map(|e| e.target_fn).collect())
                .unwrap_or_default();

            let expected_sig = tables.struct_field_fnptr.get(&key).copied();

            (binding_targets, expected_sig)
        }
        IndirectShape::PointerExpr { var_name } => {
            // No bindings_by_var table exists at HEAD; binding plane
            // is intentionally empty for the pointer-expression shape.
            // DESIGN §7's intra-file `binding_lookup_by_var(var_name,
            // file_id)` is reserved for a follow-up cluster.
            let Some(scope) = tables.scope_index_for(callsite.file_id) else {
                return ResolutionOutcome::FallbackToStub;
            };
            let Some(type_token) = scope.resolve_type(var_name, callsite.use_span.0) else {
                return ResolutionOutcome::FallbackToStub;
            };
            let expected_sig = graph.strings().get(type_token);
            (Vec::new(), expected_sig)
        }
    };

    // Step 1: binding-plane first (DESIGN §4.2 step 2). On cardinality
    // exceeded, fall through to type-match per the same step.
    //
    // IMPORTANT: dedupe BEFORE applying CAP. `bindings_by_field` may
    // carry duplicate `target_fn` entries when the same function is
    // bound under multiple struct instances (e.g. 33 `struct ops`
    // instances all binding `.read = my_read`). The pre-dedup count
    // can blow past CAP while the unique candidate set is small (often
    // 1). Capping before dedupe would lose `BindingPlane` provenance
    // for what is genuinely a single candidate.
    if !binding_targets.is_empty() {
        let mut seen: std::collections::HashSet<(u32, u64)> =
            std::collections::HashSet::with_capacity(binding_targets.len());
        let deduped: Vec<NodeId> = binding_targets
            .into_iter()
            .filter(|nid| seen.insert((nid.index(), nid.generation())))
            .collect();
        if !deduped.is_empty() && deduped.len() <= CAP {
            return ResolutionOutcome::BindingPlane(deduped);
        }
        // Deduped set still over cap (or empty after dedupe, which
        // shouldn't happen given the `!binding_targets.is_empty()`
        // guard above but is handled defensively). Fall through to
        // type-match (which applies its own cap below).
    }

    // Step 2: type-match fallback (DESIGN §4.2 step 3). The expected
    // signature must be present AND match against the address-taken
    // index.
    let Some(expected) = expected_sig else {
        return ResolutionOutcome::FallbackToStub;
    };
    let typematch_targets: Vec<NodeId> =
        signature_index.get(&expected).cloned().unwrap_or_default();
    if typematch_targets.is_empty() {
        return ResolutionOutcome::FallbackToStub;
    }
    if typematch_targets.len() > CAP {
        return ResolutionOutcome::CapExceeded;
    }
    ResolutionOutcome::TypeMatch(typematch_targets)
}

// ---------------------------------------------------------------------------
// Internal: edge rewrite (DESIGN §4.3)
// ---------------------------------------------------------------------------

/// Build the planned mutation for a single callsite's stub rewrite —
/// read-only over `graph`.
///
/// Locates the synthetic stub in the precomputed `outgoing` slice
/// (Phase 5b-a's per-source outgoing-edge map view of `callsite.caller`).
/// On match, returns a [`PlannedAction::Rewrite`] capturing the stub
/// coordinates and the new-edge template; on miss, returns
/// [`PlannedAction::Noop`] after logging.
///
/// Steps per DESIGN §4.3:
///
/// 1. Locate the synthetic stub edge on the caller's outgoing-edge set:
///    the one `Calls` edge whose target's name matches the callsite
///    shape's `field_name` / `var_name` AND whose argument_count
///    matches the staged callsite.
/// 2. (planned) Remove it. Applied in [`apply_planned_action`].
/// 3. (planned) Emit `Calls { argument_count, is_async, resolved_via }`
///    from caller → each candidate, preserving the original span (or
///    an empty span vec when the stub had none). Applied in
///    [`apply_planned_action`].
///
/// If no synthetic stub is found, the miss is logged and no plan is
/// emitted — this is a defensive branch; in practice every staged
/// `IndirectCallsite` had its stub emitted in Phase 1.
fn build_rewrite_action(
    graph: &CodeGraph,
    callsite: &IndirectCallsite,
    candidates: &[NodeId],
    resolved_via: ResolvedVia,
    outgoing: &[StoreEdgeRef],
) -> PlannedAction {
    // The stub edge target name is the field name (FieldExpr) or var
    // name (PointerExpr) per `extract_call_target` in
    // sqry-lang-c/src/relations/graph_builder.rs:1385-1417.
    let stub_target_name: &str = match &callsite.shape {
        IndirectShape::FieldExpr { field_name, .. } => field_name.as_str(),
        IndirectShape::PointerExpr { var_name } => var_name.as_str(),
    };

    // Argument count widening: the synthetic stub stores u8 via
    // add_call_edge_full_with_span; IndirectCallsite carries u32.
    // Clamp on the comparison side, matching how Phase 1 clamped
    // (u8::try_from(...).unwrap_or(u8::MAX) — graph_builder.rs:657).
    let staged_argc_u8 = u8::try_from(callsite.argument_count).unwrap_or(u8::MAX);

    // Find the synthetic stub: walk caller's outgoing edges, match
    // Calls{} variant whose target node's name equals
    // stub_target_name AND whose argument_count matches.
    let stub = outgoing.iter().find(|e| {
        let EdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via,
        } = &e.kind
        else {
            return false;
        };
        // Stub edges from Phase 1 always carry resolved_via=Direct
        // (helper.rs::add_call_edge_full_with_span).
        if *resolved_via != ResolvedVia::Direct {
            return false;
        }
        if *argument_count != staged_argc_u8 || *is_async != callsite.is_async {
            return false;
        }
        // Target node name must equal the stub's recorded callee
        // text (field name / var name).
        let Some(entry) = graph.nodes().get(e.target) else {
            return false;
        };
        let Some(name) = graph.strings().resolve(entry.name) else {
            return false;
        };
        name.as_ref() == stub_target_name
    });

    let Some(stub) = stub else {
        // Defensive: every staged IndirectCallsite should have had
        // its stub edge emitted by Phase 1. Missing-stub means either
        // (a) a different pass already rewrote it, or (b) the edge
        // got tombstoned across reindex. Either way, do not silently
        // emit duplicates — log and skip.
        log::debug!(
            target: "sqry_core::build",
            "Pass 5b: no synthetic stub found for callsite caller={:?} \
             shape={:?} use_span={:?} — skipping rewrite",
            callsite.caller,
            callsite.shape,
            callsite.use_span,
        );
        return PlannedAction::Noop;
    };

    // Preserve original spans (call-site identity) on the rewritten
    // precise edges. The stub's `file` IS the callsite's file_id by
    // construction (helper.rs::add_call_edge_full_with_span uses
    // self.file_id), but we use the stub's recorded file to be
    // robust against future helper refactors.
    let new_kind = EdgeKind::Calls {
        argument_count: staged_argc_u8,
        is_async: callsite.is_async,
        resolved_via,
    };

    PlannedAction::Rewrite {
        caller: callsite.caller,
        stub_target: stub.target,
        stub_kind: stub.kind.clone(),
        stub_file: stub.file,
        stub_spans: stub.spans.clone(),
        new_kind,
        candidates: candidates.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Internal: fn_signature seeding (transitive via bindings)
// ---------------------------------------------------------------------------

/// Seed `fn_signature` for every address-taken function that is bound
/// to a known struct-field signature.
///
/// Per DESIGN §3.7 / §4.2, `fn_signature[node_id] = canonical
/// signature StringId` is the type-match index over functions. The C
/// plugin does not stage this directly at HEAD (U10's
/// `signature_builder` fires only for struct fields). However, every
/// binding entry establishes a typed equivalence: if function `f` is
/// bound to field `g.field` of declared type `T`, then `f` must have
/// type `T` (C's type system enforces this at the binding site, even
/// if the C plugin used an implicit cast). We exploit this
/// equivalence here to seed `fn_signature` transitively.
///
/// This is a no-op when `bindings_by_field` or `struct_field_fnptr` is
/// empty — both common cases (no C in workspace, or no bindings at
/// all).
fn seed_fn_signature_from_bindings(graph: &mut CodeGraph) {
    // Snapshot the (key, target_fn) pairs from bindings_by_field and
    // the corresponding signature from struct_field_fnptr. Operating
    // on a local Vec avoids the dual borrow when we later mutate
    // c_indirect_tables_mut().
    let pairs: Vec<(NodeId, StringId)> = {
        let Some(tables) = graph.c_indirect_tables() else {
            return;
        };
        let mut out = Vec::new();
        for (key, entries) in &tables.bindings_by_field {
            let Some(&sig) = tables.struct_field_fnptr.get(key) else {
                continue;
            };
            for entry in entries {
                out.push((entry.target_fn, sig));
            }
        }
        out
    };

    if pairs.is_empty() {
        return;
    }

    let Some(tables_mut) = graph.c_indirect_tables_mut().as_mut() else {
        return;
    };
    for (node_id, sig) in pairs {
        // Last-write-wins; the binding semantics enforce equality
        // when multiple bindings reach the same target, so any
        // duplicate insert is benign.
        tables_mut.fn_signature.entry(node_id).or_insert(sig);
    }
}

/// Build the reverse signature → address-taken Vec<NodeId> index used
/// by the type-match fallback.
///
/// Walks `fn_signature` (seeded above) and includes only nodes whose
/// `is_address_taken` flag is set, per DESIGN §4.2 step 3 ("`...filter(|f|
/// graph.metadata().is_address_taken(**f))`").
fn build_address_taken_signature_index(graph: &CodeGraph) -> HashMap<StringId, Vec<NodeId>> {
    let mut out: HashMap<StringId, Vec<NodeId>> = HashMap::new();
    let Some(tables) = graph.c_indirect_tables() else {
        return out;
    };
    let metadata = graph.macro_metadata();
    for (&node_id, &sig) in &tables.fn_signature {
        if metadata.is_address_taken(node_id) {
            out.entry(sig).or_default().push(node_id);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Internal: receiver-type → bare struct tag normalisation
// ---------------------------------------------------------------------------

/// Strip a leading `struct `/`union `/`enum ` keyword and trim
/// trailing pointer / qualifier characters from a `LocalScopeIndex`
/// type token.
///
/// `LocalScopeIndex` stores raw source slices verbatim (see
/// `sqry-lang-c::scope_index::bind_declaration` —
/// `type_token = source_slice(type_node, ...)`), so a `struct ops *f`
/// declaration produces `"struct ops"` (the pointer lives in the
/// declarator, not the type node). This helper normalises to the bare
/// tag form used by `struct_field_fnptr` / `bindings_by_field` (see
/// the integration test `struct_field_fnptr_merge_across_files`).
fn strip_struct_keyword_and_pointer(type_token: &str) -> &str {
    let trimmed = type_token.trim();
    let after_keyword = trimmed
        .strip_prefix("struct ")
        .or_else(|| trimmed.strip_prefix("union "))
        .or_else(|| trimmed.strip_prefix("enum "))
        .unwrap_or(trimmed);
    // Drop any trailing pointer / qualifier residue (defensive — the
    // LocalScopeIndex builder generally elides the declarator, but
    // multi-token type specifiers like `const struct foo` could leak
    // a trailing `*`).
    after_keyword
        .trim()
        .trim_end_matches(|c: char| c == '*' || c.is_whitespace())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_struct_keyword_strips_struct_prefix() {
        assert_eq!(strip_struct_keyword_and_pointer("struct ops"), "ops");
        assert_eq!(
            strip_struct_keyword_and_pointer("struct file_operations"),
            "file_operations"
        );
    }

    #[test]
    fn strip_struct_keyword_strips_union_and_enum() {
        assert_eq!(strip_struct_keyword_and_pointer("union variant"), "variant");
        assert_eq!(strip_struct_keyword_and_pointer("enum color"), "color");
    }

    #[test]
    fn strip_struct_keyword_no_op_on_bare_tag() {
        assert_eq!(strip_struct_keyword_and_pointer("ops"), "ops");
    }

    #[test]
    fn strip_struct_keyword_trims_trailing_pointer() {
        assert_eq!(strip_struct_keyword_and_pointer("struct ops *"), "ops");
        assert_eq!(strip_struct_keyword_and_pointer("struct ops **"), "ops");
    }

    #[test]
    fn pass5b_stats_total_resolved_sums_binding_and_typematch() {
        let s = Pass5bStats {
            binding_resolved: 3,
            typematch_resolved: 5,
            cap_exceeded: 1,
            stub_fallback: 2,
        };
        assert_eq!(s.total_resolved(), 8);
    }

    #[test]
    fn resolve_c_indirect_calls_short_circuits_when_no_c() {
        // Construct a default CodeGraph (no c_indirect_tables set).
        let mut graph = CodeGraph::new();
        let stats = resolve_c_indirect_calls(&mut graph);
        assert_eq!(stats, Pass5bStats::default());
    }
}
