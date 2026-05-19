//! Integration tests for the Phase A C indirect-call-precision planner
//! predicates added in U14.
//!
//! Surfaces under test (DESIGN §11.1–§11.4):
//!
//! * `address_taken:true|false` → `Predicate::IsAddressTaken`
//! * `resolved_via:direct|type_match|binding_plane` →
//!   `Predicate::ResolvedVia`
//! * `callsite_promiscuous:true|false` →
//!   `Predicate::HasCallsitePromiscuous`
//!
//! These tests pair the parser surface (locked spellings per DESIGN §11.1)
//! with the executor's metadata-store / Calls-edge field probes, and
//! verify the fuse heuristic from the U14 DAG acceptance:
//!
//! > `kind:function address_taken:true` collapses to a single fused
//! > NodeScan over `by_kind(Function)` filtered by `is_address_taken`
//! > — no edge walk required.
//!
//! The fuse acceptance is exercised by submitting two plans whose
//! `kind:function` NodeScan prefix is structurally identical; the
//! existing prefix-sharing fuser then folds the prefix into a single
//! group with `scans_eliminated == 1`. Atomic node-flag filters never
//! widen the shared prefix, so they belong to the tail.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{PlanNode, Predicate, execute_plan, fuse_plans, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};

// ---------------------------------------------------------------------------
// Fixture builder
// ---------------------------------------------------------------------------

/// Six C functions in `lib.c`.
///
/// * `alpha`, `beta`, `gamma`, `kappa` — address-taken (MARKED).
/// * `delta`, `epsilon`                — NOT address-taken.
/// * `epsilon`                         — `callsite_promiscuous` flagged.
///
/// Calls edges (illustrate every resolved_via flavour):
///
/// * `delta`   --Calls(Direct)--> `alpha`
/// * `delta`   --Calls(TypeMatch)--> `beta`
/// * `epsilon` --Calls(BindingPlane)--> `gamma`
///
/// `kappa` is deliberately isolated — address-taken marked, but it has
/// ZERO incoming `Calls` edges and emits no outgoing edges. It pins the
/// O(1) metadata-flag execution path for `IsAddressTaken`: if the
/// executor accidentally enumerated candidates by walking reverse
/// `Calls` edges into `Function` nodes (as a previous regression might
/// suggest), `kappa` would be missed. Its presence in the result set
/// therefore witnesses that the filter consults the macro-metadata
/// store directly — codex iter-1 LOW (DESIGN §11.2).
///
/// This fixture lets each U14 predicate be exercised independently: the
/// address-taken set is exactly {alpha, beta, gamma, kappa}; the
/// BindingPlane resolved_via set (sources only) is exactly {epsilon};
/// the callsite-promiscuous set is exactly {epsilon}.
struct Fixture {
    db: QueryDb,
    alpha: NodeId,
    beta: NodeId,
    gamma: NodeId,
    delta: NodeId,
    epsilon: NodeId,
    kappa: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();

        let lib_file = graph
            .files_mut()
            .register_with_language(Path::new("lib.c"), Some(Language::C))
            .expect("register lib");

        let intern = |g: &mut CodeGraph, s: &str| g.strings_mut().intern(s).expect("intern");

        let alpha_name = intern(&mut graph, "alpha");
        let beta_name = intern(&mut graph, "beta");
        let gamma_name = intern(&mut graph, "gamma");
        let delta_name = intern(&mut graph, "delta");
        let epsilon_name = intern(&mut graph, "epsilon");
        let kappa_name = intern(&mut graph, "kappa");

        let alloc_fn = |g: &mut CodeGraph, name_id, start: u32| {
            let entry = NodeEntry::new(NodeKind::Function, name_id, lib_file)
                .with_qualified_name(name_id)
                .with_byte_range(start, start + 50);
            g.nodes_mut().alloc(entry).expect("alloc")
        };

        let alpha = alloc_fn(&mut graph, alpha_name, 10);
        let beta = alloc_fn(&mut graph, beta_name, 70);
        let gamma = alloc_fn(&mut graph, gamma_name, 130);
        let delta = alloc_fn(&mut graph, delta_name, 190);
        let epsilon = alloc_fn(&mut graph, epsilon_name, 250);
        let kappa = alloc_fn(&mut graph, kappa_name, 310);

        // Mirror the by-kind index for these nodes so `kind:function`
        // scans pick them up.
        for (id, name_id) in [
            (alpha, alpha_name),
            (beta, beta_name),
            (gamma, gamma_name),
            (delta, delta_name),
            (epsilon, epsilon_name),
            (kappa, kappa_name),
        ] {
            graph
                .indices_mut()
                .add(id, NodeKind::Function, name_id, Some(name_id), lib_file);
        }

        // Address-taken flags (Tier-3 metadata).
        graph.macro_metadata_mut().mark_address_taken(alpha);
        graph.macro_metadata_mut().mark_address_taken(beta);
        graph.macro_metadata_mut().mark_address_taken(gamma);
        // `kappa` is address-taken AND has zero inbound `Calls` edges —
        // its membership in the result pins the O(1) flag-lookup path.
        graph.macro_metadata_mut().mark_address_taken(kappa);
        // Callsite-promiscuous flag.
        graph
            .macro_metadata_mut()
            .mark_callsite_promiscuous(epsilon);

        // Calls edges with every resolved_via flavour.
        graph.edges().add_edge(
            delta,
            alpha,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            lib_file,
        );
        graph.edges().add_edge(
            delta,
            beta,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: false,
                resolved_via: ResolvedVia::TypeMatch,
            },
            lib_file,
        );
        graph.edges().add_edge(
            epsilon,
            gamma,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::BindingPlane,
            },
            lib_file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(snapshot, QueryDbConfig::default());

        Fixture {
            db,
            alpha,
            beta,
            gamma,
            delta,
            epsilon,
            kappa,
        }
    }

    fn run(&self, source: &str) -> Vec<NodeId> {
        let plan = parse_query(source).expect("parse");
        execute_plan(&plan, &self.db)
    }
}

fn sorted(mut v: Vec<NodeId>) -> Vec<NodeId> {
    v.sort_unstable_by_key(|id| (id.index(), id.generation()));
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// `address_taken` — DAG acceptance criterion 2.
// ---------------------------------------------------------------------------

#[test]
fn query_address_taken_set_returns_marked_functions() {
    let fx = Fixture::build();

    // `kind:function address_taken:true` returns exactly the
    // address-taken set: {alpha, beta, gamma, kappa}.
    //
    // `kappa` has ZERO incoming `Calls` edges by construction (see
    // Fixture rustdoc). Its presence here pins the O(1) metadata-flag
    // execution path — if the executor accidentally enumerated
    // candidates by walking reverse `Calls` edges, `kappa` would be
    // missed and this assertion would fail. Addresses codex iter-1
    // LOW (IMPL/U14-planner-predicates).
    let actual = fx.run("kind:function address_taken:true");
    assert_eq!(actual, sorted(vec![fx.alpha, fx.beta, fx.gamma, fx.kappa]));
    assert!(
        actual.contains(&fx.kappa),
        "address-taken result must include `kappa` (no inbound Calls edges) — \
         witnesses O(1) flag lookup path",
    );

    // Bare form `address_taken` defaults to `:true` per DESIGN §11.1.
    let bare = fx.run("kind:function address_taken");
    assert_eq!(bare, sorted(vec![fx.alpha, fx.beta, fx.gamma, fx.kappa]));

    // Negative polarity: not-address-taken functions.
    let negated = fx.run("kind:function address_taken:false");
    assert_eq!(negated, sorted(vec![fx.delta, fx.epsilon]));
}

// ---------------------------------------------------------------------------
// `resolved_via` — DAG acceptance criterion 3.
// ---------------------------------------------------------------------------

#[test]
fn query_resolved_via_binding_plane_filters_calls_edges() {
    let fx = Fixture::build();

    // `resolved_via:binding_plane` filter form selects nodes which
    // source at least one outgoing BindingPlane Calls edge. Only
    // `epsilon` calls `gamma` via BindingPlane, so the result is
    // exactly {epsilon}.
    let bp = fx.run("kind:function resolved_via:binding_plane");
    assert_eq!(bp, sorted(vec![fx.epsilon]));

    // Sanity: `resolved_via:type_match` selects {delta} (delta -> beta
    // is the only TypeMatch caller in the fixture).
    let tm = fx.run("kind:function resolved_via:type_match");
    assert_eq!(tm, sorted(vec![fx.delta]));

    // Sanity: `resolved_via:direct` selects {delta} (delta -> alpha is
    // the only Direct caller in the fixture). alpha, beta, gamma, and
    // kappa are address-taken targets but emit no outgoing Calls edges
    // so they never appear as sources of resolved_via probes.
    let direct = fx.run("kind:function resolved_via:direct");
    assert_eq!(direct, sorted(vec![fx.delta]));
}

// ---------------------------------------------------------------------------
// `callsite_promiscuous` — additional executor coverage (Phase A flag).
// ---------------------------------------------------------------------------

#[test]
fn query_callsite_promiscuous_returns_marked_callers() {
    let fx = Fixture::build();

    let actual = fx.run("kind:function callsite_promiscuous:true");
    assert_eq!(actual, sorted(vec![fx.epsilon]));

    // Negative polarity returns every non-promiscuous function.
    let negated = fx.run("kind:function callsite_promiscuous:false");
    assert_eq!(
        negated,
        sorted(vec![fx.alpha, fx.beta, fx.gamma, fx.delta, fx.kappa]),
    );
}

// ---------------------------------------------------------------------------
// Fuse heuristic — DAG acceptance criterion 4.
// ---------------------------------------------------------------------------
//
// "`kind:function address_taken:true` collapses to a single fused
// NodeScan over `by_kind(Function)` filtered by `is_address_taken` —
// no edge walk required."
//
// Two cooperating properties make this true:
//
//   1. The planner's prefix-sharing fuser groups plans whose first
//      context-free step is structurally identical (see
//      `fusion_test.rs::two_plans_with_identical_node_scan_prefix_*`).
//      `kind:function` is the leading NodeScan in both U14 surface
//      queries and in any future query that combines an
//      address-taken predicate with another tail step, so they share
//      a single scan.
//   2. The executor's filter implementation for `IsAddressTaken`
//      consults the metadata store via
//      `GraphSnapshot::macro_metadata().is_address_taken` — an O(1)
//      flag lookup, NOT an edge walk.
//
// This test pins property (1) — that the new predicates do NOT widen
// the fusion-eligible prefix and that two plans sharing `kind:function`
// land in a single fusion group with `scans_eliminated == 1`. Property
// (2) is exercised by the address-taken integration test above
// (correct membership without any reverse-Calls traversal).

#[test]
fn fuse_shares_prefix_when_address_taken_and_another_tail() {
    let p1 = parse_query("kind:function address_taken:true").expect("parse p1");
    let p2 = parse_query("kind:function address_taken:false").expect("parse p2");

    let batch = fuse_plans(vec![p1.clone(), p2.clone()]);

    assert_eq!(batch.stats().total_plans, 2);
    assert_eq!(
        batch.stats().fusion_groups,
        1,
        "both plans share the `kind:function` NodeScan prefix and must collapse to one fusion group",
    );
    assert_eq!(
        batch.stats().scans_eliminated,
        1,
        "fuser must eliminate exactly one redundant `kind:function` NodeScan",
    );

    // The shared prefix is the `kind:function` NodeScan; the
    // address_taken filter must remain in the tail.
    let group = &batch.groups()[0];
    assert!(
        matches!(
            group.prefix(),
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                ..
            }
        ),
        "fuse prefix must be the NodeScan(Function), got {:?}",
        group.prefix(),
    );
    assert_eq!(group.tail_count(), 2);
}

#[test]
fn fuse_shares_prefix_for_address_taken_and_resolved_via() {
    // Mixed Phase A predicates over the same scan prefix must still
    // fuse to one group: the IR shape is `Chain { steps: [scan,
    // filter] }` for both, and only the filter changes.
    let p1 = parse_query("kind:function address_taken:true").expect("parse p1");
    let p2 = parse_query("kind:function resolved_via:binding_plane").expect("parse p2");
    let p3 = parse_query("kind:function callsite_promiscuous:true").expect("parse p3");

    let batch = fuse_plans(vec![p1, p2, p3]);

    assert_eq!(batch.stats().fusion_groups, 1);
    assert_eq!(batch.stats().scans_eliminated, 2);
    assert_eq!(batch.groups()[0].tail_count(), 3);
}

// ---------------------------------------------------------------------------
// Filter step shape — the new predicates always land as `Filter` steps,
// not as part of a NodeScan. Pins the IR-shape contract from DESIGN
// §11.3 used by U15 when folding adjacent `resolved_via` predicates
// into `PlanNode::EdgeTraversal`.
// ---------------------------------------------------------------------------

#[test]
fn parsed_phase_a_predicates_land_as_filter_steps() {
    for (src, predicate) in [
        (
            "kind:function address_taken:true",
            Predicate::IsAddressTaken(true),
        ),
        (
            "kind:function resolved_via:binding_plane",
            Predicate::ResolvedVia(ResolvedVia::BindingPlane),
        ),
        (
            "kind:function callsite_promiscuous:true",
            Predicate::HasCallsitePromiscuous(true),
        ),
    ] {
        let plan = parse_query(src).unwrap_or_else(|err| panic!("parse {src:?}: {err}"));
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root for {src:?}");
        };
        assert_eq!(steps.len(), 2, "{src:?} must lower to NodeScan + Filter");
        match &steps[1] {
            PlanNode::Filter { predicate: actual } => {
                assert_eq!(actual, &predicate, "{src:?} filter predicate mismatch");
            }
            other => panic!("{src:?} step[1] must be Filter, got {other:?}"),
        }
    }
}
