//! D3: `missing_guard` - a sink reachable from a source WITHOUT the guard on the
//! path, and its `trust_boundary` framing.
//!
//! Parameterized builders (NOT shipped instances): the caller supplies the
//! source, sink, and guard name patterns for their codebase. Composes L1
//! Primitive B (`PathQuery.avoid`): the witness call paths are the source->sink
//! paths that avoid the guard, i.e. the sink is reachable without the guard.
//! CWE is caller-supplied (`.with_cwe(...)`).
//!
//! Soundness bound (documented): path enumeration is capped per target by
//! `max_paths`, so a returned guard-avoiding path is a sound positive, but the
//! absence of one is NOT a proof that every path passes the guard.

use sqry_db::planner::StringPattern;

use super::{path_query_avoiding, scan};
use crate::dsl::RuleDefinition;
use crate::ir::RulePlan;
use crate::witness::RuleSeverity;

/// Builds a `missing_guard` detector: call paths from `source` to `sink` that do
/// NOT pass through `guard` (the sink is reachable without the guard).
///
/// Advisory `Warning` severity, a description, and a remediation; the caller
/// attaches a CWE via `.with_cwe(...)`.
#[must_use]
pub fn definition(
    id: impl Into<String>,
    source: StringPattern,
    sink: StringPattern,
    guard: StringPattern,
    max_depth: u32,
) -> RuleDefinition {
    // A root guard-avoiding PathQuery (NOT wrapped in a Chain): PathQuery yields
    // witness paths, and a single-step chain would demand a node-producing first
    // step.
    let plan = RulePlan::new(path_query_avoiding(
        scan(None, Some(source)),
        scan(None, Some(sink)),
        scan(None, Some(guard)),
        max_depth,
    ));
    RuleDefinition::new(id, plan)
        .with_severity(RuleSeverity::Warning)
        .with_description(
            "a sink is reachable from a source without the required guard on the path (audit candidate)",
        )
        .with_remediation("ensure the guard is enforced on every path from the source to the sink")
}

/// Builds a `trust_boundary` detector: a thin framing of [`definition`] where
/// the "guard" is a validator on a trust boundary. Data from `boundary` reaches
/// `sink` without passing the `validator`.
///
/// This is deliberately the same query as `missing_guard` (a boundary crossing
/// that skips validation IS a missing guard); it exists only to carry
/// boundary-specific metadata, not to duplicate the plan.
#[must_use]
pub fn trust_boundary(
    id: impl Into<String>,
    boundary: StringPattern,
    sink: StringPattern,
    validator: StringPattern,
    max_depth: u32,
) -> RuleDefinition {
    // A thin wrapper: the plan (and severity) is exactly `definition`'s
    // guard-avoiding PathQuery with the validator as the guard; only the
    // boundary-specific description / remediation differ. Delegating (rather
    // than rebuilding the plan) keeps the two from drifting.
    definition(id, boundary, sink, validator, max_depth)
        .with_description(
            "data crosses a trust boundary and reaches a sink without passing the validator (audit candidate)",
        )
        .with_remediation("validate or sanitize boundary input before it reaches the sink")
}
