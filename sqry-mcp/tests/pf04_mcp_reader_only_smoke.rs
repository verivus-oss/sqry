//! PF04 — surface proof that the MCP `sqry_query` tool is a derived-cache
//! READER only.
//!
//! Drives [`execute_sqry_query`] in-process against a temp workspace
//! whose snapshot is built via the same auto-index path the real MCP
//! server uses (`engine.ensure_graph()`). Asserts that the
//! `derived.sqry` companion file is never created by the tool dispatch
//! path.
//!
//! `execute_sqry_query` internally calls
//! [`sqry_db::queries::dispatch::make_query_db_cold`], which is allowed
//! to *delete* a stale/corrupt derived-cache file but never to *write*
//! one. The writer lives exclusively in the daemon's `QueryDbHook`
//! (PF03B).
//!
//! Spec: docs/reviews/generational-design-analysis/2026-05-07/codex_in_code_verification_2026-05-07T030441Z.md
//! Plan: docs/development/generational-analysis-platform/priority-followups/03_IMPLEMENTATION_PLAN.md (unit PF04)

use anyhow::Result;
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::SqryQueryParams;
use sqry_mcp::tool_handlers::execute_sqry_query;
use std::fs;
use std::num::NonZeroUsize;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

/// Initialize the path-resolver discovery cache, engine cache, and
/// trace-path / subgraph telemetry caches exactly once across the whole
/// test binary. Mirrors `migration_golden_relations_test.rs::init_caches`.
fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

/// Write a minimal Rust workspace under `temp` so the auto-indexer has
/// real source to parse.
fn write_fixture() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let root = temp.path();
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"pf04_mcp\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"src/lib.rs\"\n",
    )?;
    fs::write(
        root.join("src/lib.rs"),
        "pub fn helper() -> u32 { 42 }\npub fn caller() -> u32 { helper() + helper() }\n",
    )?;
    Ok(temp)
}

#[test]
fn pf04_mcp_sqry_query_does_not_create_derived_sqry() -> Result<()> {
    init_caches();

    let temp = write_fixture()?;
    let workspace = temp.path();

    // Auto-index the workspace via the live MCP engine — this is what
    // every MCP tool call does on first invocation. It builds and saves
    // snapshot.sqry but is NOT permitted to create derived.sqry.
    let engine = engine_for_workspace(Some(&workspace.to_path_buf()))?;
    let _graph = engine.ensure_graph()?;

    let snapshot_path = workspace.join(".sqry").join("graph").join("snapshot.sqry");
    let derived_path = workspace.join(".sqry").join("graph").join("derived.sqry");
    assert!(
        snapshot_path.exists(),
        "precondition: ensure_graph must produce snapshot.sqry; got missing {}",
        snapshot_path.display()
    );
    assert!(
        !derived_path.exists(),
        "precondition: ensure_graph must NOT produce derived.sqry; \
         derived-cache writer leaked into MCP auto-index path (file: {})",
        derived_path.display()
    );

    // Drive the MCP `sqry_query` tool against the freshly-indexed
    // workspace. This is the canonical reader entry point — it calls
    // `make_query_db_cold(snapshot, &workspace_root)` and
    // `execute_plan(...)` then formats results.
    let params = SqryQueryParams {
        query: "kind:function".to_string(),
        path: workspace.to_string_lossy().into_owned(),
        limit: Some(100),
        budget_rows: None,
    };
    let _result = execute_sqry_query(&params)?;

    assert!(
        !derived_path.exists(),
        "PF04 contract violation: MCP `sqry_query` tool created derived.sqry at {}. \
         CLI/LSP/MCP must be reader-only.",
        derived_path.display()
    );

    Ok(())
}
