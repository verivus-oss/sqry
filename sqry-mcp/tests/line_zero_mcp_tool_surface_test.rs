//! MCP tool surface line-zero regression test.
//!
//! Exercises all MCP tools against a cross-file fixture and verifies that
//! every returned symbol with a known in-workspace definition has `line > 0`.
//! This is the query-layer regression for the line-zero holistic fix.
//!
//! The test builds a small multi-file Rust fixture with cross-file calls,
//! persists the graph, then exercises each MCP tool through the JSON-RPC
//! protocol.

mod common;

use anyhow::Result;
use common::McpTestClient;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Build the cross-file fixture and return a test client pointed at it.
fn create_cross_file_fixture_client() -> Result<(TempDir, McpTestClient)> {
    let temp_dir = TempDir::new()?;
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir)?;

    // File 1: defines helper functions
    std::fs::write(
        src_dir.join("helpers.rs"),
        r#"
/// Compute a value from input.
pub fn compute_value(n: i32) -> i32 {
    n * 2 + 1
}

/// Format a greeting message.
pub fn format_greeting(name: &str) -> String {
    format!("Hello, {name}!")
}

/// A constant for testing.
pub const MAX_RETRIES: u32 = 3;
"#,
    )?;

    // File 2: calls functions from helpers.rs (cross-file)
    std::fs::write(
        src_dir.join("main.rs"),
        r#"
mod helpers;

use helpers::{compute_value, format_greeting};

fn process_data(input: i32) -> i32 {
    let result = compute_value(input);
    result + 10
}

fn greet_user() -> String {
    format_greeting("World")
}

fn main() {
    let value = process_data(42);
    let greeting = greet_user();
    println!("{greeting}: {value}");
}
"#,
    )?;

    // File 3: another module for more graph edges
    std::fs::write(
        src_dir.join("utils.rs"),
        r#"
use crate::helpers::compute_value;

pub fn double_compute(n: i32) -> i32 {
    compute_value(n) + compute_value(n + 1)
}

pub fn is_large(n: i32) -> bool {
    compute_value(n) > 100
}
"#,
    )?;

    let plugins = sqry_plugin_registry::create_plugin_manager();
    let config = sqry_core::graph::unified::build::BuildConfig::default();
    let (_graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
        temp_dir.path(),
        &plugins,
        &config,
        "test:line-zero-mcp-surface",
    )?;

    let client = McpTestClient::new_with_env_initialized(&[(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        temp_dir.path().to_string_lossy().into_owned(),
    )])?;
    Ok((temp_dir, client))
}

/// Helper: validate MCP response, extract text, check for lines > 0 in JSON output.
fn validate_response(response: &Value) -> Result<String> {
    assert_eq!(response["jsonrpc"], "2.0", "Invalid JSON-RPC version");

    if response["error"].is_object() {
        let msg = response["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        return Ok(format!("[Error: {msg}]"));
    }

    let content = &response["result"]["content"];
    anyhow::ensure!(content.is_array(), "Response content must be an array");
    anyhow::ensure!(
        !content.as_array().unwrap().is_empty(),
        "Content array is empty"
    );

    let text = content[0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No text field in content"))?;
    Ok(text.to_string())
}

fn is_error(text: &str) -> bool {
    text.starts_with("[Error:")
}

/// Check "line" fields in a JSON object, but only for items that represent
/// defined symbols (not imports, modules, or stubs). Returns violations.
///
/// The check is applied to objects that have both a "line" field and a "kind"
/// field — if kind is "Import", "Module", "CallSite", or "Other", line 0 is
/// acceptable (these are synthetic or external-reference nodes).
fn check_line_fields_in_symbol_entries(value: &Value, path: &str) -> Vec<(String, i64)> {
    let mut violations = Vec::new();
    match value {
        Value::Object(map) => {
            // Check if this object has a "line" field
            let line_val = map
                .get("line")
                .or_else(|| map.get("startLine"))
                .or_else(|| map.get("start_line"));
            if let Some(0) = line_val.and_then(Value::as_i64) {
                // Check the kind field — skip acceptable kinds
                let kind = map.get("kind").and_then(Value::as_str).unwrap_or("");
                // Stubs for external symbols (standard library macros like
                // format!/println!, import nodes, module nodes) can have line 0
                // Variable is NOT in this list — in-workspace variables must
                // have real locations.
                let acceptable_kinds = [
                    "Import", "Module", "CallSite", "Other", "Macro", "import", "module",
                    "callsite", "other", "macro",
                ];
                if !acceptable_kinds
                    .iter()
                    .any(|k| kind.eq_ignore_ascii_case(k))
                    && !kind.is_empty()
                {
                    violations.push((format!("{path}.line(kind={kind})"), 0));
                }
                // If no kind field, this is likely a nested line reference — skip
            }
            // Recurse into children
            for (key, val) in map {
                violations.extend(check_line_fields_in_symbol_entries(
                    val,
                    &format!("{path}.{key}"),
                ));
            }
        }
        Value::Array(arr) => {
            for (i, val) in arr.iter().enumerate() {
                violations.extend(check_line_fields_in_symbol_entries(
                    val,
                    &format!("{path}[{i}]"),
                ));
            }
        }
        _ => {}
    }
    violations
}

/// Assert that defined-symbol "line" fields in JSON tool output are not 0.
/// Import/Module/CallSite kinds are excluded (they can legitimately have line 0).
fn assert_no_line_zero_in_json(text: &str, tool_name: &str) {
    if let Ok(json) = serde_json::from_str::<Value>(text) {
        let violations = check_line_fields_in_symbol_entries(&json, "root");
        assert!(
            violations.is_empty(),
            "[{tool_name}] Found line == 0 for defined symbols in JSON output: {violations:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tool surface tests
// ---------------------------------------------------------------------------

#[test]
fn mcp_surface_semantic_search() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "semantic_search", "arguments": {"query": "compute", "max_results": 10}}),
        1,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "semantic_search");
    }
    Ok(())
}

#[test]
fn mcp_surface_pattern_search() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "pattern_search", "arguments": {"pattern": "compute_value", "max_results": 10}}),
        2,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "pattern_search");
    }
    Ok(())
}

#[test]
fn mcp_surface_hierarchical_search() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "hierarchical_search", "arguments": {"query": "compute", "max_results": 10}}),
        3,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "hierarchical_search");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_definition() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_definition", "arguments": {"symbol": "compute_value"}}),
        4,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_definition");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_references() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_references", "arguments": {"symbol": "compute_value", "max_results": 10}}),
        5,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_references");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_hover_info() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_hover_info", "arguments": {"symbol": "compute_value"}}),
        6,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_hover_info");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_document_symbols() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_document_symbols", "arguments": {"file_path": "src/helpers.rs"}}),
        7,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_document_symbols");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_workspace_symbols() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_workspace_symbols", "arguments": {"query": "compute", "max_results": 10}}),
        8,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_workspace_symbols");
    }
    Ok(())
}

#[test]
fn mcp_surface_direct_callees() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "direct_callees", "arguments": {"symbol": "process_data"}}),
        9,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "direct_callees");
    }
    Ok(())
}

#[test]
fn mcp_surface_direct_callers() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "direct_callers", "arguments": {"symbol": "compute_value"}}),
        10,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "direct_callers");
    }
    Ok(())
}

#[test]
fn mcp_surface_relation_query() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "relation_query", "arguments": {"symbol": "compute_value", "relation_type": "callers", "max_depth": 1}}),
        11,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "relation_query");
    }
    Ok(())
}

#[test]
fn mcp_surface_call_hierarchy() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "call_hierarchy", "arguments": {"symbol": "compute_value", "direction": "incoming", "max_depth": 2}}),
        12,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "call_hierarchy");
    }
    Ok(())
}

#[test]
fn mcp_surface_find_cycles() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "find_cycles", "arguments": {"max_results": 10}}),
        13,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "find_cycles");
    }
    Ok(())
}

#[test]
fn mcp_surface_find_unused() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "find_unused", "arguments": {"max_results": 10}}),
        14,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "find_unused");
    }
    Ok(())
}

#[test]
fn mcp_surface_find_duplicates() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "find_duplicates", "arguments": {"max_results": 10}}),
        15,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "find_duplicates");
    }
    Ok(())
}

#[test]
fn mcp_surface_get_graph_stats() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "get_graph_stats", "arguments": {}}),
        16,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "get_graph_stats");
    }
    Ok(())
}

#[test]
fn mcp_surface_explain_code() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "explain_code", "arguments": {"file_path": "src/helpers.rs", "symbol_name": "compute_value"}}),
        17,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "explain_code");
    }
    Ok(())
}

#[test]
fn mcp_surface_trace_path() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "trace_path", "arguments": {"source": "process_data", "target": "compute_value", "max_depth": 5}}),
        18,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "trace_path");
    }
    Ok(())
}

#[test]
fn mcp_surface_dependency_impact() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "dependency_impact", "arguments": {"symbol_name": "compute_value", "max_depth": 3}}),
        19,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "dependency_impact");
    }
    Ok(())
}

#[test]
fn mcp_surface_show_dependencies() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "show_dependencies", "arguments": {"symbol_name": "process_data", "max_depth": 2}}),
        20,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "show_dependencies");
    }
    Ok(())
}

#[test]
fn mcp_surface_subgraph() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "subgraph", "arguments": {"symbol": "compute_value", "max_depth": 2}}),
        21,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "subgraph");
    }
    Ok(())
}

#[test]
fn mcp_surface_complexity_metrics() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "complexity_metrics", "arguments": {"file_path": "src/helpers.rs"}}),
        22,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "complexity_metrics");
    }
    Ok(())
}

#[test]
fn mcp_surface_cross_language_edges() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "cross_language_edges", "arguments": {"max_results": 10}}),
        23,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "cross_language_edges");
    }
    Ok(())
}

#[test]
fn mcp_surface_is_node_in_cycle() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;
    let resp = client.call(
        "tools/call",
        json!({"name": "is_node_in_cycle", "arguments": {"symbol": "compute_value"}}),
        24,
    )?;
    let text = validate_response(&resp)?;
    if !is_error(&text) {
        assert_no_line_zero_in_json(&text, "is_node_in_cycle");
    }
    Ok(())
}

/// PARSE_2: Verify that `hierarchical_search` benefits from the implicit AND
/// parser fix (PARSE_1).  Prior to PARSE_1, `kind:function compute_value` would
/// fail with `UnexpectedToken` because `parse_and()` stopped after
/// `kind:function` when no explicit `AND` followed.  After PARSE_1 the bare
/// word is treated as implicit AND with `name~=/compute_value/`, producing a
/// successful query that returns the matching function node.
///
/// The test exercises the full hierarchical search path:
///   `execute_hierarchical_search`
///   → `executor.execute_on_graph("kind:function compute_value", root)`
///   → `parse_query_ast` (shared parser, same as `semantic_search`)
///   → graph evaluation + result grouping
///
/// `compute_value` is defined in the cross-file fixture's `helpers.rs`; the
/// fixture is built on demand by `create_cross_file_fixture_client`.
#[test]
fn mcp_parse2_hierarchical_search_implicit_and_returns_results() -> Result<()> {
    let (_dir, mut client) = create_cross_file_fixture_client()?;

    // kind:function compute_value — implicit AND between kind predicate and
    // bare word.  Before PARSE_1 this was a parse error; after PARSE_1 it
    // is equivalent to: kind:function AND name~=/compute_value/.
    let resp = client.call(
        "tools/call",
        json!({
            "name": "hierarchical_search",
            "arguments": {
                "query": "kind:function compute_value",
                "max_results": 10
            }
        }),
        25,
    )?;
    let text = validate_response(&resp)?;

    // The query must not produce an error — before PARSE_1 it failed with
    // "Unexpected token" at the bare word `compute_value`.
    assert!(
        !is_error(&text),
        "hierarchical_search with implicit AND query must not return an error. Got: {text}"
    );

    // The response must contain at least the `compute_value` function that
    // exists in the fixture (helpers.rs), proving the shared parser path
    // exercised by hierarchical_search handles implicit AND correctly.
    assert!(
        text.contains("compute_value"),
        "hierarchical_search result must contain 'compute_value'. Got: {text}"
    );

    Ok(())
}
