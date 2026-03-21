mod common;

use anyhow::Result;
use common::McpTestClient;
use serde_json::json;

#[cfg(unix)]
#[test]
fn symlink_escape_rejected() -> Result<()> {
    use std::fs;
    use std::os::unix::fs as unix_fs;

    let root = tempfile::tempdir()?;
    let root_path = root.path().to_path_buf();
    let inside = root_path.join("inside");
    fs::create_dir_all(&inside)?;

    let outside = tempfile::tempdir()?;
    let outside_path = outside.path().to_path_buf();

    let escape_link = inside.join("escape");
    unix_fs::symlink(&outside_path, &escape_link)?;

    let envs = vec![(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        root_path.to_string_lossy().to_string(),
    )];
    let mut client = McpTestClient::new_with_env_initialized(&envs)?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "semantic_search",
            "arguments": {
                "query": "kind:function",
                "path": "inside/escape",
                "max_results": 5
            }
        }),
        42,
    )?;

    assert_eq!(response["jsonrpc"], "2.0");
    let error = response["error"].as_object().expect("error object");
    assert_eq!(error["code"], -32603);
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("outside of the workspace root")
            || message.contains("Failed to canonicalize path"),
        "unexpected error message: {message}"
    );

    Ok(())
}

#[test]
fn parent_directory_traversal_rejected() -> Result<()> {
    let mut client = McpTestClient::new_initialized()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "semantic_search",
            "arguments": {
                "query": "kind:function",
                "path": "../../../etc",
                "max_results": 5
            }
        }),
        7,
    )?;

    assert_eq!(response["jsonrpc"], "2.0");
    let error = response["error"].as_object().expect("error");
    assert_eq!(error["code"], -32603);
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("outside of the workspace root")
            || message.contains("Failed to canonicalize path"),
        "unexpected error message: {message}"
    );

    Ok(())
}
