//! Black-box feature-surface exercise for the `sqry-mcp` stdio transport.

mod common;

use anyhow::Result;
use common::McpTestClient;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn setup_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let src = workspace_root().join("test-fixtures/e2e-scenarios/multi-lang");
    copy_dir_recursive(&src, temp.path())?;
    fs::create_dir_all(temp.path().join(".home"))?;
    fs::create_dir_all(temp.path().join(".xdg/config"))?;
    fs::create_dir_all(temp.path().join(".xdg/cache"))?;
    fs::create_dir_all(temp.path().join(".xdg/data"))?;
    Ok(temp)
}

fn sqry_bin() -> PathBuf {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.parent().expect("workspace root");
    let candidate = workspace.join("target/debug/sqry");
    if candidate.is_file() {
        return candidate;
    }

    let release_candidate = workspace.join("target/release/sqry");
    if release_candidate.is_file() {
        return release_candidate;
    }

    panic!("Could not find sqry binary. Set SQRY_E2E_SQRY_BIN or run `cargo build --bin sqry`.");
}

fn assert_success(name: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_sqry(project: &Path, args: &[&str]) {
    let output = Command::new(sqry_bin())
        .args(args)
        .current_dir(project)
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("HOME", project.join(".home"))
        .env("XDG_CONFIG_HOME", project.join(".xdg/config"))
        .env("XDG_CACHE_HOME", project.join(".xdg/cache"))
        .env("XDG_DATA_HOME", project.join(".xdg/data"))
        .output()
        .expect("run sqry");
    assert_success(&format!("sqry {}", args.join(" ")), &output);
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("run git");
    assert_success(&format!("git {}", args.join(" ")), &output);
}

fn make_git_history(project: &Path) {
    git(project, &["init", "--initial-branch", "main"]);
    git(project, &["config", "user.name", "sqry e2e"]);
    git(
        project,
        &["config", "user.email", "sqry-e2e@example.invalid"],
    );
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "baseline"]);

    let lib = project.join("src/lib.rs");
    let mut content = fs::read_to_string(&lib).expect("read lib.rs");
    content.push_str("\npub fn surface_added(value: i32) -> i32 { value + 7 }\n");
    fs::write(&lib, content).expect("write changed lib.rs");
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "add surface function"]);
}

fn tool_arguments(name: &str, project: &Path) -> Option<Value> {
    let rust_lib = project.join("src/lib.rs").to_string_lossy().into_owned();
    let args = match name {
        "call_hierarchy" => {
            json!({"symbol": "process", "direction": "outgoing", "max_depth": 2, "max_results": 20})
        }
        "complexity_metrics" => json!({"min_complexity": 1, "max_results": 20}),
        "context_propagation" => json!({"mode": "all", "max_results": 20}),
        "cross_language_edges" => json!({"max_results": 20}),
        "dependency_impact" => json!({"symbol": "helper", "max_depth": 2, "max_results": 20}),
        "direct_callees" => json!({"symbol": "process", "max_results": 20}),
        "direct_callers" => json!({"symbol": "helper", "max_results": 20}),
        "expand_cache_status" => json!({}),
        "explain_code" => {
            json!({"file_path": "src/lib.rs", "symbol_name": "process", "include_context": true, "include_relations": true})
        }
        "export_graph" => {
            json!({"symbol_name": "process", "format": "json", "max_depth": 2, "max_results": 50})
        }
        "find_cycles" => json!({"cycle_type": "calls", "max_results": 20}),
        "find_duplicates" => {
            json!({"duplicate_type": "signature", "threshold": 80, "max_results": 20})
        }
        "find_unused" => json!({"scope": "all", "max_results": 20}),
        "get_definition" => json!({"symbol": "process"}),
        "get_document_symbols" => json!({"file_path": "src/lib.rs"}),
        "get_graph_stats" => json!({}),
        "get_hover_info" => json!({"symbol": "helper"}),
        "get_index_status" => json!({}),
        "get_insights" => json!({}),
        "get_references" => json!({"symbol": "process", "max_results": 20}),
        "get_workspace_symbols" => json!({"query": "process", "max_results": 20}),
        "hierarchical_search" => json!({"query": "process", "max_results": 20, "max_files": 5}),
        "is_node_in_cycle" => json!({"symbol": "helper", "cycle_type": "calls"}),
        "list_files" => json!({"max_results": 20}),
        "list_symbols" => json!({"kind": "function", "max_results": 20}),
        "pattern_search" => json!({"pattern": "process", "max_results": 20}),
        "rebuild_index" => json!({"force": true}),
        "relation_query" => {
            json!({"symbol": "process", "relation_type": "callees", "max_depth": 1, "max_results": 20})
        }
        "search_similar" => {
            json!({"reference": {"file_path": rust_lib, "symbol_name": "helper"}, "max_results": 20})
        }
        "semantic_diff" => {
            json!({"base": {"ref": "HEAD~1"}, "target": {"ref": "HEAD"}, "max_results": 50})
        }
        "semantic_search" => json!({"query": "process", "max_results": 20}),
        "show_dependencies" => json!({"symbol_name": "helper", "max_depth": 2}),
        "sqry_query" => json!({"query": "kind:function", "limit": 20}),
        "subgraph" => json!({"symbols": ["process"], "max_depth": 2, "max_nodes": 20}),
        "trace_path" => json!({"from_symbol": "process", "to_symbol": "helper", "max_hops": 4}),
        "workspace_status" => json!({}),
        _ => return None,
    };

    Some(args)
}

fn extract_tool_text(response: &Value) -> Result<&str> {
    if let Some(error) = response.get("error") {
        anyhow::bail!("JSON-RPC error response: {error}");
    }

    let content = response["result"]["content"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing MCP content array: {response}"))?;
    let first = content
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty MCP content array: {response}"))?;
    first["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing MCP text content: {response}"))
}

#[test]
fn installed_mcp_feature_surface_matrix() -> Result<()> {
    let project = setup_fixture()?;
    make_git_history(project.path());
    run_sqry(project.path(), &["index", "--force", "."]);

    let envs = vec![
        (
            "SQRY_MCP_WORKSPACE_ROOT".to_string(),
            project.path().to_string_lossy().into_owned(),
        ),
        ("SQRY_MCP_TIMEOUT_MS".to_string(), "120000".to_string()),
        (
            "SQRY_MCP_INDEX_TIMEOUT_MS".to_string(),
            "120000".to_string(),
        ),
        ("SQRY_REDACTION_PRESET".to_string(), "none".to_string()),
        (
            "HOME".to_string(),
            project.path().join(".home").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CONFIG_HOME".to_string(),
            project
                .path()
                .join(".xdg/config")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            project
                .path()
                .join(".xdg/cache")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            project
                .path()
                .join(".xdg/data")
                .to_string_lossy()
                .into_owned(),
        ),
    ];
    let mut client = McpTestClient::new_with_env_initialized(&envs)?;

    let list = client.call("tools/list", json!({}), 10)?;
    let tools = list["result"]["tools"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools/list did not return a tools array: {list}"))?;
    assert!(
        !tools.is_empty(),
        "tools/list must expose at least one tool: {list}"
    );

    let mut tool_names = Vec::new();
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tool missing name: {tool}"))?;
        tool_names.push(name.to_string());
    }
    tool_names.sort_by(|left, right| match (left.as_str(), right.as_str()) {
        ("rebuild_index", "rebuild_index") => std::cmp::Ordering::Equal,
        ("rebuild_index", _) => std::cmp::Ordering::Greater,
        (_, "rebuild_index") => std::cmp::Ordering::Less,
        _ => left.cmp(right),
    });

    for (idx, name) in tool_names.iter().enumerate() {
        let args = tool_arguments(name, project.path())
            .ok_or_else(|| anyhow::anyhow!("missing test arguments for MCP tool `{name}`"))?;
        let response = client.call(
            "tools/call",
            json!({
                "name": name,
                "arguments": args
            }),
            i64::try_from(idx + 100).expect("tool index fits i64"),
        )?;

        if name == "search_similar" {
            let message = response["error"]["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("not found"),
                "search_similar negative assertion must return a structured not-found error: {response}"
            );
            continue;
        }

        let text = extract_tool_text(&response)?;
        assert!(
            !text.trim().is_empty(),
            "MCP tool `{name}` returned empty text content"
        );
        serde_json::from_str::<Value>(text).unwrap_or_else(|error| {
            panic!("MCP tool `{name}` returned non-JSON text: {error}\n{text}")
        });
    }

    Ok(())
}
