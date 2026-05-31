//! Plan-tree generator strategy for the WS1 fusion equivalence property
//! test (`U_WS1_9_FUSION`, DESIGN §2.4 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! Produces random [`QueryPlan`] trees satisfying the IR contract enforced
//! by [`sqry_db::planner::QueryBuilder`]:
//!
//! * Every [`PlanNode::Chain`] starts with a context-free step (a
//!   [`PlanNode::NodeScan`] or a [`PlanNode::SetOp`]).
//! * Every [`PlanNode::EdgeTraversal`] has `max_depth >= 1`.
//! * Every nested subtree is itself a well-formed plan (recursive
//!   invariant).
//!
//! Tree shape is bounded:
//!
//! * **depth** — at most `6` (DAG acceptance line).
//! * **arity** — set-op operands count toward arity; we never construct a
//!   single `PlanNode` with more than `4` immediate children
//!   (`Chain.steps.len() <= 4`).
//!
//! Plans are deliberately diverse in operator mix: `NodeScan`,
//! `EdgeTraversal`, `Filter` predicates (existence, attribute, boolean
//! combinator), `SetOp`, and `Chain`. Relation predicates with
//! [`PredicateValue::Subquery`] inner plans are also produced so the
//! fusion path's subquery-priming logic is exercised.
//!
//! # Why this lives next to `fusion_equivalence.rs`
//!
//! The DAG specifies `plan_gen.rs` as a sibling helper module under
//! `tests/property/`. It is **not** a separate `[[test]]` target — it has
//! no `#[test]` functions of its own and is included from
//! `fusion_equivalence.rs` via `#[path = "plan_gen.rs"] mod plan_gen;`.

#![allow(dead_code, clippy::needless_pass_by_value)]

use proptest::prelude::*;

use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;

use sqry_db::planner::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan,
    SetOperation, StringPattern,
};

/// Maximum operator-tree depth, locked by the DAG acceptance line for
/// `U_WS1_9_FUSION` (DESIGN §2.4).
pub const MAX_DEPTH: u32 = 6;
/// Maximum immediate-child count at any composite operator (`Chain.steps`
/// length, set-op operand count taken as 2). Locked by the DAG acceptance
/// line for `U_WS1_9_FUSION`.
pub const MAX_ARITY: usize = 4;

/// Strategy for a random [`QueryPlan`] satisfying the IR well-formedness
/// invariants the executor relies on.
///
/// See the module docstring for the bounded shape contract.
pub fn arbitrary_plan_tree() -> impl Strategy<Value = QueryPlan> {
    plan_node(MAX_DEPTH, /* root */ true).prop_map(QueryPlan::new)
}

/// Generates a [`PlanNode`] with bounded depth.
///
/// `depth_budget` is the maximum recursion depth remaining; leaves are
/// produced when it reaches 0. `is_root` is `true` for the top-level
/// invocation — the root may not be a [`PlanNode::EdgeTraversal`] or
/// [`PlanNode::Filter`] (they require an inherited input set the executor
/// cannot supply at the root).
fn plan_node(depth_budget: u32, is_root: bool) -> BoxedStrategy<PlanNode> {
    if depth_budget == 0 {
        // Leaf: always a NodeScan so the result is context-free and
        // well-formed regardless of how the parent composed it.
        return node_scan().boxed();
    }

    let child_budget = depth_budget.saturating_sub(1);

    // Weighted union over the well-formed shapes. Weights bias the
    // generator toward producing trees that exercise multiple operators
    // before bottoming out — pure-leaf trees are cheap and uninteresting.
    if is_root {
        // Root cannot be Filter / EdgeTraversal: the executor has no
        // input set to feed them. SetOp at the root is fine because both
        // operands are evaluated context-free.
        prop_oneof![
            2 => node_scan(),
            3 => chain_node(child_budget),
            2 => set_op_node(child_budget),
        ]
        .boxed()
    } else {
        prop_oneof![
            // Leaves and small interior shapes — high weight so the tree
            // stays shallow on average.
            3 => node_scan(),
            // SetOp as an interior node (operands themselves recurse).
            1 => set_op_node(child_budget),
            // Chains keep first-step context-free invariant intact.
            2 => chain_node(child_budget),
            // EdgeTraversal as an interior step of a chain — handled via
            // chain_node's tail builder. Including it here too gives the
            // generator the option of producing a `SetOp(left: scan,
            // right: scan)` whose own structural equality drives fusion's
            // shared-prefix code path. We still produce a context-free
            // wrapping by re-rooting via a single-step chain.
        ]
        .boxed()
    }
}

/// A context-free [`PlanNode::NodeScan`] step.
fn node_scan() -> BoxedStrategy<PlanNode> {
    (
        prop::option::of(node_kind_strategy()),
        prop::option::of(name_pattern_strategy()),
    )
        .prop_map(|(kind, name_pattern)| PlanNode::NodeScan {
            kind,
            // Visibility is intentionally always `None`: the well-formed
            // graph generator does not stamp visibility strings on most
            // synthetic nodes, so filtering by visibility yields almost
            // always the empty set and is dominated by the simpler kind /
            // name filters. Leaving it out keeps the generator's output
            // distribution skewed toward plans that actually match real
            // nodes in the synthetic snapshot.
            visibility: None,
            name_pattern,
        })
        .boxed()
}

/// Strategy: a single-step `EdgeTraversal` for use as a chain interior
/// step. Carries canonical-metadata edge kinds so plan hashing is stable
/// (the fuser contract — see `fuse.rs` module docstring).
fn edge_traversal_step() -> BoxedStrategy<PlanNode> {
    (
        direction_strategy(),
        prop::option::of(edge_kind_strategy()),
        1u32..=3u32,
    )
        .prop_map(
            |(direction, edge_kind, max_depth)| PlanNode::EdgeTraversal {
                direction,
                edge_kind,
                max_depth,
                // `resolved_via` is meaningful only paired with a
                // `Calls` edge_kind; for arbitrary plans we keep it `None`
                // so the generator's traversal step is direction/depth
                // shaped and does not depend on Phase A C-indirect-call
                // metadata that the synthetic graph may not exercise.
                resolved_via: None,
            },
        )
        .boxed()
}

/// Strategy: a `Filter` step (interior of a chain).
fn filter_step(depth_budget: u32) -> BoxedStrategy<PlanNode> {
    predicate(depth_budget)
        .prop_map(|predicate| PlanNode::Filter { predicate })
        .boxed()
}

/// Strategy: a [`PlanNode::Chain`] with a context-free first step plus
/// 0..=`MAX_ARITY-1` trailing chain steps (each of which may be
/// `EdgeTraversal`, `Filter`, or — recursively — a nested `Chain` wrapped
/// in an outer chain via `set_op`'s context-free shape).
fn chain_node(depth_budget: u32) -> BoxedStrategy<PlanNode> {
    // First step must be context-free.
    let first_step = prop_oneof![
        2 => node_scan(),
        1 => set_op_node(depth_budget.saturating_sub(1)),
    ];

    // Trailing steps: 0..=MAX_ARITY-1 (so the total Chain.steps length
    // stays <= MAX_ARITY).
    let trailing_step = prop_oneof![
        2 => edge_traversal_step(),
        1 => filter_step(depth_budget.saturating_sub(1)),
    ];

    let trailing = prop::collection::vec(trailing_step, 0..MAX_ARITY);

    (first_step, trailing)
        .prop_map(|(first, mut rest)| {
            // Build steps; if no trailing steps were generated we still
            // wrap the single first step as a Chain so the planner
            // exercises the Chain dispatch path. The executor handles
            // length-1 chains correctly (it threads with `None` input,
            // same as the root would).
            let mut steps = Vec::with_capacity(1 + rest.len());
            steps.push(first);
            steps.append(&mut rest);
            // Guard MAX_ARITY (defence-in-depth: prop::collection::vec
            // already bounds rest.len() to MAX_ARITY-1).
            if steps.len() > MAX_ARITY {
                steps.truncate(MAX_ARITY);
            }
            PlanNode::Chain { steps }
        })
        .boxed()
}

/// Strategy: a [`PlanNode::SetOp`] node whose two operands are themselves
/// well-formed plan subtrees.
fn set_op_node(depth_budget: u32) -> BoxedStrategy<PlanNode> {
    if depth_budget == 0 {
        // Below-budget: emit a context-free leaf so we never bottom out
        // into an invalid (input-requiring) operand.
        return node_scan().boxed();
    }

    let child_budget = depth_budget.saturating_sub(1);
    let op = prop_oneof![
        Just(SetOperation::Union),
        Just(SetOperation::Intersect),
        Just(SetOperation::Difference),
    ];

    // Operands must be context-free — only NodeScan, Chain (treated as
    // context-free by the IR because its first step is context-free),
    // and SetOp qualify. We deliberately weight toward NodeScan and
    // Chain so the tree's branching factor stays bounded and the trivial
    // flat `Union(scan, scan)` shape does not dominate.
    let leaf_operand = prop_oneof![
        3 => node_scan(),
        1 => chain_node(child_budget),
    ];

    let nested_operand = if child_budget > 1 {
        // Recurse only one level deeper. The depth budget guarantees we
        // bottom out; we additionally weight nested set-ops below the
        // leaf shapes so the average tree depth stays well below the
        // `MAX_DEPTH` cap.
        prop_oneof![
            3 => leaf_operand.clone(),
            1 => set_op_node(child_budget.saturating_sub(1)),
        ]
        .boxed()
    } else {
        leaf_operand.boxed()
    };

    (op, nested_operand.clone(), nested_operand)
        .prop_map(|(op, left, right)| PlanNode::SetOp {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
        .boxed()
}

/// Strategy: a [`Predicate`]. Leaf-skewed.
fn predicate(depth_budget: u32) -> BoxedStrategy<Predicate> {
    let leaf = prop_oneof![
        Just(Predicate::HasCaller),
        Just(Predicate::HasCallee),
        Just(Predicate::IsUnused),
        path_pattern_strategy().prop_map(Predicate::InFile),
        name_pattern_strategy().prop_map(Predicate::MatchesName),
        // Relation predicates with Pattern (no subquery) — exercises the
        // DerivedQuery dispatch (`Q::get`) path inside the executor.
        name_pattern_strategy().prop_map(|p| Predicate::Callers(PredicateValue::Pattern(p))),
        name_pattern_strategy().prop_map(|p| Predicate::Callees(PredicateValue::Pattern(p))),
    ];

    if depth_budget == 0 {
        return leaf.boxed();
    }

    // With remaining budget we can produce boolean combinators and
    // (rarely) Predicate variants that embed a subquery PlanNode.
    let child_budget = depth_budget.saturating_sub(1);
    prop_oneof![
        4 => leaf,
        1 => prop::collection::vec(predicate(child_budget), 1..=MAX_ARITY)
            .prop_map(Predicate::And),
        1 => prop::collection::vec(predicate(child_budget), 1..=MAX_ARITY)
            .prop_map(Predicate::Or),
        1 => predicate(child_budget).prop_map(|p| Predicate::Not(Box::new(p))),
        // Subquery-bearing relation predicate: the inner PlanNode must
        // itself be a well-formed (context-free) plan, so we re-enter the
        // top-level shape generator with the remaining budget. This is
        // the single place where plans embed plans — exactly the cross-
        // plan deduplication the fuser's subquery-batch path is built
        // to handle.
        1 => plan_node(child_budget, /* is_root */ true)
            .prop_map(|inner| Predicate::Callers(PredicateValue::Subquery(Box::new(inner)))),
    ]
    .boxed()
}

// ---------------------------------------------------------------------------
// Leaf-strategy helpers
// ---------------------------------------------------------------------------

fn node_kind_strategy() -> impl Strategy<Value = NodeKind> {
    prop_oneof![
        Just(NodeKind::Function),
        Just(NodeKind::Method),
        Just(NodeKind::Class),
        Just(NodeKind::Variable),
        Just(NodeKind::Constant),
        Just(NodeKind::Type),
        Just(NodeKind::Struct),
        Just(NodeKind::Enum),
        Just(NodeKind::Module),
        Just(NodeKind::Property),
    ]
}

fn direction_strategy() -> impl Strategy<Value = Direction> {
    prop_oneof![
        Just(Direction::Forward),
        Just(Direction::Reverse),
        Just(Direction::Both),
    ]
}

/// Edge-kind strategy with canonical zero/false/default metadata so the
/// fuser's plan-hash contract holds (see `fuse.rs` module docstring).
fn edge_kind_strategy() -> impl Strategy<Value = EdgeKind> {
    prop_oneof![
        Just(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }),
        Just(EdgeKind::References),
        Just(EdgeKind::Defines),
        Just(EdgeKind::Contains),
        Just(EdgeKind::Inherits),
        Just(EdgeKind::Implements),
        Just(EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        }),
    ]
}

fn name_pattern_strategy() -> impl Strategy<Value = StringPattern> {
    // Restrict to alphanumeric ASCII + a small set of meta characters so
    // patterns are valid globs and reasonably likely to match nothing
    // (the property holds for empty result sets too — equality of empty
    // sets is still equality).
    let raw = prop_oneof![
        // Names the well-formed graph generator stamps on every node:
        // `n{idx}_{kind}_{offset}`. Including a few literals biases
        // toward matching SOMETHING in the synthetic snapshot.
        Just("n0_function_0".to_string()),
        Just("n1_method_0".to_string()),
        Just("missing".to_string()),
        Just("*".to_string()),
        Just("n*".to_string()),
        // Arbitrary short identifiers — generally won't match, but the
        // property holds regardless.
        "[a-z][a-z0-9_]{0,8}".prop_map(|s: String| s),
    ];

    let mode = prop_oneof![
        Just(MatchMode::Exact),
        Just(MatchMode::Prefix),
        Just(MatchMode::Suffix),
        Just(MatchMode::Contains),
        Just(MatchMode::Glob),
    ];

    (raw, mode, any::<bool>()).prop_map(|(raw, mode, case_insensitive)| StringPattern {
        raw,
        mode,
        case_insensitive,
    })
}

fn path_pattern_strategy() -> impl Strategy<Value = PathPattern> {
    prop_oneof![
        Just(PathPattern::new("**/*.rs")),
        Just(PathPattern::new("file_*")),
        Just(PathPattern::new("nonexistent/**")),
        Just(PathPattern::new("*")),
    ]
}
