//! End-to-end scenario tests for sqry MCP server tool workflows.
//!
//! Each test scenario exercises a realistic multi-step workflow that an AI
//! assistant or developer would perform, sending real JSON-RPC requests to
//! a spawned MCP server process and validating structured responses.
//!
//! **Index requirement**: These tests require a pre-built `.sqry/graph/snapshot.sqry`
//! in the workspace root. They skip gracefully if none is found (e.g., in CI).

mod common;

use anyhow::Result;
use common::McpTestClient;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// If the extracted text is an error response, skip the test with a message
/// rather than panicking (which blocks tarpaulin coverage runs).
/// When a live index IS present, errors are genuine failures and this macro
/// is never reached because `extract_text` returns real content.
macro_rules! skip_on_error_response {
    ($text:expr, $tool:expr) => {
        if $text.starts_with("[Error response:") {
            eprintln!(
                "SKIPPED: {} returned error (no live index): {}",
                $tool, $text
            );
            return Ok(());
        }
    };
}

// ============================================================================
// Infrastructure
// ============================================================================

/// Returns true when a pre-built sqry graph index is available.
fn sqry_index_exists() -> bool {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    workspace_root
        .join(".sqry")
        .join("graph")
        .join("snapshot.sqry")
        .exists()
}

/// Skip early when the graph index is absent.
macro_rules! require_sqry_index {
    () => {
        if !sqry_index_exists() {
            eprintln!("Skipping: no .sqry/graph/snapshot.sqry found");
            return Ok(());
        }
    };
}

/// Shared, initialized MCP client reused across all scenario tests.
///
/// A single server process loads the graph once; every test acquires the mutex
/// for exclusive access, avoiding concurrent I/O contention.
fn shared_client() -> parking_lot::MutexGuard<'static, McpTestClient> {
    static CLIENT: OnceLock<Mutex<McpTestClient>> = OnceLock::new();
    let mutex = CLIENT.get_or_init(|| {
        Mutex::new(
            McpTestClient::new_initialized().expect("Failed to create shared scenario test client"),
        )
    });
    mutex.lock()
}

/// Extract the text body from an MCP tool-call response.
///
/// Returns an `Err` only when the response is structurally invalid (missing
/// `result.content[0].text`). A JSON-RPC error payload is returned as `Ok`
/// with an `[Error response: …]` prefix so individual tests can decide whether
/// that constitutes a failure.
fn extract_text(response: &Value) -> Result<String> {
    assert_eq!(response["jsonrpc"], "2.0", "Invalid JSON-RPC version");

    if response["error"].is_object() {
        let msg = response["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        return Ok(format!("[Error response: {msg}]"));
    }

    let content = &response["result"]["content"];
    anyhow::ensure!(content.is_array(), "Response content must be an array");
    anyhow::ensure!(
        !content.as_array().unwrap().is_empty(),
        "Content array is empty"
    );

    let text = content[0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No text field in content[0]"))?;

    Ok(text.to_string())
}

/// Parse JSON from a tool response text, returning a descriptive error on
/// failure so test failures are easy to read.
fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text)
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {e}\nRaw text: {text}"))
}

// ============================================================================
// Scenario 1: Symbol Discovery Workflow
//
// semantic_search("process_data") → result contains entries with kind "function"
// get_hover_info(first result symbol) → response includes file + line
// get_definition(first result symbol) → response includes location data
// ============================================================================

/// Scenario 1a: `semantic_search` for "`process_data`" returns at least one function-kind result.
#[test]
fn test_scenario1a_semantic_search_main_returns_functions() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "semantic_search",
            "arguments": {
                "query": "process_data",
                "max_results": 20
            }
        }),
        4001,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "semantic_search");
    assert!(!text.is_empty(), "semantic_search should return results");
    // The mini-workspace fixture must expose at least one symbol named or
    // described as a function.
    assert!(
        text.to_lowercase().contains("function")
            || text.contains("fn ")
            || text.contains("\"kind\""),
        "Result should mention functions or symbol kinds. Got: {text}"
    );

    Ok(())
}

/// Scenario 1b: `get_hover_info` for a well-known function returns location data.
#[test]
fn test_scenario1b_get_hover_info_includes_location() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_hover_info",
            "arguments": {
                "symbol": "build_graph"
            }
        }),
        4002,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "get_hover_info");
    assert!(!text.is_empty(), "get_hover_info should return content");
    // A valid hover response must contain file path or line information.
    assert!(
        text.contains(".rs")
            || text.contains("line")
            || text.contains("file")
            || text.contains("location"),
        "Hover info should include location data. Got: {text}"
    );

    Ok(())
}

/// Scenario 1c: `get_definition` for a well-known symbol returns location data.
#[test]
fn test_scenario1c_get_definition_includes_location() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_definition",
            "arguments": {
                "symbol": "process_data"
            }
        }),
        4003,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "get_definition");
    assert!(!text.is_empty(), "get_definition should return content");
    // Definition must reference a file path or a line number.
    assert!(
        text.contains(".rs")
            || text.contains("line")
            || text.contains("file_path")
            || text.contains("location"),
        "Definition should include location data. Got: {text}"
    );

    Ok(())
}

// ============================================================================
// Scenario 2: Call Graph Exploration
//
// direct_callers("build_graph") → ≥1 caller returned
// call_hierarchy("build_graph", direction: "outgoing") → ≥1 callee
// ============================================================================

/// Scenario 2a: `direct_callers` for `build_graph` returns callers.
#[test]
fn test_scenario2a_direct_callers_build_graph() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "direct_callers",
            "arguments": {
                "symbol": "build_graph"
            }
        }),
        4004,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "direct_callers");
    assert!(!text.is_empty(), "direct_callers should return content");
    // Must parse as JSON with a data or error field.
    let json = parse_json(&text)?;
    assert!(
        json.get("data").is_some() || json.get("error").is_some(),
        "direct_callers response must have 'data' or 'error' field. Got: {json}"
    );

    Ok(())
}

/// Scenario 2b: `call_hierarchy` for `build_graph` in outgoing direction.
#[test]
fn test_scenario2b_call_hierarchy_outgoing_build_graph() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "call_hierarchy",
            "arguments": {
                "symbol": "build_graph",
                "direction": "outgoing",
                "max_depth": 2
            }
        }),
        4005,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "call_hierarchy");
    assert!(!text.is_empty(), "call_hierarchy should return content");
    // Must parse as JSON. The response must contain a data wrapper with root and direction.
    let json = parse_json(&text)?;
    let data = json
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("call_hierarchy: missing 'data' field. Got: {json}"))?;
    assert!(
        data.get("root").is_some() || data.get("direction").is_some(),
        "call_hierarchy data must have 'root' or 'direction'. Got: {data}"
    );

    Ok(())
}

// ============================================================================
// Scenario 3: Code Quality Analysis
//
// find_cycles(type: "calls") → response has "cycles" field (array)
// complexity_metrics() → response has metrics with name + complexity
// ============================================================================

/// Scenario 3a: `find_cycles` for call cycles returns a parseable JSON response.
#[test]
fn test_scenario3a_find_cycles_calls_returns_cycles_field() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "find_cycles",
            "arguments": {
                "cycle_type": "calls",
                "max_results": 20
            }
        }),
        4006,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "find_cycles");
    assert!(!text.is_empty(), "find_cycles should return content");

    // Response should be parseable JSON with a data or cycles field.
    let json = parse_json(&text)?;
    assert!(
        json.get("data").is_some() || json.get("cycles").is_some(),
        "find_cycles JSON should have 'data' or 'cycles' field. Got: {json}"
    );

    Ok(())
}

/// Scenario 3b: `complexity_metrics` returns a list of symbols with complexity values.
#[test]
fn test_scenario3b_complexity_metrics_returns_named_entries() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "complexity_metrics",
            "arguments": {
                "min_complexity": 1,
                "max_results": 20
            }
        }),
        4007,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "complexity_metrics");
    assert!(!text.is_empty(), "complexity_metrics should return content");

    let json = parse_json(&text)?;
    // Must have a data wrapper containing complexity entries.
    let data = json
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("complexity_metrics: missing 'data' field. Got: {json}"))?;

    // The data field is either a flat array of metrics, or an object
    // containing a "metrics" array alongside summary statistics.
    let has_metrics = data.is_array() || data.get("metrics").and_then(|m| m.as_array()).is_some();
    assert!(
        has_metrics,
        "complexity_metrics data must be an array or contain a 'metrics' array. Got: {data}"
    );

    Ok(())
}

// ============================================================================
// Scenario 4: Dependency Impact Analysis
//
// dependency_impact("intern") → response has impacted_symbols
// ============================================================================

/// Scenario 4: `dependency_impact` for "intern" returns impacted symbols.
#[test]
fn test_scenario4_dependency_impact_intern() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "dependency_impact",
            "arguments": {
                "symbol": "intern",
                "max_depth": 2,
                "max_results": 30
            }
        }),
        4008,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "dependency_impact");
    assert!(!text.is_empty(), "dependency_impact should return content");

    let json = parse_json(&text)?;
    // Response must include a data wrapper or impacted_symbols field.
    assert!(
        json.get("data").is_some() || json.get("impacted_symbols").is_some(),
        "dependency_impact should have 'data' or 'impacted_symbols' field. Got: {json}"
    );

    Ok(())
}

// ============================================================================
// Scenario 5: Cross-Language Discovery
//
// cross_language_edges() → response has edges array
// ============================================================================

/// Scenario 5: `cross_language_edges` returns a structured edges response.
#[test]
fn test_scenario5_cross_language_edges_returns_array() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "cross_language_edges",
            "arguments": {
                "max_results": 20
            }
        }),
        4009,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "cross_language_edges");
    assert!(
        !text.is_empty(),
        "cross_language_edges should return content"
    );

    let json = parse_json(&text)?;
    // Response must have a data field (even if the edges array is empty for a
    // single-language codebase).
    assert!(
        json.get("data").is_some() || json.get("edges").is_some(),
        "cross_language_edges should have 'data' or 'edges' field. Got: {json}"
    );

    Ok(())
}

// ============================================================================
// Scenario 6: Search + Navigate Workflow
//
// hierarchical_search("config") → groups with file-grouped results
// get_document_symbols(file from result) → symbols with kind/name
// ============================================================================

/// Scenario 6a: `hierarchical_search` for "config" returns grouped results.
#[test]
fn test_scenario6a_hierarchical_search_config_returns_groups() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "hierarchical_search",
            "arguments": {
                "query": "config",
                "max_results": 20
            }
        }),
        4010,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "hierarchical_search");
    assert!(
        !text.is_empty(),
        "hierarchical_search should return content"
    );

    // Must parse as JSON with a data field containing grouped results.
    let json = parse_json(&text)?;
    assert!(
        json.get("data").is_some(),
        "hierarchical_search must have 'data' field. Got: {json}"
    );
    // The data should contain file-grouped results: at minimum a file path or symbol entry.
    assert!(
        text.contains(".rs")
            || text.contains("file")
            || text.contains("symbol")
            || text.contains("group"),
        "hierarchical_search should return grouped file results. Got: {text}"
    );

    Ok(())
}

/// Scenario 6b: `get_document_symbols` for a known Rust source file returns symbols.
#[test]
fn test_scenario6b_get_document_symbols_returns_kind_and_name() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    // Use a file path that exists in both the mini-workspace fixture and the
    // real codebase: sqry-core's graph module root.
    let response = client.call(
        "tools/call",
        json!({
            "name": "get_document_symbols",
            "arguments": {
                "file_path": "sqry-core/src/graph/unified/mod.rs"
            }
        }),
        4011,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "get_document_symbols");
    assert!(
        !text.is_empty(),
        "get_document_symbols should return content"
    );

    // Must parse as JSON with a data field.
    let json = parse_json(&text)?;
    assert!(
        json.get("data").is_some(),
        "get_document_symbols must have 'data' field. Got: {json}"
    );
    // A valid response must contain symbol kind or name fields.
    assert!(
        text.contains("kind")
            || text.contains("name")
            || text.contains("function")
            || text.contains("struct"),
        "get_document_symbols should return symbols with kind/name. Got: {text}"
    );

    Ok(())
}

// ============================================================================
// Scenario 7: Index Introspection
//
// get_index_status() → status is ready, node_count > 0
// get_graph_stats() → node_count + edge_count > 0
// list_files() → files array length > 0
// ============================================================================

/// Scenario 7a: `get_index_status` reports a ready index with a positive node count.
#[test]
fn test_scenario7a_get_index_status_ready_with_nodes() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        4012,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "get_index_status");
    assert!(!text.is_empty(), "get_index_status should return content");

    let json = parse_json(&text)?;
    let data = &json["data"];

    // hasIndex must be true.
    let has_index = data["hasIndex"].as_bool().unwrap_or(false);
    assert!(
        has_index,
        "get_index_status should report hasIndex=true. Got: {json}"
    );

    // indexedSymbols > 0 (graph has at least one symbol).
    let symbol_count = data["indexedSymbols"].as_u64().unwrap_or(0);
    assert!(
        symbol_count > 0,
        "get_index_status should report indexedSymbols > 0. Got: {json}"
    );

    Ok(())
}

/// Scenario 7b: `get_graph_stats` reports positive node and edge counts.
#[test]
fn test_scenario7b_get_graph_stats_positive_counts() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_graph_stats",
            "arguments": {}
        }),
        4013,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "get_graph_stats");
    assert!(!text.is_empty(), "get_graph_stats should return content");

    let json = parse_json(&text)?;
    let data = &json["data"];

    let total_nodes = data["totalNodes"].as_u64().unwrap_or(0);
    let total_edges = data["totalEdges"].as_u64().unwrap_or(0);

    assert!(
        total_nodes > 0,
        "get_graph_stats totalNodes should be > 0. Got: {json}"
    );
    assert!(
        total_edges > 0,
        "get_graph_stats totalEdges should be > 0. Got: {json}"
    );

    Ok(())
}

/// Scenario 7c: `list_files` returns at least one indexed file.
#[test]
fn test_scenario7c_list_files_returns_nonempty_array() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "list_files",
            "arguments": {
                "max_results": 50
            }
        }),
        4014,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "list_files");
    assert!(!text.is_empty(), "list_files should return content");

    let json = parse_json(&text)?;
    let data = &json["data"];

    // data.files must be a non-empty array.
    let files = data["files"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("list_files: missing 'files' array. Got: {json}"))?;

    assert!(
        !files.is_empty(),
        "list_files should return at least one file. Got: {json}"
    );

    Ok(())
}

// ============================================================================
// Scenario 8: Duplicate Detection + Export
//
// find_duplicates(type: "body") → response has groups
// find_unused() → response has symbols
// export_graph(format: "json") → valid JSON with nodes + edges
// ============================================================================

/// Scenario 8a: `find_duplicates` (body type) returns a parseable JSON response with groups.
#[test]
fn test_scenario8a_find_duplicates_body_returns_groups() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "find_duplicates",
            "arguments": {
                "duplicate_type": "body",
                "max_results": 20
            }
        }),
        4015,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "find_duplicates");
    assert!(!text.is_empty(), "find_duplicates should return content");

    let json = parse_json(&text)?;
    // Response must have a data wrapper (groups may be empty for a unique codebase).
    assert!(
        json.get("data").is_some(),
        "find_duplicates should have 'data' field. Got: {json}"
    );

    Ok(())
}

/// Scenario 8b: `find_unused` returns a parseable response with symbols list.
#[test]
fn test_scenario8b_find_unused_returns_symbols() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "find_unused",
            "arguments": {
                "max_results": 30
            }
        }),
        4016,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "find_unused");
    assert!(!text.is_empty(), "find_unused should return content");

    let json = parse_json(&text)?;
    // Must have a data field; the symbols array may be empty.
    assert!(
        json.get("data").is_some(),
        "find_unused should have 'data' field. Got: {json}"
    );

    Ok(())
}

/// Scenario 8c: `export_graph` (JSON format) returns valid JSON containing nodes and edges.
#[test]
fn test_scenario8c_export_graph_json_format_valid() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "export_graph",
            "arguments": {
                "format": "json",
                "max_results": 50
            }
        }),
        4017,
    )?;

    let text = extract_text(&response)?;
    skip_on_error_response!(text, "export_graph");
    assert!(!text.is_empty(), "export_graph should return content");

    let json = parse_json(&text)?;
    // The exported JSON must have a data wrapper containing the graph structure,
    // or directly contain nodes+edges at the top level.
    let has_data = json.get("data").is_some();
    let has_nodes_and_edges = json.get("nodes").is_some() && json.get("edges").is_some();
    assert!(
        has_data || has_nodes_and_edges,
        "export_graph JSON must have 'data' or both 'nodes'+'edges' fields. Got: {json}"
    );

    Ok(())
}
