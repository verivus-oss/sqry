//! Integration tests for the Phase β joint-stubs planner predicates added
//! in V12: `Predicate::FrameworkEq` (Plan A) and `Predicate::ResolvedViaEq`
//! (Plan B set-membership form).
//!
//! Surfaces under test:
//!
//! * `Predicate::FrameworkEq(FrameworkId)` — node-level probe against the
//!   per-node `NodeMetadataStore::framework_routes` BTreeMap. A node
//!   satisfies the predicate iff a `FrameworkRouteMetadata` entry is
//!   recorded for it AND its `framework_id` field equals the target.
//!
//! * `Predicate::ResolvedViaEq(Vec<ResolvedVia>)` — node-level probe over
//!   outgoing `Calls` edges. A node satisfies the predicate iff at least
//!   one outgoing `Calls` edge carries a `resolved_via` value that is a
//!   member of the requested set (OR-semantics across the vec).
//!
//! Coverage shape (mirrors `phase_a_planner_predicates.rs`):
//!
//! * Empty input — predicate evaluated against zero matching nodes.
//! * Match path — fixture has marked nodes / edges; predicate returns
//!   exactly the marked set.
//! * Non-match path — same fixture with a different target; predicate
//!   returns the empty set.
//! * Multi-target (ResolvedViaEq) — OR-semantics across the requested
//!   variants verified.
//! * Filter composition — `kind:function` chained with the new predicate
//!   AND a second filter; intersection verified.
//!
//! The data-population direction (Plan A's framework-route extractors,
//! Plan B's dispatch resolvers) is exercised in this test via direct
//! `framework_routes_mut().insert(...)` and `add_edge(... Calls { ... })`
//! calls on the test fixture. The evaluation logic under test is the
//! same path the production extractors / resolvers will hit once they
//! land; this is the predicate-evaluation contract — not a stub.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, HttpMethod, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::schema::{FrameworkId, FrameworkRouteMetadata};

use sqry_db::planner::{Predicate, execute_plan, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};

// ---------------------------------------------------------------------------
// Fixture builder
// ---------------------------------------------------------------------------
//
// Six function nodes in `app.py`:
//
//   * `flask_index`  — framework-routed under FrameworkId::Flask
//   * `flask_users`  — framework-routed under FrameworkId::Flask
//   * `django_root`  — framework-routed under FrameworkId::Django
//   * `dispatcher`   — emits a Calls(Direct) edge to `target_a`
//                       and a Calls(VirtualDispatch) edge to `target_b`
//   * `forwarder`    — emits a single Calls(InterfaceDispatch) edge to
//                       `target_c`
//   * `unrelated`    — no framework routing, no outgoing Calls edges
//
// Target nodes:
//
//   * `target_a`, `target_b`, `target_c` — bare functions (Calls edge
//     destinations only; no outgoing edges).
//
// The fixture exercises both predicates against the same graph so the
// AND-composition path can be tested end-to-end.

struct Fixture {
    db: QueryDb,
    flask_index: NodeId,
    flask_users: NodeId,
    django_root: NodeId,
    dispatcher: NodeId,
    forwarder: NodeId,
    unrelated: NodeId,
    target_a: NodeId,
    target_b: NodeId,
    target_c: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();

        let app_file = graph
            .files_mut()
            .register_with_language(Path::new("app.py"), Some(Language::Python))
            .expect("register app");

        let intern = |g: &mut CodeGraph, s: &str| g.strings_mut().intern(s).expect("intern");

        let flask_index_name = intern(&mut graph, "flask_index");
        let flask_users_name = intern(&mut graph, "flask_users");
        let django_root_name = intern(&mut graph, "django_root");
        let dispatcher_name = intern(&mut graph, "dispatcher");
        let forwarder_name = intern(&mut graph, "forwarder");
        let unrelated_name = intern(&mut graph, "unrelated");
        let target_a_name = intern(&mut graph, "target_a");
        let target_b_name = intern(&mut graph, "target_b");
        let target_c_name = intern(&mut graph, "target_c");

        let alloc_fn = |g: &mut CodeGraph, name_id, start: u32| {
            let entry = NodeEntry::new(NodeKind::Function, name_id, app_file)
                .with_qualified_name(name_id)
                .with_byte_range(start, start + 50);
            g.nodes_mut().alloc(entry).expect("alloc")
        };

        let flask_index = alloc_fn(&mut graph, flask_index_name, 10);
        let flask_users = alloc_fn(&mut graph, flask_users_name, 70);
        let django_root = alloc_fn(&mut graph, django_root_name, 130);
        let dispatcher = alloc_fn(&mut graph, dispatcher_name, 190);
        let forwarder = alloc_fn(&mut graph, forwarder_name, 250);
        let unrelated = alloc_fn(&mut graph, unrelated_name, 310);
        let target_a = alloc_fn(&mut graph, target_a_name, 370);
        let target_b = alloc_fn(&mut graph, target_b_name, 430);
        let target_c = alloc_fn(&mut graph, target_c_name, 490);

        // Mirror the by-kind index so `kind:function` scans pick all
        // nine nodes up.
        for (id, name_id) in [
            (flask_index, flask_index_name),
            (flask_users, flask_users_name),
            (django_root, django_root_name),
            (dispatcher, dispatcher_name),
            (forwarder, forwarder_name),
            (unrelated, unrelated_name),
            (target_a, target_a_name),
            (target_b, target_b_name),
            (target_c, target_c_name),
        ] {
            graph
                .indices_mut()
                .add(id, NodeKind::Function, name_id, Some(name_id), app_file);
        }

        // Plan A: populate the framework-routes side store directly.
        // This mirrors what the downstream Phase 4f extractor pass will
        // do; the predicate-evaluation contract under test consumes the
        // same `framework_routes()` accessor the executor reads in
        // `sqry-db/src/planner/execute.rs::check_predicate`.
        graph.macro_metadata_mut().framework_routes_mut().insert(
            flask_index,
            FrameworkRouteMetadata::new(FrameworkId::Flask, "/", HttpMethod::Get),
        );
        graph.macro_metadata_mut().framework_routes_mut().insert(
            flask_users,
            FrameworkRouteMetadata::new(FrameworkId::Flask, "/users/{id}", HttpMethod::Get),
        );
        graph.macro_metadata_mut().framework_routes_mut().insert(
            django_root,
            FrameworkRouteMetadata::new(FrameworkId::Django, "/admin/", HttpMethod::Get),
        );

        // Plan B: emit Calls edges with the Phase β resolved-via
        // variants. `dispatcher` covers two distinct variants; the
        // node-level set-membership predicate must OR across them.
        graph.edges().add_edge(
            dispatcher,
            target_a,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            app_file,
        );
        graph.edges().add_edge(
            dispatcher,
            target_b,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: false,
                resolved_via: ResolvedVia::VirtualDispatch,
            },
            app_file,
        );
        graph.edges().add_edge(
            forwarder,
            target_c,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::InterfaceDispatch,
            },
            app_file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(snapshot, QueryDbConfig::default());

        Fixture {
            db,
            flask_index,
            flask_users,
            django_root,
            dispatcher,
            forwarder,
            unrelated,
            target_a,
            target_b,
            target_c,
        }
    }

    fn run_plan(&self, plan: &sqry_db::planner::QueryPlan) -> Vec<NodeId> {
        execute_plan(plan, &self.db)
    }
}

fn sorted(mut v: Vec<NodeId>) -> Vec<NodeId> {
    v.sort_unstable_by_key(|id| (id.index(), id.generation()));
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// `FrameworkEq` (Plan A)
// ---------------------------------------------------------------------------
//
// The predicate is evaluated by
// `CompiledPredicate::FrameworkEq(framework) => snapshot
//      .macro_metadata().framework_route(node_id)
//      .is_some_and(|meta| meta.framework_id == *framework)`
// in `sqry-db/src/planner/execute.rs:587`. The accessor returns
// `None` when no entry is recorded — that is the empty-input path.

#[test]
fn framework_eq_returns_matching_nodes_for_flask() {
    let fx = Fixture::build();

    // Build the plan via the IR directly — Phase β predicates do not
    // (yet) have a text-syntax surface; `overlay_phase_beta_filters`
    // appends them as a trailing Filter step in the production path.
    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Flask),
            },
        ],
    });

    let actual = fx.run_plan(&plan);
    assert_eq!(actual, sorted(vec![fx.flask_index, fx.flask_users]));
}

#[test]
fn framework_eq_returns_matching_nodes_for_django() {
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Django),
            },
        ],
    });

    let actual = fx.run_plan(&plan);
    assert_eq!(actual, sorted(vec![fx.django_root]));
}

#[test]
fn framework_eq_returns_empty_for_unrecorded_framework() {
    // Spring is a valid FrameworkId discriminant but the fixture
    // records no Spring routes. The predicate must return the empty
    // set, not error out.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Spring),
            },
        ],
    });

    assert!(fx.run_plan(&plan).is_empty());
}

#[test]
fn framework_eq_excludes_unrouted_nodes() {
    // `dispatcher`, `forwarder`, `unrelated`, `target_*` are all
    // `Function` kind but have NO framework-route entry. A bare
    // `framework_route()` lookup returns `None` for these and the
    // `.is_some_and(...)` evaluation falls through to `false`.
    // Pins the empty-input arm of the predicate.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Flask),
            },
        ],
    });
    let actual = fx.run_plan(&plan);

    // Result must contain Flask-routed nodes only.
    assert!(actual.contains(&fx.flask_index));
    assert!(actual.contains(&fx.flask_users));

    // Result must EXCLUDE every unrouted Function node.
    for missing in [
        fx.django_root, // routed but under a different framework
        fx.dispatcher,
        fx.forwarder,
        fx.unrelated,
        fx.target_a,
        fx.target_b,
        fx.target_c,
    ] {
        assert!(
            !actual.contains(&missing),
            "node {missing:?} should not satisfy FrameworkEq(Flask)",
        );
    }
}

#[test]
fn framework_eq_returns_empty_on_graph_with_zero_routes() {
    // Build a graph with no framework_routes entries at all — pins the
    // empty-store path (matches the Plan-A-not-yet-shipped state).
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("empty.py"), Some(Language::Python))
        .expect("register");
    let name = graph.strings_mut().intern("only_fn").expect("intern");
    let entry = NodeEntry::new(NodeKind::Function, name, file)
        .with_qualified_name(name)
        .with_byte_range(0, 10);
    let only_fn = graph.nodes_mut().alloc(entry).expect("alloc");
    graph
        .indices_mut()
        .add(only_fn, NodeKind::Function, name, Some(name), file);

    let snapshot = Arc::new(graph.snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Flask),
            },
        ],
    });

    let actual = execute_plan(&plan, &db);
    assert!(
        actual.is_empty(),
        "FrameworkEq must return empty on an empty framework_routes store, got {actual:?}",
    );
}

// ---------------------------------------------------------------------------
// `ResolvedViaEq` (Plan B set-membership form)
// ---------------------------------------------------------------------------
//
// The predicate is evaluated by
// `CompiledPredicate::ResolvedViaEq(set) => set.iter().any(|via|
//      self.node_has_calls_resolved_via(node_id, *via))`
// in `sqry-db/src/planner/execute.rs:599`. The helper walks outgoing
// edges from the candidate node and returns true as soon as it finds
// the first `Calls` edge whose `resolved_via` field matches.

#[test]
fn resolved_via_eq_returns_matching_nodes_for_direct_only() {
    // The fixture has exactly one outgoing Direct Calls edge sourced
    // from `dispatcher` (→ target_a). The set-membership predicate
    // with set = [Direct] must therefore return {dispatcher}.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::ResolvedViaEq(vec![ResolvedVia::Direct]),
            },
        ],
    });

    assert_eq!(fx.run_plan(&plan), sorted(vec![fx.dispatcher]));
}

#[test]
fn resolved_via_eq_or_semantics_across_multiple_variants() {
    // Set = [VirtualDispatch, InterfaceDispatch] must match
    // {dispatcher (via VirtualDispatch → target_b), forwarder (via
    // InterfaceDispatch → target_c)} — OR-semantics across the vec.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::ResolvedViaEq(vec![
                    ResolvedVia::VirtualDispatch,
                    ResolvedVia::InterfaceDispatch,
                ]),
            },
        ],
    });

    assert_eq!(
        fx.run_plan(&plan),
        sorted(vec![fx.dispatcher, fx.forwarder]),
    );
}

#[test]
fn resolved_via_eq_returns_empty_for_unmatched_variant() {
    // The fixture has no DuckTyped Calls edges. The set-membership
    // predicate with set = [DuckTyped] must return the empty set.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::ResolvedViaEq(vec![ResolvedVia::DuckTyped]),
            },
        ],
    });

    assert!(fx.run_plan(&plan).is_empty());
}

#[test]
fn resolved_via_eq_returns_empty_for_empty_target_set() {
    // Set = [] (no requested variants) is the degenerate case: the
    // `any(|via| ...)` short-circuit on an empty iterator returns
    // `false`, so the filter excludes every node.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::ResolvedViaEq(vec![]),
            },
        ],
    });

    assert!(fx.run_plan(&plan).is_empty());
}

#[test]
fn resolved_via_eq_excludes_nodes_without_outgoing_calls() {
    // `target_a`/`target_b`/`target_c` are Calls-edge destinations,
    // not sources — they emit zero outgoing edges. The predicate must
    // never include them.
    let fx = Fixture::build();

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::ResolvedViaEq(vec![
                    ResolvedVia::Direct,
                    ResolvedVia::VirtualDispatch,
                    ResolvedVia::InterfaceDispatch,
                ]),
            },
        ],
    });

    let actual = fx.run_plan(&plan);
    for tgt in [fx.target_a, fx.target_b, fx.target_c] {
        assert!(
            !actual.contains(&tgt),
            "node {tgt:?} has no outgoing Calls edges; must not satisfy ResolvedViaEq",
        );
    }
    // `unrelated` also emits no outgoing edges.
    assert!(!actual.contains(&fx.unrelated));
}

// ---------------------------------------------------------------------------
// Filter composition — AND-intersection
// ---------------------------------------------------------------------------
//
// `overlay_phase_beta_filters` appends BOTH predicates as a single
// `Predicate::And(...)` filter step when both MCP params are present.
// This test pins that the executor's `Predicate::And` short-circuits
// correctly across the two new predicates.

#[test]
fn framework_eq_and_resolved_via_eq_intersect_correctly() {
    // Augment the fixture: give `flask_users` an outgoing
    // Calls(VirtualDispatch) edge to `target_b`. Now the AND of
    // FrameworkEq(Flask) AND ResolvedViaEq([VirtualDispatch]) must
    // return exactly {flask_users} — `flask_index` is Flask-routed but
    // has no outgoing Calls edges; `dispatcher` has the VirtualDispatch
    // edge but is not framework-routed.
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register_with_language(Path::new("composed.py"), Some(Language::Python))
        .expect("register");
    let intern = |g: &mut CodeGraph, s: &str| g.strings_mut().intern(s).expect("intern");

    let flask_index_name = intern(&mut graph, "flask_index");
    let flask_users_name = intern(&mut graph, "flask_users");
    let dispatcher_name = intern(&mut graph, "dispatcher");
    let target_name = intern(&mut graph, "target");

    let alloc_fn = |g: &mut CodeGraph, name_id, start: u32| {
        let entry = NodeEntry::new(NodeKind::Function, name_id, file)
            .with_qualified_name(name_id)
            .with_byte_range(start, start + 50);
        g.nodes_mut().alloc(entry).expect("alloc")
    };

    let flask_index = alloc_fn(&mut graph, flask_index_name, 10);
    let flask_users = alloc_fn(&mut graph, flask_users_name, 70);
    let dispatcher = alloc_fn(&mut graph, dispatcher_name, 130);
    let target = alloc_fn(&mut graph, target_name, 190);

    for (id, name_id) in [
        (flask_index, flask_index_name),
        (flask_users, flask_users_name),
        (dispatcher, dispatcher_name),
        (target, target_name),
    ] {
        graph
            .indices_mut()
            .add(id, NodeKind::Function, name_id, Some(name_id), file);
    }

    // Routes.
    graph.macro_metadata_mut().framework_routes_mut().insert(
        flask_index,
        FrameworkRouteMetadata::new(FrameworkId::Flask, "/", HttpMethod::Get),
    );
    graph.macro_metadata_mut().framework_routes_mut().insert(
        flask_users,
        FrameworkRouteMetadata::new(FrameworkId::Flask, "/users", HttpMethod::Get),
    );

    // `flask_users` AND `dispatcher` both emit a VirtualDispatch Calls
    // edge to `target`. The framework-route filter must narrow the
    // ResolvedVia result.
    graph.edges().add_edge(
        flask_users,
        target,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::VirtualDispatch,
        },
        file,
    );
    graph.edges().add_edge(
        dispatcher,
        target,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::VirtualDispatch,
        },
        file,
    );

    let snapshot = Arc::new(graph.snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    let plan = sqry_db::planner::QueryPlan::new(sqry_db::planner::PlanNode::Chain {
        steps: vec![
            sqry_db::planner::PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::And(vec![
                    Predicate::FrameworkEq(FrameworkId::Flask),
                    Predicate::ResolvedViaEq(vec![ResolvedVia::VirtualDispatch]),
                ]),
            },
        ],
    });

    let actual = execute_plan(&plan, &db);
    assert_eq!(
        actual,
        sorted(vec![flask_users]),
        "AND intersection must return only the node that satisfies both predicates",
    );
}

#[test]
fn framework_eq_composes_with_kind_filter() {
    // `kind:function` + FrameworkEq(Flask) returns the Flask-routed
    // Functions only. Verifies the new predicate composes cleanly with
    // the existing text-syntax NodeScan filter via the production
    // Chain → Filter shape.
    let fx = Fixture::build();

    // Parse the kind:function NodeScan via the text parser, then graft
    // the FrameworkEq filter manually — the predicate has no text
    // syntax surface in this PR.
    let mut plan = parse_query("kind:function").expect("parse");
    let scan = plan.root.clone();
    plan.root = sqry_db::planner::PlanNode::Chain {
        steps: vec![
            scan,
            sqry_db::planner::PlanNode::Filter {
                predicate: Predicate::FrameworkEq(FrameworkId::Flask),
            },
        ],
    };

    assert_eq!(
        execute_plan(&plan, &fx.db),
        sorted(vec![fx.flask_index, fx.flask_users]),
    );
}
