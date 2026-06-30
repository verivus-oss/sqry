//! TC9: regression guard against churn to Phases 1-4.
//!
//! The workspace-wide `cargo test` / `clippy` / `fmt` / `sync-versions` gates
//! are enforced by CI and the P5U11 acceptance criteria. At the rule-layer
//! level the durable regression invariants are: (1) the dependency surface
//! stays on the published allowlist (no private graph internals creep in), and
//! (2) running read-only rules does not perturb the Phase 1-4 derived-DB
//! revision tiers.

mod common;

use sqry_rules::{RuleBackend, RuleEngine, SqryDbRuleBackend, shipped_rules};

const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn dependency_surface_stays_on_the_published_allowlist() {
    assert_eq!(
        common::manifest_dependency_names(MANIFEST),
        [
            "anyhow",
            "serde",
            "sqry-core",
            "sqry-db",
            "thiserror",
            "toml"
        ],
        "sqry-rules must keep its P5U01 dependency allowlist"
    );
}

#[test]
fn read_only_rule_execution_does_not_bump_derived_revision_tiers() {
    let fixture = common::two_node_call_fixture();
    let db = common::query_db_for(fixture.snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    let before = backend.snapshot_id();
    for rule in shipped_rules() {
        if common::requires_beside_cache(&rule) {
            continue;
        }
        engine
            .run(&backend, &rule.definition.plan)
            .unwrap_or_else(|error| panic!("{} should run read-only: {error}", rule.id()));
    }
    let after = backend.snapshot_id();

    assert_eq!(
        before, after,
        "read-only rule execution must not mutate edge or metadata revisions"
    );
}
