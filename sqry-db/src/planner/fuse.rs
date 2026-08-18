//! Query plan **fuser** — merge shared scan prefixes across a batch of plans.
//!
//! At 20M+ edges, a graph-wide [`PlanNode::NodeScan`] is the most expensive
//! operator in the planner. When a caller submits multiple queries together
//! (e.g. an MCP tool that issues N relation queries in one round-trip), each
//! query independently re-scans the graph. The fuser eliminates this
//! duplication by grouping plans whose root prefix is identical so that the
//! executor (DB12) evaluates the prefix exactly once and routes the resulting
//! node-set into each tail.
//!
//! # Scope (DB11)
//!
//! - **First-step prefix fusion.** Two plans share a fusion group iff their
//!   root prefix — the first context-free step — is structurally equal. For
//!   [`PlanNode::Chain`], the prefix is `steps[0]`; for a standalone
//!   [`PlanNode::NodeScan`] or [`PlanNode::SetOp`], the prefix *is* the root.
//!   Deeper-prefix fusion (e.g. `[scan, filter_A, filter_B]` vs.
//!   `[scan, filter_A, filter_C]`) is intentionally **not** implemented in
//!   this unit; the spec calls out "shared `NodeScan` prefixes" and the win
//!   from first-step fusion already eliminates the dominant cost.
//! - **Recursive subquery fusion.** Two plans that embed identical
//!   [`PredicateValue::Subquery`] trees share the subquery evaluation. The
//!   fuser walks every predicate in every input plan, deduplicates the
//!   subqueries by `PlanNode` equality, and fuses them into a sibling
//!   [`FusedPlanBatch`] exposed via [`FusedPlanBatch::subquery_batch`]. The
//!   recursion is bounded — subquery batches themselves may carry nested
//!   subquery batches.
//! - **Set-op support.** A standalone [`PlanNode::SetOp`] root is treated
//!   as a single fusion prefix (its operands evaluate together as part of
//!   the prefix). Two plans whose roots are equal `SetOp` trees share that
//!   evaluation.
//! - **Singleton & empty inputs** are handled as identity passes — the
//!   fuser never *removes* a plan, only groups it.
//!
//! # `EdgeKind` metadata (option `(a)` from the design memo)
//!
//! [`PlanNode`] hashes structurally. [`EdgeKind`] carries metadata
//! (e.g. `Calls { argument_count, is_async }`) that participates in equality
//! and therefore in fusion. The query builder (DB10) is contracted to
//! construct [`EdgeKind`] values with **canonical zero / `None` / `false`
//! metadata** so that two semantically identical traversal predicates fuse.
//! The fuser does **not** re-canonicalise — it would force a deep clone of
//! every input plan and would mask bugs in the builder. Callers hand-rolling
//! a [`QueryPlan`] without the builder must follow the same convention.
//!
//! # Determinism
//!
//! Fusion groups are returned in **first-appearance order** of their prefix
//! within the input batch. Tails inside a group are returned in the order
//! their originating plan appeared. This makes test snapshots and structured
//! log output stable across runs.
//!
//! # Idempotence
//!
//! Re-fusing an already-fused batch (via [`FusedPlanBatch::input_plans`])
//! produces a batch with the same `groups`, the same `subquery_batch`, and
//! the same `stats`. See `idempotent_round_trip` in
//! `sqry-db/tests/fusion_test.rs`.
//!
//! # Algorithmic complexity
//!
//! `O(N · D)` where `N` is the number of input plans and `D` is the average
//! plan depth (subquery walking). Grouping uses a [`HashMap`] keyed on the
//! prefix [`PlanNode`]; the IR's structural [`Hash`] derive provides the
//! key-equality property the algorithm relies on.
//!
//! # Public surface
//!
//! - [`fuse_plans`] — top-level batch fuser.
//! - [`fuse_single`] — convenience for a single plan.
//! - [`FusedPlanBatch`] — grouped output, plus per-batch [`FusionStats`].
//! - [`FusionGroup`] — one shared prefix and the tails that joined it.
//! - [`FusedTail`] — one plan's contribution to a group, tagged with its
//!   original index so the executor can route results back.
//! - [`FusionTail`] — what comes after the prefix: nothing, a chain
//!   continuation, or a wrapper variant for cleanly representing originals
//!   that *were* the prefix.
//! - [`FusionStats`] — per-batch counters: total plans, group count, scans
//!   eliminated, subqueries deduplicated.
//!
//! [`PlanNode`]: super::ir::PlanNode
//! [`EdgeKind`]: sqry_core::graph::unified::edge::kind::EdgeKind
//! [`PredicateValue`]: super::ir::PredicateValue

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::ir::{PlanNode, Predicate, PredicateValue, QueryPlan};

// ============================================================================
// Public output types
// ============================================================================

/// Grouped representation of a batch of [`QueryPlan`]s after shared-prefix
/// fusion.
///
/// The executor (DB12) consumes one of these per submission round, evaluates
/// each [`FusionGroup`]'s shared prefix exactly once, and then evaluates each
/// tail against the prefix output. Subqueries — which are themselves
/// [`QueryPlan`]-shaped — are fused recursively into [`Self::subquery_batch`]
/// so the executor can dedup their evaluation just like top-level plans.
///
/// `FusedPlanBatch` derives [`Serialize`] and [`Deserialize`] so it can be
/// included in structured log output and stored alongside cache entries in
/// downstream units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusedPlanBatch {
    /// Top-level fusion groups, in first-appearance order of their prefix.
    groups: Vec<FusionGroup>,
    /// Recursively fused subqueries discovered in any predicate of any tail
    /// inside [`Self::groups`]. `None` if no subqueries were present.
    ///
    /// The executor evaluates this batch first (depth-first), caches each
    /// resulting node-set keyed on the original subquery [`PlanNode`], and
    /// then re-uses those results when it encounters the same subquery
    /// during top-level tail evaluation.
    subquery_batch: Option<Box<FusedPlanBatch>>,
    /// Per-batch fusion statistics.
    stats: FusionStats,
    /// Promoted context-free shared subtrees, sorted so dependencies appear
    /// before dependents.
    #[serde(default)]
    shared_nodes: Vec<SharedNode>,
}

impl FusedPlanBatch {
    /// Returns the fusion groups in first-appearance order.
    #[inline]
    #[must_use]
    pub fn groups(&self) -> &[FusionGroup] {
        &self.groups
    }

    /// Returns the recursively-fused subquery batch, if any subqueries were
    /// present in the input plans.
    #[inline]
    #[must_use]
    pub fn subquery_batch(&self) -> Option<&FusedPlanBatch> {
        self.subquery_batch.as_deref()
    }

    /// Returns the per-batch fusion statistics.
    #[inline]
    #[must_use]
    pub fn stats(&self) -> &FusionStats {
        &self.stats
    }

    /// Returns the promoted shared subtrees for this batch.
    #[inline]
    #[must_use]
    pub fn shared_nodes(&self) -> &[SharedNode] {
        &self.shared_nodes
    }

    /// Returns the number of fusion groups in this batch.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Returns `true` if this batch contains no fusion groups (i.e. an empty
    /// input batch was fused).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Iterates over the fusion groups in first-appearance order.
    pub fn iter_groups(&self) -> impl Iterator<Item = &FusionGroup> {
        self.groups.iter()
    }

    /// Reconstructs the original list of [`QueryPlan`]s that were fused into
    /// this batch, in their original submission order.
    ///
    /// This is the inverse of [`fuse_plans`] and is used to verify
    /// idempotence (`fuse_plans(fused.input_plans()) == fused`) and to drive
    /// round-trip tests.
    ///
    /// # Panics
    ///
    /// Panics if the batch's internal `original_index` bookkeeping is sparse.
    /// [`fuse_plans`] constructs every `FusedPlanBatch` with a contiguous
    /// original-index range, so a panic here indicates corrupted fusion
    /// metadata.
    #[must_use]
    pub fn input_plans(&self) -> Vec<QueryPlan> {
        // Capacity = total tails across all groups.
        let total_tails: usize = self.groups.iter().map(|g| g.tails.len()).sum();
        let mut out: Vec<Option<QueryPlan>> = (0..total_tails).map(|_| None).collect();

        for group in &self.groups {
            for tail in &group.tails {
                let plan = tail.reconstruct(&group.prefix);
                let idx = tail.original_index;
                debug_assert!(idx < out.len(), "original_index out of bounds");
                out[idx] = Some(plan);
            }
        }

        out.into_iter()
            .map(|p| p.expect("every original index must be filled"))
            .collect()
    }

    /// Returns the total number of original plans contained in this batch
    /// (i.e. the sum of tails across all groups).
    #[must_use]
    pub fn total_plans(&self) -> usize {
        self.groups.iter().map(|g| g.tails.len()).sum()
    }
}

/// Stable identifier for a promoted shared subtree within one fused batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SharedNodeId(u32);

impl SharedNodeId {
    #[inline]
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One promoted shared subtree that can be executed once and reused by
/// multiple plans in the same batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNode {
    canonical_plan: PlanNode,
    consumers: Vec<usize>,
    id: SharedNodeId,
}

impl SharedNode {
    /// Returns the canonical subtree plan keyed into the executor cache.
    #[inline]
    #[must_use]
    pub fn canonical_plan(&self) -> &PlanNode {
        &self.canonical_plan
    }

    /// Returns the original plan indices that consume this shared node.
    #[inline]
    #[must_use]
    pub fn consumers(&self) -> &[usize] {
        &self.consumers
    }

    /// Returns this node's batch-local identifier.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> SharedNodeId {
        self.id
    }
}

/// One fusion group: a shared prefix and the tails that share it.
///
/// The prefix is always context-free — that is, it is the first
/// [`PlanNode`] step that can be evaluated without any preceding input set
/// ([`PlanNode::NodeScan`] or [`PlanNode::SetOp`] in current IR; see
/// [`PlanNode::is_context_free`]).
///
/// `tails` is non-empty: every group represents at least one original plan.
/// When `tails.len() > 1`, the executor saves `tails.len() - 1` redundant
/// prefix evaluations.
///
/// [`PlanNode`]: super::ir::PlanNode
/// [`PlanNode::NodeScan`]: super::ir::PlanNode::NodeScan
/// [`PlanNode::SetOp`]: super::ir::PlanNode::SetOp
/// [`PlanNode::is_context_free`]: super::ir::PlanNode::is_context_free
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionGroup {
    /// The shared prefix, evaluated exactly once by the executor.
    prefix: PlanNode,
    /// Tails belonging to this group, in original-submission order.
    tails: Vec<FusedTail>,
}

impl FusionGroup {
    /// Returns the shared prefix.
    #[inline]
    #[must_use]
    pub fn prefix(&self) -> &PlanNode {
        &self.prefix
    }

    /// Returns the tails that share this prefix, in original-submission
    /// order.
    #[inline]
    #[must_use]
    pub fn tails(&self) -> &[FusedTail] {
        &self.tails
    }

    /// Returns the number of original plans that fused into this group.
    #[inline]
    #[must_use]
    pub fn tail_count(&self) -> usize {
        self.tails.len()
    }
}

/// One plan's contribution to a [`FusionGroup`].
///
/// `original_index` lets the executor route each per-tail result back to
/// the submitter using the original submission order. `tail` is the
/// continuation that the executor must apply to the shared prefix output
/// to reconstruct this plan's full evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusedTail {
    /// Index of this tail's originating plan in the input `Vec<QueryPlan>`
    /// passed to [`fuse_plans`].
    pub original_index: usize,
    /// Continuation past the shared prefix. See [`FusionTail`].
    pub tail: FusionTail,
}

impl FusedTail {
    /// Reconstructs the originating [`QueryPlan`] by combining the shared
    /// prefix with this tail's continuation. Used by
    /// [`FusedPlanBatch::input_plans`] for round-trip verification.
    #[must_use]
    pub fn reconstruct(&self, prefix: &PlanNode) -> QueryPlan {
        let root = match &self.tail {
            FusionTail::Identity => prefix.clone(),
            FusionTail::ChainContinuation { remaining_steps } => {
                let mut steps = Vec::with_capacity(remaining_steps.len() + 1);
                steps.push(prefix.clone());
                steps.extend(remaining_steps.iter().cloned());
                PlanNode::Chain { steps }
            }
        };
        QueryPlan::new(root)
    }
}

/// Continuation past the shared prefix.
///
/// Two flavours cover every input shape the IR can produce:
///
/// - [`FusionTail::Identity`] — the original plan was *exactly* the prefix
///   (a standalone [`PlanNode::NodeScan`] or [`PlanNode::SetOp`], or a
///   `Chain` of length 1).
/// - [`FusionTail::ChainContinuation`] — the original plan was a `Chain`
///   whose first step was the prefix; the remaining steps live here and
///   are evaluated in sequence against the prefix output.
///
/// [`PlanNode::NodeScan`]: super::ir::PlanNode::NodeScan
/// [`PlanNode::SetOp`]: super::ir::PlanNode::SetOp
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionTail {
    /// The original plan *was* the prefix; no further steps apply.
    Identity,
    /// The original plan was a [`PlanNode::Chain`]; the prefix was its
    /// first step, and these are the remaining steps in order.
    ///
    /// Always non-empty: a `Chain` whose only step matched the prefix is
    /// represented as [`FusionTail::Identity`] instead, to keep the wire
    /// format and equality checks compact.
    ///
    /// [`PlanNode::Chain`]: super::ir::PlanNode::Chain
    ChainContinuation {
        /// The remaining chain steps, executed in order against the
        /// prefix output.
        remaining_steps: Vec<PlanNode>,
    },
}

/// Per-batch fusion statistics, primarily for telemetry / structured log
/// output and for tests that want to assert savings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FusionStats {
    /// Total number of [`QueryPlan`]s in the input batch.
    pub total_plans: usize,
    /// Number of fusion groups produced. `groups <= total_plans`.
    pub fusion_groups: usize,
    /// Number of redundant prefix evaluations the executor will avoid:
    /// `total_plans - fusion_groups`. Zero for a singleton or fully-distinct
    /// batch.
    pub scans_eliminated: usize,
    /// Total number of [`PredicateValue::Subquery`] occurrences discovered
    /// in any predicate of any input plan. Counts duplicates.
    pub subqueries_total: usize,
    /// Number of distinct subquery [`PlanNode`]s discovered (post-dedup).
    /// `subqueries_unique <= subqueries_total`.
    ///
    /// `subqueries_total - subqueries_unique` is the number of subquery
    /// evaluations the executor avoids by sharing.
    pub subqueries_unique: usize,
    /// Number of promoted shared plan subtrees in this batch.
    pub shared_nodes_promoted: usize,
    /// Structural reduction ratio for the promoted shared-node DAG estimate.
    #[serde(with = "f64_bits")]
    pub plan_tree_reduction_ratio: f64,
}

impl Default for FusionStats {
    fn default() -> Self {
        Self {
            total_plans: 0,
            fusion_groups: 0,
            scans_eliminated: 0,
            subqueries_total: 0,
            subqueries_unique: 0,
            shared_nodes_promoted: 0,
            plan_tree_reduction_ratio: 0.0,
        }
    }
}

impl PartialEq for FusionStats {
    fn eq(&self, other: &Self) -> bool {
        self.total_plans == other.total_plans
            && self.fusion_groups == other.fusion_groups
            && self.scans_eliminated == other.scans_eliminated
            && self.subqueries_total == other.subqueries_total
            && self.subqueries_unique == other.subqueries_unique
            && self.shared_nodes_promoted == other.shared_nodes_promoted
            && self.plan_tree_reduction_ratio.to_bits() == other.plan_tree_reduction_ratio.to_bits()
    }
}

impl Eq for FusionStats {}

mod f64_bits {
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with-module serializers are invoked with a reference to the field"
    )]
    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_f64(*value)
        } else {
            serializer.serialize_u64(value.to_bits())
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            f64::deserialize(deserializer)
        } else {
            u64::deserialize(deserializer).map(f64::from_bits)
        }
    }
}

impl FusionStats {
    /// Returns the number of subquery evaluations avoided by deduplication
    /// (`subqueries_total - subqueries_unique`).
    #[inline]
    #[must_use]
    pub fn subqueries_eliminated(&self) -> usize {
        self.subqueries_total.saturating_sub(self.subqueries_unique)
    }
}

// ============================================================================
// Top-level fusion entry points
// ============================================================================

/// Fuses a batch of [`QueryPlan`]s into a [`FusedPlanBatch`] by grouping
/// plans whose first context-free step is structurally identical.
///
/// See the module-level documentation for the full algorithm, scope, and
/// `EdgeKind` metadata contract.
///
/// # Determinism
///
/// Groups are returned in first-appearance order. Tails within a group
/// preserve their original submission order. The output of `fuse_plans` is
/// therefore deterministic for a given input batch.
///
/// # Empty input
///
/// `fuse_plans(vec![])` returns an empty batch with all-zero
/// [`FusionStats`] and `subquery_batch == None`.
#[must_use]
pub fn fuse_plans(plans: Vec<QueryPlan>) -> FusedPlanBatch {
    let original_plans = plans.clone();
    let total_plans = plans.len();

    // Step 1 — discover & fuse subqueries from every input plan, depth-first.
    // We do this *before* assigning original indices so that the subquery
    // batch is built independently and the top-level group construction can
    // proceed without re-walking predicates.
    let (subqueries_total, subquery_plans) = collect_subquery_plans(&plans);
    let subquery_batch = if subquery_plans.is_empty() {
        None
    } else {
        Some(Box::new(fuse_plans(subquery_plans)))
    };

    // Step 2 — group top-level plans by their context-free prefix.
    //
    // We walk plans in submission order, splitting each into (prefix, tail).
    // The first time we see a prefix, we record its position in `groups`; on
    // subsequent appearances we push another tail into the existing group.
    // `prefix_index` keeps lookups O(1) without disturbing first-appearance
    // ordering of `groups`.
    let mut groups: Vec<FusionGroup> = Vec::new();
    let mut prefix_index: HashMap<PlanNode, usize> = HashMap::new();

    for (original_index, plan) in plans.into_iter().enumerate() {
        let (prefix, tail) = split_prefix_and_tail(plan.root);

        let fused_tail = FusedTail {
            original_index,
            tail,
        };

        // Match the existing group by structural prefix equality.
        if let Some(&idx) = prefix_index.get(&prefix) {
            groups[idx].tails.push(fused_tail);
        } else {
            let group = FusionGroup {
                prefix: prefix.clone(),
                tails: vec![fused_tail],
            };
            prefix_index.insert(prefix, groups.len());
            groups.push(group);
        }
    }

    let fusion_groups = groups.len();
    let scans_eliminated = total_plans.saturating_sub(fusion_groups);
    let promoted_shared_candidates = collect_promoted_candidates(&original_plans, &groups);
    let promoted_shared_nodes = materialize_shared_nodes(&promoted_shared_candidates);
    let shared_nodes_promoted = promoted_shared_nodes.len();

    let subqueries_unique = subquery_batch
        .as_deref()
        .map_or(0, FusedPlanBatch::total_plans);
    let plan_tree_reduction_ratio =
        estimate_plan_tree_reduction_ratio(&original_plans, &promoted_shared_candidates);

    let stats = FusionStats {
        total_plans,
        fusion_groups,
        scans_eliminated,
        subqueries_total,
        subqueries_unique,
        shared_nodes_promoted,
        plan_tree_reduction_ratio,
    };

    FusedPlanBatch {
        groups,
        subquery_batch,
        stats,
        shared_nodes: promoted_shared_nodes,
    }
}

/// Convenience wrapper for a single-plan batch.
///
/// Equivalent to `fuse_plans(vec![plan])`. The returned batch always
/// contains exactly one group with exactly one tail (originating index `0`),
/// and `stats.scans_eliminated == 0`.
#[must_use]
pub fn fuse_single(plan: QueryPlan) -> FusedPlanBatch {
    fuse_plans(vec![plan])
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Splits a root [`PlanNode`] into its context-free prefix and the
/// continuation tail.
///
/// - `Chain { steps }` with at least one step → `(steps[0], ChainContinuation
///   { remaining_steps: steps[1..] })`. If `steps` has exactly one element,
///   the tail collapses to [`FusionTail::Identity`].
/// - Any other root → `(root, Identity)`.
///
/// The first step of a `Chain` is contractually a context-free node
/// (see [`PlanNode::is_context_free`] and the `Chain` documentation in
/// `ir.rs`). Inputs that violate this contract — for example, a `Chain`
/// whose first step is a `Filter` — are *passed through* as a single
/// fusion group: the entire `Chain` becomes the prefix and the tail is
/// [`FusionTail::Identity`]. This keeps fusion total and side-effect-free
/// even for malformed plans; the executor will surface the violation when
/// it tries to evaluate a non-context-free root.
fn split_prefix_and_tail(root: PlanNode) -> (PlanNode, FusionTail) {
    if let PlanNode::Chain { mut steps } = root {
        match steps.len() {
            0 => {
                // An empty chain has no first step. It is its own prefix
                // and produces the empty set per IR docs.
                (PlanNode::Chain { steps }, FusionTail::Identity)
            }
            1 => {
                // Single-step chain: the step is the prefix, no continuation.
                let only = steps.pop().expect("len == 1");
                if only.is_context_free() {
                    (only, FusionTail::Identity)
                } else {
                    // Malformed (non-context-free first step) — pass through
                    // as an identity group on the original Chain so semantics
                    // are preserved verbatim.
                    (PlanNode::Chain { steps: vec![only] }, FusionTail::Identity)
                }
            }
            _ => {
                let first = steps.remove(0);
                if first.is_context_free() {
                    (
                        first,
                        FusionTail::ChainContinuation {
                            remaining_steps: steps,
                        },
                    )
                } else {
                    // Malformed — preserve the original Chain as-is.
                    let mut original = Vec::with_capacity(steps.len() + 1);
                    original.push(first);
                    original.extend(steps);
                    (PlanNode::Chain { steps: original }, FusionTail::Identity)
                }
            }
        }
    } else {
        // NodeScan, SetOp → context-free root, fuse on the whole node.
        // EdgeTraversal, Filter as a root violate the IR contract; we still
        // fuse on the whole node, mirroring the malformed-Chain behaviour.
        (root, FusionTail::Identity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum PathSegment {
    ChainStep(usize),
    ChainPrefix(usize),
    SetLeft,
    SetRight,
    PredicateValueSubquery,
    PredicateAnd(usize),
    PredicateOr(usize),
    PredicateNot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
struct SubtreePath(Vec<PathSegment>);

impl SubtreePath {
    fn push(&mut self, segment: PathSegment) {
        self.0.push(segment);
    }

    fn pop(&mut self) {
        self.0.pop();
    }
}

#[derive(Debug, Default)]
struct SharedSubtreeCollector {
    candidates: HashMap<PlanNode, Vec<(usize, SubtreePath)>>,
}

impl SharedSubtreeCollector {
    fn register(&mut self, plan_index: usize, path: &SubtreePath, plan: PlanNode) {
        self.candidates
            .entry(plan)
            .or_default()
            .push((plan_index, path.clone()));
    }
}

#[derive(Debug, Clone)]
struct PromotedCandidate {
    canonical_plan: PlanNode,
    positions: Vec<(usize, SubtreePath)>,
}

fn collect_promoted_candidates(
    plans: &[QueryPlan],
    groups: &[FusionGroup],
) -> Vec<PromotedCandidate> {
    let mut collector = SharedSubtreeCollector::default();

    for (plan_index, plan) in plans.iter().enumerate() {
        let mut path = SubtreePath::default();
        walk_plan_for_shared_subtrees(&plan.root, plan_index, &mut path, &mut collector);
    }

    let promoted = promote_candidates(collector.candidates, groups);
    sort_promoted_candidates_by_containment(promoted)
}

fn materialize_shared_nodes(candidates: &[PromotedCandidate]) -> Vec<SharedNode> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| SharedNode {
            canonical_plan: candidate.canonical_plan.clone(),
            consumers: candidate_consumers(&candidate.positions),
            id: SharedNodeId(u32::try_from(index).unwrap_or(u32::MAX)),
        })
        .collect()
}

fn is_independently_executable_root(plan: &PlanNode) -> bool {
    match plan {
        PlanNode::NodeScan { .. } | PlanNode::SetOp { .. } => true,
        PlanNode::Chain { steps } => steps.first().is_some_and(PlanNode::is_context_free),
        PlanNode::Filter { .. } | PlanNode::EdgeTraversal { .. } => false,
    }
}

fn walk_plan_for_shared_subtrees(
    plan: &PlanNode,
    plan_index: usize,
    path: &mut SubtreePath,
    collector: &mut SharedSubtreeCollector,
) {
    if is_independently_executable_root(plan) {
        collector.register(plan_index, path, plan.clone());
    }

    match plan {
        PlanNode::NodeScan { .. } | PlanNode::EdgeTraversal { .. } => {}
        PlanNode::Chain { steps } => {
            register_executable_chain_prefixes(steps, plan_index, path, collector);
            for (step_index, step) in steps.iter().enumerate() {
                path.push(PathSegment::ChainStep(step_index));
                walk_plan_for_shared_subtrees(step, plan_index, path, collector);
                path.pop();
            }
        }
        PlanNode::SetOp { left, right, .. } => {
            path.push(PathSegment::SetLeft);
            walk_plan_for_shared_subtrees(left, plan_index, path, collector);
            path.pop();

            path.push(PathSegment::SetRight);
            walk_plan_for_shared_subtrees(right, plan_index, path, collector);
            path.pop();
        }
        PlanNode::Filter { predicate } => {
            walk_predicate_for_shared_subtrees(predicate, plan_index, path, collector);
        }
    }
}

fn register_executable_chain_prefixes(
    steps: &[PlanNode],
    plan_index: usize,
    path: &mut SubtreePath,
    collector: &mut SharedSubtreeCollector,
) {
    if !steps.first().is_some_and(PlanNode::is_context_free) {
        return;
    }

    for prefix_len in 2..steps.len() {
        path.push(PathSegment::ChainPrefix(prefix_len));
        collector.register(
            plan_index,
            path,
            PlanNode::Chain {
                steps: steps[..prefix_len].to_vec(),
            },
        );
        path.pop();
    }
}

fn walk_predicate_for_shared_subtrees(
    pred: &Predicate,
    plan_index: usize,
    path: &mut SubtreePath,
    collector: &mut SharedSubtreeCollector,
) {
    match pred {
        Predicate::HasCaller
        | Predicate::HasCallee
        | Predicate::IsUnused
        | Predicate::IsDefinition(_)
        | Predicate::IsUnsafe(_)
        // Phase A (U14) leaf predicates — atomic, no nested PlanNode.
        | Predicate::IsAddressTaken(_)
        | Predicate::ResolvedVia(_)
        | Predicate::HasCallsitePromiscuous(_)
        // Phase β joint-stubs — atomic leaf predicates with no nested PlanNode.
        | Predicate::FrameworkEq(_)
        | Predicate::ResolvedViaEq(_)
        | Predicate::ShapeSimilar(_)
        | Predicate::InFile(_)
        | Predicate::InScope(_)
        | Predicate::MatchesName(_)
        | Predicate::Returns(_)
        | Predicate::CfgCondition(_)
        | Predicate::Wraps(_) => {}
        Predicate::Callers(value)
        | Predicate::Callees(value)
        | Predicate::Imports(value)
        | Predicate::Exports(value)
        | Predicate::References(value)
        | Predicate::Implements(value) => {
            walk_predicate_value_for_shared_subtrees(value, plan_index, path, collector);
        }
        Predicate::And(list) => {
            for (index, inner) in list.iter().enumerate() {
                path.push(PathSegment::PredicateAnd(index));
                walk_predicate_for_shared_subtrees(inner, plan_index, path, collector);
                path.pop();
            }
        }
        Predicate::Or(list) => {
            for (index, inner) in list.iter().enumerate() {
                path.push(PathSegment::PredicateOr(index));
                walk_predicate_for_shared_subtrees(inner, plan_index, path, collector);
                path.pop();
            }
        }
        Predicate::Not(inner) => {
            path.push(PathSegment::PredicateNot);
            walk_predicate_for_shared_subtrees(inner, plan_index, path, collector);
            path.pop();
        }
    }
}

fn walk_predicate_value_for_shared_subtrees(
    value: &PredicateValue,
    plan_index: usize,
    path: &mut SubtreePath,
    collector: &mut SharedSubtreeCollector,
) {
    if let PredicateValue::Subquery(plan) = value {
        path.push(PathSegment::PredicateValueSubquery);
        walk_plan_for_shared_subtrees(plan, plan_index, path, collector);
        path.pop();
    }
}

fn promote_candidates(
    candidates: HashMap<PlanNode, Vec<(usize, SubtreePath)>>,
    groups: &[FusionGroup],
) -> Vec<PromotedCandidate> {
    let existing_prefixes: HashSet<PlanNode> =
        groups.iter().map(|group| group.prefix.clone()).collect();
    let mut promoted: Vec<PromotedCandidate> = candidates
        .into_iter()
        .filter_map(|(canonical_plan, mut positions)| {
            if positions.len() < 2 || existing_prefixes.contains(&canonical_plan) {
                return None;
            }

            positions
                .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
            Some(PromotedCandidate {
                canonical_plan,
                positions,
            })
        })
        .collect();

    promoted.sort_by(|left, right| {
        left.positions[0]
            .0
            .cmp(&right.positions[0].0)
            .then_with(|| left.positions[0].1.cmp(&right.positions[0].1))
            .then_with(|| {
                left.canonical_plan
                    .operator_count()
                    .cmp(&right.canonical_plan.operator_count())
            })
    });

    promoted
}

fn candidate_consumers(positions: &[(usize, SubtreePath)]) -> Vec<usize> {
    let mut consumers = Vec::new();
    for (plan_index, _) in positions {
        if !consumers.contains(plan_index) {
            consumers.push(*plan_index);
        }
    }
    consumers
}

fn sort_promoted_candidates_by_containment(
    candidates: Vec<PromotedCandidate>,
) -> Vec<PromotedCandidate> {
    if candidates.len() <= 1 {
        return candidates;
    }

    let mut outgoing: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut indegree = vec![0_usize; candidates.len()];

    for (parent_index, parent) in candidates.iter().enumerate() {
        for (child_index, child) in candidates.iter().enumerate() {
            if parent_index == child_index {
                continue;
            }
            if is_proper_subtree(&child.canonical_plan, &parent.canonical_plan) {
                outgoing.entry(child_index).or_default().push(parent_index);
                indegree[parent_index] += 1;
            }
        }
    }

    let mut ready: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(candidates.len());

    while let Some(index) = ready.pop_front() {
        order.push(index);
        if let Some(edges) = outgoing.get(&index) {
            for &dependent in edges {
                indegree[dependent] -= 1;
                if indegree[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }
    }

    if order.len() != candidates.len() {
        return candidates;
    }

    order
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect()
}

fn is_proper_subtree(needle: &PlanNode, haystack: &PlanNode) -> bool {
    let mut found = false;
    visit_proper_plan_subtrees(haystack, &mut |candidate| {
        if !found && candidate == needle {
            found = true;
        }
    });
    found
}

fn visit_proper_plan_subtrees(plan: &PlanNode, visitor: &mut dyn FnMut(&PlanNode)) {
    match plan {
        PlanNode::NodeScan { .. } | PlanNode::EdgeTraversal { .. } => {}
        PlanNode::Filter { predicate } => {
            visit_proper_predicate_subtrees(predicate, visitor);
        }
        PlanNode::SetOp { left, right, .. } => {
            visitor(left);
            visit_proper_plan_subtrees(left, visitor);
            visitor(right);
            visit_proper_plan_subtrees(right, visitor);
        }
        PlanNode::Chain { steps } => {
            for prefix_len in 2..steps.len() {
                let prefix = PlanNode::Chain {
                    steps: steps[..prefix_len].to_vec(),
                };
                visitor(&prefix);
            }

            for step in steps {
                visitor(step);
                visit_proper_plan_subtrees(step, visitor);
            }
        }
    }
}

fn visit_proper_predicate_subtrees(predicate: &Predicate, visitor: &mut dyn FnMut(&PlanNode)) {
    match predicate {
        Predicate::HasCaller
        | Predicate::HasCallee
        | Predicate::IsUnused
        | Predicate::IsDefinition(_)
        | Predicate::IsUnsafe(_)
        // Phase A (U14) leaf predicates — atomic, no nested PlanNode.
        | Predicate::IsAddressTaken(_)
        | Predicate::ResolvedVia(_)
        | Predicate::HasCallsitePromiscuous(_)
        // Phase β joint-stubs — atomic leaf predicates with no nested PlanNode.
        | Predicate::FrameworkEq(_)
        | Predicate::ResolvedViaEq(_)
        | Predicate::ShapeSimilar(_)
        | Predicate::InFile(_)
        | Predicate::InScope(_)
        | Predicate::MatchesName(_)
        | Predicate::Returns(_)
        | Predicate::CfgCondition(_)
        | Predicate::Wraps(_) => {}
        Predicate::Callers(value)
        | Predicate::Callees(value)
        | Predicate::Imports(value)
        | Predicate::Exports(value)
        | Predicate::References(value)
        | Predicate::Implements(value) => {
            if let PredicateValue::Subquery(plan) = value {
                visitor(plan);
                visit_proper_plan_subtrees(plan, visitor);
            }
        }
        Predicate::And(list) | Predicate::Or(list) => {
            for inner in list {
                visit_proper_predicate_subtrees(inner, visitor);
            }
        }
        Predicate::Not(inner) => {
            visit_proper_predicate_subtrees(inner, visitor);
        }
    }
}

fn estimate_plan_tree_reduction_ratio(
    plans: &[QueryPlan],
    candidates: &[PromotedCandidate],
) -> f64 {
    let total_nodes_before: usize = plans.iter().map(|plan| plan.root.operator_count()).sum();

    if total_nodes_before == 0 {
        return 0.0;
    }

    let total_saved_nodes: usize = candidates
        .iter()
        .map(|candidate| {
            candidate
                .canonical_plan
                .operator_count()
                .saturating_mul(candidate.positions.len().saturating_sub(1))
        })
        .sum();

    let bounded_saved_nodes = total_saved_nodes.min(total_nodes_before);
    let saved = u32::try_from(bounded_saved_nodes).unwrap_or(u32::MAX);
    let total = u32::try_from(total_nodes_before).unwrap_or(u32::MAX);
    f64::from(saved) / f64::from(total)
}

/// Walks the entire input batch and collects every
/// [`PredicateValue::Subquery`] inner [`PlanNode`] in submission order.
///
/// Returns `(total_count, deduplicated_plans)`:
///
/// - `total_count` includes duplicates (one increment per occurrence,
///   counted across all plans, all predicates, all nesting levels of
///   boolean combinators).
/// - `deduplicated_plans` is a `Vec<QueryPlan>` containing each *distinct*
///   subquery once, in first-appearance order. Wrapping each subquery in a
///   [`QueryPlan`] lets us recurse with [`fuse_plans`] uniformly.
///
/// First-appearance order is preserved so that the recursive
/// [`FusedPlanBatch`] returned by the caller is deterministic.
fn collect_subquery_plans(plans: &[QueryPlan]) -> (usize, Vec<QueryPlan>) {
    let mut total = 0_usize;
    let mut seen: HashSet<PlanNode> = HashSet::new();
    let mut ordered: Vec<PlanNode> = Vec::new();

    for plan in plans {
        walk_plan_for_subqueries(&plan.root, &mut total, &mut seen, &mut ordered);
    }

    let dedup_plans = ordered.into_iter().map(QueryPlan::new).collect();
    (total, dedup_plans)
}

/// Recursively walks a [`PlanNode`] tree, dispatching into every
/// [`Predicate`] embedded in [`PlanNode::Filter`] (and nested through
/// [`PlanNode::SetOp`] / [`PlanNode::Chain`]) and forwarding any discovered
/// subquery [`PlanNode`] both to the running total and to the dedup table.
fn walk_plan_for_subqueries(
    node: &PlanNode,
    total: &mut usize,
    seen: &mut HashSet<PlanNode>,
    ordered: &mut Vec<PlanNode>,
) {
    match node {
        PlanNode::NodeScan { .. } | PlanNode::EdgeTraversal { .. } => {}
        PlanNode::Filter { predicate } => {
            walk_predicate_for_subqueries(predicate, total, seen, ordered);
        }
        PlanNode::SetOp { left, right, .. } => {
            walk_plan_for_subqueries(left, total, seen, ordered);
            walk_plan_for_subqueries(right, total, seen, ordered);
        }
        PlanNode::Chain { steps } => {
            for step in steps {
                walk_plan_for_subqueries(step, total, seen, ordered);
            }
        }
    }
}

/// Recursively walks a [`Predicate`] tree, recording every
/// [`PredicateValue::Subquery`] occurrence and forwarding the inner plan
/// into the dedup table for later [`fuse_plans`] recursion.
fn walk_predicate_for_subqueries(
    pred: &Predicate,
    total: &mut usize,
    seen: &mut HashSet<PlanNode>,
    ordered: &mut Vec<PlanNode>,
) {
    match pred {
        Predicate::HasCaller
        | Predicate::HasCallee
        | Predicate::IsUnused
        | Predicate::IsDefinition(_)
        | Predicate::IsUnsafe(_)
        // Phase A (U14) leaf predicates — atomic, no nested PlanNode.
        | Predicate::IsAddressTaken(_)
        | Predicate::ResolvedVia(_)
        | Predicate::HasCallsitePromiscuous(_)
        // Phase β joint-stubs — atomic leaf predicates with no nested PlanNode.
        | Predicate::FrameworkEq(_)
        | Predicate::ResolvedViaEq(_)
        | Predicate::ShapeSimilar(_)
        | Predicate::InFile(_)
        | Predicate::InScope(_)
        | Predicate::MatchesName(_)
        | Predicate::Returns(_)
        | Predicate::CfgCondition(_)
        | Predicate::Wraps(_) => {}
        Predicate::Callers(v)
        | Predicate::Callees(v)
        | Predicate::Imports(v)
        | Predicate::Exports(v)
        | Predicate::References(v)
        | Predicate::Implements(v) => {
            walk_predicate_value_for_subqueries(v, total, seen, ordered);
        }
        Predicate::And(list) | Predicate::Or(list) => {
            for inner in list {
                walk_predicate_for_subqueries(inner, total, seen, ordered);
            }
        }
        Predicate::Not(inner) => {
            walk_predicate_for_subqueries(inner, total, seen, ordered);
        }
    }
}

/// Inspects a [`PredicateValue`]; if it is a subquery, increments the
/// running total, deduplicates against `seen`, and records first
/// appearance into `ordered`.
///
/// The current subquery is recorded **before** the recursion descends
/// into its inner plan. This preserves outer-to-inner first-appearance
/// ordering in the deduplicated subquery list, which matches reading
/// order in the source plan and makes test snapshots stable.
///
/// The subquery's *inner* plan is then recursed into so nested
/// subqueries (a subquery whose predicate references another subquery)
/// are surfaced to the same dedup table at this level — this is what
/// makes recursion through [`fuse_plans`] both correct and bounded:
/// every nesting level is exposed to the dedup table at the *current*
/// recursion depth, so the recursive call in [`fuse_plans`] only needs
/// to walk one structural layer at a time.
fn walk_predicate_value_for_subqueries(
    value: &PredicateValue,
    total: &mut usize,
    seen: &mut HashSet<PlanNode>,
    ordered: &mut Vec<PlanNode>,
) {
    if let PredicateValue::Subquery(inner) = value {
        *total += 1;
        // Record the current subquery first (outer-to-inner ordering).
        let key: PlanNode = (**inner).clone();
        if seen.insert(key.clone()) {
            ordered.push(key);
        }
        // Then recurse into the inner plan to surface deeper subqueries.
        walk_plan_for_subqueries(inner, total, seen, ordered);
    }
}

// ============================================================================
// Inline unit tests for internal helpers
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::ir::{Direction, MatchMode, PathPattern, SetOperation, StringPattern};
    use sqry_core::graph::unified::node::kind::NodeKind;

    fn make_scan(kind: NodeKind) -> PlanNode {
        PlanNode::NodeScan {
            kind: Some(kind),
            visibility: None,
            name_pattern: None,
        }
    }

    fn make_filter_has_caller() -> PlanNode {
        PlanNode::Filter {
            predicate: Predicate::HasCaller,
        }
    }

    #[test]
    fn split_chain_with_multiple_steps() {
        let chain = PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Function),
                make_filter_has_caller(),
                PlanNode::EdgeTraversal {
                    direction: Direction::Forward,
                    edge_kind: None,
                    max_depth: 1,
                    resolved_via: None,
                },
            ],
        };
        let (prefix, tail) = split_prefix_and_tail(chain);
        assert_eq!(prefix, make_scan(NodeKind::Function));
        match tail {
            FusionTail::ChainContinuation { remaining_steps } => {
                assert_eq!(remaining_steps.len(), 2);
            }
            FusionTail::Identity => panic!("expected ChainContinuation"),
        }
    }

    #[test]
    fn split_chain_with_one_step_collapses_to_identity() {
        let chain = PlanNode::Chain {
            steps: vec![make_scan(NodeKind::Class)],
        };
        let (prefix, tail) = split_prefix_and_tail(chain);
        assert_eq!(prefix, make_scan(NodeKind::Class));
        assert_eq!(tail, FusionTail::Identity);
    }

    #[test]
    fn split_standalone_scan_is_identity() {
        let scan = make_scan(NodeKind::Method);
        let (prefix, tail) = split_prefix_and_tail(scan.clone());
        assert_eq!(prefix, scan);
        assert_eq!(tail, FusionTail::Identity);
    }

    #[test]
    fn split_standalone_setop_is_identity() {
        let set = PlanNode::SetOp {
            op: SetOperation::Union,
            left: Box::new(make_scan(NodeKind::Function)),
            right: Box::new(make_scan(NodeKind::Method)),
        };
        let (prefix, tail) = split_prefix_and_tail(set.clone());
        assert_eq!(prefix, set);
        assert_eq!(tail, FusionTail::Identity);
    }

    #[test]
    fn split_malformed_chain_with_filter_first_passes_through() {
        let chain = PlanNode::Chain {
            steps: vec![make_filter_has_caller(), make_scan(NodeKind::Function)],
        };
        let original = chain.clone();
        let (prefix, tail) = split_prefix_and_tail(chain);
        assert_eq!(prefix, original);
        assert_eq!(tail, FusionTail::Identity);
    }

    #[test]
    fn split_empty_chain_passes_through() {
        let chain = PlanNode::Chain { steps: vec![] };
        let (prefix, tail) = split_prefix_and_tail(chain.clone());
        assert_eq!(prefix, chain);
        assert_eq!(tail, FusionTail::Identity);
    }

    #[test]
    fn collect_subquery_plans_empty_when_no_filters() {
        let plans = vec![
            QueryPlan::new(make_scan(NodeKind::Function)),
            QueryPlan::new(make_scan(NodeKind::Method)),
        ];
        let (total, dedup) = collect_subquery_plans(&plans);
        assert_eq!(total, 0);
        assert!(dedup.is_empty());
    }

    #[test]
    fn collect_subquery_plans_dedupes_identical_subqueries() {
        let inner = make_scan(NodeKind::Method);
        let pred = |v: PlanNode| Predicate::Callers(PredicateValue::Subquery(Box::new(v)));

        let plan_a = QueryPlan::new(PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Function),
                PlanNode::Filter {
                    predicate: pred(inner.clone()),
                },
            ],
        });
        let plan_b = QueryPlan::new(PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Class),
                PlanNode::Filter {
                    predicate: pred(inner.clone()),
                },
            ],
        });

        let (total, dedup) = collect_subquery_plans(&[plan_a, plan_b]);
        assert_eq!(total, 2);
        assert_eq!(dedup.len(), 1);
        assert_eq!(dedup[0].root(), &inner);
    }

    #[test]
    fn collect_subquery_plans_walks_all_predicate_arms() {
        // Build one plan that drops a subquery into every predicate arm
        // that accepts a value, plus a nested combinator. Verify the
        // running total counts each occurrence exactly once.
        let inner_a = make_scan(NodeKind::Function);
        let inner_b = make_scan(NodeKind::Method);

        let sub_a = || PredicateValue::Subquery(Box::new(inner_a.clone()));
        let sub_b = || PredicateValue::Subquery(Box::new(inner_b.clone()));

        let predicate = Predicate::And(vec![
            Predicate::Or(vec![
                Predicate::Callers(sub_a()),
                Predicate::Callees(sub_a()),
                Predicate::Imports(sub_b()),
                Predicate::Exports(sub_b()),
                Predicate::References(sub_a()),
                Predicate::Implements(sub_b()),
            ]),
            Predicate::Not(Box::new(Predicate::Callers(sub_a()))),
        ]);

        let plan = QueryPlan::new(PlanNode::Chain {
            steps: vec![make_scan(NodeKind::Class), PlanNode::Filter { predicate }],
        });
        let (total, dedup) = collect_subquery_plans(&[plan]);
        assert_eq!(total, 7);
        // Exactly two unique subquery PlanNodes (inner_a, inner_b).
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn collect_subquery_plans_recurses_into_nested_subqueries() {
        // A subquery whose predicate references another subquery.
        let leaf = make_scan(NodeKind::Function);
        let nested_pred = Predicate::Callers(PredicateValue::Subquery(Box::new(leaf.clone())));
        let mid_plan = PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Method),
                PlanNode::Filter {
                    predicate: nested_pred,
                },
            ],
        };
        let outer = QueryPlan::new(PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Class),
                PlanNode::Filter {
                    predicate: Predicate::Callees(PredicateValue::Subquery(Box::new(mid_plan))),
                },
            ],
        });

        let (total, dedup) = collect_subquery_plans(&[outer]);
        // Two subquery occurrences total (the outer Callees + the inner
        // Callers inside the mid-plan).
        assert_eq!(total, 2);
        // Two distinct inner plans (mid_plan, leaf).
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn fused_tail_reconstruct_identity() {
        let scan = make_scan(NodeKind::Function);
        let tail = FusedTail {
            original_index: 0,
            tail: FusionTail::Identity,
        };
        let plan = tail.reconstruct(&scan);
        assert_eq!(plan.root(), &scan);
    }

    #[test]
    fn fused_tail_reconstruct_chain_continuation() {
        let scan = make_scan(NodeKind::Function);
        let f = make_filter_has_caller();
        let tail = FusedTail {
            original_index: 7,
            tail: FusionTail::ChainContinuation {
                remaining_steps: vec![f.clone()],
            },
        };
        let plan = tail.reconstruct(&scan);
        match plan.root() {
            PlanNode::Chain { steps } => {
                assert_eq!(steps.len(), 2);
                assert_eq!(&steps[0], &scan);
                assert_eq!(&steps[1], &f);
            }
            other => panic!("expected Chain, got {other:?}"),
        }
    }

    #[test]
    fn fusion_stats_subqueries_eliminated() {
        let stats = FusionStats {
            total_plans: 5,
            fusion_groups: 3,
            scans_eliminated: 2,
            subqueries_total: 7,
            subqueries_unique: 3,
            shared_nodes_promoted: 0,
            plan_tree_reduction_ratio: 0.0,
        };
        assert_eq!(stats.subqueries_eliminated(), 4);
    }

    #[test]
    fn fusion_stats_subqueries_eliminated_saturates() {
        // Defensive: the constructor never produces unique > total, but
        // the helper should not panic even if it does.
        let stats = FusionStats {
            total_plans: 0,
            fusion_groups: 0,
            scans_eliminated: 0,
            subqueries_total: 1,
            subqueries_unique: 5,
            shared_nodes_promoted: 0,
            plan_tree_reduction_ratio: 0.0,
        };
        assert_eq!(stats.subqueries_eliminated(), 0);
    }

    #[test]
    fn fuse_single_round_trip() {
        let scan = make_scan(NodeKind::Function);
        let plan = QueryPlan::new(scan.clone());
        let batch = fuse_single(plan.clone());
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.stats().total_plans, 1);
        assert_eq!(batch.stats().scans_eliminated, 0);
        let recovered = batch.input_plans();
        assert_eq!(recovered, vec![plan]);
    }

    #[test]
    fn helpers_do_not_double_count_distinct_pattern_arms() {
        // Pattern and Regex predicate values must not contribute to
        // subquery totals.
        let predicate = Predicate::And(vec![
            Predicate::Callers(PredicateValue::Pattern(StringPattern::glob("foo*"))),
            Predicate::References(PredicateValue::Pattern(StringPattern {
                raw: "bar".into(),
                mode: MatchMode::Exact,
                case_insensitive: true,
            })),
            Predicate::InFile(PathPattern::new("src/**")),
        ]);
        let plan = QueryPlan::new(PlanNode::Chain {
            steps: vec![
                make_scan(NodeKind::Function),
                PlanNode::Filter { predicate },
            ],
        });
        let (total, dedup) = collect_subquery_plans(&[plan]);
        assert_eq!(total, 0);
        assert!(dedup.is_empty());
    }
}
