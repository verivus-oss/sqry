//! T3 Cluster H1 — MCP smoke test for the `context_propagation` tool.
//!
//! Pins the wire-up between the MCP server's `tools/list` advertisement
//! and `tools/call` dispatch for the T3.7 tool. Per 05_TEST_PLAN.md §1.4,
//! this is the single MCP-layer assertion that complements the CLI's
//! cli_context_propagation.rs end-to-end coverage.

mod common;

use anyhow::{Result, anyhow};
use common::{McpTestClient, ensure_graph_snapshot, unwrap_mcp_content};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip dot-prefixed entries (`.sqry/`, `.git/`, …) so leftover
        // dev-loop indexes in the canonical fixture tree don't bleed
        // into the tempdir. Without this, `ensure_graph_snapshot` would
        // observe the pre-existing snapshot and skip rebuilding — and
        // the test would assert against a stale, partial graph.
        // Discovered while closing codex iter-3 concern 5 (Cluster H1c).
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn prepare_indexed_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let src = workspace_root().join("test-fixtures/go/context_propagation");
    copy_dir_recursive(&src, temp.path())?;
    // Build the graph snapshot in-process (no subprocess; matches the
    // ensure_graph_snapshot helper convention in common::mod). This
    // also avoids any sqry binary path resolution overhead.
    ensure_graph_snapshot(temp.path())?;
    Ok(temp)
}

/// AC-T3.7-1 wire-up smoke: the MCP server advertises
/// `context_propagation` in `tools/list` and, when called with a valid
/// workspace path, returns a result envelope containing the documented
/// `leaks` array shape from `sqry-mcp/src/execution/types.rs::ContextPropagationData`.
#[test]
fn context_propagation_tool_smoke_test() -> Result<()> {
    let fixture = prepare_indexed_fixture()?;
    let env_vars = vec![(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        fixture.path().to_string_lossy().into_owned(),
    )];
    let mut client = McpTestClient::new_with_env_initialized(&env_vars)?;

    // Step 1: advertisement — the tool MUST appear in `tools/list`.
    let list_resp = client.call("tools/list", json!({}), 100)?;
    let tools = list_resp
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("tools/list missing tools array: {list_resp:?}"))?;
    let advertised = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .any(|n| n == "context_propagation");
    assert!(
        advertised,
        "context_propagation must be advertised by tools/list (Cluster G wire-up); response={list_resp:?}",
    );

    // Step 2: dispatch — a tools/call with a valid path must succeed
    // and return the documented envelope. We tolerate either an empty
    // or non-empty `leaks` array (the precise count depends on the Go
    // plugin's Calls-edge attribution for each fixture); we only pin
    // the SHAPE of the response.
    let call_resp = client.call(
        "tools/call",
        json!({
            "name": "context_propagation",
            "arguments": {
                "path": fixture.path().to_string_lossy(),
                "scope": {"kind": "global"},
                "mode": "all",
                "max_results": 50
            }
        }),
        101,
    )?;
    let inner: Value = unwrap_mcp_content(&call_resp)?;
    // The MCP envelope wraps the tool data in `data.{leaks, total,
    // scope, mode, truncated}`. Per
    // sqry-mcp/src/execution/types.rs::ContextPropagationData.
    let data = inner
        .get("data")
        .ok_or_else(|| anyhow!("tools/call response missing `data` envelope: {inner:?}"))?;
    let leaks = data
        .get("leaks")
        .ok_or_else(|| anyhow!("data envelope missing `leaks` field: {data:?}"))?;
    assert!(
        leaks.is_array(),
        "context_propagation `leaks` field must be a JSON array; got {leaks:?}",
    );
    for envelope_field in ["total", "scope", "mode", "truncated"] {
        assert!(
            data.get(envelope_field).is_some(),
            "context_propagation data envelope must carry `{envelope_field}`; got {data:?}",
        );
    }
    // Sanity: each leak record must carry the documented per-leak
    // fields (mode + caller + callee + callerFile + callSpan).
    for leak in leaks.as_array().unwrap_or(&Vec::new()) {
        for field in ["mode", "caller", "callee", "callerFile", "callSpan"] {
            assert!(
                leak.get(field).is_some(),
                "each leak record must carry `{field}`; leak={leak:?}",
            );
        }
    }
    Ok(())
}
