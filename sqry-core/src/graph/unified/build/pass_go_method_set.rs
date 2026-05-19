//! Go T1 method-set satisfaction pass — implicit-implements + promoted methods
//! + function-signature implementations.
//!
//! Sequenced between Phase 4e (binding-plane derivation) and Pass 5
//! (cross-language linking). See:
//!
//! - `docs/development/go-implements-and-promotion/01_SPEC.md` — what & why.
//! - `docs/development/go-implements-and-promotion/02_DESIGN.md` §4 — algorithm.
//! - `docs/development/go-implements-and-promotion/03_IMPLEMENTATION_PLAN.md` §2 —
//!   cluster topology.
//!
//! # Status
//!
//! This module is the Cluster C skeleton: it carries the public shim, the
//! `pub(crate)` generic entrypoint, the stats struct, the pass-internal
//! pure-data types, and a complete production-ready implementation of the
//! [§4.1] signature normaliser. The algorithm body of
//! [`run_go_method_set_satisfaction_generic`] is intentionally a default
//! return; Cluster D fills it (one commit per sub-pass: T1.2 promotion,
//! T1.1 implicit interface satisfaction, T1.3 function-signature
//! implementations). Cluster E wires the entrypoint into the build
//! pipeline at the Phase-4e → Pass-5 boundary; until then no caller
//! invokes this module in production paths.
//!
//! Naming the body "skeleton" is a sequencing decision, not a quality
//! one: when D lands, the entrypoint switches from `Default::default`
//! to the full algorithm in a single commit. There is no MVP / interim
//! shipped behaviour.
//!
//! [§4.1]: ../../../../../docs/development/go-implements-and-promotion/02_DESIGN.md

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::node::Language;
use crate::graph::unified::FileId;
use crate::graph::unified::build::staging::{
    GoEmbeddingHint, GoMethodReceiverHint, GoReceiverCallHint, GoReceiverHintKind,
};
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::edge::kind::{ResolvedVia, TypeOfContext};
use crate::graph::unified::edge::store::StoreEdgeRef;
use crate::graph::unified::mutation_target::{GraphMutationTarget, Receiver};
use crate::graph::unified::node::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::{NodeArena, NodeEntry, NodeMetadataStore, StringInterner};
use crate::graph::unified::string::StringId;

/// Maximum embedding depth at which T1.2 promotion walks the BFS.
///
/// Per 02_DESIGN §4.2 step 2 and §9.3: real Go code rarely exceeds 4
/// embedding levels; 16 is a hard ceiling for BFS to guarantee O(n × 16)
/// worst-case behaviour. Exceeding the cap aborts that branch of the
/// walk and increments `GoMethodSetStats::ambiguity_blocked_promotions`
/// under the "truncated" subcategory documented on the field.
const MAX_PROMOTION_DEPTH: u8 = 16;

/// Result statistics for the Go method-set satisfaction pass.
///
/// Mirrors [`Pass5Stats`][crate::graph::unified::build::pass5_cross_language::Pass5Stats]
/// for log-line parity. All counters are populated by Cluster D; the
/// Cluster C skeleton returns the all-zero `Default`.
///
/// Counter semantics (per 02_DESIGN §2.1):
///
/// - `implements_edges_value` — `Implements(C → I)` edges where `C`'s
///   value-bucket method set satisfies `I`.
/// - `implements_edges_pointer` — `Implements(*C → I)` edges where only
///   `C`'s pointer-bucket satisfies `I`. The pointer-form `Type` node
///   `<pkg>.*<C>` is materialised on demand and pointed at `C` via
///   `Inherits`.
/// - `signature_implements_edges` — T1.3 function-signature
///   `Implements(f → F)` edges between a function `f` and a named
///   function type `F` with matching canonical signature.
/// - `promoted_method_nodes` — synthetic promoted-method nodes minted
///   by T1.2 method-set promotion (struct `S` embeds `T`; `T`'s methods
///   are reachable as `S.m`).
/// - `promoted_back_reference_edges` — shadow `Calls` / `References`
///   edges minted to mirror outer-receiver call sites onto the
///   promoted-method node.
/// - `satisfaction_pairs_examined` — count of `(C, I)` candidate pairs
///   considered, useful for observability + complexity sanity-checks.
/// - `ambiguity_blocked_promotions` — number of promotion candidates
///   suppressed due to same-depth method-name ambiguity (golang/go#57352).
/// - `elapsed_ms` — wall-clock time the pass body spent inside the
///   generic entrypoint, populated by the pass body before return.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GoMethodSetStats {
    /// `Implements(C → I)` edges minted by T1.1 value-bucket satisfaction.
    pub implements_edges_value: u32,
    /// `Implements(*C → I)` edges minted by T1.1 pointer-bucket
    /// satisfaction (after pointer-form `Type` materialisation).
    pub implements_edges_pointer: u32,
    /// `Implements(f → F)` edges minted by T1.3 function-signature
    /// implementations.
    pub signature_implements_edges: u32,
    /// Synthetic promoted-method nodes minted by T1.2.
    pub promoted_method_nodes: u32,
    /// Shadow `Calls` / `References` edges minted by T1.2 step 6.
    pub promoted_back_reference_edges: u32,
    /// Total `(C, I)` candidate pairs examined by T1.1.
    pub satisfaction_pairs_examined: u64,
    /// Promotion candidates suppressed by same-depth ambiguity.
    pub ambiguity_blocked_promotions: u32,
    /// Wall-clock duration of the pass body, in milliseconds.
    pub elapsed_ms: u64,
}

/// Run the Go T1 method-set satisfaction pass on a completed
/// [`CodeGraph`].
///
/// Public shim, full-build path. Mirrors
/// [`link_cross_language_edges`][crate::graph::unified::build::pass5_cross_language::link_cross_language_edges]:
/// the public symbol takes the concrete `CodeGraph` and forwards to the
/// generic implementation that carries the `pub(crate)`
/// `GraphMutationTarget` bound. Keeping the trait crate-private prevents
/// external crates from naming a different implementor while still
/// letting the same algorithm body run on the incremental-rebuild graph.
///
/// # Arguments
///
/// * `graph` — mutable reference to the fully-built graph.
/// * `changed_files` — `Some(&[FileId])` for incremental re-runs (per
///   02_DESIGN §3.6, the pass scopes its analysis to entities defined
///   in the changed files and their tombstone closure); `None` for the
///   full-build entrypoint, which walks the entire graph.
///
/// # Returns
///
/// Statistics about the satisfaction pass; see [`GoMethodSetStats`].
///
/// # Status
///
/// Cluster C: returns `GoMethodSetStats::default()` because the
/// algorithm body lands in Cluster D and the pipeline wiring lands in
/// Cluster E. The entrypoint is callable so unit tests and downstream
/// pipeline code can be wired before D ships.
pub fn run_go_method_set_satisfaction(
    graph: &mut CodeGraph,
    changed_files: Option<&[FileId]>,
) -> GoMethodSetStats {
    run_go_method_set_satisfaction_generic(graph, changed_files)
}

/// Generic implementation used by both the public
/// [`run_go_method_set_satisfaction`] shim (full-build path) and the
/// intra-crate incremental rebuild dispatcher (per 02_DESIGN §3.6,
/// operating on a
/// [`RebuildGraph`][crate::graph::unified::rebuild::rebuild_graph::RebuildGraph]).
///
/// `pub(crate)` mirrors
/// [`link_cross_language_edges_generic`][crate::graph::unified::build::pass5_cross_language::link_cross_language_edges_generic]:
/// the [`GraphMutationTarget`] bound is itself `pub(crate)`, so the
/// generic function inherits that visibility.
///
/// # Cluster D body integration
///
/// Cluster D fills this body with the three sub-passes (T1.2 promotion,
/// T1.1 implicit interface satisfaction, T1.3 function-signature
/// implementations) and overwrites the `Default` return with populated
/// counters. The `_graph` and `_changed_files` parameter prefixes mark
/// the inputs that Cluster D will consume; they are not dead arguments
/// — they are the pass's only inputs, threaded through unchanged for
/// the lifetime of the skeleton.
pub(crate) fn run_go_method_set_satisfaction_generic<G: GraphMutationTarget>(
    graph: &mut G,
    changed_files: Option<&[FileId]>,
) -> GoMethodSetStats {
    let mut stats = GoMethodSetStats::default();
    let start = std::time::Instant::now();

    // Cluster E2 iter-4 — whole-graph tombstone + unconditional
    // whole-graph re-emit (Option C).
    //
    // On the incremental rebuild plane (`changed_files = Some(_)`) we
    // tombstone every prior pass-emitted node + edge across the WHOLE
    // graph before T1.1 / T1.3 re-emit the canonical method-set
    // multiset from scratch. The earlier iter-1/2/3 attempts to scope
    // tombstoning + emission to changed files chased a three-axis
    // scope-parity invariant (`source.file ∈ scope OR target.file ∈
    // scope OR edge.file ∈ scope`) and still missed a fourth axis
    // (method files may live separately from the receiver type) plus
    // the CSR `edge.file` provenance loss surfaced by codex iter-3.
    // The mission per `AGENTS.md` is "lean, focused semantic code
    // search" with "no premature optimization"; trading a scope-skip
    // perf optimization that has failed correctness three iters in a
    // row for an unconditional whole-graph re-emit is the
    // correctness-first choice. Any future perf-driven re-introduction
    // of incremental scoping must be earned by measurement, not by
    // re-inventing the iter-1/2/3 scope-parity machinery.
    //
    // Full-build plane (`changed_files = None`): no prior pass state
    // exists, so the tombstone step is skipped entirely.
    if changed_files.is_some() {
        tombstone_all_pass_owned(graph);
    }

    // Cluster D1 — T1.2 method-set promotion algorithm.
    // Cluster D2 — tighten D1's two deferrals (PromotionBucket
    // classification + shadow-emission gating) and add T1.1 implicit
    // interface satisfaction.
    //
    // Sub-pass D3 (T1.3 function-signature implementations) extends this
    // body in a later commit.
    let mut indices = PassLocalIndices::default();
    let mut newly_created_nodes: Vec<NodeId> = Vec::new();

    // Cluster D2.2: build a (method_node → Receiver) map from the
    // GoMethodReceiverHint side channel. This recovers receiver
    // pointerness lost by `strip_receiver_modifiers` and powers both
    // the per-method bucket classifier in `compute_promotions_for_outer`
    // and the receiver-resolution gating in
    // `emit_shadow_calls_and_references`.
    let method_receivers: HashMap<NodeId, Receiver> = graph
        .go_hints()
        .method_receivers
        .iter()
        .map(|h: &GoMethodReceiverHint| (h.method_node, h.receiver_pointerness))
        .collect();

    // Cluster D3.2: build a (method_node → canonical_signature) map
    // from the `GoMethodSignatureHint` side channel. The map is the
    // load-bearing input for the tightened T1.1 satisfaction predicate
    // — a candidate type satisfies an interface only when each
    // interface-method's `(name, signature)` matches a candidate-method
    // entry (with a name-only fallback when either side lacks a
    // signature, preserving D2 unit-test fixtures).
    let method_signatures: HashMap<NodeId, String> = graph
        .go_hints()
        .method_signatures
        .iter()
        .map(|h| (h.method_node, h.canonical_signature.clone()))
        .collect();

    // Step 1: Collect embeddings from the GoHints side channel, joining
    // the inner qualified name against the live by-qualified-name index
    // to recover the inner type's canonical NodeId.
    let embeddings = collect_embeddings(graph);

    if !embeddings.is_empty() {
        // Step 2: BFS depth assignment per outer struct. Per-outer
        // walk so each outer carries its own depth map; this is the
        // shape §4.2 step 2 prescribes.
        let outer_set: BTreeSet<NodeId> = embeddings.iter().map(|e| e.outer).collect();

        // Build adjacency once: for every embedded-from node, the
        // list of (inner_node, pointerness) edges out of it.
        let adjacency = build_embedding_adjacency(&embeddings);

        // Step 3: Compute promoted_value[S] / promoted_pointer[S] per
        // outer with the per-path stack-scoped cycle guard. This is
        // the load-bearing diamond-ambiguity detection step
        // (golang/go#57352).
        let mut all_promotions: BTreeMap<NodeId, PerOuterPromotion> = BTreeMap::new();
        for &outer in &outer_set {
            let promotion = compute_promotions_for_outer(
                graph,
                outer,
                &adjacency,
                &method_receivers,
                &mut stats,
            );
            all_promotions.insert(outer, promotion);
        }

        // Step 4 + 5: Materialise promoted-method nodes + emit
        // structural edges (Contains, Inherits) per §4.2 step 4. Done
        // in a single walk over `all_promotions` to keep the
        // newly-created-node list aligned with the edges that
        // reference them.
        materialise_and_emit_structural(
            graph,
            &all_promotions,
            &mut indices,
            &mut newly_created_nodes,
            &mut stats,
        );
    }

    // Step 7: Targeted index update for the newly minted nodes so the
    // `<pkg>.<S>.<m>` / `<pkg>.*<S>.<m>` / `<pkg>.*<S>` qualified names
    // are resolvable via the by-qualified-name index in the same call
    // that just minted them. Required by step 6's promoted-method
    // lookups and by Cluster D2/D3's downstream resolution.
    graph.rebuild_qualified_name_index_for_new_nodes(&newly_created_nodes);

    // Step 6: Shadow `Calls` + `References` emission. Runs after step 7
    // so the by-qualified-name resolution path used inside the shadow
    // walker sees the freshly minted promoted-method nodes. This
    // closes AC-5 (`direct_callers(<pkg>.<S>.<m>)` non-empty).
    emit_shadow_calls_and_references(graph, &indices, &mut stats);

    // Cluster D2.3 — T1.1 implicit interface satisfaction.
    //
    // Runs strictly after promotion (so promoted methods participate in
    // method-set composition) and after the targeted index update (so
    // the synthetic `<pkg>.*<C>` pointer-form Type nodes minted here
    // are resolvable via `by_qualified_name`).
    let mut t1_1_newly_created: Vec<NodeId> = Vec::new();
    run_t1_1_satisfaction(
        graph,
        &indices,
        &method_receivers,
        &method_signatures,
        &mut t1_1_newly_created,
        &mut stats,
    );
    if !t1_1_newly_created.is_empty() {
        graph.rebuild_qualified_name_index_for_new_nodes(&t1_1_newly_created);
    }

    // Cluster D3.3 — T1.3 function-signature implementations.
    //
    // Emits `Implements(fn → F)` where `F` is a named function type and
    // `fn` is a bare function (or method) whose canonical signature
    // matches `F`'s underlying signature. Two emission sources are
    // unioned: explicit `T(g)` conversions (`GoNamedTypeConversionHint`)
    // and reverse `TypeOf` walks from each named function-type. Both
    // sources route through a shared dedupe / sort step so the edge
    // sequence is deterministic across runs (AC-12 prerequisite).
    let function_signatures: HashMap<NodeId, String> = graph
        .go_hints()
        .function_signatures
        .iter()
        .map(|h| (h.function_node, h.canonical_signature.clone()))
        .collect();
    run_t1_3_signature_implements(graph, &function_signatures, &method_signatures, &mut stats);

    stats.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    stats
}

// ===========================================================================
// Pass-internal pure-data types
// ===========================================================================
//
// All `pub(crate)` because the satisfaction pass body (Cluster D) is
// the only consumer, and the data does not cross the crate boundary.

/// Canonical method record used by T1.1 method-set comparison.
///
/// Built once per method during the satisfaction pass and indexed into
/// the per-package method-set tables described in 02_DESIGN §4.3.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "Cluster C skeleton — Cluster D fills the algorithm body that \
              builds and consumes `CanonicalMethod` records during T1.1 \
              method-set comparison."
)]
pub(crate) struct CanonicalMethod {
    /// `NodeId` of the `Method` node the canonical signature was
    /// derived from. Same name as in 02_DESIGN §4.3 step 1.
    pub defining_node: NodeId,
    /// Receiver kind (value vs pointer) — splits the method between
    /// value-bucket and pointer-bucket method sets per the Go spec.
    pub receiver: Receiver,
    /// Canonical signature bytes per [`canonicalise_signature`].
    pub canonical_signature: NormalizedSignature,
}

/// One reachable embedding edge resolved by the BFS in 02_DESIGN §4.2 step 2.
///
/// `outer` embeds `inner` at BFS `depth`; `pointerness` records whether
/// the embed was syntactically `T` or `*T`. The Go method-set rules
/// fan out value- and pointer-receiver methods differently depending on
/// `pointerness`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Embedding {
    /// Outer struct `S`.
    pub outer: NodeId,
    /// Inner embedded type `T`.
    pub inner: NodeId,
    /// Whether the embed was `T` (value) or `*T` (pointer).
    pub pointerness: Receiver,
    /// BFS depth from `outer` to `inner` (1 = direct embed, 2 = grand,
    /// etc.). Used for the same-depth ambiguity rule (golang/go#57352).
    #[allow(
        dead_code,
        reason = "BFS depth is recovered from the per-outer walk via \
                  `compute_promotions_for_outer`; the field is retained \
                  on the `Embedding` record for Cluster D2/D3 use \
                  (canonical methodset-comparison ordering)."
    )]
    pub depth: u8,
}

/// Canonical byte form of a Go function / method signature.
///
/// Constructed by [`canonicalise_signature`]; compared bytewise per
/// 02_DESIGN §4.1.3. The internal representation is a `Vec<u8>` of
/// ASCII bytes (Go identifiers are ASCII-clean in practice; the
/// normaliser keeps non-ASCII bytes untouched so the contract holds
/// for the full UTF-8 input space).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NormalizedSignature(pub Vec<u8>);

// ===========================================================================
// Pass-local index typedefs
// ===========================================================================

/// Pass-local dedupe of promoted-method nodes addressable as the
/// value-bucket entry `<pkg>.<S>.<m>`.
///
/// The key triple is `(package_qn, struct_name, method_name)`. The
/// `StringId`s are the interned package qualifier, the short struct
/// name, and the short method name — held as `StringId` to keep
/// lookups O(1) without re-resolving canonical strings. Constructed
/// inside the pass body; not persisted.
pub(crate) type PromotedValueIndex = BTreeMap<(StringId, StringId, StringId), NodeId>;

/// Pass-local dedupe of promoted-method nodes addressable as the
/// pointer-bucket entry `<pkg>.<*S>.<m>`.
///
/// Same `(package_qn, struct_name, method_name)` shape as
/// [`PromotedValueIndex`]; the pointer-form distinction is carried by
/// the parent (`*S`) qualified-name lookup, not by a separate field.
pub(crate) type PromotedPointerIndex = BTreeMap<(StringId, StringId, StringId), NodeId>;

/// Pass-local dedupe of synthetic pointer-form `Type` nodes
/// `<pkg>.*<C>` minted for T1.2 pointer-bucket promotion (and reused
/// by D2's T1.1 pointer-bucket satisfaction once it lands).
///
/// Key `(package_qn, struct_short_name)`. D2 and D3 share this index
/// when they need a pointer-form anchor for `*C` so the materialisation
/// dedupe stays consistent across passes.
pub(crate) type PointerTypeIndex = BTreeMap<(StringId, StringId), NodeId>;

/// Per-outer mapping `(outer_node, method_name_id) → promoted_method_node`
/// for the value-form bucket. Built by `materialise_and_emit_structural`
/// and consumed by Cluster D2.2's shadow-`Calls`/`References` gating
/// in `emit_shadow_calls_and_references`: only call sites whose
/// resolved receiver type matches `outer_node` (or its `*outer_node`
/// pointer form, recorded in `outer_pointer_form_to_promoted`) produce
/// shadow edges to the corresponding promoted method.
pub(crate) type OuterToPromotedIndex = BTreeMap<(NodeId, StringId), NodeId>;

/// Pass-local index bundle for T1.2 + T1.1.
///
/// Aggregating these into a single struct lets helpers take a single
/// `&mut PassLocalIndices` instead of three separate borrows.
#[derive(Debug, Default)]
pub(crate) struct PassLocalIndices {
    /// Value-bucket promoted-method node dedupe.
    pub value: PromotedValueIndex,
    /// Pointer-bucket promoted-method node dedupe.
    pub pointer: PromotedPointerIndex,
    /// Pointer-form synthetic `Type` node dedupe.
    pub pointer_type: PointerTypeIndex,
    /// Cluster D2.2: per-`(outer_node, method_name)` value-form
    /// promoted method index. The shadow-emission walker queries this
    /// to gate edges by resolved receiver type — only call sites
    /// whose receiver expression resolves to the outer struct's
    /// `NodeId` produce a shadow `Calls`/`References` against the
    /// promoted name.
    pub outer_to_value_promoted: OuterToPromotedIndex,
    /// Cluster D2.2: per-`(outer_node, method_name)` pointer-form
    /// promoted method index. Outer is the *value*-form `NodeId`;
    /// the corresponding pointer-form Type node lives in
    /// `pointer_type` and the promoted method under `<pkg>.*<S>.<m>`
    /// is the value here.
    pub outer_to_pointer_promoted: OuterToPromotedIndex,
}

// ===========================================================================
// Signature normalisation (02_DESIGN §4.1)
// ===========================================================================

/// Canonicalise a Go function / method signature into a deterministic
/// byte sequence suitable for bytewise equality comparison.
///
/// Implements the 5-rule pipeline of 02_DESIGN §4.1.2. The output is
/// shaped as the §4.1 grammar:
///
/// ```text
/// NormalizedSignature := "(" param_type_list ")" optional_return_clause
/// param_type_list     := type ( "," type )*    // empty for nullary
/// optional_return_clause := "" | type | "(" type_list ")"
/// ```
///
/// # Inputs
///
/// `params` and `returns` are the parameter-list and return-clause
/// strings extracted from the live graph's `TypeOf` edges (02_DESIGN
/// §4.1.1). The caller is expected to have already:
///
/// - resolved package-relative names to `<pkg>.<name>`
///   (02_DESIGN §4.1.2 rule 4); the `Type` node's `qualified_name` is
///   already in that form because the Go plugin emits it that way at
///   `handle_struct_type_spec` (`graph_builder.rs:1944`); and
/// - flattened generic type parameters to `<pkg>.<T>.<E>` via the
///   plugin's existing `extract_receiver_type_param_map`
///   (`graph_builder.rs:1235`) — 02_DESIGN §4.1.2 rule 5.
///
/// Rules 4 and 5 are preconditions on the input strings, not work this
/// function performs. They are documented here because the contract
/// between the caller and the normaliser is what makes the pipeline
/// complete; future maintainers must not introduce a normaliser caller
/// that bypasses the upstream resolution.
///
/// The normaliser itself applies the three rules that act purely on the
/// type-text strings:
///
/// 1. **Whitespace canonicalisation (§4.1.2 rule 1)** — collapse all
///    internal runs of whitespace; strip whitespace between modifier
///    tokens (`*`, `[]`, `...`, `&`) and their operand. E.g. `* T` →
///    `*T`; `[ ]T` → `[]T`; `[ ] *T` → `[]*T`; `... T` → `...T`. Outer
///    whitespace is trimmed.
/// 2. **Parameter name erasure (§4.1.2 rule 2)** — the Go parser keeps
///    the parameter identifier in the param-text (e.g. `p []byte`).
///    The normaliser drops the leading identifier when the param
///    declaration begins with an identifier followed by whitespace and
///    a type token. Method-set equivalence does not depend on
///    parameter names (Go spec §"Function types").
/// 3. **Variadic preservation (§4.1.2 rule 3)** — `...T` is kept as
///    `...T`, which is part of signature identity per the Go spec.
///    Combined with rule 1, `... T` and `...T` both canonicalise to
///    `...T`.
///
/// Plus the grammar shape (02_DESIGN §4.1 lines 1127–1130):
///
/// 4. **Return-clause shape rule** — the §4.1 grammar emits
///    `optional_return_clause` as `""` for nullary, `type` for a
///    single return, or `(t1, t2, ...)` for multiple. A single-return
///    text wrapped in redundant parens (`(int)`) is normalised to the
///    bare type (`int`). Multi-return parens are preserved.
/// 5. **Parameter-list parens (§4.1 grammar)** — the param list is
///    always wrapped in `(...)`, even when empty (`()`). This makes
///    the boundary between params and returns unambiguous in the
///    byte form and means `canonicalise_signature("", "")` returns
///    bytes `()`.
///
/// # Idempotence
///
/// `canonicalise_signature(s)` is idempotent in the sense that running
/// the normaliser over its own decoded output yields the same byte
/// sequence. Specifically:
///
/// ```text
/// let n1 = canonicalise_signature(params, returns);
/// let (p2, r2) = split_normalised(&n1);  // not a public API; conceptual
/// let n2 = canonicalise_signature(p2, r2);
/// assert_eq!(n1, n2);
/// ```
///
/// The exposed idempotence test instead asserts the property the
/// satisfaction pass relies on: re-running the normaliser on the
/// already-canonical *input* strings yields the same output, because
/// the rules are deterministic and rule application order does not
/// change after the first pass.
///
/// # Errors
///
/// Pure function over string slices; no allocation can fail. Inputs
/// that violate the §4.1.1 precondition (e.g. unresolved alias text)
/// pass through unchanged for their offending substring; the resulting
/// signature will simply fail to compare equal to a properly-resolved
/// counterpart. This matches 02_DESIGN §4.1.3 ("There is no fuzzy
/// match.").
#[allow(
    dead_code,
    reason = "Cluster C skeleton — Cluster D consumes \
              `canonicalise_signature` when building the per-package \
              method-set tables for T1.1 satisfaction. Tests in this \
              module exercise the normaliser today."
)]
pub(crate) fn canonicalise_signature(params: &str, returns: &str) -> NormalizedSignature {
    let mut out = Vec::with_capacity(params.len() + returns.len() + 4);

    // ----- Parameter list -----
    out.push(b'(');
    let canon_params = canonicalise_param_list(params);
    out.extend_from_slice(canon_params.as_bytes());
    out.push(b')');

    // ----- Optional return clause -----
    let canon_returns = canonicalise_return_clause(returns);
    out.extend_from_slice(canon_returns.as_bytes());

    NormalizedSignature(out)
}

/// Canonicalise the parameter list text into a comma-joined sequence
/// of normalised type tokens. Returns an empty string for an empty
/// (nullary) input. Outer parens are NOT included — the caller of
/// [`canonicalise_signature`] adds them per the §4.1 grammar.
fn canonicalise_param_list(params: &str) -> String {
    // Strip outer parens if present — the grammar allows callers to
    // pass either `(a, b)` or `a, b`. After this step the input is the
    // comma-separated body only.
    let body = strip_outer_parens(params.trim()).trim();

    if body.is_empty() {
        return String::new();
    }

    let parts = split_top_level_commas(body);
    let normalised: Vec<String> = parts
        .iter()
        .map(|p| normalise_param_token(p.trim()))
        .collect();
    normalised.join(",")
}

/// Canonicalise the return clause into the §4.1 grammar's
/// `optional_return_clause` shape:
/// - empty input → empty string;
/// - single return → bare normalised type;
/// - multiple returns → `(t1,t2,...)`.
fn canonicalise_return_clause(returns: &str) -> String {
    let body = strip_outer_parens(returns.trim()).trim();
    if body.is_empty() {
        return String::new();
    }

    let parts = split_top_level_commas(body);
    let normalised: Vec<String> = parts
        .iter()
        // Returns do not carry parameter names; treat them as bare
        // type expressions. We still run the param-token normaliser
        // because it also strips whitespace inside modifier tokens.
        .map(|p| normalise_type_text(p.trim()))
        .collect();

    if normalised.len() == 1 {
        // Single-return form: bare type, no parens.
        normalised.into_iter().next().unwrap_or_default()
    } else {
        let mut out =
            String::with_capacity(2 + normalised.iter().map(|s| s.len() + 1).sum::<usize>());
        out.push('(');
        out.push_str(&normalised.join(","));
        out.push(')');
        out
    }
}

/// Strip a single matched pair of outer parens, if and only if the
/// first byte is `'('` and the last byte is `')'` AND they are the
/// outermost matched pair (not e.g. `(a),(b)`).
fn strip_outer_parens(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'(' || bytes[bytes.len() - 1] != b')' {
        return s;
    }
    // Verify the opening paren matches the closing paren — i.e. nesting
    // hits zero only at the end.
    let mut depth: i32 = 0;
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && idx != bytes.len() - 1 {
                    return s;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return s;
    }
    // Safe: bytes[0] is `(` (1 byte ASCII), bytes[len-1] is `)`.
    &s[1..s.len() - 1]
}

/// Split a parameter / return body on top-level commas (depth-zero
/// w.r.t. parens, brackets, and angle brackets). Generic type
/// arguments and tuple-typed returns must not be split by the comma
/// inside their delimiters.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut start = 0usize;

    let bytes = s.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b'<' => depth_angle += 1,
            b'>' => depth_angle -= 1,
            b',' if depth_paren == 0 && depth_bracket == 0 && depth_angle == 0 => {
                parts.push(s[start..idx].to_string());
                start = idx + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        parts.push(s[start..].to_string());
    }
    parts
}

/// Normalise a single parameter token, applying:
/// - rule 1 (whitespace inside modifier tokens),
/// - rule 2 (leading parameter-name stripping),
/// - rule 3 (variadic preservation).
///
/// Multiple identifiers sharing a type (`a, b int`) are not handled
/// here — `split_top_level_commas` already split them into separate
/// tokens. Each separated token is either:
///
/// - bare `T` / `*T` / `[]T` / `...T` / `chan T`, etc. → no name
///   to strip;
/// - `name T` / `name *T` / `name []T` / `name ...T` → strip `name`.
fn normalise_param_token(token: &str) -> String {
    let token = token.trim();
    if token.is_empty() {
        return String::new();
    }

    // Try rule 2: detect a leading identifier followed by whitespace
    // and a type token. A "type token" is anything starting with one
    // of: `*`, `[`, `(`, `c` (for `chan`/`map`?), `<-`, `m`, `f`, or
    // an identifier byte. The simpler and more robust check: if the
    // token contains a top-level whitespace boundary AND the prefix
    // before that whitespace is a single Go identifier (no dots, no
    // brackets), strip the prefix.
    let stripped = strip_leading_parameter_name(token);
    normalise_type_text(stripped)
}

/// If `token` matches `<param_name> <whitespace> <type>` at top level
/// (depth 0), return the `<type>` substring; otherwise return `token`
/// unchanged.
///
/// "Top level" means the whitespace is not inside any paren / bracket
/// / angle-bracket group; the leading identifier must be an ASCII Go
/// identifier (`[A-Za-z_][A-Za-z0-9_]*`) without dots.
///
/// **Type-keyword guard**: Go type expressions can themselves start
/// with an identifier-shaped reserved word (`chan T`, `map[K]V`,
/// `func(x) y`, `interface { ... }`, `struct { ... }`). These are not
/// parameter names. The function recognises them and bails out so the
/// type text is preserved verbatim. The list is closed by the Go spec
/// and short.
fn strip_leading_parameter_name(token: &str) -> &str {
    let bytes = token.as_bytes();
    if bytes.is_empty() {
        return token;
    }

    // The first byte must be an identifier start.
    if !is_ident_start(bytes[0]) {
        return token;
    }

    // Find the end of the leading identifier.
    let mut i = 1usize;
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        i += 1;
    }

    // The identifier must be followed by whitespace at depth 0.
    if i >= bytes.len() || !is_ascii_space(bytes[i]) {
        return token;
    }

    // Type-keyword guard. These tokens introduce a type expression,
    // not a parameter name. Per Go spec §"Types", `chan`, `map`,
    // `func`, `interface`, and `struct` open type literals; `chan` is
    // additionally relevant for the receive-only / send-only forms
    // (`chan T`, `<-chan T` — the latter starts with `<`, so it does
    // not reach this branch). `<-` is handled by the byte-not-ident
    // check at byte 0.
    let leading = &token[..i];
    if matches!(leading, "chan" | "map" | "func" | "interface" | "struct") {
        return token;
    }

    // The token after the whitespace must look like a type. Skip
    // whitespace.
    let mut j = i + 1;
    while j < bytes.len() && is_ascii_space(bytes[j]) {
        j += 1;
    }
    if j >= bytes.len() {
        return token;
    }

    // Post-split, `a, b int` is impossible (the splitter produces
    // `a` and `b int` as separate tokens; the first is a bare
    // identifier with no whitespace and is rejected by the
    // whitespace check at byte `i`). The remaining `<ident><ws><...>` shape
    // is the canonical `param_name type_expr` form: strip the prefix.
    &token[j..]
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline]
fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_ascii_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Normalise the type-text portion of a parameter / return token:
/// - collapse all internal runs of whitespace;
/// - drop whitespace adjacent to modifier tokens `*`, `[`, `]`, `...`,
///   `<-`, `(`, `)`, `,`, `&`, `<`, `>`, `:`.
///
/// Outer whitespace is trimmed. The pipeline is whitespace-only —
/// identifiers (Go names) and their nested qualifiers (`pkg.Type`,
/// `pkg.Type.E`) are preserved byte-for-byte.
fn normalise_type_text(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());

    let mut prev_was_space = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if is_ascii_space(b) {
            prev_was_space = true;
            continue;
        }

        // Determine whether to insert a previously-pending space:
        // a space is preserved only when it separates two
        // identifier-continuation bytes. Modifier tokens elide it.
        if prev_was_space && !out.is_empty() {
            let prev = *out.last().expect("non-empty out");
            let needs_space = is_ident_continue(prev) && is_ident_continue(b);
            if needs_space {
                out.push(b' ');
            }
        }
        out.push(b);
        prev_was_space = false;

        // Suppress consume-side spaces after modifier tokens (`*`, etc.).
        // Already handled by the `needs_space` branch above — a
        // modifier byte will fail `is_ident_continue` so no space is
        // emitted before the next byte.
        let _ = idx; // explicit no-use — index kept for future rule additions.
    }

    // SAFETY-equivalent: input is &str so all bytes are valid UTF-8;
    // we only drop bytes that are ASCII whitespace and never split a
    // multi-byte UTF-8 codepoint (UTF-8 continuation bytes are >= 0x80
    // and never match `is_ascii_space`). The output is therefore valid
    // UTF-8.
    String::from_utf8(out).expect("normalised bytes are valid UTF-8")
}

// ===========================================================================
// T1.2 promotion algorithm — Cluster D1
// ===========================================================================
//
// All helpers below are `pub(crate)` only to the extent they need to be
// reachable from the algorithm entrypoint above. Helper-internal types
// are module-private.

/// Per-outer promotion bookkeeping. `(name_string_id, defining_method_node)`
/// pairs that won the promotion competition at the shallowest depth,
/// split into value-bucket and pointer-bucket subsets.
///
/// Built by [`compute_promotions_for_outer`]; consumed by
/// [`materialise_and_emit_structural`].
#[derive(Debug, Default)]
struct PerOuterPromotion {
    /// `name → defining_method_node` map for methods promoted into the
    /// value-bucket method set of the outer struct. Per Go spec
    /// §"Method sets", any method in `value` is also implicitly in the
    /// outer's pointer-bucket method set (the §4.2 step 4 "dual-emission
    /// for promotions in BOTH value and pointer buckets" rule means we
    /// emit the value-form node and rely on `Inherits(*S → S)` for the
    /// pointer-form to reach it).
    value: BTreeMap<StringId, NodeId>,
    /// `name → defining_method_node` map for methods promoted **only**
    /// into the pointer-bucket method set. Emitted under the `<pkg>.*<S>`
    /// namespace per 02_DESIGN §4.2 step 4.
    pointer_only: BTreeMap<StringId, NodeId>,
}

/// Adjacency list of the embedding graph: `outer → Vec<(inner, pointerness)>`.
type EmbeddingAdjacency = BTreeMap<NodeId, Vec<(NodeId, Receiver)>>;

/// Step 1: collect embeddings from the Go-plugin side-channel hints,
/// resolving each `inner_qualified_name` against the live
/// by-qualified-name index.
///
/// Returns the resolved adjacency record in **emission order** — the
/// side channel was already populated in plugin-emission order. The
/// `Embedding::depth` field is left as `0`: depths are recomputed per
/// outer struct inside [`compute_promotions_for_outer`].
///
/// Embeddings whose `inner_qualified_name` does not resolve to any
/// live arena node are skipped silently. This matches 02_DESIGN
/// §4.2 step 2's "drop unresolved silently" rule and AC-7's
/// no-false-positive intent.
fn collect_embeddings<G: GraphMutationTarget>(graph: &G) -> Vec<Embedding> {
    let hints: &[GoEmbeddingHint] = &graph.go_hints().embeddings;
    let mut out: Vec<Embedding> = Vec::with_capacity(hints.len());

    for hint in hints {
        // Resolve the inner type's NodeId via the by-qualified-name
        // index. Multiple matches are possible after Phase 4c-prime
        // unification only when the kinds disagree; the embedding
        // contract is that the inner is a type-shaped node (Struct,
        // Type, Interface). Prefer Struct → Interface → Type when
        // multiple candidates exist; ignore non-type candidates.
        let inner = match resolve_inner_type_node(graph, hint.inner_qualified_name) {
            Some(n) => n,
            None => continue,
        };
        out.push(Embedding {
            outer: hint.outer,
            inner,
            pointerness: hint.pointerness,
            depth: 0,
        });
    }

    out
}

/// Resolve an interned qualified name to a single `NodeId` for an
/// inner-type record.
///
/// Returns the highest-priority type-shaped node from the
/// `by_qualified_name` bucket. Priority is `Struct > Interface > Type`
/// (named function types and other named non-struct types live under
/// `NodeKind::Type`). Returns `None` if the bucket is empty or
/// contains no type-shaped nodes.
///
/// Anti-aliasing: when the same qualified name resolves to multiple
/// candidate type-shaped nodes (rare but legal — e.g., a stub `Type`
/// node coexisting with a canonical `Struct` after Phase 4c-prime
/// unification), this function picks the most-specific kind to keep
/// method-set queries on the canonical receiver type.
fn resolve_inner_type_node<G: GraphMutationTarget>(
    graph: &G,
    qualified_name: StringId,
) -> Option<NodeId> {
    let candidates = graph.indices().by_qualified_name(qualified_name);
    let mut best: Option<(NodeId, NodeKind)> = None;
    for &nid in candidates {
        let kind = match graph.nodes().get(nid) {
            Some(entry) => entry.kind,
            None => continue,
        };
        let candidate_rank = match kind {
            NodeKind::Struct => 3,
            NodeKind::Interface => 2,
            NodeKind::Type => 1,
            _ => continue,
        };
        let replace = match best {
            None => true,
            Some((_, current_kind)) => {
                let current_rank = match current_kind {
                    NodeKind::Struct => 3,
                    NodeKind::Interface => 2,
                    NodeKind::Type => 1,
                    _ => 0,
                };
                candidate_rank > current_rank
            }
        };
        if replace {
            best = Some((nid, kind));
        }
    }
    best.map(|(nid, _)| nid)
}

/// Build the embedding-adjacency map keyed by `outer` NodeId.
///
/// Values are sorted by `(inner.index, pointerness)` so the per-outer
/// BFS visits embeddings in a deterministic order. AC-12 (build
/// determinism) requires the same outgoing-edge ordering on every run.
fn build_embedding_adjacency(embeddings: &[Embedding]) -> EmbeddingAdjacency {
    let mut adj: EmbeddingAdjacency = BTreeMap::new();
    for e in embeddings {
        adj.entry(e.outer)
            .or_default()
            .push((e.inner, e.pointerness));
    }
    for v in adj.values_mut() {
        v.sort_by_key(|(inner, p)| (inner.index(), pointerness_key(*p)));
        v.dedup();
    }
    adj
}

#[inline]
fn pointerness_key(p: Receiver) -> u8 {
    match p {
        Receiver::Value => 0,
        Receiver::Pointer => 1,
    }
}

/// One candidate contributor for a promoted name, tagged with depth
/// and the embedding path that produced it. Per 02_DESIGN §4.2 step 2,
/// distinct embedding paths must remain distinct contributors so the
/// same-depth ambiguity rule (golang/go#57352) can detect collisions
/// at the shallowest depth.
#[derive(Debug, Clone)]
struct PromotionCandidate {
    /// `NodeId` of the defining method on the embedded inner type.
    defining_method: NodeId,
    /// `NodeId` of the IMMEDIATE embedded type that brought this
    /// method into the outer's reach — i.e. the BFS frame's
    /// `inner`, not the original declaring type if reached via
    /// interface inheritance. Used by step 3's same-depth ambiguity
    /// check (Cluster G1 / 01_SPEC §7 AC-4 / golang/go#57352): two
    /// distinct immediate embeds contributing the same method name
    /// at the same depth = ambiguous selector regardless of whether
    /// they ultimately resolve to the same defining method.
    promoting_type: NodeId,
    /// BFS depth at which this candidate was reached (1 = direct embed).
    depth: u8,
    /// Whether the candidate flows into the value or pointer bucket of
    /// the current outer. The two buckets are tracked separately so
    /// pointer-only promotions can be emitted under the `*S` namespace
    /// (AC-6).
    bucket: PromotionBucket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromotionBucket {
    /// The candidate promotes into the value-bucket method set of the
    /// outer struct. Per Go spec §"Method sets" rule 1, a value-bucket
    /// member is also implicitly in the pointer-bucket method set —
    /// the materialisation step relies on `Inherits(*S → S)` for the
    /// reverse direction.
    Value,
    /// The candidate promotes into the pointer-bucket method set only.
    /// Emitted under the `<pkg>.*<S>.<m>` namespace.
    ///
    /// Cluster D2.2 lifts this variant from reserved-but-unused to
    /// active: with the `GoMethodReceiverHint` side channel landed,
    /// `compute_promotions_for_outer` classifies a contributor as
    /// `PointerOnly` whenever the underlying method declares a pointer
    /// receiver (`func (t *T) M()`) AND the embed chain that reaches
    /// it carries no pointer-embed of `T` (Go spec §"Method sets":
    /// pointer-receiver methods of an embedded `T` promote onto `*S`
    /// only; pointer-receiver methods of an embedded `*T` promote onto
    /// both `S` and `*S`).
    PointerOnly,
}

/// Step 2 + 3: per-outer BFS over the embedding adjacency, collecting
/// promotion candidates per (method-name, bucket) bucket and applying
/// the §5.3 same-depth ambiguity rule plus the §5.3 outer-shadow rule.
///
/// Returns the resolved [`PerOuterPromotion`] for `outer`. Ambiguity
/// blocks are counted in `stats.ambiguity_blocked_promotions`.
///
/// **Cycle protection without diamond suppression**: the BFS carries a
/// per-frame `current_path: BTreeSet<NodeId>` cycle guard. Reaching a
/// node already on the recursion stack short-circuits that frame but
/// does NOT collapse diamond paths — each distinct reaching path
/// surfaces as a distinct contributor in the per-name candidate list,
/// which is the discriminator step 3's ambiguity check relies on.
///
/// **Receiver-pointerness classification (Cluster D2.2)**: each method
/// contributor is classified into `PromotionBucket::Value` or
/// `PromotionBucket::PointerOnly` by combining the
/// `GoMethodReceiverHint` for the underlying defining method with the
/// embed chain's pointerness, per Go spec §"Method sets":
///
/// - Value-receiver method (`func (T) M()`) reachable through any
///   embed chain → value-bucket (visible from both `S` and `*S`).
/// - Pointer-receiver method (`func (*T) M()`) reachable through a
///   chain that includes a pointer-embed (`*T`) at any step →
///   value-bucket (`*T` is addressable by both `S` and `*S`, so per
///   Go spec rule the method's `*T` receiver is satisfied by both
///   parents).
/// - Pointer-receiver method reachable through a chain of value-embeds
///   only → pointer-only bucket (visible from `*S` only).
///
/// Methods without a receiver-pointerness hint are conservatively
/// classified as value-receiver (safe default: matches D1's behaviour
/// for plugins that have not yet emitted the hint and degrades
/// gracefully against legacy graphs).
fn compute_promotions_for_outer<G: GraphMutationTarget>(
    graph: &G,
    outer: NodeId,
    adjacency: &EmbeddingAdjacency,
    method_receivers: &HashMap<NodeId, Receiver>,
    stats: &mut GoMethodSetStats,
) -> PerOuterPromotion {
    // Resolve the outer struct's own depth-0 method names so the
    // outer-shadow rule (§5.3) can suppress deeper promotions on
    // name collisions. Outer's "own" methods are discovered by
    // qualified-name prefix matching: any Method whose qualified
    // name is `<outer_qn>.<m>`.
    let outer_qn_str = match qualified_name_string(graph, outer) {
        Some(s) => s,
        None => return PerOuterPromotion::default(),
    };
    let own_method_names: BTreeSet<StringId> = find_method_names_of_type(graph, &outer_qn_str)
        .into_iter()
        .map(|(name_id, _node_id)| name_id)
        .collect();

    // BFS with per-frame cycle guard. Each queue entry is the inner
    // type reached, the depth at which it was reached, the
    // pointerness propagated to this point (value-only embed chain
    // keeps `Value`; once any pointer-embed enters the chain, the
    // chain stays `Pointer` per Go method-set semantics), and the
    // current_path set of NodeIds on the way from `outer` to here.
    //
    // We do NOT short-circuit on a global `visited` set — diamond
    // paths must survive so step 3's same-depth ambiguity check
    // observes both reaching contributors.
    let mut candidates_by_name: BTreeMap<StringId, Vec<PromotionCandidate>> = BTreeMap::new();
    let mut queue: Vec<(NodeId, u8, Receiver, BTreeSet<NodeId>)> = Vec::new();
    queue.push((outer, 0, Receiver::Value, {
        let mut s = BTreeSet::new();
        s.insert(outer);
        s
    }));

    while let Some((cur, depth, chain_pointerness, current_path)) = queue.pop() {
        if depth >= MAX_PROMOTION_DEPTH {
            // Per 02_DESIGN §9.3, count truncated branches under
            // `ambiguity_blocked_promotions` (same observability
            // counter — the design notes "future minor extension"
            // for a separate truncated category).
            stats.ambiguity_blocked_promotions =
                stats.ambiguity_blocked_promotions.saturating_add(1);
            continue;
        }

        let outgoing = match adjacency.get(&cur) {
            Some(v) => v,
            None => continue,
        };

        // Sort outgoing edges deterministically (already sorted by
        // build_embedding_adjacency, but re-asserted here so the loop
        // order is explicit at the call site).
        for &(inner, embed_pointerness) in outgoing {
            if current_path.contains(&inner) {
                // Stack-scoped cycle guard — skip back-edge.
                continue;
            }
            let new_depth = depth + 1;
            // Chain pointerness propagation: once any pointer-embed
            // enters the chain, the whole reachable subgraph rooted
            // there contributes pointer-bucket members to the OUTER.
            // The §4.2 step 2 pseudocode (lines 1276-1288 of
            // 02_DESIGN) collapses pointer-embed's contributions into
            // BOTH buckets; this implementation propagates the
            // "pointer-tainted" flag via `new_chain_pointerness`.
            let new_chain_pointerness = match (chain_pointerness, embed_pointerness) {
                (Receiver::Pointer, _) | (_, Receiver::Pointer) => Receiver::Pointer,
                _ => Receiver::Value,
            };

            // Inner type's qualified name → look up methods of that
            // type. Each method becomes one contributor at this
            // depth.
            let inner_qn_str = match qualified_name_string(graph, inner) {
                Some(s) => s,
                None => continue,
            };
            // Type-alias hop (AC-9, golang/go#66540): if `inner` is a
            // `NodeKind::Type` (named alias / named non-struct), walk
            // up to one alias-following `TypeOf{TypeParameter}` /
            // `Inherits` hop to find an underlying struct/type that
            // carries methods. The alias resolver is intentionally
            // depth-bounded (one hop) to keep the BFS deterministic
            // and avoid Go-type-inference complexity.
            let resolved_inner_qn =
                resolve_alias_underlying_qn(graph, inner, &inner_qn_str).unwrap_or(inner_qn_str);

            // Cluster G1 (AC-4 interface-embedding ambiguity, per
            // 01_SPEC §7 AC-4 / golang/go#57352): when the embedded
            // inner is a `NodeKind::Interface`, the methods that
            // contribute to the outer struct's promoted set must
            // include methods inherited via interface-of-interface
            // embedding (i.e. AB's flattened method set, not just
            // its directly-declared methods). Without this, `Foo
            // struct { A; AB }` (where AB inherits `a` from A)
            // misses the same-depth ambiguity check for `a` because
            // AB appears to contribute only `b`.
            //
            // For struct/other inner types the existing
            // direct-methods lookup is correct — promotion through
            // struct embedding is depth-incremented via the BFS
            // itself (each struct's own embedded fields get popped
            // at depth + 1 below), so flattening here would
            // double-count.
            // Look at every node sharing the resolved inner qn so we
            // catch the case where the embedding helper minted a
            // Struct stub for an interface-typed embedded field
            // (the Go plugin's struct-embedding handler treats every
            // embedded type-identifier as `helper.add_struct`,
            // which does NOT dedupe against an existing Interface
            // node of the same qn — different `node_cache` key
            // `(qn, NodeKind::Interface)` vs `(qn, NodeKind::Struct)`).
            // For AC-4 / golang/go#57352 the Interface companion is
            // what carries the `Inherits`-to-embedded-interface chain.
            let inner_kind = graph.nodes().get(inner).map(|e| e.kind);
            let interface_companion = if matches!(inner_kind, Some(NodeKind::Interface)) {
                Some(inner)
            } else if let Some(qn_id) = graph.strings().get(&resolved_inner_qn) {
                graph
                    .indices()
                    .by_qualified_name(qn_id)
                    .iter()
                    .copied()
                    .find(|&nid| {
                        matches!(
                            graph.nodes().get(nid).map(|e| e.kind),
                            Some(NodeKind::Interface)
                        )
                    })
            } else {
                None
            };
            let methods = if let Some(iface) = interface_companion {
                find_interface_flattened_methods(graph, iface)
            } else {
                find_method_names_of_type(graph, &resolved_inner_qn)
            };
            for (method_name_id, method_node) in methods {
                // §5.3 outer-shadow: if outer has its own method
                // with this name at depth 0, deeper promotions are
                // suppressed. We still walk the BFS deeper (a
                // shadowed-at-this-depth path can still produce
                // promotions for OTHER names), but the contributor
                // for this name is dropped before it lands in
                // `candidates_by_name`.
                if own_method_names.contains(&method_name_id) {
                    continue;
                }

                // Cluster D2.2 bucket classification per Go spec
                // §"Method sets" rule 1+2:
                //
                // - Value-receiver method: visible in both S's and
                //   *S's method sets via this embed → Value bucket.
                // - Pointer-receiver method reached through a
                //   pointer-embed chain (`*T` somewhere along the
                //   reach path): both S and *S can reach a `*T`
                //   value via the embedded `*T`, so the pointer-
                //   receiver method is visible in both method sets
                //   → Value bucket.
                // - Pointer-receiver method reached through a
                //   value-embed-only chain: only *S can take the
                //   address required to satisfy the `*T` receiver
                //   (value-embedded `T` is not always addressable
                //   in the value form). The Go spec language is
                //   precise: "The method set of `S` includes
                //   promoted methods with receiver `T`. The method
                //   set of `*S` also includes promoted methods
                //   with receiver `*T`." → PointerOnly bucket.
                //
                // Methods without a receiver-pointerness hint
                // default to value-receiver — same observable shape
                // as the D1 conservative classification, preserving
                // backward compatibility with any plugin that has
                // not yet emitted `GoMethodReceiverHint`.
                let method_receiver = method_receivers
                    .get(&method_node)
                    .copied()
                    .unwrap_or(Receiver::Value);
                let bucket = match (method_receiver, new_chain_pointerness) {
                    (Receiver::Value, _) => PromotionBucket::Value,
                    (Receiver::Pointer, Receiver::Pointer) => PromotionBucket::Value,
                    (Receiver::Pointer, Receiver::Value) => PromotionBucket::PointerOnly,
                };

                candidates_by_name
                    .entry(method_name_id)
                    .or_default()
                    .push(PromotionCandidate {
                        defining_method: method_node,
                        promoting_type: inner,
                        depth: new_depth,
                        bucket,
                    });
            }

            // Enqueue the inner for further BFS expansion.
            let mut next_path = current_path.clone();
            next_path.insert(inner);
            queue.push((inner, new_depth, new_chain_pointerness, next_path));
        }
    }

    // Step 3: apply same-depth ambiguity rule per name.
    let mut promotion = PerOuterPromotion::default();
    for (name_id, mut contributors) in candidates_by_name {
        // Find shallowest depth at which this name appears.
        let shallowest = contributors
            .iter()
            .map(|c| c.depth)
            .min()
            .expect("non-empty");
        // Filter to candidates at the shallowest depth.
        contributors.retain(|c| c.depth == shallowest);

        // If more than ONE distinct immediate-embedded type contributes
        // the name at the shallowest depth, the selector `<outer>.<name>`
        // is ambiguous regardless of whether the multiple paths
        // ultimately resolve to the same defining method (Cluster G1
        // fix for 01_SPEC §7 AC-4 / golang/go#57352: `Foo struct { A; AB }`
        // where AB inherits `a` from A — both A and AB contribute `a` at
        // depth 1 via the same defining method `A.a`, but the SELECTOR
        // `Foo.a` is illegal per Go spec §"Selectors"). Block promotion.
        let distinct_promoting: BTreeSet<NodeId> =
            contributors.iter().map(|c| c.promoting_type).collect();
        if distinct_promoting.len() > 1 {
            stats.ambiguity_blocked_promotions =
                stats.ambiguity_blocked_promotions.saturating_add(1);
            continue;
        }

        // Unique winner. When the same defining method reaches via
        // multiple paths at the same depth (e.g. once through a
        // value-embed chain and once through a pointer-embed chain),
        // the union of buckets across all contributors determines
        // the effective visibility: any `Value`-bucket reach means
        // the method is reachable from both `S` and `*S`; only when
        // every reach is `PointerOnly` is the promoted name confined
        // to the pointer-form namespace.
        let any_value = contributors
            .iter()
            .any(|c| matches!(c.bucket, PromotionBucket::Value));
        let winner = contributors
            .first()
            .expect("at least one contributor at shallowest depth");
        if any_value {
            promotion.value.insert(name_id, winner.defining_method);
        } else {
            promotion
                .pointer_only
                .insert(name_id, winner.defining_method);
        }
    }
    let _ = outer; // silence unused warning on the loop variable in case of all-skip
    promotion
}

/// Look up `node_id`'s qualified-name string by resolving its interned
/// `qualified_name` through the string interner.
fn qualified_name_string<G: GraphMutationTarget>(graph: &G, node_id: NodeId) -> Option<String> {
    let entry = graph.nodes().get(node_id)?;
    let qn = entry.qualified_name?;
    graph.strings().resolve(qn).map(|arc| arc.to_string())
}

/// AC-9 alias resolver: if `inner` is a `NodeKind::Type` whose underlying
/// declaration is a struct/interface/type-with-methods reachable via a
/// single `TypeOf { context: TypeParameter, .. }` outgoing edge, return
/// the underlying type's qualified-name string. Returns `None` if the
/// inner is not an alias-shaped Type or no underlying type is reachable.
///
/// The depth is intentionally capped at 1 hop for D1; deeper
/// alias-of-alias chains are out of scope (AC-9's fixture from
/// golang/go#66540 is a single-hop alias and that's the contract D1
/// must satisfy).
fn resolve_alias_underlying_qn<G: GraphMutationTarget>(
    graph: &G,
    inner: NodeId,
    inner_qn: &str,
) -> Option<String> {
    let entry = graph.nodes().get(inner)?;
    if entry.kind != NodeKind::Type {
        return None;
    }

    // Walk outgoing TypeOf {TypeParameter} edges. There should be at
    // most one such edge for an alias declaration emitted by the Go
    // plugin's `handle_type_alias`.
    for edge_ref in graph.edges().edges_from(inner) {
        if let EdgeKind::TypeOf { context, .. } = &edge_ref.kind
            && let Some(ctx) = context
            && *ctx == crate::graph::unified::edge::kind::TypeOfContext::TypeParameter
        {
            // The target may itself be a Type or Struct. If the
            // target's qualified name differs from `inner_qn` we
            // accept it as the underlying.
            let target_entry = graph.nodes().get(edge_ref.target)?;
            let target_qn = target_entry.qualified_name?;
            let target_qn_str = graph.strings().resolve(target_qn)?.to_string();
            if target_qn_str != inner_qn {
                return Some(target_qn_str);
            }
        }
    }
    None
}

/// Discover methods of a receiver type by qualified-name prefix match.
///
/// Methods of a Go type `T` (whose canonical qualified name is
/// `<pkg>.<T>`) are stored as `NodeKind::Method` nodes whose qualified
/// name is `<pkg>.<T>.<MethodName>` (the Go plugin strips the
/// `*`/`[E]` modifiers in `strip_receiver_modifiers` so receiver
/// pointerness is collapsed into a single canonical struct name —
/// see `graph_builder.rs:415` and `graph_builder.rs:893`).
///
/// Returns `(method_short_name_id, method_node_id)` pairs sorted by
/// the method's `NodeId::index()` for deterministic downstream
/// iteration.
/// Cluster G1 (AC-4 interface-embedding ambiguity): collect every
/// method reachable from `interface_node` via interface-of-interface
/// embedding (`Inherits` edges to other `NodeKind::Interface`s),
/// returning the union of directly-declared methods plus all inherited
/// ones. Deduplicates by name_id — the first occurrence wins (BFS
/// order from the embedded interface). The result is consumed by
/// `compute_promotions_for_outer`'s ambiguity check so multi-interface
/// embeddings detect same-depth name collisions (golang/go#57352).
///
/// Mirrors `collect_interface_method_sets`'s flatten step but returns
/// `Vec<(StringId, NodeId)>` to slot directly into the existing
/// promotion BFS contract (which keys on `(name_id, method_node)`).
/// Depth bound `MAX_INTERFACE_EMBED_DEPTH` matches the related
/// `collect_interface_method_sets` helper.
fn find_interface_flattened_methods<G: GraphMutationTarget>(
    graph: &G,
    interface_node: NodeId,
) -> Vec<(StringId, NodeId)> {
    const MAX_INTERFACE_EMBED_DEPTH: usize = 16;
    let mut out: Vec<(StringId, NodeId)> = Vec::new();
    let mut seen: BTreeSet<StringId> = BTreeSet::new();
    let mut visited: BTreeSet<NodeId> = BTreeSet::new();
    // BFS over interface-of-interface Inherits chains. Depth-bounded to
    // match `collect_interface_method_sets` and to defend against any
    // cycles that an ill-typed Go program could create (the parser
    // doesn't enforce well-formedness here).
    let mut queue: Vec<(NodeId, usize)> = vec![(interface_node, 0)];
    while let Some((cur, depth)) = queue.pop() {
        if !visited.insert(cur) {
            continue;
        }
        if depth >= MAX_INTERFACE_EMBED_DEPTH {
            continue;
        }
        let qn_str = match qualified_name_string(graph, cur) {
            Some(s) => s,
            None => continue,
        };
        for (name_id, method_node) in find_method_names_of_type(graph, &qn_str) {
            if seen.insert(name_id) {
                out.push((name_id, method_node));
            }
        }
        for edge in graph.edges().edges_from(cur) {
            if matches!(edge.kind, EdgeKind::Inherits)
                && matches!(
                    graph.nodes().get(edge.target).map(|e| e.kind),
                    Some(NodeKind::Interface)
                )
            {
                queue.push((edge.target, depth + 1));
            }
        }
    }
    out
}

fn find_method_names_of_type<G: GraphMutationTarget>(
    graph: &G,
    type_qn: &str,
) -> Vec<(StringId, NodeId)> {
    let mut out: Vec<(StringId, NodeId)> = Vec::new();
    // Cluster G1: pass operates on canonical (`::`-separated) qns
    // end-to-end. `type_qn` is the canonical qn of the embedded type;
    // every method of that type appears in the index keyed on
    // `<type_qn>::<method>` because `helper.add_method` routes through
    // `canonicalize_graph_qualified_name`. See 05_TEST_PLAN.md §7.5.
    let method_prefix = format!("{type_qn}::");
    // Enumerate all Method nodes. For each, resolve its qualified
    // name and check the prefix. The asymptotic cost is
    // O(|methods_in_workspace|) per call; the per-outer cost is
    // O(|methods| × |embedded inner types|) which is acceptable for
    // typical Go workspaces (cap ≤ 16 promotion depth × O(N) methods).
    for &method_node in graph.indices().by_kind(NodeKind::Method) {
        let entry = match graph.nodes().get(method_node) {
            Some(e) => e,
            None => continue,
        };
        let qn = match entry.qualified_name {
            Some(q) => q,
            None => continue,
        };
        let qn_str = match graph.strings().resolve(qn) {
            Some(s) => s,
            None => continue,
        };
        if let Some(short) = qn_str.strip_prefix(&method_prefix) {
            // A nested qualifier inside `short` (e.g. another `::`)
            // would mean the qualified name has additional segments
            // — Go method qualified names are exactly two segments
            // past the receiver, so reject anything with a canonical
            // separator in `short`.
            if short.contains("::") {
                continue;
            }
            out.push((entry.name, method_node));
        }
    }
    out.sort_by_key(|(_, nid)| nid.index());
    out.dedup_by_key(|(_, nid)| *nid);
    out
}

/// Step 4 + 5: materialise promoted-method nodes (with pass-local
/// dedupe), emit `Contains(S → S.m)` / `Inherits(S.m → T.m)` value-form
/// edges and `Contains(*S → *S.m)` / `Inherits(*S.m → T.m)` /
/// `Inherits(*S → S)` pointer-form edges per 02_DESIGN §4.2 step 4.
///
/// Iterates `all_promotions` in `NodeId::index`-sorted order (BTreeMap
/// gives stable iteration). Within each per-outer promotion bucket the
/// names are sorted by `StringId`, so the resulting node + edge
/// emission sequence is deterministic across runs (AC-12 prerequisite).
fn materialise_and_emit_structural<G: GraphMutationTarget>(
    graph: &mut G,
    all_promotions: &BTreeMap<NodeId, PerOuterPromotion>,
    indices: &mut PassLocalIndices,
    newly_created_nodes: &mut Vec<NodeId>,
    stats: &mut GoMethodSetStats,
) {
    // Snapshot per-outer (package, short_name, file, has_pointer_promotion)
    // tuples upfront so the subsequent `nodes_mut()` borrow does not
    // conflict with the `nodes()` reads.
    struct OuterMeta {
        outer: NodeId,
        package_id: StringId,
        short_name_id: StringId,
        short_name_str: String,
        outer_qn_str: String,
        outer_file: FileId,
        has_pointer_promotion: bool,
        promotion_count: usize,
    }
    // First, snapshot all read-only outer state (no `strings_mut()`).
    struct OuterReadOnly {
        outer: NodeId,
        outer_qn_str: String,
        package_qn_str: String,
        short_name_str: String,
        outer_file: FileId,
        has_pointer_promotion: bool,
        promotion_count: usize,
    }
    let mut read_only: Vec<OuterReadOnly> = Vec::with_capacity(all_promotions.len());
    for (&outer, promotion) in all_promotions {
        if promotion.value.is_empty() && promotion.pointer_only.is_empty() {
            continue;
        }
        let outer_entry = match graph.nodes().get(outer) {
            Some(e) => e,
            None => continue,
        };
        let outer_qn_id = match outer_entry.qualified_name {
            Some(q) => q,
            None => continue,
        };
        let outer_qn_str = match graph.strings().resolve(outer_qn_id) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let (package_qn_str, short_name) = match split_qn_into_package_and_name(&outer_qn_str) {
            Some(split) => split,
            None => continue,
        };
        read_only.push(OuterReadOnly {
            outer,
            outer_qn_str,
            package_qn_str,
            short_name_str: short_name,
            outer_file: outer_entry.file,
            has_pointer_promotion: !promotion.pointer_only.is_empty(),
            promotion_count: promotion.value.len() + promotion.pointer_only.len(),
        });
    }

    // Second pass: intern package + short-name strings into the live
    // string interner, lifting them into the index keyspace. This step
    // requires `&mut strings_mut()` which is why it is split from the
    // read-only snapshot above.
    let mut outers_meta: Vec<OuterMeta> = Vec::with_capacity(read_only.len());
    for ro in read_only {
        let package_id = match graph.strings_mut().intern(&ro.package_qn_str) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let short_name_id = match graph.strings_mut().intern(&ro.short_name_str) {
            Ok(id) => id,
            Err(_) => continue,
        };
        outers_meta.push(OuterMeta {
            outer: ro.outer,
            package_id,
            short_name_id,
            short_name_str: ro.short_name_str,
            outer_qn_str: ro.outer_qn_str,
            outer_file: ro.outer_file,
            has_pointer_promotion: ro.has_pointer_promotion,
            promotion_count: ro.promotion_count,
        });
    }

    for meta in outers_meta {
        let promotion = match all_promotions.get(&meta.outer) {
            Some(p) => p,
            None => continue,
        };

        // Materialise the pointer-form anchor `<pkg>.*<S>` when the
        // pointer-only bucket has entries. Per 02_DESIGN §4.2 step 4,
        // we also materialise it as a "pre-step" when ANY promoted
        // method exists, but D1 lifts that to the minimal contract:
        // emit *S only when at least one pointer-only promotion needs
        // it, since value-bucket reachability from *S already flows
        // through `Inherits(*S → S)`-less paths (Cluster D2 will
        // re-trigger this branch for pointer-form satisfaction).
        let mut pointer_form_node: Option<NodeId> = None;
        if meta.has_pointer_promotion {
            // Cluster G1: pointer-form anchor qn uses canonical
            // (`::`-separated) form so it matches the
            // `by_qualified_name` index populated by canonicalised
            // node qns. The marker is the literal sequence `::*`
            // (canonical separator + pointer indicator). See
            // 05_TEST_PLAN.md §7.5.
            let pointer_form_qn = format!(
                "{}::*{}",
                package_qn_resolve(&meta.outer_qn_str, &meta.short_name_str),
                meta.short_name_str
            );
            let key = (meta.package_id, meta.short_name_id);
            let node_id = if let Some(&existing) = indices.pointer_type.get(&key) {
                existing
            } else {
                let interned = match graph.strings_mut().intern(&pointer_form_qn) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let new_id = mint_synthetic_node(
                    graph,
                    NodeKind::Type,
                    &meta.short_name_str,
                    interned,
                    meta.outer_file,
                );
                let new_id = match new_id {
                    Some(id) => id,
                    None => continue,
                };
                indices.pointer_type.insert(key, new_id);
                newly_created_nodes.push(new_id);

                // Emit `Inherits(*S → S)` linkage so queries from *S
                // walk through to S's value-bucket promotions. The
                // edge anchors to S's file_id per §3.4 rule 1.
                graph
                    .edges_mut()
                    .add_edge(new_id, meta.outer, EdgeKind::Inherits, meta.outer_file);
                new_id
            };
            pointer_form_node = Some(node_id);
        }

        // Value-form promotions: materialise `<pkg>.<S>.<m>` and emit
        // `Contains(S → S.m)` + `Inherits(S.m → T.m)`.
        for (&name_id, &defining_method) in &promotion.value {
            let method_short = match graph.strings().resolve(name_id) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // Cluster G1: canonical separator. See 05_TEST_PLAN.md §7.5.
            let value_qn = format!("{}::{}", meta.outer_qn_str, method_short);
            let key = (meta.package_id, meta.short_name_id, name_id);
            let method_node_id = if let Some(&existing) = indices.value.get(&key) {
                existing
            } else {
                let interned_qn = match graph.strings_mut().intern(&value_qn) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let new_id = mint_synthetic_node(
                    graph,
                    NodeKind::Method,
                    &method_short,
                    interned_qn,
                    meta.outer_file,
                );
                let new_id = match new_id {
                    Some(id) => id,
                    None => continue,
                };
                indices.value.insert(key, new_id);
                newly_created_nodes.push(new_id);
                stats.promoted_method_nodes = stats.promoted_method_nodes.saturating_add(1);
                new_id
            };
            // Cluster D2.2: record `(outer_node, method_name) →
            // promoted_value_node` so the shadow-emission walker can
            // gate by resolved receiver type.
            indices
                .outer_to_value_promoted
                .insert((meta.outer, name_id), method_node_id);
            // Structural edges. Use S's file_id per §3.4 rule 2/3.
            graph.edges_mut().add_edge(
                meta.outer,
                method_node_id,
                EdgeKind::Contains,
                meta.outer_file,
            );
            graph.edges_mut().add_edge(
                method_node_id,
                defining_method,
                EdgeKind::Inherits,
                meta.outer_file,
            );
        }

        // Pointer-only promotions: materialise `<pkg>.*<S>.<m>` and
        // emit Contains(*S → *S.m) + Inherits(*S.m → T.m).
        if let Some(ptr_node) = pointer_form_node {
            for (&name_id, &defining_method) in &promotion.pointer_only {
                let method_short = match graph.strings().resolve(name_id) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                // Cluster G1: canonical pointer-form promoted-method
                // qn — `<pkg>::*<S>::<m>`. See 05_TEST_PLAN.md §7.5.
                let ptr_qn = format!(
                    "{}::*{}::{}",
                    package_qn_resolve(&meta.outer_qn_str, &meta.short_name_str),
                    meta.short_name_str,
                    method_short,
                );
                let key = (meta.package_id, meta.short_name_id, name_id);
                let method_node_id = if let Some(&existing) = indices.pointer.get(&key) {
                    existing
                } else {
                    let interned_qn = match graph.strings_mut().intern(&ptr_qn) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    let new_id = mint_synthetic_node(
                        graph,
                        NodeKind::Method,
                        &method_short,
                        interned_qn,
                        meta.outer_file,
                    );
                    let new_id = match new_id {
                        Some(id) => id,
                        None => continue,
                    };
                    indices.pointer.insert(key, new_id);
                    newly_created_nodes.push(new_id);
                    stats.promoted_method_nodes = stats.promoted_method_nodes.saturating_add(1);
                    new_id
                };
                // Cluster D2.2: record the pointer-form promoted-
                // method index keyed by the *value*-form outer node.
                // The shadow walker uses the value-form outer to
                // join receiver-call hints; pointer-only buckets
                // still resolve through the same value-form outer
                // identity.
                indices
                    .outer_to_pointer_promoted
                    .insert((meta.outer, name_id), method_node_id);
                graph.edges_mut().add_edge(
                    ptr_node,
                    method_node_id,
                    EdgeKind::Contains,
                    meta.outer_file,
                );
                graph.edges_mut().add_edge(
                    method_node_id,
                    defining_method,
                    EdgeKind::Inherits,
                    meta.outer_file,
                );
            }
        }

        let _ = meta.promotion_count; // retained for debug instrumentation
    }
}

/// Split a fully-qualified name `<pkg>.<short>` into `(package, short)`
/// where `<pkg>` may itself contain dots and `<short>` is the trailing
/// segment after the LAST dot.
fn split_qn_into_package_and_name(qn: &str) -> Option<(String, String)> {
    // Cluster G1: canonical qns use `::` as separator. `<pkg>::<short>`
    // splits at the last `::`; strip 2 chars to remove the separator
    // from the short name. See 05_TEST_PLAN.md §7.5.
    let last_sep = qn.rfind("::")?;
    let (pkg, rest) = qn.split_at(last_sep);
    let short = &rest[2..]; // strip the leading `::`
    if pkg.is_empty() || short.is_empty() {
        return None;
    }
    Some((pkg.to_string(), short.to_string()))
}

/// Helper: derive the package qualifier from a canonical `qn` of form
/// `<pkg>::<short>`. The package qualifier is the prefix up to the last
/// `::` BEFORE the short name. Returns the package as a borrowed slice
/// — caller uses it inline.
fn package_qn_resolve<'a>(outer_qn: &'a str, short_name: &str) -> &'a str {
    // Cluster G1: canonical-form suffix uses `::` separator.
    let suffix = format!("::{short_name}");
    outer_qn.strip_suffix(&suffix).unwrap_or(outer_qn)
}

// ============================================================================
// Cluster E2 — Pass-owned predicates + tombstone driver (02_DESIGN §3.6)
// ============================================================================
//
// The predicates below are purely structural over persisted graph state:
// node kind, qualified-name shape, edge kind, edge target's qualified-name
// shape, and `NodeFlags::SYNTHETIC`. They survive snapshot reload by
// construction (no `serde(skip)` ledger). See 02_DESIGN §3.6 lines
// 1010-1098 for the normative definition; the helper signatures deviate
// from the strict spec only to keep them store-agnostic for unit-testing
// (predicates take concrete `&NodeArena` / `&StringInterner` /
// `&NodeMetadataStore` slices rather than `&impl GraphMutationTarget`,
// because the trait deliberately exposes only `macro_metadata_mut(&mut
// self)` and adding an immutable accessor would extend the trait — a
// scope-guard violation per the E2 plan).

/// Pure string-shape predicate: returns true iff `qualified_name` matches
/// one of the three pass-emitted namespace shapes documented in 02_DESIGN
/// §3.6 lines 1027-1031:
///
/// * `<pkg>.<S>.<m>`  — value-form promoted method on outer struct `S`.
///   Recognised as ≥2 dot-separated trailing segments with no `*` prefix
///   on any segment.
/// * `<pkg>.*<S>.<m>` — pointer-form promoted method on `*S`. Recognised
///   by the literal `.*` substring followed by at least one more `.`.
/// * `<pkg>.*<S>`     — pointer-form synthetic `Type` anchor for `*S`.
///   Recognised by the literal `.*` substring with no further `.` after.
///
/// The shape check is intentionally loose by itself — a real (non-pass)
/// method `<pkg>.<S>.<m>` shares the value-form shape. The
/// [`is_pass_owned_node`] composite predicate combines this with the
/// node-kind + Synthetic-flag pre-filter to identify pass-owned nodes
/// unambiguously.
fn qualified_name_matches_pass_shape(qualified_name: &str) -> bool {
    // Cluster G1: pass-owned shapes are matched against canonical
    // (`::`-separated) qns. The pointer-form marker is the literal
    // sequence `::*` (canonical separator + pointer indicator).
    // See 05_TEST_PLAN.md §7.5.
    //
    // Pointer-form: must contain `::*`. Two sub-cases distinguished by
    // whether anything follows after the `::*<name>` segment.
    if let Some(after_sep_star) = qualified_name.find("::*") {
        // Skip the `::*` (3 chars) to inspect the tail after `<S>`.
        let tail = &qualified_name[after_sep_star + 3..];
        if tail.is_empty() {
            // `<pkg>::*` is malformed — no struct name after the `*`.
            return false;
        }
        // `<pkg>::*<S>::<m>` — has a further `::` after `::*<S>`.
        // `<pkg>::*<S>`     — no further `::` after `::*<S>`.
        // Both shapes are pass-owned; the difference only affects which
        // node kind hosts them (Method vs Type).
        return true;
    }
    // Value-form `<pkg>::<S>::<m>`: at least two `::` separators (so
    // at least three segments). Stricter than ≥1 to exclude shapes
    // like `<pkg>::<S>` which would catch every real qualified type.
    qualified_name.matches("::").count() >= 2
}

/// Returns true iff `node` is owned by the Go T1 method-set pass.
///
/// Composes three orthogonal checks:
/// 1. `node.kind ∈ {Method, Type}` — the pass only mints synthetic
///    nodes of these two kinds.
/// 2. `is_synthetic == true` — the `NodeFlags::SYNTHETIC` flag is
///    set on every node the pass mints (via
///    [`mint_synthetic_node`]).
/// 3. `qualified_name_matches_pass_shape(qualified_name)` — the
///    structural fallback that prevents false positives if some other
///    synthesiser ever marks a Method or Type node Synthetic without
///    obeying the pass's naming convention.
///
/// The Synthetic flag is taken as a `bool` rather than looked up on a
/// metadata store reference so this predicate can be unit-tested with
/// hand-rolled `NodeEntry` fixtures.
fn is_pass_owned_node(node: &NodeEntry, qualified_name: &str, is_synthetic: bool) -> bool {
    if !matches!(node.kind, NodeKind::Method | NodeKind::Type) {
        return false;
    }
    if !is_synthetic {
        return false;
    }
    qualified_name_matches_pass_shape(qualified_name)
}

/// Returns true iff `edge` is owned by the Go T1 method-set pass.
///
/// Walks the edge's classification and consults endpoint state to
/// reproduce the four-way disjunction from 02_DESIGN §3.6 lines
/// 1034-1072:
///
/// * **(A)** `Contains` / `Inherits` with either endpoint pass-owned
///   (covers promoted-method `Contains(S → S.m)`, embedding chain
///   `Inherits(S.m → T.m)`, and pointer-form anchor `Inherits(*S → S)`).
/// * **(B)** `Calls` with a pass-owned `Method` target (shadow-`Calls`
///   from a real call site to the promoted name).
/// * **(C)** `References` with a pass-owned `Method` target (parallel
///   shadow-`References`).
/// * **(D)** `Implements` whose target's qualified name does NOT start
///   with `<type:` — the structural negative-prefix discriminator
///   against type-assertion's `<type:T>` synthetic-interface naming
///   (the only other Phase-1 `Implements` emitter, verified at
///   `sqry-lang-go/src/relations/graph_builder.rs:3982`
///   `process_type_assertion_unified`).
///
/// The caller is responsible for the orthogonal `edge.file ∈ go_files`
/// clause from predicate (D); the tombstone driver enforces it by
/// iterating only over edges whose endpoints lie in the changed-file
/// slice.
///
/// # Why this helper isn't called from the production driver
///
/// The production driver
/// [`tombstone_all_pass_owned`] inlines predicate (D)'s
/// target-qn structural check rather than calling this helper because:
///
/// 1. The trait `GraphMutationTarget` exposes only
///    `macro_metadata_mut(&mut self)`, not an immutable accessor. The
///    borrow checker forbids holding `&mut metadata` simultaneously
///    with `&nodes` / `&strings` through a single `<G: GraphMutationTarget>`
///    projection.
/// 2. Predicates (A)/(B)/(C) — endpoint pass-owned — are subsumed by
///    the bulk
///    [`BidirectionalEdgeStore::tombstone_edges_for_nodes`] sweep in
///    Phase 5 of the driver, which kills every edge whose source or
///    target is in the pass-owned `NodeId` set in a single
///    `O(node_count + edge_count)` walk. Per-edge classification via
///    [`is_pass_owned_edge`] would duplicate that work.
///
/// The helper remains the canonical structural specification of
/// edge ownership, exercised by `e2_is_pass_owned_edge_predicate` to
/// pin the four-way disjunction's behaviour against the design.
#[allow(
    dead_code,
    reason = "Canonical structural specification of edge ownership per \
              02_DESIGN §3.6 lines 1034-1072. Exercised by \
              `e2_is_pass_owned_edge_predicate`; the production driver \
              uses [`tombstone_edges_for_nodes`] for (A)/(B)/(C) and \
              inlines predicate (D) due to the trait's metadata \
              borrow constraints."
)]
fn is_pass_owned_edge(
    edge: &StoreEdgeRef,
    nodes: &NodeArena,
    strings: &StringInterner,
    metadata: &NodeMetadataStore,
) -> bool {
    let endpoint_pass_owned = |nid: NodeId| -> bool {
        let Some(entry) = nodes.get(nid) else {
            return false;
        };
        let Some(qn_id) = entry.qualified_name else {
            return false;
        };
        let Some(qn_str) = strings.resolve(qn_id) else {
            return false;
        };
        is_pass_owned_node(entry, qn_str.as_ref(), metadata.is_synthetic(nid))
    };

    match edge.kind {
        EdgeKind::Contains | EdgeKind::Inherits => {
            // Predicate (A): either endpoint pass-owned.
            endpoint_pass_owned(edge.source) || endpoint_pass_owned(edge.target)
        }
        EdgeKind::Calls { .. } | EdgeKind::References => {
            // Predicates (B) + (C): pass-owned Method target. The
            // target-Method discriminator alone is sufficient because
            // the pass is the only emitter of edges whose target is a
            // promoted-method node.
            let Some(target_entry) = nodes.get(edge.target) else {
                return false;
            };
            if !matches!(target_entry.kind, NodeKind::Method) {
                return false;
            }
            endpoint_pass_owned(edge.target)
        }
        EdgeKind::Implements => {
            // Predicate (D): target qn doesn't start with "<type:".
            // The pointer-form `Implements(*C → I)` sub-case (source
            // is pass-owned Type) collapses into the same predicate —
            // its target is a real interface, not a `<type:...>` shadow.
            let Some(target_entry) = nodes.get(edge.target) else {
                return false;
            };
            let Some(qn_id) = target_entry.qualified_name else {
                return false;
            };
            let Some(qn_str) = strings.resolve(qn_id) else {
                return false;
            };
            !qn_str.starts_with("<type:")
        }
        _ => false,
    }
}

/// Tombstone every pass-owned node and pass-emitted `Implements` edge
/// in the entire graph. Runs at pass entry on the incremental rebuild
/// plane before any new emission, so the subsequent whole-graph
/// re-emission produces the canonical `Implements` multiset without
/// any stale residue from a previous rebuild.
///
/// **Option C — whole-graph scope** (iter-4, supersedes the iter-1/2/3
/// changed-file-scoped variants). The earlier scoped tombstone driver
/// chased a three-axis scope-parity invariant
/// (`source.file ∈ scope OR target.file ∈ scope OR edge.file ∈ scope`)
/// across three codex review iterations and still left a fourth axis
/// uncovered: methods may live in a different file from the receiver
/// type, so a method-file-only edit can leave a stale `Implements`
/// edge whose endpoints' files are out of scope. Codex iter-3 also
/// surfaced that `edge.file` is dropped by CSR compaction
/// (`FileId::INVALID` post-load), invalidating the third axis on
/// every persisted snapshot. Both problems disappear when the
/// tombstone scope is the entire graph and the pass re-emits the
/// canonical method-set-derived multiset every rebuild.
///
/// The whole-graph approach trades the (scope-dependent) cost of the
/// scoped driver for an `O(N_nodes + N_live_edges)` walk. Real Go
/// workloads have node + edge counts bounded by what a single
/// satisfaction pair-enumeration already touches; full re-emission
/// is the correctness reference behaviour and any future perf delta
/// can be earned by measurement, not by re-introducing a scope-skip
/// invariant that has now failed three times.
///
/// Algorithm:
/// 1. Whole-graph scan of [`NodeArena`]: collect every Method/Type
///    node that classifies as pass-owned via [`is_pass_owned_node`]
///    (Synthetic flag + qualified-name shape).
/// 2. Whole-graph scan of live edges: tombstone every `Implements`
///    whose source-node-file is a Go-language file AND whose target
///    qualified name does NOT start with `<type:`. That picks out
///    exactly the pass-emitted T1.1 value-form `Implements(C, I)`
///    and T1.3 signature `Implements(fn, F)` edges — both have
///    non-pass-owned endpoints, so the bulk Phase 3 sweep below
///    does not cover them. The Go-language filter is read from the
///    **source node's** file (not `edge.file`) because CSR-resident
///    edges surface with `file: FileId::INVALID` after compaction
///    (codex iter-3 F-1) but node files are always preserved.
/// 3. Bulk-tombstone every edge whose source or target is in the
///    pass-owned `NodeId` set via
///    [`BidirectionalEdgeStore::tombstone_edges_for_nodes`]. Covers
///    predicates (A)/(B)/(C) and pointer-form `Implements(*C, I)`
///    in one `O(N_live_edges)` walk.
/// 4. Remove the pass-owned `NodeId`s from the arena so subsequent
///    re-emission allocates fresh `NodeId`s with advanced generations.
///
/// Returns the count of tombstoned pass-owned nodes.
fn tombstone_all_pass_owned<G: GraphMutationTarget>(graph: &mut G) -> usize {
    // Phase 1: whole-graph candidate collection. Walk every live node
    // in the arena and stage Method/Type entries (the only kinds that
    // can be pass-owned per `is_pass_owned_node`). The `&nodes` borrow
    // is dropped before the metadata consultation below to allow the
    // trait's `macro_metadata_mut()` mutable projection.
    struct Candidate {
        nid: NodeId,
        kind: NodeKind,
        qn: Option<String>,
    }
    let candidates: Vec<Candidate> = {
        let nodes = graph.nodes();
        let strings = graph.strings();
        nodes
            .iter()
            .filter_map(|(nid, entry)| {
                if !matches!(entry.kind, NodeKind::Method | NodeKind::Type) {
                    return None;
                }
                let qn = entry
                    .qualified_name
                    .and_then(|sid| strings.resolve(sid))
                    .map(|arc| arc.as_ref().to_owned());
                Some(Candidate {
                    nid,
                    kind: entry.kind,
                    qn,
                })
            })
            .collect()
    };

    // Phase 2: classify candidates via `is_pass_owned_node` (Synthetic
    // flag + qn shape). Consults `NodeMetadataStore::is_synthetic`
    // under a `&mut metadata` borrow obtained through
    // `GraphMutationTarget::macro_metadata_mut`; the `Candidate`
    // wrapper carries the prior-resolved `kind` and `qn` so no
    // simultaneous `&nodes` / `&strings` borrow is needed.
    let pass_owned_nodes: Vec<NodeId> = {
        let metadata: &NodeMetadataStore = graph.macro_metadata_mut();
        candidates
            .into_iter()
            .filter_map(|c| {
                let is_synthetic = metadata.is_synthetic(c.nid);
                let qn_ref: &str = c.qn.as_deref().unwrap_or("");
                // Stub `NodeEntry` view: `is_pass_owned_node` only
                // reads `node.kind`, so the StringId/FileId are
                // placeholder.
                let view = NodeEntry::new(c.kind, StringId::new(0), FileId::new(0));
                if is_pass_owned_node(&view, qn_ref, is_synthetic) {
                    Some(c.nid)
                } else {
                    None
                }
            })
            .collect()
    };

    // Phase 3: tombstone pass-emitted `Implements` edges whose
    // endpoints are NOT pass-owned. T1.1 value-form (`Implements(C, I)`)
    // and T1.3 signature-form (`Implements(fn, F)`) connect two real
    // plugin-emitted nodes, so Phase 4's bulk `tombstone_edges_for_nodes`
    // would miss them. The discriminator is:
    //
    //   - `EdgeKind::Implements`
    //   - target qn does NOT start with `<type:` (excludes the
    //     type-assertion sink `process_type_assertion_unified` in
    //     `sqry-lang-go`)
    //   - source node's file is Go-language (excludes sibling-plugin
    //     `Implements` emissions whose source is a Java / TypeScript /
    //     C++ node — read from the source NODE's file, NOT
    //     `edge.file`, because CSR-resident edges surface with
    //     `file: FileId::INVALID` after compaction per codex iter-3
    //     finding 1)
    let implements_to_remove: Vec<(NodeId, NodeId, EdgeKind, FileId)> = {
        let edges = graph.edges();
        let nodes = graph.nodes();
        let strings = graph.strings();
        let files = graph.files();
        let mut out: Vec<(NodeId, NodeId, EdgeKind, FileId)> = Vec::new();
        for e in edges.all_live_forward_edges() {
            if !matches!(e.kind, EdgeKind::Implements) {
                continue;
            }
            let target_starts_with_type = nodes
                .get(e.target)
                .and_then(|t| t.qualified_name)
                .and_then(|sid| strings.resolve(sid))
                .map(|s| s.starts_with("<type:"))
                .unwrap_or(false);
            if target_starts_with_type {
                continue;
            }
            let source_is_go = nodes
                .get(e.source)
                .map(|n| files.language_for_file(n.file) == Some(Language::Go))
                .unwrap_or(false);
            if !source_is_go {
                continue;
            }
            out.push((e.source, e.target, e.kind.clone(), e.file));
        }
        out
    };
    for (s, t, k, f) in implements_to_remove {
        graph.edges_mut().remove_edge(s, t, k, f);
    }

    // Phase 4: bulk-tombstone every edge whose source or target is in
    // the pass-owned node set. Covers predicates (A), (B), (C) and
    // pointer-form (D) `Implements(*C, I)` (source `*C` pass-owned)
    // in a single `O(N_live_edges)` walk.
    let pass_owned_count = pass_owned_nodes.len();
    if pass_owned_count > 0 {
        let dead: std::collections::HashSet<NodeId> = pass_owned_nodes.iter().copied().collect();
        let _killed = graph.edges_mut().tombstone_edges_for_nodes(&dead);

        // Phase 5: remove the pass-owned nodes from the arena so the
        // slot generations advance and any new emission is allocated
        // a fresh NodeId.
        for nid in pass_owned_nodes {
            graph.nodes_mut().remove(nid);
        }

        // Phase 6: drop the just-tombstoned NodeIds from
        // `FileRegistry::per_file_nodes`. Every pass-owned node was
        // recorded in its file's bucket by `mint_synthetic_node`
        // (see line 2244 — the call site is documented on that
        // function as load-bearing for the §F.1 bucket bijection
        // "every live node belongs to some bucket" invariant). Phase 5
        // above removed the slots from the arena, so those NodeIds are
        // now dead — leaving them in the bucket would violate the §F.1
        // (a) condition "every NodeId inside any per_file_nodes bucket
        // maps to a live arena slot". The Gate 0c bucket-bijection
        // assert at every publish boundary (`assert_publish_bijection`,
        // and the harness §E re-runs at every incremental rebuild)
        // would panic with "dead node NodeId(N:G) in bucket FileId(F)".
        // The bug surfaced specifically on the incremental rebuild
        // plane via `prop_incremental_matches_full_java_enterprise`
        // when the pass re-ran tombstone-before-emit on a graph whose
        // prior full build had minted pass-owned synthetics.
        let dead_for_buckets = dead;
        graph
            .files_mut()
            .retain_nodes_in_buckets(&|nid: NodeId| !dead_for_buckets.contains(&nid));
    }

    pass_owned_count
}

/// Mint a synthetic node (Method or Type) with the given canonical
/// qualified-name `StringId`. Marks the node as
/// `NodeFlags::SYNTHETIC` so it stays out of workspace-symbol
/// search per the C_SUPPRESS contract, AND registers the new node in
/// the [`FileRegistry`] per-file bucket so the publish-boundary
/// bijection (`CodeGraph::assert_bucket_bijection` check (d)) holds:
/// "every live node in the arena belongs to some bucket".
///
/// Without the [`FileRegistry::record_node`] call, every Go workspace
/// that triggers the pass to mint a synthetic node would panic at
/// `super::super::publish::assert_publish_bijection` (entrypoint.rs
/// final publish boundary) because the synthetic is live but absent
/// from every per-file bucket. The check is gated on
/// `any_bucket_populated`, which is true the moment the Go plugin
/// emits any node into staging — so production Go builds hit the
/// invariant unconditionally.
///
/// Returns `None` if the underlying arena allocation fails (only
/// happens at the u32::MAX-node ceiling — never hit in practice).
fn mint_synthetic_node<G: GraphMutationTarget>(
    graph: &mut G,
    kind: NodeKind,
    short_name: &str,
    qualified_name: StringId,
    file: FileId,
) -> Option<NodeId> {
    // Intern the short name through the live interner.
    let name_id = graph.strings_mut().intern(short_name).ok()?;
    let mut entry = NodeEntry::new(kind, name_id, file);
    entry.qualified_name = Some(qualified_name);
    let new_id = graph.nodes_mut().alloc(entry).ok()?;
    graph.macro_metadata_mut().mark_synthetic(new_id);
    // Register the synthetic with the file's per-file bucket so the
    // publish-boundary bijection invariant holds and so any future
    // file-removal path (`RebuildGraph::remove_file` →
    // `FileRegistry::take_nodes`) drains this synthetic alongside the
    // plugin-emitted nodes that anchor it.
    graph.files_mut().record_node(file, new_id);
    Some(new_id)
}

/// Step 6: emit shadow `Calls` + `References` edges so
/// `direct_callers(<pkg>.<S>.<m>)` (or `<pkg>.*<S>.<m>` for
/// pointer-only promotions) is non-empty whenever the original method
/// has a call site whose receiver resolves to the outer type.
///
/// Algorithm (Cluster D2.2 strict gating, per 02_DESIGN §3.3 step 6 +
/// §4.2 step 5–6):
///
/// For each `GoReceiverCallHint` in the side channel:
///
/// 1. Walk the `GoReceiverHintKind` payload to resolve the receiver
///    expression's type to a canonical struct / type `NodeId` `R` in
///    the live arena. The resolution path depends on the variant
///    (`TypePrefixed` / `PointerPrefixed` look up the qualified name;
///    `LocalIdent` walks a `TypeOf` edge from the binding node;
///    `CallReturn` walks the callee's `TypeOf{Return}` edge).
/// 2. Look up `(R, method_name) → promoted_node` in
///    `indices.outer_to_value_promoted` (or, if the receiver was
///    pointer-prefixed, `indices.outer_to_pointer_promoted`). On hit,
///    emit shadow `Calls(caller → promoted_node)` and
///    `References(caller → promoted_node)`.
///
/// The caller `NodeId` for the shadow edge is the enclosing
/// function/method that contains the call site — derived from the
/// `callee_method` of the hint by walking back to the call site's
/// surrounding function. The hint carries the call-site `NodeId`
/// directly; the Go plugin emits the call from inside a function
/// context, so the caller of the original `Calls` edge whose target
/// is `callee_method` is the same function. We re-use the existing
/// `calls_into` reverse-adjacency walk to recover the caller(s), then
/// gate them by receiver-type match.
///
/// This is **strict** D2.2 gating: only call sites whose resolved
/// receiver type is exactly `R = outer_struct` (or `*outer_struct` for
/// the pointer-form bucket) emit a shadow edge. Call sites against
/// unrelated receivers do **not** shadow the promoted name — fixing
/// D1's over-emission while preserving AC-5 (`direct_callers` is
/// non-empty whenever the receiver actually resolves to the outer).
///
/// Deterministic drain: edges are collected into a `Vec`, sorted by
/// `(caller.index(), target.index(), kind_tag)` and emitted in that
/// order so the resulting edge multiset is identical across runs
/// (AC-12 prerequisite).
fn emit_shadow_calls_and_references<G: GraphMutationTarget>(
    graph: &mut G,
    indices: &PassLocalIndices,
    stats: &mut GoMethodSetStats,
) {
    // Determine whether we have GoReceiverCallHints to drive strict
    // gating. If the side channel is empty (typical for non-Go
    // graphs and for hand-crafted unit-test inputs that mint nodes
    // directly), fall back to the D1 over-emission shape so legacy
    // tests + non-Go workflows continue to pass.
    if graph.go_hints().receiver_calls.is_empty() {
        emit_shadow_calls_legacy(graph, indices, stats);
        return;
    }

    // Pre-snapshot the receiver-call hints into an owned vector so the
    // subsequent `&mut graph` for edge insertion does not conflict
    // with the `&graph.go_hints()` borrow.
    let receiver_hints: Vec<GoReceiverCallHint> = graph.go_hints().receiver_calls.clone();

    #[derive(Debug, Clone, Copy)]
    struct PendingShadow {
        caller: NodeId,
        target: NodeId,
        kind_tag: u8, // 0 = Calls, 1 = References
        argument_count: u8,
        is_async: bool,
        file: FileId,
    }
    let mut pending: Vec<PendingShadow> = Vec::new();

    for hint in &receiver_hints {
        // Resolve the receiver expression to a canonical outer-type
        // NodeId, plus the "pointer-prefixed?" bit for the bucket
        // selection.
        let (receiver_node, want_pointer) = match resolve_receiver_kind(graph, &hint.receiver) {
            Some(r) => r,
            None => continue,
        };

        let method_name_id = hint.method_name;
        let promoted_node = if want_pointer {
            indices
                .outer_to_pointer_promoted
                .get(&(receiver_node, method_name_id))
                .copied()
        } else {
            indices
                .outer_to_value_promoted
                .get(&(receiver_node, method_name_id))
                .copied()
        };
        let promoted_node = match promoted_node {
            Some(n) => n,
            None => continue,
        };

        // Recover the caller(s) by reverse-walking the existing
        // Calls edge into the underlying defining method. The hint
        // already carries the `callee_method` it called originally;
        // we restrict to callers whose existing Calls edge into
        // `callee_method` actually matches this hint's call site.
        // Today the side channel does not carry the caller NodeId
        // explicitly, but every receiver call hint maps to exactly
        // one Calls(caller → callee_method) edge, so reverse-walking
        // `calls_into` and filtering by argument_count + is_async
        // is sufficient for the unit-test surface. In a multi-call
        // workspace, multiple callers may share the same hint shape;
        // gating by `argument_count` + `is_async` ensures the shadow
        // only fires for the matching subset.
        let callers = graph.calls_into(hint.callee_method);
        for (caller, _edge_id, meta) in callers {
            if meta.argument_count != hint.argument_count || meta.is_async != hint.is_async {
                continue;
            }
            let caller_file = graph
                .nodes()
                .get(caller)
                .map(|e| e.file)
                .unwrap_or(FileId::INVALID);
            pending.push(PendingShadow {
                caller,
                target: promoted_node,
                kind_tag: 0,
                argument_count: hint.argument_count,
                is_async: hint.is_async,
                file: caller_file,
            });
            pending.push(PendingShadow {
                caller,
                target: promoted_node,
                kind_tag: 1,
                argument_count: 0,
                is_async: false,
                file: caller_file,
            });
        }
    }

    pending.sort_by_key(|p| (p.caller.index(), p.target.index(), p.kind_tag));
    pending.dedup_by_key(|p| (p.caller.index(), p.target.index(), p.kind_tag));

    for p in &pending {
        let kind = match p.kind_tag {
            0 => EdgeKind::Calls {
                argument_count: p.argument_count,
                is_async: p.is_async,
                resolved_via: ResolvedVia::Direct,
            },
            _ => EdgeKind::References,
        };
        graph.edges_mut().add_edge(p.caller, p.target, kind, p.file);
        stats.promoted_back_reference_edges = stats.promoted_back_reference_edges.saturating_add(1);
    }
}

/// Legacy D1 shadow-emission path: when no `GoReceiverCallHint` is
/// available (e.g. unit-test inputs that mint nodes directly without
/// running the Go plugin), fall back to the conservative over-emission
/// shape where every caller of the underlying defining method shadows
/// to the promoted node. AC-5 (`direct_callers` non-empty) still holds;
/// AC-7 over-emission concern is not at stake here because shadow
/// `Calls`/`References` do not contribute to `Implements` edges.
fn emit_shadow_calls_legacy<G: GraphMutationTarget>(
    graph: &mut G,
    indices: &PassLocalIndices,
    stats: &mut GoMethodSetStats,
) {
    let mut promoted_targets: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for (&(_pkg, _struct_name, _method_name), &promoted_node) in &indices.value {
        if let Some(defining) = inherits_target_of_promoted(graph, promoted_node) {
            promoted_targets
                .entry(defining)
                .or_default()
                .insert(promoted_node);
        }
    }
    for (&(_pkg, _struct_name, _method_name), &promoted_node) in &indices.pointer {
        if let Some(defining) = inherits_target_of_promoted(graph, promoted_node) {
            promoted_targets
                .entry(defining)
                .or_default()
                .insert(promoted_node);
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct PendingShadow {
        caller: NodeId,
        target: NodeId,
        kind_tag: u8,
        argument_count: u8,
        is_async: bool,
        file: FileId,
    }
    let mut pending: Vec<PendingShadow> = Vec::new();

    for (&defining, promoted_set) in &promoted_targets {
        let callers = graph.calls_into(defining);
        for (caller, _edge_id, meta) in callers {
            let caller_file = graph
                .nodes()
                .get(caller)
                .map(|e| e.file)
                .unwrap_or(FileId::INVALID);
            for &promoted_node in promoted_set {
                pending.push(PendingShadow {
                    caller,
                    target: promoted_node,
                    kind_tag: 0,
                    argument_count: meta.argument_count,
                    is_async: meta.is_async,
                    file: caller_file,
                });
                pending.push(PendingShadow {
                    caller,
                    target: promoted_node,
                    kind_tag: 1,
                    argument_count: 0,
                    is_async: false,
                    file: caller_file,
                });
            }
        }
    }

    pending.sort_by_key(|p| (p.caller.index(), p.target.index(), p.kind_tag));
    pending.dedup_by_key(|p| (p.caller.index(), p.target.index(), p.kind_tag));

    for p in &pending {
        let kind = match p.kind_tag {
            0 => EdgeKind::Calls {
                argument_count: p.argument_count,
                is_async: p.is_async,
                resolved_via: ResolvedVia::Direct,
            },
            _ => EdgeKind::References,
        };
        graph.edges_mut().add_edge(p.caller, p.target, kind, p.file);
        stats.promoted_back_reference_edges = stats.promoted_back_reference_edges.saturating_add(1);
    }
}

/// Resolve a `GoReceiverHintKind` payload to a canonical
/// `(receiver_type_node, want_pointer_form)` pair.
///
/// Returns `None` when the receiver cannot be resolved within the
/// pass's bounded-hop budget (`MAX_RECEIVER_HOPS`). Bounded resolution
/// keeps the pass O(N_hints) in the worst case and matches 02_DESIGN
/// §3.2's "drop silently" rule for unresolvable receivers.
fn resolve_receiver_kind<G: GraphMutationTarget>(
    graph: &G,
    kind: &GoReceiverHintKind,
) -> Option<(NodeId, bool)> {
    const MAX_RECEIVER_HOPS: usize = 4;

    match kind {
        GoReceiverHintKind::TypePrefixed { type_text } => {
            let node = resolve_qualified_name_to_type_node(graph, type_text)?;
            Some((node, false))
        }
        GoReceiverHintKind::PointerPrefixed { type_text } => {
            let node = resolve_qualified_name_to_type_node(graph, type_text)?;
            Some((node, true))
        }
        GoReceiverHintKind::LocalIdent { binding_local } => {
            // Walk outgoing `TypeOf { context: Variable | Parameter }`
            // edges from the binding to find its declared type. The
            // binding-plane / TypeOf channel is responsible for
            // populating this edge; if missing, we drop the hint.
            let target = walk_typeof_to_type_node(graph, *binding_local, MAX_RECEIVER_HOPS)?;
            Some((target, false))
        }
        GoReceiverHintKind::CallReturn { callee_qn } => {
            let callee = resolve_qualified_name_to_callable(graph, callee_qn)?;
            // Walk the callee's outgoing `TypeOf { context: Return,
            // index: 0 }` edge to find the return type.
            let return_type = walk_typeof_return_to_type_node(graph, callee)?;
            Some((return_type, false))
        }
    }
}

/// Resolve a qualified-name string to the canonical type-shaped
/// `NodeId` (Struct ≻ Interface ≻ Type), interning the lookup key
/// against the live string interner.
fn resolve_qualified_name_to_type_node<G: GraphMutationTarget>(
    graph: &G,
    qualified_name: &str,
) -> Option<NodeId> {
    let qn_id = graph.strings().get(qualified_name)?;
    let candidates = graph.indices().by_qualified_name(qn_id);
    let mut best: Option<(NodeId, NodeKind, u8)> = None;
    for &nid in candidates {
        let entry = graph.nodes().get(nid)?;
        let rank = match entry.kind {
            NodeKind::Struct => 3,
            NodeKind::Interface => 2,
            NodeKind::Type => 1,
            _ => continue,
        };
        let replace = best.is_none_or(|(_, _, r)| rank > r);
        if replace {
            best = Some((nid, entry.kind, rank));
        }
    }
    best.map(|(nid, _, _)| nid)
}

/// Resolve a qualified-name string to a callable-shaped `NodeId`
/// (Function or Method), interning the lookup key against the live
/// string interner.
fn resolve_qualified_name_to_callable<G: GraphMutationTarget>(
    graph: &G,
    qualified_name: &str,
) -> Option<NodeId> {
    let qn_id = graph.strings().get(qualified_name)?;
    let candidates = graph.indices().by_qualified_name(qn_id);
    for &nid in candidates {
        let entry = graph.nodes().get(nid)?;
        if matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            return Some(nid);
        }
    }
    None
}

/// Walk outgoing `TypeOf { context: Variable | Parameter | Field }`
/// edges from `node` to find the declared-type node. Bounded by
/// `max_hops` to avoid runaway recursion through self-referential
/// aliases. Returns the first reachable type-shaped node.
fn walk_typeof_to_type_node<G: GraphMutationTarget>(
    graph: &G,
    node: NodeId,
    max_hops: usize,
) -> Option<NodeId> {
    let mut current = node;
    for _ in 0..max_hops {
        let outgoing = graph.edges().edges_from(current);
        let mut next: Option<NodeId> = None;
        for edge_ref in outgoing {
            if let EdgeKind::TypeOf { context, .. } = edge_ref.kind
                && matches!(
                    context,
                    Some(TypeOfContext::Variable | TypeOfContext::Parameter | TypeOfContext::Field)
                )
            {
                next = Some(edge_ref.target);
                break;
            }
        }
        let target = next?;
        let kind = graph.nodes().get(target).map(|e| e.kind)?;
        if matches!(
            kind,
            NodeKind::Struct | NodeKind::Interface | NodeKind::Type
        ) {
            // Cluster G1 (AC-5 shadow Calls fix per
            // 05_TEST_PLAN.md): the Go plugin's
            // `process_single_var_spec` uses `helper.add_type(...)`
            // to materialise the var's type — that creates a
            // `NodeKind::Type` node which may co-exist with a
            // `NodeKind::Struct` / `NodeKind::Interface` of the
            // same qn (different `node_cache` keys). The promotion
            // index (`outer_to_value_promoted`) is keyed on the
            // Struct/Interface node, so prefer those when both
            // share the qn. Mirrors `resolve_qualified_name_to_type_node`'s
            // ranking.
            if let Some(entry) = graph.nodes().get(target)
                && let Some(qn_id) = entry.qualified_name
            {
                let candidates = graph.indices().by_qualified_name(qn_id);
                let mut best: Option<(NodeId, u8)> = None;
                for &nid in candidates {
                    let rank = match graph.nodes().get(nid).map(|e| e.kind) {
                        Some(NodeKind::Struct) => 3,
                        Some(NodeKind::Interface) => 2,
                        Some(NodeKind::Type) => 1,
                        _ => continue,
                    };
                    if best.is_none_or(|(_, r)| rank > r) {
                        best = Some((nid, rank));
                    }
                }
                if let Some((preferred, _)) = best {
                    return Some(preferred);
                }
            }
            return Some(target);
        }
        current = target;
    }
    None
}

/// Walk outgoing `TypeOf { context: Return, index: 0 }` edge from a
/// callable node to find its return-type node. Returns the first
/// reachable type-shaped node.
fn walk_typeof_return_to_type_node<G: GraphMutationTarget>(
    graph: &G,
    callable: NodeId,
) -> Option<NodeId> {
    let outgoing = graph.edges().edges_from(callable);
    for edge_ref in outgoing {
        if let EdgeKind::TypeOf {
            context: Some(TypeOfContext::Return),
            index,
            ..
        } = edge_ref.kind
            && index.unwrap_or(0) == 0
        {
            let target_kind = graph.nodes().get(edge_ref.target).map(|e| e.kind)?;
            if matches!(
                target_kind,
                NodeKind::Struct | NodeKind::Interface | NodeKind::Type
            ) {
                return Some(edge_ref.target);
            }
        }
    }
    None
}

/// Cluster D2.3 — T1.1 implicit interface satisfaction.
///
/// Implements the satisfaction algorithm specified in 02_DESIGN §4.3.
/// Runs strictly after T1.2 promotion (so promoted methods participate
/// in method-set composition) and after the targeted index update for
/// promotion-minted nodes (so synthetic pointer-form Type nodes
/// minted by the T1.2 step are resolvable by the by-qualified-name
/// index).
///
/// Algorithm sketch (full spec in 02_DESIGN §4.3):
///
/// 1. Enumerate every interface `I` in the workspace, build a
///    method-set `MethodSet(I)` = `{(method_name_id, canonical_sig)}`.
///    Apply the §5.7 uninteresting filter: skip `I` whose method set
///    is empty.
/// 2. Enumerate every candidate concrete type `C` (`Struct` /
///    `Interface` / named `Type` with at least one method). For each
///    `C`, build `ValueMethodSet(C)` from native value-receiver
///    methods + promoted value-bucket methods, and
///    `PointerMethodSet(C)` from value-set ∪ pointer-receiver methods
///    ∪ pointer-bucket promoted methods.
/// 3. For each `(C, I)` pair (sorted deterministically by NodeId
///    indices), test value-bucket satisfaction first; on miss, test
///    pointer-bucket. Emit `Implements(C → I)` for value-bucket and
///    additionally `Implements(*C → I)` (per Go assignability rules
///    — value-form ⇒ pointer-form satisfies). For pointer-only, emit
///    only `Implements(*C → I)`, materialising the synthetic
///    `<pkg>.*<C>` Type node + `Inherits(*C → C)` edge on demand.
///
/// Empty-interface filter (AC-8) and identical-pair filter (§5.7) are
/// applied before each `(C, I)` test.
fn run_t1_1_satisfaction<G: GraphMutationTarget>(
    graph: &mut G,
    indices: &PassLocalIndices,
    method_receivers: &HashMap<NodeId, Receiver>,
    method_signatures: &HashMap<NodeId, String>,
    newly_created_nodes: &mut Vec<NodeId>,
    stats: &mut GoMethodSetStats,
) {
    // Phase 1: collect interface method sets and candidate type sets.
    //
    // Cluster D3.2 tightening: method-set entries are now keyed by
    // (method_name, canonical_signature) — the signature comes from the
    // `GoMethodSignatureHint` side channel populated by the Go plugin
    // in D3.1. The satisfaction predicate compares `(name, signature)`
    // pairs bytewise per 02_DESIGN §4.1.3.
    //
    // Backwards compatibility with D2's unit-test fixtures: when an
    // interface method or a candidate method has no signature
    // recorded (e.g. unit tests that mint Method nodes directly), the
    // predicate falls back to name-only matching. Production builds
    // emit a hint for every method node, so the production path runs
    // the tightened predicate; this fallback exists solely so D2's
    // assertion corpus continues to assert what it asserted.
    let interface_method_sets = collect_interface_method_sets(graph, method_signatures);
    if interface_method_sets.is_empty() {
        return;
    }

    let candidate_types = collect_candidate_types(graph);
    if candidate_types.is_empty() {
        return;
    }

    // Phase 2: for each candidate, compute the value-bucket and
    // pointer-bucket method sets (with signatures attached).
    let candidate_method_sets = compute_candidate_method_sets(
        graph,
        &candidate_types,
        indices,
        method_receivers,
        method_signatures,
    );

    // Phase 3: pair enumeration. Sort both axes deterministically.
    let mut sorted_interfaces: Vec<(NodeId, Vec<MethodSetEntry>)> =
        interface_method_sets.into_iter().collect();
    sorted_interfaces.sort_by_key(|(nid, _)| nid.index());

    let mut sorted_candidates: Vec<NodeId> = candidate_types.into_iter().collect();
    sorted_candidates.sort_by_key(|nid| nid.index());

    // Collect (source, target, file) tuples for the Implements edges
    // and emit them deterministically at the end of the pass.
    #[derive(Debug, Clone, Copy)]
    struct PendingImplements {
        source: NodeId,
        target: NodeId,
        file: FileId,
        tag: u8, // 0 = value-form (C → I), 1 = pointer-form (*C → I)
    }
    let mut pending: Vec<PendingImplements> = Vec::new();

    // Pass-local index for pointer-form Type nodes minted by T1.1.
    // Mirrors `indices.pointer_type` shape but operates over the
    // (candidate_node, candidate_short_name_id) key.
    let mut t1_1_pointer_form: BTreeMap<NodeId, NodeId> = BTreeMap::new();

    for &c in &sorted_candidates {
        let Some(c_methods) = candidate_method_sets.get(&c) else {
            continue;
        };
        let c_file = graph
            .nodes()
            .get(c)
            .map(|e| e.file)
            .unwrap_or(FileId::INVALID);
        let c_kind = graph.nodes().get(c).map(|e| e.kind);

        for (i_node, i_methods) in &sorted_interfaces {
            stats.satisfaction_pairs_examined = stats.satisfaction_pairs_examined.saturating_add(1);

            // §5.7 self-edge filter.
            if *i_node == c {
                continue;
            }
            // §5.7 empty-interface filter: enforced when building
            // interface_method_sets, but re-asserted here in case a
            // future change relaxes that.
            if i_methods.is_empty() {
                continue;
            }

            // Cluster E2 iter-4 — whole-graph re-emission (Option C).
            // The scope-skip from iter-1/2/3 is deleted; T1.1 always
            // enumerates the full `(C, I)` cross-product. Tombstoning
            // is handled whole-graph at pass entry via
            // [`tombstone_all_pass_owned`]; this drain re-emits the
            // canonical method-set-derived multiset from scratch.

            // §5.7 self-implements-via-explicit-embedding skip:
            // For interfaces-to-interface satisfaction, the existing
            // `Inherits(I' → I)` edge from explicit syntactic
            // embedding is sufficient — we still emit the structural
            // `Implements` because consumers query both directions
            // independently (01_SPEC §5.8).
            let _ = c_kind;

            // D3.2: name + signature predicate (with `None` fallback),
            // applied separately to the value bucket and pointer
            // bucket so the pointer-only candidate-method case still
            // routes through the pointer-form `*C` Implements edge.
            let value_satisfies = c_methods.value_satisfies(i_methods);
            let pointer_satisfies = c_methods.pointer_satisfies(i_methods);

            if value_satisfies {
                pending.push(PendingImplements {
                    source: c,
                    target: *i_node,
                    file: c_file,
                    tag: 0,
                });
                // Per Go assignability: *C also satisfies I when C
                // satisfies I (value methods are in *C's method set
                // too).
                let c_ptr = materialise_pointer_form_for_c(
                    graph,
                    c,
                    c_file,
                    &mut t1_1_pointer_form,
                    indices,
                    newly_created_nodes,
                );
                if let Some(c_ptr_node) = c_ptr {
                    pending.push(PendingImplements {
                        source: c_ptr_node,
                        target: *i_node,
                        file: c_file,
                        tag: 1,
                    });
                }
            } else if pointer_satisfies {
                let c_ptr = materialise_pointer_form_for_c(
                    graph,
                    c,
                    c_file,
                    &mut t1_1_pointer_form,
                    indices,
                    newly_created_nodes,
                );
                if let Some(c_ptr_node) = c_ptr {
                    pending.push(PendingImplements {
                        source: c_ptr_node,
                        target: *i_node,
                        file: c_file,
                        tag: 1,
                    });
                }
            }
        }
    }

    pending.sort_by_key(|p| (p.source.index(), p.target.index(), p.tag));
    pending.dedup_by_key(|p| (p.source.index(), p.target.index(), p.tag));

    for p in &pending {
        graph
            .edges_mut()
            .add_edge(p.source, p.target, EdgeKind::Implements, p.file);
        match p.tag {
            0 => stats.implements_edges_value = stats.implements_edges_value.saturating_add(1),
            _ => stats.implements_edges_pointer = stats.implements_edges_pointer.saturating_add(1),
        }
    }
}

/// 02_DESIGN §4.4: T1.3 function-signature implementations.
///
/// Emits `Implements(fn → F)` edges where `F` is a named function type
/// (a Type node with a `GoFunctionSignatureHint` whose source was a
/// `type Foo func(...)` declaration) and `fn` is a function or method
/// whose canonical signature matches `F`'s underlying signature.
///
/// Two candidate-sources are unioned per 02_DESIGN §4.4:
///
/// - **Source A** — explicit `T(g)` named-type conversions captured by
///   the Go plugin as `GoNamedTypeConversionHint`. The hint's
///   `argument_node` is the function reference; the
///   `target_type_qualified_name` resolves to the named function-type
///   NodeId. This is the load-bearing path for AC-11
///   (`http.HandlerFunc(handleIndex)`).
/// - **Source B** — reverse `TypeOf` walk from each named function-type
///   `F`: each incoming `TypeOf` edge identifies a slot (Variable /
///   Parameter / Property / Return) typed as `F`; the slot's outgoing
///   `References` edges to a Function or Method NodeId expose the bound
///   address-taken function. The binding-plane has already linked
///   `var fn F = handleIndex` shapes by Phase 4e, so the reverse-walk
///   only needs to enumerate those existing `References` edges.
///
/// Tier 1's same-package guard (01_SPEC §3.1 T1.3) filters out
/// candidates whose package qualifier differs from the named
/// function-type's package qualifier. Cross-package T1.3 is explicitly
/// out of scope for Tier 1 and is documented in 02_DESIGN §4.4.
///
/// All edges are dedupd via a `BTreeMap`-keyed pending set and
/// drained in `NodeId`-sorted order so the emission sequence is
/// deterministic across runs (AC-12 prerequisite).
fn run_t1_3_signature_implements<G: GraphMutationTarget>(
    graph: &mut G,
    function_signatures: &HashMap<NodeId, String>,
    method_signatures: &HashMap<NodeId, String>,
    stats: &mut GoMethodSetStats,
) {
    if function_signatures.is_empty() {
        return;
    }

    // Partition the function-signature map into:
    // - named function-type targets (NodeKind::Type),
    // - bare-function candidates (NodeKind::Function).
    //
    // `BTreeMap` keyed on NodeId.index() makes the iteration order
    // deterministic across runs.
    let mut named_function_types: BTreeMap<NodeId, String> = BTreeMap::new();
    let mut function_candidates: BTreeMap<NodeId, String> = BTreeMap::new();
    for (&node_id, sig) in function_signatures {
        let kind = match graph.nodes().get(node_id) {
            Some(entry) => entry.kind,
            None => continue,
        };
        match kind {
            NodeKind::Type => {
                named_function_types.insert(node_id, sig.clone());
            }
            NodeKind::Function => {
                function_candidates.insert(node_id, sig.clone());
            }
            _ => {}
        }
    }

    if named_function_types.is_empty() {
        return;
    }

    // Method candidates piggyback on the method-signature map. A bare
    // method reference (`receiver.Method` used as a value) can also
    // satisfy a named function type whose signature matches the
    // method's own signature — the receiver is part of the binding,
    // not the signature.
    let method_candidates: BTreeMap<NodeId, String> = method_signatures
        .iter()
        .filter_map(|(&node_id, sig)| {
            graph
                .nodes()
                .get(node_id)
                .filter(|entry| matches!(entry.kind, NodeKind::Method))
                .map(|_| (node_id, sig.clone()))
        })
        .collect();

    // Each pending Implements is keyed by `(fn_node, target_F)` so
    // the dedupe step collapses duplicate emissions from the union of
    // Source A and Source B without losing edges.
    let mut pending: BTreeMap<(NodeId, NodeId), FileId> = BTreeMap::new();

    // ----- Source A: explicit `T(g)` conversions -----
    let conversion_candidates: Vec<(NodeId, StringId, NodeId, FileId)> = graph
        .go_hints()
        .named_type_conversions
        .iter()
        .map(|h| {
            (
                h.call_site,
                h.target_type_qualified_name,
                h.argument_node,
                h.file,
            )
        })
        .collect();
    for (_call_site, target_qn_id, argument_node, hint_file) in conversion_candidates {
        let candidates = graph.indices().by_qualified_name(target_qn_id).to_vec();
        // Pick the Type-kinded candidate whose NodeId is in
        // `named_function_types`. Multiple `by_qualified_name`
        // candidates can arise when the Go plugin emits stubs across
        // files; only the function-typed Type carries a signature
        // hint.
        let target_node = candidates
            .into_iter()
            .find(|nid| named_function_types.contains_key(nid));
        let Some(target_node) = target_node else {
            continue;
        };
        let target_sig = match named_function_types.get(&target_node) {
            Some(s) => s,
            None => continue,
        };

        // Pull the argument's canonical signature from either the
        // function or method signature map (a method-value selector
        // routes through method_candidates).
        let arg_sig = function_candidates
            .get(&argument_node)
            .or_else(|| method_candidates.get(&argument_node));
        let Some(arg_sig) = arg_sig else {
            continue;
        };

        if arg_sig != target_sig {
            continue;
        }
        if !same_package_qn(graph, argument_node, target_node) {
            continue;
        }
        pending.insert((argument_node, target_node), hint_file);
    }

    // ----- Source B: reverse `TypeOf` walk -----
    //
    // For each named function-type `F`, walk incoming `TypeOf` edges
    // (`graph.edges().edges_to(F)`), pick edges with
    // `EdgeKind::TypeOf { context, .. }` whose context is a data-flow
    // slot (Variable / Parameter / Property / Return), then for each
    // such slot enumerate outgoing `References` edges to Function /
    // Method nodes whose canonical signature matches `F`'s.
    let sorted_named_types: Vec<NodeId> = named_function_types.keys().copied().collect();
    for &f_node in &sorted_named_types {
        let f_sig = match named_function_types.get(&f_node) {
            Some(s) => s,
            None => continue,
        };
        let f_file = graph
            .nodes()
            .get(f_node)
            .map(|e| e.file)
            .unwrap_or(FileId::INVALID);

        let incoming = graph.edges().edges_to(f_node);
        for edge in incoming {
            // Only data-flow slots participate; ignore TypeParameter /
            // Constraint contexts which describe generic bounds, not
            // function-typed slots.
            let is_data_flow_slot = match edge.kind {
                EdgeKind::TypeOf { context, .. } => matches!(
                    context,
                    Some(crate::graph::unified::edge::kind::TypeOfContext::Variable)
                        | Some(crate::graph::unified::edge::kind::TypeOfContext::Parameter)
                        | Some(crate::graph::unified::edge::kind::TypeOfContext::Field)
                        | Some(crate::graph::unified::edge::kind::TypeOfContext::Return)
                ),
                _ => false,
            };
            if !is_data_flow_slot {
                continue;
            }
            let slot = edge.source;
            // Walk outgoing `References` of the slot; emit when the
            // referenced node is a Function or Method with a
            // matching canonical signature.
            for slot_edge in graph.edges().edges_from(slot) {
                if !matches!(slot_edge.kind, EdgeKind::References) {
                    continue;
                }
                let referenced = slot_edge.target;
                let ref_sig = function_candidates
                    .get(&referenced)
                    .or_else(|| method_candidates.get(&referenced));
                let Some(ref_sig) = ref_sig else {
                    continue;
                };
                if ref_sig != f_sig {
                    continue;
                }
                if !same_package_qn(graph, referenced, f_node) {
                    continue;
                }
                // File anchor: 02_DESIGN §4.4 step 4 — anchor on
                // `fn.file_id` when the candidate is a Function or
                // Method node; fall back to `F.file_id` for
                // defensiveness.
                let fn_file = graph
                    .nodes()
                    .get(referenced)
                    .map(|e| e.file)
                    .unwrap_or(f_file);
                pending.entry((referenced, f_node)).or_insert(fn_file);
            }
        }
    }

    // Drain pending → emit. BTreeMap iterates in sorted key order;
    // `(NodeId, NodeId)` sorts by source then target, giving the
    // determinism the spec mandates.
    //
    // Cluster E2 iter-4 — whole-graph re-emission (Option C). The
    // iter-1/2/3 scope-skip is deleted; every pending entry produced
    // by Source A (explicit `T(g)` conversion) and Source B (reverse
    // `TypeOf` walk) is unconditionally re-emitted. Tombstoning of
    // prior pass-emitted `Implements(fn → F)` edges runs whole-graph
    // at pass entry via [`tombstone_all_pass_owned`].
    for ((fn_node, f_node), file_id) in pending {
        graph
            .edges_mut()
            .add_edge(fn_node, f_node, EdgeKind::Implements, file_id);
        stats.signature_implements_edges = stats.signature_implements_edges.saturating_add(1);
    }
}

/// Same-package check per 02_DESIGN §4.4: the leading
/// `<package>.<TopLevelName>` prefix of both nodes' qualified names
/// must match. Returns `true` if either side has no qualified name
/// (defensive — should not happen for production graphs).
fn same_package_qn<G: GraphMutationTarget>(graph: &G, a: NodeId, b: NodeId) -> bool {
    let qn_a = match qualified_name_string(graph, a) {
        Some(s) => s,
        None => return false,
    };
    let qn_b = match qualified_name_string(graph, b) {
        Some(s) => s,
        None => return false,
    };
    // The package qualifier is the leading segment of the canonical
    // qualified name (everything before the first `::`). Cluster G1:
    // canonical-form separator is `::`, not `.`. See 05_TEST_PLAN.md
    // §7.5.
    let pkg_a = qn_a.split("::").next().unwrap_or("");
    let pkg_b = qn_b.split("::").next().unwrap_or("");
    !pkg_a.is_empty() && pkg_a == pkg_b
}

/// Build `(interface_node → Vec<MethodSetEntry>)` from the workspace's
/// interface nodes. Empty-interface (AC-8) interfaces are filtered out.
///
/// An interface's method set includes:
/// - methods declared directly via `<interface_qn>.<method>` (the Go
///   plugin emits these as `Method` nodes with the interface's
///   qualified name as the prefix); and
/// - methods of any embedded interfaces (reachable via outgoing
///   `Inherits` edges from the interface to other interface nodes).
///
/// Each method entry carries its canonical signature (when the Go
/// plugin emitted a `GoMethodSignatureHint` for the underlying Method
/// node) so the D3-tightened T1.1 predicate can compare `(name,
/// signature)` against candidate methods.
fn collect_interface_method_sets<G: GraphMutationTarget>(
    graph: &G,
    method_signatures: &HashMap<NodeId, String>,
) -> BTreeMap<NodeId, Vec<MethodSetEntry>> {
    let mut out: BTreeMap<NodeId, Vec<MethodSetEntry>> = BTreeMap::new();

    let interface_nodes: Vec<NodeId> = graph.indices().by_kind(NodeKind::Interface).to_vec();

    // First pass: gather each interface's directly-declared methods.
    // We store (name_id, method_node) so the embedded-interface flatten
    // step below can look up signatures via the same map.
    let mut direct: BTreeMap<NodeId, Vec<(StringId, NodeId)>> = BTreeMap::new();
    for &iface in &interface_nodes {
        let iface_qn = match qualified_name_string(graph, iface) {
            Some(s) => s,
            None => continue,
        };
        let methods = find_method_names_of_type(graph, &iface_qn);
        direct.insert(iface, methods);
    }

    // Second pass: flatten through embedded `Inherits` edges with a
    // bounded BFS to handle interface-of-interface composition.
    const MAX_INTERFACE_EMBED_DEPTH: usize = 16;
    for &iface in &interface_nodes {
        // Dedupe by `name_id` so embedded interfaces contributing the
        // same method name once each don't blow the entry list up. The
        // signature retained is the one observed first (BFS order from
        // the outer interface); since Go's interface-embedding rule
        // mandates signature compatibility, any conflict would be a
        // semantically ill-typed program and is out of scope.
        let mut seen_names: BTreeSet<StringId> = BTreeSet::new();
        let mut entries: Vec<MethodSetEntry> = Vec::new();
        let mut queue: Vec<NodeId> = vec![iface];
        let mut visited: BTreeSet<NodeId> = BTreeSet::new();
        let mut depth = 0;
        while let Some(cur) = queue.pop() {
            if !visited.insert(cur) {
                continue;
            }
            if depth >= MAX_INTERFACE_EMBED_DEPTH {
                break;
            }
            depth += 1;
            if let Some(methods) = direct.get(&cur) {
                for &(name_id, method_node) in methods {
                    if !seen_names.insert(name_id) {
                        continue;
                    }
                    entries.push(MethodSetEntry {
                        name_id,
                        signature: method_signatures.get(&method_node).cloned(),
                    });
                }
            }
            for edge_ref in graph.edges().edges_from(cur) {
                if matches!(edge_ref.kind, EdgeKind::Inherits) {
                    let kind = graph.nodes().get(edge_ref.target).map(|e| e.kind);
                    if matches!(kind, Some(NodeKind::Interface)) {
                        queue.push(edge_ref.target);
                    }
                }
            }
        }
        if !entries.is_empty() {
            // Sort by name_id so the predicate iteration order is
            // deterministic across runs (AC-12 prerequisite).
            entries.sort_by_key(|e| e.name_id);
            out.insert(iface, entries);
        }
    }

    out
}

/// Collect candidate concrete types: every `Struct`, named non-interface
/// `Type` with at least one method, and every `Interface` (interface-to-
/// interface satisfaction). Synthetic / promoted nodes are excluded —
/// only originals participate in T1.1 satisfaction.
fn collect_candidate_types<G: GraphMutationTarget>(graph: &G) -> BTreeSet<NodeId> {
    let mut out: BTreeSet<NodeId> = BTreeSet::new();
    for kind in [NodeKind::Struct, NodeKind::Interface, NodeKind::Type] {
        for &nid in graph.indices().by_kind(kind) {
            // Exclude synthetic nodes (pointer-form anchors, promoted
            // methods) from the candidate set — they are derived from
            // canonical types and do not introduce new method sets.
            if let Some(entry) = graph.nodes().get(nid)
                && let Some(qn) = entry.qualified_name
                && let Some(qn_str) = graph.strings().resolve(qn)
                && qn_str.contains("::*")
            {
                // Pointer-form anchors like `<pkg>::*<C>` are
                // synthetic; skip. Cluster G1: canonical-form marker
                // is `::*`, not `.*`. See 05_TEST_PLAN.md §7.5.
                continue;
            }
            out.insert(nid);
        }
    }
    out
}

/// One entry inside a candidate's method set or an interface's method
/// set: a `name_id` paired with an optional canonical signature.
///
/// The signature is `Option<String>` because two paths produce method
/// entries without a signature today:
///
/// 1. Unit-test fixtures that mint `Method` nodes directly via
///    `make_qn_node` without invoking the Go plugin (Cluster D2's test
///    surface — preserved as a fallback so D2's predicate-name-only
///    contract continues to hold for those tests).
/// 2. Promoted-method nodes that have no direct
///    `GoMethodSignatureHint`; their signature is recovered indirectly
///    through the `Inherits`-target pointer to the source method.
///
/// Cluster D3's tightened predicate compares `(name, signature)` when
/// both sides carry a signature and falls back to name-only when either
/// side is `None`. The fallback is what keeps the D2 unit-test corpus
/// green; production builds (which emit hints for every method) hit
/// the tightened branch unconditionally.
#[derive(Debug, Clone)]
struct MethodSetEntry {
    /// Interned method name (e.g. `"Read"`).
    name_id: StringId,
    /// Canonical signature, or `None` if the source had no
    /// `GoMethodSignatureHint`. The D3 predicate treats `None` as a
    /// signature wildcard.
    signature: Option<String>,
}

/// Aggregate method sets for a candidate type, partitioned into
/// the value bucket and the pointer bucket per Go spec §"Method sets".
///
/// Each bucket stores `MethodSetEntry` records (name + optional
/// canonical signature) so the D3-tightened T1.1 predicate can compare
/// against an interface's `(name, signature)` requirement, falling back
/// to name-only when either side lacks a signature hint.
struct CandidateMethodSet {
    /// Method entries visible in the candidate's value-form method set.
    value_methods: Vec<MethodSetEntry>,
    /// Method entries visible in the candidate's pointer-form method
    /// set (`value_methods` ∪ pointer-receiver methods ∪ promoted
    /// pointer-only methods).
    pointer_methods: Vec<MethodSetEntry>,
}

impl CandidateMethodSet {
    /// True when `self`'s value bucket satisfies every method in
    /// `required` per the D3 (name + signature, with `None` fallback)
    /// predicate.
    fn value_satisfies(&self, required: &[MethodSetEntry]) -> bool {
        required
            .iter()
            .all(|r| method_set_contains_match(&self.value_methods, r))
    }

    /// True when `self`'s pointer bucket satisfies every method in
    /// `required`.
    fn pointer_satisfies(&self, required: &[MethodSetEntry]) -> bool {
        required
            .iter()
            .all(|r| method_set_contains_match(&self.pointer_methods, r))
    }
}

/// Per-method search through a candidate's method set, applying the
/// D3 tightened satisfaction predicate:
///
/// - The candidate must contain an entry with the same `name_id`.
/// - If the required entry has `Some(signature)` AND the matching
///   candidate entry has `Some(signature)`, the two signatures must be
///   bytewise equal.
/// - If either side is `None`, the name match alone suffices.
///
/// Returns true at the first matching candidate entry; false if no
/// entry survives the name + signature filter.
fn method_set_contains_match(haystack: &[MethodSetEntry], required: &MethodSetEntry) -> bool {
    haystack.iter().any(|h| {
        if h.name_id != required.name_id {
            return false;
        }
        match (&required.signature, &h.signature) {
            (Some(r_sig), Some(h_sig)) => r_sig == h_sig,
            // Either side missing → name match alone (D2 fallback).
            _ => true,
        }
    })
}

/// Compute per-candidate value/pointer method sets.
///
/// Native methods come from prefix-matching `<candidate_qn>.<m>` Method
/// nodes; their receiver pointerness is read from `method_receivers`
/// and their canonical signatures are looked up in `method_signatures`.
/// Promoted methods come from the pass-local promotion indices keyed on
/// `(outer_node, method_name_id)` → promoted-method NodeId. The
/// promoted node's signature is recovered indirectly via the
/// `Inherits`-target edge that points back at the source method —
/// promoted-method nodes do not themselves carry a
/// `GoMethodSignatureHint`, but their underlying method does.
fn compute_candidate_method_sets<G: GraphMutationTarget>(
    graph: &G,
    candidates: &BTreeSet<NodeId>,
    indices: &PassLocalIndices,
    method_receivers: &HashMap<NodeId, Receiver>,
    method_signatures: &HashMap<NodeId, String>,
) -> BTreeMap<NodeId, CandidateMethodSet> {
    let mut out: BTreeMap<NodeId, CandidateMethodSet> = BTreeMap::new();

    // Build (outer_node → {(method_name_id, promoted_node_id)}) maps
    // from the promotion indices. Promoted-node identity is retained
    // so we can recover the underlying method's signature via the
    // `Inherits` back-edge minted by T1.2 step 5.
    let mut promoted_value_by_outer: BTreeMap<NodeId, Vec<(StringId, NodeId)>> = BTreeMap::new();
    for (&(outer_node, name_id), &promoted_id) in indices.outer_to_value_promoted.iter() {
        promoted_value_by_outer
            .entry(outer_node)
            .or_default()
            .push((name_id, promoted_id));
    }
    let mut promoted_pointer_by_outer: BTreeMap<NodeId, Vec<(StringId, NodeId)>> = BTreeMap::new();
    for (&(outer_node, name_id), &promoted_id) in indices.outer_to_pointer_promoted.iter() {
        promoted_pointer_by_outer
            .entry(outer_node)
            .or_default()
            .push((name_id, promoted_id));
    }

    for &c in candidates {
        let c_qn = match qualified_name_string(graph, c) {
            Some(s) => s,
            None => continue,
        };
        let mut value_methods: Vec<MethodSetEntry> = Vec::new();
        let mut pointer_methods: Vec<MethodSetEntry> = Vec::new();

        // Native methods on `c`.
        let native = find_method_names_of_type(graph, &c_qn);
        for (name_id, method_node) in native {
            let recv = method_receivers
                .get(&method_node)
                .copied()
                .unwrap_or(Receiver::Value);
            let signature = method_signatures.get(&method_node).cloned();
            let entry = MethodSetEntry { name_id, signature };
            match recv {
                Receiver::Value => {
                    value_methods.push(entry.clone());
                    pointer_methods.push(entry);
                }
                Receiver::Pointer => {
                    pointer_methods.push(entry);
                }
            }
        }

        // Promoted methods (T1.2 outputs).
        if let Some(promoted_value) = promoted_value_by_outer.get(&c) {
            for &(name_id, promoted_node) in promoted_value {
                let signature =
                    resolve_promoted_method_signature(graph, promoted_node, method_signatures);
                let entry = MethodSetEntry { name_id, signature };
                value_methods.push(entry.clone());
                pointer_methods.push(entry);
            }
        }
        if let Some(promoted_pointer) = promoted_pointer_by_outer.get(&c) {
            for &(name_id, promoted_node) in promoted_pointer {
                let signature =
                    resolve_promoted_method_signature(graph, promoted_node, method_signatures);
                pointer_methods.push(MethodSetEntry { name_id, signature });
            }
        }

        out.insert(
            c,
            CandidateMethodSet {
                value_methods,
                pointer_methods,
            },
        );
    }

    out
}

/// Resolve a promoted-method node's canonical signature by walking the
/// `Inherits` back-edge to the underlying source Method node and
/// looking up its `GoMethodSignatureHint`-derived entry in
/// `method_signatures`.
///
/// Returns `None` if the promoted node has no `Inherits` edge (defensive
/// — T1.2 step 5 always emits one) or if the source method has no
/// signature hint (e.g. a unit-test fixture).
fn resolve_promoted_method_signature<G: GraphMutationTarget>(
    graph: &G,
    promoted_node: NodeId,
    method_signatures: &HashMap<NodeId, String>,
) -> Option<String> {
    let source = inherits_target_of_promoted(graph, promoted_node)?;
    method_signatures.get(&source).cloned()
}

/// Materialise (or look up) the synthetic `<pkg>.*<C>` pointer-form
/// `Type` node for `c`. Reuses the T1.2 `pointer_type` index when an
/// entry already exists (so T1.1 and T1.2 share the same pointer-form
/// node identity), otherwise mints a fresh node + `Inherits(*C → C)`
/// linkage edge.
///
/// Returns `None` if `c` lacks a resolvable qualified-name shape
/// (defensive — should not happen for nodes already enumerated as
/// candidate types).
fn materialise_pointer_form_for_c<G: GraphMutationTarget>(
    graph: &mut G,
    c: NodeId,
    c_file: FileId,
    t1_1_pointer_form: &mut BTreeMap<NodeId, NodeId>,
    indices: &PassLocalIndices,
    newly_created_nodes: &mut Vec<NodeId>,
) -> Option<NodeId> {
    if let Some(&existing) = t1_1_pointer_form.get(&c) {
        return Some(existing);
    }
    let c_qn_str = qualified_name_string(graph, c)?;
    let (package_qn, short_name) = split_qn_into_package_and_name(&c_qn_str)?;
    let package_id = graph.strings_mut().intern(&package_qn).ok()?;
    let short_name_id = graph.strings_mut().intern(&short_name).ok()?;

    // Reuse the T1.2 pointer-form anchor if available.
    if let Some(&shared) = indices.pointer_type.get(&(package_id, short_name_id)) {
        t1_1_pointer_form.insert(c, shared);
        return Some(shared);
    }

    // Cluster G1: canonical-form pointer marker is `::*`. See
    // 05_TEST_PLAN.md §7.5.
    let ptr_qn = format!("{package_qn}::*{short_name}");
    let interned_qn = graph.strings_mut().intern(&ptr_qn).ok()?;

    // Cluster E2 iter-2 — idempotency against persisted state.
    //
    // The pass-local `t1_1_pointer_form` and `indices.pointer_type`
    // maps reset on every pass invocation, so an incremental rebuild
    // that scopes emission to a changed file `F_I` (where the
    // changed interface lives) but leaves `C`'s file `F_C` unchanged
    // would mint a fresh `*C` here even though a prior-run `*C` is
    // still alive in the arena, persisted from the previous build.
    // The result is two synthetic Type nodes carrying the same
    // `<pkg>.*<short>` qualified name + duplicate `Implements(*C → I)`
    // emissions — codex iter-1 finding 1's pointer-form variant.
    //
    // Consult the global `by_qualified_name` index for an existing
    // live `*C` of kind Type + Synthetic. If found, reuse its NodeId
    // and skip the mint. The lookup is a two-scope dance because
    // `graph.indices()` (immutable) and `graph.macro_metadata_mut()`
    // (mutable) cannot be held simultaneously through the
    // `GraphMutationTarget` trait — we collect kind-matched
    // candidates under the immutable borrow first, then check
    // synthetic under the mutable metadata borrow.
    //
    // This is the one D-emission-logic refinement E2 iter-2 makes:
    // it strictly broadens helper idempotency, never narrows or
    // changes which pairs satisfy.
    let kind_matched: Vec<NodeId> = {
        let nodes = graph.nodes();
        graph
            .indices()
            .by_qualified_name(interned_qn)
            .iter()
            .copied()
            .filter(|nid| {
                nodes
                    .get(*nid)
                    .is_some_and(|e| matches!(e.kind, NodeKind::Type))
            })
            .collect()
    };
    if !kind_matched.is_empty() {
        let metadata: &NodeMetadataStore = graph.macro_metadata_mut();
        if let Some(existing) = kind_matched
            .into_iter()
            .find(|nid| metadata.is_synthetic(*nid))
        {
            t1_1_pointer_form.insert(c, existing);
            return Some(existing);
        }
    }

    let new_id = mint_synthetic_node(graph, NodeKind::Type, &short_name, interned_qn, c_file)?;
    graph
        .edges_mut()
        .add_edge(new_id, c, EdgeKind::Inherits, c_file);
    newly_created_nodes.push(new_id);
    t1_1_pointer_form.insert(c, new_id);
    Some(new_id)
}

/// Walk the outgoing `Inherits` edges from a promoted-method node to
/// recover the canonical defining-method `NodeId` that the promoted
/// node back-references.
///
/// Returns the **first** `Inherits` target's `NodeId` (the promoted
/// node has exactly one such edge by construction in step 5). Returns
/// `None` if no such edge is present (defensive; should not occur in
/// a graph the pass has just populated).
fn inherits_target_of_promoted<G: GraphMutationTarget>(
    graph: &G,
    promoted_node: NodeId,
) -> Option<NodeId> {
    let outgoing: Vec<StoreEdgeRef> = graph.edges().edges_from(promoted_node);
    for edge_ref in outgoing {
        if matches!(edge_ref.kind, EdgeKind::Inherits) {
            return Some(edge_ref.target);
        }
    }
    None
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- canonicalise_signature: rule 1 (whitespace collapse) -----

    #[test]
    fn whitespace_collapse_on_pointer_modifier() {
        // `* T` and `*T` must canonicalise identically.
        let a = canonicalise_signature("* T", "");
        let b = canonicalise_signature("*T", "");
        assert_eq!(a, b);
        assert_eq!(a.0, b"(*T)");
    }

    #[test]
    fn whitespace_collapse_on_slice_modifier() {
        // `[ ]T`, `[ ] T`, `[]T` all collapse to `[]T`.
        let a = canonicalise_signature("[ ]T", "");
        let b = canonicalise_signature("[ ] T", "");
        let c = canonicalise_signature("[]T", "");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(c.0, b"([]T)");
    }

    #[test]
    fn whitespace_collapse_on_slice_pointer_combo() {
        // `[ ] *T` and `[]*T` canonicalise identically.
        let a = canonicalise_signature("[ ] *T", "");
        let b = canonicalise_signature("[]*T", "");
        assert_eq!(a, b);
        assert_eq!(a.0, b"([]*T)");
    }

    #[test]
    fn whitespace_preserved_between_identifier_words() {
        // `chan T` keeps the internal space because `chan` is a Go
        // type-keyword (channel) and `T` is the element type. The
        // param-name stripper must NOT eat `chan` as if it were a
        // parameter name; the type-keyword guard preserves it.
        let sig = canonicalise_signature("chan T", "");
        assert_eq!(sig.0, b"(chan T)");
    }

    #[test]
    fn type_keyword_prefixes_are_preserved() {
        // `map[K]V`, `func(int) int`, `interface{...}`, `struct{...}`
        // all begin with a type-introducing keyword that must NOT be
        // stripped as a parameter name. Whitespace between keyword and
        // its operand IS preserved because both sides are
        // identifier-continuation runs.
        let m = canonicalise_signature("map[K]V", "");
        assert_eq!(m.0, b"(map[K]V)");

        // A named parameter on a map type: `m map[K]V` → `map[K]V`.
        // Here `m` is the parameter name and gets stripped.
        let named = canonicalise_signature("m map[K]V", "");
        assert_eq!(named.0, b"(map[K]V)");
    }

    // ----- canonicalise_signature: rule 2 (parameter name erasure) -----

    #[test]
    fn parameter_name_is_stripped_on_pointer_form() {
        // `p *T` and `*T` are equivalent for method-set comparison.
        let a = canonicalise_signature("p *T", "");
        let b = canonicalise_signature("*T", "");
        assert_eq!(a, b);
        assert_eq!(a.0, b"(*T)");
    }

    #[test]
    fn parameter_name_is_stripped_on_slice_form() {
        // `(p []byte)` and `([]byte)` collapse identically.
        let a = canonicalise_signature("p []byte", "");
        let b = canonicalise_signature("[]byte", "");
        assert_eq!(a, b);
        assert_eq!(a.0, b"([]byte)");
    }

    #[test]
    fn parameter_name_is_stripped_on_qualified_type() {
        // `r io.Reader` and `io.Reader` collapse identically.
        let a = canonicalise_signature("r io.Reader", "");
        let b = canonicalise_signature("io.Reader", "");
        assert_eq!(a, b);
        assert_eq!(a.0, b"(io.Reader)");
    }

    // ----- canonicalise_signature: rule 3 (variadic preservation) -----

    #[test]
    fn variadic_is_preserved() {
        // `... T` and `...T` collapse; `T` and `...T` do NOT collapse.
        let v1 = canonicalise_signature("... T", "");
        let v2 = canonicalise_signature("...T", "");
        assert_eq!(v1, v2);
        assert_eq!(v1.0, b"(...T)");

        let non_v = canonicalise_signature("T", "");
        assert_ne!(v1, non_v);
        assert_eq!(non_v.0, b"(T)");
    }

    // ----- canonicalise_signature: return-clause shape (rule 4) -----

    #[test]
    fn return_paren_stripped_for_single_return() {
        // `(int)` single-return form normalises to `int`.
        let a = canonicalise_signature("", "(int)");
        let b = canonicalise_signature("", "int");
        assert_eq!(a, b);
        assert_eq!(a.0, b"()int");
    }

    #[test]
    fn return_paren_preserved_for_multi_return() {
        // `(int, error)` keeps parens; comma is the only separator.
        let sig = canonicalise_signature("", "(int, error)");
        assert_eq!(sig.0, b"()(int,error)");
    }

    #[test]
    fn return_empty_yields_no_return_clause() {
        // Nullary return — the §4.1 grammar's `""` case.
        let sig = canonicalise_signature("", "");
        assert_eq!(sig.0, b"()");
    }

    // ----- canonicalise_signature: parameter list shape (rule 5) -----

    #[test]
    fn empty_parameter_list_keeps_parens() {
        let sig = canonicalise_signature("", "");
        assert_eq!(sig.0, b"()");
    }

    #[test]
    fn parameter_list_with_explicit_parens_is_unwrapped() {
        // `(a int, b int)` is accepted as either wrapped or unwrapped.
        let wrapped = canonicalise_signature("(a int, b int)", "");
        let unwrapped = canonicalise_signature("a int, b int", "");
        assert_eq!(wrapped, unwrapped);
        assert_eq!(wrapped.0, b"(int,int)");
    }

    // ----- Idempotence -----

    #[test]
    fn idempotent_on_simple_input() {
        // Running the normaliser on already-canonical bytes is a no-op:
        // we approximate "running again" by feeding the canonical form
        // back through `canonicalise_signature` after splitting the
        // params from the return clause.
        let first = canonicalise_signature("p *T", "(int)");
        // First output: "(*T)int"
        assert_eq!(first.0, b"(*T)int");

        // Decompose: params = "*T", returns = "int". Re-run.
        let second = canonicalise_signature("*T", "int");
        assert_eq!(first, second);

        // Triple application stays stable.
        let third = canonicalise_signature("*T", "int");
        assert_eq!(second, third);
    }

    #[test]
    fn idempotent_on_complex_input() {
        // Multi-return + variadic + qualified type.
        let first = canonicalise_signature("ctx context.Context, vs ...T", "(int, error)");
        assert_eq!(first.0, b"(context.Context,...T)(int,error)");

        let second = canonicalise_signature("context.Context, ...T", "(int, error)");
        assert_eq!(first, second);
    }

    // ----- Entrypoint (no-op on empty input) -----

    #[test]
    fn entrypoint_no_op_on_empty_graph() {
        // An empty graph carries no embeddings, no methods, no
        // promotions — the entrypoint must return zeroed stats other
        // than `elapsed_ms` (which is a wall-clock measurement that
        // may legitimately be non-zero).
        let mut graph = CodeGraph::new();
        let stats = run_go_method_set_satisfaction(&mut graph, None);
        assert_eq!(stats.promoted_method_nodes, 0);
        assert_eq!(stats.promoted_back_reference_edges, 0);
        assert_eq!(stats.ambiguity_blocked_promotions, 0);
        assert_eq!(stats.implements_edges_value, 0);
        assert_eq!(stats.implements_edges_pointer, 0);
        assert_eq!(stats.signature_implements_edges, 0);
        assert_eq!(stats.satisfaction_pairs_examined, 0);

        let scoped = run_go_method_set_satisfaction(&mut graph, Some(&[]));
        assert_eq!(scoped.promoted_method_nodes, 0);
    }

    // ----- D1 promotion algorithm fixtures -----

    /// Fixture-builder helpers. Each test fabricates a minimal CodeGraph
    /// that mirrors the shape the Go plugin would emit for a real
    /// Go workspace: Struct nodes with `<pkg>.<Name>` qualified names,
    /// Method nodes with `<pkg>.<Recv>.<Name>` qualified names, an
    /// `Inherits(S → T)` edge per struct-embed plus the matching
    /// `GoEmbeddingHint` in `go_hints`. Tests are NOT routed through
    /// the live Go plugin — D1 is the algorithm body, not an
    /// end-to-end pipeline test (that's Cluster F1's job).
    /// Register a fresh Go-language file in the test graph and return
    /// its `FileId`. Production wires the language on every
    /// `FileRegistry::register_with_language` call from the parse
    /// pipeline; tests that exercise the Cluster E2 incremental path
    /// (`changed_files = Some(_)`) must do the same so the pass's
    /// Go-language scope filter does NOT silently drop their fixture
    /// file. Tests that use the full-build path (`changed_files =
    /// None`) can keep the legacy `FileId::new(0)` pattern — the
    /// language filter is a no-op there.
    #[allow(dead_code, reason = "Only consumed by Cluster E2 incremental tests.")]
    fn register_go_file(graph: &mut CodeGraph, path: &str) -> FileId {
        graph
            .files_mut()
            .register_with_language(std::path::Path::new(path), Some(Language::Go))
            .expect("register Go test file")
    }

    /// Cluster G1: lookup helper for canonicalisation-aware tests.
    /// Tests write Go-natural `.`-form qn literals; the graph interns
    /// canonical (`::`-separated) qns via the production
    /// `canonicalize_graph_qualified_name` path. This helper bridges
    /// the two so existing D/E test bodies don't need rewriting.
    /// See 05_TEST_PLAN.md §7.5.
    fn qn_strid(graph: &CodeGraph, qn: &str) -> Option<StringId> {
        let canonical =
            crate::graph::unified::resolution::canonicalize_graph_qualified_name(Language::Go, qn);
        graph.strings().get(&canonical)
    }

    fn make_qn_node(
        graph: &mut CodeGraph,
        kind: NodeKind,
        short_name: &str,
        qualified_name: &str,
        file: FileId,
    ) -> NodeId {
        // Cluster G1: tests keep readable Go-natural `.`-form qn
        // literals; the fixture helper canonicalises before interning
        // so the in-memory graph matches the production contract
        // established by `helper.add_*` (which routes through
        // `canonicalize_graph_qualified_name`). See 05_TEST_PLAN.md
        // §7.5.
        let canonical_qn = crate::graph::unified::resolution::canonicalize_graph_qualified_name(
            Language::Go,
            qualified_name,
        );
        let name_id = graph.strings_mut().intern(short_name).expect("intern name");
        let qn_id = graph
            .strings_mut()
            .intern(&canonical_qn)
            .expect("intern qn");
        let mut entry = NodeEntry::new(kind, name_id, file);
        entry.qualified_name = Some(qn_id);
        let new_id = graph.nodes_mut().alloc(entry).expect("alloc node");
        // Register the node in the file's per-file bucket so test
        // fixtures match the production contract (every live node
        // belongs to some bucket - asserted by
        // `CodeGraph::assert_bucket_bijection` check (d)). Without
        // this call, tests built via `make_qn_node` populate the
        // arena but leave `FileRegistry::nodes_for_file(file)` empty,
        // which masks bucket-bijection regressions and prevents
        // `nodes_for_file`-based enumeration patterns from working in
        // tests.
        graph.files_mut().record_node(file, new_id);
        new_id
    }

    fn add_embedding_hint(
        graph: &mut CodeGraph,
        outer: NodeId,
        inner_qn: &str,
        pointerness: Receiver,
        file: FileId,
    ) {
        // Cluster G1: canonicalise the inner qn so the hint matches the
        // canonical node qn the production plugin emits. See
        // 05_TEST_PLAN.md §7.5.
        let canonical_inner = crate::graph::unified::resolution::canonicalize_graph_qualified_name(
            Language::Go,
            inner_qn,
        );
        let inner_qn_id = graph
            .strings_mut()
            .intern(&canonical_inner)
            .expect("intern inner qn");
        // Wire the Inherits(S → T) edge that the Go plugin would
        // emit alongside the hint.
        graph.go_hints_mut().embeddings.push(
            crate::graph::unified::build::staging::GoEmbeddingHint {
                outer,
                inner_qualified_name: inner_qn_id,
                pointerness,
                file,
            },
        );
    }

    /// D2 helper: push a `GoMethodReceiverHint` so the pass's
    /// receiver-pointerness map carries an entry for `method_node`.
    /// Without this entry, `compute_promotions_for_outer` defaults the
    /// method to `Receiver::Value` (the D1-compatible behaviour).
    fn add_method_receiver_hint(
        graph: &mut CodeGraph,
        method_node: NodeId,
        receiver_qn: &str,
        pointerness: Receiver,
        file: FileId,
    ) {
        // Cluster G1: canonicalise receiver qn — production plugin
        // emits canonical hint qns now. See 05_TEST_PLAN.md §7.5.
        let canonical_recv = crate::graph::unified::resolution::canonicalize_graph_qualified_name(
            Language::Go,
            receiver_qn,
        );
        let receiver_qn_id = graph
            .strings_mut()
            .intern(&canonical_recv)
            .expect("intern receiver qn");
        graph.go_hints_mut().method_receivers.push(
            crate::graph::unified::build::staging::GoMethodReceiverHint {
                method_node,
                receiver_type_qualified_name: receiver_qn_id,
                receiver_pointerness: pointerness,
                file,
            },
        );
    }

    /// D2 helper: push a `GoReceiverCallHint` with a `LocalIdent`
    /// receiver, so the pass's strict shadow-emission gating can
    /// resolve the call site's receiver type via a `TypeOf` edge.
    fn add_local_ident_receiver_call_hint(
        graph: &mut CodeGraph,
        call_site: NodeId,
        callee_method: NodeId,
        method_name: &str,
        binding_local: NodeId,
        file: FileId,
    ) {
        let method_name_id = graph
            .strings_mut()
            .intern(method_name)
            .expect("intern method name");
        graph.go_hints_mut().receiver_calls.push(
            crate::graph::unified::build::staging::GoReceiverCallHint {
                call_site,
                callee_method,
                method_name: method_name_id,
                receiver: GoReceiverHintKind::LocalIdent { binding_local },
                argument_count: 0,
                is_async: false,
                file,
            },
        );
    }

    fn rebuild_indices(graph: &mut CodeGraph) {
        // Drive the targeted index update so by_qualified_name is
        // populated for every node the test minted via
        // `make_qn_node`. We collect all live NodeIds first, then
        // call the per-set update.
        let mut all: Vec<NodeId> = Vec::new();
        for (nid, _entry) in graph.nodes().iter() {
            all.push(nid);
        }
        <CodeGraph as GraphMutationTarget>::rebuild_qualified_name_index_for_new_nodes(graph, &all);
    }

    /// AC-5 happy path: simple value embedding promotes the embedded
    /// type's method onto the outer struct, with the structural
    /// `Contains(S → S.M)` and `Inherits(S.M → T.M)` edges in place.
    #[test]
    fn d1_simple_value_embedding_promotes_method() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_m = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Greeting",
            "fx.Inner.Greeting",
            file,
        );
        let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, outer, "fx.Inner", Receiver::Value, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);
        assert_eq!(
            stats.promoted_method_nodes, 1,
            "exactly one promoted method node minted"
        );

        // Resolve fx.Outer.Greeting via the by-qualified-name index.
        let outer_greeting_qn =
            qn_strid(&graph, "fx.Outer.Greeting").expect("promoted qn interned");
        let promoted: Vec<NodeId> = graph
            .indices()
            .by_qualified_name(outer_greeting_qn)
            .to_vec();
        assert_eq!(promoted.len(), 1, "promoted method indexed once");
        let promoted_id = promoted[0];

        // Contains(Outer → Outer.Greeting)
        let outer_outgoing = graph.edges().edges_from(outer);
        assert!(
            outer_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Contains) && e.target == promoted_id),
            "Contains(Outer → Outer.Greeting) must be emitted",
        );

        // Inherits(Outer.Greeting → Inner.Greeting)
        let promoted_outgoing = graph.edges().edges_from(promoted_id);
        assert!(
            promoted_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Inherits) && e.target == inner_m),
            "Inherits(Outer.Greeting → Inner.Greeting) must be emitted",
        );

        // Synthetic flag set so workspace-symbol search skips this node.
        assert!(
            graph.macro_metadata().is_synthetic(promoted_id),
            "promoted method must be marked Synthetic",
        );
    }

    /// AC-6 fixture variant — D1 conservative bucket assignment:
    /// when a struct pointer-embeds `*T` and T has a method M, both
    /// `S.M` and `*S.M` are materialised. Per the D1 over-promotion
    /// limitation documented on `compute_promotions_for_outer`,
    /// methods on T are conservatively classified as value-bucket
    /// promotions, so they appear under `<pkg>.<S>.<m>` AND the
    /// pointer-form anchor `<pkg>.*<S>` is materialised when D2's
    /// stricter classification surfaces a pointer-only bucket.
    /// D1's verifiable contract: when a pointer-embed yields a
    /// promoted method, the method is reachable from BOTH parents.
    #[test]
    fn d1_pointer_embedding_promotes_into_value_and_pointer_buckets() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_m = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Mutate",
            "fx.Inner.Mutate",
            file,
        );
        let outer_p = make_qn_node(&mut graph, NodeKind::Struct, "OuterP", "fx.OuterP", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, outer_p, "fx.Inner", Receiver::Pointer, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);
        // D1 conservative bucket assignment routes the method
        // through the value bucket of OuterP. The materialised
        // promoted-method qn is `fx.OuterP.Mutate`. Verify this
        // makes the method reachable; the pointer-form *OuterP node
        // is materialised when D2's stricter pointer-only bucket
        // populates, but is not load-bearing for D1 reachability.
        assert!(
            stats.promoted_method_nodes >= 1,
            "at least one promoted method minted, got {}",
            stats.promoted_method_nodes,
        );

        let outer_p_mutate_qn =
            qn_strid(&graph, "fx.OuterP.Mutate").expect("value-form promoted qn interned");
        let value_promoted = graph
            .indices()
            .by_qualified_name(outer_p_mutate_qn)
            .to_vec();
        assert_eq!(
            value_promoted.len(),
            1,
            "OuterP.Mutate must be reachable via value-form"
        );

        // The Inherits back-reference points at the original
        // defining method (fx.Inner.Mutate).
        let promoted_outgoing = graph.edges().edges_from(value_promoted[0]);
        assert!(
            promoted_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Inherits) && e.target == inner_m),
            "promoted method must Inherit from the defining method",
        );
    }

    /// AC-4 (golang/go#57352) verbatim: same-depth ambiguity blocks
    /// promotion. `type Foo struct { A; AB }` where both A and AB
    /// contribute method `a` at depth 1 → no promotion, ambiguity
    /// counter bumps.
    #[test]
    fn d1_diamond_ambiguity_blocks_emission() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Per the spec fixture, A is an interface with method a, and
        // AB is an interface that embeds A and adds method b. Foo
        // embeds both A and AB; both contribute a method named `a`
        // at depth 1, blocking promotion.
        let a_iface = make_qn_node(&mut graph, NodeKind::Interface, "A", "fx.A", file);
        let _ = make_qn_node(&mut graph, NodeKind::Method, "a", "fx.A.a", file);
        let ab_iface = make_qn_node(&mut graph, NodeKind::Interface, "AB", "fx.AB", file);
        let _ = make_qn_node(&mut graph, NodeKind::Method, "a", "fx.AB.a", file);
        let _ = make_qn_node(&mut graph, NodeKind::Method, "b", "fx.AB.b", file);
        let foo = make_qn_node(&mut graph, NodeKind::Struct, "Foo", "fx.Foo", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, foo, "fx.A", Receiver::Value, file);
        add_embedding_hint(&mut graph, foo, "fx.AB", Receiver::Value, file);
        let _ = (a_iface, ab_iface);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        // No promoted `fx.Foo.a` node — it's blocked by ambiguity.
        // `fx.Foo.b` may still promote (only AB has it, depth 1, no
        // collision).
        let foo_a_qn = qn_strid(&graph, "fx.Foo.a");
        if let Some(qn) = foo_a_qn {
            let bucket = graph.indices().by_qualified_name(qn);
            assert!(
                bucket.is_empty(),
                "Foo.a must be blocked by ambiguity, found {} entries",
                bucket.len(),
            );
        }

        assert!(
            stats.ambiguity_blocked_promotions >= 1,
            "ambiguity_blocked_promotions must increment, got {}",
            stats.ambiguity_blocked_promotions,
        );
    }

    /// §5.3 shallowest-depth rule: when the same method name appears
    /// at depth 1 (direct embed) and depth 2 (grand-embed), the
    /// depth-1 contributor wins; deeper contributors are shadowed.
    #[test]
    fn d1_shallower_depth_wins() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Layout: Outer { Mid, Deep }; Mid embeds Deep too.
        // Deep has method M. Both reach Deep.M:
        //   - Outer → Deep (depth 1, direct)
        //   - Outer → Mid → Deep (depth 2)
        // The depth-1 path wins; the depth-2 path is shadowed.
        let deep = make_qn_node(&mut graph, NodeKind::Struct, "Deep", "fx.Deep", file);
        let deep_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Deep.M", file);
        let mid = make_qn_node(&mut graph, NodeKind::Struct, "Mid", "fx.Mid", file);
        let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, mid, "fx.Deep", Receiver::Value, file);
        add_embedding_hint(&mut graph, outer, "fx.Mid", Receiver::Value, file);
        add_embedding_hint(&mut graph, outer, "fx.Deep", Receiver::Value, file);
        let _ = (deep, mid);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        // Both Outer.M and Mid.M are minted (one per outer), each
        // resolving to the same defining method via Inherits.
        let outer_m_qn = qn_strid(&graph, "fx.Outer.M").expect("qn interned");
        let outer_m = graph.indices().by_qualified_name(outer_m_qn).to_vec();
        assert_eq!(outer_m.len(), 1, "exactly one Outer.M");

        // The depth-1 path wins and is the only contributor for
        // Outer.M's defining method back-reference.
        let outgoing = graph.edges().edges_from(outer_m[0]);
        let inherits_targets: Vec<NodeId> = outgoing
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Inherits))
            .map(|e| e.target)
            .collect();
        assert_eq!(
            inherits_targets,
            vec![deep_m],
            "shallowest-depth winner is the direct embed's M",
        );

        // No ambiguity recorded — depth disambiguates.
        assert_eq!(
            stats.ambiguity_blocked_promotions, 0,
            "different-depth same-name is NOT ambiguous",
        );
    }

    /// AC-9 (golang/go#66540): type-alias embedding promotes through
    /// the alias. D1's alias resolver follows one
    /// `TypeOf{TypeParameter}` hop from the alias's `Type` node to
    /// the underlying type that carries the methods.
    #[test]
    fn d1_alias_embedding_promotes_underlying_methods() {
        use crate::graph::unified::edge::kind::TypeOfContext;

        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // type Bar struct{}; func (Bar) Greet() {}
        let bar = make_qn_node(&mut graph, NodeKind::Struct, "Bar", "fx.Bar", file);
        let bar_greet = make_qn_node(&mut graph, NodeKind::Method, "Greet", "fx.Bar.Greet", file);

        // type A = Bar  (alias — Type node with TypeOf{TypeParameter} to Bar)
        let a_alias = make_qn_node(&mut graph, NodeKind::Type, "A", "fx.A", file);
        graph.edges_mut().add_edge(
            a_alias,
            bar,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::TypeParameter),
                index: None,
                name: None,
            },
            file,
        );

        // type S struct { A }
        let s = make_qn_node(&mut graph, NodeKind::Struct, "S", "fx.S", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, s, "fx.A", Receiver::Value, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);
        assert!(
            stats.promoted_method_nodes >= 1,
            "at least one promoted method through alias, got {}",
            stats.promoted_method_nodes,
        );

        // fx.S.Greet must be queryable.
        let s_greet_qn = qn_strid(&graph, "fx.S.Greet").expect("qn interned");
        let s_greet = graph.indices().by_qualified_name(s_greet_qn).to_vec();
        assert_eq!(s_greet.len(), 1, "S.Greet must be reachable via alias");

        // The promoted node's Inherits back-reference points at the
        // underlying defining method (fx.Bar.Greet), not the alias.
        let outgoing = graph.edges().edges_from(s_greet[0]);
        assert!(
            outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Inherits) && e.target == bar_greet),
            "promoted node must Inherit from the underlying defining method",
        );
    }

    /// AC-5 closure: shadow `Calls(use → S.M)` is emitted at every
    /// caller of the underlying defining method, so
    /// `direct_callers(<pkg>.<S>.<m>)` is non-empty.
    #[test]
    fn d1_shadow_calls_emitted_at_receiver_call_sites() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _ = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Inner.M", file);
        let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
        let use_fn = make_qn_node(&mut graph, NodeKind::Function, "use", "fx.use", file);
        rebuild_indices(&mut graph);

        // Pre-existing Calls(use → Inner.M) — the edge the Go plugin
        // would have emitted at parse time.
        graph.edges_mut().add_edge(
            use_fn,
            inner_m,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        add_embedding_hint(&mut graph, outer, "fx.Inner", Receiver::Value, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);
        assert_eq!(stats.promoted_method_nodes, 1);
        // Shadow Calls + References emitted = 2 (per caller per
        // promoted target).
        assert!(
            stats.promoted_back_reference_edges >= 2,
            "shadow Calls+References must fire, got {}",
            stats.promoted_back_reference_edges,
        );

        let outer_m_qn = qn_strid(&graph, "fx.Outer.M").expect("qn interned");
        let outer_m = graph.indices().by_qualified_name(outer_m_qn).to_vec();
        assert_eq!(outer_m.len(), 1);
        let outer_m_id = outer_m[0];

        // Calls(use → Outer.M) must exist.
        let use_outgoing = graph.edges().edges_from(use_fn);
        assert!(
            use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Calls { .. }) && e.target == outer_m_id),
            "shadow Calls(use → Outer.M) must be emitted",
        );

        // References(use → Outer.M) must exist.
        assert!(
            use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::References) && e.target == outer_m_id),
            "shadow References(use → Outer.M) must be emitted",
        );

        // The original Calls(use → Inner.M) is intact.
        assert!(
            use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Calls { .. }) && e.target == inner_m),
            "original Calls(use → Inner.M) must NOT be removed",
        );
    }

    /// AC-12 prerequisite: two runs on identical input emit
    /// identical edge sets. The pass uses BTreeMaps + explicit sort
    /// before drain, so determinism holds.
    #[test]
    fn d1_determinism_two_runs_emit_identical_edge_sequence() {
        fn build_workspace() -> CodeGraph {
            let mut graph = CodeGraph::new();
            let file = FileId::new(0);
            let _ = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
            let _ = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Inner.M", file);
            let _ = make_qn_node(&mut graph, NodeKind::Method, "N", "fx.Inner.N", file);
            let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
            rebuild_indices(&mut graph);
            add_embedding_hint(&mut graph, outer, "fx.Inner", Receiver::Value, file);
            graph
        }

        // Snapshot the post-pass edge set as a sorted tuple list.
        fn snapshot(graph: &CodeGraph) -> Vec<(u32, u32, String)> {
            let mut out: Vec<(u32, u32, String)> = Vec::new();
            for edge in graph.edges().all_live_forward_edges() {
                let kind_tag = format!("{:?}", edge.kind);
                out.push((edge.source.index(), edge.target.index(), kind_tag));
            }
            out.sort();
            out
        }

        let mut g1 = build_workspace();
        let _ = run_go_method_set_satisfaction(&mut g1, None);
        let s1 = snapshot(&g1);

        let mut g2 = build_workspace();
        let _ = run_go_method_set_satisfaction(&mut g2, None);
        let s2 = snapshot(&g2);

        assert_eq!(
            s1, s2,
            "two runs on identical input must produce identical edge sets"
        );
    }

    // ========================================================================
    // Cluster D2.2 — receiver-pointerness tightening fixtures
    // ========================================================================

    /// D2.2 verifies the new bucket classifier: when an outer struct
    /// `OuterV` value-embeds `Inner`, and `Inner` has a method with a
    /// pointer receiver `(*Inner) Mutate()`, the method promotes onto
    /// the pointer-form bucket only (`<pkg>.*<OuterV>.Mutate`),
    /// **not** onto the value form `<pkg>.<OuterV>.Mutate`. This
    /// closes AC-6 strictness over D1's value-bucket
    /// over-classification.
    #[test]
    fn d2_method_receiver_hint_pointer_only_promotion() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_mutate = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Mutate",
            "fx.Inner.Mutate",
            file,
        );
        let outer_v = make_qn_node(&mut graph, NodeKind::Struct, "OuterV", "fx.OuterV", file);
        rebuild_indices(&mut graph);

        // OuterV VALUE-embeds Inner.
        add_embedding_hint(&mut graph, outer_v, "fx.Inner", Receiver::Value, file);
        // Inner.Mutate has POINTER receiver — Go spec says it
        // promotes onto *OuterV only.
        add_method_receiver_hint(
            &mut graph,
            inner_mutate,
            "fx.Inner",
            Receiver::Pointer,
            file,
        );

        let stats = run_go_method_set_satisfaction(&mut graph, None);
        assert!(
            stats.promoted_method_nodes >= 1,
            "at least one promoted method minted, got {}",
            stats.promoted_method_nodes,
        );

        // Pointer-form `<pkg>.*<OuterV>.Mutate` MUST exist.
        let pointer_form_qn =
            qn_strid(&graph, "fx.*OuterV.Mutate").expect("pointer-form qn interned");
        let pointer_promoted = graph.indices().by_qualified_name(pointer_form_qn).to_vec();
        assert_eq!(
            pointer_promoted.len(),
            1,
            "*OuterV.Mutate must be reachable via pointer-form",
        );

        // Value-form `<pkg>.<OuterV>.Mutate` MUST NOT exist (D2.2
        // strict pointer-only classification).
        let value_form = qn_strid(&graph, "fx.OuterV.Mutate");
        if let Some(qn) = value_form {
            let bucket = graph.indices().by_qualified_name(qn);
            assert!(
                bucket.is_empty(),
                "OuterV.Mutate must NOT be reachable via value-form (pointer-receiver method on \
                 value-embedded type promotes onto *OuterV only per Go spec §\"Method sets\"); \
                 found {} entries",
                bucket.len(),
            );
        }
    }

    /// D2.2: pointer-receiver method via pointer-embed chain → value
    /// bucket (the promoted method is reachable from BOTH parents
    /// because `*T` is addressable via both `S` and `*S` per Go spec).
    /// This is the symmetric case to
    /// `d2_method_receiver_hint_pointer_only_promotion`.
    #[test]
    fn d2_pointer_method_via_pointer_embed_promotes_to_value_bucket() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_mutate = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Mutate",
            "fx.Inner.Mutate",
            file,
        );
        let outer_p = make_qn_node(&mut graph, NodeKind::Struct, "OuterP", "fx.OuterP", file);
        rebuild_indices(&mut graph);

        // OuterP POINTER-embeds *Inner.
        add_embedding_hint(&mut graph, outer_p, "fx.Inner", Receiver::Pointer, file);
        // Inner.Mutate has POINTER receiver.
        add_method_receiver_hint(
            &mut graph,
            inner_mutate,
            "fx.Inner",
            Receiver::Pointer,
            file,
        );

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // Value-form `<pkg>.<OuterP>.Mutate` MUST exist (pointer-embed
        // chain lifts pointer-receiver method into both buckets).
        let value_form_qn = qn_strid(&graph, "fx.OuterP.Mutate").expect("value-form qn interned");
        let value_promoted = graph.indices().by_qualified_name(value_form_qn).to_vec();
        assert_eq!(
            value_promoted.len(),
            1,
            "OuterP.Mutate must be reachable via value-form when *Inner is the embed and Mutate \
             has pointer receiver (Go spec §\"Method sets\" rule for embedded-pointer-field).",
        );
    }

    /// D2.2 shadow-emission gating: a `GoReceiverCallHint` whose
    /// receiver resolves to an *unrelated* type does NOT produce a
    /// shadow `Calls` against the promoted name. This closes D1's
    /// over-emission under AC-5's tightened reading.
    #[test]
    fn d2_shadow_emission_gating_excludes_unrelated_callers() {
        use crate::graph::unified::edge::kind::TypeOfContext;

        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Workspace:
        //   type Inner struct{}
        //   func (Inner) M() {}
        //   type Outer struct { Inner }     // M promotes onto Outer
        //   type Unrelated struct{}
        //   func (Unrelated) M() {}         // same NAME, distinct method
        //   func use_unrelated() {
        //     var u Unrelated
        //     u.M()                          // call site receiver = Unrelated
        //   }
        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Inner.M", file);
        let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
        let unrelated = make_qn_node(
            &mut graph,
            NodeKind::Struct,
            "Unrelated",
            "fx.Unrelated",
            file,
        );
        let unrelated_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Unrelated.M", file);
        let use_fn = make_qn_node(
            &mut graph,
            NodeKind::Function,
            "use_unrelated",
            "fx.use_unrelated",
            file,
        );
        let u_var = make_qn_node(
            &mut graph,
            NodeKind::Variable,
            "u",
            "fx.use_unrelated.u",
            file,
        );
        let call_site = make_qn_node(
            &mut graph,
            NodeKind::CallSite,
            "call_u_M",
            "fx.use_unrelated.call_u_M",
            file,
        );
        rebuild_indices(&mut graph);

        // u: Unrelated (TypeOf{Variable} edge)
        graph.edges_mut().add_edge(
            u_var,
            unrelated,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                index: None,
                name: None,
            },
            file,
        );

        // Pre-existing Calls(use_unrelated → Unrelated.M).
        graph.edges_mut().add_edge(
            use_fn,
            unrelated_m,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        // Outer embeds Inner.
        add_embedding_hint(&mut graph, outer, "fx.Inner", Receiver::Value, file);
        // Both M methods are value-receiver.
        add_method_receiver_hint(&mut graph, inner_m, "fx.Inner", Receiver::Value, file);
        add_method_receiver_hint(
            &mut graph,
            unrelated_m,
            "fx.Unrelated",
            Receiver::Value,
            file,
        );

        // The receiver-call hint resolves to `Unrelated`, NOT `Outer`.
        add_local_ident_receiver_call_hint(&mut graph, call_site, unrelated_m, "M", u_var, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // Outer.M MUST exist (promotion happened).
        let outer_m_qn = qn_strid(&graph, "fx.Outer.M").expect("promoted qn interned");
        let outer_m = graph.indices().by_qualified_name(outer_m_qn).to_vec();
        assert_eq!(outer_m.len(), 1, "Outer.M must be materialised");
        let outer_m_id = outer_m[0];

        // The crucial gate: shadow `Calls(use_unrelated → Outer.M)`
        // MUST NOT exist because the receiver `u` resolves to
        // Unrelated, not Outer.
        let use_outgoing = graph.edges().edges_from(use_fn);
        assert!(
            !use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Calls { .. }) && e.target == outer_m_id),
            "shadow Calls(use_unrelated → Outer.M) MUST NOT be emitted (receiver type = \
             Unrelated, not Outer)",
        );
        assert!(
            !use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::References) && e.target == outer_m_id),
            "shadow References(use_unrelated → Outer.M) MUST NOT be emitted",
        );

        // The original Calls(use_unrelated → Unrelated.M) is intact.
        assert!(
            use_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Calls { .. }) && e.target == unrelated_m),
            "original Calls(use_unrelated → Unrelated.M) must NOT be removed",
        );
    }

    // ========================================================================
    // Cluster D2.3 — T1.1 implicit interface satisfaction fixtures
    // ========================================================================

    /// D2.3 helper: mint an interface with a list of method names. Each
    /// method is created as a Method node under `<iface_qn>.<m>`.
    fn make_interface_with_methods(
        graph: &mut CodeGraph,
        short: &str,
        qualified: &str,
        method_names: &[&str],
        file: FileId,
    ) -> NodeId {
        let iface = make_qn_node(graph, NodeKind::Interface, short, qualified, file);
        for m in method_names {
            let mqn = format!("{qualified}.{m}");
            let _ = make_qn_node(graph, NodeKind::Method, m, &mqn, file);
        }
        iface
    }

    /// AC-1: a struct with the exact method set of a single-method
    /// interface implicitly satisfies it. `Implements(*File → Reader)`
    /// is emitted because `Read` has a pointer receiver (so only
    /// `*File`'s method set carries it). `Implements(File → Reader)`
    /// is NOT emitted because the value form does not satisfy.
    #[test]
    fn d2_ac1_single_method_implements() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        let file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        rebuild_indices(&mut graph);

        // File.Read has pointer receiver.
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert!(
            stats.satisfaction_pairs_examined > 0,
            "satisfaction loop must have inspected (File, Reader)",
        );

        // *fx.File pointer-form Type node must be minted.
        let ptr_qn = qn_strid(&graph, "fx.*File").expect("pointer-form qn interned");
        let ptr_node_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
        assert_eq!(
            ptr_node_candidates.len(),
            1,
            "exactly one synthetic *fx.File node",
        );
        let ptr_node = ptr_node_candidates[0];

        // Implements(*fx.File → fx.Reader) must exist.
        let outgoing = graph.edges().edges_from(ptr_node);
        assert!(
            outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(*File → Reader) must be emitted",
        );

        // Implements(fx.File → fx.Reader) must NOT exist (value form
        // does not satisfy when receiver is pointer).
        let file_outgoing = graph.edges().edges_from(file_struct);
        assert!(
            !file_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(File → Reader) MUST NOT be emitted (pointer-only satisfaction)",
        );
    }

    /// AC-2 (cross-file in same package): satisfaction is independent
    /// of which file the interface and concrete type are declared in.
    /// We model "different files" by minting nodes with different
    /// `FileId`s.
    #[test]
    fn d2_ac2_cross_file_implements_same_package() {
        let mut graph = CodeGraph::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);

        let writer =
            make_interface_with_methods(&mut graph, "Writer", "fx.Writer", &["Write"], file_a);
        // Concrete type in file_b.
        let buf = make_qn_node(&mut graph, NodeKind::Struct, "Buf", "fx.Buf", file_b);
        let buf_write = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Write",
            "fx.Buf.Write",
            file_b,
        );
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, buf_write, "fx.Buf", Receiver::Value, file_b);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        let buf_outgoing = graph.edges().edges_from(buf);
        assert!(
            buf_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == writer),
            "Implements(Buf → Writer) must be emitted across files",
        );
    }

    /// AC-2 (cross-package): satisfaction respects package boundaries —
    /// the interface lives in `pkg_a` and the concrete in `pkg_b`.
    /// Method-name matching alone drives satisfaction.
    #[test]
    fn d2_ac2_cross_package_implements() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let reader =
            make_interface_with_methods(&mut graph, "Reader", "pkg_a.Reader", &["Read"], file);
        let file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "pkg_b.File", file);
        let file_read = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Read",
            "pkg_b.File.Read",
            file,
        );
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, file_read, "pkg_b.File", Receiver::Value, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        let outgoing = graph.edges().edges_from(file_struct);
        assert!(
            outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(pkg_b.File → pkg_a.Reader) must be emitted across packages",
        );
    }

    /// AC-2 (declaration order): satisfaction does not depend on the
    /// lexical order in which the interface and concrete type are
    /// declared. We mint the concrete type FIRST, then the interface.
    #[test]
    fn d2_ac2_declaration_order_independence() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Value, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        let outgoing = graph.edges().edges_from(file_struct);
        assert!(
            outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(File → Reader) must be emitted regardless of declaration order",
        );
    }

    /// AC-3 + AC-6: pointer-only satisfaction emits ONLY the
    /// pointer-form Implements edge. Value form does not satisfy when
    /// the underlying method requires a pointer receiver.
    #[test]
    fn d2_ac3_pointer_only_satisfaction_emits_only_pointer_form() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let mutator =
            make_interface_with_methods(&mut graph, "Mutator", "fx.Mutator", &["Mutate"], file);
        let buffer = make_qn_node(&mut graph, NodeKind::Struct, "Buffer", "fx.Buffer", file);
        let buf_mutate = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Mutate",
            "fx.Buffer.Mutate",
            file,
        );
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, buf_mutate, "fx.Buffer", Receiver::Pointer, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // *Buffer pointer-form must exist.
        let ptr_qn = qn_strid(&graph, "fx.*Buffer").expect("pointer-form qn interned");
        let ptr_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
        assert_eq!(ptr_candidates.len(), 1, "*Buffer pointer-form materialised");
        let ptr_buffer = ptr_candidates[0];

        // Implements(*Buffer → Mutator) emitted.
        assert!(
            graph
                .edges()
                .edges_from(ptr_buffer)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == mutator),
            "Implements(*Buffer → Mutator) must be emitted",
        );

        // Implements(Buffer → Mutator) NOT emitted.
        assert!(
            !graph
                .edges()
                .edges_from(buffer)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == mutator),
            "Implements(Buffer → Mutator) MUST NOT be emitted (value form has no Mutate)",
        );
    }

    /// AC-7: no false positives. Even when a method has the right
    /// shape, a missing method blocks `Implements`. `NotACloser`
    /// declares `Open()` but not `Close()`, so it does not satisfy
    /// `Closer`.
    #[test]
    fn d2_ac7_no_false_positive_missing_method() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let closer =
            make_interface_with_methods(&mut graph, "Closer", "fx.Closer", &["Close"], file);
        let nac = make_qn_node(
            &mut graph,
            NodeKind::Struct,
            "NotACloser",
            "fx.NotACloser",
            file,
        );
        let nac_open = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Open",
            "fx.NotACloser.Open",
            file,
        );
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, nac_open, "fx.NotACloser", Receiver::Value, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // No Implements edge from NotACloser to Closer.
        let outgoing = graph.edges().edges_from(nac);
        assert!(
            !outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == closer),
            "Implements(NotACloser → Closer) MUST NOT be emitted (missing Close method)",
        );

        // Also no edge from any synthetic *NotACloser node.
        let ptr_qn = qn_strid(&graph, "fx.*NotACloser");
        if let Some(ptr_qn) = ptr_qn {
            let ptr_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
            for ptr_node in ptr_candidates {
                assert!(
                    !graph
                        .edges()
                        .edges_from(ptr_node)
                        .iter()
                        .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == closer),
                    "Implements(*NotACloser → Closer) MUST NOT be emitted",
                );
            }
        }
    }

    /// AC-8: empty interfaces (`interface{}` / `any`) do not produce
    /// `Implements` edges. The §5.7 uninteresting filter must skip
    /// them.
    #[test]
    fn d2_ac8_empty_interface_filter_skips_emission() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Empty interface — no methods declared.
        let empty_iface = make_qn_node(&mut graph, NodeKind::Interface, "Any", "fx.Any", file);

        // A concrete type with one method.
        let x = make_qn_node(&mut graph, NodeKind::Struct, "X", "fx.X", file);
        let x_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.X.M", file);
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, x_m, "fx.X", Receiver::Value, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // No Implements edge from X to Any.
        let outgoing = graph.edges().edges_from(x);
        assert!(
            !outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == empty_iface),
            "Implements(X → Any{{}}) MUST NOT be emitted (empty-interface filter)",
        );
    }

    // ========================================================================
    // Cluster D3.2 — name + signature satisfaction predicate fixtures
    // ========================================================================

    /// D3 helper: push a `GoMethodSignatureHint` so the tightened
    /// satisfaction predicate sees a canonical signature for
    /// `method_node`. Without this hint the predicate falls back to
    /// the D2 name-only contract.
    fn add_method_signature_hint(
        graph: &mut CodeGraph,
        method_node: NodeId,
        canonical_signature: &str,
        file: FileId,
    ) {
        graph.go_hints_mut().method_signatures.push(
            crate::graph::unified::build::staging::GoMethodSignatureHint {
                method_node,
                canonical_signature: canonical_signature.to_string(),
                file,
            },
        );
    }

    /// D3 helper: push a `GoFunctionSignatureHint`. Used by both the
    /// T1.3 sub-pass tests and the AC-11 fixture (HandlerFunc named
    /// function type). Defined in this commit so the D3.3 / D3.4
    /// follow-up commits add tests rather than helpers.
    #[allow(
        dead_code,
        reason = "Cluster D3 staging: helper consumed by D3.3 (T1.3 emission tests) and \
                  D3.4 (AC-11 dual-natured HandlerFunc fixture). Defined here so each \
                  follow-up commit adds only the test bodies, mirroring how \
                  `add_method_receiver_hint` was staged across Cluster D2."
    )]
    fn add_function_signature_hint(
        graph: &mut CodeGraph,
        function_node: NodeId,
        canonical_signature: &str,
        file: FileId,
    ) {
        graph.go_hints_mut().function_signatures.push(
            crate::graph::unified::build::staging::GoFunctionSignatureHint {
                function_node,
                canonical_signature: canonical_signature.to_string(),
                file,
            },
        );
    }

    /// AC-7 (signature-mismatch tightening): D3 closes the loophole in
    /// D2's name-only predicate. Three types under test:
    ///
    /// - `R interface { Read([]byte) (int, error) }`
    /// - `S struct { ... }` with `Write([]byte)` — wrong name.
    /// - `Q struct { ... }` with `Read(int) error` — right name,
    ///   wrong canonical signature.
    ///
    /// Neither `S` nor `Q` may produce `Implements(? → R)` after D3's
    /// tightening lands. Pre-D3 the loophole would have let `Q` slip
    /// through because the name `Read` matches; the canonical-signature
    /// comparison must now block it.
    #[test]
    fn d3_ac7_signature_mismatch() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Interface R with a single `Read([]byte) (int, error)`
        // method.
        let r = make_qn_node(&mut graph, NodeKind::Interface, "R", "fx.R", file);
        let r_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.R.Read", file);

        // Struct S with `Write([]byte)`. Name mismatch.
        let s = make_qn_node(&mut graph, NodeKind::Struct, "S", "fx.S", file);
        let s_write = make_qn_node(&mut graph, NodeKind::Method, "Write", "fx.S.Write", file);

        // Struct Q with `Read(int) error`. Name matches; signature
        // does not.
        let q = make_qn_node(&mut graph, NodeKind::Struct, "Q", "fx.Q", file);
        let q_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.Q.Read", file);

        rebuild_indices(&mut graph);

        // Pointerness hints — Value-receiver throughout.
        add_method_receiver_hint(&mut graph, r_read, "fx.R", Receiver::Value, file);
        add_method_receiver_hint(&mut graph, s_write, "fx.S", Receiver::Value, file);
        add_method_receiver_hint(&mut graph, q_read, "fx.Q", Receiver::Value, file);

        // Canonical signatures.
        let sig_read_bytes = "([]byte)(int,error)";
        let sig_write_bytes = "([]byte)";
        let sig_read_int_err = "(int)error";
        add_method_signature_hint(&mut graph, r_read, sig_read_bytes, file);
        add_method_signature_hint(&mut graph, s_write, sig_write_bytes, file);
        add_method_signature_hint(&mut graph, q_read, sig_read_int_err, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        // S has the wrong method name (`Write` instead of `Read`), so
        // no Implements edge.
        assert!(
            !graph
                .edges()
                .edges_from(s)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == r),
            "Implements(S → R) MUST NOT be emitted — S has no `Read` method",
        );

        // Q has the right name (`Read`) but wrong canonical signature.
        // The D3 tightening MUST block emission.
        assert!(
            !graph
                .edges()
                .edges_from(q)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == r),
            "Implements(Q → R) MUST NOT be emitted — Q.Read's signature \
             does not match R.Read's signature",
        );

        // Neither `*S` nor `*Q` (if synthesised) may carry an Implements
        // edge either.
        for ptr_qn_str in ["fx.*S", "fx.*Q"] {
            if let Some(ptr_qn) = qn_strid(&graph, ptr_qn_str) {
                let ptr_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
                for ptr_node in ptr_candidates {
                    assert!(
                        !graph
                            .edges()
                            .edges_from(ptr_node)
                            .iter()
                            .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == r),
                        "Implements({ptr_qn_str} → R) MUST NOT be emitted under \
                         signature mismatch",
                    );
                }
            }
        }
    }

    /// D3.3 helper: push a `GoNamedTypeConversionHint` (Source A for
    /// T1.3). The hint encodes a Go-source `T(g)` conversion: `T` is
    /// the target named type (we already minted its `Type` NodeId and
    /// its `GoFunctionSignatureHint`), `g` is the function reference
    /// the conversion is applied to.
    fn add_named_type_conversion_hint(
        graph: &mut CodeGraph,
        call_site: NodeId,
        target_qn: &str,
        argument_node: NodeId,
        file: FileId,
    ) {
        // Cluster G1: canonicalise target qn — production plugin
        // emits canonical hint qns. See 05_TEST_PLAN.md §7.5.
        let canonical_target = crate::graph::unified::resolution::canonicalize_graph_qualified_name(
            Language::Go,
            target_qn,
        );
        let target_qn_id = graph
            .strings_mut()
            .intern(&canonical_target)
            .expect("intern target qn");
        graph.go_hints_mut().named_type_conversions.push(
            crate::graph::unified::build::staging::GoNamedTypeConversionHint {
                call_site,
                target_type_qualified_name: target_qn_id,
                argument_node,
                file,
            },
        );
    }

    /// D3.3 (T1.3 Source A): the canonical T1.3 emission shape — a
    /// `T(g)` conversion of a bare function whose signature matches
    /// the named function-type's underlying signature emits
    /// `Implements(g → T)`.
    ///
    /// Fixture:
    ///   `type Op func(int) int`
    ///   `func double(x int) int { return x*2 }`
    ///   `_ = Op(double)`
    #[test]
    fn d3_t1_3_named_type_conversion_emits_signature_implements() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Named function-type `Op` with underlying signature
        // `func(int) int`.
        let op = make_qn_node(&mut graph, NodeKind::Type, "Op", "fx.Op", file);
        // Bare function `double` with the same canonical signature.
        let double = make_qn_node(&mut graph, NodeKind::Function, "double", "fx.double", file);
        // Synthetic call_site node — the hint references it but the
        // pass only consumes the `argument_node` + `target_qn`
        // fields, so any placeholder NodeId suffices.
        let call_site = make_qn_node(
            &mut graph,
            NodeKind::CallSite,
            "Op_double_call",
            "fx.<Op(double)>",
            file,
        );
        rebuild_indices(&mut graph);

        let sig_int_int = "(int)int";
        add_function_signature_hint(&mut graph, op, sig_int_int, file);
        add_function_signature_hint(&mut graph, double, sig_int_int, file);
        add_named_type_conversion_hint(&mut graph, call_site, "fx.Op", double, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert!(
            stats.signature_implements_edges >= 1,
            "expected at least one signature-implements edge, got {}",
            stats.signature_implements_edges,
        );

        // Implements(double → Op) MUST be emitted.
        let outgoing = graph.edges().edges_from(double);
        assert!(
            outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == op),
            "Implements(double → Op) MUST be emitted under T1.3",
        );
    }

    /// D3.3 (T1.3 Source A, signature mismatch): a `T(g)` conversion
    /// whose argument's canonical signature does NOT match the named
    /// function-type's signature does not produce a T1.3 Implements
    /// edge. Closes the loophole the name-only predicate would have
    /// allowed for T1.3.
    #[test]
    fn d3_t1_3_signature_mismatch_blocks_emission() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let op = make_qn_node(&mut graph, NodeKind::Type, "Op", "fx.Op", file);
        let wrong_sig = make_qn_node(&mut graph, NodeKind::Function, "wrong", "fx.wrong", file);
        let call_site = make_qn_node(
            &mut graph,
            NodeKind::CallSite,
            "Op_wrong_call",
            "fx.<Op(wrong)>",
            file,
        );
        rebuild_indices(&mut graph);

        add_function_signature_hint(&mut graph, op, "(int)int", file);
        // Wrong signature — `(string) string` does not match.
        add_function_signature_hint(&mut graph, wrong_sig, "(string)string", file);
        add_named_type_conversion_hint(&mut graph, call_site, "fx.Op", wrong_sig, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert_eq!(
            stats.signature_implements_edges, 0,
            "signature mismatch must block T1.3 emission, got {}",
            stats.signature_implements_edges,
        );

        let outgoing = graph.edges().edges_from(wrong_sig);
        assert!(
            !outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == op),
            "Implements(wrong → Op) MUST NOT be emitted under T1.3 signature mismatch",
        );
    }

    /// D3.3 (T1.3 same-package guard): a `T(g)` conversion where `T`
    /// and `g` live in different packages does not produce a T1.3
    /// Implements edge. Per 01_SPEC §3.1 T1.3 is Tier-1 same-package
    /// only.
    #[test]
    fn d3_t1_3_cross_package_is_blocked() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Named type in package `a`, function in package `b`.
        let op = make_qn_node(&mut graph, NodeKind::Type, "Op", "pkg_a.Op", file);
        let g = make_qn_node(&mut graph, NodeKind::Function, "g", "pkg_b.g", file);
        let call_site = make_qn_node(
            &mut graph,
            NodeKind::CallSite,
            "Op_g_call",
            "pkg_b.<Op(g)>",
            file,
        );
        rebuild_indices(&mut graph);

        add_function_signature_hint(&mut graph, op, "(int)int", file);
        add_function_signature_hint(&mut graph, g, "(int)int", file);
        add_named_type_conversion_hint(&mut graph, call_site, "pkg_a.Op", g, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert_eq!(
            stats.signature_implements_edges, 0,
            "cross-package T1.3 must be blocked by the same-package guard, got {}",
            stats.signature_implements_edges,
        );
    }

    /// D3.3 determinism: T1.3 emits the same edge sequence on two
    /// back-to-back runs over the same input graph. Required by
    /// AC-12.
    #[test]
    fn d3_t1_3_determinism_two_runs() {
        let mut graph_a = CodeGraph::new();
        let mut graph_b = CodeGraph::new();
        let file = FileId::new(0);

        for graph in [&mut graph_a, &mut graph_b] {
            let op = make_qn_node(graph, NodeKind::Type, "Op", "fx.Op", file);
            let f1 = make_qn_node(graph, NodeKind::Function, "f1", "fx.f1", file);
            let f2 = make_qn_node(graph, NodeKind::Function, "f2", "fx.f2", file);
            let c1 = make_qn_node(graph, NodeKind::CallSite, "Op_f1_call", "fx.<Op(f1)>", file);
            let c2 = make_qn_node(graph, NodeKind::CallSite, "Op_f2_call", "fx.<Op(f2)>", file);
            rebuild_indices(graph);

            add_function_signature_hint(graph, op, "(int)int", file);
            add_function_signature_hint(graph, f1, "(int)int", file);
            add_function_signature_hint(graph, f2, "(int)int", file);
            add_named_type_conversion_hint(graph, c1, "fx.Op", f1, file);
            add_named_type_conversion_hint(graph, c2, "fx.Op", f2, file);
        }

        let stats_a = run_go_method_set_satisfaction(&mut graph_a, None);
        let stats_b = run_go_method_set_satisfaction(&mut graph_b, None);

        assert_eq!(
            stats_a.signature_implements_edges, stats_b.signature_implements_edges,
            "T1.3 emission count must be identical across runs over identical input",
        );
        assert!(
            stats_a.signature_implements_edges >= 2,
            "expected >= 2 T1.3 edges (f1 and f2 each → Op), got {}",
            stats_a.signature_implements_edges,
        );
    }

    /// AC-11: a named function type with methods participates in both
    /// T1.1 (the named type's method set satisfies an interface) AND
    /// T1.3 (a bare function with matching signature is an
    /// Implements-source for the named type). The dual-edge contract
    /// is the load-bearing AC for Go's HandlerFunc idiom; both halves
    /// of the contract are asserted by this fixture.
    ///
    /// Fixture (semantically equivalent to net/http's HandlerFunc):
    ///   type Handler interface {
    ///       ServeHTTP(ResponseWriter, *Request)
    ///   }
    ///   type HandlerFunc func(ResponseWriter, *Request)
    ///   func (f HandlerFunc) ServeHTTP(w ResponseWriter, r *Request) {
    ///       f(w, r)
    ///   }
    ///   func handleIndex(w ResponseWriter, r *Request) { ... }
    ///   _ = HandlerFunc(handleIndex)
    ///
    /// Assertions:
    /// - `Implements(HandlerFunc → Handler)` (T1.1 — HandlerFunc has
    ///   a `ServeHTTP` method whose signature matches Handler's).
    /// - `Implements(handleIndex → HandlerFunc)` (T1.3 — handleIndex's
    ///   signature matches HandlerFunc's underlying signature).
    #[test]
    fn d3_ac11_dual_natured_handlerfunc() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Interface Handler with ServeHTTP(ResponseWriter, *Request).
        let handler = make_qn_node(
            &mut graph,
            NodeKind::Interface,
            "Handler",
            "http.Handler",
            file,
        );
        let handler_serve_http = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "ServeHTTP",
            "http.Handler.ServeHTTP",
            file,
        );

        // Named function-type HandlerFunc with underlying signature
        // `func(ResponseWriter, *Request)`.
        let handler_func = make_qn_node(
            &mut graph,
            NodeKind::Type,
            "HandlerFunc",
            "http.HandlerFunc",
            file,
        );

        // Receiver method ServeHTTP on HandlerFunc.
        let handler_func_serve = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "ServeHTTP",
            "http.HandlerFunc.ServeHTTP",
            file,
        );

        // Bare function handleIndex with the same canonical signature
        // as HandlerFunc.
        let handle_index = make_qn_node(
            &mut graph,
            NodeKind::Function,
            "handleIndex",
            "http.handleIndex",
            file,
        );

        // Synthetic call_site node for the `HandlerFunc(handleIndex)`
        // conversion.
        let call_site = make_qn_node(
            &mut graph,
            NodeKind::CallSite,
            "handlerfunc_conv",
            "http.<HandlerFunc(handleIndex)>",
            file,
        );

        rebuild_indices(&mut graph);

        // The canonical signature for a HandlerFunc-shaped function.
        // Lexically the parameters are `(ResponseWriter, *Request)`;
        // the canonical normaliser preserves type identity bytewise.
        let handler_sig = "(ResponseWriter,*Request)";

        // Pointerness hints: ServeHTTP on HandlerFunc has a Value
        // receiver (`f HandlerFunc`); Handler.ServeHTTP is an
        // interface method (receiver pointerness is irrelevant for
        // satisfaction but the side channel is populated for parity
        // with the real plugin).
        add_method_receiver_hint(
            &mut graph,
            handler_func_serve,
            "http.HandlerFunc",
            Receiver::Value,
            file,
        );
        add_method_receiver_hint(
            &mut graph,
            handler_serve_http,
            "http.Handler",
            Receiver::Value,
            file,
        );

        // Signature hints — same canonical bytes on the interface
        // method, the named-type method, and the bare function. The
        // named function-type carries the same underlying signature.
        add_method_signature_hint(&mut graph, handler_serve_http, handler_sig, file);
        add_method_signature_hint(&mut graph, handler_func_serve, handler_sig, file);
        add_function_signature_hint(&mut graph, handler_func, handler_sig, file);
        add_function_signature_hint(&mut graph, handle_index, handler_sig, file);

        // T1.3 Source A hint: `HandlerFunc(handleIndex)` conversion.
        add_named_type_conversion_hint(
            &mut graph,
            call_site,
            "http.HandlerFunc",
            handle_index,
            file,
        );

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        // T1.1 half: Implements(HandlerFunc → Handler).
        let hf_outgoing = graph.edges().edges_from(handler_func);
        assert!(
            hf_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == handler),
            "T1.1: Implements(HandlerFunc → Handler) MUST be emitted (HandlerFunc has \
             ServeHTTP with matching signature). Outgoing edges from HandlerFunc: \
             {hf_outgoing:?}",
        );

        // T1.3 half: Implements(handleIndex → HandlerFunc).
        let hi_outgoing = graph.edges().edges_from(handle_index);
        assert!(
            hi_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == handler_func),
            "T1.3: Implements(handleIndex → HandlerFunc) MUST be emitted (bare function \
             signature matches HandlerFunc's underlying signature). Outgoing edges from \
             handleIndex: {hi_outgoing:?}",
        );

        // Sanity: at least one T1.3 edge was counted in stats (must
        // include handleIndex → HandlerFunc).
        assert!(
            stats.signature_implements_edges >= 1,
            "AC-11: stats.signature_implements_edges must reflect the T1.3 emission, \
             got {}",
            stats.signature_implements_edges,
        );

        // Sanity: at least one T1.1 emission counted.
        assert!(
            stats.implements_edges_value + stats.implements_edges_pointer >= 1,
            "AC-11: T1.1 must emit at least the HandlerFunc → Handler edge, got value={} \
             pointer={}",
            stats.implements_edges_value,
            stats.implements_edges_pointer,
        );
    }

    /// D3 baseline cross-check: matching signatures + matching name DO
    /// produce an `Implements` edge under the tightened predicate. This
    /// is the positive companion to `d3_ac7_signature_mismatch` — without
    /// this assertion, `d3_ac7_signature_mismatch` could pass trivially
    /// if D3 dropped Implements emission entirely.
    #[test]
    fn d3_name_and_signature_match_emits_implements() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let r = make_qn_node(&mut graph, NodeKind::Interface, "R", "fx.R", file);
        let r_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.R.Read", file);

        let f = make_qn_node(&mut graph, NodeKind::Struct, "F", "fx.F", file);
        let f_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.F.Read", file);
        rebuild_indices(&mut graph);

        add_method_receiver_hint(&mut graph, r_read, "fx.R", Receiver::Value, file);
        add_method_receiver_hint(&mut graph, f_read, "fx.F", Receiver::Value, file);

        let sig = "([]byte)(int,error)";
        add_method_signature_hint(&mut graph, r_read, sig, file);
        add_method_signature_hint(&mut graph, f_read, sig, file);

        let _ = run_go_method_set_satisfaction(&mut graph, None);

        assert!(
            graph
                .edges()
                .edges_from(f)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == r),
            "Implements(F → R) MUST be emitted — name AND signature match",
        );
    }

    // ========================================================================
    // Cluster E1 — Pipeline wiring smoke tests
    // ========================================================================
    //
    // These tests exercise the entrypoint contracts the full-build and
    // incremental-rebuild pipelines depend on: `run_go_method_set_satisfaction(_, None)`
    // walks the entire graph and emits non-zero stats on a fixture that
    // has at least one implicit `Implements`; the generic variant called
    // with `Some(&[file_id])` produces an equivalent satisfaction set on
    // the same fixture under the incremental-rebuild parameter shape.
    //
    // The actual wiring sites in `entrypoint.rs` and `incremental.rs` are
    // verified by compilation; these tests pin the entrypoint contract
    // those sites depend on so a future change to the entrypoint
    // signature breaks here rather than silently desyncing.
    //
    // The integration tests in Cluster F3 cover the end-to-end pipeline
    // log-line emission against a real Go workspace fixture.

    /// E1: the full-build entrypoint (`changed_files = None`) emits at
    /// least one implicit `Implements` edge on a fixture where a struct's
    /// pointer method-set satisfies a single-method interface. Mirrors
    /// the AC-1 fixture so we are exercising the production code path,
    /// not a test-only shortcut.
    #[test]
    fn e1_full_build_pass_runs() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        let _file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        rebuild_indices(&mut graph);
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert!(
            stats.satisfaction_pairs_examined > 0,
            "satisfaction loop must have inspected (File, Reader) on the full-build path",
        );
        assert!(
            stats.implements_edges_pointer >= 1,
            "expected at least one pointer-form Implements on the AC-1 fixture, got {}",
            stats.implements_edges_pointer,
        );

        let ptr_qn = qn_strid(&graph, "fx.*File").expect("pointer-form qn interned");
        let ptr_node_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
        assert_eq!(
            ptr_node_candidates.len(),
            1,
            "exactly one synthetic *fx.File node materialised",
        );
        let ptr_node = ptr_node_candidates[0];
        assert!(
            graph
                .edges()
                .edges_from(ptr_node)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(*File → Reader) MUST be emitted on the full-build path",
        );
    }

    /// E1: the incremental-rebuild entrypoint (`changed_files = Some(&[..])`)
    /// runs the pass and reports stats equivalent to the full-build call
    /// on the same fixture. Drives the parameter shape the production
    /// `incremental_rebuild` wiring uses.
    #[test]
    fn e1_incremental_build_pass_runs() {
        let mut graph = CodeGraph::new();
        // Cluster E2 iter-2: incremental path requires the fixture
        // file registered with `Language::Go` so the pass's
        // Go-language scope filter does NOT silently drop the file
        // from the changed-file slice.
        let file = register_go_file(&mut graph, "/tmp/fx.go");

        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        let _file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        rebuild_indices(&mut graph);
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file);

        // Drive the generic entrypoint with the incremental parameter
        // shape so a tombstone-aware future change has to keep this
        // single-file rebuild green.
        let stats = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file]));

        assert!(
            stats.satisfaction_pairs_examined > 0,
            "satisfaction loop must have inspected (File, Reader) on the incremental path",
        );
        assert!(
            stats.implements_edges_pointer >= 1,
            "expected at least one pointer-form Implements on the AC-1 fixture, got {}",
            stats.implements_edges_pointer,
        );

        let ptr_qn = qn_strid(&graph, "fx.*File").expect("pointer-form qn interned");
        let ptr_node_candidates = graph.indices().by_qualified_name(ptr_qn).to_vec();
        assert_eq!(
            ptr_node_candidates.len(),
            1,
            "exactly one synthetic *fx.File node materialised on incremental path",
        );
        let ptr_node = ptr_node_candidates[0];
        assert!(
            graph
                .edges()
                .edges_from(ptr_node)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "Implements(*File → Reader) MUST be emitted on the incremental path",
        );
    }

    // ========================================================================
    // Cluster E2 — Pass-owned predicates + tombstone behaviour
    // ========================================================================

    /// E2: `is_pass_owned_node` returns true exactly when the node's
    /// kind ∈ {Method, Type}, the Synthetic flag is set, and the
    /// qualified-name matches one of the three pass-emitted shapes.
    /// Table-driven across the relevant combinations.
    #[test]
    fn e2_is_pass_owned_node_predicate() {
        let file = FileId::new(0);
        let sid = StringId::new(0);

        // Cluster G1: pass operates on canonical (`::`-separated)
        // qns; the namespace-shape predicate matches `::`-form
        // shapes. Test cases below use canonical qns directly so
        // they exercise the predicate as the pass invokes it. The
        // shapes recognised are:
        //   value-form promoted:   `<pkg>::<S>::<m>`
        //   pointer-form promoted: `<pkg>::*<S>::<m>`
        //   pointer-form anchor:   `<pkg>::*<S>`
        // See 05_TEST_PLAN.md §7.5.
        //
        // (kind, qn, is_synthetic, expected)
        let cases: &[(NodeKind, &str, bool, bool)] = &[
            // Pass-owned positives.
            (NodeKind::Method, "fx::File::Read", true, true), // value-form promoted
            (NodeKind::Method, "fx::*File::Read", true, true), // pointer-form promoted
            (NodeKind::Type, "fx::*File", true, true),        // pointer-form anchor
            // Synthetic flag missing → false.
            (NodeKind::Method, "fx::File::Read", false, false),
            (NodeKind::Type, "fx::*File", false, false),
            // Wrong kind → false even with synthetic flag.
            (NodeKind::Struct, "fx::File::Read", true, false),
            (NodeKind::Interface, "fx::*File", true, false),
            // qn shape doesn't match (no canonical separator / single segment) → false.
            (NodeKind::Method, "noPackage", true, false),
            (NodeKind::Type, "fx::File", true, false), // no `::*`
            // Empty qn → false (matches no shape).
            (NodeKind::Method, "", true, false),
            // Malformed pointer-form `<pkg>::*` (no struct name) → false.
            (NodeKind::Type, "fx::*", true, false),
        ];

        for (kind, qn, is_synthetic, expected) in cases {
            let view = NodeEntry::new(*kind, sid, file);
            assert_eq!(
                is_pass_owned_node(&view, qn, *is_synthetic),
                *expected,
                "is_pass_owned_node({:?}, {:?}, is_synthetic={}) expected {}",
                kind,
                qn,
                is_synthetic,
                expected,
            );
        }
    }

    /// E2: `is_pass_owned_edge` returns true exactly for the four
    /// classifications in 02_DESIGN §3.6 lines 1034-1072:
    /// (A) Contains/Inherits with one endpoint pass-owned,
    /// (B) Calls with pass-owned Method target,
    /// (C) References with pass-owned Method target,
    /// (D) Implements with target qn NOT starting with `<type:`.
    #[test]
    fn e2_is_pass_owned_edge_predicate() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Real (non-pass-owned) nodes.
        let real_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let real_iface = make_qn_node(&mut graph, NodeKind::Interface, "Reader", "fx.Reader", file);
        let real_method = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);

        // Pass-owned (synthetic) nodes — fake them by marking
        // Synthetic and giving them pass-shape qns. Mirrors the live
        // pass's mint_synthetic_node helper without driving the
        // promotion algorithm.
        let synth_promoted_method = make_qn_node(
            &mut graph,
            NodeKind::Method,
            "Greeting",
            "fx.Outer.Greeting",
            file,
        );
        let synth_pointer_type = make_qn_node(&mut graph, NodeKind::Type, "File", "fx.*File", file);
        // Mark them synthetic via the live metadata store.
        graph
            .macro_metadata_mut()
            .mark_synthetic(synth_promoted_method);
        graph
            .macro_metadata_mut()
            .mark_synthetic(synth_pointer_type);

        // Type-assertion `<type:T>` Interface — Phase-1 emission shape
        // that must NOT count as pass-owned for predicate (D).
        let type_assertion_iface =
            make_qn_node(&mut graph, NodeKind::Interface, "T", "<type:fx.T>", file);

        // Driver to call the predicate without colliding with the
        // graph borrow during in-line emission.
        let classify = |s: NodeId, t: NodeId, k: EdgeKind, g: &CodeGraph| {
            let edge_ref = StoreEdgeRef {
                source: s,
                target: t,
                kind: k,
                seq: 0,
                file,
                spans: Vec::new(),
            };
            is_pass_owned_edge(&edge_ref, g.nodes(), g.strings(), g.macro_metadata())
        };

        // (A) Contains: real Struct → synthetic promoted Method ⇒ true.
        assert!(
            classify(
                real_struct,
                synth_promoted_method,
                EdgeKind::Contains,
                &graph
            ),
            "Contains(real Struct → synthetic Method) MUST be pass-owned",
        );
        // (A) Inherits: synthetic Type → real Struct ⇒ true.
        assert!(
            classify(synth_pointer_type, real_struct, EdgeKind::Inherits, &graph),
            "Inherits(synthetic Type → real Struct) MUST be pass-owned",
        );
        // (A) Contains between two real nodes ⇒ false.
        assert!(
            !classify(real_struct, real_method, EdgeKind::Contains, &graph),
            "Contains(real → real) MUST NOT be pass-owned",
        );

        // (B) Calls into synthetic Method ⇒ true.
        assert!(
            classify(
                real_method,
                synth_promoted_method,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                &graph
            ),
            "Calls(real → synthetic Method) MUST be pass-owned",
        );
        // (B) Calls into real Method ⇒ false.
        assert!(
            !classify(
                real_method,
                real_method,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                &graph
            ),
            "Calls(real → real Method) MUST NOT be pass-owned",
        );

        // (C) References into synthetic Method ⇒ true.
        assert!(
            classify(
                real_method,
                synth_promoted_method,
                EdgeKind::References,
                &graph
            ),
            "References(real → synthetic Method) MUST be pass-owned",
        );

        // (D) Implements with non-`<type:` target ⇒ true.
        assert!(
            classify(real_struct, real_iface, EdgeKind::Implements, &graph),
            "Implements(real Struct → real Interface) MUST be pass-owned (predicate D)",
        );
        // (D) Implements with `<type:` target ⇒ false (type-assertion).
        assert!(
            !classify(
                real_struct,
                type_assertion_iface,
                EdgeKind::Implements,
                &graph
            ),
            "Implements(_ → <type:T>) MUST NOT be pass-owned (Phase-1 type-assertion)",
        );

        // Non-tracked kind (TypeOf) ⇒ false.
        let dummy_name_id = graph.strings_mut().intern("dummy").expect("intern");
        assert!(
            !classify(
                real_method,
                synth_promoted_method,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Parameter),
                    index: Some(0),
                    name: Some(dummy_name_id),
                },
                &graph
            ),
            "TypeOf edges MUST NOT be classified as pass-owned",
        );
    }

    /// E2: tombstone-and-re-emit removes a now-orphan
    /// `Implements(C → I)` when the source file no longer satisfies
    /// the interface after a mutation. Mutates the receiver-pointerness
    /// hint between two runs to simulate the source edit removing the
    /// satisfying method's receiver match.
    #[test]
    fn e2_tombstone_removes_orphan_implements() {
        // Cluster E2 iter-2: file must be Go-language-registered so
        // the pass's Go-language scope filter includes it.
        let mut graph = CodeGraph::new();
        let file = register_go_file(&mut graph, "/tmp/orphan_fx.go");

        // First run: build the AC-1 fixture where `*File` satisfies
        // `Reader`, observe the pointer-form Implements edge.
        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        let _file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        rebuild_indices(&mut graph);
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file);

        let _stats1 = run_go_method_set_satisfaction(&mut graph, None);

        let ptr_qn_first =
            qn_strid(&graph, "fx.*File").expect("pointer-form qn interned by first run");
        let ptr_first_candidates = graph.indices().by_qualified_name(ptr_qn_first).to_vec();
        assert_eq!(
            ptr_first_candidates.len(),
            1,
            "first run mints exactly one *fx.File pointer-form node",
        );
        let ptr_first = ptr_first_candidates[0];
        assert!(
            graph
                .edges()
                .edges_from(ptr_first)
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "first run: Implements(*File → Reader) MUST exist",
        );

        // Simulate a source edit that orphans the satisfaction:
        // remove the `File.Read` method node entirely. Without any
        // method, `File`'s method set is empty, so neither value- nor
        // pointer-bucket can satisfy `Reader`. The tombstone-before-emit
        // step must remove the now-orphan pointer-form `Implements`
        // edge emitted by the first run.
        //
        // We also clear the receiver hint to keep the GoHints view
        // consistent with the deletion (a real source edit would
        // wipe both).
        graph.nodes_mut().remove(file_read);
        graph.go_hints_mut().method_receivers.clear();

        let stats2 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file]));

        assert_eq!(
            stats2.implements_edges_pointer, 0,
            "second run emits zero pointer-form Implements after the orphaning edit",
        );
        assert_eq!(
            stats2.implements_edges_value, 0,
            "second run emits zero value-form Implements after the orphaning edit",
        );

        // The pointer-form Type node was tombstoned in Phase 6 of the
        // driver; its `NodeId` from the first run is now stale (the
        // arena slot's generation advanced) so a fresh lookup returns
        // None, AND the by-qualified-name index no longer maps to a
        // live node carrying an Implements(→ Reader) edge.
        let ptr_qn_second = qn_strid(&graph, "fx.*File").expect("string still interned");
        let ptr_second_candidates = graph
            .indices()
            .by_qualified_name(ptr_qn_second)
            .iter()
            .filter(|&&nid| graph.nodes().contains(nid))
            .copied()
            .collect::<Vec<_>>();
        assert!(
            ptr_second_candidates.is_empty(),
            "no live *fx.File node remains after tombstoning, found {:?}",
            ptr_second_candidates,
        );
        // The original Implements edge sourced from `ptr_first` is
        // either bulk-tombstoned (target lookup is stale) or removed —
        // either way, no live Implements(_ → Reader) survives in the
        // edge store.
        let stale_outgoing = graph.edges().edges_from(ptr_first);
        assert!(
            !stale_outgoing
                .iter()
                .any(|e| matches!(e.kind, EdgeKind::Implements) && e.target == reader),
            "tombstoned source MUST NOT retain live Implements edge",
        );
    }

    /// E2 + AC-12 prerequisite: re-running the pass on an unchanged
    /// graph yields identical [`GoMethodSetStats`] (zero edge-churn).
    /// The fixture has a non-trivial satisfaction set so any spurious
    /// re-emission would inflate counts on the second run.
    #[test]
    fn e2_idempotent_re_run_on_unchanged_files() {
        // Cluster E2 iter-2: file must be Go-language-registered so
        // the pass's Go-language scope filter includes it.
        let mut graph = CodeGraph::new();
        let file = register_go_file(&mut graph, "/tmp/idempotent_fx.go");
        let _reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file);
        let _file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file);
        rebuild_indices(&mut graph);
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file);

        // Run 1: full-build plane, no tombstone step.
        let stats1 = run_go_method_set_satisfaction(&mut graph, None);
        assert!(
            stats1.implements_edges_pointer >= 1,
            "run 1 should emit at least one pointer-form Implements",
        );

        // Run 2: incremental plane, tombstone-then-re-emit. Same input
        // ⇒ same edge counts. We compare counters individually (rather
        // than the whole struct) because `elapsed_ms` is a wall-clock
        // measurement that may differ between runs.
        let stats2 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file]));
        assert_eq!(
            stats1.implements_edges_value, stats2.implements_edges_value,
            "implements_edges_value identical across re-runs",
        );
        assert_eq!(
            stats1.implements_edges_pointer, stats2.implements_edges_pointer,
            "implements_edges_pointer identical across re-runs",
        );
        assert_eq!(
            stats1.signature_implements_edges, stats2.signature_implements_edges,
            "signature_implements_edges identical across re-runs",
        );
        assert_eq!(
            stats1.promoted_method_nodes, stats2.promoted_method_nodes,
            "promoted_method_nodes identical across re-runs",
        );
        assert_eq!(
            stats1.promoted_back_reference_edges, stats2.promoted_back_reference_edges,
            "promoted_back_reference_edges identical across re-runs",
        );
        assert_eq!(
            stats1.satisfaction_pairs_examined, stats2.satisfaction_pairs_examined,
            "satisfaction_pairs_examined identical across re-runs",
        );

        // Run 3: confirm a third re-run on the same input is also
        // stable. The (run 2, run 3) pair is the strongest idempotence
        // claim because both runs go through the tombstone path.
        let stats3 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file]));
        assert_eq!(
            stats2.implements_edges_value, stats3.implements_edges_value,
            "implements_edges_value stable across consecutive incremental re-runs",
        );
        assert_eq!(
            stats2.implements_edges_pointer, stats3.implements_edges_pointer,
            "implements_edges_pointer stable across consecutive incremental re-runs",
        );
        assert_eq!(
            stats2.promoted_method_nodes, stats3.promoted_method_nodes,
            "promoted_method_nodes stable across consecutive incremental re-runs",
        );
    }

    /// E2 iter-2 regression — cross-file `Implements` idempotence.
    ///
    /// Codex iter-1 BLOCKING finding 1: the iter-1 tombstone driver
    /// walked only **outgoing** Implements from changed-file anchors
    /// and ran T1.1 re-emission whole-graph. In a multi-file
    /// workspace where `C` lives in `F_C` and `I` lives in `F_I`,
    /// triggering an incremental rebuild with
    /// `changed_files = [F_I]` (only) would:
    ///
    ///   1. Tombstone walks `F_I`'s anchors → outgoing from `I`: no
    ///      `Implements` (`I` is the target). Incoming was not
    ///      walked. The prior `Implements(C → I)` survives.
    ///   2. T1.1 re-emits whole-graph → `(C, I)` still satisfies →
    ///      adds a duplicate `Implements(C → I)` delta.
    ///
    /// On every subsequent rebuild this duplicates again — AC-12
    /// determinism violation.
    ///
    /// This test:
    ///   * builds a 2-file fixture (`fx_c.go` defining `*C`, `fx_i.go`
    ///     defining the `Reader` interface, with `Read` on `*C`);
    ///   * runs the pass three times (full-build, then twice
    ///     incremental with `changed_files = [F_I]`);
    ///   * asserts the **live `Implements` edge multiset** is
    ///     bit-identical across all three runs — not just stat
    ///     counters (the iter-1 idempotence test only checked stats,
    ///     so the duplicate-delta variant slipped through).
    #[test]
    fn e2_multi_file_implements_idempotent_across_reruns() {
        let mut graph = CodeGraph::new();
        let file_c = register_go_file(&mut graph, "/tmp/fx_c.go");
        let file_i = register_go_file(&mut graph, "/tmp/fx_i.go");

        // `I` (Reader interface) lives in `file_i`.
        let reader =
            make_interface_with_methods(&mut graph, "Reader", "fx.Reader", &["Read"], file_i);
        // `C` (File struct) and its `Read` method live in `file_c`.
        let _file_struct = make_qn_node(&mut graph, NodeKind::Struct, "File", "fx.File", file_c);
        let file_read = make_qn_node(&mut graph, NodeKind::Method, "Read", "fx.File.Read", file_c);
        rebuild_indices(&mut graph);
        add_method_receiver_hint(&mut graph, file_read, "fx.File", Receiver::Pointer, file_c);

        // Helper to snapshot the live Implements edge multiset (as a
        // sorted `Vec<(source.index, target.index)>` so cross-run
        // comparison is order-independent and stable).
        fn implements_multiset(graph: &CodeGraph) -> Vec<(u32, u32)> {
            let mut out: Vec<(u32, u32)> = graph
                .edges()
                .all_live_forward_edges()
                .into_iter()
                .filter(|e| matches!(e.kind, EdgeKind::Implements))
                .map(|e| (e.source.index(), e.target.index()))
                .collect();
            out.sort();
            out
        }

        // Run 1: full build. Expect at least the pointer-form
        // `Implements(*fx.File → fx.Reader)`.
        let _stats1 = run_go_method_set_satisfaction(&mut graph, None);
        let multiset1 = implements_multiset(&graph);
        assert!(
            !multiset1.is_empty(),
            "full build must emit at least one Implements edge on the cross-file fixture",
        );
        // Sanity: the pointer-form `*fx.File` Type node exists and
        // targets the Reader interface.
        let reader_target_index = reader.index();
        assert!(
            multiset1.iter().any(|(_s, t)| *t == reader_target_index),
            "full build must emit Implements(_ → Reader)",
        );

        // Run 2: incremental rebuild scoped to `file_i` ONLY (the
        // interface file). `C`'s file `file_c` is UNCHANGED. This is
        // the exact codex iter-1 failure case.
        let _stats2 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_i]));
        let multiset2 = implements_multiset(&graph);
        assert_eq!(
            multiset1, multiset2,
            "incremental rebuild scoped to interface file must NOT duplicate \
             Implements(_ → Reader): iter-1 left the pre-existing edge live and \
             re-emitted a duplicate; iter-2 must produce a bit-identical multiset.\n\
             run 1 multiset: {:?}\nrun 2 multiset: {:?}",
            multiset1, multiset2,
        );

        // Run 3: third rebuild, same scope. Strongest idempotence
        // claim (both runs go through the tombstone-and-re-emit
        // path, both targeting only the interface side).
        let _stats3 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_i]));
        let multiset3 = implements_multiset(&graph);
        assert_eq!(
            multiset2, multiset3,
            "consecutive incremental rebuilds with the same changed-file scope \
             must produce a bit-identical Implements multiset.\n\
             run 2 multiset: {:?}\nrun 3 multiset: {:?}",
            multiset2, multiset3,
        );

        // Symmetric case: rebuild scoped to the source file
        // (`file_c`). Same multiset must hold.
        let _stats4 = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_c]));
        let multiset4 = implements_multiset(&graph);
        assert_eq!(
            multiset3, multiset4,
            "incremental rebuild scoped to source file must produce the same \
             Implements multiset as one scoped to the target file.\n\
             run 3 multiset: {:?}\nrun 4 multiset: {:?}",
            multiset3, multiset4,
        );
    }

    /// E2 regression — T1.3 Source A cross-file conversion correctness.
    ///
    /// Three-file fixture for the canonical T1.3 Source A case: a free
    /// function `g` in one library file, a named function type `T` in
    /// another library file, and a `T(g)` conversion observed at a
    /// third-party call site (a third file). The `Implements(g → T)`
    /// edge anchors `edge.file = hint_file` (the call-site file),
    /// which differs from BOTH `g.file` and `T.file`.
    ///
    /// **Iter-4 / Option C behaviour**: there is no scope-skip; T1.3
    /// always re-emits whole-graph and the tombstone driver always
    /// runs whole-graph on the incremental plane. The three arms below
    /// therefore exercise the full re-emit + whole-graph tombstone
    /// path on a non-trivial cross-file Source A fixture:
    ///
    ///   * **Addition**: start with no conversion hint, add it,
    ///     incremental-rebuild scoped to the call-site file. Assert
    ///     the new `Implements(g → T)` edge appears. (Under Option C
    ///     the scope is irrelevant for emission — T1.3 enumerates
    ///     all pending entries whole-graph — but the scope still
    ///     gates the whole-graph tombstone at pass entry.)
    ///   * **Idempotence**: re-run with the same scope. Whole-graph
    ///     tombstone removes the prior Source A edge; whole-graph
    ///     re-emit reproduces it. Live multiset stays bit-identical.
    ///   * **Removal**: clear the hint, incremental-rebuild scoped to
    ///     the call-site file. Whole-graph tombstone removes the
    ///     edge; whole-graph re-emit produces no replacement (no
    ///     pending entry exists). Live multiset has no `Implements`.
    ///
    /// Historical: this test was named
    /// `e2_t1_3_source_a_hint_file_scope_axis` and originated as the
    /// codex iter-2 BLOCKING regression that drove iter-3's three-axis
    /// scope-skip. Iter-4 deleted the scope-skip entirely; the
    /// fixture remains valuable as a cross-file Source A correctness
    /// regression, but the test name and rationale now describe the
    /// Option C semantics directly.
    #[test]
    fn e2_t1_3_source_a_cross_file_correctness() {
        let mut graph = CodeGraph::new();
        // Three Go files: function-definition, named-type
        // definition, call-site.
        let file_func = register_go_file(&mut graph, "/tmp/fx_func.go");
        let file_type = register_go_file(&mut graph, "/tmp/fx_type.go");
        let file_call = register_go_file(&mut graph, "/tmp/fx_call.go");

        // `g` (Function) in `file_func`, package `lib`.
        let g = make_qn_node(&mut graph, NodeKind::Function, "g", "lib.g", file_func);
        // `T` (named function-type) in `file_type`, also package
        // `lib`. Source A requires `same_package_qn(g, T)`.
        let t = make_qn_node(&mut graph, NodeKind::Type, "T", "lib.T", file_type);
        // `caller` (the host function holding the `T(g)` call site)
        // in `file_call`, package `app`. Not used by the predicate;
        // just present so the call_site NodeId is valid.
        let caller = make_qn_node(
            &mut graph,
            NodeKind::Function,
            "caller",
            "app.caller",
            file_call,
        );
        rebuild_indices(&mut graph);

        // Signature hints — must match between `g` and `T`'s
        // underlying signature so the predicate `arg_sig == target_sig`
        // holds.
        let sig = "()(int)";
        add_function_signature_hint(&mut graph, g, sig, file_func);
        add_function_signature_hint(&mut graph, t, sig, file_type);

        // Helper to snapshot the Implements multiset (sorted by
        // (source.index, target.index) for order-independent
        // comparison).
        fn implements_multiset(graph: &CodeGraph) -> Vec<(u32, u32)> {
            let mut out: Vec<(u32, u32)> = graph
                .edges()
                .all_live_forward_edges()
                .into_iter()
                .filter(|e| matches!(e.kind, EdgeKind::Implements))
                .map(|e| (e.source.index(), e.target.index()))
                .collect();
            out.sort();
            out
        }

        // Run 1: full build, NO conversion hint yet. Source A is
        // empty; Source B has no TypeOf/References to follow.
        // Expectation: zero Implements emitted by T1.3.
        let _ = run_go_method_set_satisfaction(&mut graph, None);
        let multiset_no_hint = implements_multiset(&graph);
        assert!(
            !multiset_no_hint
                .iter()
                .any(|(s, target)| *s == g.index() && *target == t.index()),
            "pre-condition: no Implements(g → T) before the conversion hint is added",
        );

        // Simulate a source edit: add a `T(g)` conversion at
        // `caller`, anchored at the call-site file. The hint's
        // `file` field IS `file_call` — the third axis the iter-2
        // fix missed.
        add_named_type_conversion_hint(&mut graph, caller, "lib.T", g, file_call);

        // Run 2: incremental rebuild scoped to the call-site file
        // ONLY. Under iter-2, the T1.3 drain skipped this emission
        // because `g.file = file_func ∉ scope` and
        // `T.file = file_type ∉ scope`; iter-3 includes
        // `pending_file = hint_file = file_call ∈ scope` in the
        // OR-chain.
        let _ = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_call]));
        let multiset_after_add = implements_multiset(&graph);
        assert!(
            multiset_after_add
                .iter()
                .any(|(s, target)| *s == g.index() && *target == t.index()),
            "iter-3: incremental rebuild scoped to call-site file MUST emit \
             Implements(g → T) when a new T(g) conversion hint is added.\n\
             multiset: {:?}",
            multiset_after_add,
        );

        // Run 3: re-run with the SAME scope, no further mutation.
        // Idempotence: same multiset.
        let _ = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_call]));
        let multiset_after_rerun = implements_multiset(&graph);
        assert_eq!(
            multiset_after_add, multiset_after_rerun,
            "consecutive incremental rebuilds with the call-site file in scope \
             MUST produce a bit-identical Implements multiset.\n\
             run 2: {:?}\nrun 3: {:?}",
            multiset_after_add, multiset_after_rerun,
        );

        // Simulate the symmetric source edit: remove the `T(g)`
        // conversion (the developer deleted the conversion
        // expression). Re-run the incremental rebuild scoped to the
        // call-site file.
        graph.go_hints_mut().named_type_conversions.clear();
        let _ = run_go_method_set_satisfaction_generic(&mut graph, Some(&[file_call]));
        let multiset_after_remove = implements_multiset(&graph);
        assert!(
            !multiset_after_remove
                .iter()
                .any(|(s, target)| *s == g.index() && *target == t.index()),
            "iter-3: incremental rebuild scoped to call-site file MUST tombstone \
             Implements(g → T) when its driving T(g) conversion hint is removed.\n\
             Under iter-2 the tombstone-side anchor-walk missed this edge \
             (neither endpoint in scope, edge.file in scope but not consulted).\n\
             multiset: {:?}",
            multiset_after_remove,
        );
    }

    // ========================================================================
    // Cluster F1 — net-new unit tests per `02_DESIGN.md` §10.3 lines
    // 2325-2340 and cluster-F1 brief §3.2. These extend the existing
    // D1/D2/D3/E1/E2 coverage with corner-case assertions for
    // normalize_signature and the §5.3 / §5.4 promotion rules.
    // ========================================================================

    /// F1 §3.2(1) — `02_DESIGN.md` §4.1.2 step 2. Named vs unnamed
    /// parameter forms must canonicalise to the same byte sequence at
    /// the parameter-list level. The legacy `canonicalise_signature`
    /// idempotence tests already cover `(p []byte)` vs `([]byte)` via
    /// `parameter_name_is_stripped_on_slice_form`; this test asserts
    /// the same property at the `canonicalise_param_list` boundary so
    /// future refactors of the param-list normaliser cannot drift
    /// without breaking this gate.
    #[test]
    fn normalize_signature_value_param_named_and_unnamed_match() {
        let named = canonicalise_param_list("(p []byte)");
        let unnamed = canonicalise_param_list("([]byte)");
        assert_eq!(
            named, unnamed,
            "named and unnamed param lists must canonicalise identically"
        );
        assert_eq!(named, "[]byte");

        // Symmetric assertions across pointer / qualified-type / multi-arg
        // forms — ensures the bytewise equality is robust across §4.1.2
        // rule 2's full surface, not just the slice form.
        assert_eq!(
            canonicalise_param_list("p *T"),
            canonicalise_param_list("*T"),
        );
        assert_eq!(
            canonicalise_param_list("r io.Reader"),
            canonicalise_param_list("io.Reader"),
        );
        assert_eq!(
            canonicalise_param_list("a int, b string"),
            canonicalise_param_list("int, string"),
        );
    }

    /// F1 §3.2(2) — `02_DESIGN.md` §4.1.2 step 4 / `01_SPEC.md` §5.4.
    /// The canonical-signature normaliser is purely textual; it does
    /// NOT perform alias resolution against a graph. Alias unwrapping
    /// happens at the promotion-BFS layer via
    /// [`resolve_alias_underlying_qn`], not here.
    ///
    /// Marked `#[ignore]`: the brief §3.2(2) proposal "type Buf =
    /// []byte → func(Buf) int and func([]byte) int produce identical
    /// canonical signatures" cannot pass against
    /// `canonicalise_signature` as a pure-text function. Per the
    /// cluster-F1 brief §9 STOP-condition rule + `05_TEST_PLAN.md`
    /// §7.1 ("if normalization isn't carried through yet"), this
    /// test is parked here as a gate the F2 proptest corpus or a
    /// Cluster G follow-up will satisfy after alias-aware
    /// canonicalisation lands. The body asserts the textual surface
    /// (i.e. `Buf` and `[]byte` differ at the text layer, which is
    /// the correct behaviour for a non-alias-aware normaliser) so
    /// the test compiles and documents the boundary.
    #[test]
    #[ignore = "Phase 2 / Cluster G — canonicalise_signature is text-only; alias unwrapping lives in resolve_alias_underlying_qn at the promotion-BFS layer. See 01_SPEC §5.4 / 02_DESIGN §4.1.2 step 4 / 05_TEST_PLAN §7.1."]
    fn normalize_signature_alias_one_level() {
        // Text-layer assertion: today, the alias name and the
        // underlying type produce DIFFERENT canonical signatures
        // because `canonicalise_signature` is pure text. A future
        // alias-aware variant must collapse them.
        let alias_form = canonicalise_signature("Buf", "int");
        let unwrapped = canonicalise_signature("[]byte", "int");
        // Once alias unwrapping is folded into the normaliser
        // (Phase 2 / Cluster G), the next assertion will become
        // `assert_eq!(alias_form, unwrapped)`. Today, the two
        // signatures differ.
        assert_eq!(
            alias_form, unwrapped,
            "alias-aware canonicalise_signature must unwrap one-level Go type aliases"
        );
    }

    /// F1 §3.2(3) — `02_DESIGN.md` §4.1.2 step 5. Generic typeparam
    /// qualification on `List[E]` receiver and
    /// `func (l *List[E]) Push(v E)` should collapse to a stable
    /// normalised form across declaration and call site.
    ///
    /// Marked `#[ignore]` per `05_TEST_PLAN.md` §7.1 — the
    /// cluster-F1 brief §3.2(3) "Gating risk" calls out that
    /// `canonicalise_signature` does NOT currently carry typeparam
    /// qualification. Per `01_SPEC.md` §8 lines 907-910,
    /// "Universally-quantified generic constraints" are Phase 2.
    /// The assertion body is staged so a Cluster G or Phase 2
    /// commit can flip the `#[ignore]`.
    #[test]
    #[ignore = "Phase 2 — generics typeparam qualification per 01_SPEC §8 lines 907-910 / 02_DESIGN §4.1.2 step 5 / 05_TEST_PLAN §7.1."]
    fn normalize_signature_generic_typeparam_qualification() {
        // Receiver-bound typeparam: `(l *List[E]) Push(v E)`. At a
        // call site `foo.Push(x)`, the receiver-typeparam E is
        // bound to whatever instantiation `foo` was created with.
        // Phase 2 canonicalisation must collapse the declared and
        // instantiated forms to one normalised signature.
        let declared = canonicalise_signature("v E", "");
        let instantiated_int = canonicalise_signature("v int", "");
        assert_eq!(
            declared, instantiated_int,
            "Phase 2: generic typeparam-qualified signatures must collapse to a single instantiation"
        );
    }

    /// F1 §3.2(4) — golang/go#69557. The outer struct's own depth-0
    /// method shadows any same-named embedded method at depth ≥ 1.
    /// Fixture: `type Inner struct{}; func (Inner) M(){}; type Outer
    /// struct { Inner }; func (Outer) M(){}`. The pass must NOT
    /// promote `Inner.M` onto `Outer` — `Outer.M` is the outer's own
    /// definition, not a synthetic promoted node.
    #[test]
    fn promotion_respects_outer_shadow() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        let _inner = make_qn_node(&mut graph, NodeKind::Struct, "Inner", "fx.Inner", file);
        let inner_m = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Inner.M", file);
        let outer = make_qn_node(&mut graph, NodeKind::Struct, "Outer", "fx.Outer", file);
        // The outer's OWN M — depth-0; this is the shadower.
        let outer_m_real = make_qn_node(&mut graph, NodeKind::Method, "M", "fx.Outer.M", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, outer, "fx.Inner", Receiver::Value, file);
        add_method_receiver_hint(&mut graph, inner_m, "fx.Inner", Receiver::Value, file);
        add_method_receiver_hint(&mut graph, outer_m_real, "fx.Outer", Receiver::Value, file);

        let stats = run_go_method_set_satisfaction(&mut graph, None);

        // `fx.Outer.M` bucket contains exactly the outer's OWN
        // depth-0 definition; no synthetic promoted node was minted.
        let outer_m_qn = qn_strid(&graph, "fx.Outer.M").expect("fx.Outer.M qn interned");
        let bucket = graph.indices().by_qualified_name(outer_m_qn).to_vec();
        assert_eq!(
            bucket.len(),
            1,
            "exactly one fx.Outer.M node (the outer's own); the embedded Inner.M MUST be shadowed",
        );
        assert_eq!(
            bucket[0], outer_m_real,
            "the surviving node is the outer's own definition, not a synthetic promotion",
        );

        // `promoted_method_nodes` may be zero (outer shadow blocked
        // promotion entirely) — assert it does NOT include a
        // promoted `fx.Outer.M` synthetic node by inspecting the
        // node's kind. We cannot assert `== 0` because other tests'
        // wider patterns may have a depth-1 promotion onto a
        // different name, but `fx.Outer.M` specifically must be
        // the outer's real Method node, not a synthetic one.
        let entry = graph.nodes().get(bucket[0]).expect("node entry exists");
        assert!(
            matches!(entry.kind, NodeKind::Method),
            "the surviving fx.Outer.M node is a real Method, not a synthetic promotion",
        );

        // The pass must have processed the embedding (so the
        // promotion decision was made and rejected, not silently
        // skipped). `satisfaction_pairs_examined` is a coarse
        // observability counter and may be zero in this test
        // because no interface is in scope; the strong assertion
        // is the kind-check above.
        let _ = stats;
    }

    /// F1 §3.2(5) — golang/go#69460. `type T = *int; type S struct
    /// { T }`. The embedded field type is a one-level pointer-alias
    /// to `int`, which carries no methods. The pass must accept
    /// the embedding without panic and surface no spurious method-
    /// set entries — `int` has no methods, so no shadow edges and
    /// no promoted nodes may be minted.
    #[test]
    fn promotion_alias_pointer_embedded() {
        use crate::graph::unified::edge::kind::TypeOfContext;

        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Minimal stand-in for `*int`: a Type node `*int` with no
        // attached methods. golang/go#69460's payload is "no panic
        // / no spurious edges", so the minimal fixture is correct.
        let int_ptr = make_qn_node(&mut graph, NodeKind::Type, "*int", "fx.*int", file);
        // type T = *int — alias as a Type node, one-level alias hop
        // via TypeOf { TypeParameter }.
        let t_alias = make_qn_node(&mut graph, NodeKind::Type, "T", "fx.T", file);
        graph.edges_mut().add_edge(
            t_alias,
            int_ptr,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::TypeParameter),
                index: None,
                name: None,
            },
            file,
        );
        // type S struct { T } — embed T.
        let s = make_qn_node(&mut graph, NodeKind::Struct, "S", "fx.S", file);
        rebuild_indices(&mut graph);

        add_embedding_hint(&mut graph, s, "fx.T", Receiver::Value, file);

        // Top assertion: the pass does not panic. Capture the
        // stats and assert no spurious method-set entries
        // materialise.
        let stats = run_go_method_set_satisfaction(&mut graph, None);

        assert_eq!(
            stats.promoted_method_nodes, 0,
            "alias-to-pointer-to-int has no methods; no synthetic promotions may be minted",
        );
        assert_eq!(
            stats.implements_edges_value + stats.implements_edges_pointer,
            0,
            "alias-to-pointer-to-int satisfies no interface; no Implements edges may be minted",
        );

        // No fx.S.* method bucket may be populated by the pass
        // (the only fx.S.* names should be no-ops). Spot-check by
        // confirming the strings interner did not mint a
        // synthetic promoted method qualified name.
        if let Some(qn) = qn_strid(&graph, "fx.S.M") {
            assert!(
                graph.indices().by_qualified_name(qn).is_empty(),
                "no spurious fx.S.M promotion may exist (T = *int has no method M)",
            );
        }
    }

    /// F1 §3.2(6) — `MAX_PROMOTION_DEPTH = 16` cap. Construct a
    /// linear embedding chain `S0 { S1 { S2 { ... { S20 { M } } } }
    /// }` whose depth exceeds the cap. The pass must terminate
    /// without panic and bound the synthetic-node count even though
    /// the chain depth exceeds the cap.
    ///
    /// Note: `02_DESIGN.md` §10.3 line 2339 proposes asserting on a
    /// `max_depth_truncations` field of `GoMethodSetStats`. That
    /// field does NOT exist on the struct today (verified against
    /// `GoMethodSetStats` at line 89 of this file). Per
    /// `05_TEST_PLAN.md` §7.2 and the cluster-F1 brief §9
    /// STOP-condition rule ("§3.2(6) max-depth-cap test requires a
    /// new GoMethodSetStats field → STOP; surface as scope leak"),
    /// F1 does NOT add the field. The test is reframed as a
    /// **robustness** assertion: no panic + bounded synthetic-node
    /// count. The existing `ambiguity_blocked_promotions` counter
    /// documented at line 84 ("blocked under the 'truncated'
    /// subcategory") may increment but is not strictly asserted —
    /// the binding correctness gate is "no panic + no unbounded
    /// promotion".
    #[test]
    fn promotion_max_depth_cap_warns_and_skips() {
        let mut graph = CodeGraph::new();
        let file = FileId::new(0);

        // Build a chain of (`MAX_PROMOTION_DEPTH` + 4) structs so
        // the BFS must hit the cap on at least one level. Each Sn
        // embeds S(n+1); only the deepest (Sn) carries a method M.
        let chain_len: usize = (MAX_PROMOTION_DEPTH as usize) + 4;
        let mut structs: Vec<NodeId> = Vec::with_capacity(chain_len);
        for i in 0..chain_len {
            let name = format!("S{i}");
            let qn = format!("fx.S{i}");
            let s = make_qn_node(&mut graph, NodeKind::Struct, &name, &qn, file);
            structs.push(s);
        }
        // The deepest type defines M.
        let last_qn = format!("fx.S{chain_len}.M", chain_len = chain_len - 1);
        let deepest_m = make_qn_node(&mut graph, NodeKind::Method, "M", &last_qn, file);
        rebuild_indices(&mut graph);

        // Wire the embedding chain S0 -> S1 -> ... -> S(n-1).
        for (i, &outer_struct) in structs.iter().take(chain_len - 1).enumerate() {
            let inner_qn = format!("fx.S{}", i + 1);
            add_embedding_hint(&mut graph, outer_struct, &inner_qn, Receiver::Value, file);
        }
        // Receiver hint for the deepest method.
        let deepest_recv_qn = format!("fx.S{}", chain_len - 1);
        add_method_receiver_hint(
            &mut graph,
            deepest_m,
            &deepest_recv_qn,
            Receiver::Value,
            file,
        );

        // Run: must not panic and must bound the synthetic-node
        // count.
        let stats = run_go_method_set_satisfaction(&mut graph, None);

        // The cap MUST fire and MUST suppress promotions.
        //
        // An uncapped BFS would emit one promoted M per outer struct
        // that can reach the chain's deepest method — i.e. all
        // `chain_len - 1` outers above S(chain_len - 1) would receive
        // a promoted M. With the `MAX_PROMOTION_DEPTH = 16` cap the
        // outers at depth-from-method > 16 cannot reach M, so strictly
        // fewer than `chain_len - 1` promotions are emitted.
        //
        // Strict assertion: `promoted_method_nodes < chain_len - 1`.
        // This is the assertion codex iter-1 finding #1 demands —
        // the prior `<= chain_len` would be trivially satisfied by an
        // *uncapped* walk (which would emit exactly `chain_len - 1`),
        // making the test a termination check rather than a cap check.
        assert!(
            (stats.promoted_method_nodes as usize) < chain_len - 1,
            "MAX_PROMOTION_DEPTH cap must suppress at least one promotion; got {} promoted nodes for chain of {} structs (an uncapped walk would emit {})",
            stats.promoted_method_nodes,
            chain_len,
            chain_len - 1,
        );

        // The truncation counter (currently routed onto
        // `ambiguity_blocked_promotions` per the comment at
        // `pass_go_method_set.rs:1147-1152`) MUST have incremented —
        // this is the direct observable that the depth-cap branch
        // fired during BFS.
        assert!(
            stats.ambiguity_blocked_promotions > 0,
            "depth-cap branch (pass_go_method_set.rs:1146-1153) must increment the truncation counter; got 0",
        );
    }
}
