//! Rule plan wrapper.

use serde::{Deserialize, Serialize};
use sqry_db::planner::QueryPlan;

use super::RuleNode;

/// Top-level rule plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePlan {
    /// Root rule node.
    pub root: RuleNode,
}

impl RulePlan {
    /// Creates a rule plan.
    #[must_use]
    pub const fn new(root: RuleNode) -> Self {
        Self { root }
    }

    /// Converts a set-only `sqry-db` planner plan into rule IR.
    #[must_use]
    pub fn from_query_plan(plan: QueryPlan) -> Self {
        Self {
            root: RuleNode::from(plan.root),
        }
    }

    /// Returns the root node.
    #[must_use]
    pub const fn root(&self) -> &RuleNode {
        &self.root
    }
}
