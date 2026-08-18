//! Unit tests for the L1 security detectors: plan shape + metadata.

use sqry_db::planner::{Direction, Predicate, StringPattern};

use super::{dangerous_sink, missing_guard, security_rules, unsafe_ffi_reach};
use crate::ir::{RuleNode, TraversalEmit};
use crate::witness::RuleSeverity;

#[test]
fn unsafe_ffi_reach_has_the_nested_emit_sources_shape() {
    let def = unsafe_ffi_reach::definition();
    assert_eq!(def.id, unsafe_ffi_reach::RULE_ID);
    let RuleNode::Chain { steps } = def.plan.root() else {
        panic!("expected an outer chain");
    };
    assert_eq!(steps.len(), 2, "inner unsafe-node chain + traversal");
    // Inner chain: NodeScan(any) -> Filter(IsUnsafe(true)).
    let RuleNode::Chain { steps: inner } = &steps[0] else {
        panic!("expected an inner chain");
    };
    assert!(matches!(
        &inner[0],
        RuleNode::NodeScan {
            kind: None,
            name_pattern: None,
            ..
        }
    ));
    assert!(matches!(
        &inner[1],
        RuleNode::Filter {
            predicate: Predicate::IsUnsafe(true)
        }
    ));
    // Tail: cross-boundary traversal emitting the sources (the unsafe fns that crossed).
    assert!(matches!(
        &steps[1],
        RuleNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_class: None,
            cross_boundary: Some(true),
            emit: TraversalEmit::EdgeSources,
            ..
        }
    ));
}

#[test]
fn unsafe_ffi_reach_carries_advisory_metadata_but_no_cwe() {
    let def = unsafe_ffi_reach::definition();
    assert_eq!(def.severity, Some(RuleSeverity::Warning));
    assert!(def.description.is_some());
    assert!(def.remediation.is_some());
    // No single CWE spans FFI/DB/service; the shipped rule omits it.
    assert_eq!(def.cwe, None);
}

#[test]
fn security_rules_ship_only_the_universal_detector() {
    let rules = security_rules();
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    assert_eq!(rule.id(), unsafe_ffi_reach::RULE_ID);
    assert!(
        !rule.requires_trace_path,
        "uses EdgeTraversal, not trace_path"
    );
    assert!(!rule.requires_beside_cache);
}

#[test]
fn missing_guard_is_a_root_guard_avoiding_path_query() {
    let def = missing_guard::definition(
        "test.missing_guard",
        StringPattern::contains("entry"),
        StringPattern::contains("sink"),
        StringPattern::contains("guard"),
        6,
    );
    assert!(matches!(
        def.plan.root(),
        RuleNode::PathQuery {
            max_depth: 6,
            avoid: Some(_),
            ..
        }
    ));
    assert_eq!(def.severity, Some(RuleSeverity::Warning));
    assert_eq!(def.cwe, None, "CWE is caller-supplied");
}

#[test]
fn trust_boundary_is_the_same_shape_as_missing_guard_with_boundary_metadata() {
    let def = missing_guard::trust_boundary(
        "test.trust_boundary",
        StringPattern::contains("recv"),
        StringPattern::contains("sink"),
        StringPattern::contains("validate"),
        6,
    );
    assert!(matches!(
        def.plan.root(),
        RuleNode::PathQuery { avoid: Some(_), .. }
    ));
    assert_eq!(def.severity, Some(RuleSeverity::Warning));
    assert!(
        def.description
            .as_deref()
            .unwrap_or_default()
            .contains("trust boundary")
    );
}

#[test]
fn trust_boundary_delegates_to_missing_guard_and_only_swaps_metadata() {
    // Same id + patterns => identical plan and severity; only the description /
    // remediation differ (trust_boundary is a thin wrapper over definition).
    let mg = missing_guard::definition(
        "same.id",
        StringPattern::contains("b"),
        StringPattern::contains("s"),
        StringPattern::contains("v"),
        6,
    );
    let tb = missing_guard::trust_boundary(
        "same.id",
        StringPattern::contains("b"),
        StringPattern::contains("s"),
        StringPattern::contains("v"),
        6,
    );
    assert_eq!(
        tb.plan, mg.plan,
        "trust_boundary reuses the missing_guard plan"
    );
    assert_eq!(tb.severity, mg.severity);
    assert_ne!(
        tb.description, mg.description,
        "metadata is boundary-specific"
    );
}

#[test]
fn dangerous_sink_is_a_root_path_query_with_caller_supplied_cwe() {
    let def = dangerous_sink::definition(
        "test.dangerous_sink",
        StringPattern::contains("read_input"),
        StringPattern::contains("system"),
        5,
    );
    assert!(matches!(
        def.plan.root(),
        RuleNode::PathQuery { max_depth: 5, .. }
    ));
    assert_eq!(def.severity, Some(RuleSeverity::Warning));
    // CWE is caller-supplied: the builder does not bake one.
    assert_eq!(def.cwe, None);
    let with_cwe = dangerous_sink::definition(
        "test.dangerous_sink",
        StringPattern::contains("read_input"),
        StringPattern::contains("system"),
        5,
    )
    .with_cwe("CWE-78");
    assert_eq!(with_cwe.cwe.as_deref(), Some("CWE-78"));
}
