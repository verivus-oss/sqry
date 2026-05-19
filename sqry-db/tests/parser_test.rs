//! Integration tests for [`sqry_db::planner::parse`] (DB13).
//!
//! Coverage targets (mirroring the DB13 task brief):
//!
//! - Every step keyword produces the expected [`PlanNode`]
//! - `kind:` / `visibility:` / `name:` fold into a single leading `NodeScan`
//!   when possible, preserving the by-kind index hot path
//! - Every [`Direction`] spelling (`forward`/`outgoing`/`out`,
//!   `reverse`/`incoming`/`in`, `both`)
//! - Every edge-kind keyword supported by the text syntax
//! - `impl:` and `implements:` both produce [`Predicate::Implements`]
//!   (spec §M8)
//! - Subqueries via parenthesised value
//! - Regex literals with every flag combination
//! - Bare globs, quoted strings (including escapes)
//! - Error variants: `UnexpectedChar`, `UnexpectedEnd`, `UnknownIdent`,
//!   `InvalidInteger`, `Build`

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::Visibility;

use sqry_db::planner::{
    Direction, ParseError, PlanNode, Predicate, PredicateValue, QueryPlan, SetOperation,
    StringPattern, parse_query,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse(src: &str) -> QueryPlan {
    parse_query(src).unwrap_or_else(|err| panic!("parse({src:?}) failed: {err}"))
}

fn chain_steps(plan: QueryPlan) -> Vec<PlanNode> {
    let PlanNode::Chain { steps } = plan.root else {
        panic!("expected Chain root");
    };
    steps
}

// ---------------------------------------------------------------------------
// kind / visibility / name folding
// ---------------------------------------------------------------------------

#[test]
fn kind_only_produces_single_nodescan() {
    let steps = chain_steps(parse("kind:function"));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility,
            name_pattern,
        } => {
            assert!(visibility.is_none());
            assert!(name_pattern.is_none());
        }
        other => panic!("unexpected root: {other:?}"),
    }
}

#[test]
fn visibility_folds_into_leading_scan() {
    for (vis_text, expected) in [
        ("public", Visibility::Public),
        ("private", Visibility::Private),
    ] {
        let src = format!("kind:function visibility:{vis_text}");
        let steps = chain_steps(parse(&src));
        assert_eq!(steps.len(), 1, "{src}");
        match &steps[0] {
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: Some(vis),
                ..
            } => assert_eq!(*vis, expected),
            other => panic!("unexpected root in {src:?}: {other:?}"),
        }
    }
}

#[test]
fn name_pattern_folds_into_leading_scan() {
    let steps = chain_steps(parse("kind:function name:parse_*"));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            name_pattern: Some(pat),
            ..
        } => {
            assert_eq!(pat.raw, "parse_*");
            assert_eq!(pat.mode, sqry_db::planner::MatchMode::Glob);
        }
        other => panic!("unexpected root: {other:?}"),
    }
}

#[test]
fn exact_name_pattern_is_chosen_when_no_glob_meta() {
    let steps = chain_steps(parse("kind:struct name:Parser"));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        PlanNode::NodeScan {
            name_pattern: Some(pat),
            ..
        } => {
            assert_eq!(pat.raw, "Parser");
            assert_eq!(pat.mode, sqry_db::planner::MatchMode::Exact);
        }
        other => panic!("unexpected root: {other:?}"),
    }
}

#[test]
fn kind_visibility_and_name_fold_together_into_single_scan() {
    let steps = chain_steps(parse("kind:function visibility:public name:parse_*"));
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: Some(Visibility::Public),
            name_pattern: Some(pat),
        } => {
            assert_eq!(pat.raw, "parse_*");
        }
        other => panic!("unexpected root: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Attribute-only filters
// ---------------------------------------------------------------------------

#[test]
fn has_caller_and_has_callee() {
    let steps = chain_steps(parse("kind:function has:caller"));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCaller
        }
    ));

    let steps = chain_steps(parse("kind:function has:callee"));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCallee
        }
    ));
}

#[test]
fn unused_is_isunused_filter() {
    let steps = chain_steps(parse("kind:function unused"));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::IsUnused
        }
    ));
}

#[test]
fn in_path_glob_filter() {
    let steps = chain_steps(parse("kind:function in:src/api/**"));
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::InFile(path),
        } => {
            assert_eq!(path.as_str(), "src/api/**");
        }
        other => panic!("expected InFile, got {other:?}"),
    }
}

#[test]
fn scope_kind_filter() {
    for (text, expected) in [
        ("module", ScopeKind::Module),
        ("function", ScopeKind::Function),
        ("class", ScopeKind::Class),
        ("namespace", ScopeKind::Namespace),
        ("trait", ScopeKind::Trait),
        ("impl", ScopeKind::Impl),
    ] {
        let src = format!("kind:function scope:{text}");
        let steps = chain_steps(parse(&src));
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::InScope(kind),
            } => assert_eq!(*kind, expected, "{src}"),
            other => panic!("{src:?}: expected InScope, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Relation predicates
// ---------------------------------------------------------------------------

#[test]
fn relation_predicates_with_pattern_values() {
    type Check = fn(&Predicate) -> bool;
    let cases: &[(&str, Check)] = &[
        ("callers", |p| matches!(p, Predicate::Callers(_))),
        ("callees", |p| matches!(p, Predicate::Callees(_))),
        ("imports", |p| matches!(p, Predicate::Imports(_))),
        ("exports", |p| matches!(p, Predicate::Exports(_))),
        ("implements", |p| matches!(p, Predicate::Implements(_))),
        ("impl", |p| matches!(p, Predicate::Implements(_))),
        ("references", |p| matches!(p, Predicate::References(_))),
    ];
    for (keyword, check) in cases {
        let src = format!("kind:function {keyword}:parse_expr");
        let steps = chain_steps(parse(&src));
        match &steps[1] {
            PlanNode::Filter { predicate } => {
                assert!(
                    check(predicate),
                    "keyword {keyword}: unexpected predicate {predicate:?}"
                );
            }
            other => panic!("{src:?}: expected Filter, got {other:?}"),
        }
    }
}

#[test]
fn callers_with_subquery_produces_plannode_subquery() {
    let steps = chain_steps(parse("kind:function callers:(kind:method)"));
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::Callers(PredicateValue::Subquery(inner)),
        } => match inner.as_ref() {
            PlanNode::Chain { steps: sub_steps } => {
                assert_eq!(sub_steps.len(), 1);
                assert!(matches!(
                    sub_steps[0],
                    PlanNode::NodeScan {
                        kind: Some(NodeKind::Method),
                        ..
                    }
                ));
            }
            other => panic!("expected Chain subquery, got {other:?}"),
        },
        other => panic!("expected Callers(Subquery), got {other:?}"),
    }
}

#[test]
fn nested_subquery_inside_outer_subquery() {
    let src = "kind:function callers:(kind:method callees:(kind:function))";
    let steps = chain_steps(parse(src));
    // Outer: Callers(Subquery(Chain[NodeScan(Method), Filter(Callees(Subquery(Chain[NodeScan(Function)])))]))
    let PlanNode::Filter {
        predicate: Predicate::Callers(PredicateValue::Subquery(outer_sub)),
    } = &steps[1]
    else {
        panic!("expected outer Callers(Subquery)");
    };
    let PlanNode::Chain { steps: outer_steps } = outer_sub.as_ref() else {
        panic!("outer subquery should be a Chain");
    };
    let PlanNode::Filter {
        predicate: Predicate::Callees(PredicateValue::Subquery(inner_sub)),
    } = &outer_steps[1]
    else {
        panic!("expected inner Callees(Subquery)");
    };
    match inner_sub.as_ref() {
        PlanNode::Chain { steps: inner_steps } => {
            assert!(matches!(
                inner_steps[0],
                PlanNode::NodeScan {
                    kind: Some(NodeKind::Function),
                    ..
                }
            ));
        }
        other => panic!("inner subquery Chain expected, got {other:?}"),
    }
}

#[test]
fn references_with_regex_and_flags() {
    let plan = parse("kind:function references ~= /handle_.*/ims");
    let steps = chain_steps(plan);
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::References(PredicateValue::Regex(re)),
        } => {
            assert_eq!(re.pattern, "handle_.*");
            assert!(re.flags.case_insensitive);
            assert!(re.flags.multiline);
            assert!(re.flags.dot_all);
        }
        other => panic!("expected References(Regex), got {other:?}"),
    }
}

#[test]
fn references_colon_still_accepts_pattern_form() {
    let steps = chain_steps(parse("kind:function references:parse_expr"));
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::References(PredicateValue::Pattern(pat)),
        } => {
            assert_eq!(pat.raw, "parse_expr");
        }
        other => panic!("expected References(Pattern), got {other:?}"),
    }
}

#[test]
fn quoted_string_value_preserves_spaces_and_escapes() {
    let steps = chain_steps(parse(r#"kind:function callers:"has space""#));
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::Callers(PredicateValue::Pattern(pat)),
        } => assert_eq!(pat.raw, "has space"),
        other => panic!("unexpected: {other:?}"),
    }

    let steps = chain_steps(parse(r#"kind:function callers:"with\"quote""#));
    match &steps[1] {
        PlanNode::Filter {
            predicate: Predicate::Callers(PredicateValue::Pattern(pat)),
        } => assert_eq!(pat.raw, "with\"quote"),
        other => panic!("unexpected: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Traverse
// ---------------------------------------------------------------------------

#[test]
fn traverse_with_every_direction_alias() {
    let cases = [
        ("forward", Direction::Forward),
        ("outgoing", Direction::Forward),
        ("out", Direction::Forward),
        ("reverse", Direction::Reverse),
        ("incoming", Direction::Reverse),
        ("in", Direction::Reverse),
        ("both", Direction::Both),
    ];
    for (text, expected) in cases {
        let src = format!("kind:function traverse:{text}(calls,2)");
        let steps = chain_steps(parse(&src));
        match &steps[1] {
            PlanNode::EdgeTraversal {
                direction,
                max_depth,
                ..
            } => {
                assert_eq!(*direction, expected, "{src}");
                assert_eq!(*max_depth, 2);
            }
            other => panic!("{src}: expected EdgeTraversal, got {other:?}"),
        }
    }
}

#[test]
fn traverse_accepts_every_supported_edge_kind() {
    // Each edge kind is compared via discriminant; we only need to check the
    // discriminant matches what the text parser should have built.
    let cases = [
        (
            "calls",
            std::mem::discriminant(&EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }),
        ),
        ("references", std::mem::discriminant(&EdgeKind::References)),
        (
            "imports",
            std::mem::discriminant(&EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            }),
        ),
        ("implements", std::mem::discriminant(&EdgeKind::Implements)),
        ("inherits", std::mem::discriminant(&EdgeKind::Inherits)),
        ("defines", std::mem::discriminant(&EdgeKind::Defines)),
        ("contains", std::mem::discriminant(&EdgeKind::Contains)),
    ];
    for (text, expected_disc) in cases {
        let src = format!("kind:function traverse:forward({text},1)");
        let steps = chain_steps(parse(&src));
        match &steps[1] {
            PlanNode::EdgeTraversal {
                edge_kind: Some(kind),
                ..
            } => {
                assert_eq!(std::mem::discriminant(kind), expected_disc, "{src}");
            }
            other => panic!("{src}: expected EdgeTraversal with edge_kind, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Full pipeline: the design-doc example round-trips through the parser.
// ---------------------------------------------------------------------------

#[test]
fn design_doc_example_roundtrips() {
    let plan = parse("kind:function has:caller traverse:reverse(calls,3) in:src/api/**");
    let steps = chain_steps(plan);
    assert_eq!(steps.len(), 4);
    assert!(matches!(
        steps[0],
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            ..
        }
    ));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCaller
        }
    ));
    match &steps[2] {
        PlanNode::EdgeTraversal {
            direction: Direction::Reverse,
            max_depth: 3,
            edge_kind: Some(ek),
            ..
        } => {
            assert_eq!(
                std::mem::discriminant(ek),
                std::mem::discriminant(&EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                })
            );
        }
        other => panic!("unexpected step[2]: {other:?}"),
    }
    match &steps[3] {
        PlanNode::Filter {
            predicate: Predicate::InFile(path),
        } => assert_eq!(path.as_str(), "src/api/**"),
        other => panic!("unexpected step[3]: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn empty_query_fails_build() {
    let err = parse_query("").unwrap_err();
    assert!(matches!(err, ParseError::Build(_)));
}

#[test]
fn whitespace_only_query_fails_build() {
    let err = parse_query("   \n\t  ").unwrap_err();
    assert!(matches!(err, ParseError::Build(_)));
}

#[test]
fn unknown_step_keyword_reports_unknown_ident() {
    let err = parse_query("kind:function notakeyword:foo").unwrap_err();
    match err {
        ParseError::UnknownIdent { kind, value, .. } => {
            assert_eq!(kind, "step keyword");
            assert_eq!(value, "notakeyword");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unknown_node_kind_reports_unknown_ident() {
    let err = parse_query("kind:gadget").unwrap_err();
    match err {
        ParseError::UnknownIdent { kind, value, .. } => {
            assert_eq!(kind, "node kind");
            assert_eq!(value, "gadget");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unknown_scope_kind_reports_unknown_ident() {
    let err = parse_query("kind:function scope:submarine").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnknownIdent {
            kind: "scope kind",
            ..
        }
    ));
}

#[test]
fn unknown_edge_kind_reports_unknown_ident() {
    let err = parse_query("kind:function traverse:forward(rollercoaster,2)").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnknownIdent {
            kind: "edge kind",
            ..
        }
    ));
}

#[test]
fn unknown_direction_reports_unknown_ident() {
    let err = parse_query("kind:function traverse:sideways(calls,2)").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnknownIdent {
            kind: "traversal direction",
            ..
        }
    ));
}

#[test]
fn invalid_has_target_reports_unknown_ident() {
    let err = parse_query("kind:function has:parent").unwrap_err();
    assert!(matches!(err, ParseError::UnknownIdent { .. }));
}

#[test]
fn missing_colon_after_kind_errors() {
    let err = parse_query("kind function").unwrap_err();
    match err {
        ParseError::UnexpectedChar { expected, .. } => {
            assert_eq!(expected, "':' after 'kind'");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unterminated_quoted_string_errors() {
    let err = parse_query(r#"kind:function callers:"unclosed"#).unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedEnd { .. }));
}

#[test]
fn unterminated_regex_errors() {
    let err = parse_query("kind:function references ~= /unclosed").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedEnd { .. }));
}

#[test]
fn unterminated_subquery_errors() {
    let err = parse_query("kind:function callers:(kind:method").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedChar { .. } | ParseError::UnexpectedEnd { .. }
    ));
}

#[test]
fn non_digit_depth_reports_unexpectedchar() {
    let err = parse_query("kind:function traverse:forward(calls,notnumber)").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedChar { .. }));
}

#[test]
fn zero_depth_surfaces_build_error() {
    let err = parse_query("kind:function traverse:forward(calls,0)").unwrap_err();
    assert!(matches!(err, ParseError::Build(_)));
}

// ---------------------------------------------------------------------------
// Determinism: same input, same plan (hash stability).
// ---------------------------------------------------------------------------

#[test]
fn parsing_identical_queries_yields_equal_plans() {
    let a = parse("kind:function has:caller in:src/**");
    let b = parse("kind:function has:caller in:src/**");
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// SetOperation reachability guard — the text parser does not currently expose
// set operations directly; this test documents that by attempting a textual
// union and ensuring it parses as a single filter rather than silently
// producing a SetOp. When a future text-syntax extension adds `|` / `UNION`
// tokens, this test should be updated to reflect the new behaviour.
// ---------------------------------------------------------------------------

#[test]
fn bar_pipe_character_is_rejected_for_now() {
    let err = parse_query("kind:function | kind:method").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedChar { .. } | ParseError::UnknownIdent { .. }
    ));
    // Silence unused SetOperation import while this test is a negative guard.
    let _ = SetOperation::Union;
    let _ = StringPattern::exact("sentinel");
}
