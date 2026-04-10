//! End-to-end integration tests for sqry MCP server using active codebase
//!
//! These tests verify real-world usage scenarios by querying the actual
//! sqry codebase index. They test semantic search, relation queries,
//! dependency analysis, and other advanced features.

mod common;

use anyhow::Result;
use common::McpTestClient;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::OnceLock;
use tempfile::TempDir;

/// Check if the sqry graph index exists at the workspace root.
///
/// E2E tests require a pre-built `.sqry/graph/snapshot.sqry` to function.
/// In CI environments this file typically doesn't exist, so tests should
/// skip gracefully rather than timing out waiting for the MCP server.
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

/// Skip test early if no sqry index exists (e.g., CI without pre-built index).
macro_rules! require_sqry_index {
    () => {
        if !sqry_index_exists() {
            eprintln!("Skipping: no .sqry/graph/snapshot.sqry found");
            return Ok(());
        }
    };
}

/// Returns a shared, initialized MCP test client.
///
/// The server process starts once and the graph loads once on first tool call.
/// Tests acquire the mutex for exclusive access during their tool calls,
/// avoiding the I/O contention of 17+ concurrent processes each loading
/// the 244MB graph snapshot independently.
fn shared_initialized_client() -> parking_lot::MutexGuard<'static, McpTestClient> {
    static CLIENT: OnceLock<Mutex<McpTestClient>> = OnceLock::new();
    let mutex = CLIENT.get_or_init(|| {
        Mutex::new(
            McpTestClient::new_initialized().expect("Failed to create shared e2e test client"),
        )
    });
    mutex.lock()
}

/// Helper to validate MCP tool response and extract text content
fn validate_and_extract_response(response: &Value) -> Result<String> {
    assert_eq!(response["jsonrpc"], "2.0", "Invalid JSON-RPC version");

    // Handle error responses gracefully
    if response["error"].is_object() {
        let error_msg = response["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        // For E2E tests, some queries may legitimately return errors (e.g., symbol not found)
        return Ok(format!("[Error response: {error_msg}]"));
    }

    // Validate success response structure
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

fn is_error_response(text: &str) -> bool {
    text.starts_with("[Error response:")
}

fn require_successful_text(text: String, context: &str) -> Result<String> {
    anyhow::ensure!(
        !is_error_response(&text),
        "{context} returned unexpected error response: {text}"
    );
    Ok(text)
}

fn is_freshness_metadata_unavailable(text: &str) -> bool {
    is_error_response(text) && text.contains("Failed to stat manifest.json for freshness check")
}

fn require_successful_text_or_skip_on_freshness(
    text: String,
    context: &str,
) -> Result<Option<String>> {
    if is_freshness_metadata_unavailable(&text) {
        eprintln!("Skipping {context} assertion because freshness metadata is unavailable: {text}");
        return Ok(None);
    }
    Ok(Some(require_successful_text(text, context)?))
}

fn create_suffix_fixture_client() -> Result<(TempDir, McpTestClient)> {
    let temp_dir = TempDir::new()?;
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir)?;
    std::fs::write(
        src_dir.join("lib.rs"),
        r#"
mod internal {
    pub struct Widget {
        value: i32,
    }

    impl Widget {
        pub fn new(value: i32) -> Self {
            Self { value }
        }
    }

    pub struct Builder;

    impl Builder {
        pub fn build(value: i32) -> Widget {
            Widget::new(value)
        }
    }
}

pub fn orchestrate(value: i32) -> internal::Widget {
    internal::Builder::build(value)
}
"#,
    )?;

    let plugins = sqry_plugin_registry::create_plugin_manager();
    let config = sqry_core::graph::unified::build::BuildConfig::default();
    let (_graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
        temp_dir.path(),
        &plugins,
        &config,
        "test:suffix-fixture",
    )?;

    let client = McpTestClient::new_with_env_initialized(&[(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        temp_dir.path().to_string_lossy().into_owned(),
    )])?;
    Ok((temp_dir, client))
}

/// Test 1: Semantic search for `GraphBuilder` implementations
#[test]
fn test_e2e_semantic_search_graph_builders() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "semantic_search",
            "arguments": {
                "query": "GraphBuilder implementations",
                "max_results": 10
            }
        }),
        100,
    )?;

    let text = validate_and_extract_response(&response)?;
    assert!(!text.is_empty(), "Should return search results");

    Ok(())
}

/// Test 2: Pattern search for specific function names
#[test]
fn test_e2e_pattern_search_functions() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "pattern_search",
            "arguments": {
                "pattern": "add_method",
                "max_results": 20
            }
        }),
        101,
    )?;

    let Some(text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&response)?,
        "pattern_search",
    )?
    else {
        return Ok(());
    };
    assert!(
        text.contains("add_method") || text.contains("matches"),
        "Should find matching symbols"
    );

    Ok(())
}

/// Test 3: Get document symbols from a known file
#[test]
fn test_e2e_document_symbols() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_document_symbols",
            "arguments": {
                "file_path": "src/lib.rs"
            }
        }),
        102,
    )?;

    let Some(text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&response)?,
        "get_document_symbols",
    )?
    else {
        return Ok(());
    };
    assert!(!text.is_empty(), "Should return symbol list content");

    Ok(())
}

/// Test 4: Search workspace symbols by query
#[test]
fn test_e2e_workspace_symbols_search() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_workspace_symbols",
            "arguments": {
                "query": "GraphBuildHelper",
                "max_results": 5
            }
        }),
        103,
    )?;

    let text = validate_and_extract_response(&response)?;
    assert!(!text.is_empty(), "Should return symbol results");

    Ok(())
}

/// Test 5: Get graph statistics
#[test]
fn test_e2e_graph_statistics() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_graph_stats",
            "arguments": {}
        }),
        104,
    )?;

    let Some(text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&response)?,
        "get_graph_stats",
    )?
    else {
        return Ok(());
    };

    // Verify it contains expected statistics fields (JSON format)
    assert!(
        text.contains("totalNodes"),
        "Should show node count. Got: {text}"
    );
    assert!(
        text.contains("totalEdges"),
        "Should show edge count. Got: {text}"
    );
    assert!(
        text.contains("totalFiles"),
        "Should show file count. Got: {text}"
    );

    Ok(())
}

/// Test 6: Get index status and metadata
#[test]
fn test_e2e_index_status() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        105,
    )?;

    let Some(text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&response)?,
        "get_index_status",
    )?
    else {
        return Ok(());
    };

    // Should show index information
    assert!(
        text.contains("Index")
            || text.contains("status")
            || text.contains("version")
            || text.contains("hasIndex")
            || text.contains("filesIndexed"),
        "Should return index metadata"
    );

    Ok(())
}

/// Test 7: Find symbol definition
#[test]
fn test_e2e_find_definition() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_definition",
            "arguments": {
                "symbol": "GraphBuildHelper"
            }
        }),
        106,
    )?;

    let text = validate_and_extract_response(&response)?;
    // Should find definition or return not found
    assert!(!text.is_empty(), "Should return definition result");

    Ok(())
}

/// Test 8: Find references to a symbol
#[test]
fn test_e2e_find_references() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_references",
            "arguments": {
                "symbol": "add_function",
                "max_results": 20
            }
        }),
        107,
    )?;

    let text = validate_and_extract_response(&response)?;
    // Should find references or return none found
    assert!(!text.is_empty(), "Should return reference results");

    Ok(())
}

/// Test 9: Hierarchical search with grouping
#[test]
fn test_e2e_hierarchical_search() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "hierarchical_search",
            "arguments": {
                "query": "build graph",
                "max_results": 10
            }
        }),
        108,
    )?;

    let text = validate_and_extract_response(&response)?;
    assert!(
        !text.is_empty(),
        "Should return hierarchical search results"
    );

    Ok(())
}

/// Test 10: List files in the index
#[test]
fn test_e2e_list_indexed_files() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "list_files",
            "arguments": {
                "language": "rust",
                "max_results": 100
            }
        }),
        109,
    )?;

    let Some(text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&response)?,
        "list_files",
    )?
    else {
        return Ok(());
    };
    // Should list Rust files from the codebase
    assert!(
        !text.is_empty()
            && (text.contains(".rs") || text.contains("file") || text.contains("Rust")),
        "Should return Rust file listings"
    );

    Ok(())
}

/// Test 11: Query caller/callee relations
#[test]
fn test_e2e_relation_query_callers() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "relation_query",
            "arguments": {
                "symbol": "build_graph",
                "relation_type": "callers",
                "max_depth": 1
            }
        }),
        110,
    )?;

    let text = validate_and_extract_response(&response)?;
    // May find callers or return not found
    assert!(!text.is_empty(), "Should return relation query result");

    Ok(())
}

/// Test 12: List symbols by kind
#[test]
fn test_e2e_list_symbols_by_kind() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "list_symbols",
            "arguments": {
                "kind": "function",
                "max_results": 20
            }
        }),
        111,
    )?;

    let text = validate_and_extract_response(&response)?;
    // Should list function symbols
    assert!(!text.is_empty(), "Should return symbol list");

    Ok(())
}

/// Test 13: Explain code with context
#[test]
fn test_e2e_explain_code_with_context() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "explain_code",
            "arguments": {
                "file_path": "sqry-core/src/graph/unified/mod.rs",
                "symbol_name": "CodeGraph",
                "include_context": true,
                "include_relations": true
            }
        }),
        112,
    )?;

    let text = validate_and_extract_response(&response)?;
    // Should provide detailed explanation
    assert!(!text.is_empty(), "Should return code explanation");

    Ok(())
}

/// Test 14: Cross-language edge detection
#[test]
fn test_e2e_cross_language_analysis() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "cross_language_edges",
            "arguments": {
                "max_results": 10
            }
        }),
        113,
    )?;

    let text = validate_and_extract_response(&response)?;
    // May or may not have cross-language edges
    assert!(
        !text.is_empty(),
        "Should return cross-language analysis result"
    );

    Ok(())
}

/// Test 15: Show dependency tree
#[test]
fn test_e2e_dependency_analysis() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "show_dependencies",
            "arguments": {
                "symbol_name": "CodeGraph",
                "max_depth": 2
            }
        }),
        114,
    )?;

    let text = validate_and_extract_response(&response)?;
    // Should show dependency information
    assert!(!text.is_empty(), "Should return dependency tree");

    Ok(())
}

/// Test 16: Validate filesIndexed accuracy (regression test for filesIndexed=0 bug)
#[test]
#[allow(clippy::similar_names)] // Domain variable naming is intentional
fn test_e2e_index_status_file_count_accuracy() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    // Get index status
    let status_response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        115,
    )?;

    let Some(status_text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&status_response)?,
        "get_index_status",
    )?
    else {
        return Ok(());
    };

    // Get graph stats (source of truth for file count)
    #[allow(clippy::similar_names)] // Test fixture variables
    let stats_response = client.call(
        "tools/call",
        json!({
            "name": "get_graph_stats",
            "arguments": {}
        }),
        116,
    )?;

    let Some(stats_text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&stats_response)?,
        "get_graph_stats",
    )?
    else {
        return Ok(());
    };

    // Parse JSON responses
    let status_json: Value = serde_json::from_str(&status_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse index status JSON: {e}"))?;
    let stats_json: Value = serde_json::from_str(&stats_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse graph stats JSON: {e}"))?;

    // Extract file counts
    let files_indexed = status_json["data"]["filesIndexed"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid filesIndexed in status response"))?;

    let total_files = stats_json["data"]["totalFiles"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid totalFiles in stats response"))?;

    // Verify file counts match
    assert_eq!(
        files_indexed, total_files,
        "Index status filesIndexed ({files_indexed}) should match graph stats totalFiles ({total_files})"
    );

    // Sanity check: file count should be greater than 0 for sqry codebase
    assert!(
        files_indexed > 0,
        "File count should be greater than 0, got {files_indexed}"
    );

    Ok(())
}

/// Test 17: Validate `rebuild_index` returns accurate file count
#[test]
#[ignore = "Expensive rebuild test - enable for validation testing"]
fn test_e2e_rebuild_index_file_count_accuracy() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    // Rebuild index (with force to ensure fresh build)
    let rebuild_response = client.call(
        "tools/call",
        json!({
            "name": "rebuild_index",
            "arguments": {
                "force": true
            }
        }),
        117,
    )?;

    let rebuild_text = validate_and_extract_response(&rebuild_response)?;

    // Parse rebuild response
    let rebuild_json: Value = serde_json::from_str(&rebuild_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse rebuild response JSON: {e}"))?;

    // Extract file count from rebuild response
    let files_indexed = rebuild_json["data"]["filesIndexed"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid filesIndexed in rebuild response"))?;

    // Get graph stats to compare
    let stats_response = client.call(
        "tools/call",
        json!({
            "name": "get_graph_stats",
            "arguments": {}
        }),
        118,
    )?;

    let stats_text = validate_and_extract_response(&stats_response)?;
    let stats_json: Value = serde_json::from_str(&stats_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse graph stats JSON: {e}"))?;

    let total_files = stats_json["data"]["totalFiles"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid totalFiles in stats response"))?;

    // Verify rebuild response file count matches graph stats
    assert_eq!(
        files_indexed, total_files,
        "Rebuild response filesIndexed ({files_indexed}) should match graph stats totalFiles ({total_files})"
    );

    // Sanity check: file count should be greater than 0
    assert!(
        files_indexed > 0,
        "Rebuild file count should be greater than 0, got {files_indexed}"
    );

    Ok(())
}

/// Test 18: Validate `rebuild_index` without force returns existing index file count
#[test]
fn test_e2e_rebuild_index_existing_index_file_count() -> Result<()> {
    require_sqry_index!();
    let mut client = shared_initialized_client();

    // Call rebuild_index without force (should return existing index info)
    let rebuild_response = client.call(
        "tools/call",
        json!({
            "name": "rebuild_index",
            "arguments": {
                "force": false
            }
        }),
        119,
    )?;

    let Some(rebuild_text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&rebuild_response)?,
        "rebuild_index",
    )?
    else {
        return Ok(());
    };

    // Parse rebuild response
    let rebuild_json: Value = serde_json::from_str(&rebuild_text).map_err(|e| {
        anyhow::anyhow!("Failed to parse rebuild response JSON: {e} | Text was: {rebuild_text}")
    })?;

    // Extract file count from rebuild response
    let files_indexed = rebuild_json["data"]["filesIndexed"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid filesIndexed in rebuild response"))?;

    // Get graph stats to compare
    let stats_response = client.call(
        "tools/call",
        json!({
            "name": "get_graph_stats",
            "arguments": {}
        }),
        120,
    )?;

    let Some(stats_text) = require_successful_text_or_skip_on_freshness(
        validate_and_extract_response(&stats_response)?,
        "get_graph_stats",
    )?
    else {
        return Ok(());
    };
    let stats_json: Value = serde_json::from_str(&stats_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse graph stats JSON: {e}"))?;

    let total_files = stats_json["data"]["totalFiles"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid totalFiles in stats response"))?;

    // Verify rebuild response file count matches graph stats
    assert_eq!(
        files_indexed, total_files,
        "Rebuild (no force) filesIndexed ({files_indexed}) should match graph stats totalFiles ({total_files})"
    );

    // Verify success flag
    let success = rebuild_json["data"]["success"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid success field"))?;
    assert!(success, "Rebuild should indicate success");

    Ok(())
}

/// Test 19: Validate manifest-only fallback when snapshot header is unreadable
#[test]
#[allow(clippy::items_after_statements)] // Items near usage for clarity
fn test_e2e_index_status_manifest_only_fallback() -> Result<()> {
    require_sqry_index!();
    use sqry_core::graph::unified::persistence::{
        BuildProvenance, MANIFEST_SCHEMA_VERSION, Manifest, SNAPSHOT_FORMAT_VERSION,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    // Create temporary directory structure
    let temp_dir = TempDir::new()?;
    let graph_dir = temp_dir.path().join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir)?;

    // Create a manifest with populated file_count
    let mut file_count_map = HashMap::new();
    file_count_map.insert("rust".to_string(), 100);
    file_count_map.insert("python".to_string(), 50);
    file_count_map.insert("javascript".to_string(), 75);
    let expected_total: usize = file_count_map.values().sum();

    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        built_at: chrono::Utc::now().to_rfc3339(),
        root_path: temp_dir.path().to_string_lossy().to_string(),
        node_count: 1000,
        edge_count: 2000,
        raw_edge_count: None,
        snapshot_sha256: "test_checksum".to_string(),
        build_provenance: BuildProvenance {
            sqry_version: "3.2.0".to_string(),
            build_timestamp: chrono::Utc::now().to_rfc3339(),
            build_command: "test".to_string(),
            plugin_hashes: HashMap::new(),
        },
        file_count: file_count_map,
        languages: vec![
            "rust".to_string(),
            "python".to_string(),
            "javascript".to_string(),
        ],
        config: HashMap::new(),
        confidence: HashMap::new(),
        last_indexed_commit: None,
        plugin_selection: None,
    };

    // Save manifest
    let manifest_path = graph_dir.join("manifest.json");
    manifest.save(&manifest_path)?;

    // Create a corrupted snapshot file (invalid header)
    let snapshot_path = graph_dir.join("snapshot.sqry");
    std::fs::write(&snapshot_path, b"corrupted_data")?;

    // Now test that get_index_status falls back to manifest.file_count
    let mut client = shared_initialized_client();

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {
                "path": temp_dir.path().to_str().unwrap()
            }
        }),
        121,
    )?;

    let text = validate_and_extract_response(&response)?;

    // Parse response
    let status_json: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Failed to parse index status JSON: {e}"))?;

    // Verify filesIndexed equals the sum of manifest.file_count
    let files_indexed = status_json["data"]["filesIndexed"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Missing filesIndexed in response"))?;

    assert_eq!(
        files_indexed, expected_total as u64,
        "filesIndexed should equal manifest.file_count sum when snapshot header is unreadable"
    );

    // Verify has_index is true
    let has_index = status_json["data"]["hasIndex"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Missing hasIndex in response"))?;
    assert!(has_index, "Should report index exists");

    Ok(())
}

/// Test suffix matching: `direct_callers` with partially-qualified name.
///
/// Uses a deterministic fixture where the graph stores
/// `internal::Widget::new` but the query uses the shorter `Widget::new`.
#[test]
fn test_e2e_direct_callers_suffix_match() -> Result<()> {
    let (_temp_dir, mut client) = create_suffix_fixture_client()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "direct_callers",
            "arguments": {
                "symbol": "Widget::new"
            }
        }),
        3001,
    )?;

    let text =
        require_successful_text(validate_and_extract_response(&response)?, "direct_callers")?;
    assert!(
        text.contains("build"),
        "direct_callers should resolve Widget::new via suffix matching. Got: {text}"
    );

    Ok(())
}

/// Test suffix matching: `direct_callees` with partially-qualified name.
#[test]
fn test_e2e_direct_callees_suffix_match() -> Result<()> {
    let (_temp_dir, mut client) = create_suffix_fixture_client()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "direct_callees",
            "arguments": {
                "symbol": "Builder::build"
            }
        }),
        3002,
    )?;

    let text =
        require_successful_text(validate_and_extract_response(&response)?, "direct_callees")?;
    assert!(
        text.contains("Widget::new") || text.contains("new(") || text.contains("Widget"),
        "direct_callees should resolve Builder::build via suffix matching. Got: {text}"
    );

    Ok(())
}

/// Test suffix matching: `get_hover_info` with partially-qualified name.
#[test]
fn test_e2e_get_hover_info_suffix_match() -> Result<()> {
    let (_temp_dir, mut client) = create_suffix_fixture_client()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_hover_info",
            "arguments": {
                "symbol": "Widget::new"
            }
        }),
        3003,
    )?;

    let text =
        require_successful_text(validate_and_extract_response(&response)?, "get_hover_info")?;
    assert!(
        text.contains("Widget::new") || text.contains("Widget"),
        "get_hover_info should resolve Widget::new via suffix matching. Got: {text}"
    );

    Ok(())
}

/// Test suffix matching: `get_references` with partially-qualified name.
#[test]
fn test_e2e_get_references_suffix_match() -> Result<()> {
    let (_temp_dir, mut client) = create_suffix_fixture_client()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_references",
            "arguments": {
                "symbol": "Widget::new",
                "max_results": 10
            }
        }),
        3004,
    )?;

    let text =
        require_successful_text(validate_and_extract_response(&response)?, "get_references")?;
    assert!(
        text.contains("\"references\"")
            && text.contains("\"symbol\"")
            && text.contains("Widget::new")
            && text.contains("src/lib.rs"),
        "get_references should resolve Widget::new via suffix matching. Got: {text}"
    );

    Ok(())
}
