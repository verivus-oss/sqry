//! Issue #394 Part 1b: daemon-mode subtree scoping.
//!
//! Proves that a daemon-hosted tool call whose `path` names a
//! SUBDIRECTORY of a loaded workspace (rather than the workspace root
//! itself) resolves to its owning loaded workspace and scopes results to
//! that subtree, instead of failing workspace classification up front.
//!
//! Before this fix, `DaemonGraphProvider::acquire` built the
//! classification key straight from the requested path, so a subtree
//! path (not a registered `WorkspaceKey`) surfaced as `WorkspaceEvicted`
//! and the bounded reload of a non-existent snapshot failed. The
//! acquirer now resolves the subtree to the longest registered workspace
//! root that contains it (`WorkspaceManager::find_owning_workspace_root`
//! -> `sqry_core::workspace::scope::owning_workspace_root`) and
//! classifies against that owning root; the shared inner tool body then
//! applies the same subtree filter (`subtree_within` + `path_in_subtree`)
//! used in standalone mode.
//!
//! Coverage:
//!
//! 1. `find_unused` with a subdir `path` returns only the subtree's
//!    findings (a private uncalled function in that subtree), and NOT
//!    the sibling subtree's findings.
//! 2. `complexity_metrics` with a subdir `path` reports only that
//!    subtree's functions; the whole-workspace call reports both.
//! 3. A `path` under NO loaded workspace errors clearly (does not
//!    silently succeed with whole-workspace results).

#![allow(clippy::too_many_lines)]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use serial_test::serial;
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path, save_to_path};
use sqry_daemon::DaemonError;
use sqry_daemon::workspace::WorkspaceBuilder;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture: two sibling subtrees, each with distinctive symbol names
// ---------------------------------------------------------------------------

/// A private, uncalled function only in the FIRST subtree (unambiguously
/// "unused"). Named so it cannot collide with a directory name in the
/// response payload.
const FIRST_UNUSED: &str = "zeta_unused_in_first";
/// A function with a branch (complexity >= 1) only in the FIRST subtree.
const FIRST_METRIC: &str = "zeta_metric_in_first";
/// A private, uncalled function only in the SECOND subtree.
const SECOND_UNUSED: &str = "omega_unused_in_second";
/// A function with a branch only in the SECOND subtree.
const SECOND_METRIC: &str = "omega_metric_in_second";

/// Write the two-subtree workspace: `first/f.rs` and `second/s.rs`, each
/// with a private unused function and a pub function carrying a branch.
fn write_two_subtree_workspace(root: &Path) {
    std::fs::create_dir_all(root.join("first")).expect("create first dir");
    std::fs::create_dir_all(root.join("second")).expect("create second dir");
    std::fs::write(
        root.join("first").join("f.rs"),
        format!(
            "fn {FIRST_UNUSED}() {{}}\n\
             pub fn {FIRST_METRIC}() {{ let x = 3; if x > 1 {{ let _ = x + 1; }} }}\n"
        ),
    )
    .expect("write first/f.rs");
    std::fs::write(
        root.join("second").join("s.rs"),
        format!(
            "fn {SECOND_UNUSED}() {{}}\n\
             pub fn {SECOND_METRIC}() {{ let y = 4; if y > 1 {{ let _ = y + 1; }} }}\n"
        ),
    )
    .expect("write second/s.rs");
}

// ---------------------------------------------------------------------------
// Real-graph persisting builder (mirrors the SGA08 dogfood fixture so the
// test is self-contained).
// ---------------------------------------------------------------------------

struct SubtreePersistingBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    cfg: BuildConfig,
}

impl std::fmt::Debug for SubtreePersistingBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubtreePersistingBuilder")
            .finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for SubtreePersistingBuilder {
    fn build(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
        let g =
            sqry_core::graph::unified::build::build_unified_graph(root, &self.plugins, &self.cfg)
                .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("subtree build: {e}"),
            })?;
        let graph_dir = root.join(".sqry").join("graph");
        std::fs::create_dir_all(&graph_dir).map_err(|e| DaemonError::WorkspaceBuildFailed {
            root: root.to_path_buf(),
            reason: format!("create .sqry/graph dir: {e}"),
        })?;
        save_to_path(&g, graph_dir.join("snapshot.sqry").as_path()).map_err(|e| {
            DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("persist subtree snapshot: {e}"),
            }
        })?;
        Ok(g)
    }

    fn load_persisted(&self, root: &Path) -> Result<CodeGraph, DaemonError> {
        let storage = GraphStorage::new(root);
        if !storage.snapshot_exists() {
            return Err(DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: "subtree load_persisted: snapshot missing".into(),
            });
        }
        load_from_path(storage.snapshot_path(), Some(&self.plugins)).map_err(|e| {
            DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("subtree load_persisted: {e}"),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Response inspection
// ---------------------------------------------------------------------------

/// Recursively search a JSON value for a string containing `needle`.
fn json_contains_string(v: &Value, needle: &str) -> bool {
    match v {
        Value::String(s) => s.contains(needle),
        Value::Array(items) => items.iter().any(|x| json_contains_string(x, needle)),
        Value::Object(map) => map.values().any(|x| json_contains_string(x, needle)),
        _ => false,
    }
}

async fn boot_loaded_workspace(root: &Path) -> (TestServer, TestIpcClient, String) {
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(SubtreePersistingBuilder {
        plugins,
        cfg: BuildConfig::default(),
    });
    let server = TestServer::with_builder(builder).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let path_str = root.to_string_lossy().into_owned();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path_str }))
            .await,
    );
    (server, client, path_str)
}

// ---------------------------------------------------------------------------
// Test 1: find_unused scoped to a subtree
// ---------------------------------------------------------------------------

#[serial(subtree_scoping)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_unused_subdir_path_scopes_to_subtree() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    write_two_subtree_workspace(&root);

    let (server, mut client, root_str) = boot_loaded_workspace(&root).await;
    let first_subdir = root.join("first").to_string_lossy().into_owned();

    // Subtree call: only the FIRST subtree's unused function must surface.
    let resp = client
        .request(
            "find_unused",
            json!({ "path": first_subdir, "scope": "all", "max_results": 100 }),
        )
        .await;
    let payload = expect_success(&resp).clone();
    assert!(
        json_contains_string(&payload, FIRST_UNUSED),
        "#394 Part 1b: find_unused(path=first) MUST resolve the subtree to its \
         owning workspace and report the first subtree's unused symbol; \
         payload: {payload}",
    );
    assert!(
        !json_contains_string(&payload, SECOND_METRIC)
            && !json_contains_string(&payload, SECOND_UNUSED),
        "#394 Part 1b: find_unused(path=first) MUST NOT report the sibling \
         subtree's symbols; payload: {payload}",
    );

    // Whole-workspace call: both subtrees' unused symbols may surface,
    // proving the subtree call above actually narrowed the result.
    let resp_all = client
        .request(
            "find_unused",
            json!({ "path": root_str, "scope": "all", "max_results": 100 }),
        )
        .await;
    let payload_all = expect_success(&resp_all).clone();
    assert!(
        json_contains_string(&payload_all, FIRST_UNUSED)
            && json_contains_string(&payload_all, SECOND_UNUSED),
        "#394 Part 1b: whole-workspace find_unused must report BOTH subtrees' \
         unused symbols (baseline that scoping narrows); payload: {payload_all}",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: complexity_metrics scoped to a subtree
// ---------------------------------------------------------------------------

#[serial(subtree_scoping)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complexity_metrics_subdir_path_scopes_to_subtree() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    write_two_subtree_workspace(&root);

    let (server, mut client, root_str) = boot_loaded_workspace(&root).await;
    let second_subdir = root.join("second").to_string_lossy().into_owned();

    // Subtree call: only the SECOND subtree's function must surface.
    let resp = client
        .request(
            "complexity_metrics",
            json!({ "path": second_subdir, "min_complexity": 1, "max_results": 100 }),
        )
        .await;
    let payload = expect_success(&resp).clone();
    assert!(
        json_contains_string(&payload, SECOND_METRIC),
        "#394 Part 1b: complexity_metrics(path=second) MUST report the second \
         subtree's function; payload: {payload}",
    );
    assert!(
        !json_contains_string(&payload, FIRST_METRIC),
        "#394 Part 1b: complexity_metrics(path=second) MUST NOT report the \
         first subtree's function; payload: {payload}",
    );

    // Whole-workspace call: both functions surface.
    let resp_all = client
        .request(
            "complexity_metrics",
            json!({ "path": root_str, "min_complexity": 1, "max_results": 100 }),
        )
        .await;
    let payload_all = expect_success(&resp_all).clone();
    assert!(
        json_contains_string(&payload_all, FIRST_METRIC)
            && json_contains_string(&payload_all, SECOND_METRIC),
        "#394 Part 1b: whole-workspace complexity_metrics must report BOTH \
         functions; payload: {payload_all}",
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: a path under NO loaded workspace errors clearly
// ---------------------------------------------------------------------------

#[serial(subtree_scoping)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_under_no_loaded_workspace_errors_clearly() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    write_two_subtree_workspace(&root);

    let (server, mut client, _root_str) = boot_loaded_workspace(&root).await;

    // A real, existing directory that is NOT under the loaded workspace.
    let other = TempDir::new().expect("other tempdir");
    std::fs::create_dir_all(other.path().join("src")).expect("create other/src");
    let other_str = other.path().join("src").to_string_lossy().into_owned();

    let resp = client
        .request(
            "complexity_metrics",
            json!({ "path": other_str, "min_complexity": 1, "max_results": 100 }),
        )
        .await;
    // Must error (owning resolution returns None, classification against the
    // unknown path fails), NOT silently succeed with whole-workspace results.
    let err = expect_error(&resp);
    assert!(
        err.code != 0,
        "#394 Part 1b: a path under no loaded workspace must surface a clear \
         error, got code {}: {err:?}",
        err.code,
    );

    drop(client);
    server.stop().await;
}
