//! `daemon/search` integration coverage — DAEMON_SEARCH_TESTS unit for
//! verivus-oss/sqry#238 tier 2 (DAG `2026-05-11-issue-238-search-progress-ux-dag.toml`
//! lines 607–641, spec § 3.2.4).
//!
//! Four test groups:
//!
//! 1. **Parity**: spin up a real sqryd in-test, load a fixture workspace,
//!    run the same exact-name query via the IPC client AND via an
//!    independent in-process projection (mirroring the CLI's
//!    `convert_node_to_display_symbol` shape). Both vectors of
//!    `SearchItem` must be byte-equal across 5 fixture queries.
//!
//! 2. **Latency**: warm the workspace (load + 1 priming query),
//!    measure 100 successive `daemon/search` exact-name queries, assert
//!    p99 < 100 ms. The CI runner may be slower than a dev box; the
//!    threshold tracks the spec's claim plus generous IPC headroom
//!    against an in-process p99 < 50 ms baseline.
//!
//! 3. **Workspace-evicted reload-on-read** (gated on `feature =
//!    "test-hooks"` because eviction is driven through
//!    `WorkspaceManager::evict_for_test`): after a deliberate
//!    eviction the next `daemon/search` MUST recover transparently via
//!    the bounded one-shot reload, not surface `-32004
//!    WorkspaceEvicted`.
//!
//! 4. **Workspace-incompatible-graph -32005** (ungated, exercised via
//!    `daemon/load`): a builder whose `build()` returns
//!    `DaemonError::WorkspaceIncompatibleGraph` must surface as JSON-RPC
//!    `-32005`. The acceptance criterion is verified on the load path,
//!    not via post-eviction `daemon/search`, because
//!    `DaemonGraphProvider::handle_classify_error` deliberately
//!    collapses every reload failure into `-32004` per the SGA04
//!    bounded-one-shot-reload contract — the `-32005` wire envelope is
//!    only reachable through the load path. The detailed rationale
//!    lives in the docstring on the test function itself.
//!
//! No fixture used here lives outside `test-fixtures/cli-basic/`; the
//! suite does NOT depend on `/srv/repos/public/benchmark-repos/linux/`
//! or any other heavy fixture.

#![allow(clippy::too_many_lines)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::graph::unified::persistence::{GraphStorage, save_to_path};
use sqry_daemon::DaemonError;
use sqry_daemon::workspace::WorkspaceBuilder;
// `SearchMode` / `SearchRequest` / `expect_error` are only consumed by
// the `test-hooks`-gated tests further down; allow `unused_imports` so
// the default build (without that feature) does not warn.
#[allow(unused_imports)]
use sqry_daemon_protocol::{ENVELOPE_VERSION, SearchItem, SearchMode, SearchRequest, SearchResult};
#[allow(unused_imports)]
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture: copy `test-fixtures/cli-basic/` into a tempdir.
//
// `cli-basic` carries ~11 named Rust symbols (`calculate_sum`,
// `Calculator`, `multiply`, `Processor`, `PI`, `DefaultProcessor`,
// `subtract`, `divide`, `new`, `add`, `get_value`) — small, deterministic,
// and parity-friendly (no proc-macro expansion → no macro_generated
// metadata → `include_generated == false` is a no-op).
// ---------------------------------------------------------------------------

fn copy_cli_basic_fixture(tmp_root: &Path) {
    let repo_root = repo_root();
    let src = repo_root.join("test-fixtures").join("cli-basic");
    assert!(
        src.is_dir(),
        "test-fixtures/cli-basic missing at {} — required by DAEMON_SEARCH_TESTS",
        src.display()
    );
    let dst = tmp_root.to_path_buf();
    std::fs::create_dir_all(&dst).expect("create tmp_root");
    // Copy regular files only and skip the fixture's own `.sqry/`
    // pre-baked index directory — the daemon builds its own index
    // under the tempdir, and copying the existing `.sqry` would put a
    // stale snapshot in the way. README.md is harmless (sqry's indexer
    // ignores non-source extensions).
    for entry in std::fs::read_dir(&src).expect("read cli-basic dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(".sqry") {
            continue;
        }
        let file_type = entry.file_type().expect("file type");
        if !file_type.is_file() {
            continue;
        }
        let to = dst.join(name);
        std::fs::copy(&from, &to).unwrap_or_else(|e| {
            panic!("copy {} -> {}: {}", from.display(), to.display(), e);
        });
    }
}

/// Locate the repository root by walking up from the current test
/// executable until a `Cargo.toml` with `[workspace]` is found.
fn repo_root() -> PathBuf {
    let mut cur = std::env::current_exe().expect("current_exe");
    // current_exe is target/<profile>/deps/<test-binary>; walk up.
    while cur.pop() {
        let cargo = cur.join("Cargo.toml");
        if cargo.is_file()
            && let Ok(s) = std::fs::read_to_string(&cargo)
            && s.contains("[workspace]")
        {
            return cur;
        }
    }
    panic!(
        "could not locate workspace root from {:?}",
        std::env::current_exe()
    );
}

// ---------------------------------------------------------------------------
// Real-graph builder that persists a snapshot.
//
// Mirrors `DogfoodPersistingBuilder` in
// `tests/shared_graph_acquisition_dogfood.rs`. Duplicated rather than
// shared so this test file stays self-contained.
// ---------------------------------------------------------------------------

struct PersistingBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    cfg: BuildConfig,
}

impl std::fmt::Debug for PersistingBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistingBuilder").finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for PersistingBuilder {
    fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
        let g =
            sqry_core::graph::unified::build::build_unified_graph(root, &self.plugins, &self.cfg)
                .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("daemon-search-tests build: {e}"),
            })?;
        let graph_dir = root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: root.to_path_buf(),
            reason: format!("create .sqry/graph: {e}"),
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
        let storage = GraphStorage::new(root);
        if !storage.snapshot_exists() {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: "load_persisted: snapshot missing".into(),
            });
        }
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

// ---------------------------------------------------------------------------
// Independent in-process projection that mirrors the CLI's
// `convert_node_to_display_symbol` (lexically — drift between the
// daemon's `node_to_search_item` and this projection surfaces as a
// parity-test failure). Reusing the daemon's own helper would let drift
// hide; the test deliberately re-implements the wire shape.
// ---------------------------------------------------------------------------

fn in_process_exact_projection(graph: &CodeGraph, pattern: &str) -> SearchResult {
    let snapshot = graph.snapshot();
    let mut node_ids = snapshot.find_by_exact_name(pattern);
    node_ids.sort_unstable();
    node_ids.dedup();

    // `include_generated == false` parity. The cli-basic fixture has no
    // macro_generated metadata, so this is a no-op for the parity
    // assertions below — but the filter is exercised so a future
    // fixture change does not silently break the contract.
    let store = graph.macro_metadata();
    let node_ids: Vec<NodeId> = node_ids
        .into_iter()
        .filter(|nid| {
            store
                .get_macro(*nid)
                .is_none_or(|m| m.macro_generated != Some(true))
        })
        .collect();

    let items: Vec<SearchItem> = node_ids
        .into_iter()
        .filter_map(|nid| node_to_search_item_independent(graph, nid))
        .collect();
    let total = items.len() as u64;
    SearchResult {
        items,
        total,
        truncated: false,
        cursor: None,
    }
}

fn node_to_search_item_independent(graph: &CodeGraph, nid: NodeId) -> Option<SearchItem> {
    let entry = graph.nodes().get(nid)?;
    let strings = graph.strings();
    let files = graph.files();

    let name = strings.resolve(entry.name).map(|s| s.to_string())?;
    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());
    let file_path = files
        .resolve(entry.file)
        .map(|p| p.to_string_lossy().into_owned())?;
    let language = language_from_path_local(Path::new(&file_path));
    let kind = node_kind_to_str_local(entry.kind).to_owned();
    Some(SearchItem {
        name,
        qualified_name,
        kind,
        language,
        file_path,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        score: None,
    })
}

fn node_kind_to_str_local(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::CallSite => "call_site",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Other => "other",
    }
}

fn language_from_path_local(path: &Path) -> String {
    path.extension().and_then(|ext| ext.to_str()).map_or_else(
        || "unknown".to_string(),
        |ext| match ext.to_lowercase().as_str() {
            "rs" => "rust".to_string(),
            "py" | "pyw" => "python".to_string(),
            "ts" | "mts" | "cts" => "typescript".to_string(),
            "tsx" => "typescriptreact".to_string(),
            "js" | "mjs" | "cjs" => "javascript".to_string(),
            "jsx" => "javascriptreact".to_string(),
            "go" => "go".to_string(),
            "java" => "java".to_string(),
            _ => "unknown".to_string(),
        },
    )
}

// ---------------------------------------------------------------------------
// Helper: send a `daemon/search` request and parse the result envelope.
// ---------------------------------------------------------------------------

fn build_search_params(workspace_root: &Path, pattern: &str) -> Value {
    json!({
        "envelope_version": ENVELOPE_VERSION,
        "pattern": pattern,
        "search_path": workspace_root.to_string_lossy(),
        "mode": "exact",
        "include_generated": false,
    })
}

async fn daemon_search(
    client: &mut TestIpcClient,
    workspace_root: &Path,
    pattern: &str,
) -> SearchResult {
    let resp = client
        .request(
            "daemon/search",
            build_search_params(workspace_root, pattern),
        )
        .await;
    let envelope = expect_success(&resp);
    serde_json::from_value::<SearchResult>(envelope["result"].clone()).unwrap_or_else(|e| {
        panic!(
            "daemon/search response did not decode as SearchResult: {e}\n  envelope: {envelope}"
        );
    })
}

// ---------------------------------------------------------------------------
// 1. Parity: 5 fixture queries via daemon vs in-process projection.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_search_parity_with_in_process_for_five_fixture_queries() {
    let tmp = TempDir::new().expect("tempdir");
    copy_cli_basic_fixture(tmp.path());
    let workspace_root = tmp.path().to_path_buf();

    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(PersistingBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(Arc::clone(&builder)).await;

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    // daemon/load
    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": workspace_root.to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    // Build the same graph in-process for the independent projection.
    // Using the same builder type ensures both paths consume the same
    // input + plugin set; the projection helper above is independent
    // from the daemon's wire-shape code.
    let in_process_graph = sqry_core::graph::unified::build::build_unified_graph(
        &workspace_root,
        &plugins,
        &BuildConfig::default(),
    )
    .expect("in-process build");

    // 5 fixture queries chosen for coverage:
    //   - hits a known function (multi-letter, common): `calculate_sum`
    //   - hits a struct: `Calculator`
    //   - hits a const: `PI`
    //   - hits a trait: `Processor`
    //   - hits no-results (negative case): `does_not_exist`
    let queries = [
        "calculate_sum",
        "Calculator",
        "PI",
        "Processor",
        "does_not_exist",
    ];
    for pat in queries {
        let daemon_result = daemon_search(&mut client, &workspace_root, pat).await;
        let in_process_result = in_process_exact_projection(&in_process_graph, pat);

        // Byte-identical SearchItem JSON parity, per DAG line 632:
        //   "in-process and daemon paths produce byte-identical
        //    SqrySearchItem JSON for 5 fixture queries".
        // The daemon may apply a per-mode default limit (100 for
        // exact); the in-process projection above returns every
        // matching node so we compare unbounded by truncation. The
        // cli-basic fixture has at most a handful of nodes per name,
        // well below 100, so truncation never engages.
        assert!(
            !daemon_result.truncated,
            "fixture is too small to truncate; got {daemon_result:?} for pattern {pat}",
        );
        let daemon_json = serde_json::to_string(&daemon_result.items).expect("serialize daemon");
        let in_process_json =
            serde_json::to_string(&in_process_result.items).expect("serialize in-process");
        assert_eq!(
            daemon_json, in_process_json,
            "parity FAILED for pattern {pat}\n  daemon: {daemon_json}\n  in-process: {in_process_json}",
        );
        assert_eq!(
            daemon_result.total, in_process_result.total,
            "total mismatch for pattern {pat}",
        );
    }

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 2. Latency: 100 successive daemon/search calls, p99 < 100ms.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_search_latency_p99_under_100ms() {
    let tmp = TempDir::new().expect("tempdir");
    copy_cli_basic_fixture(tmp.path());
    let workspace_root = tmp.path().to_path_buf();

    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(PersistingBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(Arc::clone(&builder)).await;

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": workspace_root.to_string_lossy() }),
            )
            .await,
    );

    // Priming query — warms any per-request caches (trigram, etc.)
    // before the timed window so the first sample is not an outlier.
    let _warm = daemon_search(&mut client, &workspace_root, "calculate_sum").await;

    let mut samples: Vec<Duration> = Vec::with_capacity(100);
    for _ in 0..100 {
        let t = Instant::now();
        let _ = daemon_search(&mut client, &workspace_root, "calculate_sum").await;
        samples.push(t.elapsed());
    }
    samples.sort();
    // p99 = sample at index 98 (zero-based, ceiling of 0.99 * 100 - 1).
    let p99 = samples[98];
    assert!(
        p99 < Duration::from_millis(100),
        "daemon/search p99 = {p99:?} exceeds the 100ms threshold (samples \
         min={:?} median={:?} p95={:?} max={:?}). The DAG acceptance \
         criterion is `p99 < 100ms across 100 successive daemon/search \
         calls on a warmed workspace`.",
        samples[0],
        samples[50],
        samples[95],
        samples[99],
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 3. Workspace-evicted reload-on-read (test-hooks).
//
// The SGA04 shared-graph-acquisition contract states that post-eviction
// read-only tool calls perform a single bounded reload from the
// persisted snapshot — `WorkspaceEvicted` (-32004) MUST NOT surface for
// the 14 read-only graph-backed MCP tools. `daemon/search` routes
// through the same `acquire_and_execute` contract, so the same recovery
// guarantee applies here.
// ---------------------------------------------------------------------------

#[cfg(feature = "test-hooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_search_workspace_evicted_reload_on_read() {
    use sqry_core::project::{ProjectRootMode, canonicalize_path};
    use sqry_daemon::WorkspaceKey;

    let tmp = TempDir::new().expect("tempdir");
    copy_cli_basic_fixture(tmp.path());
    let workspace_root = tmp.path().to_path_buf();

    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(PersistingBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(Arc::clone(&builder)).await;

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": workspace_root.to_string_lossy() }),
            )
            .await,
    );

    // Warm: pre-eviction search must succeed.
    let _warm = daemon_search(&mut client, &workspace_root, "calculate_sum").await;

    // Evict deterministically via the test-hooks helper.
    let canonical = canonicalize_path(&workspace_root).expect("canonicalize");
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "evict_for_test must succeed against the freshly-loaded workspace",
    );

    // Post-eviction: the bounded one-shot reload must rehydrate the
    // workspace transparently so the search succeeds, with no
    // `-32004 WorkspaceEvicted` leak.
    let resp = client
        .request(
            "daemon/search",
            build_search_params(&workspace_root, "calculate_sum"),
        )
        .await;
    let envelope = expect_success(&resp);
    let result: SearchResult = serde_json::from_value(envelope["result"].clone())
        .expect("post-eviction SearchResult decode");
    assert!(
        !result.items.is_empty(),
        "post-eviction reload must return the same hits the pre-eviction \
         path did — got an empty result set, which means the reload \
         loaded a different (empty) graph",
    );
    // The wire envelope must report `state = Loaded` (the bounded
    // reload promotes Reloaded → Loaded per acquire_and_execute).
    assert_eq!(envelope["meta"]["workspace_state"], json!("Loaded"));

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 4. Workspace-incompatible-graph -32005.
//
// Acceptance criterion text: "load a fixture with a plugin selection
// that mismatches the daemon's enabled plugins, assert -32005 is
// returned". The wire-mapping check is on the load path — the daemon
// surfaces `DaemonError::WorkspaceIncompatibleGraph` (which the
// builder.build() return value can produce directly, mirroring the
// real plugin-mismatch path that `RealWorkspaceBuilder::load_persisted`
// would otherwise take when it sees `PersistenceError::IncompatibleVersion`).
//
// We do NOT test this through the post-eviction reload path because
// `DaemonGraphProvider::handle_classify_error` deliberately collapses
// every reload failure into `GraphAcquisitionError::Evicted` (-32004),
// per the SGA04 contract — preserving the bounded one-shot reload
// promise that callers see at most one transient -32004 across the
// eviction window. A reload-time incompatible-graph error therefore
// surfaces as -32004 + reload-failure diagnostic context, not -32005.
// The -32005 wire envelope is reachable through `daemon/load` (or any
// other path that propagates `DaemonError::WorkspaceIncompatibleGraph`
// directly), and that is what this test pins.
//
// No `test-hooks` gating needed — the builder.build() error path is
// reachable through the standard `daemon/load` handler.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_load_workspace_incompatible_graph_returns_32005() {
    /// Builder that always reports an incompatible graph from `build()`.
    /// Mirrors the wire shape `RealWorkspaceBuilder::load_persisted`
    /// produces when `PersistenceError::IncompatibleVersion` fires —
    /// the explicit synthetic shape lets the test exercise the
    /// wire-mapping for `-32005` without needing to hand-craft a
    /// snapshot file with a bogus header version.
    struct IncompatibleBuilder;
    impl std::fmt::Debug for IncompatibleBuilder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("IncompatibleBuilder")
                .finish_non_exhaustive()
        }
    }
    impl WorkspaceBuilder for IncompatibleBuilder {
        fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
            Err(DaemonError::WorkspaceIncompatibleGraph {
                root: root.to_path_buf(),
                reason: "test fixture: snapshot built with plugin selection \
                         that mismatches the daemon's enabled plugins"
                    .into(),
            })
        }
    }

    let tmp = TempDir::new().expect("tempdir");
    copy_cli_basic_fixture(tmp.path());
    let workspace_root = tmp.path().to_path_buf();

    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(IncompatibleBuilder);
    let server = TestServer::with_builder(Arc::clone(&builder)).await;

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": workspace_root.to_string_lossy() }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32005,
        "incompatible-graph load must surface as JSON-RPC -32005 \
         (WorkspaceIncompatibleGraph wire mapping), got: {err:?}",
    );

    drop(client);
    server.stop().await;
}
