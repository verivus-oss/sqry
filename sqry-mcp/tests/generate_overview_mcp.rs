//! `generate_overview` MCP tool integration tests.
//!
//! Covers the 05_TEST_PLAN "MCP" bullets:
//! - Standalone and daemon-hosted handlers both return the overview structure
//!   with the same sections.
//! - The active redaction preset is applied to the MCP response.
//! - (Tool-list / capability-map presence for both transports is covered by
//!   `tools_schema_parity.rs`, the `resources` unit tests, and the daemon
//!   `ipc_tools_list_daemon_subset` integration test.)

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use common::McpTestClient;
use serde_json::{Value, json};

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::query::executor::QueryExecutor;
use sqry_mcp::daemon_adapter::{
    WorkspaceContext, execute_generate_overview_for_daemon, tool_response_json,
};
use sqry_mcp::daemon_params::params_to_generate_overview_args;
use sqry_plugin_registry::create_plugin_manager;

/// Canonical section keys in the serialized (camelCase) overview payload.
const SECTION_KEYS: [&str; 6] = [
    "summary",
    "hubs",
    "subsystems",
    "hotspots",
    "issues",
    "suggestedQuestions",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn mini_workspace() -> PathBuf {
    repo_root().join("sqry-lsp/tests/fixtures/mini-workspace")
}

/// Copy the mini-workspace source tree into a fresh tempdir so each test builds
/// its own isolated index. Avoids a cross-test-binary race on the shared
/// fixture's persisted `.sqry/` directory under parallel `cargo test`.
fn isolated_mini_workspace() -> Result<tempfile::TempDir> {
    let src = mini_workspace().join("src");
    let dir = tempfile::tempdir().context("create tempdir")?;
    let dst = dir.path().join("src");
    std::fs::create_dir_all(&dst).context("create src dir")?;
    for entry in std::fs::read_dir(&src).context("read fixture src")? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))
                .with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(dir)
}

/// Call `generate_overview` through the standalone rmcp server and return the
/// unwrapped tool payload (the serialized `ToolExecution`).
fn call_overview_standalone(
    workspace_root: &Path,
    arguments: Value,
    preset: Option<&str>,
) -> Result<Value> {
    common::ensure_graph_snapshot(workspace_root)?;

    let mut envs = vec![(
        "SQRY_MCP_WORKSPACE_ROOT".to_string(),
        workspace_root.to_string_lossy().to_string(),
    )];
    if let Some(preset) = preset {
        envs.push(("SQRY_REDACTION_PRESET".to_string(), preset.to_string()));
    }

    let mut client = McpTestClient::new_with_env_initialized(&envs)?;
    let response = client.call(
        "tools/call",
        json!({ "name": "generate_overview", "arguments": arguments }),
        1,
    )?;
    common::unwrap_mcp_content(&response)
}

/// The standalone handler returns every canonical section by default.
#[test]
fn standalone_generate_overview_returns_all_sections() -> Result<()> {
    let tmp = isolated_mini_workspace()?;
    let ws = tmp.path();
    // Redaction "none" so section presence is asserted without path scrubbing.
    let payload = call_overview_standalone(ws, json!({}), Some("none"))?;

    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .context("overview payload must carry a `data` object")?;

    for key in SECTION_KEYS {
        assert!(
            data.contains_key(key),
            "default overview must include section `{key}`; got keys {:?}",
            data.keys().collect::<Vec<_>>()
        );
    }
    // Summary must carry the health block the CLI report emits.
    let health = data
        .get("summary")
        .and_then(|s| s.get("health"))
        .context("summary.health must be present")?;
    for hk in [
        "cycles",
        "unusedSymbols",
        "duplicateGroups",
        "crossLanguageEdges",
    ] {
        assert!(
            health.get(hk).is_some(),
            "summary.health must include `{hk}`"
        );
    }
    Ok(())
}

/// The `sections` filter restricts the report to the requested sections.
#[test]
fn standalone_generate_overview_respects_sections_filter() -> Result<()> {
    let tmp = isolated_mini_workspace()?;
    let ws = tmp.path();
    let payload =
        call_overview_standalone(ws, json!({ "sections": "summary,hubs" }), Some("none"))?;

    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .context("overview payload must carry a `data` object")?;

    assert!(data.contains_key("summary"), "summary requested");
    assert!(data.contains_key("hubs"), "hubs requested");
    for absent in ["subsystems", "hotspots", "issues", "suggestedQuestions"] {
        assert!(
            !data.contains_key(absent),
            "section `{absent}` was not requested and must be omitted"
        );
    }
    Ok(())
}

/// Under the default (`minimal`) preset the raw absolute workspace root must
/// never leak into the report content: the active redaction preset is applied.
///
/// The check targets the `data` object (the report sections). The envelope's
/// `workspace_path` identity field intentionally carries the resolved root for
/// every tool response and is out of scope for report-content redaction.
#[test]
fn standalone_generate_overview_applies_active_redaction_preset() -> Result<()> {
    let tmp = isolated_mini_workspace()?;
    let ws = tmp.path();
    // No preset override => server default (`minimal`).
    let payload = call_overview_standalone(ws, json!({}), None)?;

    let data = payload
        .get("data")
        .context("overview payload must carry a `data` object")?;

    let root_canon = ws.canonicalize().context("canonicalize workspace root")?;
    let root_str = root_canon.to_string_lossy().replace('\\', "/");
    let rendered = serde_json::to_string(data)?.replace('\\', "/");

    assert!(
        !rendered.contains(root_str.as_str()),
        "default redaction preset must strip the absolute workspace root from the report content"
    );
    Ok(())
}

/// The daemon-hosted shared body returns the same section structure as the
/// standalone handler. Both transports delegate to the same
/// `overview_inner::execute_generate_overview`, so exercising the
/// `_for_daemon` wrapper proves daemon parity.
#[test]
fn daemon_hosted_generate_overview_returns_same_sections() -> Result<()> {
    let tmp = isolated_mini_workspace()?;
    let ws = tmp.path().to_path_buf();
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    let graph: CodeGraph =
        build_unified_graph(&ws, &plugins, &config).context("build mini-workspace graph")?;

    let ctx = WorkspaceContext {
        workspace_root: ws.clone(),
        graph: Arc::new(graph),
        executor: Arc::new(QueryExecutor::new()),
    };

    let args = params_to_generate_overview_args(json!({ "path": ws.to_string_lossy() }))
        .map_err(|e| anyhow::anyhow!("args conversion failed: {e:?}"))?;
    let exec = execute_generate_overview_for_daemon(&ctx, &args)?;
    let payload =
        tool_response_json(exec).map_err(|e| anyhow::anyhow!("response build failed: {e:?}"))?;

    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .context("daemon overview payload must carry a `data` object")?;

    for key in SECTION_KEYS {
        assert!(
            data.contains_key(key),
            "daemon-hosted overview must include section `{key}`; got keys {:?}",
            data.keys().collect::<Vec<_>>()
        );
    }
    Ok(())
}
