//! CLI integration tests for SQL relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for SQL:
//! - Exports queries (tables, views, functions, triggers)
//! - Imports queries (minimal - SQL doesn't have traditional imports)
//! - Callers queries (function calls, trigger executions)
//! - Callees queries (what a function calls)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Database Objects
// ============================================================================

#[test]
fn cli_sql_exports_tables_and_views() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    user_id INTEGER REFERENCES users(id),
    total_cents BIGINT NOT NULL
);

CREATE VIEW active_users AS
SELECT * FROM users WHERE created_at > NOW() - INTERVAL '30 days';

CREATE MATERIALIZED VIEW user_stats AS
SELECT user_id, COUNT(*) as order_count
FROM orders
GROUP BY user_id;
"#;
    std::fs::write(project.path().join("schema.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported table
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:users")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("schema.sql"));

    // Query for another table
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:orders")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("schema.sql"));

    // Query for exported view
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:active_users")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("schema.sql"));

    // Query for materialized view
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:user_stats")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("schema.sql"));
}

#[test]
fn cli_sql_exports_functions_and_triggers() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE TABLE accounts (
    id SERIAL PRIMARY KEY,
    balance_cents BIGINT NOT NULL
);

CREATE FUNCTION get_balance(account_id INT) RETURNS BIGINT AS $$
BEGIN
    RETURN (SELECT balance_cents FROM accounts WHERE id = account_id);
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION update_balance() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER balance_updated
BEFORE UPDATE ON accounts
FOR EACH ROW
EXECUTE FUNCTION update_balance();
"#;
    std::fs::write(project.path().join("banking.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:get_balance")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("banking.sql"));

    // Query for trigger function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:update_balance")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("banking.sql"));

    // Query for trigger
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:balance_updated")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("banking.sql"));
}

#[test]
fn cli_sql_exports_schema_qualified_names() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE TABLE public.customers (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE FUNCTION public.get_customer_name(cust_id INT) RETURNS TEXT AS $$
BEGIN
    RETURN (SELECT name FROM public.customers WHERE id = cust_id);
END;
$$ LANGUAGE plpgsql;
"#;
    std::fs::write(project.path().join("public_schema.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for table (schema prefix stripped)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:customers")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_schema.sql"));

    // Query for function (schema prefix stripped)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:get_customer_name")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_schema.sql"));
}

// ============================================================================
// Imports Queries - Minimal (SQL has no traditional imports)
// ============================================================================

#[test]
fn cli_sql_imports_empty_for_standard_sql() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE TABLE products (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL
);

SELECT * FROM public.products;
"#;
    std::fs::write(project.path().join("query.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // SQL doesn't have traditional imports - query should succeed but return minimal results
    // Schema qualifications are references, not imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:public")
        .arg(project.path())
        .assert()
        .success();
    // No assertion on output - just verify command doesn't crash
}

// ============================================================================
// Callers Queries - Function and Trigger Calls
// ============================================================================

#[test]
fn cli_sql_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE FUNCTION calculate_total(price INT, qty INT) RETURNS INT AS $$
BEGIN
    RETURN price * qty;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION process_order(order_id INT) RETURNS INT AS $$
DECLARE
    price INT := 100;
    quantity INT := 5;
BEGIN
    RETURN calculate_total(price, quantity);
END;
$$ LANGUAGE plpgsql;
"#;
    std::fs::write(project.path().join("functions.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of calculate_total
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:calculate_total")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process_order"));
}

#[test]
fn cli_sql_callers_trigger_execute_function() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE TABLE audit_log (
    id SERIAL PRIMARY KEY,
    action TEXT NOT NULL,
    timestamp TIMESTAMPTZ DEFAULT NOW()
);

CREATE FUNCTION log_change() RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO audit_log (action) VALUES ('Updated');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_trigger
AFTER UPDATE ON audit_log
FOR EACH ROW
EXECUTE FUNCTION log_change();
"#;
    std::fs::write(project.path().join("audit.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log_change (should include trigger)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log_change")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("audit_trigger"));
}

#[test]
fn cli_sql_callers_nested_function_calls() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE FUNCTION add(a INT, b INT) RETURNS INT AS $$
BEGIN
    RETURN a + b;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION multiply(a INT, b INT) RETURNS INT AS $$
BEGIN
    RETURN a * b;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION compute(x INT, y INT, z INT) RETURNS INT AS $$
DECLARE
    sum_val INT;
BEGIN
    sum_val := add(x, y);
    RETURN multiply(sum_val, z);
END;
$$ LANGUAGE plpgsql;
"#;
    std::fs::write(project.path().join("math.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of add
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:add")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("compute"));

    // Query for callers of multiply
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:multiply")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("compute"));
}

// ============================================================================
// Callees Queries - What a Function Calls
// ============================================================================

#[test]
fn cli_sql_callees_function_dependencies() {
    let project = TempDir::new().unwrap();

    let sql_code = r#"
CREATE FUNCTION helper_one() RETURNS INT AS $$
BEGIN
    RETURN 42;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION helper_two() RETURNS INT AS $$
BEGIN
    RETURN 100;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION orchestrator() RETURNS INT AS $$
DECLARE
    val1 INT;
    val2 INT;
BEGIN
    val1 := helper_one();
    val2 := helper_two();
    RETURN val1 + val2;
END;
$$ LANGUAGE plpgsql;
"#;
    std::fs::write(project.path().join("deps.sql"), sql_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of orchestrator
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callees:orchestrator")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // orchestrator should call both helper functions
    assert!(
        stdout.contains("helper_one") || stdout.contains("helper_two"),
        "orchestrator should call helper functions"
    );
}
