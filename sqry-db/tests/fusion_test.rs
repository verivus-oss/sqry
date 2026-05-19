//! Integration tests for the DB11 query fuser
//! (`sqry_db::planner::fuse`).
//!
//! The fuser merges shared `NodeScan` prefixes across a batch of
//! [`QueryPlan`]s, exposes the grouped output via [`FusedPlanBatch`], and
//! recursively fuses subqueries embedded in any predicate of any tail.
//! These integration tests cover every promised behaviour in the DB11
//! brief plus realistic mixed batches and serde round-trips.

use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::Visibility;

use sqry_db::planner::{
    Direction, FusedPlanBatch, FusionTail, PathPattern, PlanNode, Predicate, PredicateValue,
    QueryPlan, SetOperation, StringPattern, fuse_plans, fuse_single,
};

// ---------------------------------------------------------------------------
// Plan-construction helpers
// ---------------------------------------------------------------------------

fn scan(kind: NodeKind) -> PlanNode {
    PlanNode::NodeScan {
        kind: Some(kind),
        visibility: None,
        name_pattern: None,
    }
}

fn scan_visible(kind: NodeKind, vis: Visibility) -> PlanNode {
    PlanNode::NodeScan {
        kind: Some(kind),
        visibility: Some(vis),
        name_pattern: None,
    }
}

fn scan_named(kind: NodeKind, raw: &str) -> PlanNode {
    PlanNode::NodeScan {
        kind: Some(kind),
        visibility: None,
        name_pattern: Some(StringPattern::glob(raw)),
    }
}

fn filter_has_caller() -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::HasCaller,
    }
}

fn filter_has_callee() -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::HasCallee,
    }
}

fn filter_in_file(glob: &str) -> PlanNode {
    PlanNode::Filter {
        predicate: Predicate::InFile(PathPattern::new(glob)),
    }
}

fn traverse_calls() -> PlanNode {
    PlanNode::EdgeTraversal {
        direction: Direction::Forward,
        // Canonical metadata per the DB10 contract — see fuse module docs.
        edge_kind: Some(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }),
        max_depth: 1,
        resolved_via: None,
    }
}

fn chain(steps: Vec<PlanNode>) -> QueryPlan {
    QueryPlan::new(PlanNode::Chain { steps })
}

fn standalone(node: PlanNode) -> QueryPlan {
    QueryPlan::new(node)
}

// ---------------------------------------------------------------------------
// Trivial inputs
// ---------------------------------------------------------------------------

#[test]
fn empty_batch_produces_empty_fusion() {
    let batch = fuse_plans(vec![]);
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    assert_eq!(batch.total_plans(), 0);
    assert_eq!(batch.stats().total_plans, 0);
    assert_eq!(batch.stats().fusion_groups, 0);
    assert_eq!(batch.stats().scans_eliminated, 0);
    assert_eq!(batch.stats().subqueries_total, 0);
    assert_eq!(batch.stats().subqueries_unique, 0);
    assert!(batch.subquery_batch().is_none());
}

#[test]
fn singleton_chain_batch_creates_one_group_with_one_tail() {
    let plan = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let batch = fuse_single(plan.clone());

    assert_eq!(batch.len(), 1);
    assert_eq!(batch.total_plans(), 1);
    assert_eq!(batch.stats().total_plans, 1);
    assert_eq!(batch.stats().fusion_groups, 1);
    assert_eq!(batch.stats().scans_eliminated, 0);

    let group = &batch.groups()[0];
    assert_eq!(group.prefix(), &scan(NodeKind::Function));
    assert_eq!(group.tail_count(), 1);
    let tail = &group.tails()[0];
    assert_eq!(tail.original_index, 0);
    match &tail.tail {
        FusionTail::ChainContinuation { remaining_steps } => {
            assert_eq!(remaining_steps, &vec![filter_has_caller()]);
        }
        FusionTail::Identity => panic!("expected ChainContinuation"),
    }

    assert_eq!(batch.input_plans(), vec![plan]);
}

#[test]
fn singleton_standalone_scan_creates_identity_tail() {
    let plan = standalone(scan(NodeKind::Class));
    let batch = fuse_single(plan.clone());

    assert_eq!(batch.len(), 1);
    let group = &batch.groups()[0];
    assert_eq!(group.prefix(), &scan(NodeKind::Class));
    assert_eq!(group.tail_count(), 1);
    assert_eq!(group.tails()[0].original_index, 0);
    assert_eq!(group.tails()[0].tail, FusionTail::Identity);
    assert_eq!(batch.input_plans(), vec![plan]);
}

// ---------------------------------------------------------------------------
// Distinct vs. shared prefixes
// ---------------------------------------------------------------------------

#[test]
fn two_plans_with_different_prefixes_produce_two_groups() {
    let p1 = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let p2 = chain(vec![scan(NodeKind::Method), filter_has_callee()]);

    let batch = fuse_plans(vec![p1.clone(), p2.clone()]);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().total_plans, 2);
    assert_eq!(batch.stats().fusion_groups, 2);
    assert_eq!(batch.stats().scans_eliminated, 0);

    // First-appearance order: p1's prefix comes first.
    assert_eq!(batch.groups()[0].prefix(), &scan(NodeKind::Function));
    assert_eq!(batch.groups()[1].prefix(), &scan(NodeKind::Method));

    assert_eq!(batch.input_plans(), vec![p1, p2]);
}

#[test]
fn two_plans_with_identical_node_scan_prefix_share_a_group() {
    let p1 = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let p2 = chain(vec![scan(NodeKind::Function), filter_has_callee()]);

    let batch = fuse_plans(vec![p1.clone(), p2.clone()]);

    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().total_plans, 2);
    assert_eq!(batch.stats().fusion_groups, 1);
    assert_eq!(batch.stats().scans_eliminated, 1);

    let group = &batch.groups()[0];
    assert_eq!(group.prefix(), &scan(NodeKind::Function));
    assert_eq!(group.tail_count(), 2);
    assert_eq!(group.tails()[0].original_index, 0);
    assert_eq!(group.tails()[1].original_index, 1);

    // Round-trip preserves the originals in submission order.
    assert_eq!(batch.input_plans(), vec![p1, p2]);
}

#[test]
fn three_plans_two_share_one_differs_produces_two_groups_one_eliminated() {
    let p1 = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let p2 = chain(vec![scan(NodeKind::Function), filter_has_callee()]);
    let p3 = chain(vec![scan(NodeKind::Class), filter_has_caller()]);

    let batch = fuse_plans(vec![p1.clone(), p2.clone(), p3.clone()]);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().total_plans, 3);
    assert_eq!(batch.stats().fusion_groups, 2);
    assert_eq!(batch.stats().scans_eliminated, 1);

    // First group has two tails (p1, p2); second has one (p3).
    let g0 = &batch.groups()[0];
    let g1 = &batch.groups()[1];
    assert_eq!(g0.prefix(), &scan(NodeKind::Function));
    assert_eq!(g0.tail_count(), 2);
    assert_eq!(g0.tails()[0].original_index, 0);
    assert_eq!(g0.tails()[1].original_index, 1);

    assert_eq!(g1.prefix(), &scan(NodeKind::Class));
    assert_eq!(g1.tail_count(), 1);
    assert_eq!(g1.tails()[0].original_index, 2);

    assert_eq!(batch.input_plans(), vec![p1, p2, p3]);
}

// ---------------------------------------------------------------------------
// Standalone (non-Chain) roots
// ---------------------------------------------------------------------------

#[test]
fn standalone_node_scan_plans_fuse_correctly() {
    // Two plans whose entire body is one NodeScan.
    let p1 = standalone(scan(NodeKind::Function));
    let p2 = standalone(scan(NodeKind::Function));
    let p3 = standalone(scan(NodeKind::Method));

    let batch = fuse_plans(vec![p1, p2, p3]);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().scans_eliminated, 1);

    assert_eq!(batch.groups()[0].prefix(), &scan(NodeKind::Function));
    assert_eq!(batch.groups()[0].tail_count(), 2);
    assert_eq!(batch.groups()[0].tails()[0].tail, FusionTail::Identity);
    assert_eq!(batch.groups()[0].tails()[1].tail, FusionTail::Identity);

    assert_eq!(batch.groups()[1].prefix(), &scan(NodeKind::Method));
    assert_eq!(batch.groups()[1].tail_count(), 1);
    assert_eq!(batch.groups()[1].tails()[0].tail, FusionTail::Identity);
}

#[test]
fn standalone_setop_root_is_treated_as_a_single_prefix() {
    let setop = PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan(NodeKind::Function)),
        right: Box::new(scan(NodeKind::Method)),
    };

    let p1 = standalone(setop.clone());
    let p2 = standalone(setop.clone());
    let p3 = chain(vec![setop.clone(), filter_has_caller()]);

    let batch = fuse_plans(vec![p1.clone(), p2.clone(), p3.clone()]);

    // p1 and p2 share an identity group on the SetOp, p3 shares the
    // same SetOp prefix but with a Chain continuation, so all three
    // belong to ONE fusion group.
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().scans_eliminated, 2);

    let group = &batch.groups()[0];
    assert_eq!(group.prefix(), &setop);
    assert_eq!(group.tail_count(), 3);
    assert_eq!(group.tails()[0].tail, FusionTail::Identity);
    assert_eq!(group.tails()[1].tail, FusionTail::Identity);
    match &group.tails()[2].tail {
        FusionTail::ChainContinuation { remaining_steps } => {
            assert_eq!(remaining_steps, &vec![filter_has_caller()]);
        }
        FusionTail::Identity => panic!("p3 should be ChainContinuation"),
    }

    assert_eq!(batch.input_plans(), vec![p1, p2, p3]);
}

// ---------------------------------------------------------------------------
// NodeScan filter sensitivity: visibility / name_pattern matter
// ---------------------------------------------------------------------------

#[test]
fn scans_differing_only_in_visibility_do_not_fuse() {
    let p1 = chain(vec![
        scan_visible(NodeKind::Function, Visibility::Public),
        filter_has_caller(),
    ]);
    let p2 = chain(vec![
        scan_visible(NodeKind::Function, Visibility::Private),
        filter_has_caller(),
    ]);

    let batch = fuse_plans(vec![p1, p2]);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().scans_eliminated, 0);
}

#[test]
fn scans_differing_only_in_name_pattern_do_not_fuse() {
    let p1 = chain(vec![
        scan_named(NodeKind::Function, "foo*"),
        filter_has_caller(),
    ]);
    let p2 = chain(vec![
        scan_named(NodeKind::Function, "bar*"),
        filter_has_caller(),
    ]);

    let batch = fuse_plans(vec![p1, p2]);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().scans_eliminated, 0);
}

#[test]
fn scans_with_identical_name_pattern_fuse() {
    let p1 = chain(vec![
        scan_named(NodeKind::Function, "foo*"),
        filter_has_caller(),
    ]);
    let p2 = chain(vec![
        scan_named(NodeKind::Function, "foo*"),
        filter_has_callee(),
    ]);

    let batch = fuse_plans(vec![p1, p2]);

    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().scans_eliminated, 1);
}

// ---------------------------------------------------------------------------
// Subquery fusion (recursive)
// ---------------------------------------------------------------------------

fn callers_subquery_pred(inner: PlanNode) -> Predicate {
    Predicate::Callers(PredicateValue::Subquery(Box::new(inner)))
}

#[test]
fn identical_subqueries_dedupe_into_subquery_batch() {
    let inner = scan(NodeKind::Method);

    let p1 = chain(vec![
        scan(NodeKind::Function),
        PlanNode::Filter {
            predicate: callers_subquery_pred(inner.clone()),
        },
    ]);
    let p2 = chain(vec![
        scan(NodeKind::Class),
        PlanNode::Filter {
            predicate: callers_subquery_pred(inner.clone()),
        },
    ]);

    let batch = fuse_plans(vec![p1, p2]);

    // Top-level: two distinct prefixes, no top-level fusion.
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().scans_eliminated, 0);

    // Subquery batch: 2 occurrences, 1 unique.
    assert_eq!(batch.stats().subqueries_total, 2);
    assert_eq!(batch.stats().subqueries_unique, 1);
    assert_eq!(batch.stats().subqueries_eliminated(), 1);

    let sub = batch.subquery_batch().expect("subquery batch must exist");
    assert_eq!(sub.len(), 1);
    assert_eq!(sub.groups()[0].prefix(), &inner);
    assert_eq!(sub.groups()[0].tail_count(), 1);
}

#[test]
fn distinct_subqueries_each_get_their_own_subquery_group() {
    let inner_a = scan(NodeKind::Method);
    let inner_b = scan(NodeKind::Class);

    let p1 = chain(vec![
        scan(NodeKind::Function),
        PlanNode::Filter {
            predicate: callers_subquery_pred(inner_a.clone()),
        },
    ]);
    let p2 = chain(vec![
        scan(NodeKind::Function),
        PlanNode::Filter {
            predicate: callers_subquery_pred(inner_b.clone()),
        },
    ]);

    let batch = fuse_plans(vec![p1, p2]);

    // Top-level: identical prefixes share one group.
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().scans_eliminated, 1);

    let sub = batch.subquery_batch().expect("subquery batch must exist");
    assert_eq!(sub.len(), 2);
    assert_eq!(sub.stats().total_plans, 2);
    assert_eq!(sub.stats().scans_eliminated, 0);
    assert_eq!(sub.groups()[0].prefix(), &inner_a);
    assert_eq!(sub.groups()[1].prefix(), &inner_b);
}

#[test]
fn nested_subqueries_recurse_through_the_subquery_batch_chain() {
    // A subquery whose own predicate references another subquery.
    let leaf = scan(NodeKind::Function);
    let mid_plan = PlanNode::Chain {
        steps: vec![
            scan(NodeKind::Method),
            PlanNode::Filter {
                predicate: Predicate::Callees(PredicateValue::Subquery(Box::new(leaf.clone()))),
            },
        ],
    };
    let outer = chain(vec![
        scan(NodeKind::Class),
        PlanNode::Filter {
            predicate: callers_subquery_pred(mid_plan.clone()),
        },
    ]);

    let batch = fuse_plans(vec![outer]);

    // Top-level: 1 group, no eliminated scans.
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().scans_eliminated, 0);

    // First-level subquery batch must contain BOTH the outer subquery
    // (mid_plan) and the inner leaf, because `walk_predicate_value_for_subqueries`
    // walks the inner plan eagerly.
    let sub = batch.subquery_batch().expect("subquery batch must exist");
    assert_eq!(sub.len(), 2);
    assert_eq!(sub.stats().total_plans, 2);

    // mid_plan is a `Chain`, so its split prefix is the first step
    // (scan(Method)); leaf is a standalone NodeScan whose prefix is itself.
    // Verify by reconstructing the originals through `input_plans()` —
    // first appearance order is preserved (mid_plan first, leaf second).
    let originals = sub.input_plans();
    assert_eq!(originals.len(), 2);
    assert_eq!(originals[0].root(), &mid_plan);
    assert_eq!(originals[1].root(), &leaf);

    // The recursion must surface the leaf as well — its prefix is the
    // standalone NodeScan(Function).
    assert_eq!(sub.groups()[1].prefix(), &leaf);
    assert_eq!(sub.groups()[1].tails()[0].tail, FusionTail::Identity);

    // mid_plan's group prefix is its first chain step.
    assert_eq!(sub.groups()[0].prefix(), &scan(NodeKind::Method));
    match &sub.groups()[0].tails()[0].tail {
        FusionTail::ChainContinuation { remaining_steps } => {
            assert_eq!(remaining_steps.len(), 1);
            match &remaining_steps[0] {
                PlanNode::Filter { predicate } => match predicate {
                    Predicate::Callees(PredicateValue::Subquery(inner)) => {
                        assert_eq!(inner.as_ref(), &leaf);
                    }
                    other => panic!("unexpected predicate {other:?}"),
                },
                other => panic!("unexpected step {other:?}"),
            }
        }
        FusionTail::Identity => panic!("mid_plan should have been split"),
    }

    // Recursing one more level: the *recursive* subquery batch will be
    // present because the mid_plan still embeds a subquery (the leaf).
    // That nested batch contains exactly the leaf, deduplicated.
    let recursive = sub
        .subquery_batch()
        .expect("recursive subquery batch must exist");
    assert_eq!(recursive.len(), 1);
    assert_eq!(recursive.groups()[0].prefix(), &leaf);
    // No deeper nesting: leaf has no predicates of its own.
    assert!(recursive.subquery_batch().is_none());
}

#[test]
fn subqueries_inside_setop_branches_are_collected() {
    let inner = scan(NodeKind::Function);
    let setop = PlanNode::SetOp {
        op: SetOperation::Intersect,
        left: Box::new(PlanNode::Chain {
            steps: vec![
                scan(NodeKind::Function),
                PlanNode::Filter {
                    predicate: callers_subquery_pred(inner.clone()),
                },
            ],
        }),
        right: Box::new(scan(NodeKind::Method)),
    };

    let plan = standalone(setop);
    let batch = fuse_plans(vec![plan]);

    let sub = batch.subquery_batch().expect("subquery batch must exist");
    assert_eq!(sub.len(), 1);
    assert_eq!(sub.groups()[0].prefix(), &inner);
}

#[test]
fn subqueries_inside_boolean_combinators_are_collected() {
    let a = scan(NodeKind::Function);
    let b = scan(NodeKind::Method);

    let predicate = Predicate::And(vec![
        Predicate::Or(vec![
            Predicate::Callers(PredicateValue::Subquery(Box::new(a.clone()))),
            Predicate::Callees(PredicateValue::Subquery(Box::new(b.clone()))),
        ]),
        Predicate::Not(Box::new(Predicate::References(PredicateValue::Subquery(
            Box::new(a.clone()),
        )))),
    ]);
    let plan = chain(vec![scan(NodeKind::Class), PlanNode::Filter { predicate }]);

    let batch = fuse_plans(vec![plan]);
    assert_eq!(batch.stats().subqueries_total, 3);
    assert_eq!(batch.stats().subqueries_unique, 2);

    let sub = batch.subquery_batch().expect("subquery batch must exist");
    assert_eq!(sub.len(), 2);
    assert_eq!(sub.groups()[0].prefix(), &a);
    assert_eq!(sub.groups()[1].prefix(), &b);
}

// ---------------------------------------------------------------------------
// Idempotence
// ---------------------------------------------------------------------------

#[test]
fn fuse_is_idempotent_for_diverse_batches() {
    let inner = scan(NodeKind::Method);
    let plans = vec![
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        chain(vec![scan(NodeKind::Function), filter_has_callee()]),
        chain(vec![scan(NodeKind::Class), filter_in_file("src/**")]),
        standalone(scan(NodeKind::Method)),
        chain(vec![
            scan(NodeKind::Function),
            PlanNode::Filter {
                predicate: callers_subquery_pred(inner.clone()),
            },
        ]),
    ];

    let first = fuse_plans(plans.clone());
    let recovered = first.input_plans();
    assert_eq!(recovered, plans);

    let second = fuse_plans(recovered);
    assert_eq!(first, second);
}

#[test]
fn fuse_round_trip_preserves_all_originals_in_submission_order() {
    let plans = vec![
        chain(vec![scan(NodeKind::Function), traverse_calls()]),
        chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            traverse_calls(),
        ]),
        chain(vec![scan(NodeKind::Method), filter_has_caller()]),
    ];
    let batch = fuse_plans(plans.clone());
    let recovered = batch.input_plans();
    assert_eq!(recovered, plans);
}

// ---------------------------------------------------------------------------
// Original routing
// ---------------------------------------------------------------------------

#[test]
fn original_indices_route_results_back_to_submitters() {
    let p0 = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let p1 = chain(vec![scan(NodeKind::Method), filter_has_caller()]);
    let p2 = chain(vec![scan(NodeKind::Function), filter_has_callee()]);
    let p3 = chain(vec![scan(NodeKind::Method), filter_has_callee()]);

    let batch = fuse_plans(vec![p0.clone(), p1.clone(), p2.clone(), p3.clone()]);

    // Two groups: {Function: [0, 2]}, {Method: [1, 3]}.
    assert_eq!(batch.len(), 2);

    let g0 = &batch.groups()[0];
    let g1 = &batch.groups()[1];
    assert_eq!(g0.prefix(), &scan(NodeKind::Function));
    assert_eq!(
        g0.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![0, 2],
    );
    assert_eq!(g1.prefix(), &scan(NodeKind::Method));
    assert_eq!(
        g1.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![1, 3],
    );
}

// ---------------------------------------------------------------------------
// Realistic mixed batch
// ---------------------------------------------------------------------------

#[test]
fn realistic_mixed_batch_with_chain_setop_and_standalone_scan() {
    let setop = PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan(NodeKind::Function)),
        right: Box::new(scan(NodeKind::Method)),
    };

    let plans = vec![
        // 0: NodeScan(Function) -> filter
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        // 1: NodeScan(Function) -> different filter (shares with #0)
        chain(vec![scan(NodeKind::Function), filter_has_callee()]),
        // 2: NodeScan(Class) -> in-file filter (own group)
        chain(vec![scan(NodeKind::Class), filter_in_file("src/**")]),
        // 3: standalone NodeScan(Method) (own group)
        standalone(scan(NodeKind::Method)),
        // 4: SetOp standalone (own group)
        standalone(setop.clone()),
        // 5: SetOp -> traverse (shares prefix with #4)
        chain(vec![setop.clone(), traverse_calls()]),
        // 6: NodeScan(Function) -> traverse (shares with #0, #1)
        chain(vec![scan(NodeKind::Function), traverse_calls()]),
    ];

    let batch = fuse_plans(plans.clone());

    // Expected groups in first-appearance order:
    //   Function (3 tails: 0, 1, 6)
    //   Class    (1 tail:  2)
    //   Method   (1 tail:  3)
    //   SetOp    (2 tails: 4, 5)
    assert_eq!(batch.len(), 4);
    assert_eq!(batch.stats().total_plans, 7);
    assert_eq!(batch.stats().fusion_groups, 4);
    assert_eq!(batch.stats().scans_eliminated, 3);

    let g0 = &batch.groups()[0];
    assert_eq!(g0.prefix(), &scan(NodeKind::Function));
    assert_eq!(
        g0.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 6],
    );

    let g1 = &batch.groups()[1];
    assert_eq!(g1.prefix(), &scan(NodeKind::Class));
    assert_eq!(
        g1.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![2],
    );

    let g2 = &batch.groups()[2];
    assert_eq!(g2.prefix(), &scan(NodeKind::Method));
    assert_eq!(
        g2.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![3],
    );

    let g3 = &batch.groups()[3];
    assert_eq!(g3.prefix(), &setop);
    assert_eq!(
        g3.tails()
            .iter()
            .map(|t| t.original_index)
            .collect::<Vec<_>>(),
        vec![4, 5],
    );

    // Round-trip preserves submission order exactly.
    assert_eq!(batch.input_plans(), plans);

    // Idempotent over the realistic batch as well.
    let again = fuse_plans(batch.input_plans());
    assert_eq!(batch, again);
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn fusion_group_order_matches_first_appearance_of_prefix() {
    // Even though Method appears more times overall, Function appears
    // first, so it must come first in `groups()`.
    let plans = vec![
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        chain(vec![scan(NodeKind::Method), filter_has_caller()]),
        chain(vec![scan(NodeKind::Method), filter_has_callee()]),
        chain(vec![scan(NodeKind::Method), filter_in_file("src/**")]),
    ];

    let batch = fuse_plans(plans);
    assert_eq!(batch.groups()[0].prefix(), &scan(NodeKind::Function));
    assert_eq!(batch.groups()[1].prefix(), &scan(NodeKind::Method));
}

// ---------------------------------------------------------------------------
// Serde round-trip — JSON (used by structured logs / debug surfaces)
// ---------------------------------------------------------------------------

#[test]
fn fused_plan_batch_round_trips_through_serde_json() {
    let inner = scan(NodeKind::Method);
    let plans = vec![
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        chain(vec![scan(NodeKind::Function), filter_has_callee()]),
        chain(vec![
            scan(NodeKind::Class),
            PlanNode::Filter {
                predicate: callers_subquery_pred(inner),
            },
        ]),
        standalone(PlanNode::SetOp {
            op: SetOperation::Difference,
            left: Box::new(scan(NodeKind::Function)),
            right: Box::new(scan(NodeKind::Method)),
        }),
    ];

    let batch = fuse_plans(plans);
    let json = serde_json::to_string(&batch).expect("serialize");
    let decoded: FusedPlanBatch = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, batch);
}

#[test]
fn fused_plan_batch_postcard_round_trip_with_shared_nodes() {
    let batch = fuse_plans(vec![
        chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_has_callee(),
        ]),
        chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_in_file("src/**"),
        ]),
    ]);

    assert_eq!(batch.stats().shared_nodes_promoted, 1);
    assert_eq!(batch.shared_nodes().len(), 1);

    let encoded = postcard::to_allocvec(&batch).expect("serialize postcard");
    let decoded: FusedPlanBatch = postcard::from_bytes(&encoded).expect("deserialize postcard");

    assert_eq!(decoded, batch);
}

#[test]
fn shared_nodes_inside_subquery_batch_are_promoted() {
    let sub_left = PlanNode::Chain {
        steps: vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_has_callee(),
        ],
    };
    let sub_right = PlanNode::Chain {
        steps: vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_in_file("src/**"),
        ],
    };
    let plans = vec![
        chain(vec![
            scan(NodeKind::Class),
            PlanNode::Filter {
                predicate: Predicate::Callers(PredicateValue::Subquery(Box::new(sub_left))),
            },
        ]),
        chain(vec![
            scan(NodeKind::Class),
            PlanNode::Filter {
                predicate: Predicate::Callers(PredicateValue::Subquery(Box::new(sub_right))),
            },
        ]),
    ];

    let batch = fuse_plans(plans);
    let sub_batch = batch.subquery_batch().expect("subquery batch");
    let expected = PlanNode::Chain {
        steps: vec![scan(NodeKind::Function), filter_has_caller()],
    };

    assert_eq!(sub_batch.shared_nodes().len(), 1);
    assert_eq!(sub_batch.shared_nodes()[0].canonical_plan(), &expected);
}

#[test]
fn plan_tree_reduction_ratio_is_bounded() {
    let batch = fuse_plans(vec![
        chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_has_callee(),
        ]),
        chain(vec![
            scan(NodeKind::Function),
            filter_has_caller(),
            filter_in_file("src/**"),
        ]),
    ]);

    assert!(batch.stats().plan_tree_reduction_ratio > 0.0);
    assert!(batch.stats().plan_tree_reduction_ratio <= 1.0);
}

// ---------------------------------------------------------------------------
// EdgeKind metadata contract (option-(a) — trust DB10 builder)
// ---------------------------------------------------------------------------

#[test]
fn edge_traversals_with_canonical_metadata_fuse_when_used_as_chain_continuation() {
    // Both plans use the canonical (zeroed) EdgeKind metadata, so their
    // chain-continuation Traversal steps round-trip identically.
    let p1 = chain(vec![
        scan(NodeKind::Function),
        traverse_calls(),
        filter_has_caller(),
    ]);
    let p2 = chain(vec![
        scan(NodeKind::Function),
        traverse_calls(),
        filter_has_callee(),
    ]);

    let batch = fuse_plans(vec![p1.clone(), p2.clone()]);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.stats().scans_eliminated, 1);

    let group = &batch.groups()[0];
    assert_eq!(group.prefix(), &scan(NodeKind::Function));
    // Both tails carry the traversal as the first remaining step.
    for tail in group.tails() {
        match &tail.tail {
            FusionTail::ChainContinuation { remaining_steps } => {
                assert_eq!(&remaining_steps[0], &traverse_calls());
            }
            FusionTail::Identity => panic!("expected ChainContinuation"),
        }
    }
    // Round-trip preserves originals.
    assert_eq!(batch.input_plans(), vec![p1, p2]);
}

#[test]
fn edge_traversals_with_non_canonical_metadata_do_not_share_when_used_as_prefix_via_setop() {
    // Two SetOp prefixes differing only in EdgeKind metadata must NOT
    // fuse — this documents option-(a): the fuser does not normalise.
    let p1 = standalone(PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan(NodeKind::Function)),
        right: Box::new(PlanNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_kind: Some(EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }),
            max_depth: 1,
            resolved_via: None,
        }),
    });
    let p2 = standalone(PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan(NodeKind::Function)),
        right: Box::new(PlanNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_kind: Some(EdgeKind::Calls {
                argument_count: 5, // non-canonical metadata
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }),
            max_depth: 1,
            resolved_via: None,
        }),
    });

    let batch = fuse_plans(vec![p1, p2]);
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.stats().scans_eliminated, 0);
}

// ---------------------------------------------------------------------------
// Convenience / API surface
// ---------------------------------------------------------------------------

#[test]
fn iter_groups_matches_groups_slice() {
    let plans = vec![
        chain(vec![scan(NodeKind::Function), filter_has_caller()]),
        chain(vec![scan(NodeKind::Function), filter_has_callee()]),
        chain(vec![scan(NodeKind::Method), filter_has_caller()]),
    ];
    let batch = fuse_plans(plans);
    let via_iter: Vec<_> = batch.iter_groups().collect();
    assert_eq!(via_iter.len(), batch.groups().len());
    for (a, b) in via_iter.iter().zip(batch.groups().iter()) {
        assert_eq!(*a, b);
    }
}

#[test]
fn fuse_single_is_equivalent_to_fuse_plans_with_one_element() {
    let plan = chain(vec![scan(NodeKind::Function), filter_has_caller()]);
    let a = fuse_single(plan.clone());
    let b = fuse_plans(vec![plan]);
    assert_eq!(a, b);
}
