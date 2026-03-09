//! Integration tests for SAP ABAP TypeOf and References edge extraction.
//!
//! Tests that DATA/TYPES/FIELD-SYMBOLS/CLASS-DATA declarations produce
//! TypeOf edges and References edges for non-builtin types.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sap_abap::SapAbapPlugin;
use std::collections::HashMap;
use std::path::Path;

// ========== Test helpers ==========

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

fn has_typeof_edge(staging: &StagingGraph) -> bool {
    staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::TypeOf { .. },
                ..
            }
        )
    })
}

fn has_reference_edge(staging: &StagingGraph) -> bool {
    staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::References,
                ..
            }
        )
    })
}

fn count_typeof_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TypeOf { .. },
                    ..
                }
            )
        })
        .count()
}

fn typeof_targets(staging: &StagingGraph) -> Vec<String> {
    let nodes = build_node_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::TypeOf { .. },
                ..
            } = op
            {
                nodes.get(target).map(|(name, _)| name.clone())
            } else {
                None
            }
        })
        .collect()
}

fn reference_targets(staging: &StagingGraph) -> Vec<String> {
    let nodes = build_node_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                nodes.get(target).map(|(name, _)| name.clone())
            } else {
                None
            }
        })
        .collect()
}

// ========== TypeOf and References Edge Tests ==========

#[test]
fn test_abap_simple_data_typeof() {
    let source = b"REPORT ztest.\nDATA lv_name TYPE string.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for DATA declaration"
    );
}

#[test]
fn test_abap_custom_type_references() {
    let source = b"REPORT ztest.\nDATA ls_material TYPE zmaterial.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging), "Expected TypeOf edge");
    assert!(
        has_reference_edge(&staging),
        "Expected References edge for custom type zmaterial"
    );
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zmaterial"),
        "Expected zmaterial in References targets, got {refs:?}"
    );
}

#[test]
fn test_abap_colon_notation() {
    let source = b"REPORT ztest.\nDATA: lv_count TYPE i, lv_name TYPE string.\n";
    let staging = build_staging(source);
    assert!(
        count_typeof_edges(&staging) >= 2,
        "Expected at least 2 TypeOf edges for colon notation, got {}",
        count_typeof_edges(&staging)
    );
}

#[test]
fn test_abap_table_of() {
    let source = b"REPORT ztest.\nDATA lt_items TYPE TABLE OF zmaterial.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for TABLE OF"
    );
    assert!(
        has_reference_edge(&staging),
        "Expected References edge for zmaterial base type"
    );
}

#[test]
fn test_abap_standard_table() {
    let source = b"REPORT ztest.\nDATA lt_data TYPE STANDARD TABLE OF zstructure.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging));
    assert!(has_reference_edge(&staging));
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zstructure"),
        "Expected zstructure in References targets, got {refs:?}"
    );
}

#[test]
fn test_abap_field_symbols() {
    let source = b"REPORT ztest.\nFIELD-SYMBOLS <fs_item> TYPE zstructure.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for FIELD-SYMBOLS"
    );
    let targets = typeof_targets(&staging);
    assert!(
        targets.iter().any(|t| t == "zstructure"),
        "Expected zstructure in TypeOf targets, got {targets:?}"
    );
}

#[test]
fn test_abap_types_declaration() {
    let source = b"REPORT ztest.\nTYPES ty_name TYPE string.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for TYPES declaration"
    );
}

#[test]
fn test_abap_like_declaration() {
    let source = b"REPORT ztest.\nDATA lv_original TYPE string.\nDATA lv_copy LIKE lv_original.\n";
    let staging = build_staging(source);
    assert!(
        count_typeof_edges(&staging) >= 2,
        "Expected TypeOf edges for both declarations, got {}",
        count_typeof_edges(&staging)
    );
}

#[test]
fn test_abap_builtin_no_references() {
    let source = b"REPORT ztest.\nDATA lv_count TYPE i.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging), "Expected TypeOf edge");
    assert!(
        !has_reference_edge(&staging),
        "Should NOT have References edge for builtin type i"
    );
}

#[test]
fn test_abap_user_type_has_references() {
    let source = b"REPORT ztest.\nDATA ls_order TYPE zsales_order.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging));
    assert!(
        has_reference_edge(&staging),
        "Expected References edge for user-defined type zsales_order"
    );
}

#[test]
fn test_abap_coexists_with_table_edges() {
    // This test uses source that has both table operations and type declarations
    let source = br#"
REPORT ztest.
DATA lt_items TYPE TABLE OF zmaterial.
DATA lv_count TYPE i.
SELECT * FROM zmaterial INTO TABLE lt_items.
"#;
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging), "Expected TypeOf edges");
    assert!(staging.stats().nodes_staged > 0, "Should have nodes");
}

#[test]
fn test_abap_class_data_typeof() {
    let source = b"REPORT ztest.\nCLASS-DATA gv_instance TYPE REF TO zcl_myclass.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for CLASS-DATA declaration"
    );

    // REF TO should preserve the full class reference
    let targets = typeof_targets(&staging);
    assert!(
        targets.iter().any(|t| t.contains("zcl_myclass")),
        "TypeOf target should contain zcl_myclass, got {targets:?}"
    );

    // Should have References edge to the class
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zcl_myclass"),
        "Expected References edge to zcl_myclass, got {refs:?}"
    );
}

#[test]
fn test_abap_constants_typeof() {
    let source = b"REPORT ztest.\nCONSTANTS gc_max TYPE i VALUE 100.\n";
    let staging = build_staging(source);
    assert!(
        has_typeof_edge(&staging),
        "Expected TypeOf edge for CONSTANTS declaration"
    );
}

#[test]
fn test_abap_multiple_builtin_types_no_references() {
    let source = b"REPORT ztest.\nDATA: lv_str TYPE string, lv_int TYPE i, lv_float TYPE f.\n";
    let staging = build_staging(source);
    assert!(
        count_typeof_edges(&staging) >= 3,
        "Expected at least 3 TypeOf edges, got {}",
        count_typeof_edges(&staging)
    );
    assert!(
        !has_reference_edge(&staging),
        "Should NOT have References edges for builtin types"
    );
}

#[test]
fn test_abap_hashed_table_references() {
    let source = b"REPORT ztest.\nDATA lt_map TYPE HASHED TABLE OF zentry.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging));
    assert!(has_reference_edge(&staging));
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zentry"),
        "Expected zentry in References targets, got {refs:?}"
    );
}

#[test]
fn test_abap_sorted_table_references() {
    let source = b"REPORT ztest.\nDATA lt_sorted TYPE SORTED TABLE OF zsorted_entry.\n";
    let staging = build_staging(source);
    assert!(has_typeof_edge(&staging));
    assert!(has_reference_edge(&staging));
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zsorted_entry"),
        "Expected zsorted_entry in References targets, got {refs:?}"
    );
}

#[test]
fn test_abap_like_creates_reference_to_variable() {
    let source =
        b"REPORT ztest.\nDATA lv_original TYPE zcustomer.\nDATA lv_copy LIKE lv_original.\n";
    let staging = build_staging(source);
    let nodes = build_node_lookup(&staging);

    // LIKE should create a TypeOf edge
    assert!(
        count_typeof_edges(&staging) >= 2,
        "Expected at least 2 TypeOf edges"
    );

    // The TypeOf target for the LIKE declaration should be a Type node named lv_original
    let like_typeof_targets: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::TypeOf { .. },
                ..
            } = op
            {
                nodes.get(target).map(|(name, kind)| (name.clone(), *kind))
            } else {
                None
            }
        })
        .collect();

    // One target should be lv_original (from the LIKE declaration)
    assert!(
        like_typeof_targets
            .iter()
            .any(|(name, _)| name == "lv_original"),
        "Expected lv_original as a TypeOf target for LIKE, got {like_typeof_targets:?}"
    );

    // All TypeOf targets should be Type nodes (including the LIKE target)
    for (name, kind) in &like_typeof_targets {
        assert_eq!(
            *kind,
            NodeKind::Type,
            "TypeOf target '{name}' should be a Type node, got {kind:?}"
        );
    }

    // LIKE should create a References edge to the variable it references
    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "lv_original"),
        "Expected lv_original in References targets for LIKE declaration, got {refs:?}"
    );

    // The References target node for lv_original should be a Type node
    let ref_node_kinds: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                nodes.get(target).map(|(name, kind)| (name.clone(), *kind))
            } else {
                None
            }
        })
        .collect();

    for (name, kind) in &ref_node_kinds {
        if name == "lv_original" {
            assert_eq!(
                *kind,
                NodeKind::Type,
                "References target '{name}' should be a Type node, got {kind:?}"
            );
        }
    }
}

#[test]
fn test_abap_typeof_target_is_type_node() {
    let source = b"REPORT ztest.\nDATA ls_data TYPE zstructure.\n";
    let staging = build_staging(source);
    let nodes = build_node_lookup(&staging);

    // The TypeOf target should be a Type node, not a Variable node
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::TypeOf { .. },
            ..
        } = op
            && let Some((_name, kind)) = nodes.get(target)
        {
            assert_eq!(
                *kind,
                NodeKind::Type,
                "TypeOf target should be a Type node, got {kind:?}"
            );
        }
    }
}

#[test]
fn test_abap_typeof_source_is_variable_node() {
    let source = b"REPORT ztest.\nDATA ls_data TYPE zstructure.\n";
    let staging = build_staging(source);
    let nodes = build_node_lookup(&staging);

    // The TypeOf source should be a Variable node
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            kind: EdgeKind::TypeOf { .. },
            ..
        } = op
            && let Some((_name, kind)) = nodes.get(source)
        {
            assert_eq!(
                *kind,
                NodeKind::Variable,
                "TypeOf source should be a Variable node, got {kind:?}"
            );
        }
    }
}

#[test]
fn test_abap_mixed_types_and_table_ops() {
    // Comprehensive test with type declarations, table operations, and method structure
    let source = br#"
CLASS zcl_processor IMPLEMENTATION.
  METHOD process_data.
    DATA lt_items TYPE TABLE OF zmaterial.
    DATA ls_customer TYPE zcustomer.
    DATA lv_count TYPE i.
    SELECT * FROM zmaterial INTO TABLE lt_items.
    INSERT zcustomer FROM @ls_customer.
  ENDMETHOD.
ENDCLASS.
"#;
    let staging = build_staging(source);

    // Should have TypeOf edges from type declarations
    assert!(
        count_typeof_edges(&staging) >= 3,
        "Expected at least 3 TypeOf edges, got {}",
        count_typeof_edges(&staging)
    );

    // Should have References edges for custom types (zmaterial, zcustomer) but not for i
    assert!(
        has_reference_edge(&staging),
        "Expected References edges for custom types"
    );

    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zmaterial"),
        "Expected zmaterial in References targets"
    );
    assert!(
        refs.iter().any(|r| r == "zcustomer"),
        "Expected zcustomer in References targets"
    );
}

#[test]
fn test_abap_ref_to_type_extraction() {
    let source = b"REPORT ztest.\nDATA lo_obj TYPE REF TO zcl_processor.\n";
    let staging = build_staging(source);

    let targets = typeof_targets(&staging);
    assert!(
        targets.iter().any(|t| t.contains("zcl_processor")),
        "REF TO should extract full class name, got {targets:?}"
    );

    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zcl_processor"),
        "Should have References edge to zcl_processor, got {refs:?}"
    );
}

#[test]
fn test_abap_ref_to_with_interface() {
    let source = b"REPORT ztest.\nDATA lo_intf TYPE REF TO zif_handler.\n";
    let staging = build_staging(source);

    let targets = typeof_targets(&staging);
    assert!(
        targets.iter().any(|t| t.contains("zif_handler")),
        "REF TO should extract interface name, got {targets:?}"
    );

    let refs = reference_targets(&staging);
    assert!(
        refs.iter().any(|r| r == "zif_handler"),
        "Should have References edge to zif_handler, got {refs:?}"
    );
}
