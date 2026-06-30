//! Serde schema for shareable rule packs.

use serde::{Deserialize, Serialize};

use crate::ir::RulePlan;

/// TOML rule pack schema version supported by this crate.
pub const RULE_PACK_SCHEMA_VERSION: u32 = 1;

/// A shareable rule pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePack {
    /// Schema version.
    pub schema_version: u32,
    /// Rules in deterministic evaluation order.
    pub rules: Vec<RuleDefinition>,
}

impl RulePack {
    /// Creates a rule pack using the current schema version.
    #[must_use]
    pub fn new(rules: Vec<RuleDefinition>) -> Self {
        Self {
            schema_version: RULE_PACK_SCHEMA_VERSION,
            rules,
        }
    }
}

/// One rule definition in a shareable rule pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDefinition {
    /// Stable rule identifier.
    pub id: String,
    /// Canonical rule plan.
    pub plan: RulePlan,
}

impl RuleDefinition {
    /// Creates a rule definition.
    #[must_use]
    pub fn new(id: impl Into<String>, plan: RulePlan) -> Self {
        Self {
            id: id.into(),
            plan,
        }
    }
}
