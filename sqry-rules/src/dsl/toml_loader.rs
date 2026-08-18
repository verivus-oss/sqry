//! TOML loader for shareable rule packs.

use crate::dsl::{RuleDefinition, RulePack};
use crate::ir::RulePlan;
use crate::{RuleError, RuleResult};

/// Loads a rule pack from a TOML string.
///
/// # Errors
///
/// Returns `RuleError::InvalidRuleSource` when the schema version is not
/// supported, or `RuleError::Analysis` when TOML deserialization fails.
pub fn load_rule_pack_str(source: &str) -> RuleResult<RulePack> {
    let pack: RulePack = toml::from_str(source).map_err(anyhow::Error::from)?;
    // Accept the inclusive supported range `1..=CURRENT`: every version up to
    // and including the current one loads (older packs stay valid because the
    // schema-2 metadata fields are optional), while `0` and any future version
    // are rejected.
    if pack.schema_version < 1 || pack.schema_version > super::schema::RULE_PACK_SCHEMA_VERSION {
        return Err(RuleError::InvalidRuleSource {
            reason: "unsupported rule pack schema version",
        });
    }
    if pack.rules.is_empty() {
        return Err(RuleError::InvalidRuleSource {
            reason: "rule pack contains no rules",
        });
    }
    Ok(pack)
}

/// Loads exactly one rule plan from a TOML string.
///
/// # Errors
///
/// Returns `RuleError::InvalidRuleSource` when the pack does not contain
/// exactly one rule, or `RuleError::Analysis` when TOML deserialization fails.
pub fn load_rule_plan_str(source: &str) -> RuleResult<RulePlan> {
    let mut pack = load_rule_pack_str(source)?;
    if pack.rules.len() != 1 {
        return Err(RuleError::InvalidRuleSource {
            reason: "expected exactly one rule in rule pack",
        });
    }
    let Some(RuleDefinition { plan, .. }) = pack.rules.pop() else {
        return Err(RuleError::InvalidRuleSource {
            reason: "expected exactly one rule in rule pack",
        });
    };
    Ok(plan)
}
