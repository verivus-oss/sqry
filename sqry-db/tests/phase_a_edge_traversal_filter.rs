//! Integration tests for the Phase A C indirect-call-precision
//! `PlanNode::EdgeTraversal.resolved_via` field filter introduced in U15.
//!
//! Mechanism summary (DESIGN §6.3bis):
//!
//! * **Mechanism A** — [`normalize_edge_kind`](sqry_db::planner)
//!   preserves the `resolved_via` semantic discriminator inside
//!   `EdgeKind::Calls`, so two plans differing only in `resolved_via`
//!   hash distinctly (verified in `compile.rs` inline tests).
//! * **Mechanism B** — `run_traversal` accepts an explicit
//!   `Option<ResolvedVia>` parameter (not stashed on executor state;
//!   codex DESIGN-iter-1 BLOCKER) and applies it AFTER the discriminant
//!   filter. Non-`Calls` edge kinds are unaffected.
//!
//! These tests exercise Mechanism B end-to-end against a synthetic
//! `CodeGraph` carrying three `Calls` edges from a single source to
//! three distinct targets, one for each [`ResolvedVia`] variant.
//!
//! Test names start with `traversal_with_resolved_via_` so the U15 DAG
//! acceptance command
//!
//! ```text
//! cargo test -p sqry-db planner::traversal_with_resolved_via
//! ```
//!
//! also resolves through this integration file via the standard
//! `<test_target>::<fn>` filter syntax — `phase_a_edge_traversal_filter`
//! itself contains `traversal_with_resolved_via_*` tests, and the
//! `planner::compile::tests` module additionally houses the unit tests
//! that match the same substring.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{Direction, PlanNode, QueryBuilder, QueryPlan, SetOperation, execute_plan};
use sqry_db::{QueryDb, QueryDbConfig};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Five C functions in `lib.c`. One source emits three `Calls` edges,
/// one per [`ResolvedVia`] variant, to three distinct targets. A
/// fourth target receives an inbound `Imports` edge so we can also
/// witness that non-`Calls` traversals are unaffected by the
/// `resolved_via` field filter (Mechanism B's "non-Calls edges are
/// unaffected" invariant).
///
/// Layout:
///
/// * `caller`          → `direct_tgt`   via `Calls(Direct)`
/// * `caller`          → `tm_tgt`       via `Calls(TypeMatch)`
/// * `caller`          → `bp_tgt`       via `Calls(BindingPlane)`
/// * `import_caller`   → `import_tgt`   via `Imports`
struct Fixture {
    db: QueryDb,
    caller: NodeId,
    direct_tgt: NodeId,
    tm_tgt: NodeId,
    bp_tgt: NodeId,
    import_caller: NodeId,
    import_tgt: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();

        let lib_file = graph
            .files_mut()
            .register_with_language(Path::new("lib.c"), Some(Language::C))
            .expect("register lib");

        let intern = |g: &mut CodeGraph, s: &str| g.strings_mut().intern(s).expect("intern");

        let caller_name = intern(&mut graph, "caller");
        let direct_tgt_name = intern(&mut graph, "direct_tgt");
        let tm_tgt_name = intern(&mut graph, "tm_tgt");
        let bp_tgt_name = intern(&mut graph, "bp_tgt");
        let import_caller_name = intern(&mut graph, "import_caller");
        let import_tgt_name = intern(&mut graph, "import_tgt");

        let alloc_fn = |g: &mut CodeGraph, name_id, start: u32| {
            let entry = NodeEntry::new(NodeKind::Function, name_id, lib_file)
                .with_qualified_name(name_id)
                .with_byte_range(start, start + 50);
            g.nodes_mut().alloc(entry).expect("alloc")
        };

        let caller = alloc_fn(&mut graph, caller_name, 10);
        let direct_tgt = alloc_fn(&mut graph, direct_tgt_name, 70);
        let tm_tgt = alloc_fn(&mut graph, tm_tgt_name, 130);
        let bp_tgt = alloc_fn(&mut graph, bp_tgt_name, 190);
        let import_caller = alloc_fn(&mut graph, import_caller_name, 250);
        let import_tgt = alloc_fn(&mut graph, import_tgt_name, 310);

        // Mirror the by-kind index so `kind:function` scans pick them up.
        for (id, name_id) in [
            (caller, caller_name),
            (direct_tgt, direct_tgt_name),
            (tm_tgt, tm_tgt_name),
            (bp_tgt, bp_tgt_name),
            (import_caller, import_caller_name),
            (import_tgt, import_tgt_name),
        ] {
            graph
                .indices_mut()
                .add(id, NodeKind::Function, name_id, Some(name_id), lib_file);
        }

        // Three Calls edges from `caller` — one per ResolvedVia variant.
        graph.edges().add_edge(
            caller,
            direct_tgt,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            lib_file,
        );
        graph.edges().add_edge(
            caller,
            tm_tgt,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: false,
                resolved_via: ResolvedVia::TypeMatch,
            },
            lib_file,
        );
        graph.edges().add_edge(
            caller,
            bp_tgt,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::BindingPlane,
            },
            lib_file,
        );

        // One non-Calls edge (Imports) — exercises the "non-Calls edges
        // are unaffected by the resolved_via filter" invariant.
        graph.edges().add_edge(
            import_caller,
            import_tgt,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            lib_file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(snapshot, QueryDbConfig::default());

        Fixture {
            db,
            caller,
            direct_tgt,
            tm_tgt,
            bp_tgt,
            import_caller,
            import_tgt,
        }
    }
}

fn sorted(mut v: Vec<NodeId>) -> Vec<NodeId> {
    v.sort_unstable_by_key(|id| (id.index(), id.generation()));
    v.dedup();
    v
}

/// Build a plan that scans only the `caller` node and then traverses
/// outbound `Calls` edges with the requested `resolved_via` filter.
fn caller_then_calls_traverse(
    caller_name_glob: &str,
    resolved_via: Option<ResolvedVia>,
) -> QueryPlan {
    use sqry_db::planner::{Predicate, StringPattern};

    let canonical_calls = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };

    QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::glob(
            caller_name_glob,
        )))
        .traverse_with_resolved_via(Direction::Forward, canonical_calls, resolved_via, 1)
        .build()
        .expect("plan builds")
}

// ---------------------------------------------------------------------------
// Mechanism B — `resolved_via` field filter
// ---------------------------------------------------------------------------

#[test]
fn traversal_with_resolved_via_filter_excludes_other_variants() {
    let fx = Fixture::build();

    // Some(BindingPlane) — only the BindingPlane-target edge survives.
    let plan = caller_then_calls_traverse("caller", Some(ResolvedVia::BindingPlane));
    let bp_only = execute_plan(&plan, &fx.db);
    assert_eq!(sorted(bp_only.clone()), sorted(vec![fx.bp_tgt]));
    assert!(!bp_only.contains(&fx.direct_tgt));
    assert!(!bp_only.contains(&fx.tm_tgt));

    // Some(TypeMatch) — only the TypeMatch-target edge survives.
    let plan = caller_then_calls_traverse("caller", Some(ResolvedVia::TypeMatch));
    let tm_only = execute_plan(&plan, &fx.db);
    assert_eq!(sorted(tm_only), sorted(vec![fx.tm_tgt]));

    // Some(Direct) — only the Direct-target edge survives.
    let plan = caller_then_calls_traverse("caller", Some(ResolvedVia::Direct));
    let direct_only = execute_plan(&plan, &fx.db);
    assert_eq!(sorted(direct_only), sorted(vec![fx.direct_tgt]));
}

#[test]
fn traversal_with_resolved_via_none_is_source_compatible() {
    // U15 source-compat: `traverse(...)` (the 3-arg builder method) and
    // `traverse_with_resolved_via(..., None, ...)` both produce
    // `resolved_via: None` plans which return ALL `Calls` edges
    // regardless of provenance — identical to pre-U15 behavior.
    let fx = Fixture::build();

    let plan = caller_then_calls_traverse("caller", None);
    let all = execute_plan(&plan, &fx.db);
    assert_eq!(
        sorted(all),
        sorted(vec![fx.direct_tgt, fx.tm_tgt, fx.bp_tgt]),
        "resolved_via: None must return every Calls target — same as the \
         legacy `traverse(...)` builder method"
    );

    // Same shape via the legacy `traverse` builder; result must match.
    use sqry_db::planner::{Predicate, StringPattern};
    let canonical_calls = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let legacy_plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::glob("caller")))
        .traverse(Direction::Forward, canonical_calls, 1)
        .build()
        .expect("plan builds");
    let legacy = execute_plan(&legacy_plan, &fx.db);
    assert_eq!(
        sorted(legacy),
        sorted(vec![fx.direct_tgt, fx.tm_tgt, fx.bp_tgt]),
        "legacy `traverse(...)` is the canonical source-compat baseline"
    );
}

#[test]
fn traversal_with_resolved_via_does_not_affect_non_calls_edges() {
    // Mechanism B explicitly states "non-`Calls` edge kinds are
    // unaffected by this filter — the planner only installs
    // `resolved_via: Some(_)` alongside a Calls discriminant filter."
    //
    // This test exercises the safety property: if a caller installs
    // `resolved_via: Some(_)` together with a non-Calls edge_kind
    // filter, the executor's match arm only fires for `EdgeKind::Calls`
    // and leaves Imports / References / etc. untouched. The result
    // should be identical whether the filter is `Some(_)` or `None`.
    let fx = Fixture::build();

    use sqry_db::planner::{Predicate, StringPattern};
    let imports = EdgeKind::Imports {
        alias: None,
        is_wildcard: false,
    };
    let build = |rv: Option<ResolvedVia>| {
        QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::MatchesName(StringPattern::glob("import_caller")))
            .traverse_with_resolved_via(Direction::Forward, imports.clone(), rv, 1)
            .build()
            .expect("plan builds")
    };

    let none_plan = build(None);
    let none_result = execute_plan(&none_plan, &fx.db);
    assert_eq!(sorted(none_result), sorted(vec![fx.import_tgt]));

    let some_plan = build(Some(ResolvedVia::BindingPlane));
    let some_result = execute_plan(&some_plan, &fx.db);
    assert_eq!(
        sorted(some_result),
        sorted(vec![fx.import_tgt]),
        "non-Calls (Imports) edges must be unaffected by the resolved_via \
         field filter"
    );
}

#[test]
fn traversal_with_resolved_via_combines_with_setop_union() {
    // Combining two filtered traversals via SetOp::Union should re-form
    // the unfiltered Calls successor set. Documents that Mechanism B
    // composes orthogonally with set algebra.
    let fx = Fixture::build();

    let direct_plan = caller_then_calls_traverse("caller", Some(ResolvedVia::Direct));
    let tm_plan = caller_then_calls_traverse("caller", Some(ResolvedVia::TypeMatch));
    let bp_plan = caller_then_calls_traverse("caller", Some(ResolvedVia::BindingPlane));

    let union_dt = QueryPlan::new(PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(direct_plan.root.clone()),
        right: Box::new(tm_plan.root.clone()),
    });
    let union_all = QueryPlan::new(PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(union_dt.root),
        right: Box::new(bp_plan.root),
    });
    let result = execute_plan(&union_all, &fx.db);
    assert_eq!(
        sorted(result),
        sorted(vec![fx.direct_tgt, fx.tm_tgt, fx.bp_tgt]),
        "union of all three Mechanism-B filtered plans reconstructs the \
         unfiltered Calls successor set",
    );

    // Sanity — exercises every fixture identifier so an over-zealous
    // dead-code lint never silently masks the layout invariants.
    let _ = (
        fx.caller,
        fx.direct_tgt,
        fx.tm_tgt,
        fx.bp_tgt,
        fx.import_caller,
        fx.import_tgt,
    );
}
