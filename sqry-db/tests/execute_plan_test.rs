//! End-to-end integration tests for [`sqry_db::planner::execute`] (DB12).
//!
//! Coverage targets (mirroring the DB12 task brief):
//!
//! - Every [`PlanNode`] variant: `NodeScan`, `EdgeTraversal`, `Filter`,
//!   `SetOp` (all three operations), `Chain`
//! - `NodeScan` filters: kind, visibility (both variants), name pattern
//!   (all five [`MatchMode`] values + case-insensitive + glob)
//! - `EdgeTraversal`: forward / reverse / both directions, bounded depth,
//!   edge-kind filtering by discriminant (metadata-insensitive)
//! - Every [`Predicate`] variant: `HasCaller`, `HasCallee`, `IsUnused`,
//!   `Callers(Pattern|Regex|Subquery)`, `Callees(...)`, `Imports(...)`,
//!   `Exports(...)`, `References(...)`, `Implements(...)`, `InFile`,
//!   `MatchesName`, `And`, `Or`, `Not`
//! - Subquery caching: identical subqueries evaluate exactly once within
//!   a single executor invocation
//! - Fused batch execution: `execute_batch` scatters results back into
//!   submission order, evaluates shared prefixes once, and primes the
//!   subquery cache
//! - Output determinism: results are sorted and deduplicated

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, ExportKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::planner::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryBuilder,
    QueryPlan, RegexFlags, RegexPattern, ScanFilters, StringPattern, execute_batch, execute_plan,
    fuse_plans, fuse_single,
};
use sqry_db::{QueryDb, QueryDbConfig};

// ---------------------------------------------------------------------------
// Fixture builder
// ---------------------------------------------------------------------------

/// Test fixture: a small Rust-ish code graph with functions in two files,
/// a trait + implementer, and an export/import pair.
///
/// Layout:
///
/// ```text
/// src/lib.rs
///   pub fn parse_expr()            — Function, Public
///   pub fn parse_stmt()            — Function, Public
///   fn internal_helper()           — Function, Private
///   pub struct Parser              — Struct (referenced by parse_expr for References test)
///   pub trait Visitor              — Trait
///   pub fn run()                   — Function, Public (exports to bin)
///
/// src/bin.rs
///   fn main()                      — Function, Private (calls parse_expr + parse_stmt)
///   struct VisitorImpl             — Struct (Implements Visitor)
///   fn imports_lib()               — Function, Private (Imports edge to lib's Parser)
///
/// Edges:
///   main         --Calls--> parse_expr   (one Calls edge)
///   main         --Calls--> parse_stmt
///   parse_expr   --Calls--> internal_helper
///   VisitorImpl  --Implements--> Visitor
///   bin          --Imports--> lib's Parser  (placed on imports_lib)
///   parse_expr   --References--> Parser
///   run          --Exports--> main
/// ```
struct Fixture {
    db: QueryDb,
    ids: FixtureIds,
    snapshot: Arc<GraphSnapshot>,
}

struct FixtureIds {
    parse_expr: NodeId,
    parse_stmt: NodeId,
    internal_helper: NodeId,
    parser: NodeId,
    visitor_trait: NodeId,
    run_fn: NodeId,
    main_fn: NodeId,
    visitor_impl: NodeId,
    imports_lib: NodeId,
}

impl Fixture {
    fn build() -> Self {
        let mut graph = CodeGraph::new();

        // --- Files ---
        let lib_file = graph
            .files_mut()
            .register_with_language(Path::new("src/lib.rs"), Some(Language::Rust))
            .expect("register lib");
        let bin_file = graph
            .files_mut()
            .register_with_language(Path::new("src/bin.rs"), Some(Language::Rust))
            .expect("register bin");

        // --- Interned strings ---
        let parse_expr_name = graph.strings_mut().intern("parse_expr").expect("intern");
        let parse_stmt_name = graph.strings_mut().intern("parse_stmt").expect("intern");
        let internal_helper_name = graph
            .strings_mut()
            .intern("internal_helper")
            .expect("intern");
        let parser_name = graph.strings_mut().intern("Parser").expect("intern");
        let visitor_name = graph.strings_mut().intern("Visitor").expect("intern");
        let run_name = graph.strings_mut().intern("run").expect("intern");
        let main_name = graph.strings_mut().intern("main").expect("intern");
        let visitor_impl_name = graph.strings_mut().intern("VisitorImpl").expect("intern");
        let imports_lib_name = graph.strings_mut().intern("imports_lib").expect("intern");

        let public_vis = graph.strings_mut().intern("public").expect("intern");
        let private_vis = graph.strings_mut().intern("private").expect("intern");

        // --- Nodes ---
        // Byte spans are chosen so that end_byte fits inside a containing
        // ScopeKind::Module span (0..1000) in case scope-arena tests are added
        // later; for DB12 the scope arena stays empty.
        let add_node = |graph: &mut CodeGraph, entry: NodeEntry| -> NodeId {
            let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
            graph
                .indices_mut()
                .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
            id
        };

        let parse_expr = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, parse_expr_name, lib_file)
                .with_byte_range(10, 90)
                .with_visibility(public_vis),
        );
        let parse_stmt = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, parse_stmt_name, lib_file)
                .with_byte_range(100, 180)
                .with_visibility(public_vis),
        );
        let internal_helper = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, internal_helper_name, lib_file)
                .with_byte_range(200, 260)
                .with_visibility(private_vis),
        );
        let parser = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Struct, parser_name, lib_file)
                .with_byte_range(300, 340)
                .with_visibility(public_vis),
        );
        let visitor_trait = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Trait, visitor_name, lib_file)
                .with_byte_range(400, 460)
                .with_visibility(public_vis),
        );
        let run_fn = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, run_name, lib_file)
                .with_byte_range(500, 560)
                .with_visibility(public_vis),
        );

        let main_fn = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, main_name, bin_file)
                .with_byte_range(10, 80)
                .with_visibility(private_vis),
        );
        let visitor_impl = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Struct, visitor_impl_name, bin_file)
                .with_byte_range(100, 160)
                .with_visibility(private_vis),
        );
        let imports_lib = add_node(
            &mut graph,
            NodeEntry::new(NodeKind::Function, imports_lib_name, bin_file)
                .with_byte_range(200, 260)
                .with_visibility(private_vis),
        );

        // --- Edges ---
        graph.edges().add_edge(
            main_fn,
            parse_expr,
            EdgeKind::Calls {
                argument_count: 3,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            bin_file,
        );
        graph.edges().add_edge(
            main_fn,
            parse_stmt,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            },
            bin_file,
        );
        graph.edges().add_edge(
            parse_expr,
            internal_helper,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            lib_file,
        );
        graph
            .edges()
            .add_edge(parse_expr, parser, EdgeKind::References, lib_file);
        graph
            .edges()
            .add_edge(visitor_impl, visitor_trait, EdgeKind::Implements, bin_file);
        graph.edges().add_edge(
            imports_lib,
            parser,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            bin_file,
        );
        graph.edges().add_edge(
            run_fn,
            main_fn,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            lib_file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

        Fixture {
            db,
            snapshot,
            ids: FixtureIds {
                parse_expr,
                parse_stmt,
                internal_helper,
                parser,
                visitor_trait,
                run_fn,
                main_fn,
                visitor_impl,
                imports_lib,
            },
        }
    }

    /// Shorthand — `execute_plan(&plan, &self.db)`.
    fn execute(&self, plan: &QueryPlan) -> Vec<NodeId> {
        execute_plan(plan, &self.db)
    }
}

/// Asserts `actual` equals a sorted version of `expected`.
///
/// The executor always returns sorted/deduplicated output, so tests that
/// express expectations in unsorted form can still compare against the
/// executor's canonical order through this helper.
fn assert_same(actual: Vec<NodeId>, mut expected: Vec<NodeId>) {
    expected.sort_unstable_by_key(|id| (id.index(), id.generation()));
    expected.dedup();
    assert_eq!(actual, expected);
}

// ---------------------------------------------------------------------------
// NodeScan
// ---------------------------------------------------------------------------

#[test]
fn scan_by_kind_function_returns_every_function() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![
            fx.ids.parse_expr,
            fx.ids.parse_stmt,
            fx.ids.internal_helper,
            fx.ids.run_fn,
            fx.ids.main_fn,
            fx.ids.imports_lib,
        ],
    );
}

#[test]
fn scan_by_kind_struct_returns_structs_only() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new().scan(NodeKind::Struct).build().unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parser, fx.ids.visitor_impl]);
}

#[test]
fn scan_with_visibility_public_filter() {
    use sqry_core::schema::Visibility;

    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_visibility(Visibility::Public),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.parse_expr, fx.ids.parse_stmt, fx.ids.run_fn],
    );
}

#[test]
fn scan_with_visibility_private_filter() {
    use sqry_core::schema::Visibility;

    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_visibility(Visibility::Private),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.internal_helper, fx.ids.main_fn, fx.ids.imports_lib],
    );
}

#[test]
fn scan_with_name_prefix_pattern() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::prefix("parse_")),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn scan_with_name_glob_pattern() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::glob("parse_*")),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn scan_with_name_case_insensitive_exact() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("PARSE_EXPR").case_insensitive()),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr]);
}

#[test]
fn scan_all_returns_every_live_node() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new().scan_all().build().unwrap();
    let actual = fx.execute(&plan);
    let expected_count = fx.snapshot.nodes().len();
    assert_eq!(actual.len(), expected_count);
}

// ---------------------------------------------------------------------------
// EdgeTraversal
// ---------------------------------------------------------------------------

#[test]
fn traverse_forward_calls_reaches_callees_transitively() {
    let fx = Fixture::build();
    // Chain: scan-main -> traverse forward Calls depth=2 yields callees and
    // transitive callees (parse_expr, parse_stmt, internal_helper).
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("main")),
        )
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 99, // metadata ignored by discriminant match
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            2,
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.parse_expr, fx.ids.parse_stmt, fx.ids.internal_helper],
    );
}

#[test]
fn traverse_forward_depth_one_stops_at_first_hop() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("main")),
        )
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            1,
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn traverse_reverse_calls_yields_callers() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("parse_expr")),
        )
        .traverse(
            Direction::Reverse,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            1,
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.main_fn]);
}

#[test]
fn traverse_both_includes_incoming_and_outgoing() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("parse_expr")),
        )
        .traverse_any(Direction::Both, 1)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // parse_expr: outgoing Calls to internal_helper + References to parser;
    // incoming Calls from main.
    assert_same(
        actual,
        vec![fx.ids.internal_helper, fx.ids.parser, fx.ids.main_fn],
    );
}

#[test]
fn traverse_any_edge_kind_follows_every_edge() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("parse_expr")),
        )
        .traverse_any(Direction::Forward, 1)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // parse_expr has outgoing Calls to internal_helper and References to parser.
    assert_same(actual, vec![fx.ids.internal_helper, fx.ids.parser]);
}

// ---------------------------------------------------------------------------
// Filter — attribute predicates
// ---------------------------------------------------------------------------

#[test]
fn filter_has_caller_keeps_only_called_functions() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.parse_expr, fx.ids.parse_stmt, fx.ids.internal_helper],
    );
}

#[test]
fn filter_has_callee_keeps_only_calling_functions() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCallee)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.main_fn, fx.ids.parse_expr]);
}

#[test]
fn filter_is_unused_drops_nodes_with_inbound_use_edges() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::IsUnused)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // main_fn has incoming Exports (not a "use" edge) → counted as unused.
    // run_fn has no incoming edges → unused.
    // imports_lib has no incoming edges → unused.
    // parse_expr, parse_stmt, internal_helper have incoming Calls → not unused.
    assert_same(
        actual,
        vec![fx.ids.run_fn, fx.ids.main_fn, fx.ids.imports_lib],
    );
}

#[test]
fn filter_in_file_glob_narrows_to_matching_paths() {
    let fx = Fixture::build();
    // FileRegistry canonicalises paths to absolute form, so we use a
    // `**/` glob to match the trailing path fragment the fixture registers.
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::InFile(PathPattern::new("**/bin.rs")))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.main_fn, fx.ids.imports_lib]);
}

#[test]
fn filter_matches_name_substring() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::contains("parse")))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn filter_and_combinator_requires_every_predicate() {
    use sqry_core::schema::Visibility;

    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::And(vec![
            Predicate::HasCallee,
            Predicate::InFile(PathPattern::new("**/lib.rs")),
        ]))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr]);
    let _ = Visibility::Public; // silence unused warning if test helpers change
}

#[test]
fn filter_or_combinator_accepts_any_match() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Or(vec![
            Predicate::MatchesName(StringPattern::exact("main")),
            Predicate::MatchesName(StringPattern::exact("run")),
        ]))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.main_fn, fx.ids.run_fn]);
}

#[test]
fn filter_not_combinator_inverts_inner_result() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Not(Box::new(Predicate::HasCaller)))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.run_fn, fx.ids.main_fn, fx.ids.imports_lib],
    );
}

// ---------------------------------------------------------------------------
// Filter — value-bearing relation predicates (Pattern + Regex + Subquery)
// ---------------------------------------------------------------------------

#[test]
fn filter_callers_with_pattern_matches_by_source_name() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callers(PredicateValue::Pattern(
            StringPattern::exact("main"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // Both parse_expr and parse_stmt are called by main.
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn filter_callees_with_pattern_matches_by_target_name() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callees(PredicateValue::Pattern(
            StringPattern::exact("parse_expr"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.main_fn]);
}

#[test]
fn filter_implements_with_pattern_matches_trait_target() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Struct)
        .filter(Predicate::Implements(PredicateValue::Pattern(
            StringPattern::exact("Visitor"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.visitor_impl]);
}

#[test]
fn filter_imports_with_pattern_matches_target_name() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Imports(PredicateValue::Pattern(
            StringPattern::exact("Parser"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.imports_lib]);
}

#[test]
fn filter_exports_both_directions() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Exports(PredicateValue::Pattern(
            StringPattern::exact("main"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // run_fn has outgoing Exports edge with target "main", so run_fn matches.
    assert_same(actual, vec![fx.ids.run_fn]);
}

#[test]
fn filter_references_with_pattern_matches_source_name() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Struct)
        .filter(Predicate::References(PredicateValue::Pattern(
            StringPattern::exact("parse_expr"),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // Parser is referenced by parse_expr, and it's also imported by imports_lib;
    // the References predicate accepts Calls / References / Imports / FfiCall.
    assert_same(actual, vec![fx.ids.parser]);
}

#[test]
fn filter_callers_with_regex_matches_on_source_name() {
    let fx = Fixture::build();
    let flags = RegexFlags {
        case_insensitive: true,
        multiline: false,
        dot_all: false,
    };
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callers(PredicateValue::Regex(
            RegexPattern::with_flags("^MAIN$", flags),
        )))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn filter_callees_with_subquery_uses_plan_result_as_target_set() {
    let fx = Fixture::build();
    let subquery = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::prefix("parse_")),
        )
        .build()
        .unwrap();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callees(PredicateValue::Subquery(Box::new(
            subquery.root,
        ))))
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    // Nodes that call any node in the subquery's result set (parse_expr or
    // parse_stmt). Only main_fn does.
    assert_same(actual, vec![fx.ids.main_fn]);
}

// ---------------------------------------------------------------------------
// SetOp
// ---------------------------------------------------------------------------

#[test]
fn setop_union_concatenates_then_dedupes() {
    let fx = Fixture::build();
    let left = QueryBuilder::new().scan(NodeKind::Struct).build().unwrap();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Trait)
        .union(left)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.visitor_trait, fx.ids.parser, fx.ids.visitor_impl],
    );
}

#[test]
fn setop_intersect_returns_common_nodes_only() {
    let fx = Fixture::build();
    let public_fns = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_visibility(sqry_core::schema::Visibility::Public),
        )
        .build()
        .unwrap();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::prefix("parse_")),
        )
        .intersect(public_fns)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn setop_difference_subtracts_right_from_left() {
    let fx = Fixture::build();
    let called = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .build()
        .unwrap();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .difference(called)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(
        actual,
        vec![fx.ids.run_fn, fx.ids.main_fn, fx.ids.imports_lib],
    );
}

// ---------------------------------------------------------------------------
// Fused batch execution
// ---------------------------------------------------------------------------

#[test]
fn execute_batch_preserves_submission_order() {
    let fx = Fixture::build();
    let plans = vec![
        QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::HasCaller)
            .build()
            .unwrap(),
        QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::HasCallee)
            .build()
            .unwrap(),
        QueryBuilder::new().scan(NodeKind::Struct).build().unwrap(),
    ];
    let fused = fuse_plans(plans.clone());
    assert_eq!(fused.stats().total_plans, 3);

    let results = execute_batch(&fused, &fx.db);
    assert_eq!(results.len(), 3);

    // Each slot matches the individual plan result.
    for (i, plan) in plans.iter().enumerate() {
        let expected = execute_plan(plan, &fx.db);
        assert_eq!(results[i], expected, "batch index {i}");
    }
}

#[test]
fn execute_batch_reuses_shared_prefix_across_tails() {
    let fx = Fixture::build();
    // Two plans that share a scan(Function) prefix.
    let plans = vec![
        QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::HasCaller)
            .build()
            .unwrap(),
        QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::HasCallee)
            .build()
            .unwrap(),
    ];
    let fused = fuse_plans(plans);
    assert_eq!(fused.stats().fusion_groups, 1);
    assert_eq!(fused.stats().scans_eliminated, 1);

    let results = execute_batch(&fused, &fx.db);
    assert_eq!(results.len(), 2);
    assert_same(
        results[0].clone(),
        vec![fx.ids.parse_expr, fx.ids.parse_stmt, fx.ids.internal_helper],
    );
    assert_same(results[1].clone(), vec![fx.ids.main_fn, fx.ids.parse_expr]);
}

#[test]
fn execute_batch_primes_subqueries_before_top_level_tails() {
    let fx = Fixture::build();
    // Two plans that share the same nested subquery — fuse should dedup it.
    let shared_sub = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::prefix("parse_")),
        )
        .build()
        .unwrap();

    let plan_a = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callees(PredicateValue::Subquery(Box::new(
            shared_sub.root.clone(),
        ))))
        .build()
        .unwrap();
    let plan_b = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::And(vec![
            Predicate::Callees(PredicateValue::Subquery(Box::new(shared_sub.root.clone()))),
            Predicate::HasCaller,
        ]))
        .build()
        .unwrap();

    let fused = fuse_plans(vec![plan_a.clone(), plan_b.clone()]);
    // Fuser should deduplicate the subquery occurrences (both plans reference
    // the same inner PlanNode), yielding 1 unique subquery.
    assert!(
        fused.stats().subqueries_total >= 2,
        "expected both top-level plans to carry the subquery"
    );
    assert_eq!(
        fused.stats().subqueries_unique,
        1,
        "identical subqueries should dedupe to one"
    );

    let results = execute_batch(&fused, &fx.db);
    assert_eq!(results.len(), 2);

    // plan_a: "functions whose callees are parse_expr or parse_stmt" → main.
    assert_same(results[0].clone(), vec![fx.ids.main_fn]);
    // plan_b: intersection — main also has a caller (no — main has no caller).
    // So plan_b → empty.
    assert_same(results[1].clone(), vec![]);
}

#[test]
fn execute_batch_handles_single_plan_identity_group() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new().scan(NodeKind::Struct).build().unwrap();
    let fused = fuse_single(plan);
    let results = execute_batch(&fused, &fx.db);
    assert_eq!(results.len(), 1);
    assert_same(results[0].clone(), vec![fx.ids.parser, fx.ids.visitor_impl]);
}

// ---------------------------------------------------------------------------
// Determinism & edge-case safety
// ---------------------------------------------------------------------------

#[test]
fn output_is_sorted_by_index_then_generation() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    for window in actual.windows(2) {
        let a = (window[0].index(), window[0].generation());
        let b = (window[1].index(), window[1].generation());
        assert!(a < b, "output must be strictly sorted: {a:?} < {b:?}");
    }
}

#[test]
fn output_is_deduplicated_even_across_union() {
    let fx = Fixture::build();
    let same = QueryBuilder::new()
        .scan(NodeKind::Function)
        .build()
        .unwrap();
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .union(same)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    let direct = fx.execute(
        &QueryBuilder::new()
            .scan(NodeKind::Function)
            .build()
            .unwrap(),
    );
    assert_eq!(actual, direct);
}

#[test]
fn zero_depth_traversal_yields_empty_set() {
    let fx = Fixture::build();
    // Hand-build a plan with max_depth=0 — the builder rejects this, but the
    // executor must still short-circuit safely for programmatically-constructed
    // plans that skip the builder.
    let plan = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            PlanNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_kind: None,
                max_depth: 0,
                resolved_via: None,
            },
        ],
    });
    let actual = fx.execute(&plan);
    assert!(actual.is_empty());
}

#[test]
fn empty_input_to_traversal_yields_empty_output() {
    let fx = Fixture::build();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("does_not_exist")),
        )
        .traverse_any(Direction::Forward, 3)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert!(actual.is_empty());
}

#[test]
fn chain_threads_output_through_filters() {
    let fx = Fixture::build();
    // scan functions -> keep only public -> keep only those in lib.rs.
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::InFile(PathPattern::new("**/lib.rs")))
        .filter(Predicate::HasCallee)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr]);
}

#[test]
fn setop_inside_chain_merges_sibling_branches() {
    let fx = Fixture::build();
    // Start from union(parse_expr, parse_stmt), then chain a filter.
    let right = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("parse_stmt")),
        )
        .build()
        .unwrap();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::exact("parse_expr")),
        )
        .union(right)
        .filter(Predicate::HasCaller)
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.parse_expr, fx.ids.parse_stmt]);
}

#[test]
fn match_mode_glob_variant_is_exercised() {
    let fx = Fixture::build();
    // Ensure the MatchMode enum variant is referenced in tests for coverage.
    assert_eq!(MatchMode::Glob, StringPattern::glob("foo").mode);
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_name_pattern(StringPattern::glob("*helper")),
        )
        .build()
        .unwrap();
    let actual = fx.execute(&plan);
    assert_same(actual, vec![fx.ids.internal_helper]);
}

// ---------------------------------------------------------------------------
// Sanity: fixture expectations — guards against accidental fixture rewrites.
// ---------------------------------------------------------------------------

#[test]
fn fixture_node_count_matches_expected_layout() {
    let fx = Fixture::build();
    // 9 nodes: parse_expr, parse_stmt, internal_helper, parser,
    // visitor_trait, run_fn, main_fn, visitor_impl, imports_lib.
    assert_eq!(fx.snapshot.nodes().len(), 9);
}

// ---------------------------------------------------------------------------
// `returns:<TypeName>` predicate (B2_PLANNER)
// ---------------------------------------------------------------------------
//
// These tests build a Go-shaped fixture where two functions return `error`
// (modelled by a `TypeOf { context: Some(Return), .. }` edge to a Type node
// named `error`) and one function returns `string`. The predicate must
// resolve through edge structure, not through `NodeEntry.signature` text.

mod returns_predicate {
    use super::*;
    use sqry_core::graph::unified::edge::kind::TypeOfContext;
    use sqry_db::planner::Predicate;
    use sqry_db::planner::parse_query;

    struct ReturnsFixture {
        db: QueryDb,
        do_thing: NodeId,
        validate: NodeId,
        get_name: NodeId,
        bare_void: NodeId,
    }

    impl ReturnsFixture {
        fn build() -> Self {
            let mut graph = CodeGraph::new();

            let main_file = graph
                .files_mut()
                .register_with_language(Path::new("main.go"), Some(Language::Go))
                .expect("register file");

            let do_thing_name = graph.strings_mut().intern("DoThing").expect("intern");
            let validate_name = graph.strings_mut().intern("Validate").expect("intern");
            let get_name_name = graph.strings_mut().intern("GetName").expect("intern");
            let bare_void_name = graph.strings_mut().intern("Bare").expect("intern");
            let error_name = graph.strings_mut().intern("error").expect("intern");
            let string_name = graph.strings_mut().intern("string").expect("intern");

            let add_node = |graph: &mut CodeGraph, entry: NodeEntry| -> NodeId {
                let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
                graph.indices_mut().add(
                    id,
                    entry.kind,
                    entry.name,
                    entry.qualified_name,
                    entry.file,
                );
                id
            };

            let do_thing = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Function, do_thing_name, main_file)
                    .with_byte_range(10, 90),
            );
            let validate = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Function, validate_name, main_file)
                    .with_byte_range(100, 180),
            );
            let get_name = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Function, get_name_name, main_file)
                    .with_byte_range(200, 260),
            );
            let bare_void = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Function, bare_void_name, main_file)
                    .with_byte_range(300, 340),
            );
            let error_type = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Type, error_name, main_file).with_byte_range(400, 410),
            );
            let string_type = add_node(
                &mut graph,
                NodeEntry::new(NodeKind::Type, string_name, main_file).with_byte_range(420, 430),
            );

            // Return-type edges: DoThing -> error, Validate -> error,
            // GetName -> string. Bare has no return-type edge at all.
            graph.edges().add_edge(
                do_thing,
                error_type,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Return),
                    index: Some(0),
                    name: None,
                },
                main_file,
            );
            graph.edges().add_edge(
                validate,
                error_type,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Return),
                    index: Some(0),
                    name: None,
                },
                main_file,
            );
            graph.edges().add_edge(
                get_name,
                string_type,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Return),
                    index: Some(0),
                    name: None,
                },
                main_file,
            );

            // Counter-example: a `Parameter`-context TypeOf edge to `error`
            // on `Bare` should NOT cause `returns:error` to match.
            graph.edges().add_edge(
                bare_void,
                error_type,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Parameter),
                    index: Some(0),
                    name: None,
                },
                main_file,
            );

            let snapshot = Arc::new(graph.snapshot());
            let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

            Self {
                db,
                do_thing,
                validate,
                get_name,
                bare_void,
            }
        }
    }

    #[test]
    fn returns_error_matches_only_functions_with_return_typeof_edge_to_error() {
        let fx = ReturnsFixture::build();
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::Returns("error".to_string()))
            .build()
            .unwrap();
        let mut actual = execute_plan(&plan, &fx.db);
        actual.sort_unstable_by_key(|id| (id.index(), id.generation()));
        let mut expected = vec![fx.do_thing, fx.validate];
        expected.sort_unstable_by_key(|id| (id.index(), id.generation()));
        assert_eq!(actual, expected);
        // Bare has only a Parameter-context TypeOf edge; must NOT match.
        assert!(!actual.contains(&fx.bare_void));
        // GetName returns string, not error.
        assert!(!actual.contains(&fx.get_name));
    }

    #[test]
    fn returns_string_matches_only_get_name() {
        let fx = ReturnsFixture::build();
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::Returns("string".to_string()))
            .build()
            .unwrap();
        let actual = execute_plan(&plan, &fx.db);
        assert_eq!(actual, vec![fx.get_name]);
    }

    #[test]
    fn returns_unknown_type_yields_empty_set() {
        let fx = ReturnsFixture::build();
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::Returns("nonexistent".to_string()))
            .build()
            .unwrap();
        let actual = execute_plan(&plan, &fx.db);
        assert!(actual.is_empty());
    }

    #[test]
    fn returns_predicate_via_text_parser_end_to_end() {
        let fx = ReturnsFixture::build();
        let plan = parse_query("kind:function returns:error").expect("parse");
        let mut actual = execute_plan(&plan, &fx.db);
        actual.sort_unstable_by_key(|id| (id.index(), id.generation()));
        let mut expected = vec![fx.do_thing, fx.validate];
        expected.sort_unstable_by_key(|id| (id.index(), id.generation()));
        assert_eq!(actual, expected);
    }

    #[test]
    fn returns_is_case_sensitive_exact_match() {
        let fx = ReturnsFixture::build();
        // Lowercase needle that doesn't match the type's actual interned
        // name `error` (verifies strict casing — case-insensitive matching
        // is explicitly out of scope for this predicate).
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::Returns("ERROR".to_string()))
            .build()
            .unwrap();
        let actual = execute_plan(&plan, &fx.db);
        assert!(actual.is_empty());
    }
}
