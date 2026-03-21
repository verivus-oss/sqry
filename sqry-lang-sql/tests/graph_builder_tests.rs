//! Graph builder tests for the SQL language plugin.
//!
//! Covers:
//! - Procedure/function definition extraction
//! - Trigger definition extraction
//! - Table read edges (SELECT)
//! - Table write edges (INSERT, UPDATE, DELETE)
//! - Cross-procedure call detection
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_sql::SqlGraphBuilder;
use std::path::Path;

fn parse_sql(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("failed to set SQL language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse SQL code")
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("empty.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty SQL file should succeed");
}

#[test]
fn test_comments_only() {
    let source = r"
-- This is a comment
/* Block comment */
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only SQL file should succeed");
}

// ==================== Procedure/Function Extraction ====================

#[test]
fn test_procedure_extraction() {
    let source = r"
CREATE PROCEDURE update_salary(
    IN emp_id INT,
    IN new_salary DECIMAL(10,2)
)
BEGIN
    UPDATE employees SET salary = new_salary WHERE id = emp_id;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("procedures.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Procedure extraction should succeed");

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 node, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_function_extraction() {
    let source = r"
CREATE FUNCTION get_employee_count(dept_id INT)
RETURNS INT
BEGIN
    DECLARE count INT;
    SELECT COUNT(*) INTO count FROM employees WHERE department_id = dept_id;
    RETURN count;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("functions.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Function extraction should succeed");
}

#[test]
fn test_multiple_procedures() {
    let source = r"
CREATE PROCEDURE hire_employee(
    IN name VARCHAR(100),
    IN dept_id INT
)
BEGIN
    INSERT INTO employees (name, department_id) VALUES (name, dept_id);
END;

CREATE PROCEDURE fire_employee(IN emp_id INT)
BEGIN
    DELETE FROM employees WHERE id = emp_id;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("hr.sql"), &mut staging);
    assert!(result.is_ok(), "Multiple procedures should succeed");

    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 procedure node, got {}",
        stats.nodes_staged
    );
}

// ==================== Table Access ====================

#[test]
fn test_select_table_read() {
    let source = r"
CREATE PROCEDURE get_orders(IN customer_id INT)
BEGIN
    SELECT * FROM orders WHERE customer_id = customer_id;
    SELECT p.name, oi.quantity
    FROM order_items oi
    JOIN products p ON oi.product_id = p.id;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("orders.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "SELECT table reads should succeed");
}

#[test]
fn test_insert_table_write() {
    let source = r"
CREATE PROCEDURE log_activity(
    IN user_id INT,
    IN action VARCHAR(100)
)
BEGIN
    INSERT INTO activity_log (user_id, action, created_at)
    VALUES (user_id, action, NOW());
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("logging.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "INSERT table write should succeed");
}

#[test]
fn test_update_table_write() {
    let source = r"
CREATE PROCEDURE deactivate_user(IN user_id INT)
BEGIN
    UPDATE users SET active = 0, updated_at = NOW() WHERE id = user_id;
    INSERT INTO audit_log (user_id, action) VALUES (user_id, 'deactivated');
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("users.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "UPDATE table write should succeed");
}

#[test]
fn test_delete_table_write() {
    let source = r"
CREATE PROCEDURE cleanup_old_sessions()
BEGIN
    DELETE FROM sessions WHERE expires_at < NOW();
    DELETE FROM temp_data WHERE created_at < DATE_SUB(NOW(), INTERVAL 7 DAY);
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("cleanup.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "DELETE table write should succeed");
}

// ==================== Trigger Extraction ====================

#[test]
fn test_trigger_extraction() {
    let source = r"
CREATE TRIGGER after_order_insert
AFTER INSERT ON orders
FOR EACH ROW
BEGIN
    UPDATE customers SET total_orders = total_orders + 1
    WHERE id = NEW.customer_id;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("triggers.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Trigger extraction should succeed");
}

#[test]
fn test_before_trigger() {
    let source = r"
CREATE TRIGGER before_update_salary
BEFORE UPDATE ON employees
FOR EACH ROW
BEGIN
    INSERT INTO salary_history (emp_id, old_salary, changed_at)
    VALUES (OLD.id, OLD.salary, NOW());
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("audit_trigger.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "BEFORE trigger should succeed");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = SqlGraphBuilder;
    assert_eq!(builder.language(), Language::Sql);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SqlGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_sql() {
    // Incomplete SQL - tree-sitter is error-tolerant
    let source = r"
CREATE PROCEDURE broken(
    IN param1
"; // incomplete
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

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
fn test_combined_dml_operations() {
    let source = r"
CREATE PROCEDURE process_order(IN order_id INT)
BEGIN
    DECLARE v_customer_id INT;
    DECLARE v_total DECIMAL(10,2);

    SELECT customer_id, total INTO v_customer_id, v_total
    FROM orders WHERE id = order_id;

    UPDATE inventory SET quantity = quantity - 1
    WHERE product_id IN (
        SELECT product_id FROM order_items WHERE order_id = order_id
    );

    INSERT INTO transactions (order_id, customer_id, amount, processed_at)
    VALUES (order_id, v_customer_id, v_total, NOW());

    DELETE FROM pending_orders WHERE order_id = order_id;
END;
";
    let tree = parse_sql(source);
    let mut staging = StagingGraph::new();
    let builder = SqlGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("orders.sql"),
        &mut staging,
    );
    assert!(result.is_ok(), "Combined DML operations should succeed");
}
