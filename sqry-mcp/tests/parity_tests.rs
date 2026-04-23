mod common;

use anyhow::{Context, Result};
use common::{McpTestClient, ensure_graph_snapshot, unwrap_mcp_content};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn mini_workspace() -> PathBuf {
    workspace_root().join("sqry-lsp/tests/fixtures/mini-workspace")
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/expected_responses")
        .join(name)
}

fn normalize_value(value: &mut Value, workspace_root: &str) {
    // Strip Windows extended-length path prefix (\\?\) added by canonicalize().
    // This prefix breaks file URI matching: //?\D:/... won't match ///D:/... in URIs.
    let ws = workspace_root
        .strip_prefix(r"\\?\")
        .unwrap_or(workspace_root);
    let ws_forward = ws.replace('\\', "/");
    normalize_value_inner(value, ws, &ws_forward);
}

fn normalize_value_inner(value: &mut Value, workspace_root: &str, ws_forward: &str) {
    match value {
        Value::Object(map) => {
            map.remove("execution_ms");
            if let Some(Value::String(path)) = map.get_mut("workspace_path") {
                *path = "<WORKSPACE_ROOT>".to_string();
            }
            for entry in map.values_mut() {
                normalize_value_inner(entry, workspace_root, ws_forward);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_value_inner(item, workspace_root, ws_forward);
            }
        }
        Value::String(s) => {
            if ws_forward != workspace_root {
                // Windows: ws_forward = "D:/..." (no leading /), workspace_root = "D:\..."
                // File URIs contain "/D:/..." — consume the leading / so
                // file:///D:/.../src/lib.rs → file://<WORKSPACE_ROOT>/src/lib.rs
                // (matching the Linux result where / is part of the path prefix).
                let ws_slashed = format!("/{ws_forward}");
                if s.contains(ws_slashed.as_str()) {
                    *s = s.replace(ws_slashed.as_str(), "<WORKSPACE_ROOT>");
                }
                // Plain forward-slash paths (non-URI contexts)
                if s.contains(ws_forward) {
                    *s = s.replace(ws_forward, "<WORKSPACE_ROOT>");
                }
                // Native backslash paths
                if s.contains(workspace_root) {
                    *s = s.replace(workspace_root, "<WORKSPACE_ROOT>");
                }
            } else if s.contains(workspace_root) {
                *s = s.replace(workspace_root, "<WORKSPACE_ROOT>");
            }
            normalize_known_fixture_root(s);
            // Normalize any CRLF artifacts that may leak into symbol names
            // when tree-sitter parses files with Windows line endings.
            if s.contains("\r\n") {
                *s = s.replace("\r\n", "\n");
            }
        }
        _ => {}
    }
}

fn normalize_known_fixture_root(s: &mut String) {
    const MARKERS: [&str; 2] = [
        "/sqry-lsp/tests/fixtures/mini-workspace",
        "\\sqry-lsp\\tests\\fixtures\\mini-workspace",
    ];

    for marker in MARKERS {
        if let Some(marker_pos) = s.find(marker) {
            let suffix = s[marker_pos + marker.len()..].to_string();
            *s = if s.starts_with("file://") {
                format!("file://<WORKSPACE_ROOT>{suffix}")
            } else {
                format!("<WORKSPACE_ROOT>{suffix}")
            };
            return;
        }
    }
}

fn load_expected(name: &str, workspace_root: &str) -> Result<Value> {
    let content = std::fs::read_to_string(fixture_path(name))?;
    let response: Value = serde_json::from_str(&content)?;
    let mut payload = unwrap_mcp_content(&response)?;
    normalize_value(&mut payload, workspace_root);
    Ok(payload)
}

#[allow(clippy::needless_pass_by_value)] // Convenience for callers
fn run_tool_call(name: &str, arguments: Value, workspace_root: &Path) -> Result<Value> {
    ensure_graph_snapshot(workspace_root)?;

    let envs = vec![
        (
            "SQRY_MCP_WORKSPACE_ROOT".to_string(),
            workspace_root.to_string_lossy().to_string(),
        ),
        // Disable redaction so fixture paths are not scrubbed (default is now "minimal")
        ("SQRY_REDACTION_PRESET".to_string(), "none".to_string()),
    ];
    let mut client = McpTestClient::new_with_env_initialized(&envs)?;
    let response = client.call(
        "tools/call",
        json!({
            "name": name,
            "arguments": arguments
        }),
        1,
    )?;

    let mut payload = unwrap_mcp_content(&response)?;
    let root_canon = workspace_root
        .canonicalize()
        .context("canonicalize workspace root")?;
    let root_str = root_canon.to_string_lossy();
    normalize_value(&mut payload, root_str.as_ref());
    Ok(payload)
}

#[test]
fn mcp_document_symbols_fixture() -> Result<()> {
    let workspace_root = mini_workspace();
    let root_canon = workspace_root.canonicalize()?;
    let root_str = root_canon.to_string_lossy();
    let expected = load_expected("document_symbols.json", root_str.as_ref())?;
    let actual = run_tool_call(
        "get_document_symbols",
        json!({ "file_path": "src/lib.rs" }),
        &workspace_root,
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn mcp_workspace_symbols_fixture() -> Result<()> {
    let workspace_root = mini_workspace();
    let root_canon = workspace_root.canonicalize()?;
    let root_str = root_canon.to_string_lossy();
    let expected = load_expected("workspace_symbols.json", root_str.as_ref())?;
    let actual = run_tool_call(
        "get_workspace_symbols",
        json!({ "query": "helper", "max_results": 20 }),
        &workspace_root,
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn mcp_semantic_search_fixture() -> Result<()> {
    let workspace_root = mini_workspace();
    let root_canon = workspace_root.canonicalize()?;
    let root_str = root_canon.to_string_lossy();
    let expected = load_expected("semantic_search.json", root_str.as_ref())?;
    let actual = run_tool_call(
        "semantic_search",
        json!({ "query": "kind:function", "path": ".", "max_results": 20, "context_lines": 0 }),
        &workspace_root,
    )?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn mcp_hierarchical_search_fixture() -> Result<()> {
    let workspace_root = mini_workspace();
    let root_canon = workspace_root.canonicalize()?;
    let root_str = root_canon.to_string_lossy();
    let expected = load_expected("hierarchical_search.json", root_str.as_ref())?;
    let actual = run_tool_call(
        "hierarchical_search",
        json!({
            "query": "kind:function",
            "path": ".",
            "max_results": 50,
            "context_lines": 0,
            "max_files": 10
        }),
        &workspace_root,
    )?;
    assert_eq!(actual, expected);
    Ok(())
}
