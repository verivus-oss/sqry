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

/// A `../` path is refused whichever way it falls on this host: a path that
/// names nothing is the request's argument refused (`-32602`
/// `validation_error` naming the path, the server's shared check, round 7),
/// and one that names a directory outside the workspace is the escape
/// refusal. Both legs are built here so neither depends on the layout above
/// the checkout; the original `../../../etc` probe is kept and may take
/// either (after the escape leg the session has resolved a workspace, so a
/// path naming nothing reads "names no workspace" there).
#[test]
fn parent_directory_traversal_rejected() -> Result<()> {
    use std::fs;

    let base = tempfile::tempdir()?;
    let workspace = base.path().join("a").join("ws");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(base.path().join("outside"))?;
    let envs = vec![(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        workspace.to_string_lossy().to_string(),
    )];
    let mut client = McpTestClient::new_with_env_initialized(&envs)?;
    let mut search = |path: &str, id: i64| -> Result<serde_json::Value> {
        let response = client.call(
            "tools/call",
            json!({
                "name": "semantic_search",
                "arguments": { "query": "kind:function", "path": path, "max_results": 5 }
            }),
            id,
        )?;
        assert_eq!(response["jsonrpc"], "2.0");
        Ok(response["error"].clone())
    };

    let names_nothing = search("../../../r7-nothing-here", 1)?;
    assert_eq!(names_nothing["code"], -32602, "{names_nothing}");
    assert_eq!(
        names_nothing["data"]["kind"], "validation_error",
        "{names_nothing}"
    );
    assert!(
        names_nothing["message"]
            .as_str()
            .is_some_and(|message| message.contains("../../../r7-nothing-here")),
        "{names_nothing}"
    );

    let escapes = search("../../outside", 2)?;
    assert_eq!(escapes["code"], -32603, "{escapes}");
    assert!(
        escapes["message"]
            .as_str()
            .is_some_and(|message| message.contains("outside of the workspace root")),
        "{escapes}"
    );

    let etc = search("../../../etc", 3)?;
    let message = etc["message"].as_str().unwrap_or("");
    assert!(
        (etc["code"] == -32602
            && (message.contains("names nothing") || message.contains("names no workspace")))
            || (etc["code"] == -32603 && message.contains("outside of the workspace root")),
        "unexpected refusal: {etc}"
    );

    Ok(())
}
