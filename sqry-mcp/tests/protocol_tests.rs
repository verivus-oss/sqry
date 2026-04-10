mod common;

use anyhow::Result;
use common::{McpTestClient, StderrMode, unwrap_mcp_content};
use serde_json::json;
use std::io::Write;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn create_temp_workspace() -> Result<TempDir> {
    let workspace = TempDir::new()?;
    std::fs::create_dir_all(workspace.path().join("src"))?;
    std::fs::write(workspace.path().join("src/lib.rs"), "fn sample() {}\n")?;
    Ok(workspace)
}

fn forward_slash_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn wait_for_workspace_path(
    client: &mut McpTestClient,
    expected: &str,
    request_id_start: i64,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut request_id = request_id_start;
    loop {
        let response = client.call(
            "tools/call",
            json!({
                "name": "get_index_status",
                "arguments": {}
            }),
            request_id,
        )?;
        let payload = unwrap_mcp_content(&response)?;
        if payload["workspace_path"] == expected {
            return Ok(payload);
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "workspace roots did not refresh in time; expected {expected}, got {}",
                payload["workspace_path"]
            );
        }

        request_id += 1;
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn test_initialize() -> Result<()> {
    let mut client = McpTestClient::new()?;

    let response = client.call(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }),
        1,
    )?;

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
    assert!(response["result"]["serverInfo"]["name"].is_string());
    let capabilities = response["result"]["capabilities"]
        .as_object()
        .expect("capabilities object");
    assert!(
        capabilities.get("tools").is_some(),
        "capabilities should include tools"
    );
    assert!(
        capabilities.get("prompts").is_some(),
        "capabilities should include prompts"
    );

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    writeln!(client.stdin, "{notification}")?;
    client.stdin.flush()?;

    Ok(())
}

#[test]
fn test_tools_list() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    let response = client.call("tools/list", json!({}), 2)?;

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    // Core tools (not deprecated)
    assert!(tool_names.contains(&"semantic_search"));
    assert!(tool_names.contains(&"relation_query"));
    assert!(tool_names.contains(&"explain_code"));

    // New standardized names (v0.3+)
    assert!(tool_names.contains(&"search_similar"));
    assert!(tool_names.contains(&"show_dependencies"));
    assert!(tool_names.contains(&"get_index_status"));

    // Graph tools
    assert!(tool_names.contains(&"export_graph"));
    assert!(tool_names.contains(&"cross_language_edges"));

    // Verify each tool has required fields
    for tool in tools {
        assert!(tool["name"].is_string());
        assert!(tool["description"].is_string());
        assert!(tool["inputSchema"]["type"].is_string());
        assert!(tool["inputSchema"]["properties"].is_object());

        let schema_obj = tool["inputSchema"]
            .as_object()
            .expect("inputSchema must be an object");
        assert!(
            !schema_obj.contains_key("oneOf"),
            "inputSchema must not include oneOf at the top level (Claude API restriction)"
        );
        assert!(
            !schema_obj.contains_key("anyOf"),
            "inputSchema must not include anyOf at the top level (Claude API restriction)"
        );
        assert!(
            !schema_obj.contains_key("allOf"),
            "inputSchema must not include allOf at the top level (Claude API restriction)"
        );
    }

    Ok(())
}

#[test]
fn test_unknown_method() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    // rmcp 0.16+ responds to requests with unknown methods with a -32601 error
    // (per JSON-RPC 2.0: a request with an id MUST receive a response).
    client.send_request("unknown/method", json!({}), 3)?;
    let error_resp = client.read_response()?;
    assert_eq!(error_resp["jsonrpc"], "2.0");
    assert_eq!(error_resp["id"], 3);
    assert_eq!(error_resp["error"]["code"], -32601);

    // Verify server continues to operate normally after returning the error.
    let response = client.call("tools/list", json!({}), 30)?;
    assert_eq!(response["id"], 30);

    Ok(())
}

#[test]
fn test_invalid_jsonrpc() -> Result<()> {
    let mut client = McpTestClient::new()?;

    // Send request with wrong jsonrpc version
    let request = json!({
        "jsonrpc": "1.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        },
        "id": 4
    });
    writeln!(client.stdin, "{request}")?;
    client.stdin.flush()?;

    let response = client.read_response();
    match response {
        Ok(value) => {
            assert_eq!(value["jsonrpc"], "2.0");
            assert_eq!(value["id"], 4);
            assert!(value["error"].is_object());
            assert_eq!(value["error"]["code"], -32600); // Invalid Request
        }
        Err(err) => {
            let message = err.to_string();
            assert!(
                message.contains("EOF while parsing a value"),
                "unexpected error: {message}"
            );
        }
    }

    Ok(())
}

#[test]
fn test_parse_error() -> Result<()> {
    let mut client = McpTestClient::new()?;

    // Send invalid JSON
    writeln!(client.stdin, "{{invalid json}}")?;
    client.stdin.flush()?;

    let response = client.read_response();
    match response {
        Ok(value) => {
            assert_eq!(value["jsonrpc"], "2.0");
            assert!(value["error"].is_object());
            assert_eq!(value["error"]["code"], -32700); // Parse error
            assert!(value["id"].is_null());
        }
        Err(err) => {
            let message = err.to_string();
            assert!(
                message.contains("EOF while parsing a value"),
                "unexpected error: {message}"
            );
        }
    }

    Ok(())
}

#[test]
fn test_request_id_propagation() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    // Test with integer ID
    let response1 = client.call("tools/list", json!({}), 100)?;
    assert_eq!(response1["id"], 100);

    // Test with string ID (should work per JSON-RPC 2.0)
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {},
        "id": "test-id-123"
    });
    writeln!(client.stdin, "{request}")?;
    client.stdin.flush()?;

    let response2 = client.read_response()?;
    assert_eq!(response2["id"], "test-id-123");

    Ok(())
}

#[test]
fn test_missing_tool_name() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    // rmcp 0.16+ responds to tools/call with missing required 'name' param
    // with a -32601 error (the request has an id, so it must receive a response).
    client.send_request(
        "tools/call",
        json!({
            "arguments": {}
        }),
        5,
    )?;
    let error_resp = client.read_response()?;
    assert_eq!(error_resp["id"], 5);
    assert!(error_resp["error"].is_object());

    // Verify server continues to operate normally after returning the error.
    let response = client.call("tools/list", json!({}), 100)?;
    assert_eq!(response["id"], 100);

    Ok(())
}

#[test]
fn test_unknown_tool() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "nonexistent_tool",
            "arguments": {}
        }),
        6,
    )?;

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 6);
    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32602);
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(message.contains("tool") && message.contains("not"));

    Ok(())
}

#[test]
fn test_notification_no_response() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    // Send a notification (no id field) with an unknown method
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/test",
        "params": {}
    });
    writeln!(client.stdin, "{notification}")?;
    client.stdin.flush()?;

    // Send a regular request right after to check the server is still responsive
    let response = client.call("tools/list", json!({}), 100)?;
    assert_eq!(response["id"], 100);
    assert!(response["result"]["tools"].is_array());

    // The test verifies that the notification didn't produce output by confirming
    // that the next response we read is the tools/list response (id=100),
    // not a notification response.

    Ok(())
}

#[test]
fn test_notification_known_method() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    // Send a notification to a known method (tools/list)
    // Per JSON-RPC 2.0, notifications should be processed but not responded to
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {}
    });
    writeln!(client.stdin, "{notification}")?;
    client.stdin.flush()?;

    // Send a regular request to verify server is still alive
    let response = client.call("tools/list", json!({}), 101)?;
    assert_eq!(response["id"], 101);

    Ok(())
}

#[test]
fn test_multi_request_session() -> Result<()> {
    let mut client = McpTestClient::new()?;

    // Test a typical session: initialize → tools/list → tools/call → unknown method

    // 1. Initialize
    let resp1 = client.call(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }),
        1,
    )?;
    assert_eq!(resp1["id"], 1);
    assert!(resp1["result"]["protocolVersion"].is_string());

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    writeln!(client.stdin, "{notification}")?;
    client.stdin.flush()?;

    // 2. List tools
    let resp2 = client.call("tools/list", json!({}), 2)?;
    assert_eq!(resp2["id"], 2);
    assert!(resp2["result"]["tools"].as_array().unwrap().len() >= 5);

    // 3. Call a tool
    let resp3 = client.call(
        "tools/call",
        json!({
            "name": "semantic_search",
            "arguments": {
                "query": "kind:function",
                "path": ".",
                "max_results": 5
            }
        }),
        3,
    )?;
    assert_eq!(resp3["id"], 3);
    // Result should either be Ok or an internal error when no index exists.
    if resp3.get("result").is_none() {
        assert_eq!(resp3["error"]["code"], -32603);
    }

    // 4. Unknown method - rmcp 0.16+ returns -32601 error for requests with unknown methods.
    // Read the error response before sending the next request.
    client.send_request("unknown/method", json!({}), 4)?;
    let err4 = client.read_response()?;
    assert_eq!(err4["id"], 4);
    assert_eq!(err4["error"]["code"], -32601);

    // 5. Verify server is still alive after the error.
    let resp4 = client.call("tools/list", json!({}), 5)?;
    assert_eq!(resp4["id"], 5);

    Ok(())
}

#[test]
fn test_roots_supported_session_resolves_workspace_without_path() -> Result<()> {
    let workspace = create_temp_workspace()?;
    let mut client =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    let _ = client.initialize_with_roots(&[workspace.path().to_path_buf()])?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        401,
    )?;
    let payload = unwrap_mcp_content(&response)?;

    assert_eq!(
        payload["workspace_path"],
        forward_slash_path(&workspace.path().canonicalize()?)
    );

    Ok(())
}

#[test]
fn test_roots_list_changed_refreshes_cached_workspace_roots() -> Result<()> {
    let workspace_a = create_temp_workspace()?;
    let workspace_b = create_temp_workspace()?;
    let mut client =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    let _ = client.initialize_with_roots(&[workspace_a.path().to_path_buf()])?;

    let first = unwrap_mcp_content(&client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        402,
    )?)?;
    assert_eq!(
        first["workspace_path"],
        forward_slash_path(&workspace_a.path().canonicalize()?)
    );

    client.set_roots(&[workspace_b.path().to_path_buf()]);
    client.notify_roots_list_changed()?;

    let second = wait_for_workspace_path(
        &mut client,
        &forward_slash_path(&workspace_b.path().canonicalize()?),
        403,
    )?;
    assert_eq!(
        second["workspace_path"],
        forward_slash_path(&workspace_b.path().canonicalize()?)
    );

    Ok(())
}

#[test]
fn test_multiple_roots_require_explicit_path_when_no_other_hint_exists() -> Result<()> {
    let workspace_a = create_temp_workspace()?;
    let workspace_b = create_temp_workspace()?;
    let mut client =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    let _ = client.initialize_with_roots(&[
        workspace_a.path().to_path_buf(),
        workspace_b.path().to_path_buf(),
    ])?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        404,
    )?;

    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Multiple workspace roots are active"),
        "unexpected error message: {message}"
    );

    Ok(())
}

#[test]
fn test_last_resolved_workspace_breaks_multi_root_tie_for_followup_requests() -> Result<()> {
    let workspace_a = create_temp_workspace()?;
    let workspace_b = create_temp_workspace()?;
    let workspace_a_canonical = workspace_a.path().canonicalize()?;
    let mut client =
        McpTestClient::new_without_workspace_env_and_stderr_mode(&[], StderrMode::Null)?;
    let _ = client.initialize_with_roots(&[
        workspace_a.path().to_path_buf(),
        workspace_b.path().to_path_buf(),
    ])?;

    let seeded = unwrap_mcp_content(&client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {
                "path": workspace_a_canonical
            }
        }),
        405,
    )?)?;
    assert_eq!(
        seeded["workspace_path"],
        forward_slash_path(&workspace_a_canonical)
    );

    let followup = unwrap_mcp_content(&client.call(
        "tools/call",
        json!({
            "name": "get_index_status",
            "arguments": {}
        }),
        406,
    )?)?;
    assert_eq!(
        followup["workspace_path"],
        forward_slash_path(&workspace_a_canonical)
    );

    Ok(())
}
