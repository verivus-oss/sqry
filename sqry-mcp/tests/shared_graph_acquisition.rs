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
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A temp directory that is its own project: an empty `.git` marker at
/// its root stops the ancestor walk there, so an index or project marker
/// above `TMPDIR` cannot make `sqry index` refuse the fixture as a nested
/// index or hand the test an unrelated outer graph.
fn project_tempdir() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir(dir.path().join(".git")).expect("project marker");
    dir
}

/// Serializes the tests in this file that mutate the process-global
/// `SQRY_AUTO_INDEX` / `SQRY_FORCE_STANDALONE` env vars. Without this, a
/// concurrent test clearing `SQRY_AUTO_INDEX` re-enables auto-index inside
/// `standalone_mcp_rebuild_index_stays_mutating` and flips its expected
/// failure into a success (the long-standing flake). Poison is recovered so a
/// panicking assertion in one test does not cascade-fail the rest of the binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Build a small Rust workspace and run `sqry index` against it. Returns
/// the canonicalized workspace root for use by `Engine::for_workspace`.
fn build_indexed_workspace() -> (TempDir, PathBuf) {
    let tmp = project_tempdir();
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

/// Locate the `sqry` binary for testing.
///
/// Delegates to the one resolver, `sqry_core::test_support::binaries::sqry_binary`
/// (surface parity W4, design W4-D13), which reads `SQRY_E2E_SQRY_BIN`, then
/// `CARGO_BIN_EXE_sqry`, then `CARGO_TARGET_DIR` and the workspace `target`,
/// debug before release, and panics naming every variable and candidate.
fn sqry_bin_for_test() -> PathBuf {
    sqry_core::test_support::binaries::sqry_binary()
}

/// SGA03 acceptance — `Engine::ensure_graph` (now backed by
/// `FilesystemGraphProvider`) returns a non-empty graph for the same
/// workspace that the CLI just indexed. This proves the MCP and CLI use
/// the same acquisition contract for fresh, valid graphs.
#[test]
fn standalone_mcp_semantic_search_matches_cli() -> Result<()> {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let tmp = project_tempdir();
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
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = project_tempdir();
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
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// T15 (surface parity W1): a forced standalone `rebuild_index` over an
/// `include_all` index rebuilds with the manifest roster and keeps both the
/// `json` id and `high_cost_mode: include_all`. On the pre-change head the
/// tool rebuilt with the fast-path manager and rewrote the manifest without
/// `json` and without a mode.
#[test]
fn standalone_mcp_forced_rebuild_index_keeps_include_all_selection() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }

    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canon root");
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn func_alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
    fs::write(
        root.join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("write config.json");

    let status = Command::new(sqry_bin_for_test())
        .arg("index")
        .arg("--include-high-cost")
        .arg(&root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .status()
        .expect("spawn sqry index --include-high-cost");
    assert!(status.success(), "sqry index --include-high-cost failed");

    let manifest_path = root.join(".sqry/graph/manifest.json");
    let read_selection = || -> (Vec<String>, Option<String>) {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
                .expect("manifest json");
        let selection = &manifest["plugin_selection"];
        let ids = selection["active_plugin_ids"]
            .as_array()
            .expect("active_plugin_ids array")
            .iter()
            .map(|id| id.as_str().expect("id string").to_string())
            .collect();
        let mode = selection["high_cost_mode"].as_str().map(str::to_string);
        (ids, mode)
    };
    let (ids_before, mode_before) = read_selection();
    assert!(
        ids_before.iter().any(|id| id == "json"),
        "fixture precondition: the CLI must record json, got {ids_before:?}"
    );
    assert_eq!(mode_before.as_deref(), Some("include_all"));

    // The standalone tool path reads the engine / discovery caches the
    // `sqry-mcp` binary initialises in `main` (and `IpcServer::bind`
    // initialises for the daemon); the initialisers are idempotent.
    {
        use std::num::NonZeroUsize;
        sqry_mcp::test_setup::init_discovery_cache(NonZeroUsize::new(64).unwrap());
        sqry_mcp::test_setup::init_engine_cache(NonZeroUsize::new(8).unwrap());
        sqry_mcp::test_setup::init_trace_path_cache(
            NonZeroUsize::new(64).unwrap(),
            std::time::Duration::from_secs(60),
        );
        sqry_mcp::test_setup::init_subgraph_cache(
            NonZeroUsize::new(64).unwrap(),
            std::time::Duration::from_secs(60),
        );
    }
    // Bind the fixture root the way the server binds every request
    // (`with_workspace_override` before tool execution).
    let execution = sqry_mcp::workspace_session_test_api::with_workspace_override(
        Some(&root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(&root),
        || {
            sqry_mcp::execution::execute_rebuild_index(&sqry_mcp::tool_args::RebuildIndexArgs {
                path: root.to_string_lossy().to_string(),
                force: true,
                cfg_flags: None,
                expand_cache: None,
                reset_macro_options: false,
            })
        },
    )
    .expect("forced rebuild_index runs");
    assert!(execution.data.success);
    assert!(
        execution.data.node_count > 0,
        "a rebuilt graph must have nodes"
    );

    let (ids_after, mode_after) = read_selection();
    assert_eq!(
        ids_after, ids_before,
        "a forced rebuild must record exactly the selection the manifest already recorded"
    );
    assert_eq!(
        mode_after.as_deref(),
        Some("include_all"),
        "high_cost_mode must be carried through the rebuild, not dropped"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

/// Verifier round 1, battery row M18: the standalone auto-build of a
/// brand-new index (no manifest) records the fast-path selection, so a
/// fresh index is not silently built with high-cost plugins.
#[test]
fn standalone_mcp_auto_build_records_the_fast_path_selection() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("SQRY_AUTO_INDEX");
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
    }
    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canon root");
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn func_alpha() -> u32 { 1 }\n",
    )
    .expect("lib.rs");
    fs::write(
        root.join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("config.json");
    assert!(
        !root.join(".sqry").exists(),
        "fixture precondition: no index yet"
    );

    let engine = Engine::for_workspace(root.clone()).expect("engine for workspace");
    let graph = engine
        .ensure_graph()
        .expect("auto-build of a brand-new index succeeds");

    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest written"),
    )
    .expect("manifest json");
    let selection = &manifest["plugin_selection"];
    let ids: Vec<String> = selection["active_plugin_ids"]
        .as_array()
        .expect("active_plugin_ids array")
        .iter()
        .map(|id| id.as_str().expect("id string").to_string())
        .collect();
    assert!(
        !ids.iter().any(|id| id == "json"),
        "a brand-new index must be built with the fast path, got {ids:?}"
    );
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("fast_path_default"),
        "the fallback mode must be recorded: {selection}"
    );
    let json_nodes = sqry_core::query::QueryExecutor::with_plugin_manager(
        sqry_plugin_registry::create_plugin_manager_all(),
    )
    .execute_on_preloaded_graph(graph, "lang:json", &root, None)
    .expect("lang:json runs")
    .len();
    assert_eq!(
        json_nodes, 0,
        "the fast-path auto-build must contain no json nodes"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

/// A `tracing` writer that appends to a shared buffer, so a test can read
/// back what a tool logged without a global subscriber.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// T24 (surface parity W1 round 2, design D9 MCP row, declared control,
/// green on both heads): a forced standalone `rebuild_index` over an index
/// whose manifest cannot be read rebuilds with the fast path, records that
/// fallback selection, and logs a warning naming the manifest.
#[test]
fn standalone_mcp_forced_rebuild_index_over_unreadable_manifest_records_the_fallback_control() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }

    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canon root");
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn func_alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
    fs::write(
        root.join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("write config.json");
    let manifest_path = root.join(".sqry/graph/manifest.json");
    fs::create_dir_all(manifest_path.parent().expect("graph dir")).expect("mkdir graph");
    fs::write(&manifest_path, b"{}").expect("unparseable manifest");

    {
        use std::num::NonZeroUsize;
        sqry_mcp::test_setup::init_discovery_cache(NonZeroUsize::new(64).unwrap());
        sqry_mcp::test_setup::init_engine_cache(NonZeroUsize::new(8).unwrap());
        sqry_mcp::test_setup::init_trace_path_cache(
            NonZeroUsize::new(64).unwrap(),
            std::time::Duration::from_secs(60),
        );
        sqry_mcp::test_setup::init_subgraph_cache(
            NonZeroUsize::new(64).unwrap(),
            std::time::Duration::from_secs(60),
        );
    }

    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    let execution = tracing::subscriber::with_default(subscriber, || {
        sqry_mcp::workspace_session_test_api::with_workspace_override(
            Some(&root),
            sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(&root),
            || {
                sqry_mcp::execution::execute_rebuild_index(&sqry_mcp::tool_args::RebuildIndexArgs {
                    path: root.to_string_lossy().to_string(),
                    force: true,
                    cfg_flags: None,
                    expand_cache: None,
                    reset_macro_options: false,
                })
            },
        )
    })
    .expect("a forced rebuild_index over an unreadable manifest rebuilds");
    assert!(execution.data.success);
    assert!(execution.data.node_count > 0, "a rebuilt graph has nodes");

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
            .expect("manifest json after the rebuild");
    let selection = &manifest["plugin_selection"];
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("fast_path_default"),
        "the fallback selection must be recorded: {selection}"
    );
    let ids: Vec<&str> = selection["active_plugin_ids"]
        .as_array()
        .expect("active_plugin_ids array")
        .iter()
        .map(|id| id.as_str().expect("id string"))
        .collect();
    assert!(
        !ids.contains(&"json"),
        "the fallback is the fast path: {ids:?}"
    );

    let log = String::from_utf8(
        captured
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
    )
    .expect("utf-8 log");
    assert!(
        log.contains("manifest unreadable") && log.contains(&manifest_path.display().to_string()),
        "the tool must log the fallback naming the manifest; log was: {log:?}"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

/// Initialise the standalone caches the tool executors read, once per
/// process (idempotent: the `test_setup` functions ignore a second call).
fn init_standalone_caches() {
    use std::num::NonZeroUsize;
    sqry_mcp::test_setup::init_discovery_cache(NonZeroUsize::new(64).unwrap());
    sqry_mcp::test_setup::init_engine_cache(NonZeroUsize::new(8).unwrap());
    sqry_mcp::test_setup::init_trace_path_cache(
        NonZeroUsize::new(64).unwrap(),
        std::time::Duration::from_secs(60),
    );
    sqry_mcp::test_setup::init_subgraph_cache(
        NonZeroUsize::new(64).unwrap(),
        std::time::Duration::from_secs(60),
    );
}

/// Sorted listing of every file under `<root>/.sqry` with its bytes.
fn index_dir_listing(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &std::path::Path, prefix: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(prefix).expect("under prefix");
            if path.is_dir() {
                walk(&path, prefix, out);
            } else {
                out.push((
                    rel.display().to_string(),
                    fs::read(&path).expect("file bytes"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(".sqry"), root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

const R3_PLANTED_ID: &str = "w1-r3-planted-plugin";

/// Run the standalone `rebuild_index` tool in-process over `root` with
/// `force`, under the workspace override the standalone tools resolve.
fn run_rebuild_index(
    root: &std::path::Path,
    force: bool,
) -> Result<sqry_mcp::execution::ToolExecution<sqry_mcp::execution::RebuildIndexData>> {
    sqry_mcp::workspace_session_test_api::with_workspace_override(
        Some(root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(root),
        || {
            sqry_mcp::execution::execute_rebuild_index(&sqry_mcp::tool_args::RebuildIndexArgs {
                path: root.to_string_lossy().to_string(),
                force,
                cfg_flags: None,
                expand_cache: None,
                reset_macro_options: false,
            })
        },
    )
}

/// T32b (surface parity W1 round 3, design D17, codex Finding 3): the
/// standalone `rebuild_index` with `force: false` over an existing index
/// classifies the manifest through the same resolver the read path uses
/// before it reports the index. A planted id is refused by name and with
/// the manifest path; an unreadable manifest is refused naming the file;
/// the `.sqry` listing is unchanged by either. On `416debe48` the planted
/// id gave `Ok` with "Index already exists. Use force=true to rebuild." and
/// `{}` gave "Index exists but manifest is unreadable" without the path.
#[test]
fn standalone_rebuild_index_cache_hit_classifies_the_manifest() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }
    assert!(
        sqry_plugin_registry::create_plugin_manager_all()
            .plugin_by_id(R3_PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    init_standalone_caches();

    let (_tmp, root) = build_indexed_workspace();
    let manifest_path = root.join(".sqry/graph/manifest.json");
    let manifest_text = manifest_path.display().to_string();

    // Leg 1: a planted id.
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("active_plugin_ids array")
        .push(serde_json::Value::String(R3_PLANTED_ID.to_string()));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("manifest rewritten");
    let listing_planted = index_dir_listing(&root);
    let err = match run_rebuild_index(&root, false) {
        Ok(execution) => panic!(
            "rebuild_index without force must refuse the planted id; survived {:?}",
            execution.data.message
        ),
        Err(err) => format!("{err:#}"),
    };
    assert!(err.contains(R3_PLANTED_ID), "must name the id: {err}");
    assert!(
        err.contains(&manifest_text),
        "must name the manifest: {err}"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "the refusal must change nothing under .sqry"
    );

    // Leg 2: an unreadable manifest on the same root. Leg 1 may have
    // populated the standalone engine cache for this root; `rebuild_index`
    // resolves its root without the engine (whose freshness refresh would
    // refuse first, with no envelope), so the refusal is the cache-hit
    // leg's own and must name the file too.
    fs::write(&manifest_path, b"{}").expect("unparseable manifest");
    let listing_unreadable = index_dir_listing(&root);
    let err = match run_rebuild_index(&root, false) {
        Ok(execution) => panic!(
            "rebuild_index without force must refuse an unreadable manifest; survived {:?}",
            execution.data.message
        ),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        err.contains(&manifest_text),
        "must name the manifest path: {err}"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_unreadable,
        "the refusal must change nothing under .sqry"
    );

    // Leg 3: an unreadable manifest on a fresh root (no cached engine). The
    // registry resolver refuses it on the cache-hit leg, and the wire
    // carries the daemon-hosted MCP's envelope for that refusal:
    // "manifest at <path> cannot be read (..); repair with: ...".
    let (_tmp_fresh, fresh_root) = build_indexed_workspace();
    let fresh_manifest = fresh_root.join(".sqry/graph/manifest.json");
    fs::write(&fresh_manifest, b"{}").expect("unparseable manifest");
    let listing_fresh = index_dir_listing(&fresh_root);
    let err = match run_rebuild_index(&fresh_root, false) {
        Ok(execution) => panic!(
            "rebuild_index without force must refuse an unreadable manifest; survived {:?}",
            execution.data.message
        ),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        err.contains(&format!(
            "manifest at {} cannot be read (",
            fresh_manifest.display()
        )) && err.contains("repair with: sqry index --force"),
        "the resolver's refusal names the file and the repair: {err}"
    );
    assert_eq!(
        index_dir_listing(&fresh_root),
        listing_fresh,
        "the refusal must change nothing under .sqry"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

/// SHA-256 of a file's bytes, hex.
fn sha256_of(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).expect("file bytes");
    Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Rewrite the recorded selection at `root` to `include_all` with every
/// compiled id, then corrupt the snapshot. Returns the manifest's SHA-256.
fn plant_include_all_beside_a_corrupt_snapshot(root: &std::path::Path) -> String {
    let manifest_path = root.join(".sqry/graph/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    let full_ids: Vec<serde_json::Value> = sqry_plugin_registry::create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| serde_json::Value::String(plugin.metadata().id.to_string()))
        .collect();
    manifest["plugin_selection"]["active_plugin_ids"] = serde_json::Value::Array(full_ids);
    manifest["plugin_selection"]["high_cost_mode"] =
        serde_json::Value::String("include_all".to_string());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("manifest rewritten");
    fs::write(
        root.join(".sqry/graph/snapshot.sqry"),
        b"not a sqry snapshot",
    )
    .expect("corrupt snapshot");
    sha256_of(&manifest_path)
}

fn hierarchical_args(path: &std::path::Path) -> sqry_mcp::tool_args::HierarchicalSearchArgs {
    use sqry_mcp::tool_args::{HierarchicalSearchArgs, PaginationArgs, SearchFilters};
    HierarchicalSearchArgs {
        query: "kind:function".to_string(),
        path: path.to_string_lossy().to_string(),
        max_results: 100,
        max_total_symbols: 500,
        max_files: 20,
        max_containers_per_file: 50,
        max_symbols_per_container: 100,
        context_lines: 0,
        score_min: None,
        filters: SearchFilters {
            languages: Vec::new(),
            kinds: Vec::new(),
            visibility: None,
            min_score: None,
            cfg_condition: None,
        },
        pagination: PaginationArgs {
            offset: 0,
            size: 10,
        },
        expand_files: Vec::new(),
        file_target_tokens: 2000,
        container_target_tokens: 800,
        symbol_target_tokens: 500,
        context_cluster_target_tokens: 768,
        merge_threshold: 5,
        auto_merge: true,
        include_file_context: false,
        include_container_context: false,
        budget_rows: None,
    }
}

/// T39 (surface parity W1 round 3, design D18, section 2.6 Q6): the
/// standalone `hierarchical_search` over an `include_all` manifest beside a
/// snapshot that cannot load returns an error and rewrites nothing: the
/// manifest's SHA-256 before equals after. The executor behind the tool
/// carries no build hook, so the refusal the engine's provider gives for a
/// corrupt snapshot is the answer; on `28f7337e1` an executor that reached
/// its own load-failure branch would have rebuilt with its full roster and
/// rewritten the manifest with `high_cost_mode: None`. Whether that branch
/// is reachable through this tool at all (the provider refuses first) is
/// measured by the red check and recorded; the SHA leg is the oracle either
/// way.
#[test]
fn standalone_hierarchical_search_over_a_corrupt_snapshot_rewrites_nothing() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }
    init_standalone_caches();

    let (_tmp, root) = build_indexed_workspace();
    let sha_before = plant_include_all_beside_a_corrupt_snapshot(&root);
    let manifest_path = root.join(".sqry/graph/manifest.json");

    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let outcome = sqry_mcp::workspace_session_test_api::with_workspace_override(
        Some(&root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(&root),
        || sqry_mcp::execution::execute_hierarchical_search(&hierarchical_args(&root), &cancel),
    );
    let err = match outcome {
        Ok(execution) => panic!(
            "hierarchical_search over a corrupt snapshot must not serve; survived total={:?}",
            execution.total
        ),
        Err(err) => format!("{err:#}"),
    };
    println!("T39 refusal: {err}");
    assert_eq!(
        sha256_of(&manifest_path),
        sha_before,
        "the manifest must not be rewritten: {}",
        fs::read_to_string(&manifest_path).expect("manifest text")
    );
    let manifest_after: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest bytes"))
            .expect("manifest json");
    let selection = manifest_after["plugin_selection"].clone();
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("include_all"),
        "the recorded mode is untouched: {selection}"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

/// T39b (surface parity W1 round 3, design D18, section 2.6 Q6): the
/// standalone `hierarchical_search` with a subdirectory `path` over a valid
/// index passes that subdirectory to the executor's own
/// `get_or_load_graph`, which finds no index there. With no hook the
/// executor builds nothing, so no nested `<root>/src/.sqry` may appear; the
/// call's outcome is printed and recorded, not pinned, because which answer
/// the tool gives for a subdirectory is W3's question (the executor's own
/// load path, design 2.5), and this test's oracle is the absence of a
/// nested index. Measured on `28f7337e1` by the red check and recorded.
#[test]
fn standalone_hierarchical_search_over_a_subdirectory_builds_no_nested_index() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
        std::env::remove_var("SQRY_AUTO_INDEX");
    }
    init_standalone_caches();

    let (_tmp, root) = build_indexed_workspace();
    let subdir = root.join("src");
    let root_manifest_sha = sha256_of(&root.join(".sqry/graph/manifest.json"));
    assert!(
        !subdir.join(".sqry").exists(),
        "fixture precondition: no nested index"
    );

    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let outcome = sqry_mcp::workspace_session_test_api::with_workspace_override(
        Some(&root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(&root),
        || sqry_mcp::execution::execute_hierarchical_search(&hierarchical_args(&subdir), &cancel),
    );
    match &outcome {
        Ok(execution) => println!("T39b outcome: Ok total={:?}", execution.total),
        Err(err) => println!("T39b outcome: Err {err:#}"),
    }
    assert!(
        !subdir.join(".sqry").exists(),
        "the executor must not build a nested index under the subdirectory; found {:?}",
        fs::read_to_string(subdir.join(".sqry/graph/manifest.json"))
    );
    assert_eq!(
        sha256_of(&root.join(".sqry/graph/manifest.json")),
        root_manifest_sha,
        "the workspace manifest must be untouched"
    );

    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
}

// ---------------------------------------------------------------------------
// Surface parity W4 (design W4-D7, W4-D8): the standalone `rebuild_index`
// accepts the macro build options, records them, reuses the record and
// refuses a recorded expand cache directory that no longer exists.
// ---------------------------------------------------------------------------

const W4_CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";

fn w4_cfg_fixture() -> (TempDir, PathBuf) {
    let tmp = project_tempdir();
    fs::create_dir_all(tmp.path().join("src")).expect("src dir");
    fs::write(tmp.path().join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
    let root = tmp.path().canonicalize().expect("canonical root");
    (tmp, root)
}

fn w4_recorded_macro_options(
    root: &std::path::Path,
) -> Option<sqry_core::graph::unified::persistence::MacroOptionsManifest> {
    sqry_core::graph::unified::persistence::GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
}

fn w4_persisted_cfg_test_activation(root: &std::path::Path) -> Option<bool> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let graph =
        sqry_core::graph::unified::persistence::load_from_path(storage.snapshot_path(), None)
            .expect("snapshot loads");
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("recorded cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

/// Run the standalone `rebuild_index` in-process with explicit arguments.
fn run_rebuild_index_with(
    root: &std::path::Path,
    args: sqry_mcp::tool_args::RebuildIndexArgs,
) -> Result<sqry_mcp::execution::ToolExecution<sqry_mcp::execution::RebuildIndexData>> {
    sqry_mcp::workspace_session_test_api::with_workspace_override(
        Some(root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(root),
        || sqry_mcp::execution::execute_rebuild_index(&args),
    )
}

/// T13 (standalone leg) and T15 (MCP leg): `cfg_flags` is accepted and
/// recorded; a forced rebuild without arguments reuses the record; a macro
/// argument beside `force=false` over an existing index is refused;
/// `reset_macro_options` drops the record. On the pre-change head the
/// arguments did not exist on `RebuildIndexArgs` (compile error) and a
/// forced rebuild gave `cfg_active == None`.
#[test]
fn standalone_mcp_rebuild_index_accepts_records_and_reuses_the_macro_options() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    init_standalone_caches();
    let (_tmp, root) = w4_cfg_fixture();
    let path = root.to_string_lossy().to_string();

    run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path: path.clone(),
            force: true,
            cfg_flags: Some(vec!["test".to_string()]),
            expand_cache: None,
            reset_macro_options: false,
        },
    )
    .expect("rebuild_index with cfg_flags succeeds");
    let record = w4_recorded_macro_options(&root).expect("the rebuild records the flags");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    assert_eq!(w4_persisted_cfg_test_activation(&root), Some(true));

    run_rebuild_index(&root, true).expect("a forced rebuild without arguments succeeds");
    assert_eq!(
        w4_recorded_macro_options(&root).map(|r| r.cfg_flags),
        Some(vec!["test".to_string()]),
        "the forced rebuild reuses the record"
    );
    assert_eq!(
        w4_persisted_cfg_test_activation(&root),
        Some(true),
        "the forced rebuild runs with the recorded --cfg test"
    );

    let refused = match run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path: path.clone(),
            force: false,
            cfg_flags: None,
            expand_cache: None,
            reset_macro_options: true,
        },
    ) {
        Err(err) => err,
        Ok(_) => panic!("macro arguments beside force=false over an existing index are refused"),
    };
    let rendered = format!("{refused:#}");
    assert!(
        rendered.contains("force=true"),
        "the refusal names the way out: {rendered}"
    );
    assert_eq!(
        w4_recorded_macro_options(&root).map(|r| r.cfg_flags),
        Some(vec!["test".to_string()]),
        "a refused call writes nothing"
    );

    run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path,
            force: true,
            cfg_flags: None,
            expand_cache: None,
            reset_macro_options: true,
        },
    )
    .expect("rebuild_index with reset succeeds");
    assert!(
        w4_recorded_macro_options(&root).is_none(),
        "the reset drops the record"
    );
    assert_eq!(w4_persisted_cfg_test_activation(&root), None);
}

/// T14 (standalone `rebuild_index` leg): a recorded expand cache directory
/// that no longer exists is refused by name; nothing on disk changes.
#[test]
fn standalone_mcp_rebuild_index_refuses_a_missing_recorded_expand_cache() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    init_standalone_caches();
    let (_tmp, root) = w4_cfg_fixture();
    let cache = root.join("expand-cache");
    fs::create_dir_all(&cache).expect("cache dir");
    let path = root.to_string_lossy().to_string();

    run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path: path.clone(),
            force: true,
            cfg_flags: Some(vec!["test".to_string()]),
            expand_cache: Some(cache.clone()),
            reset_macro_options: false,
        },
    )
    .expect("rebuild_index with an existing expand cache succeeds");
    let recorded_dir = w4_recorded_macro_options(&root)
        .and_then(|record| record.expand_cache_dir)
        .expect("the manifest names the expand cache directory");
    assert_eq!(std::path::Path::new(&recorded_dir), cache.as_path());

    fs::remove_dir_all(&cache).expect("remove the cache");
    let listing_before = index_dir_listing(&root);

    let err = match run_rebuild_index(&root, true) {
        Err(err) => err,
        Ok(_) => panic!("a missing recorded expand cache must refuse"),
    };
    let rendered = format!("{err:#}");
    println!("refusal: {rendered}");
    assert!(
        rendered.contains(&recorded_dir)
            && rendered.contains("reset_macro_options: true")
            && !rendered.contains("--no-macro-options"),
        "the refusal names the directory and the MCP way out, not a CLI flag: {rendered}"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "nothing on disk changed"
    );

    run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path,
            force: true,
            cfg_flags: None,
            expand_cache: None,
            reset_macro_options: true,
        },
    )
    .expect("the reset is the way out");
    assert!(w4_recorded_macro_options(&root).is_none());
}

/// Integration of W1 and W4: an empty `expand_cache` names no directory and is
/// refused. Joined to the workspace root it named the root itself, which the
/// manifest then recorded as the expand cache for every later rebuild.
#[test]
fn standalone_mcp_rebuild_index_refuses_an_empty_expand_cache() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    init_standalone_caches();
    let (_tmp, root) = w4_cfg_fixture();
    run_rebuild_index(&root, true).expect("a plain rebuild indexes the fixture");
    assert!(
        w4_recorded_macro_options(&root).is_none(),
        "fixture precondition: no record"
    );
    let listing_before = index_dir_listing(&root);

    let err = match run_rebuild_index_with(
        &root,
        sqry_mcp::tool_args::RebuildIndexArgs {
            path: root.to_string_lossy().to_string(),
            force: true,
            cfg_flags: None,
            expand_cache: Some(std::path::PathBuf::new()),
            reset_macro_options: false,
        },
    ) {
        Err(err) => err,
        Ok(_) => panic!("an empty expand cache must be refused"),
    };
    let rendered = format!("{err:#}");
    println!("refusal: {rendered}");
    assert!(
        rendered.contains("expand cache directory is empty"),
        "the refusal says why: {rendered}"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_before,
        "nothing on disk changed"
    );
    assert!(
        w4_recorded_macro_options(&root).is_none(),
        "no expand cache was recorded"
    );
}
