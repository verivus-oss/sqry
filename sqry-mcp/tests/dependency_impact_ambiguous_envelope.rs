//! `dependency_impact` MCP integration: ambiguous-symbol envelope.
//!
//! Verifies the C_AMBIGUOUS DAG unit's MCP boundary: when a bare symbol
//! name resolves to multiple nodes the tool returns an error whose
//! message body is the canonical `sqry::ambiguous_symbol` JSON envelope
//! (with `code`, `message`, `candidates[]`, `truncated`). Qualified
//! names resolve unambiguously.

use anyhow::Result;
use serde_json::Value;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{DependencyImpactArgs, PaginationArgs};
use sqry_mcp::tool_handlers::execute_dependency_impact;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

fn workspace_arg(temp: &TempDir) -> String {
    temp.path()
        .canonicalize()
        .unwrap_or_else(|_| temp.path().to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn index_fixture(workspace: &std::path::Path) -> Result<()> {
    init_caches();
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

/// Go fixture with a struct field + a package-scope variable both
/// named `NeedTags`. The package-scope variable is the only Go form
/// the plugin emits today under the unsuffixed simple name `NeedTags`,
/// so this is what makes the resolver see two candidates instead of one.
fn write_ambiguous_go_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    fs::write(
        temp.path().join("main.go"),
        r#"package main

type SelectorSource struct {
    NeedTags bool
}

var NeedTags = "package-scope shadow"

func useSelector(selector SelectorSource) bool {
    if selector.NeedTags {
        return true
    }
    return false
}

func unrelated() {
    _ = NeedTags
}
"#,
    )?;
    Ok(temp)
}

#[test]
fn dependency_impact_returns_ambiguous_envelope_for_bare_name() -> Result<()> {
    let temp = write_ambiguous_go_fixture()?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "NeedTags".to_string(),
        path: workspace_arg(&temp),
        max_depth: 3,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };

    let err = match execute_dependency_impact(&args) {
        Ok(_) => panic!("ambiguous symbol must surface as Err, got Ok"),
        Err(e) => e,
    };
    let message = err.to_string();
    let envelope: Value = serde_json::from_str(&message).unwrap_or_else(|e| {
        panic!("expected ambiguous envelope JSON in error message; got {message:?} ({e})")
    });

    let error_obj = envelope
        .get("error")
        .expect("envelope wraps the ambiguous payload under `error`");
    assert_eq!(error_obj["code"], "sqry::ambiguous_symbol");
    let msg = error_obj["message"].as_str().expect("message is string");
    assert!(
        msg.contains("NeedTags") && msg.contains("ambiguous"),
        "envelope message must name the symbol and the ambiguity, got {msg:?}"
    );
    assert_eq!(error_obj["truncated"], Value::Bool(false));

    let candidates = error_obj["candidates"]
        .as_array()
        .expect("candidates[] is required");
    assert!(
        candidates.len() >= 2,
        "expected at least 2 candidates, got {}",
        candidates.len()
    );
    for candidate in candidates {
        assert!(candidate.get("qualified_name").is_some());
        assert!(candidate.get("kind").is_some());
        assert!(candidate.get("file_path").is_some());
        assert!(candidate.get("start_line").is_some());
        assert!(candidate.get("start_column").is_some());
    }
    Ok(())
}

#[test]
fn dependency_impact_resolves_qualified_name_unambiguously() -> Result<()> {
    let temp = write_ambiguous_go_fixture()?;
    index_fixture(temp.path())?;

    let args = DependencyImpactArgs {
        symbol: "main.SelectorSource.NeedTags".to_string(),
        path: workspace_arg(&temp),
        max_depth: 3,
        include_files: false,
        include_indirect: true,
        max_results: 100,
        pagination: paging(),
        file_path: None,
    };

    let result =
        execute_dependency_impact(&args).expect("qualified name must resolve unambiguously");
    assert_eq!(result.data.target_symbol, "main.SelectorSource.NeedTags");
    Ok(())
}
