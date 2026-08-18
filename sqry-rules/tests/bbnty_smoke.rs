use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqry_core::graph::unified::CodeGraph;
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_rules::derived::BesideCachePrimitive;
use sqry_rules::ir::{RuleEndpoint, RuleNode};
use sqry_rules::rules::{RuleVariant, ShippedRule, recipes};
use sqry_rules::{
    RuleEngine, RuleOutput, RuleStep, SqryDbRuleBackend, beside_cache_route_for, load_rule_plan_str,
};

const BBNTY_PROVENANCE_ROOT_ENV: &str = "SQRY_BBNTY_PROVENANCE_ROOT";
const BBNTY_PUBLIC_ROOT_ENV: &str = "SQRY_BBNTY_PUBLIC_ROOT";
const METHODOLOGY: &str = "research/sqry-vulnerability-hunting-methodology.md";
const TIKV_METHODOLOGY: &str = "tikv-analysis-methodology.md";

#[test]
fn recipe_suite_declares_all_seven_bbnty_recipes_in_order() {
    let recipes = recipes::bbnty_recipe_rules();

    assert_eq!(
        recipes.iter().map(ShippedRule::id).collect::<Vec<_>>(),
        [
            "bbnty.pr_r1.variant_from_seed",
            "bbnty.pr_r2.missing_call_check",
            "bbnty.pr_r3.new_feature_coverage",
            "bbnty.pr_r4.post_patch_sibling",
            "bbnty.pr_r5.trust_boundary_audit",
            "bbnty.pr_r6.speculation_trust",
            "bbnty.pr_r7.peer_asymmetry",
        ]
    );
}

#[test]
fn recipe_and_intake_suite_cover_every_fr2_extension_variant() {
    let mut covered = BTreeSet::new();
    for rule in all_rules() {
        collect_variants(rule.definition.plan.root(), &mut covered);
        covered.extend(rule.variants.iter().copied());
    }

    for required in [
        RuleVariant::PathQuery,
        RuleVariant::SubgraphExtract,
        RuleVariant::RelationEdges,
        RuleVariant::CycleWitness,
        RuleVariant::ReferencesAt,
        RuleVariant::ComplexityAggregate,
        RuleVariant::CrossSnapshotDiff,
        RuleVariant::EntryPointUnion,
        RuleVariant::SimilarTo,
    ] {
        assert!(
            covered.contains(&required),
            "FR2 variant {required:?} is not covered by P5U08 rules"
        );
    }
}

#[test]
fn beside_cache_recipes_route_through_registered_beside_cache_primitives() {
    let recipes = recipes::bbnty_recipe_rules();
    let pr_r3 = find_rule(&recipes, "bbnty.pr_r3.new_feature_coverage");
    let pr_r4 = find_rule(&recipes, "bbnty.pr_r4.post_patch_sibling");
    let pr_r7 = find_rule(&recipes, "bbnty.pr_r7.peer_asymmetry");

    assert_route(pr_r3, BesideCachePrimitive::ComparativeDiff);
    assert_route(pr_r4, BesideCachePrimitive::ComparativeDiff);
    assert_route(pr_r4, BesideCachePrimitive::FindSimilar);
    assert_route(pr_r7, BesideCachePrimitive::FindDuplicates);
}

#[test]
fn trust_boundary_toml_fixture_round_trips_to_rust_dsl_ir() {
    let loaded = load_rule_plan_str(include_str!("fixtures/trust-boundary.toml"))
        .expect("trust-boundary TOML fixture must load");
    let rust_plan = recipes::r5_trust_boundary_audit::rule().definition.plan;

    assert_eq!(loaded, rust_plan);
}

#[test]
fn seed_findings_and_methodology_sources_are_present_for_hand_review() {
    let Some(provenance_root) = env_path(BBNTY_PROVENANCE_ROOT_ENV) else {
        eprintln!("skipping bbnty provenance check: {BBNTY_PROVENANCE_ROOT_ENV} is not set");
        return;
    };
    let Some(public_root) = env_path(BBNTY_PUBLIC_ROOT_ENV) else {
        eprintln!("skipping bbnty provenance check: {BBNTY_PUBLIC_ROOT_ENV} is not set");
        return;
    };

    assert!(
        provenance_root.join(METHODOLOGY).exists(),
        "bbnty methodology source is required for P5U08 provenance"
    );
    assert!(
        provenance_root.join(TIKV_METHODOLOGY).exists(),
        "TiKV methodology source is required for intake provenance"
    );
    assert!(
        public_root.exists(),
        "public bbnty checkout should exist even when research files are absent"
    );

    for rule in recipes::bbnty_recipe_rules() {
        let Some(seed_finding) = rule.seed_finding else {
            panic!("recipe {} must name a hand-review seed finding", rule.id());
        };
        let finding_path = provenance_root.join("findings").join(seed_finding);
        assert!(
            finding_path.exists(),
            "seed finding for {} is missing: {}",
            rule.id(),
            finding_path.display()
        );
    }
}

fn env_path(variable_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os(variable_name).map(PathBuf::from)?;
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping bbnty provenance check: {variable_name} path is absent");
        None
    }
}

#[test]
fn non_beside_cache_recipes_emit_rule_witnesses_on_empty_graph() {
    let graph = CodeGraph::new();
    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    for rule in recipes::bbnty_recipe_rules()
        .into_iter()
        .filter(|rule| !rule.requires_beside_cache)
    {
        let run = engine
            .run(&backend, &rule.definition.plan)
            .unwrap_or_else(|error| panic!("{} should run on an empty graph: {error}", rule.id()));
        assert!(
            run.witness
                .steps
                .iter()
                .any(|step| matches!(step, RuleStep::RuleFired { .. })),
            "{} did not emit a RuleFired witness step",
            rule.id()
        );
    }
}

#[test]
fn smoke_budget_stays_within_two_x_local_hand_composition_floor() {
    let graph = CodeGraph::new();
    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    for rule in all_rules() {
        let started = Instant::now();
        if rule.requires_beside_cache {
            assert!(
                has_beside_cache_route(rule.definition.plan.root()),
                "{} is marked beside-cache but has no beside-cache route",
                rule.id()
            );
        } else {
            let run = engine
                .run(&backend, &rule.definition.plan)
                .unwrap_or_else(|error| {
                    panic!("{} should execute for smoke budget: {error}", rule.id())
                });
            assert_rule_output_shape(&run.output);
        }
        let elapsed = started.elapsed();

        let baseline = Duration::from_millis(rule.baseline_ms_floor);
        let allowed = baseline.saturating_mul(2).max(Duration::from_millis(20));
        assert!(
            elapsed <= allowed,
            "{} exceeded smoke wall-clock budget: {:?} > {:?}",
            rule.id(),
            elapsed,
            allowed
        );
    }
}

fn assert_rule_output_shape(output: &RuleOutput) {
    match output {
        RuleOutput::Nodes(_)
        | RuleOutput::Paths(_)
        | RuleOutput::Subgraph { .. }
        | RuleOutput::Relations(_)
        | RuleOutput::Cycles(_)
        | RuleOutput::References(_)
        | RuleOutput::Metrics(_)
        | RuleOutput::DiffEntries(_)
        | RuleOutput::EntryPoints(_)
        | RuleOutput::SimilarityMatches(_) => {}
        RuleOutput::Sequence(outputs) => {
            assert!(
                !outputs.is_empty(),
                "sequence output must retain step outputs"
            );
            for output in outputs {
                assert_rule_output_shape(output);
            }
        }
    }
}

fn all_rules() -> Vec<ShippedRule> {
    // Single source of truth: the shipped set (recipes + intake + security), so
    // the smoke suite never drifts from what `shipped_rules()` actually ships.
    sqry_rules::rules::shipped_rules()
}

fn find_rule<'a>(rules: &'a [ShippedRule], id: &str) -> &'a ShippedRule {
    rules
        .iter()
        .find(|rule| rule.id() == id)
        .unwrap_or_else(|| panic!("missing rule {id}"))
}

fn assert_route(rule: &ShippedRule, primitive: BesideCachePrimitive) {
    assert!(
        contains_beside_route(rule.definition.plan.root(), primitive),
        "{} does not route through {primitive:?}",
        rule.id()
    );
}

fn contains_beside_route(node: &RuleNode, primitive: BesideCachePrimitive) -> bool {
    if beside_cache_route_for(node).is_some_and(|route| route.primitive == primitive) {
        return true;
    }
    child_nodes(node)
        .iter()
        .any(|child| contains_beside_route(child, primitive))
}

fn has_beside_cache_route(node: &RuleNode) -> bool {
    beside_cache_route_for(node).is_some()
        || child_nodes(node)
            .iter()
            .any(|child| has_beside_cache_route(child))
}

fn collect_variants(node: &RuleNode, variants: &mut BTreeSet<RuleVariant>) {
    variants.insert(variant_for(node));
    for child in child_nodes(node) {
        collect_variants(child, variants);
    }
}

fn variant_for(node: &RuleNode) -> RuleVariant {
    match node {
        RuleNode::NodeScan { .. } => RuleVariant::NodeScan,
        RuleNode::EdgeTraversal { .. } => RuleVariant::EdgeTraversal,
        RuleNode::Filter { .. } => RuleVariant::Filter,
        RuleNode::SetOp { .. } => RuleVariant::SetOp,
        RuleNode::Chain { .. } => RuleVariant::Chain,
        RuleNode::PathQuery { .. } => RuleVariant::PathQuery,
        RuleNode::SubgraphExtract { .. } => RuleVariant::SubgraphExtract,
        RuleNode::RelationEdges { .. } => RuleVariant::RelationEdges,
        RuleNode::CycleWitness { .. } => RuleVariant::CycleWitness,
        RuleNode::ReferencesAt { .. } => RuleVariant::ReferencesAt,
        RuleNode::ComplexityAggregate { .. } => RuleVariant::ComplexityAggregate,
        RuleNode::CrossSnapshotDiff { .. } => RuleVariant::CrossSnapshotDiff,
        RuleNode::EntryPointUnion { .. } => RuleVariant::EntryPointUnion,
        RuleNode::SimilarTo { .. } => RuleVariant::SimilarTo,
    }
}

fn child_nodes(node: &RuleNode) -> Vec<&RuleNode> {
    match node {
        RuleNode::SetOp { left, right, .. } => vec![left, right],
        RuleNode::Chain { steps } => steps.iter().collect(),
        RuleNode::PathQuery {
            from, to, avoid, ..
        } => {
            let mut children = endpoint_children([from, to]);
            if let Some(avoid) = avoid {
                children.extend(endpoint_children([avoid]));
            }
            children
        }
        RuleNode::SubgraphExtract { seeds, .. } => endpoint_children([seeds]),
        RuleNode::RelationEdges { from, .. } => endpoint_children([from]),
        RuleNode::ReferencesAt { target } => endpoint_children([target]),
        RuleNode::SimilarTo { seed, scope, .. } => {
            let mut children = endpoint_children([seed]);
            if let Some(scope) = scope {
                children.extend(endpoint_children([scope]));
            }
            children
        }
        RuleNode::NodeScan { .. }
        | RuleNode::EdgeTraversal { .. }
        | RuleNode::Filter { .. }
        | RuleNode::CycleWitness { .. }
        | RuleNode::ComplexityAggregate { .. }
        | RuleNode::CrossSnapshotDiff { .. }
        | RuleNode::EntryPointUnion { .. } => Vec::new(),
    }
}

fn endpoint_children<const N: usize>(endpoints: [&RuleEndpoint; N]) -> Vec<&RuleNode> {
    endpoints
        .into_iter()
        .filter_map(|endpoint| match endpoint {
            RuleEndpoint::Query(query) => Some(query.as_ref()),
            RuleEndpoint::Nodes(_) => None,
        })
        .collect()
}
