//! Plan executor — walks a [`QueryPlan`] tree and produces a sorted, deduplicated
//! `Vec<NodeId>`.
//!
//! # Pipeline position
//!
//! ```text
//!   text syntax / builder API
//!         │
//!         ▼
//!     [ir]                                         (DB09)
//!         │
//!         ▼
//!   [compile]  ── QueryBuilder materialises the IR (DB10)
//!         │
//!         ▼
//!     [fuse]   ── merge shared NodeScan prefixes   (DB11)
//!         │
//!         ▼
//!   [execute]  ← THIS MODULE                       (DB12)
//!         │
//!         ▼
//!     Vec<NodeId>
//! ```
//!
//! # Public surface
//!
//! - [`execute_plan`] — evaluate a single [`QueryPlan`] against a [`QueryDb`].
//! - [`execute_batch`] — evaluate a [`FusedPlanBatch`] and scatter per-tail
//!   results back into submission order.
//! - [`PlanExecutor`] — lower-level entry point when the caller wants to
//!   evaluate multiple plans while sharing the batch-local shared-node cache.
//!
//! # Semantics
//!
//! - All output node-sets are **sorted and deduplicated** by
//!   [`NodeId`]'s `(index, generation)` ordering. This makes results stable
//!   across runs and makes [`SetOperation`] implementations trivially correct.
//! - `NodeScan` uses the pre-built [`AuxiliaryIndices::by_kind`] slice when a
//!   [`NodeKind`] filter is present; otherwise it falls back to iterating the
//!   arena.
//! - `EdgeTraversal` performs a bounded BFS up to `max_depth` hops. `max_depth
//!   == 0` short-circuits to the empty set (the builder rejects this at
//!   construction time; the guard is here for defence-in-depth when callers
//!   hand-roll [`QueryPlan`] values). Seed nodes are **excluded** from the
//!   output — the traversal yields only nodes reached by at least one hop.
//! - `Filter` applies a [`Predicate`] per input node. Boolean combinators
//!   short-circuit.
//! - `SetOp::Union` concatenates and deduplicates. `Intersect` and
//!   `Difference` evaluate the right-hand side, build a [`HashSet`] of it, and
//!   stream the left-hand side against the set.
//! - `Chain` threads each step's output into the next step's input. The first
//!   step must be context-free (IR contract — validated by [`QueryBuilder`]).
//!
//! # Shared-node caching
//!
//! [`PlanExecutor`] memoises every promoted shared subtree and subquery
//! [`PlanNode`] it evaluates by structural equality. This mirrors the
//! fuser's deduplication contract: identical executable subtrees share a
//! single evaluation. The cache is per-executor; callers that want to share
//! across plan submissions should use [`execute_batch`] or manually drive
//! [`PlanExecutor`].
//!
//! # Dependency tracking
//!
//! When called from inside a [`DependencyRecorderGuard`] scope, the executor
//! records every [`FileId`] it reads via [`record_file_dep`]. Outside a guard
//! (standalone `execute_plan` call), the recorder is a no-op — the thread-local
//! simply accumulates into a vector that is never drained.
//!
//! # Design references
//!
//! - Spec: `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md` (§3 — Execution)
//! - DAG: `docs/superpowers/plans/2026-04-12-phase3-4-combined-implementation-dag.toml` (unit DB12)
//!
//! [`AuxiliaryIndices::by_kind`]: sqry_core::graph::unified::storage::indices::AuxiliaryIndices::by_kind
//! [`DependencyRecorderGuard`]: crate::dependency::DependencyRecorderGuard
//! [`QueryBuilder`]: super::compile::QueryBuilder

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use globset::GlobBuilder;

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::edge::store::StoreEdgeRef;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::schema::Visibility;

use super::fuse::{FusedPlanBatch, FusionTail};
use super::ir::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan,
    SetOperation, StringPattern,
};
use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::queries::relation::{RelationKey, RelationKind, relation_matches_node_via_set};
use crate::queries::{
    CalleesQuery, CallersQuery, ExportsQuery, ImplementsQuery, ImportsQuery, ReferencesQuery,
};

// ============================================================================
// Public entry points
// ============================================================================

/// Evaluates a single [`QueryPlan`] and returns the sorted, deduplicated set
/// of matching [`NodeId`]s.
///
/// A fresh [`PlanExecutor`] is allocated for this call; subquery results
/// cached during evaluation are discarded on return. For evaluating multiple
/// plans that may share subqueries, prefer [`execute_batch`] or construct a
/// [`PlanExecutor`] directly.
#[must_use]
pub fn execute_plan(plan: &QueryPlan, db: &QueryDb) -> Vec<NodeId> {
    let mut executor = PlanExecutor::new(db);
    executor.run(&plan.root, None).as_ref().clone()
}

/// Evaluates a [`FusedPlanBatch`] produced by [`super::fuse::fuse_plans`].
///
/// Each fusion group's shared prefix is evaluated exactly once and routed
/// into every tail, avoiding the redundant-scan cost that motivated DB11.
/// Subqueries detected during fusion are evaluated *first* (priming the
/// shared-node cache) so that top-level tail predicates referencing them hit
/// the cache instead of re-evaluating.
///
/// The returned `Vec<Vec<NodeId>>` is indexed by the original submission
/// order — `out[i]` corresponds to `plans[i]` in the input batch given to
/// [`super::fuse::fuse_plans`].
#[must_use]
pub fn execute_batch(batch: &FusedPlanBatch, db: &QueryDb) -> Vec<Vec<NodeId>> {
    let mut executor = PlanExecutor::new(db);
    executor.prime_subqueries(batch);
    executor.prime_shared_nodes(batch);

    let total = batch.total_plans();
    let mut out: Vec<Vec<NodeId>> = vec![Vec::new(); total];

    for group in batch.groups() {
        let prefix_result = executor.run(group.prefix(), None);
        for fused in group.tails() {
            let tail_result: Arc<Vec<NodeId>> = match &fused.tail {
                FusionTail::Identity => Arc::clone(&prefix_result),
                FusionTail::ChainContinuation { remaining_steps } => executor
                    .run_chain_continuation(
                        group.prefix(),
                        Arc::clone(&prefix_result),
                        remaining_steps,
                    ),
            };
            // Defence-in-depth: preserve idempotence even for malformed batches
            // with duplicated original_index values.
            if let Some(slot) = out.get_mut(fused.original_index) {
                slot.clone_from(tail_result.as_ref());
            }
        }
    }

    out
}

// ============================================================================
// PlanExecutor
// ============================================================================

/// Stateful executor for walking [`PlanNode`] trees.
///
/// Holds a reference to the [`QueryDb`] (reserved for DB14+ when relation
/// predicate evaluation is routed through [`QueryDb::get`]), a strong [`Arc`]
/// to the [`GraphSnapshot`] taken at construction time (so subsequent
/// snapshot swaps do not race with evaluation), and a shared-node result
/// cache keyed on the structural [`PlanNode`] of each evaluated subtree.
pub struct PlanExecutor<'db> {
    /// Host database — reserved for DB14+ when relation predicate evaluation
    /// is routed through [`QueryDb::get`].
    #[allow(dead_code)]
    db: &'db QueryDb,
    /// Snapshot view for this evaluation session. Taking the snapshot once at
    /// construction means every step in a multi-step plan sees a consistent
    /// view, even if another thread swaps `db.snapshot()` mid-run.
    snapshot: Arc<GraphSnapshot>,
    /// Cache of shared [`PlanNode`] → result set.
    ///
    /// Keyed on structural equality of the full sub-plan (matching the fuser's
    /// dedup contract), so two predicates carrying the same subquery share a
    /// single evaluation.
    shared_node_cache: HashMap<PlanNode, Arc<Vec<NodeId>>>,
}

impl<'db> PlanExecutor<'db> {
    /// Constructs a new executor bound to the database's current snapshot.
    #[must_use]
    pub fn new(db: &'db QueryDb) -> Self {
        Self {
            db,
            snapshot: db.snapshot_arc(),
            shared_node_cache: HashMap::new(),
        }
    }

    /// Evaluates a [`QueryPlan`] with this executor's cache in scope.
    #[must_use]
    pub fn execute(&mut self, plan: &QueryPlan) -> Vec<NodeId> {
        self.run(&plan.root, None).as_ref().clone()
    }

    /// Pre-populates the shared-node cache from a fused batch's promoted
    /// shared subtrees.
    fn prime_shared_nodes(&mut self, batch: &FusedPlanBatch) {
        for shared_node in batch.shared_nodes() {
            let result = self.run(shared_node.canonical_plan(), None);
            self.shared_node_cache
                .insert(shared_node.canonical_plan().clone(), result);
        }
    }

    /// Pre-populates the shared-node cache from a fused batch's
    /// [`FusedPlanBatch::subquery_batch`] so that top-level tail execution
    /// finds every subquery already cached.
    fn prime_subqueries(&mut self, batch: &FusedPlanBatch) {
        let Some(sub_batch) = batch.subquery_batch() else {
            return;
        };
        // Recurse — nested subqueries are primed before their parents.
        self.prime_subqueries(sub_batch);
        self.prime_shared_nodes(sub_batch);
        for group in sub_batch.groups() {
            let prefix_result = self.run(group.prefix(), None);
            for fused in group.tails() {
                let plan = fused.reconstruct(group.prefix());
                let result: Arc<Vec<NodeId>> = match &fused.tail {
                    FusionTail::Identity => Arc::clone(&prefix_result),
                    FusionTail::ChainContinuation { remaining_steps } => self
                        .run_chain_continuation(
                            group.prefix(),
                            Arc::clone(&prefix_result),
                            remaining_steps,
                        ),
                };
                self.shared_node_cache.insert(plan.root, result);
            }
        }
    }

    /// Core dispatch. `input` is the carry set from an enclosing
    /// [`PlanNode::Chain`]; `None` means the node is being evaluated in a
    /// context-free position (root, set-op operand, subquery).
    fn run(&mut self, node: &PlanNode, input: Option<Arc<Vec<NodeId>>>) -> Arc<Vec<NodeId>> {
        if input.is_none()
            && let Some(existing) = self.shared_node_cache.get(node)
        {
            return Arc::clone(existing);
        }

        match node {
            PlanNode::NodeScan {
                kind,
                visibility,
                name_pattern,
            } => self.run_scan(*kind, *visibility, name_pattern.as_ref()),
            PlanNode::EdgeTraversal {
                direction,
                edge_kind,
                max_depth,
            } => {
                let input = input.unwrap_or_else(|| Arc::new(Vec::new()));
                self.run_traversal(input.as_ref(), *direction, edge_kind.as_ref(), *max_depth)
            }
            PlanNode::Filter { predicate } => {
                let input = input.unwrap_or_else(|| Arc::new(Vec::new()));
                self.run_filter(input.as_ref(), predicate)
            }
            PlanNode::SetOp { op, left, right } => self.run_setop(*op, left, right),
            PlanNode::Chain { steps } => self.run_chain(steps),
        }
    }

    fn run_chain(&mut self, steps: &[PlanNode]) -> Arc<Vec<NodeId>> {
        if steps.is_empty() {
            return Arc::new(Vec::new());
        }
        let prefix = steps[0].clone();
        let current = self.run(&steps[0], None);
        self.run_chain_continuation(&prefix, current, &steps[1..])
    }

    fn run_chain_continuation(
        &mut self,
        prefix: &PlanNode,
        mut current: Arc<Vec<NodeId>>,
        remaining: &[PlanNode],
    ) -> Arc<Vec<NodeId>> {
        let mut current_prefix = prefix.clone();
        let mut remaining_steps = remaining;

        while !remaining_steps.is_empty() {
            if let Some((cached_result, consumed, combined_prefix)) =
                self.lookup_shared_chain_prefix(&current_prefix, remaining_steps)
            {
                current = cached_result;
                current_prefix = combined_prefix;
                remaining_steps = &remaining_steps[consumed..];
                continue;
            }

            current = self.run(&remaining_steps[0], Some(Arc::clone(&current)));
            current_prefix = append_chain_prefix(&current_prefix, &remaining_steps[..1]);
            remaining_steps = &remaining_steps[1..];
        }
        current
    }

    fn lookup_shared_chain_prefix(
        &self,
        prefix: &PlanNode,
        remaining: &[PlanNode],
    ) -> Option<(Arc<Vec<NodeId>>, usize, PlanNode)> {
        for consumed in (1..=remaining.len()).rev() {
            let combined_prefix = append_chain_prefix(prefix, &remaining[..consumed]);
            if let Some(existing) = self.shared_node_cache.get(&combined_prefix) {
                return Some((Arc::clone(existing), consumed, combined_prefix));
            }
        }
        None
    }

    fn run_scan(
        &self,
        kind: Option<NodeKind>,
        visibility: Option<Visibility>,
        name_pattern: Option<&StringPattern>,
    ) -> Arc<Vec<NodeId>> {
        let compiled_name = name_pattern.and_then(CompiledStringPattern::compile);

        let mut out: Vec<NodeId> = Vec::new();
        match kind {
            Some(k) => {
                let ids = self.snapshot.indices().by_kind(k);
                out.reserve(ids.len());
                for &id in ids {
                    if let Some(entry) = self.snapshot.nodes().get(id) {
                        Self::record_entry_deps(entry);
                        if self.scan_match(id, entry, visibility, compiled_name.as_ref()) {
                            out.push(id);
                        }
                    }
                }
            }
            None => {
                for (id, entry) in self.snapshot.nodes().iter() {
                    // Gate 0d iter-2 fix: skip unified losers from
                    // full-snapshot NodeScan. See
                    // `NodeEntry::is_unified_loser`.
                    if entry.is_unified_loser() {
                        continue;
                    }
                    Self::record_entry_deps(entry);
                    if self.scan_match(id, entry, visibility, compiled_name.as_ref()) {
                        out.push(id);
                    }
                }
            }
        }
        dedup_sort(&mut out);
        Arc::new(out)
    }

    /// Apply scan-time predicates to a single node.
    ///
    /// **B1_ALIGN contract.** When `compiled_name` is present, the name
    /// match is checked against **both** `entry.name` and
    /// `entry.qualified_name` (mirroring
    /// [`sqry_core::graph::unified::concurrent::graph::GraphSnapshot::find_by_exact_name`]),
    /// and synthetic placeholder nodes (Go-plugin
    /// `<field:operand.field>` shadows, `<ident>@<offset>`
    /// per-binding-site Variables, `NodeMetadata::Synthetic`-flagged
    /// nodes) are excluded via
    /// [`sqry_core::graph::unified::concurrent::graph::GraphSnapshot::is_node_synthetic`].
    /// This keeps the planner's `name:` predicate set-equal with the
    /// CLI `--exact` shorthand against any fixture.
    ///
    /// When no name pattern is present the synthetic filter is **not**
    /// applied — `kind:function` and similar context-free scans
    /// preserve their existing behaviour (synthetics remain visible to
    /// kind-only scans because the suppression bit is bound to the
    /// name-resolution surface, not the kind index).
    fn scan_match(
        &self,
        node_id: NodeId,
        entry: &NodeEntry,
        visibility: Option<Visibility>,
        compiled_name: Option<&CompiledStringPattern>,
    ) -> bool {
        if let Some(required) = visibility
            && entry_visibility(&self.snapshot, entry) != Some(required)
        {
            return false;
        }
        if let Some(pattern) = compiled_name {
            // Check `entry.name` first (most common hit), then fall
            // back to `entry.qualified_name` so a step like
            // `name:main.SelectorSource.NeedTags` matches Property
            // nodes whose simple name is the bare field but whose
            // qualified name carries the package + receiver prefix.
            let mut matched = false;
            if let Some(name) = self.snapshot.strings().resolve(entry.name)
                && pattern.matches(name.as_ref())
            {
                matched = true;
            }
            if !matched
                && let Some(sid) = entry.qualified_name
                && let Some(qname) = self.snapshot.strings().resolve(sid)
                && pattern.matches(qname.as_ref())
            {
                matched = true;
            }
            if !matched {
                return false;
            }
            // B1_ALIGN: synthetic placeholders are invisible to the
            // `name:` surface so the planner predicate matches the CLI
            // `--exact` shorthand byte-for-byte.
            if self.snapshot.is_node_synthetic(node_id) {
                return false;
            }
        }
        true
    }

    fn run_traversal(
        &self,
        input: &[NodeId],
        direction: Direction,
        edge_kind: Option<&EdgeKind>,
        max_depth: u32,
    ) -> Arc<Vec<NodeId>> {
        if max_depth == 0 || input.is_empty() {
            return Arc::new(Vec::new());
        }
        let target_discriminant = edge_kind.map(std::mem::discriminant);

        let mut visited: HashSet<NodeId> = input.iter().copied().collect();
        let mut result: Vec<NodeId> = Vec::new();
        let mut queue: VecDeque<(NodeId, u32)> = input.iter().map(|&id| (id, 0_u32)).collect();

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for edge in self.neighbours(current, direction) {
                if let Some(disc) = target_discriminant
                    && std::mem::discriminant(&edge.kind) != disc
                {
                    continue;
                }
                let next = match direction {
                    Direction::Forward => edge.target,
                    Direction::Reverse => edge.source,
                    Direction::Both => {
                        if edge.source == current {
                            edge.target
                        } else {
                            edge.source
                        }
                    }
                };
                if visited.insert(next) {
                    // Record the file owning the edge and the visited node.
                    record_file_dep(edge.file);
                    if let Some(entry) = self.snapshot.nodes().get(next) {
                        record_file_dep(entry.file);
                    }
                    result.push(next);
                    queue.push_back((next, depth + 1));
                }
            }
        }
        dedup_sort(&mut result);
        Arc::new(result)
    }

    fn run_filter(&mut self, input: &[NodeId], predicate: &Predicate) -> Arc<Vec<NodeId>> {
        let compiled = CompiledPredicate::compile(predicate);
        let mut out: Vec<NodeId> = Vec::with_capacity(input.len());
        for &node_id in input {
            if self.check_predicate(node_id, &compiled) {
                out.push(node_id);
            }
        }
        dedup_sort(&mut out);
        Arc::new(out)
    }

    fn run_setop(
        &mut self,
        op: SetOperation,
        left: &PlanNode,
        right: &PlanNode,
    ) -> Arc<Vec<NodeId>> {
        let left_result = self.run(left, None);
        let right_result = self.run(right, None);
        let l = left_result.as_ref();
        let r = right_result.as_ref();

        let mut out: Vec<NodeId> = match op {
            SetOperation::Union => {
                let mut v = Vec::with_capacity(l.len() + r.len());
                v.extend_from_slice(l);
                v.extend_from_slice(r);
                v
            }
            SetOperation::Intersect => {
                let rhs: HashSet<NodeId> = r.iter().copied().collect();
                l.iter().copied().filter(|id| rhs.contains(id)).collect()
            }
            SetOperation::Difference => {
                let rhs: HashSet<NodeId> = r.iter().copied().collect();
                l.iter().copied().filter(|id| !rhs.contains(id)).collect()
            }
        };
        dedup_sort(&mut out);
        Arc::new(out)
    }

    // ------------------------------------------------------------------
    // Predicate dispatch
    // ------------------------------------------------------------------

    fn check_predicate(&mut self, node_id: NodeId, predicate: &CompiledPredicate) -> bool {
        let Some(entry) = self.snapshot.nodes().get(node_id) else {
            return false;
        };
        Self::record_entry_deps(entry);

        match predicate {
            CompiledPredicate::HasCaller => {
                self.has_kind(node_id, Direction::Reverse, &CALLS_PROBE)
            }
            CompiledPredicate::HasCallee => {
                self.has_kind(node_id, Direction::Forward, &CALLS_PROBE)
            }
            CompiledPredicate::IsUnused => !self.has_any_inbound_use(node_id),

            // DB14: relation predicates dispatch through
            // `QueryDb::get::<...Query>` (or the shared subquery helper) so
            // every caller of the planner benefits from language-aware name
            // matching and cache reuse across plan submissions.
            CompiledPredicate::Callers(value) => {
                self.relation_matches_via_db::<CallersQuery>(node_id, RelationKind::Callers, value)
            }
            CompiledPredicate::Callees(value) => {
                self.relation_matches_via_db::<CalleesQuery>(node_id, RelationKind::Callees, value)
            }
            CompiledPredicate::Imports(value) => {
                self.relation_matches_via_db::<ImportsQuery>(node_id, RelationKind::Imports, value)
            }
            CompiledPredicate::Exports(value) => {
                self.relation_matches_via_db::<ExportsQuery>(node_id, RelationKind::Exports, value)
            }
            CompiledPredicate::References(value) => self
                .relation_matches_via_db::<ReferencesQuery>(
                    node_id,
                    RelationKind::References,
                    value,
                ),
            CompiledPredicate::Implements(value) => self
                .relation_matches_via_db::<ImplementsQuery>(
                    node_id,
                    RelationKind::Implements,
                    value,
                ),

            CompiledPredicate::InFile(glob) => entry_in_file(&self.snapshot, entry, glob),
            CompiledPredicate::InScope(kind) => entry_in_scope(&self.snapshot, node_id, *kind),
            CompiledPredicate::MatchesName(pattern) => {
                entry_name_matches(&self.snapshot, node_id, entry, pattern)
            }
            CompiledPredicate::Returns(type_name) => self.node_returns_type(node_id, type_name),

            CompiledPredicate::And(list) => list
                .iter()
                .all(|inner| self.check_predicate(node_id, inner)),
            CompiledPredicate::Or(list) => list
                .iter()
                .any(|inner| self.check_predicate(node_id, inner)),
            CompiledPredicate::Not(inner) => !self.check_predicate(node_id, inner),
        }
    }

    /// DB14 relation dispatch.
    ///
    /// * **Pattern / Regex values** — route through the [`DerivedQuery`] `Q`
    ///   (`CallersQuery`, `CalleesQuery`, `ImportsQuery`, …). The query
    ///   returns a sorted, deduplicated node set; we do a binary search for
    ///   `node_id` to decide whether this row satisfies the predicate.
    ///
    /// * **Subquery values** — reuse the executor's [`Self::shared_node_cache`]
    ///   to materialise the inner plan once, then delegate to
    ///   [`relation_matches_node_via_set`] which walks the edge store with
    ///   the same direction/role/edge-kind semantics as the cached path.
    ///
    /// Routing both paths through the shared set logic (plus cache) means
    /// the planner's "name matches" semantics — segment matching, language
    /// canonicalization, dynamic-language method-segment fallback — live in
    /// exactly one place.
    fn relation_matches_via_db<Q>(
        &mut self,
        node_id: NodeId,
        relation: RelationKind,
        value: &PredicateValue,
    ) -> bool
    where
        Q: crate::query::DerivedQuery<Key = RelationKey, Value = Arc<Vec<NodeId>>>,
    {
        match value {
            PredicateValue::Pattern(pat) => {
                let key = RelationKey::Pattern(pat.clone());
                let matches = self.db.get::<Q>(&key);
                matches.as_ref().binary_search(&node_id).is_ok()
            }
            PredicateValue::Regex(re) => {
                let key = RelationKey::Regex(re.clone());
                let matches = self.db.get::<Q>(&key);
                matches.as_ref().binary_search(&node_id).is_ok()
            }
            PredicateValue::Subquery(sub_plan) => {
                let subquery_set = self.subquery_result(sub_plan);
                let endpoints: HashSet<NodeId> = subquery_set.iter().copied().collect();
                relation_matches_node_via_set(relation, node_id, &endpoints, &self.snapshot)
            }
        }
    }

    /// Execute a subquery plan, memoising the result on the executor.
    fn subquery_result(&mut self, sub_plan: &PlanNode) -> Arc<Vec<NodeId>> {
        if let Some(existing) = self.shared_node_cache.get(sub_plan) {
            return Arc::clone(existing);
        }
        let result = self.run(sub_plan, None);
        self.shared_node_cache
            .insert(sub_plan.clone(), Arc::clone(&result));
        result
    }

    // ------------------------------------------------------------------
    // Edge helpers
    // ------------------------------------------------------------------

    fn neighbours(&self, node_id: NodeId, direction: Direction) -> Vec<StoreEdgeRef> {
        match direction {
            Direction::Forward => self.snapshot.edges().edges_from(node_id),
            Direction::Reverse => self.snapshot.edges().edges_to(node_id),
            Direction::Both => {
                let mut out = self.snapshot.edges().edges_from(node_id);
                out.extend(self.snapshot.edges().edges_to(node_id));
                out
            }
        }
    }

    fn has_kind(&self, node_id: NodeId, direction: Direction, probe: &EdgeKind) -> bool {
        let wanted = std::mem::discriminant(probe);
        self.neighbours(node_id, direction)
            .iter()
            .any(|edge| std::mem::discriminant(&edge.kind) == wanted)
    }

    /// `IsUnused` heuristic — no inbound edges that represent *use* of this
    /// node. DB14+ may refine this with entry-point classification; at DB12
    /// we treat "no incoming `Calls` / `References` / `Imports` / `FfiCall` /
    /// `GrpcCall` / `HttpRequest` / `WebAssemblyCall` / `Implements` /
    /// `Inherits`" as unused.
    fn has_any_inbound_use(&self, node_id: NodeId) -> bool {
        for edge in self.snapshot.edges().edges_to(node_id) {
            if matches!(
                edge.kind,
                EdgeKind::Calls { .. }
                    | EdgeKind::References
                    | EdgeKind::Imports { .. }
                    | EdgeKind::FfiCall { .. }
                    | EdgeKind::GrpcCall { .. }
                    | EdgeKind::HttpRequest { .. }
                    | EdgeKind::WebAssemblyCall
                    | EdgeKind::Implements
                    | EdgeKind::Inherits
            ) {
                return true;
            }
        }
        false
    }

    /// Evaluates `returns:<TypeName>` for a single candidate node.
    ///
    /// Walks every outgoing edge from `node_id` and checks for the first
    /// `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }` whose
    /// target node's interned primary name equals `type_name` byte-exactly
    /// (case-sensitive). Returns `false` if the candidate has no `Return`-
    /// context type edges, or if every such edge targets a different name.
    ///
    /// This routes through `snapshot.edges().edges_from(...)` directly rather
    /// than the relation-query DerivedQuery cache: `Predicate::Returns`
    /// lands as a fresh predicate without sqry-db backing in this unit
    /// (`B2_PLANNER`); a future unit can add a `ReturnsQuery` derived query
    /// for cache reuse, but the dispatch surface stays edge-based here so
    /// that the contract — `TypeOf { Return }` edges, not `NodeEntry.signature`
    /// text — is enforced unconditionally.
    fn node_returns_type(&self, node_id: NodeId, type_name: &str) -> bool {
        for edge in self.snapshot.edges().edges_from(node_id) {
            if !matches!(
                edge.kind,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Return),
                    ..
                }
            ) {
                continue;
            }
            let Some(target_entry) = self.snapshot.nodes().get(edge.target) else {
                continue;
            };
            // Record file dependency for the resolved target so the
            // dependency recorder mirrors the data we read.
            record_file_dep(target_entry.file);
            if let Some(name) = self.snapshot.strings().resolve(target_entry.name)
                && name.as_ref() == type_name
            {
                return true;
            }
        }
        false
    }

    /// Records a file-level dependency for the given node entry.
    ///
    /// Associated function (no `self`) because the body only consults the
    /// thread-local `DependencyRecorderGuard` — the executor's state is not
    /// involved. Call sites still read as `Self::record_entry_deps(entry)`
    /// so they remain symmetric with the rest of the executor's helpers.
    fn record_entry_deps(entry: &NodeEntry) {
        record_file_dep(entry.file);
    }
}

// ============================================================================
// Compiled predicate / pattern / value
// ============================================================================

/// Path / name patterns compile once per filter call (not per input node).
///
/// Relation-value patterns are no longer compiled here — DB14 moved relation
/// predicate evaluation into the [`crate::queries::relation`] `DerivedQuery`
/// impls, which own their own compilation and caching. Relation variants
/// carry the raw [`PredicateValue`] so the planner can build a
/// [`RelationKey`] for `db.get::<...>()` dispatch, or walk the executor's
/// subquery cache when the value is a subquery.
#[derive(Debug)]
enum CompiledPredicate {
    HasCaller,
    HasCallee,
    IsUnused,
    Callers(PredicateValue),
    Callees(PredicateValue),
    Imports(PredicateValue),
    Exports(PredicateValue),
    References(PredicateValue),
    Implements(PredicateValue),
    InFile(CompiledPathPattern),
    InScope(ScopeKind),
    MatchesName(CompiledStringPattern),
    /// `returns:<TypeName>`. Carries the byte-exact type-name needle that
    /// the executor compares against the resolved name string of any node
    /// targeted by a forward `TypeOf { context: Some(Return), .. }` edge.
    /// See [`Predicate::Returns`] for full semantics.
    Returns(String),
    And(Vec<CompiledPredicate>),
    Or(Vec<CompiledPredicate>),
    Not(Box<CompiledPredicate>),
}

impl CompiledPredicate {
    fn compile(predicate: &Predicate) -> Self {
        match predicate {
            Predicate::HasCaller => CompiledPredicate::HasCaller,
            Predicate::HasCallee => CompiledPredicate::HasCallee,
            Predicate::IsUnused => CompiledPredicate::IsUnused,
            // Relation values keep their raw form — compilation is the
            // DerivedQuery's job.
            Predicate::Callers(v) => CompiledPredicate::Callers(v.clone()),
            Predicate::Callees(v) => CompiledPredicate::Callees(v.clone()),
            Predicate::Imports(v) => CompiledPredicate::Imports(v.clone()),
            Predicate::Exports(v) => CompiledPredicate::Exports(v.clone()),
            Predicate::References(v) => CompiledPredicate::References(v.clone()),
            Predicate::Implements(v) => CompiledPredicate::Implements(v.clone()),
            Predicate::InFile(path) => {
                CompiledPredicate::InFile(CompiledPathPattern::compile(path))
            }
            Predicate::InScope(kind) => CompiledPredicate::InScope(*kind),
            Predicate::MatchesName(pattern) => CompiledPredicate::MatchesName(
                CompiledStringPattern::compile(pattern)
                    .unwrap_or(CompiledStringPattern::REJECT_ALL),
            ),
            Predicate::Returns(type_name) => CompiledPredicate::Returns(type_name.clone()),
            Predicate::And(list) => {
                CompiledPredicate::And(list.iter().map(CompiledPredicate::compile).collect())
            }
            Predicate::Or(list) => {
                CompiledPredicate::Or(list.iter().map(CompiledPredicate::compile).collect())
            }
            Predicate::Not(inner) => {
                CompiledPredicate::Not(Box::new(CompiledPredicate::compile(inner)))
            }
        }
    }
}

/// Compiled [`StringPattern`] — either a glob matcher or a raw-string checker.
#[derive(Debug)]
enum CompiledStringPattern {
    Literal {
        needle: String,
        mode: MatchMode,
        case_insensitive: bool,
    },
    Glob {
        matcher: globset::GlobMatcher,
    },
    /// Fallback used when pattern compilation fails (malformed user input).
    /// Matches nothing so evaluation cannot panic or produce false positives.
    RejectAll,
}

impl CompiledStringPattern {
    const REJECT_ALL: Self = CompiledStringPattern::RejectAll;

    fn compile(pattern: &StringPattern) -> Option<Self> {
        match pattern.mode {
            MatchMode::Glob => {
                let glob = GlobBuilder::new(&pattern.raw)
                    .case_insensitive(pattern.case_insensitive)
                    .literal_separator(false)
                    .build()
                    .ok()?;
                Some(CompiledStringPattern::Glob {
                    matcher: glob.compile_matcher(),
                })
            }
            MatchMode::Exact | MatchMode::Prefix | MatchMode::Suffix | MatchMode::Contains => {
                let needle = if pattern.case_insensitive {
                    pattern.raw.to_lowercase()
                } else {
                    pattern.raw.clone()
                };
                Some(CompiledStringPattern::Literal {
                    needle,
                    mode: pattern.mode,
                    case_insensitive: pattern.case_insensitive,
                })
            }
        }
    }

    fn matches(&self, candidate: &str) -> bool {
        match self {
            CompiledStringPattern::Literal {
                needle,
                mode,
                case_insensitive,
            } => {
                let haystack_owned: Option<String> = if *case_insensitive {
                    Some(candidate.to_lowercase())
                } else {
                    None
                };
                let haystack = haystack_owned.as_deref().unwrap_or(candidate);
                match mode {
                    MatchMode::Exact => haystack == needle,
                    MatchMode::Prefix => haystack.starts_with(needle.as_str()),
                    MatchMode::Suffix => haystack.ends_with(needle.as_str()),
                    MatchMode::Contains => haystack.contains(needle.as_str()),
                    MatchMode::Glob => false, // unreachable — glob compiles to Self::Glob.
                }
            }
            CompiledStringPattern::Glob { matcher } => matcher.is_match(candidate),
            CompiledStringPattern::RejectAll => false,
        }
    }
}

#[derive(Debug)]
enum CompiledPathPattern {
    Glob(globset::GlobMatcher),
    RejectAll,
}

impl CompiledPathPattern {
    fn compile(path: &PathPattern) -> Self {
        match globset::Glob::new(&path.glob) {
            Ok(glob) => CompiledPathPattern::Glob(glob.compile_matcher()),
            Err(_) => CompiledPathPattern::RejectAll,
        }
    }

    fn matches(&self, candidate: &str) -> bool {
        match self {
            CompiledPathPattern::Glob(matcher) => matcher.is_match(candidate),
            CompiledPathPattern::RejectAll => false,
        }
    }
}

// ============================================================================
// Edge-kind probe constants
// ============================================================================

/// Probe value used to compute the `Calls` discriminant for existence checks.
/// Carries zero metadata so it is interchangeable with any canonicalised
/// edge-kind probe. Still needed by [`CompiledPredicate::HasCaller`] and
/// [`CompiledPredicate::HasCallee`]; relation predicates route through
/// [`crate::queries::relation`] since DB14.
const CALLS_PROBE: EdgeKind = EdgeKind::Calls {
    argument_count: 0,
    is_async: false,
};

// ============================================================================
// Free-standing helpers
// ============================================================================

fn dedup_sort(v: &mut Vec<NodeId>) {
    v.sort_unstable_by_key(|id| (id.index(), id.generation()));
    v.dedup();
}

fn append_chain_prefix(prefix: &PlanNode, appended_steps: &[PlanNode]) -> PlanNode {
    if appended_steps.is_empty() {
        return prefix.clone();
    }

    let mut steps = match prefix {
        PlanNode::Chain { steps } => steps.clone(),
        _ => vec![prefix.clone()],
    };
    steps.extend(appended_steps.iter().cloned());
    PlanNode::Chain { steps }
}

fn entry_visibility(snapshot: &GraphSnapshot, entry: &NodeEntry) -> Option<Visibility> {
    entry
        .visibility
        .and_then(|sid| snapshot.strings().resolve(sid))
        .and_then(|s| Visibility::parse(s.as_ref()))
}

fn entry_in_file(snapshot: &GraphSnapshot, entry: &NodeEntry, glob: &CompiledPathPattern) -> bool {
    let Some(path) = snapshot.files().resolve(entry.file) else {
        return false;
    };
    let path_str = path.to_string_lossy();
    glob.matches(path_str.as_ref())
}

fn entry_in_scope(snapshot: &GraphSnapshot, node_id: NodeId, kind: ScopeKind) -> bool {
    let Some(entry) = snapshot.nodes().get(node_id) else {
        return false;
    };
    for (_, scope) in snapshot.scope_arena().iter() {
        if scope.kind != kind {
            continue;
        }
        if scope.file != entry.file {
            continue;
        }
        // Scope byte_span is `[start, end)` per ScopeArena docs.
        if scope.byte_span.0 <= entry.start_byte && entry.end_byte <= scope.byte_span.1 {
            return true;
        }
    }
    false
}

/// Filter-time `name:` predicate.
///
/// **B1_ALIGN contract.** Mirrors
/// [`sqry_core::graph::unified::concurrent::graph::GraphSnapshot::find_by_exact_name`]
/// (and the `--exact` CLI shorthand): matches `entry.name` or
/// `entry.qualified_name`, then suppresses synthetic placeholder
/// nodes via
/// [`sqry_core::graph::unified::concurrent::graph::GraphSnapshot::is_node_synthetic`]
/// so the planner's `name:` predicate returns the same set as
/// `--exact` against any fixture.
fn entry_name_matches(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    entry: &NodeEntry,
    pattern: &CompiledStringPattern,
) -> bool {
    let name_matches = snapshot
        .strings()
        .resolve(entry.name)
        .is_some_and(|s| pattern.matches(s.as_ref()));
    let qname_matches = || {
        entry
            .qualified_name
            .and_then(|sid| snapshot.strings().resolve(sid))
            .is_some_and(|s| pattern.matches(s.as_ref()))
    };
    if !(name_matches || qname_matches()) {
        return false;
    }
    // B1_ALIGN: synthetic placeholder nodes are invisible to the
    // `name:` surface (locked by C_SUPPRESS in
    // `sqry-core/src/graph/unified/concurrent/graph.rs`).
    !snapshot.is_node_synthetic(node_id)
}

// ============================================================================
// Inline smoke tests — unit-level coverage that doesn't require a full graph
// fixture lives here; end-to-end tests against a populated graph live in
// `sqry-db/tests/execute_plan_test.rs`.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::ir::{MatchMode, StringPattern};

    #[test]
    fn compiled_string_pattern_exact_case_sensitive() {
        let p = CompiledStringPattern::compile(&StringPattern::exact("Foo")).unwrap();
        assert!(p.matches("Foo"));
        assert!(!p.matches("foo"));
    }

    #[test]
    fn compiled_string_pattern_exact_case_insensitive() {
        let p = CompiledStringPattern::compile(&StringPattern::exact("Foo").case_insensitive())
            .unwrap();
        assert!(p.matches("FOO"));
        assert!(p.matches("foo"));
        assert!(!p.matches("bar"));
    }

    #[test]
    fn compiled_string_pattern_prefix_suffix_contains() {
        let pref = CompiledStringPattern::compile(&StringPattern::prefix("abc")).unwrap();
        assert!(pref.matches("abcdef"));
        assert!(!pref.matches("zabc"));

        let suf = CompiledStringPattern::compile(&StringPattern::suffix("xyz")).unwrap();
        assert!(suf.matches("foo_xyz"));
        assert!(!suf.matches("xyz_foo"));

        let cont = CompiledStringPattern::compile(&StringPattern::contains("mid")).unwrap();
        assert!(cont.matches("prefix_mid_suffix"));
        assert!(!cont.matches("no"));
    }

    #[test]
    fn compiled_string_pattern_glob() {
        let p = CompiledStringPattern::compile(&StringPattern::glob("parse_*")).unwrap();
        assert!(p.matches("parse_expr"));
        assert!(p.matches("parse_"));
        assert!(!p.matches("lexer"));
    }

    #[test]
    fn compiled_string_pattern_malformed_glob_rejects_all() {
        // An invalid glob (unterminated character class) compiles to RejectAll.
        let malformed = StringPattern {
            raw: "[abc".into(),
            mode: MatchMode::Glob,
            case_insensitive: false,
        };
        let p =
            CompiledStringPattern::compile(&malformed).unwrap_or(CompiledStringPattern::REJECT_ALL);
        assert!(!p.matches("abc"));
        assert!(!p.matches("[abc"));
    }

    // `CompiledRegex` tests were removed in DB14 — regex compilation now
    // lives in the relation DerivedQuery impls. Coverage for the DB14
    // regex path lives in `sqry-db/tests/execute_plan_test.rs`
    // (`filter_callers_with_regex_matches_on_source_name`, etc.).

    #[test]
    fn dedup_sort_sorts_and_dedupes_by_index_then_generation() {
        let mut v = vec![
            NodeId::new(3, 1),
            NodeId::new(1, 1),
            NodeId::new(3, 1),
            NodeId::new(2, 1),
            NodeId::new(1, 1),
        ];
        dedup_sort(&mut v);
        assert_eq!(
            v,
            vec![NodeId::new(1, 1), NodeId::new(2, 1), NodeId::new(3, 1)]
        );
    }

    #[test]
    fn compiled_path_pattern_glob_and_reject() {
        let good = CompiledPathPattern::compile(&PathPattern::new("src/**/*.rs"));
        assert!(good.matches("src/graph/unified/mod.rs"));
        assert!(!good.matches("docs/README.md"));

        // Invalid globs compile to RejectAll.
        let bad = CompiledPathPattern::compile(&PathPattern::new("[abc"));
        assert!(!bad.matches("abc"));
    }
}
