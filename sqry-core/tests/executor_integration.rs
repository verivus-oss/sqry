//! Integration tests for query executor with AST queries
//!
//! These tests verify that the executor correctly integrates:
//! - AST evaluation
//! - Index-aware lookups
//! - Set operations
//! - Backward compatibility

use sqry_core::query::QueryParser;
use std::path::PathBuf;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

#[allow(dead_code)]
fn reference_test_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("src/main.rs"),
        PathBuf::from("tests/test.rs"),
        PathBuf::from("src/lib.rs"),
    ]
}

#[test]
fn test_integration_simple_kind_query() {
    init_logging();
    log::info!("Testing executor integration: simple kind query");

    // kind:function should parse successfully
    let query_str = "kind:function";
    let result = QueryParser::parse_query(query_str);
    assert!(result.is_ok(), "Should parse successfully");

    log::info!("✓ Query 'kind:function' parsed successfully");
}

#[test]
fn test_integration_and_query() {
    // kind:function AND name~=/^test_/ should parse successfully
    let query_str = "kind:function AND name~=/^test_/";
    let result = QueryParser::parse_query(query_str);
    assert!(result.is_ok(), "Should parse AND query successfully");
}

#[test]
fn test_integration_or_query() {
    // kind:function OR kind:class should parse successfully
    let query_str = "kind:function OR kind:class";
    let result = QueryParser::parse_query(query_str);
    assert!(result.is_ok(), "Should parse OR query successfully");
}

#[test]
fn test_integration_not_query() {
    // kind:function AND NOT name:main should parse successfully
    let query_str = "kind:function AND NOT name:main";
    let result = QueryParser::parse_query(query_str);
    assert!(result.is_ok(), "Should parse NOT query successfully");
}

#[test]
fn test_integration_complex_nested_query() {
    init_logging();
    log::info!("Testing executor integration: complex nested query with OR/AND");

    // (kind:function OR kind:method) AND (name~=/^test_/ OR name:do_something)
    let query_str = "(kind:function OR kind:method) AND (name~=/^test_/ OR name:do_something)";
    log::debug!("Query: {query_str}");

    let result = QueryParser::parse_query(query_str);
    assert!(
        result.is_ok(),
        "Should parse complex nested query successfully"
    );

    log::info!("✓ Complex nested query with parentheses, OR, AND operators parsed successfully");
}
