//! Integration tests for the `is_unsafe:true|false` planner predicate
//! (`Predicate::IsUnsafe`), the first L0 primitive of the security add-on.
//!
//! Surface under test:
//!
//! * `is_unsafe:true|false` / bare `is_unsafe` -> `Predicate::IsUnsafe`,
//!   evaluated against the stored `NodeEntry::is_unsafe` bit.
//!
//! The predicate is a plain leaf boolean over a dense node flag, like
//! `is_definition`. Unlike `is_definition` it is NOT a definition-fidelity
//! marker, so it must return `false` from both `uses_definition_predicate`
//! and `has_subquery` (a regression guard for the codex L0 design review:
//! a `true` there would trip the CLI/MCP definition-signal guards and make
//! `is_unsafe` queries fail on snapshots lacking definition signals).

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{Predicate, execute_plan, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};

/// Five functions in `lib.rs`: two recorded unsafe, two explicitly not, and
/// one never populated (exercises the legacy default-false path).
struct Fixture {
    db: QueryDb,
    unsafe_one: NodeId,
    unsafe_two: NodeId,
    safe_one: NodeId,
    safe_two: NodeId,
    default_unset: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();

        let lib_file = graph
            .files_mut()
            .register_with_language(Path::new("lib.rs"), Some(Language::Rust))
            .expect("register lib");

        let intern = |g: &mut CodeGraph, s: &str| g.strings_mut().intern(s).expect("intern");

        let unsafe_one_name = intern(&mut graph, "unsafe_one");
        let unsafe_two_name = intern(&mut graph, "unsafe_two");
        let safe_one_name = intern(&mut graph, "safe_one");
        let safe_two_name = intern(&mut graph, "safe_two");
        let default_unset_name = intern(&mut graph, "default_unset");

        let alloc_fn = |g: &mut CodeGraph, name_id, start: u32, is_unsafe| {
            let entry = NodeEntry::new(NodeKind::Function, name_id, lib_file)
                .with_qualified_name(name_id)
                .with_byte_range(start, start + 50)
                .with_unsafe(is_unsafe);
            g.nodes_mut().alloc(entry).expect("alloc")
        };

        let unsafe_one = alloc_fn(&mut graph, unsafe_one_name, 10, true);
        let unsafe_two = alloc_fn(&mut graph, unsafe_two_name, 70, true);
        let safe_one = alloc_fn(&mut graph, safe_one_name, 130, false);
        let safe_two = alloc_fn(&mut graph, safe_two_name, 190, false);

        // Allocated WITHOUT `.with_unsafe(...)`: exercises the legacy default,
        // where `NodeEntry::is_unsafe` was never populated and defaults to false.
        let default_unset = {
            let entry = NodeEntry::new(NodeKind::Function, default_unset_name, lib_file)
                .with_qualified_name(default_unset_name)
                .with_byte_range(250, 300);
            graph.nodes_mut().alloc(entry).expect("alloc")
        };

        // Mirror the by-kind index so `kind:function` scans pick them up.
        for (id, name_id) in [
            (unsafe_one, unsafe_one_name),
            (unsafe_two, unsafe_two_name),
            (safe_one, safe_one_name),
            (safe_two, safe_two_name),
            (default_unset, default_unset_name),
        ] {
            graph
                .indices_mut()
                .add(id, NodeKind::Function, name_id, Some(name_id), lib_file);
        }

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(snapshot, QueryDbConfig::default());

        Fixture {
            db,
            unsafe_one,
            unsafe_two,
            safe_one,
            safe_two,
            default_unset,
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

#[test]
fn query_is_unsafe_true_returns_recorded_unsafe_functions() {
    let fx = Fixture::build();

    let recorded = fx.run("kind:function is_unsafe:true");
    assert_eq!(recorded, sorted(vec![fx.unsafe_one, fx.unsafe_two]));
}

#[test]
fn query_is_unsafe_bare_defaults_to_true() {
    let fx = Fixture::build();

    let bare = fx.run("kind:function is_unsafe");
    assert_eq!(bare, sorted(vec![fx.unsafe_one, fx.unsafe_two]));
}

#[test]
fn query_is_unsafe_false_returns_not_recorded_unsafe() {
    let fx = Fixture::build();

    // `is_unsafe:false` means "not recorded as unsafe", not "proven safe": it
    // selects both explicitly-safe functions and `default_unset`, which never
    // called `.with_unsafe(...)` (legacy default-false path).
    let not_unsafe = fx.run("kind:function is_unsafe:false");
    assert_eq!(
        not_unsafe,
        sorted(vec![fx.safe_one, fx.safe_two, fx.default_unset])
    );
    assert!(
        not_unsafe.contains(&fx.default_unset),
        "is_unsafe:false must select a node whose unsafe flag was never recorded"
    );
}

#[test]
fn is_unsafe_is_not_a_definition_predicate() {
    // Regression guard for the codex L0 design review: `IsUnsafe` is a plain
    // leaf flag, not a definition-fidelity marker and not a subquery, so both
    // classifiers must report false. A `true` from uses_definition_predicate
    // would route `is_unsafe` queries through the definition-signal guards.
    for want in [true, false] {
        assert!(!Predicate::IsUnsafe(want).uses_definition_predicate());
        assert!(!Predicate::IsUnsafe(want).has_subquery());
    }
}
