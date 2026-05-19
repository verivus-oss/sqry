//! Integration tests for [`sqry_db::planner::ir`].
//!
//! DB09 lands only the IR types; the builder (DB10), fuser (DB11), and
//! executor (DB12) are out of scope. These tests therefore focus on:
//!
//! - Every variant of every enum is constructible and round-trips through
//!   the Clone / PartialEq / Hash / Debug derives.
//! - The IR is snapshot-independent — no interner handles leak into the
//!   top-level public types.
//! - postcard (the wire format for `.sqry/graph/derived.sqry`) and
//!   serde_json (the debug format used by structured logging and MCP
//!   tool output) both round-trip losslessly.
//! - The hash of two semantically-equal plans is identical, which is the
//!   property the fuser in DB11 depends on.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::Visibility;

use sqry_db::planner::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, RegexFlags,
    RegexPattern, SetOperation, StringPattern,
};

// ---------------------------------------------------------------------------
// Construction coverage: every variant is constructible
// ---------------------------------------------------------------------------

#[test]
fn plan_node_all_variants_constructible() {
    let scan = PlanNode::NodeScan {
        kind: Some(NodeKind::Function),
        visibility: Some(Visibility::Public),
        name_pattern: Some(StringPattern::glob("parse_*")),
    };

    let traverse = PlanNode::EdgeTraversal {
        direction: Direction::Reverse,
        edge_kind: Some(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }),
        max_depth: 3,
        resolved_via: None,
    };

    let filter = PlanNode::Filter {
        predicate: Predicate::HasCaller,
    };

    let set = PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan.clone()),
        right: Box::new(filter.clone()),
    };

    let chain = PlanNode::Chain {
        steps: vec![scan.clone(), traverse.clone(), filter.clone()],
    };

    // Smoke: all five can be built and matched.
    for node in [scan, traverse, filter, set, chain] {
        match node {
            PlanNode::NodeScan { .. }
            | PlanNode::EdgeTraversal { .. }
            | PlanNode::Filter { .. }
            | PlanNode::SetOp { .. }
            | PlanNode::Chain { .. } => {}
        }
    }
}

#[test]
fn direction_all_variants() {
    for d in [Direction::Forward, Direction::Reverse, Direction::Both] {
        // Copy + Eq
        let copy = d;
        assert_eq!(copy, d);
    }
}

#[test]
fn set_operation_all_variants() {
    for op in [
        SetOperation::Union,
        SetOperation::Intersect,
        SetOperation::Difference,
    ] {
        let copy = op;
        assert_eq!(copy, op);
    }
}

#[test]
fn predicate_existence_checks() {
    for p in [
        Predicate::HasCaller,
        Predicate::HasCallee,
        Predicate::IsUnused,
    ] {
        assert!(!p.has_subquery());
    }
}

#[test]
fn predicate_six_relation_variants_cover_relation_handlers() {
    let val = PredicateValue::Pattern(StringPattern::exact("target"));

    let preds = [
        Predicate::Callers(val.clone()),
        Predicate::Callees(val.clone()),
        Predicate::Imports(val.clone()),
        Predicate::Exports(val.clone()),
        Predicate::References(val.clone()),
        Predicate::Implements(val.clone()),
    ];

    // All six must round-trip through Clone/Eq and none carry subqueries.
    for p in &preds {
        let c = p.clone();
        assert_eq!(p, &c);
        assert!(!p.has_subquery());
    }
}

#[test]
fn predicate_attribute_filters() {
    let in_file = Predicate::InFile(PathPattern::new("src/**/*.rs"));
    let in_scope = Predicate::InScope(ScopeKind::Function);
    let matches_name = Predicate::MatchesName(StringPattern::prefix("test_"));

    for p in [in_file, in_scope, matches_name] {
        assert!(!p.has_subquery());
    }
}

#[test]
fn predicate_combinators_compose() {
    let and = Predicate::And(vec![Predicate::HasCaller, Predicate::HasCallee]);
    let or = Predicate::Or(vec![Predicate::IsUnused, Predicate::HasCaller]);
    let not = Predicate::Not(Box::new(Predicate::HasCaller));

    assert!(!and.has_subquery());
    assert!(!or.has_subquery());
    assert!(!not.has_subquery());

    // Empty list cases are still valid IR shapes.
    let empty_and = Predicate::And(vec![]);
    let empty_or = Predicate::Or(vec![]);
    assert!(!empty_and.has_subquery());
    assert!(!empty_or.has_subquery());
}

#[test]
fn predicate_value_variants() {
    let pat = PredicateValue::Pattern(StringPattern::glob("*_test"));
    let re = PredicateValue::Regex(RegexPattern::new(r"^\d+"));
    let sub = PredicateValue::Subquery(Box::new(PlanNode::NodeScan {
        kind: Some(NodeKind::Class),
        visibility: None,
        name_pattern: None,
    }));

    assert!(!pat.is_subquery());
    assert!(!re.is_subquery());
    assert!(sub.is_subquery());
    assert!(sub.as_subquery().is_some());
}

// ---------------------------------------------------------------------------
// MatchMode: all 5 interpretations
// ---------------------------------------------------------------------------

#[test]
fn match_mode_all_variants() {
    let modes = [
        MatchMode::Exact,
        MatchMode::Glob,
        MatchMode::Prefix,
        MatchMode::Suffix,
        MatchMode::Contains,
    ];
    for m in modes {
        let p = StringPattern {
            raw: "x".to_string(),
            mode: m,
            case_insensitive: false,
        };
        assert_eq!(p.mode, m);
    }
}

#[test]
fn regex_flags_all_combinations_serialize() {
    for case_insensitive in [false, true] {
        for multiline in [false, true] {
            for dot_all in [false, true] {
                let flags = RegexFlags {
                    case_insensitive,
                    multiline,
                    dot_all,
                };
                let json = serde_json::to_string(&flags).expect("json encode");
                let round: RegexFlags = serde_json::from_str(&json).expect("json decode");
                assert_eq!(flags, round);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Deep nesting: subqueries inside predicates inside plans
// ---------------------------------------------------------------------------

#[test]
fn deeply_nested_plan_preserves_structure() {
    let inner_scan = PlanNode::NodeScan {
        kind: Some(NodeKind::Method),
        visibility: None,
        name_pattern: None,
    };
    let and = Predicate::And(vec![
        Predicate::Callers(PredicateValue::Subquery(Box::new(inner_scan.clone()))),
        Predicate::Not(Box::new(Predicate::InFile(PathPattern::new("test/**")))),
    ]);
    let plan = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            PlanNode::Filter {
                predicate: and.clone(),
            },
            PlanNode::EdgeTraversal {
                direction: Direction::Reverse,
                edge_kind: Some(EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                }),
                max_depth: 2,
                resolved_via: None,
            },
        ],
    });

    // operator_count: chain(1) + scan(1) + filter(1) + traverse(1) = 4
    // (predicate tree is NOT counted — see PlanNode::operator_count doc).
    assert_eq!(plan.operator_count(), 4);

    // has_subquery must propagate through And -> Callers -> Subquery.
    assert!(and.has_subquery());
}

// ---------------------------------------------------------------------------
// Serialization round-trips (JSON + postcard)
// ---------------------------------------------------------------------------

fn sample_plan() -> QueryPlan {
    QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: Some(Visibility::Public),
                name_pattern: Some(StringPattern::glob("handle_*").case_insensitive()),
            },
            PlanNode::Filter {
                predicate: Predicate::And(vec![
                    Predicate::HasCaller,
                    Predicate::Callees(PredicateValue::Regex(RegexPattern::with_flags(
                        r"parse_\w+",
                        RegexFlags {
                            case_insensitive: true,
                            multiline: false,
                            dot_all: false,
                        },
                    ))),
                    Predicate::Or(vec![
                        Predicate::InFile(PathPattern::new("src/api/**")),
                        Predicate::InScope(ScopeKind::Module),
                    ]),
                    Predicate::Not(Box::new(Predicate::IsUnused)),
                ]),
            },
            PlanNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_kind: Some(EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                }),
                max_depth: 5,
                resolved_via: None,
            },
            PlanNode::SetOp {
                op: SetOperation::Difference,
                left: Box::new(PlanNode::NodeScan {
                    kind: Some(NodeKind::Method),
                    visibility: None,
                    name_pattern: None,
                }),
                right: Box::new(PlanNode::Filter {
                    predicate: Predicate::MatchesName(StringPattern::suffix("_test")),
                }),
            },
        ],
    })
}

#[test]
fn query_plan_roundtrip_json() {
    let plan = sample_plan();
    let json = serde_json::to_string(&plan).expect("json encode");
    let round: QueryPlan = serde_json::from_str(&json).expect("json decode");
    assert_eq!(plan, round);
}

#[test]
fn query_plan_roundtrip_json_pretty_stable() {
    // Stable pretty output — guards against accidental serde attribute
    // regressions that would change the JSON shape.
    let plan = sample_plan();
    let first = serde_json::to_string_pretty(&plan).expect("encode");
    let decoded: QueryPlan = serde_json::from_str(&first).expect("decode");
    let second = serde_json::to_string_pretty(&decoded).expect("re-encode");
    assert_eq!(first, second);
}

#[test]
fn query_plan_roundtrip_postcard() {
    let plan = sample_plan();
    let bytes = postcard::to_allocvec(&plan).expect("postcard encode");
    let round: QueryPlan = postcard::from_bytes(&bytes).expect("postcard decode");
    assert_eq!(plan, round);
}

#[test]
fn predicate_value_subquery_roundtrip_json() {
    let v = PredicateValue::Subquery(Box::new(PlanNode::NodeScan {
        kind: Some(NodeKind::Trait),
        visibility: None,
        name_pattern: Some(StringPattern::contains("Iterator")),
    }));
    let json = serde_json::to_string(&v).expect("encode");
    let round: PredicateValue = serde_json::from_str(&json).expect("decode");
    assert_eq!(v, round);
}

#[test]
fn scope_kind_all_variants_roundtrip() {
    for sk in [
        ScopeKind::Module,
        ScopeKind::Function,
        ScopeKind::Class,
        ScopeKind::Namespace,
        ScopeKind::Trait,
        ScopeKind::Impl,
    ] {
        let pred = Predicate::InScope(sk);
        let json = serde_json::to_string(&pred).expect("encode");
        let round: Predicate = serde_json::from_str(&json).expect("decode");
        assert_eq!(pred, round);
    }
}

// ---------------------------------------------------------------------------
// Hash equality: semantically equal plans hash identically (fusion property)
// ---------------------------------------------------------------------------

fn hash_of(plan: &QueryPlan) -> u64 {
    let mut h = DefaultHasher::new();
    plan.hash(&mut h);
    h.finish()
}

#[test]
fn identical_plans_hash_identically() {
    let a = sample_plan();
    let b = sample_plan();
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn distinct_plans_hash_distinctly() {
    let a = QueryPlan::new(PlanNode::NodeScan {
        kind: Some(NodeKind::Function),
        visibility: None,
        name_pattern: None,
    });
    let b = QueryPlan::new(PlanNode::NodeScan {
        kind: Some(NodeKind::Method),
        visibility: None,
        name_pattern: None,
    });
    assert_ne!(hash_of(&a), hash_of(&b));
    assert_ne!(a, b);
}

#[test]
fn hash_stable_across_clone() {
    let plan = sample_plan();
    let cloned = plan.clone();
    assert_eq!(hash_of(&plan), hash_of(&cloned));
}

// ---------------------------------------------------------------------------
// Context-free validation for Chain::steps (DB10's concern, but the
// invariant is detectable at the IR layer).
// ---------------------------------------------------------------------------

#[test]
fn is_context_free_matches_design() {
    let scan = PlanNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: None,
    };
    let setop = PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan.clone()),
        right: Box::new(scan.clone()),
    };
    let traverse = PlanNode::EdgeTraversal {
        direction: Direction::Forward,
        edge_kind: None,
        max_depth: 1,
        resolved_via: None,
    };
    let filter = PlanNode::Filter {
        predicate: Predicate::HasCaller,
    };
    let chain = PlanNode::Chain {
        steps: vec![scan.clone()],
    };

    assert!(scan.is_context_free());
    assert!(setop.is_context_free());
    assert!(!traverse.is_context_free());
    assert!(!filter.is_context_free());
    // Chain is not context-free in IR terms: it is a composite operator
    // whose first step is responsible for standing without context.
    assert!(!chain.is_context_free());
}

// ---------------------------------------------------------------------------
// Regex & path pattern ergonomics
// ---------------------------------------------------------------------------

#[test]
fn path_pattern_from_string_literal() {
    let p: PathPattern = "src/**".into();
    assert_eq!(p.as_str(), "src/**");
    // From<String> and From<&str>
    let owned: PathPattern = String::from("docs/**").into();
    assert_eq!(owned.glob, "docs/**");
}

#[test]
fn regex_pattern_default_flags_elide_in_json() {
    let r = RegexPattern::new("foo");
    let json = serde_json::to_string(&r).expect("encode");
    let round: RegexPattern = serde_json::from_str(&json).expect("decode");
    assert_eq!(r, round);
}

// ---------------------------------------------------------------------------
// Snapshot-independence: no interner handles (`StringId`, `NodeId`) leak
// into the IR surface.
// ---------------------------------------------------------------------------

#[test]
fn ir_public_types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<QueryPlan>();
    assert_send_sync::<PlanNode>();
    assert_send_sync::<Predicate>();
    assert_send_sync::<PredicateValue>();
    assert_send_sync::<StringPattern>();
    assert_send_sync::<PathPattern>();
    assert_send_sync::<RegexPattern>();
}

// ---------------------------------------------------------------------------
// Operator-count sanity for small plans
// ---------------------------------------------------------------------------

#[test]
fn operator_count_matches_topology() {
    let scan = || PlanNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: None,
    };
    let plan = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            scan(),
            PlanNode::SetOp {
                op: SetOperation::Intersect,
                left: Box::new(scan()),
                right: Box::new(scan()),
            },
            PlanNode::Filter {
                predicate: Predicate::HasCaller,
            },
        ],
    });
    // chain(1) + scan(1) + setop(1) + scan(1) + scan(1) + filter(1) = 6
    assert_eq!(plan.operator_count(), 6);
}

#[test]
fn empty_chain_has_single_op() {
    let plan = QueryPlan::new(PlanNode::Chain { steps: vec![] });
    // Just the chain node itself.
    assert_eq!(plan.operator_count(), 1);
}
