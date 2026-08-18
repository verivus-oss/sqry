//! Production rule packs shipped with `sqry-rules`.

pub mod intake;
pub mod recipes;
pub mod security;

use crate::dsl::RuleDefinition;

/// Stable L5 rule-IR variant names used by proof-rule metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleVariant {
    /// Reused planner node scan.
    NodeScan,
    /// Reused planner filter.
    Filter,
    /// Rule-level edge traversal (witness-bearing; see `RuleNode::EdgeTraversal`).
    EdgeTraversal,
    /// Reused planner set operation.
    SetOp,
    /// Reused planner chain.
    Chain,
    /// Witness-bearing path query.
    PathQuery,
    /// Bounded subgraph extraction.
    SubgraphExtract,
    /// Relation edge emission.
    RelationEdges,
    /// Cycle witness query.
    CycleWitness,
    /// Reference-source query.
    ReferencesAt,
    /// Complexity aggregate query.
    ComplexityAggregate,
    /// Cross-snapshot semantic diff.
    CrossSnapshotDiff,
    /// Entry-point union query.
    EntryPointUnion,
    /// Beside-cache similarity query.
    SimilarTo,
}

/// Shared metadata for a shipped rule definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedRule {
    /// Stable rule definition consumed by the engine/front ends.
    pub definition: RuleDefinition,
    /// Human-readable rule title.
    pub title: &'static str,
    /// Source-methodology section for hand review.
    pub methodology: &'static str,
    /// bbnty finding that anchors hand verification.
    pub seed_finding: Option<&'static str>,
    /// IR variants intentionally exercised by the rule.
    pub variants: &'static [RuleVariant],
    /// Whether execution must route through beside-cache coordination.
    pub requires_beside_cache: bool,
    /// Whether the rule relies on the rule-backend trace-path primitive.
    pub requires_trace_path: bool,
    /// Local hand-authored composition baseline used by smoke budget tests.
    pub baseline_ms_floor: u64,
}

impl ShippedRule {
    /// Returns the stable rule ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.definition.id
    }
}

/// Returns every shipped rule: the proof recipes, the standard intake rules,
/// and the universal security detectors.
#[must_use]
pub fn shipped_rules() -> Vec<ShippedRule> {
    let mut rules = recipes::bbnty_recipe_rules();
    rules.extend(intake::standard_intake_rules());
    rules.extend(security::security_rules());
    rules
}
