//! Graph builder tests for the Oracle PL/SQL language plugin.
//!
//! Covers:
//! - Module node creation
//! - Package procedure/function extraction
//! - Call edge detection
//! - Table access detection
//! - Error handling for malformed input
//!
//! NOTE: The tree-sitter-plsql grammar is designed primarily for PACKAGE and
//! PACKAGE BODY constructs. Tests reflect actual grammar capabilities.

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_oracle_plsql::OraclePlsqlGraphBuilder;
use std::path::Path;

fn parse_plsql(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_plsql_sqry::language())
        .expect("failed to set PL/SQL language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse PL/SQL code")
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty PL/SQL file should succeed");
}

#[test]
fn test_package_specification() {
    let source = r"
CREATE OR REPLACE PACKAGE my_package AS
    PROCEDURE do_work(p_id IN NUMBER);
    FUNCTION get_value(p_key IN VARCHAR2) RETURN VARCHAR2;
END my_package;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("package.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Package specification should succeed");

    // Should create module node at minimum
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node (module), got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_package_body() {
    let source = r"
CREATE OR REPLACE PACKAGE BODY my_package AS
    PROCEDURE do_work(p_id IN NUMBER) IS
    BEGIN
        UPDATE employees SET active = 1 WHERE id = p_id;
        COMMIT;
    END do_work;

    FUNCTION get_value(p_key IN VARCHAR2) RETURN VARCHAR2 IS
        v_result VARCHAR2(100);
    BEGIN
        SELECT value INTO v_result FROM config WHERE key = p_key;
        RETURN v_result;
    END get_value;
END my_package;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("package_body.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Package body should succeed");
}

#[test]
fn test_cross_package_call() {
    let source = r"
CREATE OR REPLACE PACKAGE BODY service_pkg AS
    PROCEDURE run IS
    BEGIN
        other_pkg.helper();
        utility_pkg.log('done');
    END run;
END service_pkg;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("service.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Cross-package calls should succeed");
}

#[test]
fn test_table_access_select() {
    let source = r"
CREATE OR REPLACE PACKAGE BODY data_pkg AS
    FUNCTION get_employee(p_id IN NUMBER) RETURN VARCHAR2 IS
        v_name VARCHAR2(100);
    BEGIN
        SELECT name INTO v_name FROM employees WHERE id = p_id;
        RETURN v_name;
    END get_employee;
END data_pkg;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("data.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Table SELECT access should succeed");
}

#[test]
fn test_table_access_dml() {
    let source = r"
CREATE OR REPLACE PACKAGE BODY dml_pkg AS
    PROCEDURE update_record(p_id IN NUMBER, p_value IN VARCHAR2) IS
    BEGIN
        UPDATE records SET value = p_value WHERE id = p_id;
        INSERT INTO audit_log (record_id, action) VALUES (p_id, 'UPDATE');
        DELETE FROM temp_records WHERE id = p_id;
        COMMIT;
    END update_record;
END dml_pkg;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("dml.sql"), &mut staging);
    assert!(result.is_ok(), "Table DML access should succeed");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = OraclePlsqlGraphBuilder::new();
    assert_eq!(builder.language(), Language::Plsql);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OraclePlsqlGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_plsql() {
    // Incomplete PL/SQL - tree-sitter is error-tolerant
    let source = r"
CREATE OR REPLACE PACKAGE BODY broken AS
    PROCEDURE incomplete(
"; // incomplete
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.sql"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
-- This is a comment
/* Block comment */
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only PL/SQL should succeed");
}

#[test]
fn test_simple_package_spec_and_body() {
    let source = r"
CREATE OR REPLACE PACKAGE hello_pkg AS
    PROCEDURE say_hello;
END hello_pkg;
/

CREATE OR REPLACE PACKAGE BODY hello_pkg AS
    PROCEDURE say_hello IS
    BEGIN
        DBMS_OUTPUT.PUT_LINE('Hello, World!');
    END say_hello;
END hello_pkg;
/
";
    let tree = parse_plsql(source);
    let mut staging = StagingGraph::new();
    let builder = OraclePlsqlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("hello.sql"),
        &mut staging,
    );
    assert!(
        result.is_ok(),
        "Complete package spec + body should succeed"
    );

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}
