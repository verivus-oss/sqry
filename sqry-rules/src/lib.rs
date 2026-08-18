//! Declarative rule layer for sqry semantic code search.
//!
//! `sqry-rules` is the public crate boundary for composing declarative analysis
//! rules on top of the stable `sqry-core` graph types and `sqry-db` derived
//! query engine. This initial scaffold intentionally exposes only public
//! dependency surfaces needed by later Phase 5 units.

pub mod backend;
pub mod derived;
pub mod dsl;
pub mod engine;
pub mod error;
pub mod ir;
pub mod rules;
pub mod witness;

pub use backend::{
    CycleClass, RULE_BACKEND_METHODS, RuleBackend, RulePath, RuleReachabilityKey,
    RuleStructuralNeighbor, RuleTopologyKey, SnapshotId, SqryDbRuleBackend, TracePathKey,
};
pub use derived::{
    BesideCachePrimitive, BesideCacheRoute, CacheableRuleQuery, CacheableRuleVariant,
    ComplexityRuleQuery, ComplexityRuleQueryKey, CycleWitnessRuleQuery, CycleWitnessRuleQueryKey,
    EntryPointUnionRuleQuery, EntryPointUnionRuleQueryKey, PathRuleQuery, PathRuleQueryKey,
    ReferencesAtRuleQuery, ReferencesAtRuleQueryKey, RelationEdgesRuleQuery,
    RelationEdgesRuleQueryKey, RuleQueryFailure, RuleQueryFailureKind, RuleQueryOutcome,
    SubgraphRuleQuery, SubgraphRuleQueryKey, beside_cache_route_for, cacheable_rule_query_specs,
    register_rule_queries, requires_unsupported_beside_cache,
};
pub use dsl::{
    RULE_PACK_SCHEMA_VERSION, RuleBuilder, RuleDefinition, RulePack, load_rule_pack_str,
    load_rule_plan_str,
};
pub use engine::{
    NoopCancellationToken, RuleCancellationToken, RuleEngine, RuleEngineConfig, RuleMetricValue,
    RuleOutput, RuleRelationRows, RuleRun,
};
pub use error::{RuleError, RuleResult};
pub use ir::{RuleEdgeClass, RuleNode, RulePlan, TraversalEmit};
pub use rules::{RuleVariant, ShippedRule, shipped_rules};
pub use sqry_core::graph::unified::EdgeClassification;
pub use sqry_db::{DerivedQuery, QueryDb, QueryDbConfig};
pub use witness::{CitationSpan, RuleCitation, RuleStep, RuleWitness};

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../Cargo.toml");
    const PRODUCTION_SOURCES: &[(&str, &str)] = &[
        ("lib.rs", include_str!("lib.rs")),
        ("error.rs", include_str!("error.rs")),
        ("backend/mod.rs", include_str!("backend/mod.rs")),
        (
            "backend/sqry_db_backend.rs",
            include_str!("backend/sqry_db_backend.rs"),
        ),
        ("derived/mod.rs", include_str!("derived/mod.rs")),
        ("derived/cacheable.rs", include_str!("derived/cacheable.rs")),
        (
            "derived/beside_cache.rs",
            include_str!("derived/beside_cache.rs"),
        ),
        ("ir/mod.rs", include_str!("ir/mod.rs")),
        ("ir/node.rs", include_str!("ir/node.rs")),
        ("ir/plan.rs", include_str!("ir/plan.rs")),
        ("witness/mod.rs", include_str!("witness/mod.rs")),
        ("witness/step.rs", include_str!("witness/step.rs")),
        ("witness/citation.rs", include_str!("witness/citation.rs")),
        ("dsl/mod.rs", include_str!("dsl/mod.rs")),
        ("dsl/builder.rs", include_str!("dsl/builder.rs")),
        ("dsl/schema.rs", include_str!("dsl/schema.rs")),
        ("dsl/toml_loader.rs", include_str!("dsl/toml_loader.rs")),
        ("engine/mod.rs", include_str!("engine/mod.rs")),
        ("engine/dispatcher.rs", include_str!("engine/dispatcher.rs")),
        ("rules/mod.rs", include_str!("rules/mod.rs")),
        ("rules/recipes/mod.rs", include_str!("rules/recipes/mod.rs")),
        (
            "rules/recipes/r1_variant_from_seed.rs",
            include_str!("rules/recipes/r1_variant_from_seed.rs"),
        ),
        (
            "rules/recipes/r2_missing_call_check.rs",
            include_str!("rules/recipes/r2_missing_call_check.rs"),
        ),
        (
            "rules/recipes/r3_new_feature_coverage.rs",
            include_str!("rules/recipes/r3_new_feature_coverage.rs"),
        ),
        (
            "rules/recipes/r4_post_patch_sibling.rs",
            include_str!("rules/recipes/r4_post_patch_sibling.rs"),
        ),
        (
            "rules/recipes/r5_trust_boundary_audit.rs",
            include_str!("rules/recipes/r5_trust_boundary_audit.rs"),
        ),
        (
            "rules/recipes/r6_speculation_trust.rs",
            include_str!("rules/recipes/r6_speculation_trust.rs"),
        ),
        (
            "rules/recipes/r7_peer_asymmetry.rs",
            include_str!("rules/recipes/r7_peer_asymmetry.rs"),
        ),
        ("rules/intake/mod.rs", include_str!("rules/intake/mod.rs")),
    ];

    #[test]
    fn public_surface_reexports_only_public_analysis_types() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<EdgeClassification>();
        assert_send_sync::<QueryDb>();
        assert_send_sync::<QueryDbConfig>();
        assert_send_sync::<RuleNode>();
        assert_send_sync::<RulePlan>();
    }

    #[test]
    fn crate_manifest_dependency_allowlist_is_stable() {
        let dependency_names = dependency_names(MANIFEST);

        assert_eq!(
            dependency_names,
            [
                "anyhow",
                "serde",
                "sqry-core",
                "sqry-db",
                "thiserror",
                "toml"
            ],
            "sqry-rules may only depend on the P5U01 allowlist"
        );
    }

    #[test]
    fn scaffold_does_not_import_private_core_modules() {
        let private_surface_markers = [
            "sqry_core::graph::unified::arena",
            "sqry_core::graph::unified::edges",
            "sqry_core::graph::unified::strings",
            "sqry_core::graph::unified::txn",
        ];

        for (path, source) in PRODUCTION_SOURCES {
            let production_source = source
                .split("#[cfg(test)]")
                .next()
                .expect("source always has a production section before tests");
            for marker in private_surface_markers {
                assert!(
                    !production_source.contains(marker),
                    "sqry-rules must use public crate exports; found `{marker}` in {path}"
                );
            }
        }
    }

    #[test]
    fn edge_kind_chokepoint_stays_inside_default_backend_adapter() {
        for (path, source) in PRODUCTION_SOURCES {
            if *path == "backend/sqry_db_backend.rs" {
                continue;
            }
            let production_source = source
                .split("#[cfg(test)]")
                .next()
                .expect("source always has a production section before tests");
            let forbidden_edge_kind_markers =
                [" EdgeKind", "::EdgeKind", "<EdgeKind", "Vec<EdgeKind>"];
            for marker in forbidden_edge_kind_markers {
                assert!(
                    !production_source.contains(marker),
                    "only backend/sqry_db_backend.rs may name EdgeKind directly; found `{marker}` in {path}"
                );
            }
        }
    }

    #[test]
    fn rule_backend_trait_does_not_expose_storage_query_keys() {
        let source = include_str!("backend/mod.rs");
        let trait_source = source
            .split("pub trait RuleBackend")
            .nth(1)
            .expect("RuleBackend trait is defined in backend/mod.rs")
            .split("/// Returns a binding plane")
            .next()
            .expect("RuleBackend trait appears before binding_plane helper");
        let forbidden_storage_markers = [
            " ReachabilityKey",
            " SccKey",
            " CondensationKey",
            "::ReachabilityKey",
            "::SccKey",
            "::CondensationKey",
            " EdgeKind",
            "::EdgeKind",
            "NodeKind",
            "StringId",
        ];

        for marker in forbidden_storage_markers {
            assert!(
                !trait_source.contains(marker),
                "RuleBackend trait must expose rule-level facade types, not storage marker `{marker}`"
            );
        }
    }

    fn dependency_names(manifest: &str) -> Vec<&str> {
        let mut in_dependencies = false;
        let mut dependency_names = Vec::new();

        for line in manifest.lines() {
            let trimmed = line.trim();
            if trimmed == "[dependencies]" {
                in_dependencies = true;
                continue;
            }

            if in_dependencies && trimmed.starts_with('[') {
                break;
            }

            if in_dependencies
                && !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && let Some((name, _value)) = trimmed.split_once('=')
            {
                let package_name = name
                    .trim()
                    .split_once('.')
                    .map_or_else(|| name.trim(), |(package_name, _field)| package_name.trim());
                dependency_names.push(package_name);
            }
        }

        dependency_names
    }
}
