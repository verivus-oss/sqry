//! Witness contract for declarative rule execution.

mod citation;
mod step;

pub use citation::{CitationSpan, RuleCitation};
pub use step::{
    DEFAULT_RULE_WITNESS_STEP_CAP, DiffEntryKind, PathBudgetReason, RulePredicateKind,
    RuleSeverity, RuleStep, RuleWitness,
};

#[cfg(test)]
mod tests;
