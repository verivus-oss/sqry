//! Graph builder integration tests for SAP ABAP plugin.

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::{EdgeKind, TableWriteOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeId, StringId};
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sap_abap::SapAbapPlugin;
use std::collections::HashMap;
use std::path::Path;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn resolve_string(strings: &HashMap<u32, String>, id: StringId) -> Option<String> {
    strings.get(&id.index()).cloned()
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

fn build_staging(source: &[u8]) -> StagingGraph {
    let plugin = SapAbapPlugin::default();
    let tree = plugin.parse_ast(source).expect("parse ABAP");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, source, Path::new("test.abap"), &mut staging)
        .expect("build graph");
    staging
}

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn list_nodes_by_kind(staging: &StagingGraph, kind: NodeKind) -> Vec<String> {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .filter_map(|(node_name, node_kind)| {
            if *node_kind == kind {
                Some(node_name.clone())
            } else {
                None
            }
        })
        .collect()
}

fn count_table_reads(staging: &StagingGraph, table: &str) -> usize {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::TableRead { table_name, .. },
                ..
            } = op
            {
                return resolve_string(&strings, *table_name);
            }
            None
        })
        .filter(|name| name == table)
        .count()
}

fn list_table_reads(staging: &StagingGraph) -> Vec<String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::TableRead { table_name, .. },
                ..
            } = op
            {
                return resolve_string(&strings, *table_name);
            }
            None
        })
        .collect()
}

fn count_table_writes(staging: &StagingGraph, table: &str, op: TableWriteOp) -> usize {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op_entry| {
            if let StagingOp::AddEdge {
                kind:
                    EdgeKind::TableWrite {
                        table_name,
                        operation,
                        ..
                    },
                ..
            } = op_entry
                && *operation == op
            {
                return resolve_string(&strings, *table_name);
            }
            None
        })
        .filter(|name| name == table)
        .count()
}

#[test]
fn test_method_and_function_nodes() {
    let source = br"
CLASS zcl_example IMPLEMENTATION.
  METHOD get_customer_data.
    SELECT id name
      FROM zcustomers
      INTO TABLE @DATA(customers).
  ENDMETHOD.
ENDCLASS.

FUNCTION z_my_function.
  SELECT * FROM zorders INTO TABLE @DATA(orders).
ENDFUNCTION.
";

    let staging = build_staging(source);

    assert!(find_node(&staging, "get_customer_data", NodeKind::Method));
    assert!(find_node(&staging, "z_my_function", NodeKind::Function));
}

#[test]
fn test_table_edges_from_method() {
    let source = br"
CLASS zcl_example IMPLEMENTATION.
  METHOD get_customer_data.
    SELECT * FROM zcustomers INTO TABLE @DATA(lt_customers).
    INSERT zcustomers FROM @ls_row.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    let table_reads = list_table_reads(&staging);
    assert!(
        count_table_reads(&staging, "zcustomers") >= 1,
        "Expected table read for zcustomers, saw {table_reads:?}"
    );
    assert!(count_table_writes(&staging, "zcustomers", TableWriteOp::Insert) >= 1);
}

#[test]
fn test_full_abap_module_integration() {
    let source = br"
CLASS zcl_customer_mgmt IMPLEMENTATION.
  METHOD create_customer.
    INSERT zcustomers FROM @ls_customer.
    SELECT * FROM zcustomers INTO TABLE @lt_results.
  ENDMETHOD.

  METHOD update_status.
    UPDATE zcustomers SET status = @lv_status WHERE id = @lv_id.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    let method_names = list_nodes_by_kind(&staging, NodeKind::Method);
    assert!(
        find_node(&staging, "create_customer", NodeKind::Method),
        "Expected method create_customer, saw {method_names:?}"
    );
    assert!(
        find_node(&staging, "update_status", NodeKind::Method),
        "Expected method update_status, saw {method_names:?}"
    );
    assert!(count_table_reads(&staging, "zcustomers") >= 1);
    assert!(count_table_writes(&staging, "zcustomers", TableWriteOp::Insert) >= 1);
    assert!(count_table_writes(&staging, "zcustomers", TableWriteOp::Update) >= 1);
}
