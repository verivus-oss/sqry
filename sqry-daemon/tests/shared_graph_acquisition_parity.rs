//! SGA05 / SGA07 — parity tests proving every daemon-hosted read-only
//! tool routes through the shared
//! [`DaemonGraphProvider`](sqry_daemon::workspace::acquirer) boundary,
//! that `WorkspaceEvicted` triggers the bounded one-shot reload before
//! the tool runs, that `rebuild_index` stays on its explicit mutating
//! path, and that an invalid `path` argument short-circuits before any
//! `classify_for_serve` work.
//!
//! These tests use the `test-hooks` feature to enable the process-wide
//! acquisition counter on
//! [`sqry_daemon::workspace::acquirer::DaemonGraphProvider`] (the
//! counter is `#[cfg(any(test, feature = "test-hooks"))]` and
//! unreachable in default release builds).
//!
//! Surface parity W1 adds the one-graph tests: T2 (daemon-hosted and
//! standalone `semantic_search` agree on a high-cost graph), T3 (the
//! ADD-form negative control: a manifest naming an uncompiled plugin is
//! refused on every surface with the same code) and T5a (the
//! `plugin_selection_warning` envelope key in both divergence directions).
//! Round 2 adds T17b (an evicted workspace whose manifest now names an
//! uncompiled id is refused by the reload itself and is not republished;
//! the discriminator is residency, since the acquirer already answered
//! `-32005` on `abefdd8e3`) and T22 (`daemon/load` over an unreadable
//! manifest is refused with `-32001` naming the file and the repair, and
//! nothing becomes resident).
//!
//! Run with:
//!
//! ```sh
//! cargo test -p sqry-daemon --features test-hooks --test \
//!     shared_graph_acquisition_parity
//! ```

#![allow(clippy::too_many_lines)]
// SGA05/07: this file uses `acquire_counter_*` and `evict_for_test`,
// which are gated on `#[cfg(any(test, feature = "test-hooks"))]`.
// The integration test binary inherits the crate's `cfg(test)` only
// when compiled as a test target, BUT the gated symbols are also
// `#[cfg(feature = "test-hooks")]`, and integration tests do not see
// the library crate's `cfg(test)`. We therefore require the
// `test-hooks` feature to compile this binary.
#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use serde_json::{Value, json};
use serial_test::serial;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{WorkspaceKey, WorkspaceState, acquire_counter_reset, acquire_counter_snapshot};
use support::insert_workspace_in_state;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

// ---------------------------------------------------------------------------
// Per-tool default arg shapes (mirrors `ipc_tool_method_surface.rs`).
// ---------------------------------------------------------------------------

/// Build a default arg JSON for the named tool against the supplied
/// canonical workspace `path`. Mirrors the shapes used by
/// `ipc_tool_method_surface.rs` so the same Loaded-empty-graph
/// fixtures continue to drive the dispatch.
fn default_args_for(name: &str, path: &str) -> Value {
    match name {
        "semantic_search" => json!({
            "query": "kind:function",
            "path": path,
            "max_results": 10,
            "context_lines": 0,
            "include_classpath": false,
        }),
        "relation_query" => json!({
            "symbol": "main",
            "relation_type": "callers",
            "path": path,
            "max_results": 10,
            "max_depth": 1,
            "page_size": 50,
        }),
        "direct_callers" => json!({
            "symbol": "main",
            "path": path,
            "max_results": 10,
        }),
        "direct_callees" => json!({
            "symbol": "main",
            "path": path,
            "max_results": 10,
        }),
        "find_unused" => json!({
            "path": path,
            "scope": "all",
            "language": [],
            "symbol_kind": [],
            "max_results": 10,
        }),
        "find_cycles" => json!({
            "path": path,
            "cycle_type": "calls",
            "max_results": 10,
            "min_depth": 2,
            "include_self_loops": false,
        }),
        "is_node_in_cycle" => json!({
            "symbol": "main",
            "path": path,
            "cycle_type": "calls",
            "min_depth": 2,
        }),
        "trace_path" => json!({
            "from_symbol": "main",
            "to_symbol": "main",
            "path": path,
            "max_hops": 5,
            "max_paths": 5,
        }),
        "subgraph" => json!({
            "symbols": ["main"],
            "path": path,
            "max_depth": 2,
            "max_nodes": 10,
            "page_size": 50,
        }),
        "export_graph" => json!({
            "path": path,
            "symbol_name": "main",
            "format": "json",
            "max_depth": 2,
            "max_results": 10,
            "page_size": 200,
        }),
        "complexity_metrics" => json!({
            "path": path,
            "max_results": 10,
        }),
        "semantic_diff" => json!({
            "path": path,
            "base": {"ref": "HEAD~1"},
            "target": {"ref": "HEAD"},
            "max_results": 10,
            "page_size": 100,
        }),
        "dependency_impact" => json!({
            "symbol": "main",
            "path": path,
            "max_depth": 3,
            "max_results": 10,
            "page_size": 100,
        }),
        "show_dependencies" => json!({
            "symbol_name": "main",
            "path": path,
            "max_depth": 2,
            "max_results": 10,
            "page_size": 100,
        }),
        other => panic!("default_args_for: unknown tool {other}"),
    }
}

/// The 14 read-only daemon-hosted MCP tools that SGA05 migrates onto
/// the shared acquisition boundary. `rebuild_index` is explicitly
/// excluded (it mutates), so together with `rebuild_index` these form
/// the 15 daemon-hosted MCP tools.
const READ_ONLY_TOOLS: &[&str] = &[
    "complexity_metrics",
    "dependency_impact",
    "direct_callees",
    "direct_callers",
    "export_graph",
    "find_cycles",
    "find_unused",
    "is_node_in_cycle",
    "relation_query",
    "semantic_diff",
    "semantic_search",
    "show_dependencies",
    "subgraph",
    "trace_path",
];

// ---------------------------------------------------------------------------
// Surface parity W1 fixture helpers (one `.rs` plus one `.json` file, so the
// fast-path roster and the full roster build different graphs).
// ---------------------------------------------------------------------------

fn write_mixed_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn func_alpha() -> u32 { 1 }\n\
          pub fn func_beta() -> u32 { 2 }\n\
          pub fn func_gamma() -> u32 { 3 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "nested": {"enabled": true, "count": 3}, "items": [1, 2]}"#,
    )
    .expect("write config.json");
}

fn plugin_ids_of(plugins: &sqry_core::plugin::PluginManager) -> Vec<String> {
    plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// Persist an index the way `sqry index` does with the given manager and
/// recorded selection.
fn index_with(
    root: &std::path::Path,
    plugins: &sqry_core::plugin::PluginManager,
    high_cost_mode: &str,
) {
    sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
        root,
        plugins,
        &sqry_core::graph::unified::build::BuildConfig::default(),
        "test:parity",
        Some(
            sqry_core::graph::unified::persistence::PluginSelectionManifest {
                active_plugin_ids: plugin_ids_of(plugins),
                high_cost_mode: Some(high_cost_mode.to_string()),
            },
        ),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("index persists");
}

/// N: the `lang:json` symbol count of a full-roster build of the fixture,
/// computed by the test from the fixture rather than hardcoded. The raw
/// executor also returns the per-file `<module>` node (kind `Module`) that
/// the CLI prints; the MCP `semantic_search` body on both transports
/// reports symbols and does not list that node, so N excludes it. The
/// exclusion is asserted, not assumed: the executor must return exactly
/// N + 1 rows for the fixture.
fn expected_json_count(root: &std::path::Path) -> usize {
    use sqry_core::graph::unified::node::NodeKind;

    let plugins = sqry_plugin_registry::create_plugin_manager_all();
    let graph = sqry_core::graph::unified::build::build_unified_graph(
        root,
        &plugins,
        &sqry_core::graph::unified::build::BuildConfig::default(),
    )
    .expect("reference build");
    let executor = sqry_core::query::QueryExecutor::with_plugin_manager(plugins);
    let rows = executor
        .execute_on_preloaded_graph(std::sync::Arc::new(graph), "lang:json", root, None)
        .expect("lang:json runs");
    let module_rows = rows
        .iter()
        .filter(|hit| hit.kind() == NodeKind::Module)
        .count();
    assert_eq!(
        module_rows, 1,
        "the fixture has one json file, so exactly one module row is expected"
    );
    rows.len() - module_rows
}

fn json_search_args(path: &str) -> Value {
    json!({
        "query": "lang:json",
        "path": path,
        "max_results": 500,
        "context_lines": 0,
        "include_classpath": false,
    })
}

/// A server whose builder and dispatcher share `resolver`.
async fn server_with_resolver(
    resolver: std::sync::Arc<sqry_daemon::WorkspaceRosterResolver>,
) -> TestServer {
    let builder: std::sync::Arc<dyn sqry_daemon::WorkspaceBuilder> = std::sync::Arc::new(
        sqry_daemon::RealWorkspaceBuilder::new(std::sync::Arc::clone(&resolver)),
    );
    TestServer::with_builder_config_and_roster(
        builder,
        sqry_daemon::DaemonConfig::default(),
        resolver,
    )
    .await
}

// ---------------------------------------------------------------------------
// Test 1 — every read-only tool routes through the shared acquirer
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_all_readonly_tools_route_through_shared_acquirer() {
    // Drive each of the 14 tools sequentially against a Loaded
    // workspace and assert the global acquisition counter advances by
    // exactly one per dispatch. Sequential rather than parallel so we
    // can attribute each delta to a specific tool name on failure.
    assert_eq!(
        READ_ONLY_TOOLS.len(),
        14,
        "SGA05 acceptance: 14 read-only daemon-hosted MCP tools must all migrate"
    );

    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Reset the shared counter AFTER the daemon/load handshake so the
    // load does not pollute the per-tool delta. The counter is
    // process-wide; running this test in a binary that contains other
    // tests that also touch the counter would race, so the parity
    // tests live in this file alone.
    acquire_counter_reset();

    for tool in READ_ONLY_TOOLS {
        let before = acquire_counter_snapshot();
        let resp = client.request(tool, default_args_for(tool, &path)).await;
        // The response may be success or an inner -32603 (empty-graph)
        // — both prove the dispatcher reached `acquire_and_execute`.
        // What matters here is the counter delta.
        let _ = resp;
        let after = acquire_counter_snapshot();
        assert_eq!(
            after - before,
            1,
            "tool {tool} did not bump the shared acquire counter exactly once: \
             before={before} after={after}",
        );
    }

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2 — semantic_search recovers transparently after eviction
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_semantic_search_reloads_once_after_eviction() {
    // Use a custom builder fixture: load workspace, evict it via
    // test-hooks `evict_for_test`, then issue `semantic_search`. The
    // shared acquirer's bounded one-shot reload must serve a Fresh /
    // Reloaded acquisition — the client must NOT see a
    // `WorkspaceEvicted` (-32004) error, and the acquire counter must
    // bump exactly once.

    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_daemon::DaemonError;
    use sqry_daemon::workspace::{BuiltGraph, WorkspaceBuilder};
    use tempfile::TempDir;

    /// Builder whose `load_persisted` returns an empty graph deterministically.
    /// Used to drive the SGA04 reload path without depending on a fully
    /// indexed snapshot on disk for the parity test.
    #[derive(Debug, Default)]
    struct ReloadOkBuilder;

    impl WorkspaceBuilder for ReloadOkBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }
    }

    let tmp = TempDir::new().unwrap();
    // Persist an empty snapshot so the reload contract holds even when
    // `load_persisted` is checked against the on-disk artifact.
    let graph_dir = tmp.path().join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    save_to_path(&CodeGraph::new(), graph_dir.join("snapshot.sqry").as_path()).unwrap();

    let server =
        TestServer::with_builder(Arc::new(ReloadOkBuilder) as Arc<dyn WorkspaceBuilder>).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path = tmp.path().to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Drive deterministic eviction via the test-hooks helper.
    let canonical = canonicalize_path(tmp.path()).unwrap();
    let key = WorkspaceKey::new(canonical.clone(), ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "evict_for_test must succeed against a Loaded workspace"
    );

    acquire_counter_reset();

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    // Must succeed — the daemon provider's bounded read-only reload
    // restores the workspace transparently. Any -32004
    // (`WorkspaceEvicted`) reaching the client violates SGA02
    // §Tool Ownership Boundary.
    let result = expect_success(&resp);
    assert_eq!(
        result["meta"]["workspace_state"],
        json!("Loaded"),
        "post-reload semantic_search must report Loaded; got: {result}"
    );

    // Exactly one acquire call for this dispatch (the bounded reload
    // is a single internal recovery — not a second `acquire`).
    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "post-eviction semantic_search must bump the acquire counter exactly once",
    );

    // ENV/wire shape: the response result must NOT carry a top-level
    // reload-marker field (SGA design §Staleness and Wire
    // Compatibility — reload metadata is internal-only).
    let inner = &result["result"];
    assert!(
        inner.get("_reload_marker").is_none(),
        "Reloaded acquisitions MUST NOT add new top-level fields to the wire payload",
    );
    assert!(
        inner.get("_stale_warning").is_none(),
        "post-reload (Fresh) responses must not carry a _stale_warning",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3 — rebuild_index stays on the explicit mutating path
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_rebuild_index_does_not_use_readonly_fallback() {
    // `rebuild_index` is in DAEMON_SUPPORTED_TOOL_NAMES but MUST NOT
    // route through `acquire_and_execute` — its mutating path drives
    // `WorkspaceManager::get_or_load` directly. The JSON-RPC method
    // table reports `rebuild_index` as a separate `daemon/rebuild`
    // endpoint, so calling `rebuild_index` over JSON-RPC tool dispatch
    // surfaces `MethodNotFound` (-32601). Either way, the acquire
    // counter must NOT bump.
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    acquire_counter_reset();

    let resp = client
        .request(
            "rebuild_index",
            json!({
                "path": &path,
                "force": false,
            }),
        )
        .await;
    // The error envelope is acceptable here — the test asserts only
    // that `rebuild_index` did NOT silently flow through the read-only
    // acquire path.
    let _ = expect_error(&resp);

    assert_eq!(
        acquire_counter_snapshot(),
        0,
        "rebuild_index MUST NOT bump the read-only acquire counter — \
         it owns its own mutating workspace-load path",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 4 — invalid path short-circuits before classify_for_serve
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_invalid_path_rejected_before_classify_for_serve() {
    // An invalid `path` argument must be rejected as InvalidArgument
    // (-32602). The acquire counter still bumps (`acquire` was
    // entered) but `classify_for_serve` is NOT called — the daemon
    // provider's path-validation step short-circuits inside `acquire`.
    //
    // The structural property "classify_for_serve was not called" is
    // already proven by the in-crate `daemon_provider_invalid_path_short_circuits_*`
    // unit test in `sqry-daemon/src/workspace/acquirer.rs`; from the
    // integration boundary we observe the equivalent property — the
    // -32602 envelope reaches the client AND the daemon's lifecycle
    // workspace map is unchanged.
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    acquire_counter_reset();

    let resp = client
        .request(
            "semantic_search",
            json!({
                "query": "kind:function",
                "path": "/this/path/does/not/exist/for/sga05",
                "max_results": 1,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "invalid path must surface as -32602 InvalidArgument: {err:?}",
    );

    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "acquire was entered exactly once even though the path was invalid",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 5 — counter does NOT bump for non-tool methods
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_non_tool_methods_do_not_route_through_acquire() {
    // Sanity check: non-tool JSON-RPC methods (e.g. `daemon/status`,
    // `daemon/load`) MUST NOT bump the read-only acquire counter.
    // Otherwise the parity tests above would be measuring noise.
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    acquire_counter_reset();
    expect_success(&client.request("daemon/status", json!({})).await);
    let dir = tempfile::tempdir().unwrap();
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir.path().to_string_lossy() }),
            )
            .await,
    );

    assert_eq!(
        acquire_counter_snapshot(),
        0,
        "daemon/status + daemon/load are management methods and must NOT route \
         through `acquire_and_execute`",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — stale-serve metadata is preserved when a stale graph is served.
// ---------------------------------------------------------------------------
//
// 05_TEST_PLAN §"Stale Serve Test": a workspace in Failed state with a prior
// good graph inside `stale_serve_max_age_hours` MUST serve the last-good
// graph and the wire envelope MUST keep:
//   * `meta.stale = true`
//   * `meta.workspace_state = "Failed"`
//   * `meta.last_good_at` populated as RFC3339 UTC-Zulu
//   * `result._stale_warning` spliced with the human-readable age
//
// This test drives the Failed-with-prior-good state synthetically using the
// already-public `LoadedWorkspace` setters (`store_state`,
// `set_last_good_at_for_test`, `record_failure`). No new test-only hook is
// needed — SGA07 acceptance is satisfied by the existing surface.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_stale_serve_preserves_metadata() {
    use std::time::{Duration, SystemTime};

    use sqry_daemon::DaemonError;

    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Synthesize Failed state with a 6h-old prior good (well within the
    // default 24h cap). Use a recorded failure to populate `last_error`
    // so the stale envelope can carry a non-null last_error pointer,
    // matching the shape `ResponseMeta::stale_from(...)` produces in
    // production.
    let canonical = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("workspace registered");
    ws.store_state(WorkspaceState::Failed);
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(6 * 3600)));
    let _ = ws.record_failure(DaemonError::WorkspaceBuildFailed {
        root: dir.path().to_path_buf(),
        reason: "SGA07 synthetic failure for stale_serve_preserves_metadata".to_string(),
    });

    acquire_counter_reset();

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let result = expect_success(&resp);

    // Wire-shape assertions. The Stale verdict surfaced through the
    // shared `acquire_and_execute` path MUST preserve every existing
    // staleness signal — splicing reload-marker fields here would
    // violate SGA design §Staleness and Wire Compatibility.
    assert_eq!(
        result["meta"]["stale"],
        json!(true),
        "stale_serve_preserves_metadata: meta.stale must remain true; result={result}"
    );
    assert_eq!(
        result["meta"]["workspace_state"],
        json!("Failed"),
        "stale_serve_preserves_metadata: meta.workspace_state must remain Failed; result={result}"
    );
    let last_good_at = result["meta"]["last_good_at"]
        .as_str()
        .expect("last_good_at must be present on stale responses");
    assert!(
        last_good_at.ends_with('Z'),
        "last_good_at must round-trip as RFC3339 UTC-Zulu: {last_good_at}"
    );
    let warning = result["result"]["_stale_warning"]
        .as_str()
        .expect("_stale_warning must be spliced on Stale verdict");
    assert!(
        warning.contains("stale"),
        "_stale_warning must mention stale: {warning}"
    );

    // A Stale verdict must still bump the shared acquire counter
    // exactly once — the staleness arm runs through the same
    // `acquire_and_execute` boundary as Fresh.
    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "stale_serve must enter the shared acquire path exactly once",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — Failed without prior good is NotReady / build-failed (NOT empty
// success and NOT eviction).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_stale_without_prior_good_is_not_ready() {
    use sqry_daemon::DaemonError;

    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Failed with NO `last_good_at` — `classify_for_serve` MUST return
    // `WorkspaceBuildFailed` here, never serve an empty success.
    let canonical = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("workspace registered");
    ws.store_state(WorkspaceState::Failed);
    ws.set_last_good_at_for_test(None);
    let _ = ws.record_failure(DaemonError::WorkspaceBuildFailed {
        root: dir.path().to_path_buf(),
        reason: "SGA07 NoPriorGood synthetic failure".to_string(),
    });

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp);
    // -32001 = WorkspaceBuildFailed (the classify_for_serve `NoPriorGood`
    // arm collapses into this, per SGA design).
    assert_eq!(
        err.code, -32001,
        "Failed-without-prior-good must surface -32001 WorkspaceBuildFailed, not eviction or empty success: {err:?}"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — Stale-expired (cap exceeded) is distinct from eviction.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_stale_expired_is_not_eviction() {
    use std::time::{Duration, SystemTime};

    use sqry_daemon::DaemonError;

    // Default `stale_serve_max_age_hours = 24`; force the Expired arm
    // with a 48-hour-old last-good timestamp.
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    let canonical = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("workspace registered");
    ws.store_state(WorkspaceState::Failed);
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(48 * 3600)));
    let _ = ws.record_failure(DaemonError::WorkspaceBuildFailed {
        root: dir.path().to_path_buf(),
        reason: "SGA07 stale_expired synthetic failure".to_string(),
    });

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp);
    // Stale-expired must surface as the dedicated -32002 code, NOT as
    // -32004 (`WorkspaceEvicted`) and NOT as -32001
    // (`WorkspaceBuildFailed`). The SGA design's "Adapters must not
    // collapse" rule depends on these three codes staying distinct.
    assert_eq!(
        err.code, -32002,
        "stale-expired must surface -32002 WorkspaceStaleExpired (distinct from -32004 evicted): {err:?}"
    );
    assert_ne!(
        err.code, -32004,
        "stale-expired MUST NOT collapse into eviction"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — corrupt snapshot is not silently turned into evicted-success.
// ---------------------------------------------------------------------------
//
// Drive an eviction, then call `semantic_search` against a workspace
// whose on-disk snapshot is intentionally corrupt. The SGA04 bounded
// reload calls `builder.load_persisted`, which re-runs the SHA-256
// integrity check inside the persistence layer. The reload MUST fail
// (no empty success), and the error must carry a recognizable
// load/build-failed diagnostic.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_corrupt_snapshot_is_not_evicted_success() {
    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_daemon::DaemonError;
    use sqry_daemon::workspace::{BuiltGraph, WorkspaceBuilder};
    use tempfile::TempDir;

    /// Builder whose `load_persisted` always re-runs `load_from_path`,
    /// so a corrupt snapshot bytes file produces a deterministic
    /// `WorkspaceBuildFailed`.
    #[derive(Debug, Default)]
    struct LoadFromDiskBuilder;

    impl WorkspaceBuilder for LoadFromDiskBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }

        fn load_persisted(&self, root: &Path) -> Result<BuiltGraph, DaemonError> {
            let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
            sqry_core::graph::unified::persistence::load_from_path(storage.snapshot_path(), None)
                .map(|g| {
                    BuiltGraph::new(
                        g,
                        std::sync::Arc::new(sqry_daemon::RosterRecord::fast_path_default()),
                    )
                })
                .map_err(|e| DaemonError::WorkspaceBuildFailed {
                    root: root.to_path_buf(),
                    reason: format!("corrupt snapshot load: {e}"),
                })
        }
    }

    let tmp = TempDir::new().unwrap();
    let graph_dir = tmp.path().join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    // Persist a valid snapshot first…
    save_to_path(&CodeGraph::new(), graph_dir.join("snapshot.sqry").as_path()).unwrap();
    // …then corrupt the magic header so the persistence-layer integrity
    // check fails on reload. Writing arbitrary bytes is enough — V7+
    // load checks `SQRY_GRAPH_V*` magic before any deserialization.
    std::fs::write(graph_dir.join("snapshot.sqry"), b"NOTASQRYSNAPSHOTBYTES").unwrap();

    let server =
        TestServer::with_builder(Arc::new(LoadFromDiskBuilder) as Arc<dyn WorkspaceBuilder>).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path = tmp.path().to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Evict so the shared acquirer's bounded reload runs and hits the
    // corrupt snapshot.
    let canonical = canonicalize_path(tmp.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "evict_for_test must succeed against a Loaded workspace"
    );

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp);
    // `GraphAcquisitionError::Evicted { reload_failure: Some(...) }`
    // collapses into `DaemonError::WorkspaceEvicted` (-32004) per the
    // existing `From` impl. The wire shape MUST NOT be a -32603
    // generic error and MUST NOT be a successful empty result. Either
    // -32004 (evicted-with-reload-failure) or -32001 (load-failure
    // surfaced before classify) is acceptable; the contract under test
    // is "no empty success and no Internal-503 collapse".
    assert!(
        err.code == -32004 || err.code == -32001,
        "corrupt snapshot must surface a structured eviction/build-failed code (-32004 or -32001), not Internal: got {}",
        err.code
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — read-only rehydrate after eviction must NOT publish/touch
// `.sqry/graph/*` artifacts on disk and must NOT fire the publish hook.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_readonly_rehydrate_does_not_publish_artifacts() {
    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_daemon::DaemonError;
    use sqry_daemon::workspace::hook::RecordingHook;
    use sqry_daemon::workspace::{BuiltGraph, WorkspaceBuilder};
    use tempfile::TempDir;

    #[derive(Debug, Default)]
    struct ReloadOkBuilder;

    impl WorkspaceBuilder for ReloadOkBuilder {
        fn build(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }
        fn load_persisted(&self, _root: &Path) -> Result<BuiltGraph, DaemonError> {
            Ok(BuiltGraph::empty_fast_path())
        }
    }

    let tmp = TempDir::new().unwrap();
    let graph_dir = tmp.path().join(".sqry").join("graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    let snapshot_path = graph_dir.join("snapshot.sqry");
    save_to_path(&CodeGraph::new(), snapshot_path.as_path()).unwrap();
    let snapshot_mtime_before = std::fs::metadata(&snapshot_path)
        .expect("snapshot metadata")
        .modified()
        .expect("modified time");

    let server =
        TestServer::with_builder(Arc::new(ReloadOkBuilder) as Arc<dyn WorkspaceBuilder>).await;
    // Install a recording hook so we can prove the read-only rehydrate
    // path does NOT call `on_publish` (the daemon's `reload_from_disk_read_only`
    // intentionally suppresses the hook — see manager.rs:1629 docs).
    let recording_hook = RecordingHook::new();
    server
        .manager
        .set_hook(Arc::clone(&recording_hook) as sqry_daemon::workspace::hook::SharedHook);

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path = tmp.path().to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Snapshot the publish counter AFTER the initial daemon/load (which
    // legitimately publishes once). The rehydrate path under test must
    // not advance it again.
    let publish_count_before_rehydrate = recording_hook.invocation_count();

    // Evict and trigger rehydrate via semantic_search.
    let canonical = canonicalize_path(tmp.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(server.manager.evict_for_test(&key));

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let _result = expect_success(&resp);

    // Wait briefly for any spawned hook task. `spawn_hook` is
    // fire-and-forget but we still allow a tick so a buggy
    // implementation that mistakenly fires the hook would race-win.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let publish_count_after_rehydrate = recording_hook.invocation_count();
    assert_eq!(
        publish_count_after_rehydrate, publish_count_before_rehydrate,
        "read-only rehydrate after eviction MUST NOT call SqrydHook::on_publish; \
         counts before={publish_count_before_rehydrate} after={publish_count_after_rehydrate}",
    );

    // Snapshot mtime must be unchanged — the read-only reload reads the
    // file but never rewrites it.
    let snapshot_mtime_after = std::fs::metadata(&snapshot_path)
        .expect("snapshot metadata")
        .modified()
        .expect("modified time");
    assert_eq!(
        snapshot_mtime_before, snapshot_mtime_after,
        "read-only rehydrate must not re-write `.sqry/graph/snapshot.sqry`"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — daemon-MCP and CLI return equivalent semantic_search results
// from the same on-disk index after the daemon workspace is evicted.
// ---------------------------------------------------------------------------
//
// This is the DAG `SGA07` cross-surface acceptance test. The CLI runs
// against the bare on-disk graph; the daemon serves the same workspace
// after a deterministic `evict_for_test` + bounded read-only reload.
// The two name sets must agree.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_and_cli_return_equivalent_results_after_eviction() {
    use std::collections::HashSet;
    use std::process::Command;
    use std::sync::Arc;

    use sqry_daemon::workspace::WorkspaceBuilder;
    use sqry_daemon::{RealWorkspaceBuilder, WorkspaceRosterResolver};
    use tempfile::TempDir;

    /// Locate the `sqry` binary for testing.
    ///
    /// Delegates to the one resolver, `sqry_core::test_support::binaries::sqry_binary`
    /// (surface parity W4, design W4-D13), which reads `SQRY_E2E_SQRY_BIN`, then
    /// `CARGO_BIN_EXE_sqry`, then `CARGO_TARGET_DIR` and the workspace `target`,
    /// debug before release, and panics naming every variable and candidate.
    fn sqry_bin_path() -> std::path::PathBuf {
        sqry_core::test_support::binaries::sqry_binary()
    }

    // Build a real workspace with a known symbol set AND a json file, and
    // index it the way a user who wants json coverage does. Surface parity
    // W1 (mutation-control row "load_persisted loads with the build roster
    // instead of load_roster"): the daemon's read-only reload after
    // eviction must load this `include_all` snapshot with the full compiled
    // roster, so the json nodes survive the round trip.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_mixed_fixture(&root);

    let cli_index_status = Command::new(sqry_bin_path())
        .arg("index")
        .arg("--include-high-cost")
        .arg(&root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .status()
        .expect("run sqry index");
    assert!(
        cli_index_status.success(),
        "sqry index --include-high-cost must succeed for cross-surface parity fixture",
    );

    // Run the CLI against the on-disk graph BEFORE starting the daemon
    // so the manifest's recorded snapshot SHA-256 still matches the
    // CLI-produced bytes.
    let cli_query = |query: &str| -> String {
        let out = Command::new(sqry_bin_path())
            .arg("query")
            .arg(query)
            .arg(&root)
            .env("NO_COLOR", "1")
            .env("SQRY_FORCE_STANDALONE", "1")
            .output()
            .expect("run sqry query");
        assert!(
            out.status.success(),
            "sqry query {query} must succeed against on-disk graph; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let cli_stdout = cli_query("kind:function");
    let mut cli_names: HashSet<String> = HashSet::new();
    for needle in ["func_alpha", "func_beta", "func_gamma"] {
        if cli_stdout.contains(needle) {
            cli_names.insert(needle.to_string());
        }
    }
    assert!(
        !cli_names.is_empty(),
        "CLI must surface at least one of func_alpha/func_beta/func_gamma; stdout={cli_stdout}",
    );
    let expected_json = expected_json_count(&root);
    assert!(
        expected_json >= 1,
        "fixture precondition: json nodes, got {expected_json}"
    );
    let cli_json_stdout = cli_query("lang:json");
    assert!(
        cli_json_stdout.contains("json"),
        "CLI lang:json must surface json symbols; stdout={cli_json_stdout}"
    );

    // NOW build the daemon plumbing with the production builder. Its
    // `build` runs the pipeline in memory with the manifest roster;
    // `load_persisted` rehydrates the CLI snapshot with the full roster.
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server = TestServer::with_builder_config_and_roster(
        builder,
        sqry_daemon::DaemonConfig::default(),
        resolver,
    )
    .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path_str = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path_str }))
            .await,
    );

    // Evict the workspace so the daemon-MCP semantic_search call goes
    // through the SGA04 bounded reload path.
    let canonical = canonicalize_path(&root).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(server.manager.evict_for_test(&key));

    acquire_counter_reset();

    let resp = client
        .request(
            "semantic_search",
            json!({
                "query": "kind:function",
                "path": &path_str,
                "max_results": 100,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "post-eviction CLI-parity dispatch must bump the shared acquire counter exactly once",
    );

    // Daemon serves through MCP-flavoured envelope: result.result is the
    // tool payload; check the symbols list for the same name set.
    let inner = &result["result"];
    let inner_str = inner.to_string();
    let mut daemon_names: HashSet<String> = HashSet::new();
    for needle in ["func_alpha", "func_beta", "func_gamma"] {
        if inner_str.contains(needle) {
            daemon_names.insert(needle.to_string());
        }
    }
    for cli_name in &cli_names {
        assert!(
            daemon_names.contains(cli_name),
            "daemon-MCP missing CLI symbol {cli_name}: cli_set={cli_names:?} daemon_set={daemon_names:?}",
        );
    }
    assert!(
        inner.get("plugin_selection_warning").is_none(),
        "the reloaded include_all snapshot matches its manifest; no warning expected: {inner}"
    );

    // The json nodes survived the reload: the daemon serves the same
    // lang:json count the full-roster build of the fixture has.
    let json_resp = client
        .request("semantic_search", json_search_args(&path_str))
        .await;
    let json_result = expect_success(&json_resp);
    let daemon_json_total = json_result["result"]["data"]["total"]
        .as_u64()
        .expect("total present");
    assert_eq!(
        daemon_json_total as usize, expected_json,
        "post-eviction reload must serve every json node the CLI indexed: {json_result}"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// SGA07 — daemon-MCP rejects an incompatible-graph (unknown plugin id) and
// surfaces -32005 instead of collapsing into evicted/internal.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_unknown_plugin_id_returns_incompatible_graph() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    // Surface parity W1: the production builder resolves the roster from
    // the manifest, so a manifest naming a plugin id this binary cannot
    // load is refused at `daemon/load` with the dedicated -32005, not a
    // synthesised builder error and not one of three tolerated codes.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push("sga07-fake-plugin".to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path = root.to_string_lossy().to_string();
    let resp = client
        .request("daemon/load", json!({ "index_root": &path }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32005,
        "an unknown manifest plugin id must surface -32005 WorkspaceIncompatibleGraph at load: {err:?}"
    );
    assert!(
        err.message.contains("sga07-fake-plugin"),
        "the refusal must name the unknown id: {}",
        err.message
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T3 (surface parity W1): the ADD-form negative control. A manifest that
// names a plugin this binary did not compile is refused on every surface
// with the same verdict, and nothing on disk changes.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_load_refuses_manifest_that_adds_uncompiled_plugin() {
    use std::sync::Arc;

    use sqry_core::graph::acquisition::{
        AcquisitionOperation, FilesystemGraphProvider, GraphAcquirer, GraphAcquisitionError,
        GraphAcquisitionRequest, MissingGraphPolicy, PathPolicy, PluginSelectionPolicy,
        PluginSelectionStatus, StalePolicy,
    };
    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    // `terraform` is feature-gated. The control cannot pass vacuously: it
    // asserts the plugin is absent from this binary's full roster first.
    let full = sqry_plugin_registry::create_plugin_manager_all();
    assert!(
        full.plugin_by_id("terraform").is_none(),
        "this binary compiles terraform; the ADD-form control needs an uncompiled id"
    );

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push("terraform".to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    let manifest_bytes_before = std::fs::read(storage.manifest_path()).expect("manifest bytes");

    // Surface 3: the standalone provider with the full roster.
    let provider = FilesystemGraphProvider::new(Arc::new(full));
    let err = provider
        .acquire(GraphAcquisitionRequest {
            requested_path: root.clone(),
            operation: AcquisitionOperation::ReadOnlyQuery,
            path_policy: PathPolicy::default(),
            missing_graph_policy: MissingGraphPolicy::Error,
            stale_policy: StalePolicy::default(),
            plugin_selection_policy: PluginSelectionPolicy::default(),
            tool_name: Some("t3_negative_control"),
        })
        .expect_err("standalone provider must refuse the uncompiled id");
    match err {
        GraphAcquisitionError::IncompatibleGraph { status, .. } => match status {
            PluginSelectionStatus::IncompatibleUnknownPluginIds {
                unknown_plugin_ids, ..
            } => assert_eq!(unknown_plugin_ids, vec!["terraform".to_string()]),
            other => panic!("expected IncompatibleUnknownPluginIds, got {other:?}"),
        },
        other => panic!("expected IncompatibleGraph, got {other:?}"),
    }

    // Surfaces 1 and 2: daemon/load and daemon/rebuild.
    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();

    let load_resp = client
        .request("daemon/load", json!({ "index_root": &path }))
        .await;
    let load_err = expect_error(&load_resp);
    assert_eq!(load_err.code, -32005, "daemon/load: {load_err:?}");
    assert!(
        load_err.message.contains("terraform"),
        "daemon/load must name the id: {}",
        load_err.message
    );

    // The failed load leaves the workspace registered in `Failed`, which
    // `daemon/rebuild` accepts; the resolver refuses before the guard or
    // any build runs, so the mechanism is the same -32005.
    let rebuild_resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let rebuild_err = expect_error(&rebuild_resp);
    assert_eq!(rebuild_err.code, -32005, "daemon/rebuild: {rebuild_err:?}");
    assert!(
        rebuild_err.message.contains("terraform"),
        "daemon/rebuild must name the id: {}",
        rebuild_err.message
    );

    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_bytes_before,
        "no surface may rewrite the manifest while refusing it"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T2 (surface parity W1): daemon-hosted and standalone `semantic_search`
// return the same json nodes from the same high-cost workspace.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_mcp_and_standalone_agree_on_high_cost_graph() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use sqry_mcp::tool_args::{PaginationArgs, SearchFilters, SemanticSearchArgs};
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager_all(),
        "include_all",
    );
    let n = expected_json_count(&root);
    assert!(n >= 1, "fixture precondition: json nodes, got {n}");

    // Standalone leg: the same in-process engine `sqry-mcp` runs, bound to
    // the fixture root the way the server binds every request
    // (`with_workspace_override` before tool execution).
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
    unsafe {
        std::env::set_var("SQRY_FORCE_STANDALONE", "1");
    }
    let standalone = sqry_mcp::daemon_adapter::with_workspace_override(
        Some(&root),
        sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root(&root),
        || {
            sqry_mcp::execution::execute_semantic_search(
                &SemanticSearchArgs {
                    query: "lang:json".to_string(),
                    path: root.to_string_lossy().to_string(),
                    filters: SearchFilters::default(),
                    max_results: 500,
                    context_lines: 0,
                    pagination: PaginationArgs {
                        offset: 0,
                        size: 500,
                    },
                    score_min: None,
                    include_classpath: false,
                    budget_rows: None,
                    framework: None,
                    resolved_via: None,
                    revision: Default::default(),
                },
                &sqry_core::query::cancellation::CancellationToken::new(),
            )
        },
    )
    .expect("standalone semantic_search runs");
    unsafe {
        std::env::remove_var("SQRY_FORCE_STANDALONE");
    }
    let standalone_total = standalone.data.total;
    assert_eq!(standalone_total as usize, n, "standalone total must be N");
    let standalone_first = standalone
        .data
        .results
        .first()
        .map(|hit| hit.qualified_name.clone())
        .expect("standalone first result");
    // `semantic_search` carries no `graph_metadata` on either surface (the
    // tool body leaves it `None`); the parity claim is that both surfaces
    // agree, which here means both omit it. `used_graph` and `total` are
    // the fields the tool does fill, and they are compared below.
    let standalone_meta =
        serde_json::to_value(standalone.graph_metadata.as_ref()).expect("serialise metadata");
    assert!(
        standalone.used_graph,
        "standalone semantic_search uses the graph"
    );

    // Daemon leg.
    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let resp = client
        .request("semantic_search", json_search_args(&path))
        .await;
    let result = expect_success(&resp);
    let inner = &result["result"];
    let daemon_total = inner["data"]["total"].as_u64().expect("daemon total");
    assert_eq!(
        daemon_total as usize, n,
        "daemon total must be N (pre-change it was 0): {inner}"
    );
    assert_eq!(daemon_total, standalone_total);
    let daemon_first = inner["data"]["results"][0]["qualifiedName"]
        .as_str()
        .expect("daemon first result");
    assert_eq!(daemon_first, standalone_first);
    let daemon_meta = inner.get("graph_metadata").cloned().unwrap_or(Value::Null);
    assert_eq!(
        daemon_meta, standalone_meta,
        "both surfaces must report the same graph_metadata (both omit it for semantic_search)"
    );
    assert_eq!(inner["used_graph"], json!(true));
    assert_eq!(
        inner["total"].as_u64(),
        Some(standalone.total.expect("standalone total")),
        "envelope totals must agree"
    );
    assert!(
        inner.get("plugin_selection_warning").is_none(),
        "a matching roster carries no warning: {inner}"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T5a (surface parity W1): the `plugin_selection_warning` envelope key in
// both divergence directions, and its absence when the rosters match.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_envelope_reports_roster_divergence_both_directions() {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use sqry_daemon::{RosterRecord, WorkspaceRosterResolver};
    use sqry_plugin_registry::RosterSource;
    use tempfile::TempDir;

    async fn search_result(
        resolver: Arc<WorkspaceRosterResolver>,
        root: &std::path::Path,
    ) -> Value {
        let server = server_with_resolver(resolver).await;
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let path = root.to_string_lossy().to_string();
        expect_success(
            &client
                .request("daemon/load", json!({ "index_root": &path }))
                .await,
        );
        let resp = client
            .request("semantic_search", json_search_args(&path))
            .await;
        let result = expect_success(&resp)["result"].clone();
        drop(client);
        server.stop().await;
        result
    }

    let fast = sqry_plugin_registry::create_plugin_manager();
    let full = sqry_plugin_registry::create_plugin_manager_all();
    let fast_ids: BTreeSet<String> = plugin_ids_of(&fast).into_iter().collect();
    let full_ids: BTreeSet<String> = plugin_ids_of(&full).into_iter().collect();
    let beyond_fast: Vec<String> = full_ids.difference(&fast_ids).cloned().collect();
    assert!(
        beyond_fast.contains(&"json".to_string()),
        "fixture precondition: json is the compiled plugin outside the fast path"
    );

    // Narrower: an include_all manifest served by a resident fast-path graph.
    let narrower = TempDir::new().unwrap();
    let narrower_root = narrower.path().canonicalize().unwrap();
    write_mixed_fixture(&narrower_root);
    index_with(&narrower_root, &full, "include_all");
    let pinned_fast = Arc::new(WorkspaceRosterResolver::pinned(
        Arc::new(sqry_plugin_registry::create_plugin_manager()),
        RosterRecord::from_manager(&fast, RosterSource::Fallback),
    ));
    let result = search_result(pinned_fast, &narrower_root).await;
    let warning = result
        .get("plugin_selection_warning")
        .cloned()
        .unwrap_or_else(|| {
            let keys: Vec<&String> = result
                .as_object()
                .map(|m| m.keys().collect())
                .unwrap_or_default();
            panic!("narrower resident roster must carry plugin_selection_warning; keys: {keys:?}")
        });
    assert_eq!(warning["status"], json!("diverges_from_manifest"));
    assert_eq!(warning["missing_plugin_ids"], json!(beyond_fast));
    assert_eq!(warning["extra_plugin_ids"], json!([]));
    assert_eq!(warning["resident_source"], json!("fallback"));
    assert_eq!(
        warning["manifest_path"],
        json!(
            sqry_core::graph::unified::persistence::GraphStorage::new(&narrower_root)
                .manifest_path()
                .display()
                .to_string()
        )
    );
    assert_eq!(
        warning["hint"],
        json!(format!(
            "sqry index --force --include-high-cost {}",
            narrower_root.display()
        ))
    );
    assert_eq!(
        result["data"]["total"].as_u64(),
        Some(0),
        "the resident fast-path graph has no json nodes; the warning is what says why"
    );

    // Wider: a fast-path manifest served by a resident full-roster graph.
    let wider = TempDir::new().unwrap();
    let wider_root = wider.path().canonicalize().unwrap();
    write_mixed_fixture(&wider_root);
    index_with(&wider_root, &fast, "fast_path_default");
    let pinned_full = Arc::new(WorkspaceRosterResolver::pinned(
        Arc::new(sqry_plugin_registry::create_plugin_manager_all()),
        RosterRecord::from_manager(&full, RosterSource::Fallback),
    ));
    let result = search_result(pinned_full, &wider_root).await;
    let warning = result
        .get("plugin_selection_warning")
        .cloned()
        .expect("wider resident roster must carry plugin_selection_warning");
    assert_eq!(warning["missing_plugin_ids"], json!([]));
    assert_eq!(warning["extra_plugin_ids"], json!(beyond_fast));
    assert_eq!(
        warning["hint"],
        json!(format!(
            "sqry daemon rebuild --force {}",
            wider_root.display()
        ))
    );

    // Matching: the production resolver over the include_all manifest.
    let matching = TempDir::new().unwrap();
    let matching_root = matching.path().canonicalize().unwrap();
    write_mixed_fixture(&matching_root);
    index_with(&matching_root, &full, "include_all");
    let result = search_result(Arc::new(WorkspaceRosterResolver::new()), &matching_root).await;
    assert!(
        result.get("plugin_selection_warning").is_none(),
        "matching rosters must carry no key: {result}"
    );
    assert_eq!(
        result["data"]["total"].as_u64().map(|t| t as usize),
        Some(expected_json_count(&matching_root))
    );
}

// Suppress the unused-import warnings that creep in when we run only a
// subset of the helpers above. `insert_workspace_in_state` is a generic
// Failed/Stale helper; the SGA07 follow-up tests above use the more
// surgical `lookup` + `store_state` + `set_last_good_at_for_test` path
// so they can also drive `record_failure`. The reference is kept so the
// helper doesn't dead-code-warn when the file is built standalone.
#[allow(dead_code)]
fn _support_export_for_followups(
    _m: &std::sync::Arc<sqry_daemon::WorkspaceManager>,
    _k: &WorkspaceKey,
) {
    let _: fn(&std::sync::Arc<sqry_daemon::WorkspaceManager>, &WorkspaceKey, WorkspaceState) =
        insert_workspace_in_state;
}

// ---------------------------------------------------------------------------
// T17b (round 2, D10): after eviction, a manifest naming an uncompiled id
// is refused by `load_persisted` and the workspace is not republished.
// On `abefdd8e3` the reload published the graph (status `Loaded`) and only
// the acquirer refused the query, so the code was `-32005` on both heads;
// the residency assertion is what turns red there.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn evicted_workspace_with_an_uncompiled_manifest_id_is_not_republished() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    const PLANTED_ID: &str = "w1-r2-planted-plugin";
    assert!(
        sqry_plugin_registry::create_plugin_manager_all()
            .plugin_by_id(PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // The manifest changes under the resident graph: it now names an id
    // this binary did not compile. The snapshot bytes are untouched.
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(PLANTED_ID.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    let manifest_bytes_before = std::fs::read(storage.manifest_path()).expect("manifest bytes");

    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    assert!(server.manager.evict_for_test(&key));

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32005,
        "the reload must refuse the uncompiled id with -32005: {err:?}"
    );
    assert!(
        err.message.contains(PLANTED_ID),
        "the refusal must name the id: {}",
        err.message
    );

    // Residency is the discriminator: the reload refused before
    // publishing, so the workspace is not `Loaded`.
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    let row = status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path.as_str()))
        })
        .cloned()
        .expect("workspace row present");
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "a refused reload must not republish the workspace; row: {row}"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_bytes_before,
        "the refusal must not rewrite the manifest"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T22 (round 2, D9, D12): `daemon/load` over an index whose manifest cannot
// be read is refused with `-32001`, the message and `error.data.reason`
// name the manifest and `sqry index --force <root>`, nothing becomes
// resident, and the manifest bytes are unchanged. On `abefdd8e3` the load
// succeeded with `state: Loaded` and `plugin_roster.source == "fallback"`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_load_refuses_an_unreadable_manifest() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    assert!(
        storage.snapshot_exists(),
        "fixture precondition: valid snapshot"
    );
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let manifest_bytes_before = std::fs::read(storage.manifest_path()).expect("manifest bytes");
    let manifest_path = storage.manifest_path().display().to_string();
    let repair = format!("sqry index --force {}", root.display());

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();

    let resp = client
        .request("daemon/load", json!({ "index_root": &path }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32001,
        "an unreadable manifest must be refused with -32001: {err:?}"
    );
    assert!(
        err.message.contains(&manifest_path),
        "the message must name the manifest: {}",
        err.message
    );
    assert!(
        err.message.contains(&repair),
        "the message must name the repair: {}",
        err.message
    );
    let data = err.data.clone().expect("refusal carries error.data");
    let reason = data["reason"].as_str().expect("reason is a string");
    assert!(
        reason.contains(&manifest_path) && reason.contains("sqry index --force"),
        "error.data.reason must name the manifest and the repair: {reason}"
    );
    assert_eq!(data["manifest_path"], manifest_path);
    assert_eq!(data["repair_command"], repair);
    // Round 3 (design D19, battery row K15): the Display of
    // `WorkspaceManifestUnreadable` is contract. Its clause order is
    // pinned here as a prefix and a suffix; the parse reason between them
    // is serde's and is not pinned.
    assert_unreadable_reason_shape(reason, &manifest_path, &root);

    // Nothing is resident. A failed load registers the root in `Failed`
    // (the stale-serve flow reads `last_error` from that entry), so the
    // assertion is on the state, not on the absence of the row.
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    let row = status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path.as_str()))
        })
        .cloned();
    if let Some(row) = &row {
        assert_ne!(
            row["state"],
            json!("Loaded"),
            "a refused load must not publish a graph; row: {row}"
        );
        assert!(
            row.get("plugin_roster")
                .is_none_or(|roster| roster.is_null()),
            "no roster may be recorded for a refused load; row: {row}"
        );
    }

    // `daemon/rebuild` on the same root is refused by the same rule, and
    // the file is still untouched afterwards.
    let rebuild_resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let rebuild_err = expect_error(&rebuild_resp);
    assert!(
        rebuild_err.code == -32001 || rebuild_err.code == -32004,
        "daemon/rebuild over an unreadable manifest is refused (-32001) or, if the failed \
         load left no rebuildable entry, reports not loaded (-32004): {rebuild_err:?}"
    );
    if rebuild_err.code == -32001 {
        assert!(
            rebuild_err.message.contains(&manifest_path),
            "daemon/rebuild must name the manifest: {}",
            rebuild_err.message
        );
    }
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_bytes_before,
        "no surface may rewrite the unreadable manifest"
    );

    drop(client);
    server.stop().await;
}

/// The shape of `WorkspaceManifestUnreadable`'s Display (design D19): a
/// prefix naming the file, a suffix naming the repair, and serde's parse
/// reason in between, which is not pinned.
fn assert_unreadable_reason_shape(reason: &str, manifest_path: &str, root: &std::path::Path) {
    let prefix = format!("manifest at {manifest_path} cannot be read (");
    let suffix = format!("); repair with: sqry index --force {}", root.display());
    assert!(
        reason.starts_with(&prefix),
        "reason must start with {prefix:?}, got {reason:?}"
    );
    assert!(
        reason.ends_with(&suffix),
        "reason must end with {suffix:?}, got {reason:?}"
    );
    assert!(
        reason.len() > prefix.len() + suffix.len(),
        "the parse reason between the clauses must not be empty: {reason:?}"
    );
}

/// The `daemon/status` row for `path`, shared by T31 and T31b.
async fn status_row_for(client: &mut TestIpcClient, path: &str) -> Value {
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path))
        })
        .cloned()
        .expect("workspace row present")
}

/// Assert the typed refusal envelope of an unreadable manifest on the
/// IPC wire: `-32001`, `error.data` naming the root, the manifest, the
/// repair, and a reason of the pinned shape.
fn assert_unreadable_refusal_envelope(
    err: &sqry_daemon::ipc::protocol::JsonRpcError,
    root: &std::path::Path,
    manifest_path: &str,
) {
    let repair = format!("sqry index --force {}", root.display());
    assert_eq!(
        err.code, -32001,
        "an unreadable manifest is refused with -32001: {err:?}"
    );
    assert!(
        err.message.contains(manifest_path) && err.message.contains(&repair),
        "the message names the manifest and the repair: {}",
        err.message
    );
    let data = err.data.clone().expect("the refusal carries error.data");
    assert_eq!(data["root"], root.display().to_string(), "data: {data}");
    assert_eq!(data["manifest_path"], manifest_path, "data: {data}");
    assert_eq!(data["repair_command"], repair, "data: {data}");
    let reason = data["reason"].as_str().expect("reason is a string");
    assert_unreadable_reason_shape(reason, manifest_path, root);
}

// ---------------------------------------------------------------------------
// T31 (round 3, design D15, R3-2): the reload after eviction over an index
// whose manifest has become unreadable is the typed refusal (`-32001`
// naming the file and the repair), not `-32004` "evicted mid-rebuild"; the
// workspace is `Failed` with the refusal recorded, and the manifest bytes
// are untouched. On `deb423b87` the reload's error was collapsed into
// `WorkspaceEvicted` and the assertion read `left: -32004 right: -32001`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn evicted_reload_over_an_unreadable_manifest_is_the_typed_refusal() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // The manifest becomes unreadable under the resident graph, then the
    // workspace is evicted, so the next call reloads from disk.
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let manifest_bytes_before = std::fs::read(storage.manifest_path()).expect("manifest bytes");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "workspace was resident"
    );

    acquire_counter_reset();
    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp).clone();
    assert_unreadable_refusal_envelope(&err, &root, &manifest_path);
    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "one acquire; the bounded reload is internal to it"
    );

    // The refused reload leaves the workspace Failed with the refusal
    // recorded verbatim (the V8 pin), never Loaded.
    let row = status_row_for(&mut client, &path).await;
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "a refused reload must not republish the workspace; row: {row}"
    );
    let last_error = row["last_error"]
        .as_str()
        .expect("a refused reload records last_error");
    assert!(
        last_error.contains(&manifest_path),
        "last_error must name the manifest: {last_error}"
    );
    assert_eq!(
        last_error, err.message,
        "the recorded error is the refusal the client received"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_bytes_before,
        "the refusal must not rewrite the manifest"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T31b (round 3, design D15, R3-6): a tool call over a resident graph whose
// manifest became unreadable after load is the same typed refusal with the
// keys, the graph stays resident (`Loaded`, same `Arc`), and restoring the
// manifest bytes makes the next call succeed without a reload. On
// `deb423b87` the code was `-32001` but `error.data` was `{root, reason}`
// (the D9 table's "LoadFailed" row described the provider type, not the
// wire); the state leg is green on both heads and is a declared control.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn tool_call_over_a_manifest_corrupted_after_load_is_the_typed_refusal() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let good_manifest = std::fs::read(storage.manifest_path()).expect("manifest bytes");

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let resident_before = server
        .manager
        .lookup(&key)
        .expect("resident workspace")
        .graph();

    // The manifest becomes unreadable under the resident graph; no
    // eviction, so the resident path classifies it.
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let corrupted_bytes = std::fs::read(storage.manifest_path()).expect("manifest bytes");

    acquire_counter_reset();
    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp).clone();
    assert_unreadable_refusal_envelope(&err, &root, &manifest_path);
    assert_eq!(acquire_counter_snapshot(), 1);

    // Control leg (green on both heads): a refusal, not an unload. The
    // graph is still resident and is the same generation.
    let row = status_row_for(&mut client, &path).await;
    assert_eq!(
        row["state"],
        json!("Loaded"),
        "the resident refusal keeps the workspace Loaded; row: {row}"
    );
    let resident_after_refusal = server
        .manager
        .lookup(&key)
        .expect("resident workspace")
        .graph();
    assert!(
        Arc::ptr_eq(&resident_before, &resident_after_refusal),
        "the refusal must not swap the resident graph"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        corrupted_bytes,
        "the refusal must not rewrite the manifest"
    );

    // Repairing the manifest makes the next call serve the resident graph
    // without a reload: one acquire, the same `Arc`, state Loaded.
    std::fs::write(storage.manifest_path(), &good_manifest).expect("manifest restored");
    acquire_counter_reset();
    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["workspace_state"], json!("Loaded"));
    assert_eq!(acquire_counter_snapshot(), 1);
    let resident_after_repair = server
        .manager
        .lookup(&key)
        .expect("resident workspace")
        .graph();
    assert!(
        Arc::ptr_eq(&resident_before, &resident_after_repair),
        "a repaired manifest serves the resident graph without a reload"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// The daemon-hosted `rebuild_index` tool is not an IPC method (it is served
// by `DaemonMcpHandler::handle_rebuild_index` over the MCP shim), so T32
// and T41 drive it through an rmcp client the way `ipc_shim_mcp_host.rs`
// does.
// ---------------------------------------------------------------------------

async fn connect_mcp_shim(
    server: &TestServer,
) -> (
    tokio::io::ReadHalf<tokio::net::UnixStream>,
    tokio::io::WriteHalf<tokio::net::UnixStream>,
) {
    use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
    use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};

    let stream = tokio::net::UnixStream::connect(&server.path)
        .await
        .expect("connect");
    let (mut rh, mut wh) = tokio::io::split(stream);
    write_frame_json(
        &mut wh,
        &ShimRegister {
            protocol: ShimProtocol::Mcp,
            pid: std::process::id(),
        },
    )
    .await
    .expect("write ShimRegister");
    let ack = read_frame_json::<_, ShimRegisterAck>(&mut rh)
        .await
        .expect("read ack")
        .expect("ack frame");
    assert!(
        ack.accepted,
        "ack must be accepted; reason={:?}",
        ack.reason
    );
    (rh, wh)
}

/// Call the daemon-hosted `rebuild_index` tool with `force` over the MCP
/// shim and return the raw rmcp outcome.
async fn mcp_rebuild_index(
    server: &TestServer,
    path: &str,
    force: bool,
) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
    let (rh, wh) = connect_mcp_shim(server).await;
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let outcome = running
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("rebuild_index").with_arguments(
                serde_json::Map::from_iter([
                    ("path".to_string(), json!(path)),
                    ("force".to_string(), json!(force)),
                ]),
            ),
        )
        .await;
    drop(running);
    outcome
}

/// The MCP error envelope of a failed tool call.
fn mcp_error_of(
    outcome: Result<rmcp::model::CallToolResult, rmcp::ServiceError>,
) -> rmcp::model::ErrorData {
    match outcome {
        Ok(result) => panic!("the tool must fail; survived {result:?}"),
        Err(rmcp::ServiceError::McpError(err)) => err,
        Err(other) => panic!("expected an MCP error envelope, got {other:?}"),
    }
}

/// Sorted listing of every file under `<root>/.sqry` with its bytes.
fn index_dir_listing(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &std::path::Path, prefix: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
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
                    std::fs::read(&path).expect("file bytes"),
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

/// Plant `R3_PLANTED_ID` into the recorded selection at `root`; returns the
/// manifest bytes after planting.
fn plant_r3_id(root: &std::path::Path) -> Vec<u8> {
    assert!(
        sqry_plugin_registry::create_plugin_manager_all()
            .plugin_by_id(R3_PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(R3_PLANTED_ID.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    std::fs::read(storage.manifest_path()).expect("manifest bytes")
}

// ---------------------------------------------------------------------------
// T32 (round 3, design D17, codex Finding 3): daemon-hosted `rebuild_index`
// with `force: false` over an existing index classifies the manifest before
// it reports the index, for a workspace that is not resident: a planted id
// is refused by name with the manifest path (`kind ==
// workspace_incompatible_graph`, the MCP form of the daemon's `-32005`), and
// an unreadable manifest is refused with `details.manifest_path` and
// `details.repair_command` (the MCP form of `-32001`
// `WorkspaceManifestUnreadable`); nothing under `.sqry` changes. On
// `416debe48` both calls returned `success: true` with "Index already
// exists. Use force=true to rebuild." because the leg never resolved the
// roster (the manifest was read only for its counts, and a non-resident
// workspace was not classified at all).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_rebuild_index_cache_hit_classifies_the_manifest() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let path = root.to_string_lossy().to_string();
    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;

    // Leg 1: a planted id, workspace not resident.
    plant_r3_id(&root);
    let listing_planted = index_dir_listing(&root);
    let err = mcp_error_of(mcp_rebuild_index(&server, &path, false).await);
    let data = err.data.clone().expect("the refusal carries data");
    assert_eq!(
        data["kind"],
        json!("workspace_incompatible_graph"),
        "a planted id is the incompatible-graph refusal: {err:?}"
    );
    let reason = data["details"]["reason"]
        .as_str()
        .expect("details.reason is a string");
    assert!(
        reason.contains(R3_PLANTED_ID),
        "the refusal must name the id: {reason}"
    );
    assert!(
        reason.contains(&manifest_path),
        "the refusal must name the manifest: {reason}"
    );
    assert!(
        err.message.contains(R3_PLANTED_ID),
        "the message must name the id: {}",
        err.message
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "the refusal must change nothing under .sqry"
    );

    // Leg 2: an unreadable manifest, still not resident.
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let listing_unreadable = index_dir_listing(&root);
    let err = mcp_error_of(mcp_rebuild_index(&server, &path, false).await);
    let data = err.data.clone().expect("the refusal carries data");
    assert_eq!(data["kind"], json!("workspace_not_ready"), "{err:?}");
    assert_eq!(data["retryable"], json!(false), "{data}");
    assert_eq!(
        data["details"]["manifest_path"],
        json!(manifest_path),
        "{data}"
    );
    assert_eq!(
        data["details"]["repair_command"],
        json!(format!("sqry index --force {}", root.display())),
        "{data}"
    );
    assert_unreadable_reason_shape(&err.message, &manifest_path, &root);
    assert_eq!(
        index_dir_listing(&root),
        listing_unreadable,
        "the refusal must change nothing under .sqry"
    );

    // Nothing became resident through either leg.
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    assert!(
        server
            .manager
            .lookup(&key)
            .is_none_or(|ws| ws.load_state() != WorkspaceState::Loaded),
        "rebuild_index without force must not load the workspace"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// T41 (design D19, battery rows K17 and K18; codex survivors C11 and C12):
// the daemon `rebuild_index` envelope carries `plugin_selection_warning`
// whenever the resident record diverges from the manifest, and
// `graph_metadata` whenever a graph is resident, on every leg: the load leg
// (`force: true`, nothing resident), the cache-hit leg (`force: false`,
// resident) and the in-place leg (`force: true`, resident, through the
// rebuild dispatcher). Every daemon-hosted build persists the manifest it
// was built from (decision D-i7-2), so a record diverges only from a
// manifest rewritten after the build: a publish hook rewrites the recorded
// selection on the two build legs, and the test rewrites it between calls
// for the cache-hit leg. `missing_plugin_ids` and `extra_plugin_ids` are
// compared with the compiled set difference and `graph_metadata.totalNodes`
// with the resident graph's `node_count()`, never with literals. A declared
// control: it exists so that dropping either half of
// `resident_roster_context` does not survive the daemon suite.
// ---------------------------------------------------------------------------

/// Rewrite the plugin selection the manifest at `root` records to
/// `plugins` under `high_cost_mode`, leaving the rest of the manifest as it
/// is: another writer changing the selection after a build.
fn rewrite_manifest_selection(
    root: &std::path::Path,
    plugins: &sqry_core::plugin::PluginManager,
    high_cost_mode: &str,
) {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest readable");
    manifest.plugin_selection = Some(
        sqry_core::graph::unified::persistence::PluginSelectionManifest {
            active_plugin_ids: plugin_ids_of(plugins),
            high_cost_mode: Some(high_cost_mode.to_string()),
        },
    );
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
}

/// Which selection [`RewriteSelectionHook`] writes on the next publish.
#[derive(Debug, Clone, Copy)]
enum NextSelection {
    /// Every compiled plugin, `include_all`.
    IncludeAll,
    /// The fast path, `fast_path_default`.
    FastPath,
}

/// A publish hook that, once per arming, rewrites the manifest's selection
/// after the build persisted it and before the tool answers: the
/// deterministic form of a manifest another writer changed between a
/// build and its answer.
#[derive(Debug)]
struct RewriteSelectionHook {
    root: std::path::PathBuf,
    next: std::sync::Mutex<Option<NextSelection>>,
}

impl RewriteSelectionHook {
    fn arm(&self, selection: NextSelection) {
        *self.next.lock().expect("hook lock") = Some(selection);
    }

    fn armed(&self) -> bool {
        self.next.lock().expect("hook lock").is_some()
    }
}

impl sqry_daemon::SqrydHook for RewriteSelectionHook {
    fn on_publish(
        &self,
        _workspace_root: &std::path::Path,
        _graph: std::sync::Arc<sqry_core::graph::CodeGraph>,
    ) {
        let Some(selection) = self.next.lock().expect("hook lock").take() else {
            return;
        };
        match selection {
            NextSelection::IncludeAll => rewrite_manifest_selection(
                &self.root,
                &sqry_plugin_registry::create_plugin_manager_all(),
                "include_all",
            ),
            NextSelection::FastPath => rewrite_manifest_selection(
                &self.root,
                &sqry_plugin_registry::create_plugin_manager(),
                "fast_path_default",
            ),
        }
    }
}

/// The ids of a JSON array of strings, as a set.
fn id_set(value: &Value) -> std::collections::BTreeSet<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("an array of ids, got {value}"))
        .iter()
        .map(|id| id.as_str().expect("a string id").to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_rebuild_index_envelope_carries_warning_and_metadata_control() {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let fast = sqry_plugin_registry::create_plugin_manager();
    let full = sqry_plugin_registry::create_plugin_manager_all();
    let fast_ids: BTreeSet<String> = plugin_ids_of(&fast).into_iter().collect();
    let full_ids: BTreeSet<String> = plugin_ids_of(&full).into_iter().collect();
    let beyond_fast: BTreeSet<String> = full_ids.difference(&fast_ids).cloned().collect();
    assert!(
        beyond_fast.contains("json"),
        "fixture precondition: json is the compiled plugin outside the fast path"
    );

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(&root, &fast, "fast_path_default");
    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let hook = Arc::new(RewriteSelectionHook {
        root: root.clone(),
        next: std::sync::Mutex::new(None),
    });
    server
        .manager
        .set_hook(Arc::clone(&hook) as sqry_daemon::SharedHook);
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    fn assert_envelope(
        structured: &Value,
        leg: &str,
        missing: &BTreeSet<String>,
        extra: &BTreeSet<String>,
        resident_nodes: u64,
    ) {
        let warning = structured
            .get("plugin_selection_warning")
            .unwrap_or_else(|| panic!("{leg}: plugin_selection_warning missing; got {structured}"));
        assert_eq!(
            warning["status"],
            json!("diverges_from_manifest"),
            "{leg}: {warning}"
        );
        assert_eq!(
            &id_set(&warning["missing_plugin_ids"]),
            missing,
            "{leg}: missing_plugin_ids: {warning}"
        );
        assert_eq!(
            &id_set(&warning["extra_plugin_ids"]),
            extra,
            "{leg}: extra_plugin_ids: {warning}"
        );
        let metadata = structured
            .get("graph_metadata")
            .unwrap_or_else(|| panic!("{leg}: graph_metadata missing; got {structured}"));
        assert_eq!(
            metadata["totalNodes"],
            json!(resident_nodes),
            "{leg}: graph_metadata.totalNodes is the resident graph's node_count: {metadata}"
        );
        assert_eq!(
            structured["data"]["nodeCount"],
            json!(resident_nodes),
            "{leg}: data.nodeCount describes the resident graph"
        );
        assert_eq!(structured["data"]["success"], json!(true), "{leg}");
    }
    let resident = |leg: &str| {
        let ws = server
            .manager
            .lookup(&key)
            .unwrap_or_else(|| panic!("{leg}: resident"));
        let published = ws.published();
        let ids: BTreeSet<String> = published
            .roster
            .as_ref()
            .expect("the resident generation carries a record")
            .active_plugin_ids
            .iter()
            .cloned()
            .collect();
        (published.graph.node_count() as u64, ids)
    };
    let none = BTreeSet::new();

    // Load leg: nothing is resident, so `force: true` loads through the
    // durable build, which records the fast-path manifest it read; the
    // hook then records every compiled plugin, so the record lacks json.
    hook.arm(NextSelection::IncludeAll);
    let result = mcp_rebuild_index(&server, &path, true)
        .await
        .expect("rebuild_index with force builds");
    assert!(!hook.armed(), "load leg: the hook rewrote the manifest");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    let (load_nodes, load_ids) = resident("load leg");
    assert!(load_nodes > 0, "the fixture builds a non-empty graph");
    assert_eq!(load_ids, fast_ids, "load leg: the record is the fast path");
    assert_eq!(
        structured["data"]["message"],
        json!("Index rebuilt successfully.")
    );
    assert_envelope(&structured, "load leg", &beyond_fast, &none, load_nodes);

    // Cache-hit leg: `force: false` with the workspace resident and the
    // manifest still recording every compiled plugin.
    let result = mcp_rebuild_index(&server, &path, false)
        .await
        .expect("rebuild_index without force reports the existing index");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    assert_eq!(
        structured["data"]["message"],
        json!("Index already exists. Use force=true to rebuild."),
        "the cache-hit leg reports the existing index"
    );
    assert_eq!(
        resident("cache-hit leg").0,
        load_nodes,
        "the cache-hit leg builds nothing"
    );
    assert_envelope(
        &structured,
        "cache-hit leg",
        &beyond_fast,
        &none,
        load_nodes,
    );

    // In-place leg: resident, so `force: true` rebuilds through the
    // dispatcher, which resolves the manifest's selection (every compiled
    // plugin) and records it; the hook then records the fast path, so the
    // record carries json beyond the manifest.
    hook.arm(NextSelection::FastPath);
    let result = mcp_rebuild_index(&server, &path, true)
        .await
        .expect("rebuild_index with force rebuilds the resident workspace");
    assert!(!hook.armed(), "in-place leg: the hook rewrote the manifest");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    let (in_place_nodes, in_place_ids) = resident("in-place leg");
    assert_eq!(
        in_place_ids, full_ids,
        "in-place leg: the record is the manifest's selection"
    );
    assert_ne!(
        in_place_nodes, load_nodes,
        "fixture precondition: the json build differs in size from the fast-path build"
    );
    assert_eq!(
        structured["data"]["message"],
        json!("Index rebuilt successfully.")
    );
    assert_envelope(
        &structured,
        "in-place leg",
        &none,
        &beyond_fast,
        in_place_nodes,
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// T44, its in-place twin, and T45 (round 4, design D20, codex R4-2 and
// R4-23): the daemon-hosted `rebuild_index` envelope describes the
// generation it returns, on the load leg and on the in-place leg, and after
// an eviction its cache-hit leg carries no roster context at all.
// ---------------------------------------------------------------------------

/// A publish hook that, on the first `on_publish` it receives, records
/// every compiled plugin in the manifest, evicts `key` and loads it again
/// through `second`: a second publication planted from inside the dispatch
/// `get_or_load` performs after its own publish, the deterministic form of
/// codex's R4-2 control. The second load's own dispatch finds `fired` set
/// and returns.
struct RepublishOnceHook {
    manager: std::sync::Arc<sqry_daemon::WorkspaceManager>,
    key: WorkspaceKey,
    second: std::sync::Arc<dyn sqry_daemon::WorkspaceBuilder>,
    fired: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for RepublishOnceHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepublishOnceHook")
            .field("key", &self.key)
            .field("fired", &self.fired)
            .finish_non_exhaustive()
    }
}

impl sqry_daemon::SqrydHook for RepublishOnceHook {
    fn on_publish(
        &self,
        _workspace_root: &std::path::Path,
        _graph: std::sync::Arc<sqry_core::graph::CodeGraph>,
    ) {
        if self.fired.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        rewrite_manifest_selection(
            &self.key.source_root,
            &sqry_plugin_registry::create_plugin_manager_all(),
            "include_all",
        );
        assert!(
            self.manager.evict_for_test(&self.key),
            "the plant evicts the generation just published"
        );
        self.manager
            .get_or_load(&self.key, self.second.as_ref(), 0)
            .expect("the plant publishes the second generation");
    }
}

/// The node count of a fast-path build of the fixture, derived by the test
/// from the fixture rather than read back from the daemon.
fn fast_path_reference_nodes(root: &std::path::Path) -> u64 {
    let fast = sqry_plugin_registry::create_plugin_manager();
    let graph = sqry_core::graph::unified::build::build_unified_graph(
        root,
        &fast,
        &sqry_core::graph::unified::build::BuildConfig::default(),
    )
    .expect("reference fast-path build");
    graph.node_count() as u64
}

/// Assert the envelope describes the fast-path generation the caller's own
/// build published, while the slot holds a second generation built from
/// every compiled plugin (the manifest's own selection, so a warning read
/// from the slot would be absent).
fn assert_describes_its_own_generation(
    leg: &str,
    structured: &Value,
    slot: &sqry_daemon::workspace::loaded::PublishedGraph,
    reference_nodes: u64,
) {
    use std::collections::BTreeSet;

    let fast = sqry_plugin_registry::create_plugin_manager();
    let full = sqry_plugin_registry::create_plugin_manager_all();
    let fast_ids: BTreeSet<String> = plugin_ids_of(&fast).into_iter().collect();
    let full_ids: BTreeSet<String> = plugin_ids_of(&full).into_iter().collect();
    let beyond_fast: BTreeSet<String> = full_ids.difference(&fast_ids).cloned().collect();

    // Control: the plant did publish, so the slot holds the second
    // generation, whose record equals the manifest.
    let slot_ids: BTreeSet<String> = slot
        .roster
        .as_ref()
        .expect("the second generation carries a record")
        .active_plugin_ids
        .iter()
        .cloned()
        .collect();
    assert_eq!(
        slot_ids, full_ids,
        "{leg}: the plant published the manifest's own record"
    );
    let slot_nodes = slot.graph.node_count() as u64;

    let warning_present = structured.get("plugin_selection_warning").is_some();
    let node_count = structured["data"]["nodeCount"]
        .as_u64()
        .expect("data.nodeCount is a number");
    println!(
        "R4-2 {leg} plant: warning_present={warning_present} nodeCount={node_count} \
         reference_nodes={reference_nodes} slot_nodes={slot_nodes}"
    );
    assert_ne!(
        slot_nodes, reference_nodes,
        "fixture precondition: the second generation (with json) differs in size from the \
         fast-path generation, so the two graphs are told apart by their node counts"
    );

    // The envelope describes the generation it returns: the first
    // build's record beside the first build's graph.
    let warning = structured
        .get("plugin_selection_warning")
        .unwrap_or_else(|| {
            panic!(
                "{leg}: the envelope must carry the warning of the generation it returns; \
                 warning_present={warning_present}; envelope: {structured}"
            )
        });
    assert_eq!(
        warning["status"],
        json!("diverges_from_manifest"),
        "{leg}: warning: {warning}"
    );
    assert_eq!(
        id_set(&warning["missing_plugin_ids"]),
        beyond_fast,
        "{leg}: missing_plugin_ids is the compiled set beyond the fast path: {warning}"
    );
    assert_eq!(
        node_count, reference_nodes,
        "{leg}: data.nodeCount is the fast-path generation's node count"
    );
    assert_eq!(
        structured["graph_metadata"]["totalNodes"],
        json!(node_count),
        "{leg}: graph_metadata.totalNodes describes the same generation as data.nodeCount: \
         {structured}"
    );
    assert_eq!(structured["data"]["success"], json!(true), "{leg}");
}

/// T44. A fast-path index and the production resolver, plus a
/// `RepublishOnceHook` whose second builder is the production resolver.
/// `rebuild_index` with `force: true` (nothing resident) builds and records
/// the fast path; the hook then records every compiled plugin and
/// publishes a second generation built from that record. The answer must
/// carry the warning computed from the first build's record
/// (`missing_plugin_ids` = the compiled set beyond the fast path) and
/// `graph_metadata.totalNodes == data.nodeCount` = the fast-path build's
/// node count. On the pre-round-4 handler the warning was computed from the
/// second record, which matches the manifest, so the key was absent
/// (`warning_present=false`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_rebuild_index_envelope_describes_the_graph_it_returns() {
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let fast = sqry_plugin_registry::create_plugin_manager();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(&root, &fast, "fast_path_default");
    let reference_nodes = fast_path_reference_nodes(&root);
    assert!(reference_nodes > 0, "the fixture builds a non-empty graph");

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    let second: Arc<dyn sqry_daemon::WorkspaceBuilder> = Arc::new(
        sqry_daemon::RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new())),
    );
    let hook = Arc::new(RepublishOnceHook {
        manager: Arc::clone(&server.manager),
        key: key.clone(),
        second,
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    server
        .manager
        .set_hook(Arc::clone(&hook) as sqry_daemon::SharedHook);

    let result = mcp_rebuild_index(&server, &path, true)
        .await
        .expect("rebuild_index with force builds");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    assert!(
        hook.fired.load(std::sync::atomic::Ordering::Acquire),
        "the plant ran from the publish hook"
    );
    let slot = server
        .manager
        .lookup(&key)
        .expect("resident after the plant")
        .published();
    assert_describes_its_own_generation("load leg", &structured, &slot, reference_nodes);

    server.stop().await;
}

/// T44's in-place twin: the same fixture, resident first, then
/// `rebuild_index` with `force: true` rebuilds in place through the
/// dispatcher. Held after its publish (the dispatcher's post-publish test
/// hold), the iteration finds the manifest recording every compiled plugin
/// and the slot republished from it; released, its answer must still
/// describe the fast-path generation it published, never the slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial(sga05_acquire_counter)]
async fn daemon_rebuild_index_in_place_describes_the_graph_it_returns() {
    use std::sync::Arc;
    use std::time::Duration;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let fast = sqry_plugin_registry::create_plugin_manager();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(&root, &fast, "fast_path_default");
    let reference_nodes = fast_path_reference_nodes(&root);
    assert!(reference_nodes > 0, "the fixture builds a non-empty graph");

    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    mcp_rebuild_index(&server, &path, true)
        .await
        .expect("the load leg makes the workspace resident");

    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installed");
    capture.arm_post_publish_hold();
    let plant = async {
        tokio::time::timeout(Duration::from_secs(120), capture.wait_until_post_publish())
            .await
            .expect("the in-place iteration publishes");
        let own_nodes = capture.published_generations.lock()[0].graph.node_count() as u64;
        rewrite_manifest_selection(
            &root,
            &sqry_plugin_registry::create_plugin_manager_all(),
            "include_all",
        );
        assert!(server.manager.evict_for_test(&key), "evicted");
        let manager = Arc::clone(&server.manager);
        let reload_key = key.clone();
        tokio::task::spawn_blocking(move || {
            manager.get_or_load(
                &reload_key,
                &sqry_daemon::RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new())),
                0,
            )
        })
        .await
        .expect("reload task")
        .expect("a second generation publishes");
        capture.release_post_publish();
        own_nodes
    };
    let (outcome, own_nodes) = tokio::join!(mcp_rebuild_index(&server, &path, true), plant);
    assert_eq!(
        own_nodes, reference_nodes,
        "the in-place iteration built the fast path"
    );
    let result = outcome.expect("rebuild_index with force rebuilds in place");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    let slot = server
        .manager
        .lookup(&key)
        .expect("resident after the plant")
        .published();
    assert_describes_its_own_generation("in-place leg", &structured, &slot, reference_nodes);

    server.stop().await;
}

/// T45 (a declared control; battery row K33's envelope oracle beside
/// VR16's status oracle). A fast-path index, made resident by
/// `rebuild_index` with `force: true`, then a manifest rewritten to record
/// every compiled plugin: `rebuild_index` with `force: false` while
/// resident carries the warning (control); after `evict_for_test` the same
/// call answers "Index already exists. Use force=true to rebuild." with no
/// `plugin_selection_warning` key and `graph_metadata` null, and the
/// `daemon/status` row is `Evicted` with `plugin_roster` null: a tombstone
/// holds no record (design D20, Q8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(sga05_acquire_counter)]
async fn daemon_rebuild_index_after_eviction_carries_no_roster_context_control() {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use sqry_daemon::WorkspaceRosterResolver;
    use tempfile::TempDir;

    let fast = sqry_plugin_registry::create_plugin_manager();
    let full = sqry_plugin_registry::create_plugin_manager_all();
    let fast_ids: BTreeSet<String> = plugin_ids_of(&fast).into_iter().collect();
    let full_ids: BTreeSet<String> = plugin_ids_of(&full).into_iter().collect();
    let beyond_fast: BTreeSet<String> = full_ids.difference(&fast_ids).cloned().collect();
    assert!(
        beyond_fast.contains("json"),
        "fixture precondition: json is the compiled plugin outside the fast path"
    );

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_mixed_fixture(&root);
    index_with(&root, &fast, "fast_path_default");
    let server = server_with_resolver(Arc::new(WorkspaceRosterResolver::new())).await;
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    // Make the workspace resident with the fast-path record.
    let result = mcp_rebuild_index(&server, &path, true)
        .await
        .expect("rebuild_index with force builds");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    let resident_nodes = server
        .manager
        .lookup(&key)
        .expect("resident after the rebuild")
        .graph()
        .node_count() as u64;
    assert!(resident_nodes > 0, "the fixture builds a non-empty graph");
    assert_eq!(structured["data"]["nodeCount"], json!(resident_nodes));
    rewrite_manifest_selection(&root, &full, "include_all");

    // Control leg: resident, the cache-hit envelope carries the warning
    // and the metadata of the resident generation.
    let result = mcp_rebuild_index(&server, &path, false)
        .await
        .expect("rebuild_index without force reports the existing index");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    assert_eq!(
        structured["data"]["message"],
        json!("Index already exists. Use force=true to rebuild.")
    );
    assert_eq!(
        id_set(&structured["plugin_selection_warning"]["missing_plugin_ids"]),
        beyond_fast,
        "control: resident, the warning describes the resident record: {structured}"
    );
    assert_eq!(
        structured["graph_metadata"]["totalNodes"],
        json!(resident_nodes),
        "control: resident, the metadata describes the resident graph: {structured}"
    );

    // The pin: after eviction the tombstone holds no record and no graph.
    assert!(
        server.manager.evict_for_test(&key),
        "workspace was resident"
    );
    let result = mcp_rebuild_index(&server, &path, false)
        .await
        .expect("rebuild_index without force reports the existing index");
    let structured = result
        .structured_content
        .clone()
        .expect("structured_content present");
    assert_eq!(
        structured["data"]["message"],
        json!("Index already exists. Use force=true to rebuild."),
        "evicted: the cache-hit leg still reports the existing index: {structured}"
    );
    let warning_present = structured.get("plugin_selection_warning").is_some();
    let metadata_null = structured["graph_metadata"].is_null();
    assert!(
        !warning_present,
        "evicted: a tombstone carries no record to warn about; envelope: {structured}"
    );
    assert!(
        metadata_null,
        "evicted: a tombstone holds no graph to describe; envelope: {structured}"
    );

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let row = status_row_for(&mut client, &path).await;
    assert_eq!(row["state"], json!("Evicted"), "row: {row}");
    assert!(
        row["plugin_roster"].is_null(),
        "evicted: the status row carries no roster record; row: {row}"
    );
    println!(
        "T45 evicted envelope: warning_present={warning_present} graph_metadata_null={metadata_null} \
         state={} plugin_roster={}",
        row["state"], row["plugin_roster"]
    );

    drop(client);
    server.stop().await;
}
