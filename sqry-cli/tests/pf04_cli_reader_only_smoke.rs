//! PF04 — surface proof that the `sqry` CLI is a derived-cache READER only.
//!
//! Builds a real V10 snapshot in a temp workspace, runs `sqry plan-query`
//! as a subprocess against it, and asserts that no `derived.sqry` file is
//! created. The CLI's `plan-query` subcommand routes through
//! [`sqry_db::queries::dispatch::make_query_db_cold`] which is permitted
//! to *delete* a stale/corrupt derived-cache file but never to *write*
//! one. The writer lives exclusively in the daemon's `QueryDbHook`
//! (PF03B).
//!
//! Spec: docs/reviews/generational-design-analysis/2026-05-07/codex_in_code_verification_2026-05-07T030441Z.md
//! Plan: docs/development/generational-analysis-platform/priority-followups/03_IMPLEMENTATION_PLAN.md (unit PF04)

mod common;

use anyhow::{Context, Result};
use common::sqry_bin;
use std::path::Path;
use std::process::Command;

/// Build a real V10 snapshot in `root` so `sqry plan-query` finds an
/// index to query against. Mirrors the in-test fixture used by
/// `query_fixture_tests::build_graph_snapshot` but skips the manifest
/// (plan-query only needs the snapshot file to exist).
fn build_snapshot(root: &Path) -> Result<()> {
    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
    use sqry_core::graph::unified::persistence::{GraphStorage, save_to_path};
    use sqry_plugin_registry::create_plugin_manager;

    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    let graph = build_unified_graph(root, &plugins, &config)?;

    let storage = GraphStorage::new(root);
    std::fs::create_dir_all(storage.graph_dir())?;
    save_to_path(&graph, storage.snapshot_path())?;
    Ok(())
}

/// Write a minimal Rust workspace under `root` so the graph builder has
/// something to parse.
fn write_minimal_workspace(root: &Path) -> Result<()> {
    let src = root.join("src");
    std::fs::create_dir_all(&src)?;
    std::fs::write(
        src.join("lib.rs"),
        "pub fn helper() -> u32 { 42 }\npub fn caller() -> u32 { helper() + helper() }\n",
    )?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cli-pf04\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    Ok(())
}

#[test]
fn pf04_cli_plan_query_does_not_create_derived_sqry() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    write_minimal_workspace(tmp.path())?;
    build_snapshot(tmp.path()).context("build V10 snapshot for plan-query")?;

    let snapshot_path = tmp.path().join(".sqry").join("graph").join("snapshot.sqry");
    let derived_path = tmp.path().join(".sqry").join("graph").join("derived.sqry");
    assert!(
        snapshot_path.exists(),
        "precondition: snapshot.sqry must exist before invoking the CLI; got missing {}",
        snapshot_path.display()
    );
    assert!(
        !derived_path.exists(),
        "precondition: derived.sqry must NOT exist before invoking the CLI; \
         in-process snapshot setup leaked into a writer path"
    );

    // Drive the real CLI binary end-to-end, exactly the way users do.
    // `plan-query kind:function <path>` parses through the planner, builds
    // a `QueryDb` via `make_query_db_cold`, executes the plan, and prints
    // the matching nodes. No legitimate code path here is allowed to
    // write derived.sqry.
    let output = Command::new(sqry_bin())
        .args([
            "plan-query",
            "kind:function",
            tmp.path().to_string_lossy().as_ref(),
            "--limit",
            "100",
        ])
        .output()
        .context("execute sqry plan-query")?;

    assert!(
        output.status.success(),
        "sqry plan-query must succeed; stderr=\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !derived_path.exists(),
        "PF04 contract violation: `sqry plan-query` created derived.sqry at {}. \
         CLI/LSP/MCP must be reader-only.\nstdout:\n{}\nstderr:\n{}",
        derived_path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    Ok(())
}
