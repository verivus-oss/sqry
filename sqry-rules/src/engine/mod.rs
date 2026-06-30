//! Stateless rule engine dispatcher.

mod dispatcher;

#[cfg(test)]
mod tests;

pub use dispatcher::{
    NoopCancellationToken, RuleCancellationToken, RuleEngine, RuleEngineConfig, RuleMetricValue,
    RuleOutput, RuleRelationRows, RuleRun,
};
