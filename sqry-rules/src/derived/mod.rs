//! Derived-query and beside-cache integration for cacheable rule execution.

pub mod beside_cache;
pub mod cacheable;

pub use beside_cache::{
    BesideCachePrimitive, BesideCacheRoute, beside_cache_route_for,
    requires_unsupported_beside_cache,
};
pub use cacheable::{
    CacheableRuleQuery, CacheableRuleVariant, ComplexityRuleQuery, ComplexityRuleQueryKey,
    CycleWitnessRuleQuery, CycleWitnessRuleQueryKey, EntryPointUnionRuleQuery,
    EntryPointUnionRuleQueryKey, PathRuleQuery, PathRuleQueryKey, ReferencesAtRuleQuery,
    ReferencesAtRuleQueryKey, RelationEdgesRuleQuery, RelationEdgesRuleQueryKey, RuleQueryFailure,
    RuleQueryFailureKind, RuleQueryOutcome, SubgraphRuleQuery, SubgraphRuleQueryKey,
    cacheable_rule_query_specs, register_rule_queries,
};

#[cfg(test)]
mod tests;
