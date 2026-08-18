//! Reusable security detectors (L1) composing the L0 primitives.
//!
//! These are parameterized graph queries, not a linter ruleset: the shipped
//! `bbnty.security` pack contains only the universal, parameter-free
//! `unsafe_ffi_reach`; the pattern-taking detectors (`dangerous_sink`,
//! `missing_guard`) are a Rust authoring API the caller instantiates for their
//! own codebase. Every detector attaches advisory security metadata (P3).

pub mod dangerous_sink;
pub mod missing_guard;
pub mod unsafe_ffi_reach;

use sqry_core::graph::unified::NodeKind;
use sqry_db::planner::{Direction, Predicate, StringPattern};

use crate::ir::{PathKind, RuleEndpoint, RuleNode, RulePlan, TraversalEmit};
use crate::rules::ShippedRule;

/// Returns the universal, parameter-free security rules that ship in the
/// `bbnty.security` pack (and, via `shipped_rules()`, in `bbnty.all`).
///
/// The pattern-taking detectors (`dangerous_sink`, `missing_guard`) are NOT
/// shipped instances: baking fixed sink / guard names would be a codebase-
/// specific linter ruleset, out of sqry's semantic-search mission. They are the
/// reusable builder API instead.
#[must_use]
pub fn security_rules() -> Vec<ShippedRule> {
    vec![unsafe_ffi_reach::rule()]
}

// ---- shared plan constructors (pattern-preserving, unlike the string-taking
// recipe helpers) ----

/// A node scan over an optional kind, optionally name-filtered.
pub(crate) fn scan(kind: Option<NodeKind>, name: Option<StringPattern>) -> RuleNode {
    RuleNode::NodeScan {
        kind,
        visibility: None,
        name_pattern: name,
    }
}

/// A planner predicate filter step.
pub(crate) fn filter(predicate: Predicate) -> RuleNode {
    RuleNode::Filter { predicate }
}

/// Wraps a node as a query endpoint.
pub(crate) fn query(node: RuleNode) -> RuleEndpoint {
    RuleEndpoint::Query(Box::new(node))
}

/// A witness-bearing call-path query from `from` to `to`, bounded by depth and a
/// per-target path budget.
pub(crate) fn path_query(from: RuleNode, to: RuleNode, max_depth: u32) -> RuleNode {
    RuleNode::PathQuery {
        from: query(from),
        to: query(to),
        kind: PathKind::Calls,
        max_depth,
        max_paths: Some(32),
        avoid: None,
    }
}

/// A witness-bearing call-path query that excludes paths through `guard`.
pub(crate) fn path_query_avoiding(
    from: RuleNode,
    to: RuleNode,
    guard: RuleNode,
    max_depth: u32,
) -> RuleNode {
    RuleNode::PathQuery {
        from: query(from),
        to: query(to),
        kind: PathKind::Calls,
        max_depth,
        max_paths: Some(32),
        avoid: Some(query(guard)),
    }
}

/// A cross-boundary edge traversal emitting a chosen node set.
pub(crate) fn traverse_cross_boundary_emitting(
    direction: Direction,
    max_depth: u32,
    emit: TraversalEmit,
) -> RuleNode {
    RuleNode::EdgeTraversal {
        direction,
        edge_class: None,
        max_depth,
        resolved_via: None,
        cross_boundary: Some(true),
        emit,
    }
}

/// Wraps steps in a chain plan.
pub(crate) fn chain(steps: Vec<RuleNode>) -> RulePlan {
    RulePlan::new(RuleNode::Chain { steps })
}

#[cfg(test)]
mod tests;
