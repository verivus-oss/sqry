//! D2: `dangerous_sink` - reachability from a source to a named dangerous sink.
//!
//! Parameterized builder (NOT a shipped instance): the caller supplies the
//! source and sink name patterns for their codebase. A standalone `PathQuery`
//! whose witness call paths ARE the finding; works on today's engine (no L1
//! primitive needed). CWE is caller-supplied (`.with_cwe(...)`) so the weakness
//! class matches the specific sink.

use sqry_db::planner::StringPattern;

use super::{path_query, scan};
use crate::dsl::RuleDefinition;
use crate::ir::RulePlan;
use crate::witness::RuleSeverity;

/// Builds a `dangerous_sink` detector: call paths from any `source`-named node to
/// any `sink`-named node, up to `max_depth`.
///
/// Returns a `RuleDefinition` with advisory `Warning` severity, a description,
/// and a remediation. The caller attaches a CWE via `.with_cwe(...)` (the
/// weakness class depends on the concrete sink) and may override the `id`
/// through the returned definition.
#[must_use]
pub fn definition(
    id: impl Into<String>,
    source: StringPattern,
    sink: StringPattern,
    max_depth: u32,
) -> RuleDefinition {
    // A root PathQuery (NOT wrapped in a Chain): PathQuery yields witness paths,
    // and a single-step chain would demand a node-producing first step.
    let plan = RulePlan::new(path_query(
        scan(None, Some(source)),
        scan(None, Some(sink)),
        max_depth,
    ));
    RuleDefinition::new(id, plan)
        .with_severity(RuleSeverity::Warning)
        .with_description(
            "a source reaches a dangerous sink on a bounded call path (audit candidate)",
        )
        .with_remediation(
            "verify the reachable sink is guarded, sanitized, or unreachable in practice",
        )
}
