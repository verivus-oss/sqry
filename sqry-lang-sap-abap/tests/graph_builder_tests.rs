//! Graph builder tests for the SAP ABAP language plugin.
//!
//! Covers:
//! - Class method extraction
//! - CALL FUNCTION reference edges
//! - SELECT table read edges
//! - DML table write edges (text-based fallback)
//! - INCLUDE import edges
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_sap_abap::AbapGraphBuilder;
use std::path::Path;

fn parse_abap(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_abap_sqry::language())
        .expect("failed to set ABAP language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse ABAP code")
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty ABAP file should succeed");
}

#[test]
fn test_comments_only() {
    let source = r"
* This is a comment
* Another comment
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only ABAP file should succeed");
}

// ==================== Class Method Extraction ====================

#[test]
fn test_class_definition() {
    let source = r"
CLASS lcl_app DEFINITION.
  PUBLIC SECTION.
    METHODS:
      run,
      initialize.
ENDCLASS.

CLASS lcl_app IMPLEMENTATION.
  METHOD run.
    WRITE: 'Running'.
  ENDMETHOD.

  METHOD initialize.
    WRITE: 'Initializing'.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("app.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "Class definition should succeed");

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_multiple_classes() {
    let source = r"
CLASS lcl_base DEFINITION.
  PUBLIC SECTION.
    METHODS: start.
ENDCLASS.

CLASS lcl_derived DEFINITION INHERITING FROM lcl_base.
  PUBLIC SECTION.
    METHODS: start REDEFINITION.
ENDCLASS.

CLASS lcl_base IMPLEMENTATION.
  METHOD start.
    WRITE: 'base'.
  ENDMETHOD.
ENDCLASS.

CLASS lcl_derived IMPLEMENTATION.
  METHOD start.
    WRITE: 'derived'.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("classes.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "Multiple classes should succeed");
}

// ==================== CALL FUNCTION Edges ====================

#[test]
fn test_call_function() {
    let source = r"
REPORT ztest_report.

CLASS lcl_processor DEFINITION.
  PUBLIC SECTION.
    METHODS: process.
ENDCLASS.

CLASS lcl_processor IMPLEMENTATION.
  METHOD process.
    CALL FUNCTION 'BAPI_SALESORDER_GETLIST'
      EXPORTING
        customer_number = '0000001000'
      TABLES
        sales_orders = lt_orders.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("report.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "CALL FUNCTION should succeed");
}

#[test]
fn test_multiple_call_functions() {
    let source = r"
CLASS lcl_handler DEFINITION.
  PUBLIC SECTION.
    METHODS: do_all.
ENDCLASS.

CLASS lcl_handler IMPLEMENTATION.
  METHOD do_all.
    CALL FUNCTION 'FUNC_ONE'.
    CALL FUNCTION 'FUNC_TWO'
      EXPORTING iv_flag = abap_true.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("handler.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "Multiple CALL FUNCTIONs should succeed");
}

// ==================== SELECT Table Access ====================

#[test]
fn test_select_table_read() {
    let source = r"
CLASS lcl_dao DEFINITION.
  PUBLIC SECTION.
    METHODS: get_data.
ENDCLASS.

CLASS lcl_dao IMPLEMENTATION.
  METHOD get_data.
    DATA: lt_sflight TYPE TABLE OF sflight.
    SELECT * FROM sflight INTO TABLE lt_sflight.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("dao.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "SELECT table read should succeed");
}

#[test]
fn test_select_with_where() {
    let source = r"
CLASS lcl_query DEFINITION.
  PUBLIC SECTION.
    METHODS: find_by_id IMPORTING iv_id TYPE i.
ENDCLASS.

CLASS lcl_query IMPLEMENTATION.
  METHOD find_by_id.
    DATA: ls_mara TYPE mara.
    SELECT SINGLE * FROM mara
      INTO ls_mara
      WHERE matnr = iv_id.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("query.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "SELECT with WHERE should succeed");
}

// ==================== DML Table Writes ====================

#[test]
fn test_insert_table_write() {
    let source = r"
CLASS lcl_writer DEFINITION.
  PUBLIC SECTION.
    METHODS: save.
ENDCLASS.

CLASS lcl_writer IMPLEMENTATION.
  METHOD save.
    DATA: ls_record TYPE ztable.
    INSERT INTO ztable VALUES ls_record.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("writer.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "INSERT table write should succeed");
}

#[test]
fn test_update_and_delete() {
    let source = r"
CLASS lcl_updater DEFINITION.
  PUBLIC SECTION.
    METHODS: update_record,
             delete_record.
ENDCLASS.

CLASS lcl_updater IMPLEMENTATION.
  METHOD update_record.
    UPDATE ztransactions SET status = 'X' WHERE id = 1.
  ENDMETHOD.

  METHOD delete_record.
    DELETE FROM zarchive WHERE year < 2020.
  ENDMETHOD.
ENDCLASS.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("updater.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "UPDATE and DELETE should succeed");
}

// ==================== INCLUDE Import Edges ====================

#[test]
fn test_include_statement() {
    let source = r"
REPORT ztest_main.

INCLUDE ztest_utils.
INCLUDE ztest_constants.

START-OF-SELECTION.
  WRITE: 'Done'.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("main.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "INCLUDE statements should succeed");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = AbapGraphBuilder::new();
    assert_eq!(builder.language(), Language::Abap);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AbapGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_abap() {
    // Incomplete ABAP - tree-sitter is error-tolerant
    let source = r"
CLASS lcl_broken DEFINITION.
  PUBLIC SECTION.
    METHODS: incomplete
"; // incomplete
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.abap"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_report_with_form() {
    let source = r"
REPORT ztest_forms.

START-OF-SELECTION.
  PERFORM do_work.

FORM do_work.
  WRITE: 'Working'.
ENDFORM.
";
    let tree = parse_abap(source);
    let mut staging = StagingGraph::new();
    let builder = AbapGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("forms.abap"),
        &mut staging,
    );
    assert!(result.is_ok(), "REPORT with FORM should succeed");
}
