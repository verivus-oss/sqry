mod common;

use common::*;
use sqry_core::graph::unified::node::NodeKind;

#[test]
fn test_sys_dictionary_extracts_resource_and_variable() {
    let staging = build_graph_from_file("tests/fixtures/sys_dictionary.xml");
    let resource_count = count_nodes_of_kind(&staging, NodeKind::Resource);
    let variable_count = count_nodes_of_kind(&staging, NodeKind::Variable);
    assert!(
        resource_count >= 1,
        "Expected at least 1 Resource node for table, got {resource_count}"
    );
    assert!(
        variable_count >= 1,
        "Expected at least 1 Variable node for field, got {variable_count}"
    );
}

#[test]
fn test_sys_db_object_extracts_resource() {
    let staging = build_graph_from_file("tests/fixtures/sys_db_object.xml");
    let resource_count = count_nodes_of_kind(&staging, NodeKind::Resource);
    assert!(
        resource_count >= 1,
        "Expected at least 1 Resource node for table definition, got {resource_count}"
    );
}
