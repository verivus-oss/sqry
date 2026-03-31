mod common;

use common::*;
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::node::NodeKind;

#[test]
fn test_sys_script_extracts_function() {
    let staging = build_graph_from_file("tests/fixtures/sys_script.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 1,
        "Expected at least 1 Function node from Business Rule, got {fn_count}"
    );
}

#[test]
fn test_sys_script_include_extracts_class_and_methods() {
    let staging = build_graph_from_file("tests/fixtures/sys_script_include.xml");
    assert!(
        staging.stats().nodes_staged >= 3,
        "Expected at least 3 nodes (class + 2 methods), got {}",
        staging.stats().nodes_staged
    );
}

#[test]
fn test_sys_script_client_extracts_function() {
    let staging = build_graph_from_file("tests/fixtures/sys_script_client.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 1,
        "Expected at least 1 Function node from Client Script"
    );
}

#[test]
fn test_sys_ui_action_dual_scripts() {
    let staging = build_graph_from_file("tests/fixtures/sys_ui_action.xml");
    // UI Action has both server-side <script> and client-side <client_script>
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 2,
        "Expected at least 2 Function nodes (server + client), got {fn_count}"
    );
}

#[test]
fn test_sys_ui_policy_dual_scripts() {
    let staging = build_graph_from_file("tests/fixtures/sys_ui_policy.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 2,
        "Expected at least 2 Function nodes (true + false), got {fn_count}"
    );
}

#[test]
fn test_sys_ws_operation_extracts_function() {
    let staging = build_graph_from_file("tests/fixtures/sys_ws_operation.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 1,
        "Expected at least 1 Function node from Scripted REST, got {fn_count}"
    );
}

#[test]
fn test_sys_processor_extracts_function() {
    let staging = build_graph_from_file("tests/fixtures/sys_processor.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 1,
        "Expected at least 1 Function node from Processor, got {fn_count}"
    );
}

#[test]
fn test_empty_script_no_error() {
    let staging = build_graph_from_file("tests/fixtures/empty_script.xml");
    // Empty script element should produce no JS nodes (maybe just module)
    assert!(
        staging.stats().nodes_staged <= 1,
        "Empty script should produce 0-1 nodes"
    );
}

#[test]
fn test_non_servicenow_xml_empty() {
    let staging = build_graph_from_file("tests/fixtures/non_servicenow.xml");
    assert_eq!(
        staging.stats().nodes_staged,
        0,
        "Non-ServiceNow XML should produce empty graph"
    );
}

#[test]
fn test_multi_record_schema() {
    let staging = build_graph_from_file("tests/fixtures/multi_record.xml");
    let resource_count = count_nodes_of_kind(&staging, NodeKind::Resource);
    let variable_count = count_nodes_of_kind(&staging, NodeKind::Variable);
    // 2 sys_dictionary records for same table "incident" with different fields
    assert!(
        resource_count >= 1,
        "Expected Resource node(s) for incident table"
    );
    assert!(
        variable_count >= 2,
        "Expected at least 2 Variable nodes (priority + severity), got {variable_count}"
    );
}

#[test]
fn test_multi_record_scripts() {
    let staging = build_graph_from_file("tests/fixtures/multi_script.xml");
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert!(
        fn_count >= 2,
        "Expected at least 2 Function nodes from 2 Business Rules, got {fn_count}"
    );
}

#[test]
fn test_oversized_script_skipped() {
    // Generate an XML file with a script >1MB
    let big_script = "x".repeat(1_100_000);
    let xml = format!(
        r#"<?xml version="1.0"?><record_update table="sys_script"><sys_script><name>Big</name><script><![CDATA[{}]]></script></sys_script></record_update>"#,
        big_script,
    );
    let staging = build_graph_from_xml(&xml);
    // The oversized script should be skipped, but the file itself processes without error.
    // No Function nodes should be extracted from the oversized script.
    let fn_count = count_nodes_of_kind(&staging, NodeKind::Function);
    assert_eq!(fn_count, 0, "Oversized script should be skipped");
}

#[test]
fn test_malformed_xml_no_panic() {
    let staging = build_graph_from_file("tests/fixtures/malformed.xml");
    // Malformed XML should return empty graph without panicking
    assert_eq!(staging.stats().nodes_staged, 0);
}

#[test]
fn test_non_utf8_empty_graph() {
    // Non-UTF-8 bytes should return empty graph
    let content: &[u8] = &[0xFF, 0xFE, 0x00, 0x3C]; // UTF-16 BOM
    let plugin = sqry_lang_servicenow_xml::ServiceNowXmlPlugin::new();
    use sqry_core::plugin::LanguagePlugin;
    let tree = plugin.parse_ast(content).expect("parse_ast");
    let mut staging = StagingGraph::new();
    let builder = sqry_lang_servicenow_xml::ServiceNowXmlGraphBuilder;
    let result = builder.build_graph(
        &tree,
        content,
        std::path::Path::new("test.xml"),
        &mut staging,
    );
    assert!(result.is_ok());
    assert!(staging.is_empty());
}
