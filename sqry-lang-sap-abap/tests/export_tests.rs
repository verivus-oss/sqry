//! Tests for SAP ABAP export edge creation.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sap_abap::SapAbapPlugin;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_graph_from_source(source: &[u8]) -> StagingGraph {
    let plugin = SapAbapPlugin::default();
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.abap");
    fs::write(&file, source).expect("write test source");
    let tree = plugin.parse_ast(source).expect("parse source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

fn has_export_edge(staging: &StagingGraph, exported_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if target_name == Some(exported_name) {
                return true;
            }
        }
    }
    false
}

// ===== Export Edge Tests =====

#[test]
fn test_function_modules_exported() {
    let content = b"\
FUNCTION z_get_customer.
  SELECT * FROM zcustomers INTO TABLE @DATA(lt_result).
ENDFUNCTION.

FUNCTION z_process_order.
  INSERT zorders FROM @ls_order.
ENDFUNCTION.
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "z_get_customer"),
        "Expected export edge for function z_get_customer"
    );
    assert!(
        has_export_edge(&staging, "z_process_order"),
        "Expected export edge for function z_process_order"
    );
}

#[test]
fn test_class_methods_exported() {
    let content = b"\
CLASS zcl_data IMPLEMENTATION.
  METHOD get_customers.
    SELECT * FROM zcustomers INTO TABLE @DATA(lt_result).
  ENDMETHOD.

  METHOD process_order.
    INSERT zorders FROM @ls_order.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "get_customers"),
        "Expected export edge for method get_customers"
    );
    assert!(
        has_export_edge(&staging, "process_order"),
        "Expected export edge for method process_order"
    );
}

#[test]
fn test_mixed_functions_and_methods() {
    let content = b"\
FUNCTION z_calculate.
  DATA lv_result TYPE i.
  lv_result = 42.
ENDFUNCTION.

CLASS zcl_processor IMPLEMENTATION.
  METHOD validate.
    DATA lv_valid TYPE abap_bool.
    lv_valid = abap_true.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_graph_from_source(content);

    assert!(
        has_export_edge(&staging, "z_calculate"),
        "Expected export edge for function z_calculate"
    );
    assert!(
        has_export_edge(&staging, "validate"),
        "Expected export edge for method validate"
    );
}

#[test]
fn test_empty_file_no_exports() {
    let content = b"\
* This is a comment
DATA lv_variable TYPE i.
";

    let staging = build_graph_from_source(content);

    // Check that no export edges exist
    let mut has_any_export = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            has_any_export = true;
            break;
        }
    }

    assert!(
        !has_any_export,
        "Expected no export edges for file without functions/methods"
    );
}
