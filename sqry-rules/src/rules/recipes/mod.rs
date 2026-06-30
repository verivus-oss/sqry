//! bbnty methodology recipe rules R1..R7.

pub mod r1_variant_from_seed;
pub mod r2_missing_call_check;
pub mod r3_new_feature_coverage;
pub mod r4_post_patch_sibling;
pub mod r5_trust_boundary_audit;
pub mod r6_speculation_trust;
pub mod r7_peer_asymmetry;

use sqry_core::graph::unified::NodeKind;
use sqry_db::planner::{Direction, Predicate, SetOperation, StringPattern};

use crate::backend::SnapshotId;
use crate::ir::{
    EntrypointExtension, PathKind, RelationEdgeKind, RuleEdgeClass, RuleEndpoint, RuleNode,
    RulePlan, RuleSimilarityKind,
};
use crate::rules::ShippedRule;

/// Returns the seven bbnty proof recipe rules in methodology order.
#[must_use]
pub fn bbnty_recipe_rules() -> Vec<ShippedRule> {
    vec![
        r1_variant_from_seed::rule(),
        r2_missing_call_check::rule(),
        r3_new_feature_coverage::rule(),
        r4_post_patch_sibling::rule(),
        r5_trust_boundary_audit::rule(),
        r6_speculation_trust::rule(),
        r7_peer_asymmetry::rule(),
    ]
}

fn scan_name(kind: Option<NodeKind>, name: impl Into<String>) -> RuleNode {
    RuleNode::NodeScan {
        kind,
        visibility: None,
        name_pattern: Some(StringPattern::contains(name)),
    }
}

fn exact_name(kind: Option<NodeKind>, name: impl Into<String>) -> RuleNode {
    RuleNode::NodeScan {
        kind,
        visibility: None,
        name_pattern: Some(StringPattern::exact(name)),
    }
}

fn query(node: RuleNode) -> RuleEndpoint {
    RuleEndpoint::Query(Box::new(node))
}

fn current_snapshot() -> SnapshotId {
    SnapshotId {
        edge_revision: 0,
        metadata_revision: 0,
    }
}

fn previous_snapshot() -> SnapshotId {
    SnapshotId {
        edge_revision: 0,
        metadata_revision: 0,
    }
}

fn path_to_check(source: RuleNode, check: RuleNode, max_depth: u32) -> RuleNode {
    RuleNode::PathQuery {
        from: query(source),
        to: query(check),
        kind: PathKind::Calls,
        max_depth,
        max_paths: Some(32),
    }
}

fn references_to(target: RuleNode) -> RuleNode {
    RuleNode::ReferencesAt {
        target: query(target),
    }
}

fn subgraph_from(seed: RuleNode, edge_classes: Vec<RuleEdgeClass>, max_depth: u32) -> RuleNode {
    RuleNode::SubgraphExtract {
        seeds: query(seed),
        edge_classes,
        direction: Direction::Forward,
        max_depth,
    }
}

fn entry_points_named(pattern: impl Into<String>) -> RuleNode {
    RuleNode::EntryPointUnion {
        extensions: vec![EntrypointExtension::Name(StringPattern::contains(pattern))],
    }
}

fn relation_from(source: RuleNode, kind: RelationEdgeKind) -> RuleNode {
    RuleNode::RelationEdges {
        from: query(source),
        kind,
        with_metadata: true,
    }
}

fn similar_to(
    seed: RuleNode,
    scope: Option<RuleNode>,
    similarity_kind: RuleSimilarityKind,
) -> RuleNode {
    RuleNode::SimilarTo {
        seed: query(seed),
        scope: scope.map(query),
        similarity_kind,
    }
}

fn diff() -> RuleNode {
    RuleNode::CrossSnapshotDiff {
        base: previous_snapshot(),
        head: current_snapshot(),
        include_unchanged: false,
    }
}

fn name_filter(name: impl Into<String>) -> RuleNode {
    RuleNode::Filter {
        predicate: Predicate::MatchesName(StringPattern::contains(name)),
    }
}

fn filtered_scan(kind: Option<NodeKind>, name: impl Into<String>) -> RuleNode {
    let name = name.into();
    RuleNode::Chain {
        steps: vec![scan_name(kind, name.clone()), name_filter(name)],
    }
}

fn difference(left: RuleNode, right: RuleNode) -> RuleNode {
    RuleNode::SetOp {
        op: SetOperation::Difference,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn chain(steps: Vec<RuleNode>) -> RulePlan {
    RulePlan::new(RuleNode::Chain { steps })
}
