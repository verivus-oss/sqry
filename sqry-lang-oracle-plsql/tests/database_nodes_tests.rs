//! Comprehensive tests for Oracle PL/SQL database nodes.
//!
//! Tests all database-specific NodeKind variants:
//! - Package (Oracle-specific)
//! - Procedure (procedures and functions)
//! - Trigger
//! - Table
//! - View (if grammar supports)

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::{EdgeKind, TableWriteOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_oracle_plsql::OraclePlsqlPlugin;
use std::collections::HashMap;
use std::path::Path;

// Helper functions from basic_test.rs
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
    let plugin = OraclePlsqlPlugin::default();
    let tree = plugin.parse_ast(source).expect("parse PL/SQL");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, source, Path::new("test.pkb"), &mut staging)
        .expect("build graph");
    staging
}

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn count_export_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        })
        .count()
}

fn count_triggered_by_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TriggeredBy { .. },
                    ..
                }
            )
        })
        .count()
}

fn count_table_reads(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TableRead { .. },
                    ..
                }
            )
        })
        .count()
}

fn count_table_writes(staging: &StagingGraph, op_type: TableWriteOp) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TableWrite { operation, .. },
                    ..
                } if *operation == op_type
            )
        })
        .count()
}

#[test]
fn test_create_package_node_kind() {
    let source = br#"
CREATE OR REPLACE PACKAGE hr_utils AS
  PROCEDURE hire_employee(emp_name VARCHAR2);
END hr_utils;
"#;

    let staging = build_staging(source);

    // Verify package uses NodeKind::Module
    assert!(
        find_node(&staging, "hr_utils", NodeKind::Module),
        "Package should use NodeKind::Module"
    );

    // Verify package is exported
    assert!(
        count_export_edges(&staging) >= 1,
        "Package should be exported from file module"
    );
}

#[test]
#[ignore = "Grammar limitation: tree-sitter-plsql does not support standalone procedures outside packages"]
fn test_create_procedure_standalone_node_kind() {
    let source = br#"
CREATE OR REPLACE PROCEDURE update_salary(emp_id NUMBER, new_sal NUMBER) IS
BEGIN
  UPDATE employees SET salary = new_sal WHERE id = emp_id;
END update_salary;
"#;

    let staging = build_staging(source);

    // Verify standalone procedure uses NodeKind::Function
    assert!(
        find_node(&staging, "update_salary", NodeKind::Function),
        "Standalone procedure should use NodeKind::Function"
    );

    // Verify procedure is exported
    assert!(
        count_export_edges(&staging) >= 1,
        "Procedure should be exported from file module"
    );
}

#[test]
fn test_create_procedure_in_package_node_kind() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY payroll AS
  PROCEDURE process_payroll IS
  BEGIN
    SELECT * FROM employees;
  END process_payroll;
END payroll;
"#;

    let staging = build_staging(source);

    // Verify qualified procedure name
    assert!(
        find_node(&staging, "payroll.process_payroll", NodeKind::Function),
        "Package member procedure should use qualified name and NodeKind::Function"
    );

    // Verify both package and procedure exported
    assert!(
        count_export_edges(&staging) >= 2,
        "Both package and procedure should be exported"
    );
}

#[test]
#[ignore = "Grammar limitation: tree-sitter-plsql does not support standalone functions outside packages"]
fn test_create_function_standalone_node_kind() {
    let source = br#"
CREATE OR REPLACE FUNCTION get_employee_count RETURN NUMBER IS
BEGIN
  RETURN (SELECT COUNT(*) FROM employees);
END get_employee_count;
"#;

    let staging = build_staging(source);

    // Verify standalone function uses NodeKind::Function (functions are procedures in DB context)
    assert!(
        find_node(&staging, "get_employee_count", NodeKind::Function),
        "Standalone function should use NodeKind::Function"
    );
}

#[test]
fn test_create_function_in_package_node_kind() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY hr_utils AS
  FUNCTION get_salary(emp_id NUMBER) RETURN NUMBER IS
  BEGIN
    RETURN (SELECT salary FROM employees WHERE id = emp_id);
  END get_salary;
END hr_utils;
"#;

    let staging = build_staging(source);

    // Verify qualified function name
    assert!(
        find_node(&staging, "hr_utils.get_salary", NodeKind::Function),
        "Package member function should use qualified name and NodeKind::Function"
    );
}

#[test]
#[ignore = "Grammar limitation: tree-sitter-plsql does not support standalone triggers outside packages"]
fn test_create_trigger_node_kind() {
    let source = br#"
CREATE OR REPLACE TRIGGER audit_employee_changes
BEFORE UPDATE ON employees
FOR EACH ROW
BEGIN
  INSERT INTO audit_log VALUES (SYSDATE, USER, :OLD.id);
END;
"#;

    let staging = build_staging(source);

    // Verify trigger uses NodeKind::Function
    assert!(
        find_node(&staging, "audit_employee_changes", NodeKind::Function),
        "Trigger should use NodeKind::Function"
    );

    // Verify table uses NodeKind::Variable
    assert!(
        find_node(&staging, "employees", NodeKind::Variable),
        "Table should use NodeKind::Variable"
    );

    // Verify TriggeredBy edge
    assert!(
        count_triggered_by_edges(&staging) >= 1,
        "Trigger should have TriggeredBy edge to table"
    );
}

#[test]
fn test_create_table_node_kind() {
    let source = br#"
CREATE OR REPLACE PROCEDURE test_proc IS
BEGIN
  SELECT * FROM employees;
  INSERT INTO audit_log VALUES (1);
  UPDATE employees SET status = 'active';
  DELETE FROM temp_data;
END test_proc;
"#;

    let staging = build_staging(source);

    // Verify all tables use NodeKind::Variable
    assert!(
        find_node(&staging, "employees", NodeKind::Variable),
        "employees table should use NodeKind::Variable"
    );
    assert!(
        find_node(&staging, "audit_log", NodeKind::Variable),
        "audit_log table should use NodeKind::Variable"
    );
    assert!(
        find_node(&staging, "temp_data", NodeKind::Variable),
        "temp_data table should use NodeKind::Variable"
    );

    // Verify TableRead edge
    assert!(
        count_table_reads(&staging) >= 1,
        "Should have TableRead edge for SELECT"
    );

    // Verify TableWrite edges
    assert!(
        count_table_writes(&staging, TableWriteOp::Insert) >= 1,
        "Should have TableWrite(Insert) edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Update) >= 1,
        "Should have TableWrite(Update) edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Delete) >= 1,
        "Should have TableWrite(Delete) edge"
    );
}

#[test]
fn test_package_contains_procedures() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY employee_mgmt AS
  PROCEDURE hire(name VARCHAR2) IS
  BEGIN
    INSERT INTO employees (name) VALUES (name);
  END hire;

  PROCEDURE fire(emp_id NUMBER) IS
  BEGIN
    DELETE FROM employees WHERE id = emp_id;
  END fire;

  FUNCTION get_count RETURN NUMBER IS
  BEGIN
    RETURN (SELECT COUNT(*) FROM employees);
  END get_count;
END employee_mgmt;
"#;

    let staging = build_staging(source);

    // Verify package
    assert!(
        find_node(&staging, "employee_mgmt", NodeKind::Module),
        "Package should exist"
    );

    // Verify all procedures with qualified names
    assert!(
        find_node(&staging, "employee_mgmt.hire", NodeKind::Function),
        "hire procedure should have qualified name"
    );
    assert!(
        find_node(&staging, "employee_mgmt.fire", NodeKind::Function),
        "fire procedure should have qualified name"
    );
    assert!(
        find_node(&staging, "employee_mgmt.get_count", NodeKind::Function),
        "get_count function should have qualified name"
    );

    // Verify exports (1 package + 3 procedures = 4)
    assert!(
        count_export_edges(&staging) >= 4,
        "Should have 4 export edges (1 package + 3 procedures)"
    );
}

#[test]
fn test_procedure_table_read_write() {
    // Grammar limitation: wrap in package body
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE manage_employees IS
  BEGIN
    -- Read operation
    SELECT * FROM employees WHERE status = 'active';

    -- Write operations
    INSERT INTO audit_log VALUES (SYSDATE, 'read_employees');
    UPDATE employees SET last_access = SYSDATE;
    DELETE FROM expired_records;
  END manage_employees;
END test_pkg;
"#;

    let staging = build_staging(source);

    // Verify procedure (qualified name in package)
    assert!(
        find_node(&staging, "test_pkg.manage_employees", NodeKind::Function),
        "Procedure should exist"
    );

    // Verify tables
    assert!(
        find_node(&staging, "employees", NodeKind::Variable),
        "employees table should exist"
    );
    assert!(
        find_node(&staging, "audit_log", NodeKind::Variable),
        "audit_log table should exist"
    );

    // Verify edges
    assert!(
        count_table_reads(&staging) >= 1,
        "Should have at least 1 TableRead edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Insert) >= 1,
        "Should have at least 1 Insert edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Update) >= 1,
        "Should have at least 1 Update edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Delete) >= 1,
        "Should have at least 1 Delete edge"
    );
}

#[test]
#[ignore = "Grammar limitation: tree-sitter-plsql does not support standalone triggers"]
fn test_trigger_table_attachment() {
    let source = br#"
CREATE OR REPLACE TRIGGER log_changes
AFTER UPDATE ON employees
FOR EACH ROW
BEGIN
  INSERT INTO change_log VALUES (:NEW.id, SYSDATE);
END;
"#;

    let staging = build_staging(source);

    // Verify trigger
    assert!(
        find_node(&staging, "log_changes", NodeKind::Function),
        "Trigger should exist with NodeKind::Function"
    );

    // Verify tables
    assert!(
        find_node(&staging, "employees", NodeKind::Variable),
        "employees table should exist"
    );
    assert!(
        find_node(&staging, "change_log", NodeKind::Variable),
        "change_log table should exist"
    );

    // Verify TriggeredBy edge
    assert!(
        count_triggered_by_edges(&staging) >= 1,
        "Trigger should have TriggeredBy edge to employees table"
    );

    // Verify TableWrite edge (INSERT in trigger body)
    assert!(
        count_table_writes(&staging, TableWriteOp::Insert) >= 1,
        "Trigger body should have Insert edge"
    );
}

#[test]
fn test_cross_package_calls() {
    // Test that qualified calls to other packages create procedure nodes
    // Note: The current grammar has limited support for qualified calls
    let source = br#"
CREATE OR REPLACE PACKAGE BODY pkg1 AS
  PROCEDURE proc1 IS
  BEGIN
    SELECT * FROM table1;
    INSERT INTO table2 VALUES (1);
  END proc1;

  PROCEDURE proc2 IS
  BEGIN
    DELETE FROM table3;
  END proc2;
END pkg1;
"#;

    let staging = build_staging(source);

    // Verify package
    assert!(
        find_node(&staging, "pkg1", NodeKind::Module),
        "pkg1 package should exist"
    );

    // Verify multiple procedures with qualified names
    assert!(
        find_node(&staging, "pkg1.proc1", NodeKind::Function),
        "pkg1.proc1 should exist"
    );
    assert!(
        find_node(&staging, "pkg1.proc2", NodeKind::Function),
        "pkg1.proc2 should exist"
    );

    // Verify tables use NodeKind::Variable
    assert!(
        find_node(&staging, "table1", NodeKind::Variable),
        "table1 should exist as Table"
    );
    assert!(
        find_node(&staging, "table2", NodeKind::Variable),
        "table2 should exist as Table"
    );
    assert!(
        find_node(&staging, "table3", NodeKind::Variable),
        "table3 should exist as Table"
    );

    // Verify table operations
    assert!(
        count_table_reads(&staging) >= 1,
        "Should have TableRead edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Insert) >= 1,
        "Should have Insert edge"
    );
    assert!(
        count_table_writes(&staging, TableWriteOp::Delete) >= 1,
        "Should have Delete edge"
    );
}

#[test]
fn test_mixed_database_objects() {
    // Grammar limitation: only packages are fully supported
    let source = br#"
-- Package with procedures
CREATE OR REPLACE PACKAGE BODY utils AS
  PROCEDURE util_proc IS
  BEGIN
    INSERT INTO logs VALUES (SYSDATE);
  END util_proc;

  PROCEDURE read_users IS
  BEGIN
    SELECT * FROM users;
  END read_users;
END utils;

-- Second package
CREATE OR REPLACE PACKAGE BODY audit_pkg AS
  PROCEDURE log_event IS
  BEGIN
    INSERT INTO audit_trail VALUES (SYSDATE);
  END log_event;
END audit_pkg;
"#;

    let staging = build_staging(source);

    // Verify packages
    assert!(
        find_node(&staging, "utils", NodeKind::Module),
        "utils package should exist"
    );
    assert!(
        find_node(&staging, "audit_pkg", NodeKind::Module),
        "audit_pkg package should exist"
    );

    // Verify package procedures with qualified names
    assert!(
        find_node(&staging, "utils.util_proc", NodeKind::Function),
        "utils.util_proc should exist with qualified name"
    );
    assert!(
        find_node(&staging, "utils.read_users", NodeKind::Function),
        "utils.read_users should exist with qualified name"
    );
    assert!(
        find_node(&staging, "audit_pkg.log_event", NodeKind::Function),
        "audit_pkg.log_event should exist with qualified name"
    );

    // Verify tables
    assert!(
        find_node(&staging, "users", NodeKind::Variable),
        "users table should exist"
    );
    assert!(
        find_node(&staging, "logs", NodeKind::Variable),
        "logs table should exist"
    );
    assert!(
        find_node(&staging, "audit_trail", NodeKind::Variable),
        "audit_trail table should exist"
    );

    // Verify exports (2 packages + 3 procedures = 5)
    assert!(
        count_export_edges(&staging) >= 5,
        "Should have at least 5 export edges"
    );
}
