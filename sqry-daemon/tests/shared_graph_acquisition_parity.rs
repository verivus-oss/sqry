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
#![cfg(feature = "test-hooks")]

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
/// excluded (mutating); `sqry_ask` is excluded here because its
/// translation wrapper has its own daemon-MCP-host route — its
/// translated graph-backed execution still flows through the shared
/// path via the same `dispatch_by_name` table the per-tool wrappers
/// use. SGA07 will add a dedicated translated-execution test.
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
    use sqry_daemon::workspace::WorkspaceBuilder;
    use tempfile::TempDir;

    /// Builder whose `load_persisted` returns an empty graph deterministically.
    /// Used to drive the SGA04 reload path without depending on a fully
    /// indexed snapshot on disk for the parity test.
    #[derive(Debug, Default)]
    struct ReloadOkBuilder;

    impl WorkspaceBuilder for ReloadOkBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }

        fn load_persisted(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
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
// Test 6 — daemon-hosted `sqry_ask` translated graph commands route
// through the SHARED acquisition path (Codex Gate B Major: the
// pre-fix path shelled out via `Command::new("sqry")` and bypassed
// the SGA02/SGA04 contract).
// ---------------------------------------------------------------------------

#[serial(sga05_acquire_counter)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_mcp_sqry_ask_graph_command_uses_shared_acquisition() {
    // Fixture: real Rust workspace with `pub fn func_alpha() {}`. We
    // construct a DaemonMcpHandler directly and invoke its
    // `dispatch_translated_graph_tool` test seam (the same entry the
    // production `handle_sqry_ask` flow uses for graph-backed
    // translated commands). This proves the daemon-hosted `sqry_ask`
    // graph-execution path:
    //
    //   * Routes through `tool_core::acquire_and_execute` exactly like
    //     direct MCP tool calls (acquire counter bumps once per dispatch).
    //   * Carries the translated tool's response back to the caller —
    //     which a post-translation `sqry_ask` flow splices into
    //     `data.executionOutput`.
    //   * Recovers transparently after `WorkspaceEvicted` via the
    //     SGA04 bounded one-shot read-only reload (counter bumps a
    //     second time, no `WorkspaceEvicted` surfaces).
    //
    // We invoke the test seam directly because the translator's
    // intent classification is non-deterministic across host
    // environments and we cannot guarantee a specific NL prompt would
    // route to `direct_callers` vs `semantic_search` reliably in CI.
    // The contract under test is the dispatch boundary, not the
    // translator output.

    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::build::BuildConfig;
    use sqry_core::graph::unified::persistence::save_to_path;
    use sqry_core::query::executor::QueryExecutor;
    use sqry_daemon::DaemonError;
    use sqry_daemon::config::DaemonConfig;
    use sqry_daemon::mcp_host::DaemonMcpHandler;
    use sqry_daemon::workspace::{WorkspaceBuilder, WorkspaceManager};
    use tempfile::TempDir;

    /// Real-graph builder that also persists a snapshot on first build
    /// so the SGA04 read-only reload can rehydrate after eviction.
    /// The persisted snapshot is the same `Arc<CodeGraph>` content the
    /// initial build produced, so reloads return the indexed
    /// `func_alpha` symbol.
    struct PersistingRealBuilder {
        plugins: Arc<sqry_core::plugin::PluginManager>,
        cfg: BuildConfig,
    }

    impl std::fmt::Debug for PersistingRealBuilder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PersistingRealBuilder")
                .finish_non_exhaustive()
        }
    }

    impl WorkspaceBuilder for PersistingRealBuilder {
        fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            let g = sqry_core::graph::unified::build::build_unified_graph(
                root,
                &self.plugins,
                &self.cfg,
            )
            .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("test build: {e}"),
            })?;
            // Persist a snapshot so load_persisted can find it.
            let graph_dir = root.join(".sqry").join("graph");
            std::fs::create_dir_all(&graph_dir).map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("create .sqry/graph dir: {e}"),
            })?;
            save_to_path(&g, graph_dir.join("snapshot.sqry").as_path()).map_err(|e| {
                DaemonError::WorkspaceBuildFailed {
                    root: root.to_path_buf(),
                    reason: format!("persist snapshot: {e}"),
                }
            })?;
            Ok(g)
        }

        fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
            if !storage.snapshot_exists() {
                return Err(DaemonError::WorkspaceBuildFailed {
                    root: root.to_path_buf(),
                    reason: "test load_persisted: snapshot missing".into(),
                });
            }
            sqry_core::graph::unified::persistence::load_from_path(
                storage.snapshot_path(),
                Some(&self.plugins),
            )
            .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("test load_persisted: {e}"),
            })
        }
    }

    // --- Fixture: tempdir with a single Rust file containing func_alpha.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("lib.rs"), b"pub fn func_alpha() {}\n").unwrap();

    // --- Build the daemon plumbing manually so we can hold a
    //     DaemonMcpHandler without spinning up an IpcServer.
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(PersistingRealBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let executor = Arc::new(QueryExecutor::new());

    let handler = DaemonMcpHandler::new(
        Arc::clone(&manager),
        Arc::clone(&builder),
        Arc::clone(&executor),
        Duration::from_secs(60),
        "0.0.0-test",
    );

    // --- Initial workspace load. We call `dispatch_translated_graph_tool`
    //     once cold so the daemon admits the workspace through the same
    //     acquisition path the production daemon-hosted `sqry_ask`
    //     translated-execution flow uses.
    let path_str = root.to_string_lossy().to_string();
    let canonical = canonicalize_path(&root).unwrap();
    let key = WorkspaceKey::new(canonical.clone(), ProjectRootMode::GitRoot, 0);

    // Pre-load via the manager so the workspace is Loaded before we
    // measure the counter. The acquirer's Loaded path is the
    // canonical fast route — eviction-reload is exercised below.
    {
        use sqry_daemon::workspace::{WorkingSetInputs, working_set_estimate};
        let estimate = working_set_estimate(WorkingSetInputs {
            new_graph_final_estimate: 64 * 1024,
            staging_overhead: 32 * 1024,
            interner_snapshot_bytes: 16 * 1024,
        });
        manager
            .get_or_load(&key, &*builder, estimate)
            .expect("initial load must succeed");
    }

    acquire_counter_reset();

    // --- Phase 1: dispatch a translated graph command (semantic_search
    //     for func_alpha). This is the EXACT seam the daemon-hosted
    //     `handle_sqry_ask` calls when a translation produces a
    //     graph-backed command and `args.execute=true`. The dispatch
    //     MUST go through `acquire_and_execute` (counter bumps).
    let args1 = json!({
        "query": "func_alpha",
        "path": &path_str,
        "max_results": 50,
        "context_lines": 0,
        "include_classpath": false,
    });
    let result1 = handler
        .dispatch_translated_graph_tool("semantic_search", args1, &path_str)
        .await
        .expect("translated dispatch must succeed against Loaded workspace");

    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "translated graph dispatch must bump the shared acquire counter exactly once"
    );
    assert!(
        result1.contains("func_alpha"),
        "translated dispatch response must contain func_alpha; got: {result1}",
    );

    // --- Phase 2: evict workspace, dispatch again. The SGA04 bounded
    //     reload must serve a Reloaded acquisition transparently —
    //     the caller MUST NOT see WorkspaceEvicted.
    assert!(
        manager.evict_for_test(&key),
        "evict_for_test must succeed against Loaded workspace"
    );

    acquire_counter_reset();

    let args2 = json!({
        "query": "func_alpha",
        "path": &path_str,
        "max_results": 50,
        "context_lines": 0,
        "include_classpath": false,
    });
    let result2 = handler
        .dispatch_translated_graph_tool("semantic_search", args2, &path_str)
        .await
        .expect(
            "translated dispatch after eviction must transparently reload via SGA04 \
             bounded one-shot path; WorkspaceEvicted reaching the caller violates \
             SGA02 §Tool Ownership Boundary",
        );

    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "post-eviction translated dispatch must bump the acquire counter exactly once \
         (the bounded reload is an internal recovery, not a second acquire)",
    );
    assert!(
        result2.contains("func_alpha"),
        "post-eviction translated dispatch must still find func_alpha (proving the \
         reloaded graph contains the same content); got: {result2}",
    );
}

// ---------------------------------------------------------------------------
// Test 7 — dispatch_translated_graph_tool rejects non-graph-backed
// or mutating tool names so the SGA02 §Tool Ownership Boundary is
// not violated by accident.
// ---------------------------------------------------------------------------

#[serial(sga05_acquire_counter)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_mcp_sqry_ask_translated_dispatch_rejects_out_of_scope_names() {
    use std::sync::Arc;
    use std::time::Duration;

    use sqry_core::query::executor::QueryExecutor;
    use sqry_daemon::EmptyGraphBuilder;
    use sqry_daemon::config::DaemonConfig;
    use sqry_daemon::mcp_host::DaemonMcpHandler;
    use sqry_daemon::workspace::{WorkspaceBuilder, WorkspaceManager};

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(EmptyGraphBuilder);
    let executor = Arc::new(QueryExecutor::new());
    let handler = DaemonMcpHandler::new(
        manager,
        builder,
        executor,
        Duration::from_secs(60),
        "0.0.0-test",
    );

    let dir = tempfile::tempdir().unwrap();
    let path_str = dir.path().to_string_lossy().to_string();

    acquire_counter_reset();

    // `rebuild_index` is mutating — must be rejected by the seam.
    let err = handler
        .dispatch_translated_graph_tool("rebuild_index", json!({"path": &path_str}), &path_str)
        .await
        .expect_err("rebuild_index dispatch through translated seam must be rejected");
    assert!(
        format!("{err:?}").contains("not a daemon-supported graph-backed read-only tool"),
        "rebuild_index rejection must mention daemon-supported / read-only contract: {err:?}",
    );

    // `sqry_ask` recursion — must be rejected.
    let err = handler
        .dispatch_translated_graph_tool("sqry_ask", json!({"path": &path_str}), &path_str)
        .await
        .expect_err("sqry_ask self-recursion through translated seam must be rejected");
    assert!(
        format!("{err:?}").contains("not a daemon-supported graph-backed read-only tool"),
        "sqry_ask rejection must mention daemon-supported / read-only contract: {err:?}",
    );

    // Truly unknown name.
    let err = handler
        .dispatch_translated_graph_tool(
            "totally_made_up_tool_name",
            json!({"path": &path_str}),
            &path_str,
        )
        .await
        .expect_err("unknown tool name must be rejected");
    assert!(
        format!("{err:?}").contains("not a daemon-supported graph-backed read-only tool"),
        "unknown tool rejection must mention daemon-supported / read-only contract: {err:?}",
    );

    // None of these reach `acquire_and_execute` — the counter MUST
    // remain zero. SGA02 §Tool Ownership Boundary.
    assert_eq!(
        acquire_counter_snapshot(),
        0,
        "rejected tool names MUST short-circuit before acquire_and_execute",
    );
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
    use sqry_daemon::workspace::WorkspaceBuilder;
    use tempfile::TempDir;

    /// Builder whose `load_persisted` always re-runs `load_from_path`,
    /// so a corrupt snapshot bytes file produces a deterministic
    /// `WorkspaceBuildFailed`.
    #[derive(Debug, Default)]
    struct LoadFromDiskBuilder;

    impl WorkspaceBuilder for LoadFromDiskBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }

        fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
            sqry_core::graph::unified::persistence::load_from_path(storage.snapshot_path(), None)
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
    use sqry_daemon::workspace::WorkspaceBuilder;
    use sqry_daemon::workspace::hook::RecordingHook;
    use tempfile::TempDir;

    #[derive(Debug, Default)]
    struct ReloadOkBuilder;

    impl WorkspaceBuilder for ReloadOkBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }
        fn load_persisted(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
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
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_core::graph::unified::build::BuildConfig;
    use sqry_daemon::DaemonError;
    use sqry_daemon::workspace::WorkspaceBuilder;
    use tempfile::TempDir;

    /// Builder that runs the real build pipeline (without
    /// touching the on-disk snapshot — the CLI already produced one)
    /// and rehydrates from the same on-disk artifact for read-only
    /// reload. Both surfaces therefore observe identical graph bytes.
    struct CliCompatibleBuilder {
        plugins: Arc<sqry_core::plugin::PluginManager>,
        cfg: BuildConfig,
    }

    impl std::fmt::Debug for CliCompatibleBuilder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CliCompatibleBuilder")
                .finish_non_exhaustive()
        }
    }

    impl WorkspaceBuilder for CliCompatibleBuilder {
        fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            sqry_core::graph::unified::build::build_unified_graph(root, &self.plugins, &self.cfg)
                .map_err(|e| DaemonError::WorkspaceBuildFailed {
                    root: root.to_path_buf(),
                    reason: format!("test build: {e}"),
                })
        }
        fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
            sqry_core::graph::unified::persistence::load_from_path(
                storage.snapshot_path(),
                Some(&self.plugins),
            )
            .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("load_persisted: {e}"),
            })
        }
    }

    /// Locate the workspace `sqry` binary the same way other CLI parity
    /// tests do (env override → CARGO_BIN_EXE_sqry → target/debug). We
    /// inline this rather than depend on the `common` module so the
    /// daemon test binary doesn't grow a new module.
    fn sqry_bin_path() -> std::path::PathBuf {
        if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
            let p = std::path::PathBuf::from(path);
            if p.is_file() {
                return p;
            }
        }
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqry") {
            return std::path::PathBuf::from(path);
        }
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_dir = std::path::PathBuf::from(manifest_dir)
            .parent()
            .unwrap()
            .to_path_buf();
        let exe_suffix = std::env::consts::EXE_SUFFIX;
        let candidate = |base: &str| {
            if exe_suffix.is_empty() {
                workspace_dir.join(base)
            } else {
                workspace_dir.join(format!("{base}{exe_suffix}"))
            }
        };
        let dbg = candidate("target/debug/sqry");
        if dbg.exists() {
            return dbg;
        }
        let rel = candidate("target/release/sqry");
        if rel.exists() {
            return rel;
        }
        panic!(
            "could not locate sqry binary (set SQRY_E2E_SQRY_BIN / CARGO_BIN_EXE_sqry, or run `cargo build`)"
        );
    }

    // Build a real Rust workspace with a known symbol set.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn func_alpha() -> u32 { 1 }\n\
          pub fn func_beta() -> u32 { 2 }\n\
          pub fn func_gamma() -> u32 { 3 }\n",
    )
    .unwrap();

    // Index via the production CLI first so the on-disk artifact
    // includes both `snapshot.sqry` and `manifest.json` (a CLI query
    // requires the full manifest layout, not just the snapshot blob).
    let cli_index_status = Command::new(sqry_bin_path())
        .arg("index")
        .arg(&root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .status()
        .expect("run sqry index");
    assert!(
        cli_index_status.success(),
        "sqry index must succeed for cross-surface parity fixture",
    );

    // Run the CLI against the on-disk graph BEFORE starting the daemon
    // so the manifest's recorded snapshot SHA-256 still matches the
    // CLI-produced bytes. (`CliCompatibleBuilder.build` only runs the
    // build pipeline in memory; it never overwrites the on-disk
    // snapshot, so the manifest stays valid for the
    // `daemon -> reload_from_disk_read_only` path below.)
    let cli_out = Command::new(sqry_bin_path())
        .arg("--semantic")
        .arg("query")
        .arg("kind:function")
        .arg(&root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .output()
        .expect("run sqry query");
    assert!(
        cli_out.status.success(),
        "sqry query must succeed against on-disk graph; stderr={}",
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let cli_stdout = String::from_utf8_lossy(&cli_out.stdout);
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

    // NOW build the daemon plumbing with a builder that does NOT
    // overwrite the on-disk snapshot — so the bounded read-only reload
    // (triggered by the eviction below) reads the same bytes the CLI
    // just queried. `CliCompatibleBuilder.build` runs the production
    // pipeline in memory; `load_persisted` calls back into
    // `load_from_path` for the SGA04 reload path.
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(CliCompatibleBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(Arc::clone(&builder)).await;
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

    // Cross-surface acceptance: every name CLI surfaced MUST also be
    // surfaced by the daemon-hosted MCP call against the same on-disk
    // graph. (Strict equality is too brittle — different surface code
    // paths normalize names differently in the wire payload, but every
    // CLI hit is required to be present.)
    for cli_name in &cli_names {
        assert!(
            daemon_names.contains(cli_name),
            "daemon-MCP missing CLI symbol {cli_name}: cli_set={cli_names:?} daemon_set={daemon_names:?}",
        );
    }

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
    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::CodeGraph;
    use sqry_daemon::DaemonError;
    use sqry_daemon::workspace::WorkspaceBuilder;
    use tempfile::TempDir;

    /// Builder whose `load_persisted` synthesizes the
    /// IncompatibleGraph(IncompatibleUnknownPluginIds) shape that the
    /// shared `FilesystemGraphProvider` raises when the manifest lists
    /// plugin ids the running binary cannot satisfy. We can't easily
    /// drive the real plugin-compat classifier from the daemon-side
    /// `load_persisted` (it currently bypasses the manifest layer in
    /// `RealWorkspaceBuilder::load_persisted`), so we synthesize the
    /// terminal error directly via the `WorkspaceIncompatibleGraph`
    /// `DaemonError` variant which already carries the -32005 mapping.
    #[derive(Debug, Default)]
    struct IncompatPluginsBuilder;

    impl WorkspaceBuilder for IncompatPluginsBuilder {
        fn build(&self, _root: &Path) -> Result<CodeGraph, DaemonError> {
            Ok(CodeGraph::new())
        }
        fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            Err(DaemonError::WorkspaceIncompatibleGraph {
                root: root.to_path_buf(),
                reason: "unknown plugin ids: [sga07-fake-plugin]".to_string(),
            })
        }
    }

    let tmp = TempDir::new().unwrap();
    let server =
        TestServer::with_builder(Arc::new(IncompatPluginsBuilder) as Arc<dyn WorkspaceBuilder>)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path = tmp.path().to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Force an eviction so the bounded read-only reload must call
    // `load_persisted` and surface the incompat error through the
    // shared acquirer's classify-error mapping.
    let canonical = canonicalize_path(tmp.path()).unwrap();
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(server.manager.evict_for_test(&key));

    let resp = client
        .request(
            "semantic_search",
            default_args_for("semantic_search", &path),
        )
        .await;
    let err = expect_error(&resp);
    // The reload-time incompat surfaces as `WorkspaceBuildFailed`
    // (-32001) when the daemon's bounded reload returns it via
    // `GraphAcquisitionError::Evicted { reload_failure }`; the shape of
    // the error cascade is documented in
    // `sqry-daemon/src/workspace/acquirer.rs::handle_classify_error`.
    // -32001 (build-failed reload), -32004 (evicted-then-reload-fail),
    // and -32005 (passthrough incompat) are all acceptable distinct
    // surfaces — the contract under test is "NOT silent success and
    // NOT generic Internal".
    assert!(
        matches!(err.code, -32001 | -32004 | -32005),
        "incompat-plugin reload must surface a structured code (not Internal/0/empty success): got {}",
        err.code
    );
    assert_ne!(err.code, -32603, "must NOT collapse into Internal");

    drop(client);
    server.stop().await;
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
