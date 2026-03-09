mod common;

use common::{build_staging_graph_from_fixture, has_edge_between, has_node};
use sqry_core::graph::unified::EdgeKind;

#[test]
fn test_pulumi_yaml_graph_edges() {
    let staging = build_staging_graph_from_fixture("Pulumi.yaml");

    assert!(has_node(&staging, "resources.net"));
    assert!(has_node(&staging, "resources.app"));
    assert!(has_node(&staging, "outputs.appId"));
    assert!(has_node(&staging, "config.environment"));
    assert!(has_node(&staging, "variables.owner"));
    assert!(has_node(&staging, "package.aws"));
    assert!(has_node(&staging, "type.aws:ec2/vpc:Vpc"));

    assert!(has_edge_between(
        &staging,
        "<module>",
        "resources.net",
        |kind| { matches!(kind, EdgeKind::Defines) }
    ));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "outputs.appId",
        |kind| { matches!(kind, EdgeKind::Exports { .. }) }
    ));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "package.aws",
        |kind| { matches!(kind, EdgeKind::Imports { .. }) }
    ));
    assert!(has_edge_between(
        &staging,
        "resources.net",
        "type.aws:ec2/vpc:Vpc",
        |kind| matches!(kind, EdgeKind::TypeOf { .. })
    ));
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "config.environment",
        |kind| matches!(kind, EdgeKind::References)
    ));
    assert!(has_edge_between(
        &staging,
        "outputs.appId",
        "resources.app",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_pulumi_json_graph_edges() {
    let staging = build_staging_graph_from_fixture("Pulumi.dev.json");

    assert!(has_node(&staging, "resources.rg"));
    assert!(has_node(&staging, "resources.storage"));
    assert!(has_node(&staging, "package.azure-native"));
    assert!(has_node(
        &staging,
        "type.azure-native:resources:ResourceGroup"
    ));

    assert!(has_edge_between(
        &staging,
        "<module>",
        "package.azure-native",
        |kind| { matches!(kind, EdgeKind::Imports { .. }) }
    ));
    assert!(has_edge_between(
        &staging,
        "resources.storage",
        "resources.rg",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_pulumi_stack_yaml_graph_edges() {
    let staging = build_staging_graph_from_fixture("Pulumi.dev.yaml");

    assert!(has_node(&staging, "resources.cache"));
    assert!(has_node(&staging, "outputs.cacheId"));

    assert!(has_edge_between(
        &staging,
        "outputs.cacheId",
        "resources.cache",
        |kind| matches!(kind, EdgeKind::References)
    ));
}
