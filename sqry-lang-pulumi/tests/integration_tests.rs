mod common;

use common::{build_staging_graph_from_fixture, count_edges, has_node};
use sqry_core::graph::unified::EdgeKind;

#[test]
fn test_pulumi_stack_isolation() {
    let base = build_staging_graph_from_fixture("Pulumi.yaml");
    let stack = build_staging_graph_from_fixture("Pulumi.dev.yaml");

    assert!(has_node(&base, "resources.net"));
    assert!(has_node(&base, "resources.app"));
    assert!(!has_node(&base, "resources.cache"));

    assert!(has_node(&stack, "resources.cache"));
    assert!(!has_node(&stack, "resources.net"));
    assert!(!has_node(&stack, "resources.app"));
}

#[test]
fn test_pulumi_edge_counts() {
    let staging = build_staging_graph_from_fixture("Pulumi.yaml");

    let import_count = count_edges(&staging, |kind| matches!(kind, EdgeKind::Imports { .. }));
    let export_count = count_edges(&staging, |kind| matches!(kind, EdgeKind::Exports { .. }));
    let defines_count = count_edges(&staging, |kind| matches!(kind, EdgeKind::Defines));

    assert_eq!(import_count, 1, "Expected one package import edge");
    assert_eq!(export_count, 1, "Expected one output export edge");
    assert!(
        defines_count >= 4,
        "Expected defines edges for module entries"
    );
}
