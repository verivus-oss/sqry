//! Canonical rule intermediate representation.

mod node;
mod plan;

pub use node::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RuleSimilarityKind,
};
pub use plan::RulePlan;

#[cfg(test)]
mod tests;
