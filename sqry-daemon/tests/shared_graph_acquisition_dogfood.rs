//! SGA08 — Dogfood regression test for the shared graph acquisition
//! contract.
//!
//! This test reproduces the original observed bug shape (CLI succeeds
//! while daemon-hosted MCP surfaces `WorkspaceEvicted` against the
//! same on-disk index) and pins the post-SGA02/03/04/05 invariant
//! that:
//!
//! 1. CLI `sqry query` and daemon-hosted MCP `semantic_search` both
//!    find the same known symbol against the same index.
//! 2. Eviction between index load and MCP search must be transparently
//!    recovered by the SGA04 bounded one-shot reload — the daemon
//!    client MUST NOT see `-32004` (`WorkspaceEvicted`).
//! 3. The shared acquire counter increments through the bounded
//!    reload, proving the fix routes through the shared provider
//!    instead of bypassing it.
//!
//! ## Test file location (deviation from DAG)
//!
//! The DAG (`docs/development/shared-graph-acquisition/IMPLEMENTATION_DAG.toml`,
//! `units.SGA08`) names the file as `tests/shared_graph_acquisition_dogfood.rs`
//! — i.e. workspace-root `tests/`. The sqry workspace is a virtual
//! workspace (no root `[package]`) and there is no existing
//! workspace-root `tests/` directory; the per-package `tests/`
//! convention is the established pattern (`sqry-cli/tests/`,
//! `sqry-daemon/tests/`, `sqry-lsp/tests/`, etc.). The DAG explicitly
//! permits placing the file at `sqry-daemon/tests/shared_graph_acquisition_dogfood.rs`
//! "and document the deviation". This file is the documented
//! deviation: the test lives next to the SGA05/SGA07 daemon-hosted
//! parity tests so it can reuse the `TestServer`, `TestIpcClient`,
//! and `acquire_counter_*` infrastructure exposed by the same crate.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p sqry-daemon --features test-hooks --test \
//!     shared_graph_acquisition_dogfood
//! ```

#![cfg(feature = "test-hooks")]
#![allow(clippy::too_many_lines)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;

use serde_json::{Value, json};
use serial_test::serial;
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path, save_to_path};
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::workspace::WorkspaceBuilder;
use sqry_daemon::{DaemonError, WorkspaceKey, acquire_counter_reset, acquire_counter_snapshot};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture: the known dogfood symbol
// ---------------------------------------------------------------------------

/// The known symbol name that both CLI and MCP must find. The name is
/// distinctive enough that any false-positive match in CI noise is
/// unmistakable. This is the SGA08 stand-in for the original
/// `NodeProvenance` failure case described in `01_SPEC.md` (the
/// observed CLI-success/MCP-evicted divergence is the regression
/// target). Per the test plan a synthetic tempdir fixture is preferred
/// over indexing sqry's own source tree because the latter is heavier,
/// slower, and noisier.
const DOGFOOD_SYMBOL: &str = "dogfood_func_alpha";

/// Source for the synthetic workspace. A single Rust file with a single
/// public function — the smallest possible fixture that exercises the
/// indexer + the symbol-name search predicate.
const DOGFOOD_SRC: &str = "pub fn dogfood_func_alpha() {}\n";

/// Build the synthetic workspace under `root`. Creates `src/lib.rs`
/// containing [`DOGFOOD_SRC`].
fn write_dogfood_workspace(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("create src dir");
    std::fs::write(root.join("src").join("lib.rs"), DOGFOOD_SRC).expect("write lib.rs");
}

// ---------------------------------------------------------------------------
// Workspace builder shared with the daemon-hosted MCP test
// ---------------------------------------------------------------------------

/// Real-graph builder used by the TestServer: invokes the production
/// `build_unified_graph` over the synthetic workspace and persists a
/// `.sqry/graph/snapshot.sqry` so the SGA04 bounded reload path can
/// rehydrate exactly the same content after `evict_for_test`.
///
/// This mirrors `PersistingRealBuilder` in
/// `shared_graph_acquisition_parity.rs` (the SGA05 Test 6 fixture)
/// but is duplicated here so this regression test is self-contained
/// and can be moved to a workspace-root `tests/` directory in a
/// future cleanup without depending on internal helpers from the
/// parity test file.
struct DogfoodPersistingBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    cfg: BuildConfig,
}

impl std::fmt::Debug for DogfoodPersistingBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DogfoodPersistingBuilder")
            .finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for DogfoodPersistingBuilder {
    fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
        let g =
            sqry_core::graph::unified::build::build_unified_graph(root, &self.plugins, &self.cfg)
                .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("dogfood build: {e}"),
            })?;
        // Persist a snapshot so `load_persisted` (the SGA04 reload
        // route) can rehydrate the same indexed graph.
        let graph_dir = root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: root.to_path_buf(),
            reason: format!("create .sqry/graph dir: {e}"),
        })?;
        save_to_path(&g, graph_dir.join("snapshot.sqry").as_path()).map_err(|e| {
            DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("persist dogfood snapshot: {e}"),
            }
        })?;
        Ok(g)
    }

    fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
        let storage = GraphStorage::new(root);
        if !storage.snapshot_exists() {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: "dogfood load_persisted: snapshot missing — \
                         the SGA08 fixture must persist a snapshot \
                         before the daemon's reload path runs"
                    .into(),
            });
        }
        load_from_path(storage.snapshot_path(), Some(&self.plugins)).map_err(|e| {
            DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("dogfood load_persisted: {e}"),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// CLI binary discovery
// ---------------------------------------------------------------------------

/// Locate the workspace `sqry` CLI binary. Mirrors the discovery logic
/// in `sqry-cli/tests/common/mod.rs` and `sqry-daemon/tests/e2e_smoke.rs`.
///
/// Search order:
/// 1. `SQRY_E2E_SQRY_BIN` (release smoke / installed-binary validation).
/// 2. `CARGO_BIN_EXE_sqry` (only set when sqry-cli is a dep, which it
///    isn't here — kept for parity with the cli-side helper).
/// 3. Walk up from `current_exe()` to `target/<profile>/sqry`.
///
/// Returns `None` when the binary cannot be found; the test prints a
/// directive build hint and skips rather than failing flakily.
fn find_sqry_cli_bin() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqry") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?; // target/debug/deps
    let bin_name = format!("sqry{}", std::env::consts::EXE_SUFFIX);
    let candidate = parent.join(&bin_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let grandparent = parent.parent()?; // target/debug
    let candidate = grandparent.join(&bin_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

// ---------------------------------------------------------------------------
// CLI driver helpers
// ---------------------------------------------------------------------------

/// Run `sqry index <root>` against `root`. Panics on non-zero exit so
/// the test fails with the indexer's stderr inline — the SGA08 spec
/// requires diagnostics that distinguish corrupt-graph / build-failure
/// modes from MCP-side reload failures.
fn run_sqry_index(bin: &Path, root: &Path) {
    let output = StdCommand::new(bin)
        .arg("index")
        .arg(root)
        .output()
        .expect("invoke sqry index");
    if !output.status.success() {
        panic!(
            "SGA08 dogfood: `sqry index` failed against {}\n  status: {:?}\n  \
             stdout: {}\n  stderr: {}\n  hint: rebuild the workspace with \
             `cargo build --workspace --bin sqry` before running this test",
            root.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Run `sqry query "name:<symbol>" <root>` and return `(stdout, stderr)`.
fn run_sqry_query_for_symbol(bin: &Path, root: &Path, symbol: &str) -> (String, String) {
    let predicate = format!("name:{symbol}");
    let output = StdCommand::new(bin)
        .arg("query")
        .arg(&predicate)
        .arg(root)
        .output()
        .expect("invoke sqry query");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        panic!(
            "SGA08 dogfood: `sqry query` failed against {}\n  status: {:?}\n  \
             stdout: {}\n  stderr: {}\n  hint: this is the CLI half of the \
             regression — if CLI is broken the MCP comparison cannot prove \
             the SGA08 invariant",
            root.display(),
            output.status,
            stdout,
            stderr,
        );
    }
    (stdout, stderr)
}

// ---------------------------------------------------------------------------
// MCP response inspection
// ---------------------------------------------------------------------------

/// Walk an arbitrary `serde_json::Value` looking for a string value
/// containing `needle`. Used because the MCP `semantic_search`
/// response schema is layered (envelope → `result` → `data` →
/// `results` → per-symbol fields) and the dogfood test only cares
/// whether the symbol name surfaced anywhere in the response payload.
fn json_contains_string(v: &Value, needle: &str) -> bool {
    match v {
        Value::String(s) => s.contains(needle),
        Value::Array(items) => items.iter().any(|x| json_contains_string(x, needle)),
        Value::Object(map) => map.values().any(|x| json_contains_string(x, needle)),
        _ => false,
    }
}

/// SGA08 acceptance: the regression test must distinguish failure
/// modes (invalid path, corrupt graph, stale expired, reload failed)
/// in its diagnostic output. This helper formats the JSON-RPC error
/// envelope so that on test failure the operator can immediately see
/// which class of failure occurred.
fn diagnose_error(err: &sqry_daemon::ipc::protocol::JsonRpcError) -> String {
    // The wire codes are stable per SGA02/SGA04 and the daemon
    // `error.rs` mapping. We surface the class explicitly so the
    // operator does not have to grep `error.rs` from a CI failure log.
    let class = match err.code {
        -32602 => "INVALID_PATH (-32602 InvalidArgument)",
        -32004 => {
            "WORKSPACE_EVICTED_OR_STALE_EXPIRED (-32004) — \
                   THIS IS THE SGA08 REGRESSION SHAPE"
        }
        -32001 => {
            "WORKSPACE_BUILD_FAILED (-32001) — corrupt graph \
                   or load_persisted error"
        }
        -32603 => "INTERNAL (-32603) — generic daemon failure",
        other => return format!("UNKNOWN error code {other}: {err:?}"),
    };
    format!("error class: {class}; raw: {err:?}")
}

// ---------------------------------------------------------------------------
// The dogfood regression test
// ---------------------------------------------------------------------------

#[serial(sga05_acquire_counter)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogfood_cli_and_daemon_mcp_agree_under_eviction() {
    // Pre-flight: locate the workspace `sqry` CLI binary. If the
    // binary is not present we skip the test rather than fail — the
    // CLI half of the comparison is meaningless without it. CI runs
    // `cargo build --workspace` before `cargo test --workspace` so
    // the binary will normally be present.
    let Some(sqry_bin) = find_sqry_cli_bin() else {
        eprintln!(
            "SGA08 dogfood: `sqry` CLI binary not found.\n  \
             Tried: SQRY_E2E_SQRY_BIN, CARGO_BIN_EXE_sqry, target/debug/sqry, \
             target/release/sqry.\n  \
             Build with: cargo build --workspace --bin sqry\n  \
             Skipping (this is a soft-skip — CI must build the binary first)."
        );
        return;
    };

    // ---- Step 1: build the synthetic workspace + index it via CLI.
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    write_dogfood_workspace(&root);

    run_sqry_index(&sqry_bin, &root);

    // The on-disk artifact must exist now; the daemon-side
    // `load_persisted` path depends on it. Surface a precise
    // diagnostic if the snapshot is missing — this distinguishes
    // "indexer failure" from "daemon reload failure" per the SGA08
    // failure-mode requirement.
    let snapshot_path = root.join(".sqry").join("graph").join("snapshot.sqry");
    assert!(
        snapshot_path.is_file(),
        "SGA08 dogfood: indexer succeeded but {} is missing — \
         CORRUPT_GRAPH failure mode (the daemon's load_persisted \
         path will fail in step 4)",
        snapshot_path.display(),
    );

    // ---- Step 2: CLI query for the known symbol must succeed.
    let (cli_stdout, cli_stderr) = run_sqry_query_for_symbol(&sqry_bin, &root, DOGFOOD_SYMBOL);
    assert!(
        cli_stdout.contains(DOGFOOD_SYMBOL),
        "SGA08 dogfood: CLI side of the parity comparison did NOT \
         find {DOGFOOD_SYMBOL} in the freshly-indexed workspace — \
         this is the CLI half of the regression and means either \
         the indexer or the CLI search predicate broke.\n  \
         stdout: {cli_stdout}\n  stderr: {cli_stderr}",
    );

    // ---- Step 3: spin up the TestServer with a real-graph builder.
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(DogfoodPersistingBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(Arc::clone(&builder)).await;

    // The default `DaemonConfig::memory_limit_mb` (2048) is
    // comfortably larger than the synthetic workspace's working-set
    // estimate, so admission control will not refuse the load.

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path_str = root.to_string_lossy().into_owned();

    // Daemon load — admits the workspace so we have a known starting
    // state before deliberate eviction.
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path_str }))
            .await,
    );

    // Reset the shared acquire counter AFTER `daemon/load` so the
    // load doesn't pollute the per-dispatch deltas.
    acquire_counter_reset();

    // ---- Step 4: cold MCP semantic_search MUST find the symbol.
    let resp_warm = client
        .request(
            "semantic_search",
            json!({
                "query": format!("name:{DOGFOOD_SYMBOL}"),
                "path": &path_str,
                "max_results": 50,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;

    let warm_payload = match &resp_warm.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => result.clone(),
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => panic!(
            "SGA08 dogfood: pre-eviction MCP semantic_search FAILED — \
             this is the BASELINE half of the regression and means the \
             daemon couldn't even serve the workspace before eviction.\n  \
             {}",
            diagnose_error(error),
        ),
    };

    assert!(
        json_contains_string(&warm_payload, DOGFOOD_SYMBOL),
        "SGA08 dogfood: pre-eviction MCP semantic_search returned \
         success but {DOGFOOD_SYMBOL} is NOT in the response — the \
         daemon's graph differs from the CLI's graph for the same \
         on-disk index. This is the EXACT regression shape the spec \
         names (`01_SPEC.md`: CLI succeeds, MCP returns empty).\n  \
         CLI saw: {cli_stdout}\n  MCP payload: {warm_payload}",
    );
    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "SGA08 dogfood: pre-eviction semantic_search must bump the \
         shared acquire counter exactly once (proof the dispatcher \
         routed through `acquire_and_execute`)",
    );

    // ---- Step 5: deterministic eviction via the test-hooks helper.
    let canonical = canonicalize_path(&root).expect("canonicalize tempdir root");
    let key = WorkspaceKey::new(canonical, ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "SGA08 dogfood: evict_for_test must succeed against the \
         Loaded workspace — without deterministic eviction the test \
         cannot prove the post-eviction reload behavior",
    );

    acquire_counter_reset();

    // ---- Step 6: post-eviction MCP semantic_search MUST recover
    //              transparently. The SGA08 acceptance test fails
    //              here if `WorkspaceEvicted` (-32004) surfaces.
    let resp_evicted = client
        .request(
            "semantic_search",
            json!({
                "query": format!("name:{DOGFOOD_SYMBOL}"),
                "path": &path_str,
                "max_results": 50,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;

    let evicted_payload = match &resp_evicted.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => result.clone(),
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
            // This is THE regression case: daemon-MCP surfaces
            // `WorkspaceEvicted` while CLI can still read the same
            // index. Fail with explicit diagnostics so it is
            // immediately distinguishable from an invalid-path or
            // corrupt-graph failure.
            panic!(
                "SGA08 dogfood: post-eviction MCP semantic_search FAILED.\n  \
                 {}\n  \
                 Note: a -32004 (WORKSPACE_EVICTED) here is the EXACT \
                 regression shape the SGA02/SGA04 contract was designed \
                 to prevent — the daemon's bounded one-shot reload \
                 (`acquire_and_execute` → `load_persisted` → retry) did \
                 not run, or it ran but propagated the error past the \
                 boundary. See `01_SPEC.md` and \
                 `sqry-daemon/src/workspace/acquirer.rs`.",
                diagnose_error(error),
            );
        }
    };

    assert_eq!(
        evicted_payload["meta"]["workspace_state"],
        json!("Loaded"),
        "SGA08 dogfood: post-reload semantic_search must report \
         workspace_state=Loaded; got {evicted_payload}",
    );

    assert!(
        json_contains_string(&evicted_payload, DOGFOOD_SYMBOL),
        "SGA08 dogfood: post-eviction MCP semantic_search returned \
         success and reported Loaded but {DOGFOOD_SYMBOL} is NOT in \
         the response — the bounded reload ran but loaded a graph \
         that differs from the persisted snapshot. This is RELOAD_FAILED \
         (the reload partially succeeded but the rehydrated graph is \
         wrong).\n  payload: {evicted_payload}",
    );

    assert_eq!(
        acquire_counter_snapshot(),
        1,
        "SGA08 dogfood: post-eviction semantic_search must bump the \
         shared acquire counter exactly once (the SGA04 bounded reload \
         is an internal recovery, not a second `acquire_and_execute` \
         entry).",
    );

    // ---- Step 7: explicitly assert the response did NOT carry
    //              `WorkspaceEvicted`-flavored top-level markers.
    //              The wire shape for a clean reload is identical to
    //              a Fresh acquisition (per SGA design §Staleness and
    //              Wire Compatibility).
    let inner = &evicted_payload["result"];
    assert!(
        inner.get("_workspace_evicted").is_none(),
        "SGA08 dogfood: post-reload payload MUST NOT carry a \
         `_workspace_evicted` marker — the reload metadata is \
         internal-only.\n  payload: {evicted_payload}",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Failure-mode discrimination test — assert the test harness
// distinguishes between invalid-path and reload-failed shapes per the
// SGA08 acceptance "failure output includes enough diagnostics to
// distinguish invalid path, corrupt graph, stale expired, and reload
// failed".
//
// We deliberately drive an invalid path through the same client to
// exercise `diagnose_error` and prove that the diagnostic helper
// labels a -32602 error as INVALID_PATH (not WORKSPACE_EVICTED). If
// the dogfood test above ever flakes, this companion test confirms
// the diagnostic plumbing is sound.
// ---------------------------------------------------------------------------

#[serial(sga05_acquire_counter)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogfood_diagnostics_distinguish_invalid_path_from_evicted() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    acquire_counter_reset();

    let resp = client
        .request(
            "semantic_search",
            json!({
                "query": format!("name:{DOGFOOD_SYMBOL}"),
                "path": "/this/path/does/not/exist/sga08/dogfood",
                "max_results": 1,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;

    let err = expect_error(&resp);
    assert_eq!(
        err.code,
        -32602,
        "SGA08 dogfood: invalid path must surface as -32602 \
         (InvalidArgument), not -32004 (WorkspaceEvicted) — if this \
         flips, the diagnostic helper would mislabel the failure \
         mode in CI logs.\n  diagnose_error: {}",
        diagnose_error(err),
    );

    let label = diagnose_error(err);
    assert!(
        label.contains("INVALID_PATH"),
        "SGA08 dogfood: diagnose_error must label -32602 as \
         INVALID_PATH; got: {label}",
    );
    assert!(
        !label.contains("WORKSPACE_EVICTED"),
        "SGA08 dogfood: diagnose_error MUST NOT label -32602 as \
         WORKSPACE_EVICTED — that mislabel would mask the true \
         regression in failure reports; got: {label}",
    );

    drop(client);
    server.stop().await;
}
