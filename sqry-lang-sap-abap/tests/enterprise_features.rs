//! Integration tests for SAP ABAP enterprise features.
//!
//! Tests Program nodes, Class/Interface with visibility, and cross-program calls.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
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

fn find_node_id(staging: &StagingGraph, name: &str, kind: NodeKind) -> Option<NodeId> {
    let nodes = build_node_lookup(staging);
    nodes
        .iter()
        .find(|(_, (node_name, node_kind))| node_name == name && *node_kind == kind)
        .map(|(id, _)| *id)
}

fn has_contains_edge(staging: &StagingGraph, parent: NodeId, child: NodeId) -> bool {
    staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Contains,
                source,
                target,
                ..
            } if *source == parent && *target == child
        )
    })
}

#[allow(clippy::similar_names)] // Test fixture variables
fn has_call_edge(staging: &StagingGraph, caller: NodeId, callee: NodeId) -> bool {
    staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Calls { .. },
                source,
                target,
                ..
            } if *source == caller && *target == callee
        )
    })
}

#[test]
fn test_class_node_with_visibility() {
    let source = br"
CLASS zcl_customer_mgmt IMPLEMENTATION.
  METHOD create_customer.
    INSERT zcustomers FROM @ls_customer.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Class node
    assert!(
        find_node(&staging, "zcl_customer_mgmt", NodeKind::Class),
        "Should create Class node for ABAP class"
    );

    // Should create Method node
    assert!(
        find_node(&staging, "create_customer", NodeKind::Method),
        "Should create Method node"
    );

    // Should create Contains edge from Class to Method
    let class_id = find_node_id(&staging, "zcl_customer_mgmt", NodeKind::Class)
        .expect("Class node should exist");
    let method_id = find_node_id(&staging, "create_customer", NodeKind::Method)
        .expect("Method node should exist");

    assert!(
        has_contains_edge(&staging, class_id, method_id),
        "Class should contain Method"
    );
}

#[test]
fn test_class_with_public_visibility() {
    let source = br"
CLASS zcl_public DEFINITION PUBLIC.
  PUBLIC SECTION.
    METHODS test_method.
ENDCLASS.

CLASS zcl_public IMPLEMENTATION.
  METHOD test_method.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Class node (only implementation is processed by graph builder)
    assert!(
        find_node(&staging, "zcl_public", NodeKind::Class),
        "Should create Class node with public visibility"
    );
}

#[test]
fn test_class_with_private_visibility() {
    let source = br"
CLASS lcl_private DEFINITION.
  PRIVATE SECTION.
    METHODS internal_method.
ENDCLASS.

CLASS lcl_private IMPLEMENTATION.
  METHOD internal_method.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Class node
    assert!(
        find_node(&staging, "lcl_private", NodeKind::Class),
        "Should create Class node with private visibility"
    );
}

#[test]
fn test_multiple_classes() {
    let source = br"
CLASS zcl_first IMPLEMENTATION.
  METHOD process_first.
  ENDMETHOD.
ENDCLASS.

CLASS zcl_second IMPLEMENTATION.
  METHOD process_second.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    assert!(
        find_node(&staging, "zcl_first", NodeKind::Class),
        "Should create first Class node"
    );
    assert!(
        find_node(&staging, "zcl_second", NodeKind::Class),
        "Should create second Class node"
    );
    assert!(
        find_node(&staging, "process_first", NodeKind::Method),
        "Should create method in first class"
    );
    assert!(
        find_node(&staging, "process_second", NodeKind::Method),
        "Should create method in second class"
    );
}

#[test]
fn test_submit_program_call() {
    let source = br"
CLASS zcl_runner IMPLEMENTATION.
  METHOD execute_report.
    SUBMIT z_background_job AND RETURN.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Program node for the submitted program
    assert!(
        find_node(&staging, "z_background_job", NodeKind::Module),
        "Should create Program node for SUBMIT target"
    );

    // Should create call edge from method to program
    let method_id =
        find_node_id(&staging, "execute_report", NodeKind::Method).expect("Method should exist");
    let program_id =
        find_node_id(&staging, "z_background_job", NodeKind::Module).expect("Program should exist");

    assert!(
        has_call_edge(&staging, method_id, program_id),
        "Should have call edge from method to submitted program"
    );
}

#[test]
fn test_call_transaction() {
    let source = br"
CLASS zcl_transaction_caller IMPLEMENTATION.
  METHOD call_sales_order.
    CALL TRANSACTION 'VA01' USING bdcdata.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Program node for the transaction (prefixed with TCODE_)
    assert!(
        find_node(&staging, "TCODE_VA01", NodeKind::Module),
        "Should create Program node for CALL TRANSACTION target"
    );

    // Should create call edge from method to transaction
    let method_id =
        find_node_id(&staging, "call_sales_order", NodeKind::Method).expect("Method should exist");
    let transaction_id =
        find_node_id(&staging, "TCODE_VA01", NodeKind::Module).expect("Transaction should exist");

    assert!(
        has_call_edge(&staging, method_id, transaction_id),
        "Should have call edge from method to transaction"
    );
}

#[test]
fn test_multiple_program_calls() {
    let source = br"
CLASS zcl_orchestrator IMPLEMENTATION.
  METHOD run_batch.
    SUBMIT z_extract_data AND RETURN.
    SUBMIT z_transform_data VIA SELECTION-SCREEN.
    CALL TRANSACTION 'SE38' USING bdcdata.
  ENDMETHOD.
ENDCLASS.
";

    let staging = build_staging(source);

    // Should create Program nodes for all submitted programs
    assert!(
        find_node(&staging, "z_extract_data", NodeKind::Module),
        "Should create Program node for first SUBMIT"
    );
    assert!(
        find_node(&staging, "z_transform_data", NodeKind::Module),
        "Should create Program node for second SUBMIT"
    );
    assert!(
        find_node(&staging, "TCODE_SE38", NodeKind::Module),
        "Should create Program node for CALL TRANSACTION"
    );
}

#[test]
fn test_function_module_structure() {
    let source = br"
FUNCTION z_calculate_price.
  SELECT * FROM zpricing INTO TABLE @DATA(lt_prices).
  SUBMIT z_price_update AND RETURN.
ENDFUNCTION.
";

    let staging = build_staging(source);

    // Should create Function node
    assert!(
        find_node(&staging, "z_calculate_price", NodeKind::Function),
        "Should create Function node"
    );

    // Should create Program node for SUBMIT
    assert!(
        find_node(&staging, "z_price_update", NodeKind::Module),
        "Should create Program node for SUBMIT from function"
    );

    // Should create call edge from function to program
    let function_id = find_node_id(&staging, "z_calculate_price", NodeKind::Function)
        .expect("Function should exist");
    let program_id =
        find_node_id(&staging, "z_price_update", NodeKind::Module).expect("Program should exist");

    assert!(
        has_call_edge(&staging, function_id, program_id),
        "Should have call edge from function to program"
    );
}

#[test]
fn test_abap_module_structure() {
    let source = br#"
CLASS zcl_order_processor IMPLEMENTATION.
  METHOD process_order.
    " Read order data
    SELECT * FROM zorders INTO TABLE @DATA(lt_orders).

    " Call sub-program
    SUBMIT z_validate_orders AND RETURN.

    " Call transaction
    CALL TRANSACTION 'VA02' USING bdcdata.

    " Update status
    UPDATE zorders SET status = 'PROCESSED' WHERE id = @lv_id.
  ENDMETHOD.

  METHOD finalize_order.
    " Cleanup
    DELETE FROM ztemp WHERE session = @lv_session.
  ENDMETHOD.
ENDCLASS.
"#;

    let staging = build_staging(source);

    // Should create Class node
    assert!(
        find_node(&staging, "zcl_order_processor", NodeKind::Class),
        "Should create Class node"
    );

    // Should create Method nodes
    assert!(
        find_node(&staging, "process_order", NodeKind::Method),
        "Should create first Method node"
    );
    assert!(
        find_node(&staging, "finalize_order", NodeKind::Method),
        "Should create second Method node"
    );

    // Should create Program nodes
    assert!(
        find_node(&staging, "z_validate_orders", NodeKind::Module),
        "Should create Program node for SUBMIT"
    );
    assert!(
        find_node(&staging, "TCODE_VA02", NodeKind::Module),
        "Should create Program node for CALL TRANSACTION"
    );

    // Verify Contains edges
    let class_id =
        find_node_id(&staging, "zcl_order_processor", NodeKind::Class).expect("Class should exist");
    let method1_id =
        find_node_id(&staging, "process_order", NodeKind::Method).expect("Method 1 should exist");
    let method2_id =
        find_node_id(&staging, "finalize_order", NodeKind::Method).expect("Method 2 should exist");

    assert!(
        has_contains_edge(&staging, class_id, method1_id),
        "Class should contain first method"
    );
    assert!(
        has_contains_edge(&staging, class_id, method2_id),
        "Class should contain second method"
    );

    // Verify call edges
    let program_id = find_node_id(&staging, "z_validate_orders", NodeKind::Module)
        .expect("Program should exist");
    let transaction_id =
        find_node_id(&staging, "TCODE_VA02", NodeKind::Module).expect("Transaction should exist");

    assert!(
        has_call_edge(&staging, method1_id, program_id),
        "Method should call submitted program"
    );
    assert!(
        has_call_edge(&staging, method1_id, transaction_id),
        "Method should call transaction"
    );
}
