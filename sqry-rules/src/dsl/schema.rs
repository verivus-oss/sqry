//! Serde schema for shareable rule packs.

use serde::{Deserialize, Serialize};

use crate::ir::RulePlan;
use crate::witness::RuleSeverity;

/// TOML rule pack schema version supported by this crate.
///
/// Schema 2 (L0-P3) added optional per-rule security metadata (`severity`,
/// `cwe`, `description`, `remediation`) to [`RuleDefinition`]. All four are
/// optional and default-absent, so schema 1 packs remain valid; the loader
/// accepts the inclusive range `1..=RULE_PACK_SCHEMA_VERSION`.
pub const RULE_PACK_SCHEMA_VERSION: u32 = 2;

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
///
/// `#[non_exhaustive]`: construct via [`RuleDefinition::new`] plus the chained
/// `with_*` setters, never a struct literal from outside this crate. This keeps
/// future field additions (the schema-2 metadata was the first) non-breaking for
/// downstream consumers of the published `sqry-rules` crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RuleDefinition {
    /// Stable rule identifier.
    pub id: String,
    /// Canonical rule plan.
    pub plan: RulePlan,
    /// Optional authored severity (schema 2+). When present, it overrides the
    /// caller-supplied default at run time; absent means "use the caller default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<RuleSeverity>,
    /// Optional CWE identifier for the finding this rule surfaces, e.g. `"CWE-89"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwe: Option<String>,
    /// Optional human-readable description of what the rule detects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional remediation guidance for a match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl RuleDefinition {
    /// Creates a rule definition with no security metadata (schema 1 shape).
    ///
    /// The metadata fields default to `None`, so every existing caller keeps
    /// compiling; authoring is done via the chained `with_*` setters below or a
    /// schema 2 TOML pack.
    #[must_use]
    pub fn new(id: impl Into<String>, plan: RulePlan) -> Self {
        Self {
            id: id.into(),
            plan,
            severity: None,
            cwe: None,
            description: None,
            remediation: None,
        }
    }

    /// Sets the authored severity (overrides the caller default at run time).
    #[must_use]
    pub fn with_severity(mut self, severity: RuleSeverity) -> Self {
        self.severity = Some(severity);
        self
    }

    /// Sets the CWE identifier.
    #[must_use]
    pub fn with_cwe(mut self, cwe: impl Into<String>) -> Self {
        self.cwe = Some(cwe.into());
        self
    }

    /// Sets the human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the remediation guidance.
    #[must_use]
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }
}
