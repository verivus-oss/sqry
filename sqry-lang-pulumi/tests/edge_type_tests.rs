//! Isolated edge-type tests for the Pulumi graph builder.
//!
//! Each test uses inline YAML/JSON source to verify a single feature area,
//! unlike `graph_builder_tests.rs` which uses fixture files that exercise
//! everything simultaneously.

mod common;

use common::{
    build_staging_from_json, build_staging_from_yaml, count_edges, count_nodes,
    count_nodes_with_prefix, count_reference_edges_between, has_edge_between, has_node,
};
use sqry_core::graph::unified::EdgeKind;

// ===========================================================================
// 1. Package Imports
// ===========================================================================

#[test]
fn test_import_edge_created_for_package() {
    let staging =
        build_staging_from_yaml("name: test\nruntime: yaml\npackage: aws\nresources: {}\n");
    assert!(has_node(&staging, "package.aws"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "package.aws",
        |kind| matches!(kind, EdgeKind::Imports { .. })
    ));
}

#[test]
fn test_no_package_produces_no_import_edges() {
    let staging = build_staging_from_yaml("name: test\nruntime: yaml\nresources: {}\n");
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::Imports { .. })),
        0
    );
}

#[test]
fn test_json_import_edge() {
    let staging = build_staging_from_json(
        r#"{"name":"test","runtime":"yaml","package":"gcp","resources":{}}"#,
    );
    assert!(has_node(&staging, "package.gcp"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "package.gcp",
        |kind| matches!(kind, EdgeKind::Imports { .. })
    ));
}

// ===========================================================================
// 2. Resource Definitions
// ===========================================================================

#[test]
fn test_resource_node_and_defines_edge() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  myVpc:\n    type: aws:ec2/vpc:Vpc\n",
    );
    assert!(has_node(&staging, "resources.myVpc"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "resources.myVpc",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

#[test]
fn test_multiple_resources() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: aws:s3:Bucket\n  b:\n    type: aws:s3:Bucket\n",
    );
    assert!(has_node(&staging, "resources.a"));
    assert!(has_node(&staging, "resources.b"));
}

#[test]
fn test_resource_without_type() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  bare:\n    properties:\n      foo: bar\n",
    );
    assert!(has_node(&staging, "resources.bare"));
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. })),
        0,
        "Resource without type should produce no TypeOf edges"
    );
}

#[test]
fn test_json_resource_definitions() {
    let staging = build_staging_from_json(
        r#"{"name":"test","runtime":"yaml","resources":{"db":{"type":"aws:rds:Instance"}}}"#,
    );
    assert!(has_node(&staging, "resources.db"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "resources.db",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

// ===========================================================================
// 3. Resource TypeOf
// ===========================================================================

#[test]
fn test_typeof_edge_to_type_node() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: aws:ec2/vpc:Vpc\n",
    );
    assert!(has_node(&staging, "type.aws:ec2/vpc:Vpc"));
    assert!(has_edge_between(
        &staging,
        "resources.net",
        "type.aws:ec2/vpc:Vpc",
        |kind| matches!(kind, EdgeKind::TypeOf { .. })
    ));
}

#[test]
fn test_multiple_type_nodes() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: aws:s3:Bucket\n  b:\n    type: aws:ec2:Instance\n",
    );
    assert!(has_node(&staging, "type.aws:s3:Bucket"));
    assert!(has_node(&staging, "type.aws:ec2:Instance"));
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. })),
        2
    );
}

#[test]
fn test_no_type_produces_no_typeof() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  bare:\n    properties:\n      x: 1\n",
    );
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. })),
        0
    );
}

// ===========================================================================
// 4. Config Variables
// ===========================================================================

#[test]
fn test_config_variable_nodes_and_defines() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nconfig:\n  env: prod\n  region: us-east-1\n",
    );
    assert!(has_node(&staging, "config.env"));
    assert!(has_node(&staging, "config.region"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "config.env",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "config.region",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

#[test]
fn test_no_config_produces_no_config_nodes() {
    let staging = build_staging_from_yaml("name: test\nruntime: yaml\nresources: {}\n");
    assert_eq!(
        count_nodes_with_prefix(&staging, "config."),
        0,
        "No config section should produce zero config.* nodes"
    );
}

#[test]
fn test_json_config_variables() {
    let staging = build_staging_from_json(
        r#"{"name":"test","runtime":"yaml","config":{"zone":"us-west-2"}}"#,
    );
    assert!(has_node(&staging, "config.zone"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "config.zone",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

// ===========================================================================
// 5. Variables Section
// ===========================================================================

#[test]
fn test_variables_nodes_and_defines() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nvariables:\n  owner: team\n  project: sqry\n",
    );
    assert!(has_node(&staging, "variables.owner"));
    assert!(has_node(&staging, "variables.project"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "variables.owner",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

#[test]
fn test_no_variables_produces_no_variable_nodes() {
    let staging = build_staging_from_yaml("name: test\nruntime: yaml\nresources: {}\n");
    assert_eq!(
        count_nodes_with_prefix(&staging, "variables."),
        0,
        "No variables section should produce zero variables.* nodes"
    );
}

// ===========================================================================
// 6. Output Exports
// ===========================================================================

#[test]
fn test_output_defines_and_export_edges() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\noutputs:\n  appId:\n    value: static\n",
    );
    assert!(has_node(&staging, "outputs.appId"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "outputs.appId",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "outputs.appId",
        |kind| matches!(kind, EdgeKind::Exports { .. })
    ));
}

#[test]
fn test_multiple_outputs() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\noutputs:\n  a:\n    value: x\n  b:\n    value: y\n",
    );
    assert!(has_node(&staging, "outputs.a"));
    assert!(has_node(&staging, "outputs.b"));
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::Exports { .. })),
        2
    );
}

#[test]
fn test_output_value_interpolation_creates_reference() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  app:\n    type: aws:ec2:Instance\noutputs:\n  appId:\n    value: ${resources.app.id}\n",
    );
    assert!(has_edge_between(
        &staging,
        "outputs.appId",
        "resources.app",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_json_output_exports() {
    let staging = build_staging_from_json(
        r#"{"name":"test","runtime":"yaml","outputs":{"out1":{"value":"static"}}}"#,
    );
    assert!(has_node(&staging, "outputs.out1"));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "outputs.out1",
        |kind| matches!(kind, EdgeKind::Exports { .. })
    ));
}

// ===========================================================================
// 7. DependsOn References
// ===========================================================================

#[test]
fn test_depends_on_bare_name() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: aws:s3:Bucket\n  b:\n    type: aws:s3:Object\n    dependsOn:\n      - a\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.b",
        "resources.a",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_depends_on_prefixed_name() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: aws:ec2/vpc:Vpc\n  app:\n    type: aws:ec2:Instance\n    dependsOn:\n      - resources.net\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_depends_on_multiple_entries() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: t\n  b:\n    type: t\n  c:\n    type: t\n    dependsOn:\n      - a\n      - b\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.c",
        "resources.a",
        |kind| matches!(kind, EdgeKind::References)
    ));
    assert!(has_edge_between(
        &staging,
        "resources.c",
        "resources.b",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_depends_on_scalar_value() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: t\n  app:\n    type: t\n    dependsOn: net\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_depends_on_empty_list() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: t\n    dependsOn: []\n",
    );
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::References)),
        0,
        "Empty dependsOn should produce no Reference edges"
    );
}

// ===========================================================================
// 8. Interpolation References
// ===========================================================================

#[test]
fn test_interpolation_resource_ref() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: aws:ec2/vpc:Vpc\n  app:\n    type: aws:ec2:Instance\n    properties:\n      subnetId: ${resources.net.id}\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_interpolation_config_ref() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  app:\n    type: aws:ec2:Instance\n    properties:\n      env: ${config.environment}\nconfig:\n  environment: prod\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "config.environment",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_interpolation_escaped_skipped() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  app:\n    type: t\n    properties:\n      literal: $${resources.skip.id}\n",
    );
    assert!(
        !has_node(&staging, "resources.skip"),
        "Escaped interpolation should not create a reference"
    );
}

#[test]
fn test_interpolation_multiple_in_one_property() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: t\n  app:\n    type: t\n    properties:\n      combined: ${resources.net.id}-${config.env}\nconfig:\n  env: prod\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "config.env",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_interpolation_nested_map_traversal() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  db:\n    type: t\n  app:\n    type: t\n    properties:\n      outer:\n        inner: ${resources.db.endpoint}\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.db",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

#[test]
fn test_interpolation_sequence_traversal() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: t\n  app:\n    type: t\n    properties:\n      tags:\n        - ${resources.net.id}\n",
    );
    assert!(has_edge_between(
        &staging,
        "resources.app",
        "resources.net",
        |kind| matches!(kind, EdgeKind::References)
    ));
}

// ===========================================================================
// 9. Reference Deduplication
// ===========================================================================

#[test]
fn test_duplicate_depends_on_produces_single_edge() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: t\n  b:\n    type: t\n    dependsOn:\n      - a\n      - a\n",
    );
    assert_eq!(
        count_reference_edges_between(&staging, "resources.b", "resources.a"),
        1,
        "Duplicate dependsOn entries should deduplicate to 1 edge"
    );
}

#[test]
fn test_duplicate_interpolation_produces_single_edge() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  net:\n    type: t\n  app:\n    type: t\n    properties:\n      a: ${resources.net.id}\n      b: ${resources.net.name}\n",
    );
    assert_eq!(
        count_reference_edges_between(&staging, "resources.app", "resources.net"),
        1,
        "Multiple interpolations to same resource should deduplicate to 1 edge"
    );
}

#[test]
fn test_cross_source_dedup_depends_on_and_interpolation() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  a:\n    type: t\n  b:\n    type: t\n    dependsOn:\n      - a\n    properties:\n      ref: ${resources.a.id}\n",
    );
    assert_eq!(
        count_reference_edges_between(&staging, "resources.b", "resources.a"),
        1,
        "Same target via dependsOn and interpolation should deduplicate to 1 edge"
    );
}

#[test]
fn test_duplicate_config_interpolation_produces_single_edge() {
    let staging = build_staging_from_yaml(
        "name: test\nruntime: yaml\nresources:\n  app:\n    type: t\n    properties:\n      a: ${config.env}\n      b: ${config.env}\nconfig:\n  env: prod\n",
    );
    assert_eq!(
        count_reference_edges_between(&staging, "resources.app", "config.env"),
        1,
        "Duplicate config interpolations should deduplicate to 1 edge"
    );
}

// ===========================================================================
// 10. Empty / Minimal Stacks
// ===========================================================================

#[test]
fn test_name_only_yaml() {
    let staging = build_staging_from_yaml("name: minimal\nruntime: yaml\n");
    assert!(has_node(&staging, "<module>"));
    assert_eq!(count_edges(&staging, |_| true), 0);
}

#[test]
fn test_empty_resources_map() {
    let staging = build_staging_from_yaml("name: test\nruntime: yaml\nresources: {}\n");
    assert!(has_node(&staging, "<module>"));
    assert_eq!(
        count_edges(&staging, |kind| matches!(kind, EdgeKind::Defines)),
        0,
        "Empty resources map should produce no Defines edges"
    );
}

#[test]
fn test_empty_content() {
    let staging = build_staging_from_yaml("");
    assert_eq!(
        count_nodes(&staging),
        0,
        "Empty content should produce no nodes"
    );
}
