//! TC11: cross-boundary edge discrimination on `EdgeTraversal` (L0 P2).
//!
//! Drives the `cross_boundary` criterion end to end through the production
//! backend: builder -> `RuleNode::EdgeTraversal` -> dispatcher execution handler
//! -> `EdgeFilter.cross_boundary` -> kernel BFS. Also proves the load-bearing
//! invariant that a `cross_boundary`-carrying traversal refuses planner lowering
//! and takes the witness-bearing backend path (the planner's separate
//! `run_traversal` BFS has no `EdgeFilter` and would silently drop the
//! criterion).

mod common;

use std::collections::HashSet;

use sqry_db::planner::{Direction, StringPattern};

use sqry_rules::ir::RuleNode;
use sqry_rules::witness::RuleStep;
use sqry_rules::{RuleBuilder, RuleEngine, RuleOutput, SqryDbRuleBackend};

use common::CrossBoundaryFixture;

/// Builds a `scan(root) -> traverse_cross_boundary` plan and runs it against the
/// cross-boundary star fixture through the default backend, returning the SAME
/// fixture (so callers assert against its NodeIds, not a second isomorphic one),
/// the output node set, and whether the witness recorded per-edge traversal steps.
fn run_cross_boundary(
    cross_boundary: Option<bool>,
) -> (
    CrossBoundaryFixture,
    HashSet<sqry_core::graph::unified::NodeId>,
    bool,
) {
    let fixture = common::cross_boundary_fixture();
    let db = common::query_db_for(std::sync::Arc::clone(&fixture.snapshot));
    let backend = SqryDbRuleBackend::new(&db);

    let plan = RuleBuilder::new()
        .scan_with(
            Some(sqry_core::graph::unified::NodeKind::Function),
            None,
            Some(StringPattern::exact("root")),
        )
        .traverse_cross_boundary(Direction::Forward, None, 2, cross_boundary)
        .build()
        .expect("cross-boundary plan builds");

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("cross-boundary rule runs against the default backend");

    let nodes = match run.output {
        RuleOutput::Nodes(nodes) => nodes.into_iter().collect(),
        other => panic!("expected node set, got {other:?}"),
    };
    let has_edge_traversed = run
        .witness
        .steps
        .iter()
        .any(|step| matches!(step, RuleStep::EdgeTraversed { .. }));
    (fixture, nodes, has_edge_traversed)
}

#[test]
fn cross_boundary_true_reaches_only_ffi_and_db_targets() {
    let (fixture, nodes, witnessed) = run_cross_boundary(Some(true));

    assert!(
        nodes.contains(&fixture.ffi_target),
        "FFI target is cross-boundary and must be reached"
    );
    assert!(
        nodes.contains(&fixture.db_target),
        "DB target is cross-boundary and must be reached"
    );
    assert!(
        !nodes.contains(&fixture.intra_target),
        "intra-language call target must be excluded when cross_boundary is true"
    );
    assert!(
        witnessed,
        "a cross_boundary traversal must take the witness-bearing backend path"
    );
}

#[test]
fn cross_boundary_false_reaches_only_intra_target() {
    let (fixture, nodes, witnessed) = run_cross_boundary(Some(false));

    assert!(
        nodes.contains(&fixture.intra_target),
        "intra-language call target must be reached when cross_boundary is false"
    );
    assert!(
        !nodes.contains(&fixture.ffi_target),
        "FFI target must be excluded when cross_boundary is false"
    );
    assert!(
        !nodes.contains(&fixture.db_target),
        "DB target must be excluded when cross_boundary is false"
    );
    assert!(
        witnessed,
        "a cross_boundary traversal must take the witness-bearing backend path"
    );
}

#[test]
fn cross_boundary_none_is_boundary_agnostic_and_lowers_to_the_planner() {
    let (fixture, nodes, witnessed) = run_cross_boundary(None);

    assert!(nodes.contains(&fixture.ffi_target));
    assert!(nodes.contains(&fixture.db_target));
    assert!(nodes.contains(&fixture.intra_target));
    // With no cross_boundary criterion and no edge_class, the chain lowers to a
    // planner plan and runs via `run_plan`, which emits a `NodeScanMatched`
    // envelope step but no per-edge `EdgeTraversed` steps. The presence of
    // per-edge witness only for the Some(_) cases is the observable proof that
    // the criterion forces the witness path (planner lowering refuses it).
    assert!(
        !witnessed,
        "a boundary-agnostic traversal lowers to the planner (no per-edge witness)"
    );
}

#[test]
fn lowering_refusal_is_specifically_the_cross_boundary_criterion() {
    // Same plan shape, differing only in the cross_boundary field: Some(_)
    // takes the witness path, None lowers. This isolates the refuse to the new
    // criterion and nothing else in the traversal.
    let (_true_fixture, _true_nodes, true_witnessed) = run_cross_boundary(Some(true));
    let (_none_fixture, _none_nodes, none_witnessed) = run_cross_boundary(None);
    assert!(true_witnessed, "Some(true) forces the witness path");
    assert!(!none_witnessed, "None lowers to the planner");
}

#[test]
fn builder_traverse_defaults_leave_cross_boundary_none() {
    // The pre-existing `traverse` constructor must not silently opt into the
    // criterion; only `traverse_cross_boundary` sets it.
    let plan = RuleBuilder::new()
        .scan_all()
        .traverse(Direction::Forward, None, 1)
        .build()
        .expect("plain traverse builds");
    match plan.root() {
        RuleNode::Chain { steps } => match &steps[1] {
            RuleNode::EdgeTraversal { cross_boundary, .. } => {
                assert_eq!(*cross_boundary, None, "plain traverse defaults to None");
            }
            other => panic!("expected EdgeTraversal, got {other:?}"),
        },
        other => panic!("expected chain, got {other:?}"),
    }
}
