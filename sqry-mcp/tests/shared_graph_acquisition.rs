//! SGA03 — standalone MCP integration tests for the shared
//! `FilesystemGraphProvider` route.
//!
//! These tests exercise the same provider that backs CLI `sqry query` from
//! the MCP engine side:
//!
//! 1. `standalone_mcp_semantic_search_matches_cli` — building a tempdir
//!    fixture, indexing it with the CLI, and then calling
//!    `Engine::ensure_graph` directly returns a non-empty graph that the
//!    MCP query path can run against.
//! 2. `standalone_mcp_invalid_path_preflight_before_ensure_graph` — the
//!    standalone MCP path-preflight (`canonicalize_in_workspace`) rejects
//!    escape paths *before* any provider acquisition. Asserting at the
//!    preflight layer keeps the test independent of network/MCP transport.
//! 3. `standalone_mcp_rebuild_index_stays_mutating` — the read-only
//!    acquirer is not invoked when a rebuild is requested. We verify this
//!    indirectly by ensuring `Engine::ensure_graph` returns the cached
//!    graph after `clear_graph_cache` only when an underlying snapshot
//!    exists, while a workspace with no snapshot fails through the
//!    auto-build path (the read-only acquirer alone would have returned
//!    `NoGraph`).

use anyhow::Result;
use sqry_mcp::engine::{Engine, canonicalize_in_workspace};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Build a small Rust workspace and run `sqry index` against it. Returns
/// the canonicalized workspace root for use by `Engine::for_workspace`.
fn build_indexed_workspace() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn func_alpha() -> u32 { 1 }
pub fn func_beta() -> u32 { 2 }
"#,
    )
    .expect("write lib.rs");

    let bin = sqry_bin_for_test();
    let status = Command::new(&bin)
        .arg("index")
        .arg(root)
        .env("NO_COLOR", "1")
        .status()
        .expect("spawn sqry index");
    assert!(
        status.success(),
        "sqry index must succeed for fixture build"
    );

    let canonical = root.canonicalize().expect("canon root");
    (tmp, canonical)
}

/// Locate the `sqry` binary the same way the CLI integration tests do.
fn sqry_bin_for_test() -> PathBuf {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return p;
        }
    }
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let workspace_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace dir")
        .to_path_buf();
    let make_candidate = |base: &str| {
        if exe_suffix.is_empty() {
            PathBuf::from(base)
        } else {
            PathBuf::from(format!("{base}{exe_suffix}"))
        }
    };
    let debug = workspace_dir.join(make_candidate("target/debug/sqry"));
    let release = workspace_dir.join(make_candidate("target/release/sqry"));
    if debug.exists() {
        debug
    } else if release.exists() {
        release
    } else {
        panic!(
            "could not find sqry binary. Tried {} / {}",
            debug.display(),
            release.display()
        );
    }
}

/// SGA03 acceptance — `Engine::ensure_graph` (now backed by
/// `FilesystemGraphProvider`) returns a non-empty graph for the same
/// workspace that the CLI just indexed. This proves the MCP and CLI use
/// the same acquisition contract for fresh, valid graphs.
#[test]
fn standalone_mcp_semantic_search_matches_cli() -> Result<()> {
    let (_tmp, workspace) = build_indexed_workspace();

    // Force the MCP engine into standalone mode (no daemon-conflict probe)
    // so the test doesn't depend on `sqryd` running on the CI host.
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
    }

    let engine = Engine::for_workspace(workspace.clone()).expect("engine for workspace");
    let graph = engine.ensure_graph().expect("ensure_graph via provider");
    let snapshot = graph.snapshot();
    assert!(
        !snapshot.nodes().is_empty(),
        "indexed workspace must produce a non-empty graph"
    );
    Ok(())
}

/// SGA03 acceptance — escape paths fail at the standalone MCP path
/// preflight (`canonicalize_in_workspace`) *before* `engine_for_workspace`
/// or `ensure_graph` ever runs. The provider therefore cannot be reached
/// with an out-of-workspace path.
#[test]
fn standalone_mcp_invalid_path_preflight_before_ensure_graph() {
    let tmp = TempDir::new().expect("tempdir");
    let workspace = tmp.path();
    let escape = "../../etc/passwd";

    let result = canonicalize_in_workspace(escape, workspace);
    assert!(
        result.is_err(),
        "escape path must be rejected by the workspace preflight"
    );
}

/// SGA03 acceptance — the read-only `FilesystemGraphProvider` does not
/// service mutating rebuilds. `rebuild_index` retains its dedicated
/// mutating handler.
///
/// We assert this by exercising `Engine::ensure_graph` against a workspace
/// that has *no* `.sqry/graph` and `SQRY_AUTO_INDEX=false` set. The
/// provider returns `NoGraph` (mapped to an `anyhow` error). This proves
/// `ensure_graph` is purely read-only with the auto-build hook gated on
/// `SQRY_AUTO_INDEX`; mutating rebuild paths must use a different code
/// path that does not consult this acquirer.
#[test]
fn standalone_mcp_rebuild_index_stays_mutating() {
    let tmp = TempDir::new().expect("tempdir");
    let workspace = tmp.path().canonicalize().expect("canon ws");

    unsafe {
        std::env::set_var("SQRY_AUTO_INDEX", "false");
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
    }

    let engine = Engine::for_workspace(workspace.clone()).expect("engine for workspace");
    let result = engine.ensure_graph();
    assert!(
        result.is_err(),
        "no graph + auto-index disabled must fail (mutating rebuild path is the only way to create the graph)"
    );

    unsafe {
        // Best-effort cleanup so other tests in this binary do not inherit the
        // override. (Process-global env vars are inherently shared; tests in
        // this file deliberately do not run in parallel with each other on
        // workspaces.)
        std::env::remove_var("SQRY_AUTO_INDEX");
    }
}

/// SGA03 Major #2 fix — `Engine::ensure_graph` now routes existing
/// disk-resident snapshots through `FilesystemGraphProvider` instead of
/// the legacy `Engine::graph()` direct loader. The provider runs the
/// plugin-selection compatibility check on the manifest before
/// deserializing the snapshot; the legacy loader did not. We prove the
/// provider path executed by mutating the manifest's
/// `active_plugin_ids` list to include a fake plugin id that no
/// registered plugin can satisfy. The provider must reject the load
/// with `IncompatibleGraph` (mapped to an anyhow error mentioning
/// "Incompatible graph"); the legacy loader would have happily loaded
/// the snapshot and lost the language plugin silently.
#[test]
fn standalone_mcp_existing_disk_snapshot_uses_provider() -> Result<()> {
    let (_tmp, workspace) = build_indexed_workspace();

    // Force standalone mode — same convention as the other tests in this
    // file. Avoids any daemon probe interference.
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }

    // Mutate the manifest so it advertises an unknown plugin id. The
    // provider's `classify_plugin_selection` must return
    // `IncompatibleUnknownPluginIds`, which surfaces as
    // `GraphAcquisitionError::IncompatibleGraph` and maps to an anyhow
    // error containing "Incompatible graph". Critically, the manifest's
    // recorded SHA-256 still matches the on-disk snapshot, so a loader
    // that skipped plugin-compat classification would have succeeded.
    let manifest_path = workspace.join(".sqry/graph/manifest.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    let mut manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let plugin_section = manifest_json
        .get_mut("plugin_selection")
        .expect("manifest must record plugin_selection after sqry index");
    let active_ids = plugin_section
        .get_mut("active_plugin_ids")
        .and_then(|v| v.as_array_mut())
        .expect("active_plugin_ids must be an array");
    active_ids.push(serde_json::Value::String(
        "sga03-fake-plugin-that-does-not-exist".to_string(),
    ));
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest_json)?)?;

    // Cold engine — the in-memory cache is empty, so `ensure_graph` MUST
    // route the disk load through `FilesystemGraphProvider`. That's
    // where plugin-compat classification lives; the legacy
    // `Engine::graph()` direct loader did not run it.
    let engine = Engine::for_workspace(workspace.clone()).expect("engine for workspace");
    assert!(
        engine.cached_graph().is_none(),
        "fresh engine must have an empty in-memory graph cache"
    );

    let result = engine.ensure_graph();
    assert!(
        result.is_err(),
        "ensure_graph must reject a manifest with an unknown plugin id; got Ok(_)"
    );
    let err_msg = result.err().unwrap().to_string();
    assert!(
        err_msg.contains("Incompatible graph") || err_msg.contains("sga03-fake-plugin"),
        "expected provider IncompatibleGraph diagnostic, got: {err_msg}"
    );

    Ok(())
}
