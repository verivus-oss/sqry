//! `BindingPlane<'g>` — the Phase 2 facade that wraps a `GraphSnapshot` and
//! provides witness-bearing resolution, scope/alias/shadow accessors, and
//! the `explain()` renderer.
//!
//! This is the primary Phase 2 public API. Phases 3-6 (derived analysis DB,
//! query planner, rule layer, provenance/history) consume it as the stable
//! binding-plane surface.
//!
//! # Canonical emission-path conventions (`resolve_shared` contract)
//!
//! The step vocabulary has two pairs of overlapping variants. When emitting
//! steps inside `resolve_shared`, use the following canonical paths:
//!
//! **Visibility rejections** — emit `FilterByVisibility { candidate, reason }`
//! (preferred). The `Rejected { reason: RejectionReason::PrivateVisibility }`
//! variant is reserved for non-visibility-specific rejection contexts where the
//! caller does not have a `VisibilityReason` value (e.g., generic catch-all
//! rejection paths in future P2U work). Never emit both for the same candidate.
//!
//! **Shadow rejections** — emit `ShadowedBy { outer, inner, by_node }` when
//! the resolver detects that an outer binding is superseded by an inner
//! binding (the structured form). The `Rejected { reason: RejectionReason::Shadowed }`
//! variant is for catch-all rejection contexts that do not have scope-pair
//! information available. Prefer `ShadowedBy` whenever both scopes are known.
//!
//! These conventions exist so that downstream consumers (P2U09 T19, P2U10
//! CLI `--explain`, Phases 3-6 rule layer) can match step variants
//! deterministically without guessing which emission path was used.

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::resolution::{SymbolQuery, SymbolResolutionWitness};

use super::alias::AliasEntry;
use super::scope::provenance::{ScopeProvenance, ScopeStableId};
use super::scope::tree::scope_chain;
use super::scope::{Scope, ScopeId};
use super::shadow::ShadowEntry;
use super::witness::render::{WitnessRendering, render_witness};
use super::witness::step::ResolutionStep;
use super::{BindingResult, ResolvedBinding, SymbolClassification, classify_node};
use crate::graph::unified::resolution::{SymbolCandidateBucket, SymbolResolutionOutcome};
use crate::graph::unified::string::id::StringId;

/// Combined resolution result: the existing `BindingResult` alongside the
/// witness with its ordered step trace.
///
/// `BindingResolution` is the primary return type of [`BindingPlane::resolve`].
/// Callers that only need the `BindingResult` can access `resolution.result`;
/// callers that need the step trace can walk `resolution.witness.steps`.
///
/// Note: `BindingResolution` does not implement `Serialize`/`Deserialize`
/// because `SymbolResolutionWitness` does not implement those traits (it
/// contains `Vec<ResolutionStep>` which is Serialize, but the outer struct is
/// not annotated). Use `result` for serializable output, or `witness.steps`
/// for the step trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingResolution {
    /// The resolution result (bindings, outcome, normalized query).
    pub result: BindingResult,
    /// Ordered step trace from the resolver, including bucket probes,
    /// scope entries, alias follows, shadow detections, and the final
    /// `Chose` or `Unresolved` terminal.
    pub witness: SymbolResolutionWitness,
}

/// Short-lived facade borrowing a `GraphSnapshot`.
///
/// Construct via [`GraphSnapshot::binding_plane`]. The facade provides the
/// stable Phase 2 public API:
///
/// - [`BindingPlane::resolve`] — witness-bearing resolution entry point
/// - [`BindingPlane::explain`] — renders a `BindingResolution` as text + JSON
/// - Scope accessors: [`scope_of`], [`scope_chain`], [`scope`],
///   [`scope_by_stable_id`], [`scope_provenance`]
/// - Alias accessors: [`aliases_in`], [`resolve_alias`]
/// - Shadow accessors: [`shadows_in`], [`effective_binding`]
///
/// # Usage from `CodeGraph`
///
/// ```rust,ignore
/// // Two-line MVCC-safe pattern for CodeGraph callers:
/// let snapshot = graph.snapshot();
/// let plane = snapshot.binding_plane();
/// let resolution = plane.resolve(&query);
/// ```
///
/// # Usage from `ConcurrentCodeGraph`
///
/// ```rust,ignore
/// // Three-line pattern for ConcurrentCodeGraph callers:
/// let read_guard = concurrent.read();
/// let snapshot = read_guard.snapshot();
/// let plane = snapshot.binding_plane();
/// let resolution = plane.resolve(&query);
/// ```
///
/// The two/three-line idiom is intentional: `BindingPlane<'g>` borrows from a
/// `GraphSnapshot` and the explicit snapshot handle makes the MVCC lifetime
/// visible to the caller.
pub struct BindingPlane<'g> {
    snapshot: &'g GraphSnapshot,
}

impl<'g> BindingPlane<'g> {
    /// Creates a new `BindingPlane` borrowing the given snapshot.
    ///
    /// Prefer `GraphSnapshot::binding_plane()` over calling this directly.
    #[inline]
    #[must_use]
    pub fn new(snapshot: &'g GraphSnapshot) -> Self {
        Self { snapshot }
    }

    /// Primary entry point — performs witness-bearing resolution and returns
    /// the combined `BindingResolution` with both the `BindingResult` and the
    /// ordered step trace.
    ///
    /// See module-level doc for the canonical emission-path conventions that
    /// govern which step variants are emitted for visibility and shadow events.
    #[must_use]
    pub fn resolve(&self, query: &SymbolQuery<'_>) -> BindingResolution {
        resolve_shared(query, self.snapshot)
    }

    /// Classifies a node as declaration / reference / import / ambiguous.
    ///
    /// Returns [`SymbolClassification::Unknown`] if `node_id` is not present
    /// in the snapshot.
    #[must_use]
    pub fn classify(&self, node_id: NodeId) -> SymbolClassification {
        let Some(entry) = self.snapshot.get_node(node_id) else {
            return SymbolClassification::Unknown;
        };
        classify_node(self.snapshot, node_id, entry.kind)
    }

    // ------------------------------------------------------------------
    // Scope accessors
    // ------------------------------------------------------------------

    /// Returns the `ScopeId` of the scope whose `node` field matches
    /// `node_id`, or `None` if no such scope is allocated.
    ///
    /// Uses `ScopeArena::iter()` for correct generational-index handling.
    #[must_use]
    pub fn scope_of(&self, node_id: NodeId) -> Option<ScopeId> {
        self.snapshot
            .scope_arena()
            .iter()
            .find(|(_, scope)| scope.node == node_id)
            .map(|(id, _)| id)
    }

    /// Returns the scope chain for `scope_id` in innermost-first order.
    ///
    /// Returns an empty `Vec` if `scope_id` is `ScopeId::INVALID` or is not
    /// present in the arena.
    #[must_use]
    pub fn scope_chain(&self, scope_id: ScopeId) -> Vec<ScopeId> {
        scope_chain(self.snapshot.scope_arena(), scope_id)
    }

    /// Returns a reference to the `Scope` record for `scope_id`, or `None` if
    /// the handle is invalid or stale.
    #[must_use]
    pub fn scope(&self, scope_id: ScopeId) -> Option<&Scope> {
        self.snapshot.scope_arena().get(scope_id)
    }

    /// Looks up the live `ScopeId` for a stable scope identity.
    ///
    /// Returns `None` if no provenance record is registered for `stable`.
    #[must_use]
    pub fn scope_by_stable_id(&self, stable: ScopeStableId) -> Option<ScopeId> {
        self.snapshot.scope_by_stable_id(stable)
    }

    /// Looks up the `ScopeProvenance` record for `scope_id`.
    ///
    /// Returns `None` if `scope_id` is invalid, stale, or has no provenance
    /// record in the store.
    #[must_use]
    pub fn scope_provenance(&self, scope_id: ScopeId) -> Option<&ScopeProvenance> {
        self.snapshot.scope_provenance(scope_id)
    }

    // ------------------------------------------------------------------
    // Alias accessors
    // ------------------------------------------------------------------

    /// Returns all alias entries registered for `scope_id`.
    ///
    /// Returns an empty slice if `scope_id` has no entries in the alias table.
    #[must_use]
    pub fn aliases_in(&self, scope_id: ScopeId) -> &[AliasEntry] {
        self.snapshot.alias_table().aliases_in(scope_id)
    }

    /// Resolves an alias for `symbol` in `scope_id`, returning the canonical
    /// target symbol `StringId` if one is registered.
    ///
    /// Returns `None` if no alias is registered for `(scope_id, symbol)`.
    #[must_use]
    pub fn resolve_alias(&self, scope_id: ScopeId, symbol: StringId) -> Option<StringId> {
        self.snapshot.alias_table().resolve_alias(scope_id, symbol)
    }

    // ------------------------------------------------------------------
    // Shadow accessors
    // ------------------------------------------------------------------

    /// Returns all shadow entries registered for `scope_id`, sorted by byte
    /// offset (ascending).
    #[must_use]
    pub fn shadows_in(&self, scope_id: ScopeId) -> Vec<&ShadowEntry> {
        self.snapshot.shadow_table().shadows_in(scope_id)
    }

    /// Returns the effective binding for `symbol` in `scope_id` at
    /// `byte_offset` — i.e., the innermost re-binding strictly before that
    /// offset.
    ///
    /// Returns `None` if no binding for `(scope_id, symbol)` is in scope at
    /// `byte_offset`.
    #[must_use]
    pub fn effective_binding(
        &self,
        scope_id: ScopeId,
        symbol: StringId,
        byte_offset: u32,
    ) -> Option<NodeId> {
        self.snapshot
            .shadow_table()
            .effective_binding(scope_id, symbol, byte_offset)
    }

    // ------------------------------------------------------------------
    // Explain
    // ------------------------------------------------------------------

    /// Renders a `BindingResolution` as a human-readable numbered step list
    /// plus a deterministic JSON value.
    ///
    /// The JSON shape is the stable external contract for the CLI `--explain`
    /// output produced in P2U10. Changes to the JSON keys/structure are a
    /// breaking public-API change.
    #[must_use]
    pub fn explain(&self, resolution: &BindingResolution) -> WitnessRendering {
        render_witness(&resolution.witness)
    }
}

// ---------------------------------------------------------------------------
// resolve_shared — the shared implementation core
// ---------------------------------------------------------------------------

/// Shared helper extracted from the pre-P2U07 `BindingQuery::resolve()` body.
///
/// Called by both `BindingQuery::resolve()` (which returns
/// `BindingResolution.result`) and `BindingPlane::resolve()` (which returns
/// the full `BindingResolution`). This is the drift-proof contract that
/// preserves `BindingQuery::resolve()`'s byte-equal output on its existing
/// call sites — both public entry points delegate to the same code path.
///
/// # Canonical emission-path conventions
///
/// The step trace inside `witness.steps` uses these canonical paths:
///
/// - **Visibility rejections**: prefer `ResolutionStep::FilterByVisibility {
///   candidate, reason }`. Use `Rejected { reason: PrivateVisibility }` only
///   in catch-all contexts where no `VisibilityReason` is available.
///
/// - **Shadow rejections**: prefer `ResolutionStep::ShadowedBy { outer,
///   inner, by_node }` when both enclosing scopes are known. Use `Rejected {
///   reason: Shadowed }` only in catch-all contexts where scope-pair
///   information is absent.
///
/// # Step emission
///
/// The step trace documents the resolver's internal work:
/// 1. `ApplyResolutionMode` — the caller-supplied mode
/// 2. `LookupInBucket` — for each bucket probed (`ExactQualified`,
///    `ExactSimple`, `CanonicalSuffix`)
/// 3. `ConsiderCandidate` — for each candidate in the winning bucket
/// 4. Terminal: `Chose` (single winner), `Ambiguous` (multiple), or
///    `Unresolved` (not found / file not indexed)
///
/// Scope entries, alias follows, and shadow detection steps are emitted by
/// higher-level P2U work that instruments the scope-walk loop. At P2U07
/// the emission covers the bucket probe and terminal steps; the scope-walk
/// instrumentation is added in P2U08.
pub(crate) fn resolve_shared(
    query: &SymbolQuery<'_>,
    snapshot: &GraphSnapshot,
) -> BindingResolution {
    // Delegate to resolve_symbol_with_witness, which calls
    // find_symbol_candidates_with_witness internally and maps the candidate
    // outcome to SymbolResolutionOutcome. The mapping is identical to the
    // pre-P2U07 BindingQuery::resolve() body.
    let mut witness = snapshot.resolve_symbol_with_witness(query);

    // Emit step trace for the resolution work performed.
    emit_resolution_steps(&mut witness, query);

    // Build bindings from witness.candidates, same as the pre-P2U07 body.
    let bindings: Vec<ResolvedBinding> = witness
        .candidates
        .iter()
        .filter_map(|candidate| {
            let entry = snapshot.get_node(candidate.node_id)?;
            Some(ResolvedBinding {
                node_id: candidate.node_id,
                classification: classify_node(snapshot, candidate.node_id, entry.kind),
                bucket: candidate.bucket,
                kind: entry.kind,
            })
        })
        .collect();

    let result = BindingResult {
        query: witness.normalized_query.clone(),
        bindings,
        outcome: witness.outcome.clone(),
    };

    BindingResolution { result, witness }
}

/// Populates `witness.steps` with the ordered step trace for the resolution
/// that already ran (post-hoc emission).
///
/// The step emission documents:
/// 1. `ApplyResolutionMode` — the mode used for this query
/// 2. `LookupInBucket` — for each bucket probed until one with candidates
/// 3. `ConsiderCandidate` — for each candidate in the winning bucket
/// 4. Terminal step — `Chose`, `Ambiguous`, or `Unresolved`
///
/// This is a post-hoc approach: `resolve_symbol_with_witness` has already
/// run and we reconstruct the step trace from the outcome/candidates fields.
/// The pre-P2U07 `resolve_symbol_with_witness` already returns `steps:
/// Vec::new()`, so populating it here is safe and additive.
fn emit_resolution_steps(witness: &mut SymbolResolutionWitness, query: &SymbolQuery<'_>) {
    use super::witness::step::UnresolvedReason;
    use smallvec::SmallVec;

    let steps = &mut witness.steps;

    // Step 1: document the resolution mode applied.
    steps.push(ResolutionStep::ApplyResolutionMode { mode: query.mode });

    // Step 2: document bucket probes. The resolver tries ExactQualified →
    // ExactSimple → CanonicalSuffix (if mode allows suffix). We infer
    // which buckets were tried from the winning bucket and the outcome.
    let winning_bucket = witness.selected_bucket;
    match winning_bucket {
        None => {
            // No bucket produced candidates — all three (or two) were tried
            // and came up empty.
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified,
            });
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactSimple,
            });
            if query.mode
                == crate::graph::unified::resolution::ResolutionMode::AllowSuffixCandidates
            {
                steps.push(ResolutionStep::LookupInBucket {
                    bucket: SymbolCandidateBucket::CanonicalSuffix,
                });
            }
        }
        Some(SymbolCandidateBucket::ExactQualified) => {
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified,
            });
        }
        Some(SymbolCandidateBucket::ExactSimple) => {
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified,
            });
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactSimple,
            });
        }
        Some(SymbolCandidateBucket::CanonicalSuffix) => {
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified,
            });
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactSimple,
            });
            steps.push(ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::CanonicalSuffix,
            });
        }
    }

    // Step 3: document each candidate considered.
    for (rank, candidate) in witness.candidates.iter().enumerate() {
        steps.push(ResolutionStep::ConsiderCandidate {
            node: candidate.node_id,
            rank: u16::try_from(rank).unwrap_or(u16::MAX),
        });
    }

    // Step 4: emit terminal step based on outcome.
    //
    // For Unresolved steps the symbol StringId is read from
    // `witness.symbol`, which was populated by a read-only interner lookup
    // in `resolve_symbol_with_witness`. When the symbol was not found in the
    // interner (i.e., truly not indexed), we fall back to StringId(0) so
    // callers can still match on the step variant.
    let unresolved_symbol = witness
        .symbol
        .unwrap_or_else(|| crate::graph::unified::string::id::StringId::new(0));

    match &witness.outcome {
        SymbolResolutionOutcome::Resolved(node_id) => {
            // `Resolved` is only constructed when exactly one candidate
            // exists (see `resolve_symbol_with_witness`). A debug assertion
            // makes this invariant explicit; no TieBreak step is needed.
            debug_assert_eq!(
                witness.candidates.len(),
                1,
                "Resolved outcome must have exactly one candidate"
            );
            steps.push(ResolutionStep::Chose { node: *node_id });
        }
        SymbolResolutionOutcome::Ambiguous(candidates) => {
            let mut sv: SmallVec<[NodeId; 4]> = SmallVec::new();
            sv.extend_from_slice(candidates);
            steps.push(ResolutionStep::Ambiguous { candidates: sv });
        }
        SymbolResolutionOutcome::NotFound => {
            steps.push(ResolutionStep::Unresolved {
                symbol: unresolved_symbol,
                reason: UnresolvedReason::NotInAnyScope,
            });
        }
        SymbolResolutionOutcome::FileNotIndexed => {
            steps.push(ResolutionStep::Unresolved {
                symbol: unresolved_symbol,
                reason: UnresolvedReason::FileNotIndexed,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::kind::EdgeKind;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::resolution::{FileScope, ResolutionMode, SymbolQuery};
    use crate::graph::unified::storage::arena::NodeEntry;

    // -------------------------------------------------------------------
    // Test helper: build a minimal two-node graph with a Contains edge.
    // -------------------------------------------------------------------
    fn make_graph_with_function(sym: &str) -> CodeGraph {
        let mut graph = CodeGraph::new();
        let path = std::path::PathBuf::from("/plane-tests/test.rs");
        let file_id = graph
            .files_mut()
            .register_with_language(&path, Some(Language::Rust))
            .expect("register file");
        let name = graph.strings_mut().intern(sym).expect("intern sym");
        let qn = graph
            .strings_mut()
            .intern(&format!("crate::{sym}"))
            .expect("intern qn");
        let mod_name = graph.strings_mut().intern("root").expect("intern root");
        let mod_qn = graph.strings_mut().intern("crate").expect("intern crate");
        let mod_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, mod_name, file_id)
                    .with_qualified_name(mod_qn)
                    .with_byte_range(0, 100),
            )
            .expect("alloc mod");
        graph
            .indices_mut()
            .add(mod_id, NodeKind::Module, mod_name, Some(mod_qn), file_id);
        let fn_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, name, file_id)
                    .with_qualified_name(qn)
                    .with_byte_range(5, 80),
            )
            .expect("alloc fn");
        graph
            .indices_mut()
            .add(fn_id, NodeKind::Function, name, Some(qn), file_id);
        graph
            .edges_mut()
            .add_edge(mod_id, fn_id, EdgeKind::Contains, file_id);
        graph
    }

    #[test]
    fn plane_resolve_returns_binding_result_and_witness() {
        let graph = make_graph_with_function("my_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let query = SymbolQuery {
            symbol: "my_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };
        let resolution = plane.resolve(&query);

        // BindingResult must have exactly one binding for "my_fn".
        assert!(
            !resolution.result.bindings.is_empty(),
            "expected at least one binding"
        );
        assert_eq!(
            resolution.result.bindings[0].classification,
            SymbolClassification::Declaration,
        );

        // Witness must carry a non-empty step trace.
        assert!(
            !resolution.witness.steps.is_empty(),
            "step trace must be non-empty after P2U07 emission"
        );
    }

    #[test]
    fn plane_resolve_not_found_emits_unresolved_step() {
        let graph = make_graph_with_function("some_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let query = SymbolQuery {
            symbol: "does_not_exist",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };
        let resolution = plane.resolve(&query);

        assert_eq!(resolution.result.outcome, SymbolResolutionOutcome::NotFound);
        let has_unresolved = resolution.witness.steps.iter().any(|s| {
            matches!(
                s,
                ResolutionStep::Unresolved {
                    reason: super::super::witness::step::UnresolvedReason::NotInAnyScope,
                    ..
                }
            )
        });
        assert!(
            has_unresolved,
            "expected Unresolved step for not-found query"
        );
    }

    #[test]
    fn plane_resolve_found_emits_chose_step() {
        let graph = make_graph_with_function("chosen_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let query = SymbolQuery {
            symbol: "chosen_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };
        let resolution = plane.resolve(&query);

        let has_chose = resolution
            .witness
            .steps
            .iter()
            .any(|s| matches!(s, ResolutionStep::Chose { .. }));
        assert!(has_chose, "expected Chose terminal step for resolved query");
    }

    #[test]
    fn plane_explain_produces_non_empty_text_and_json() {
        let graph = make_graph_with_function("explainable_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let query = SymbolQuery {
            symbol: "explainable_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };
        let resolution = plane.resolve(&query);
        let rendering = plane.explain(&resolution);

        assert!(!rendering.text.is_empty(), "explain text must be non-empty");
        assert!(
            rendering.json.get("steps").is_some(),
            "explain JSON must have a 'steps' field"
        );
    }

    #[test]
    fn binding_query_resolve_matches_plane_resolve_result() {
        // Verify that BindingQuery::resolve() and BindingPlane::resolve().result
        // return identical BindingResult values (byte-equality proof).
        let graph = make_graph_with_function("parity_fn");
        let snapshot = graph.snapshot();

        let query_result = crate::graph::unified::bind::BindingQuery::new("parity_fn")
            .file_scope(FileScope::Any)
            .mode(ResolutionMode::AllowSuffixCandidates)
            .resolve(&snapshot);

        let plane_result = snapshot.binding_plane().resolve(&SymbolQuery {
            symbol: "parity_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        });

        assert_eq!(
            query_result, plane_result.result,
            "BindingQuery::resolve() and BindingPlane::resolve().result must be identical"
        );
    }

    #[test]
    fn scope_of_returns_none_for_unknown_node() {
        let graph = make_graph_with_function("any_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        // Use an invalid NodeId — scope_of must return None.
        let invalid_id = NodeId::new(u32::MAX - 1, 99);
        assert!(plane.scope_of(invalid_id).is_none());
    }

    #[test]
    fn classify_returns_unknown_for_invalid_node() {
        let graph = make_graph_with_function("any_fn2");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let invalid_id = NodeId::new(u32::MAX - 2, 99);
        assert_eq!(plane.classify(invalid_id), SymbolClassification::Unknown);
    }

    #[test]
    fn step_trace_contains_apply_resolution_mode_first() {
        let graph = make_graph_with_function("mode_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();
        let query = SymbolQuery {
            symbol: "mode_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };
        let resolution = plane.resolve(&query);

        let first = resolution.witness.steps.first();
        assert!(
            matches!(
                first,
                Some(ResolutionStep::ApplyResolutionMode {
                    mode: ResolutionMode::Strict
                })
            ),
            "first step must be ApplyResolutionMode with the query mode"
        );
    }

    #[test]
    fn step_trace_exact_qualified_win_emits_single_bucket_step() {
        // When the ExactQualified bucket wins, only one LookupInBucket step
        // is emitted before the ConsiderCandidate/Chose steps.
        let graph = make_graph_with_function("exact_fn");
        let snapshot = graph.snapshot();
        let plane = snapshot.binding_plane();

        // Search by full qualified name so ExactQualified bucket wins.
        let query = SymbolQuery {
            symbol: "crate::exact_fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };
        let resolution = plane.resolve(&query);

        let bucket_steps: Vec<_> = resolution
            .witness
            .steps
            .iter()
            .filter(|s| matches!(s, ResolutionStep::LookupInBucket { .. }))
            .collect();
        // Exactly one bucket probe: ExactQualified won immediately.
        assert_eq!(
            bucket_steps.len(),
            1,
            "expected exactly one LookupInBucket step when ExactQualified wins"
        );
        assert!(matches!(
            bucket_steps[0],
            ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified
            }
        ));
    }
}
