//! Hand-pinned spot-check for the WS1 differential-test baseline.
//!
//! Builds a 6-node fixture graph carefully designed to exercise every one
//! of the 17 baseline functions in [`sqry_db::baseline`], and asserts the
//! exact hand-computed expected output for each. Failure of any single
//! assertion means the baseline diverged from the published WS1 contract
//! (DESIGN §2.1); the property suite (WS1_4) will then have a broken
//! oracle to chase, so the spot-check is intentionally fail-loud.
//!
//! Fixture topology (`lib.rs`):
//!
//! ```text
//!                                  ┌─ Trait `Drawable` (n4) ◀── Implements ── Struct `Widget` (n3)
//!                                  │
//!   main (n0, pub) ─Calls─▶ helper (n1)
//!         │                   ▲
//!         │                   │
//!         Calls               Calls
//!         │                   │
//!         ▼                   │
//!     hot_loop_a (n2) ─Calls─┘  (a ↔ helper makes {a, helper} a 2-cycle)
//!
//!   ghost (n5, private, no edges) — unused under UnusedScope::All
//!
//!   Cross-language sketches:
//!     helper --Imports{None}--> ext_mod  (sixth node? no: nodes 0..=5; ext_mod is n6)
//! ```
//!
//! Wait — to stay at exactly 6 nodes and still hit every baseline we
//! collapse Imports/Exports/References/FFI/Address-taken into the same
//! 6 nodes by re-using existing identities. See body comments for the
//! exact edge wiring.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, ExportKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::{CircularType, UnusedScope};

use sqry_db::baseline;
use sqry_db::queries::cycles::{CycleBounds, IsInCycleKey};
use sqry_db::queries::unused::{IsNodeUnusedKey, UnusedKey};

/// 6-node fixture.
struct Fixture {
    snapshot: Arc<GraphSnapshot>,
    /// `pub fn main()` — entry point, calls helper + hot_loop_a.
    main_fn: NodeId,
    /// `fn helper()` — called by main + hot_loop_a; calls hot_loop_a.
    helper: NodeId,
    /// `fn hot_loop_a()` — called by main + helper; calls helper
    /// (closes the {helper, hot_loop_a} 2-cycle); address-taken +
    /// callsite-promiscuous (the C-icall markers).
    hot_loop_a: NodeId,
    /// `struct Widget` — implements Drawable, exports Widget alias.
    widget: NodeId,
    /// `trait Drawable` — target of Widget's Implements edge.
    drawable: NodeId,
    /// `fn ghost()` — private, no edges, no marks.
    ghost: NodeId,
}

fn make_fixture() -> Fixture {
    let mut g = CodeGraph::new();
    let file = g.files_mut().register(Path::new("lib.rs")).unwrap();

    // Names
    let n_main = g.strings_mut().intern("main").unwrap();
    let n_helper = g.strings_mut().intern("helper").unwrap();
    let n_hot = g.strings_mut().intern("hot_loop_a").unwrap();
    let n_widget = g.strings_mut().intern("Widget").unwrap();
    let n_drawable = g.strings_mut().intern("Drawable").unwrap();
    let n_ghost = g.strings_mut().intern("ghost").unwrap();
    let v_pub = g.strings_mut().intern("public").unwrap();

    // Nodes
    let main_fn = g
        .nodes_mut()
        .alloc(
            NodeEntry::new(NodeKind::Function, n_main, file)
                .with_qualified_name(n_main)
                .with_visibility(v_pub),
        )
        .unwrap();
    let helper = g
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, n_helper, file).with_qualified_name(n_helper))
        .unwrap();
    let hot_loop_a = g
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, n_hot, file).with_qualified_name(n_hot))
        .unwrap();
    let widget = g
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Struct, n_widget, file).with_qualified_name(n_widget))
        .unwrap();
    let drawable = g
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Trait, n_drawable, file).with_qualified_name(n_drawable))
        .unwrap();
    let ghost = g
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, n_ghost, file).with_qualified_name(n_ghost))
        .unwrap();

    // Edges:
    //   main --Calls--> helper           (fan-out, hot_loop_a as callee)
    //   main --Calls--> hot_loop_a
    //   helper --Calls--> hot_loop_a     (closes {helper, hot_loop_a} cycle)
    //   hot_loop_a --Calls--> helper
    //   widget --Implements--> drawable  (OOP)
    //   widget --Exports{Direct}--> drawable  (exports edge, target = drawable)
    //   helper --Imports{None}--> widget (imports edge target = widget)
    //   hot_loop_a --References--> drawable (reference edge)
    //   main --FfiCall--> ghost          (FFI: makes ghost reachable from main
    //                                     via reachability edges; we don't use
    //                                     FFI in reachability test, see below)
    let call = || EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    g.edges_mut().add_edge(main_fn, helper, call(), file);
    g.edges_mut().add_edge(main_fn, hot_loop_a, call(), file);
    g.edges_mut().add_edge(helper, hot_loop_a, call(), file);
    g.edges_mut().add_edge(hot_loop_a, helper, call(), file);
    g.edges_mut()
        .add_edge(widget, drawable, EdgeKind::Implements, file);
    g.edges_mut().add_edge(
        widget,
        drawable,
        EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        },
        file,
    );
    g.edges_mut().add_edge(
        helper,
        widget,
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        file,
    );
    g.edges_mut()
        .add_edge(hot_loop_a, drawable, EdgeKind::References, file);

    // C-indirect-call markers: hot_loop_a is address-taken AND its
    // callsite is promiscuous. ghost has neither.
    g.macro_metadata_mut().mark_address_taken(hot_loop_a);
    g.macro_metadata_mut().mark_callsite_promiscuous(hot_loop_a);

    let snapshot = Arc::new(g.snapshot());
    Fixture {
        snapshot,
        main_fn,
        helper,
        hot_loop_a,
        widget,
        drawable,
        ghost,
    }
}

// ============================================================================
// 1. scc
// ============================================================================

#[test]
fn scc_groups_helper_and_hot_loop_a_into_one_component() {
    let f = make_fixture();
    let probe = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let scc = baseline::scc(&f.snapshot, &probe);
    // 6 nodes, but {helper, hot_loop_a} form one SCC, so we expect 5
    // components: {main}, {widget}, {drawable}, {ghost}, {helper, hot_loop_a}.
    assert_eq!(scc.components.len(), 5, "expected 5 SCC components");
    let helper_idx = scc.component_of(f.helper).unwrap();
    let hot_idx = scc.component_of(f.hot_loop_a).unwrap();
    assert_eq!(
        helper_idx, hot_idx,
        "helper and hot_loop_a must share an SCC component (the 2-cycle)"
    );
    let cycle_size = scc.components[helper_idx as usize].len();
    assert_eq!(cycle_size, 2, "the helper↔hot_loop_a SCC has size 2");
    // main, widget, drawable, ghost are each in their own component.
    let main_idx = scc.component_of(f.main_fn).unwrap();
    assert_eq!(scc.components[main_idx as usize].len(), 1);
}

// ============================================================================
// 2. condensation
// ============================================================================

#[test]
fn condensation_has_main_to_cycle_dag_edge() {
    let f = make_fixture();
    let probe = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let scc = baseline::scc(&f.snapshot, &probe);
    let dag = baseline::condensation(&f.snapshot, &probe);
    let main_comp = scc.component_of(f.main_fn).unwrap();
    let helper_comp = scc.component_of(f.helper).unwrap();
    assert!(
        dag.contains(&(main_comp, helper_comp)),
        "main → {{helper, hot_loop_a}} must be a DAG edge in the condensation"
    );
    // No edge from the cycle back to main (it's the only call into the
    // cycle), so (helper_comp, main_comp) must NOT be present.
    assert!(
        !dag.contains(&(helper_comp, main_comp)),
        "condensation must not invent a back-edge from cycle to main"
    );
    // The cycle is one component, so no self-edge in the DAG.
    assert!(
        !dag.contains(&(helper_comp, helper_comp)),
        "intra-SCC edges must be elided"
    );
}

// ============================================================================
// 3. reachability
// ============================================================================

#[test]
fn reachability_from_main_via_calls_covers_cycle() {
    let f = make_fixture();
    let probe = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let reach = baseline::reachability(&f.snapshot, &[f.main_fn], &probe);
    // main can reach itself, helper, hot_loop_a via Calls. widget,
    // drawable, ghost are isolated under Calls.
    assert!(reach.contains(&f.main_fn));
    assert!(reach.contains(&f.helper));
    assert!(reach.contains(&f.hot_loop_a));
    assert!(!reach.contains(&f.widget));
    assert!(!reach.contains(&f.drawable));
    assert!(!reach.contains(&f.ghost));
}

// ============================================================================
// 4. callers — fan-in to helper
// ============================================================================

#[test]
fn callers_oracle_returns_set_called_by_target() {
    let f = make_fixture();
    // callers(main): set of nodes that main calls. main calls
    // helper + hot_loop_a → {helper, hot_loop_a}.
    let out = baseline::callers(&f.snapshot, f.main_fn);
    assert!(out.contains(&f.helper));
    assert!(out.contains(&f.hot_loop_a));
    assert!(!out.contains(&f.main_fn));
    assert!(!out.contains(&f.widget));
}

// ============================================================================
// 5. callees — fan-out from helper
// ============================================================================

#[test]
fn callees_oracle_returns_set_that_calls_target() {
    let f = make_fixture();
    // callees(helper): set of nodes that call helper. main + hot_loop_a
    // both call helper.
    let out = baseline::callees(&f.snapshot, f.helper);
    assert!(out.contains(&f.main_fn));
    assert!(out.contains(&f.hot_loop_a));
    assert!(!out.contains(&f.helper));
}

// ============================================================================
// 6. imports
// ============================================================================

#[test]
fn imports_oracle_finds_widget_importer() {
    let f = make_fixture();
    // helper --Imports--> widget. So imports(widget) = {helper}.
    let out = baseline::imports(&f.snapshot, f.widget);
    assert_eq!(out.iter().copied().collect::<Vec<_>>(), vec![f.helper]);
}

// ============================================================================
// 7. exports
// ============================================================================

#[test]
fn exports_oracle_finds_widget_via_either_endpoint() {
    let f = make_fixture();
    // widget --Exports--> drawable. So exports(drawable) (Either role)
    // returns {widget}, and exports(widget) returns {drawable}.
    let out_d = baseline::exports(&f.snapshot, f.drawable);
    assert_eq!(out_d.iter().copied().collect::<Vec<_>>(), vec![f.widget]);
    let out_w = baseline::exports(&f.snapshot, f.widget);
    assert_eq!(out_w.iter().copied().collect::<Vec<_>>(), vec![f.drawable]);
}

// ============================================================================
// 8. references
// ============================================================================

#[test]
fn references_oracle_includes_calls_imports_and_references_edges() {
    let f = make_fixture();
    // references(helper) = nodes with incoming reference edges from
    // helper. helper has outgoing Calls→hot_loop_a, Imports→widget →
    // {hot_loop_a, widget}.
    let out = baseline::references(&f.snapshot, f.helper);
    assert!(
        out.contains(&f.hot_loop_a),
        "helper --Calls--> hot_loop_a must surface under references"
    );
    assert!(
        out.contains(&f.widget),
        "helper --Imports--> widget must surface under references"
    );
    // references(hot_loop_a) = nodes hot_loop_a references via Calls/
    // References/Imports/FFI. hot_loop_a calls helper + References-edges
    // drawable → {helper, drawable}.
    let out2 = baseline::references(&f.snapshot, f.hot_loop_a);
    assert!(out2.contains(&f.helper));
    assert!(out2.contains(&f.drawable));
}

// ============================================================================
// 9. implements
// ============================================================================

#[test]
fn implements_oracle_returns_widget_for_drawable() {
    let f = make_fixture();
    let out = baseline::implements(&f.snapshot, f.drawable);
    assert_eq!(out.iter().copied().collect::<Vec<_>>(), vec![f.widget]);
}

// ============================================================================
// 10. cycles
// ============================================================================

#[test]
fn cycles_oracle_finds_the_two_node_cycle() {
    let f = make_fixture();
    let cycles = baseline::cycles(&f.snapshot, CircularType::Calls, CycleBounds::default());
    assert_eq!(cycles.len(), 1, "exactly one non-trivial cycle expected");
    assert_eq!(cycles[0].len(), 2, "cycle size = 2");
    let members: std::collections::BTreeSet<NodeId> = cycles[0].iter().copied().collect();
    let expected: std::collections::BTreeSet<NodeId> =
        [f.helper, f.hot_loop_a].iter().copied().collect();
    assert_eq!(members, expected);
}

// ============================================================================
// 11. is_in_cycle
// ============================================================================

#[test]
fn is_in_cycle_oracle_distinguishes_members() {
    let f = make_fixture();
    let bounds = CycleBounds::default();
    let key_for = |node: NodeId| IsInCycleKey {
        node_id: node,
        circular_type: CircularType::Calls,
        bounds,
    };
    assert!(baseline::is_in_cycle(&f.snapshot, &key_for(f.helper)));
    assert!(baseline::is_in_cycle(&f.snapshot, &key_for(f.hot_loop_a)));
    assert!(!baseline::is_in_cycle(&f.snapshot, &key_for(f.main_fn)));
    assert!(!baseline::is_in_cycle(&f.snapshot, &key_for(f.ghost)));
}

// ============================================================================
// 12. entry_points
// ============================================================================

#[test]
fn entry_points_oracle_finds_main_only() {
    let f = make_fixture();
    let entries = baseline::entry_points(&f.snapshot);
    // main is `pub`. ghost is private. helper/hot_loop_a have no
    // visibility set + names don't start with `test_` etc. widget +
    // drawable are not pub. Drawable trait is not an Export NodeKind.
    assert!(entries.contains(&f.main_fn));
    assert!(!entries.contains(&f.helper));
    assert!(!entries.contains(&f.hot_loop_a));
    assert!(!entries.contains(&f.widget));
    assert!(!entries.contains(&f.drawable));
    assert!(!entries.contains(&f.ghost));
}

// ============================================================================
// 13. reachable_from_entry_points
// ============================================================================

#[test]
fn reachable_from_entry_points_oracle_walks_from_main() {
    let f = make_fixture();
    let reachable = baseline::reachable_from_entry_points(&f.snapshot);
    // main is the only entry point. From main, via Calls we reach
    // helper + hot_loop_a. From helper, via Imports we reach widget.
    // From hot_loop_a, via References we reach drawable. ghost is
    // unreachable (no incoming reachability edge from main's component).
    assert!(reachable.contains(&f.main_fn));
    assert!(reachable.contains(&f.helper));
    assert!(reachable.contains(&f.hot_loop_a));
    assert!(reachable.contains(&f.widget));
    assert!(reachable.contains(&f.drawable));
    assert!(!reachable.contains(&f.ghost));
}

// ============================================================================
// 14. unused
// ============================================================================

#[test]
fn unused_oracle_lists_only_ghost_under_all_scope() {
    let f = make_fixture();
    let key = UnusedKey {
        scope: UnusedScope::All,
        max_results: 100,
    };
    let out = baseline::unused(&f.snapshot, &key);
    // ghost is the only node unreachable from main and not an entry
    // point itself. helper/hot_loop_a/widget/drawable are all reachable.
    assert_eq!(out, vec![f.ghost], "exactly ghost reported as unused");
}

// ============================================================================
// 15. is_node_unused
// ============================================================================

#[test]
fn is_node_unused_oracle_matches_unused_set() {
    let f = make_fixture();
    let mk = |node: NodeId| IsNodeUnusedKey {
        node_id: node,
        scope: UnusedScope::All,
    };
    assert!(baseline::is_node_unused(&f.snapshot, &mk(f.ghost)));
    assert!(!baseline::is_node_unused(&f.snapshot, &mk(f.main_fn)));
    assert!(!baseline::is_node_unused(&f.snapshot, &mk(f.helper)));
    assert!(!baseline::is_node_unused(&f.snapshot, &mk(f.hot_loop_a)));
    // ghost under Struct scope: filtered out by scope, not unused.
    let struct_key = IsNodeUnusedKey {
        node_id: f.ghost,
        scope: UnusedScope::Struct,
    };
    assert!(!baseline::is_node_unused(&f.snapshot, &struct_key));
}

// ============================================================================
// 16. address_taken
// ============================================================================

#[test]
fn address_taken_oracle_returns_hot_loop_a_only() {
    let f = make_fixture();
    let out = baseline::address_taken(&f.snapshot);
    assert_eq!(out, vec![f.hot_loop_a]);
}

// ============================================================================
// 17. callsite_promiscuous
// ============================================================================

#[test]
fn callsite_promiscuous_oracle_returns_hot_loop_a_only() {
    let f = make_fixture();
    let out = baseline::callsite_promiscuous(&f.snapshot);
    assert_eq!(out, vec![f.hot_loop_a]);
}
