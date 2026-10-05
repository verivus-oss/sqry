//! RWS09 revision daemon API and query-routing integration tests.

// The IPC test server and client run over a Unix domain socket
// (`support::ipc`), so this binary is Unix-only.
#![cfg(unix)]

mod support;

use std::{path::Path, process::Command, sync::Arc};

use serde_json::{Value, json};
use sqry_daemon::{
    EmptyGraphBuilder, JSONRPC_REVISION_SELECTOR_AMBIGUOUS, RealWorkspaceBuilder, WorkspaceBuilder,
    WorkspaceRosterResolver,
};
use sqry_daemon_client::DaemonClient;
use sqry_daemon_protocol::{
    ENVELOPE_VERSION, ListRevisionsRequest, LoadRevisionRequest, LoadRevisionResult, QueryResult,
    RevisionSelector, RevisionStatus, SearchResult, SourceByteMode, UnloadRevisionResult,
};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_revision_search_returns_provenance_and_omitted_revision_stays_live_only() {
    let repo = git_repo_with_one_commit();
    let server =
        TestServer::with_builder(Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>).await;
    let mut client = connected_client(&server).await;

    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": repo.path().to_string_lossy() }),
            )
            .await,
    );

    let loaded = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "ref", "name": "main"}),
        true,
    )
    .await;
    assert_eq!(
        loaded.resolved.source_byte_mode,
        SourceByteMode::RawGitObjects
    );
    assert!(loaded.resolved.commit_oid.is_some());

    let explicit = daemon_search(
        &mut client,
        repo.path(),
        "anything",
        Some(json!({
            "kind": "revision_id",
            "revision_id": loaded.revision_id,
        })),
    )
    .await;
    let revision = explicit
        .revision
        .expect("explicit revision search must carry provenance");
    assert_eq!(
        revision.revision_id,
        Some(loaded.status.revision_id.clone())
    );
    assert_eq!(revision.artifact_id, Some(loaded.artifact_id.clone()));
    assert_eq!(
        revision
            .resolved
            .as_ref()
            .map(|resolved| resolved.tree_oid.clone()),
        Some(loaded.resolved.tree_oid.clone())
    );

    let by_selector = daemon_search(
        &mut client,
        repo.path(),
        "anything",
        Some(json!({
            "kind": "selector",
            "selector": {"kind": "commit", "oid": loaded.resolved.commit_oid.clone().unwrap()},
        })),
    )
    .await;
    assert_eq!(
        by_selector
            .revision
            .and_then(|metadata| metadata.revision_id),
        Some(loaded.status.revision_id.clone())
    );

    let live_default = daemon_search(&mut client, repo.path(), "anything", None).await;
    assert!(
        live_default.revision.is_none(),
        "omitted selector must preserve live-workspace-only wire shape"
    );

    let explicit_query = daemon_query(
        &mut client,
        repo.path(),
        "kind:function",
        Some(json!({
            "kind": "revision_id",
            "revision_id": loaded.revision_id,
        })),
    )
    .await;
    let query_revision = explicit_query
        .revision
        .expect("explicit daemon/query revision must carry provenance");
    assert_eq!(
        query_revision.revision_id,
        Some(loaded.status.revision_id.clone())
    );
    assert_eq!(query_revision.artifact_id, Some(loaded.artifact_id.clone()));

    let live_query_default = daemon_query(&mut client, repo.path(), "kind:function", None).await;
    assert!(
        live_query_default.revision.is_none(),
        "omitted daemon/query selector must preserve live-workspace-only wire shape"
    );

    let list = client
        .request(
            "daemon/listRevisions",
            json!({"root": repo.path(), "include_unloaded": false}),
        )
        .await;
    let list = expect_success(&list);
    assert_eq!(
        list["result"]["revisions"]
            .as_array()
            .expect("revisions")
            .len(),
        1
    );

    let status = client
        .request(
            "daemon/revisionStatus",
            json!({"revision_id": loaded.status.revision_id}),
        )
        .await;
    let status: RevisionStatus = serde_json::from_value(expect_success(&status)["result"].clone())
        .expect("status response envelope");
    assert_eq!(status.artifact_id, loaded.artifact_id);

    let refused = client
        .request(
            "daemon/unloadRevision",
            json!({"revision_id": loaded.status.revision_id, "force": false}),
        )
        .await;
    let refused = expect_error(&refused);
    assert!(
        refused.message.contains("pinned"),
        "pinned unload should be refused, got {refused:?}"
    );

    let unloaded = client
        .request(
            "daemon/unloadRevision",
            json!({"revision_id": loaded.status.revision_id, "force": true}),
        )
        .await;
    let unloaded: UnloadRevisionResult =
        serde_json::from_value(expect_success(&unloaded)["result"].clone())
            .expect("unload response envelope");
    assert!(unloaded.unloaded);

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selector_query_reports_ambiguous_dirty_revisions() {
    let repo = git_repo_with_one_commit();
    let server =
        TestServer::with_builder(Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>).await;
    let mut client = connected_client(&server).await;

    let first = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "dirty", "include_untracked": false, "include_ignored": false}),
        false,
    )
    .await;
    std::fs::write(repo.path().join("src/lib.rs"), b"pub fn changed() {}\n").expect("modify file");
    let second = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "dirty", "include_untracked": false, "include_ignored": false}),
        false,
    )
    .await;
    assert_ne!(first.revision_id, second.revision_id);

    let resp = client
        .request(
            "daemon/search",
            search_params(
                repo.path(),
                "anything",
                Some(json!({
                    "kind": "selector",
                    "selector": {"kind": "dirty", "include_untracked": false, "include_ignored": false},
                })),
            ),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, JSONRPC_REVISION_SELECTOR_AMBIGUOUS);

    server.stop().await;
}

/// Regression guard for verivus-oss/sqry#510.
///
/// Drives the real management client (`DaemonClient`, the same type the
/// CLI `sqry daemon load-revision` / `list-revisions` path uses) end to
/// end against a live server. Before the fix the revision handlers
/// returned a bare result value while the client decodes a
/// `ResponseEnvelope<T>`, so `load_revision`/`list_revisions` failed with
/// `SchemaMismatch` ("missing field `result`") even though the daemon had
/// built the revision graph. This test fails on the buggy bare shape and
/// passes only when the handlers wrap their result in `ResponseEnvelope`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_client_round_trips_load_and_list_revisions() {
    let repo = git_repo_with_one_commit();
    let server =
        TestServer::with_builder(Arc::new(EmptyGraphBuilder) as Arc<dyn WorkspaceBuilder>).await;

    let mut client = DaemonClient::connect(&server.path)
        .await
        .expect("DaemonClient::connect must succeed");

    let load = client
        .load_revision(LoadRevisionRequest {
            root: repo.path().to_path_buf(),
            selector: RevisionSelector::Ref {
                name: "main".to_owned(),
            },
            source_byte_mode: None,
            pin: false,
        })
        .await
        .expect("load_revision must decode the ResponseEnvelope");
    assert_eq!(load.meta.daemon_version, env!("CARGO_PKG_VERSION"));
    assert!(
        load.result.resolved.commit_oid.is_some(),
        "loaded immutable revision must carry a resolved commit oid"
    );
    let loaded_revision_id = load.result.revision_id.clone();

    let list = client
        .list_revisions(ListRevisionsRequest {
            root: Some(repo.path().to_path_buf()),
            include_unloaded: false,
        })
        .await
        .expect("list_revisions must decode the ResponseEnvelope");
    assert_eq!(list.meta.daemon_version, env!("CARGO_PKG_VERSION"));
    assert!(
        list.result
            .revisions
            .iter()
            .any(|status| status.revision_id == loaded_revision_id),
        "listed revisions must include the freshly loaded handle"
    );

    server.stop().await;
}

async fn connected_client(server: &TestServer) -> TestIpcClient {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client
}

async fn load_revision(
    client: &mut TestIpcClient,
    root: &Path,
    selector: Value,
    pin: bool,
) -> LoadRevisionResult {
    let resp = client
        .request(
            "daemon/loadRevision",
            json!({
                "root": root,
                "selector": selector,
                "pin": pin,
            }),
        )
        .await;
    serde_json::from_value(expect_success(&resp)["result"].clone())
        .expect("loadRevision response envelope")
}

async fn daemon_search(
    client: &mut TestIpcClient,
    root: &Path,
    pattern: &str,
    revision: Option<Value>,
) -> SearchResult {
    let resp = client
        .request("daemon/search", search_params(root, pattern, revision))
        .await;
    let envelope = expect_success(&resp);
    serde_json::from_value(envelope["result"].clone()).expect("SearchResult envelope")
}

async fn daemon_query(
    client: &mut TestIpcClient,
    root: &Path,
    query: &str,
    revision: Option<Value>,
) -> QueryResult {
    let resp = client
        .request("daemon/query", query_params(root, query, revision))
        .await;
    let envelope = expect_success(&resp);
    serde_json::from_value(envelope["result"].clone()).expect("QueryResult envelope")
}

fn search_params(root: &Path, pattern: &str, revision: Option<Value>) -> Value {
    let mut params = json!({
        "envelope_version": ENVELOPE_VERSION,
        "pattern": pattern,
        "search_path": root.to_string_lossy(),
        "mode": "exact",
        "include_generated": false,
    });
    if let Some(revision) = revision {
        params["revision"] = revision;
    }
    params
}

fn query_params(root: &Path, query: &str, revision: Option<Value>) -> Value {
    let mut params = json!({
        "envelope_version": ENVELOPE_VERSION,
        "query": query,
        "search_path": root.to_string_lossy(),
        "limit": 10,
    });
    if let Some(revision) = revision {
        params["revision"] = revision;
    }
    params
}

fn git_repo_with_one_commit() -> TempDir {
    let repo = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(repo.path().join("src")).expect("src dir");
    std::fs::write(repo.path().join("src/lib.rs"), b"pub fn original() {}\n").expect("write src");
    git(repo.path(), ["init", "-b", "main"]);
    git(
        repo.path(),
        ["config", "user.email", "rws09@example.invalid"],
    );
    git(repo.path(), ["config", "user.name", "RWS09 Test"]);
    git(repo.path(), ["add", "src/lib.rs"]);
    git(repo.path(), ["commit", "-m", "initial"]);
    repo
}

fn git<const N: usize>(root: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git failed in {}: {}\nstdout: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

// ---------------------------------------------------------------------------
// T14 (surface parity W1): a revision artifact is keyed on the roster the
// repository's manifest records. Within ONE repository (so the repository
// identity hash is the same for both loads), the same committed tree
// loaded under an `include_all` manifest and then under a fast-path
// manifest keys two different artifacts, and the stored artifact manifest
// names `json` in the first roster digest and not in the second. Two
// separate repositories would differ in `repo_identity_hash` regardless of
// the roster, which is why the differential stays inside one repository.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revision_artifact_key_uses_the_manifest_roster() {
    use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
    use sqry_core::graph::unified::persistence::PluginSelectionManifest;
    use sqry_core::progress::no_op_reporter;
    use sqry_daemon::workspace::revision::RevisionArtifactStore;
    use sqry_plugin_registry::{create_plugin_manager, create_plugin_manager_all};

    fn index_with(root: &Path, plugins: &sqry_core::plugin::PluginManager, mode: &str) {
        let ids: Vec<String> = plugins
            .plugins()
            .iter()
            .map(|plugin| plugin.metadata().id.to_string())
            .collect();
        build_and_persist_graph_with_progress(
            root,
            plugins,
            &BuildConfig::default(),
            "test:revision-roster",
            Some(PluginSelectionManifest {
                active_plugin_ids: ids,
                high_cost_mode: Some(mode.to_string()),
            }),
            no_op_reporter(),
        )
        .expect("index persists");
    }

    let repo = git_repo_with_one_commit();
    // `.sqry/` is untracked, so re-indexing never changes the committed tree.
    index_with(repo.path(), &create_plugin_manager_all(), "include_all");

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server = TestServer::with_builder_config_and_roster(
        builder,
        sqry_daemon::DaemonConfig::default(),
        resolver,
    )
    .await;
    let mut client = connected_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": repo.path().to_string_lossy() }),
            )
            .await,
    );

    let store = RevisionArtifactStore::new(RevisionArtifactStore::default_cache_root());
    let roster_digest = |result: &LoadRevisionResult| -> String {
        let dir = store
            .artifact_dir(
                &result.resolved.repository.repo_identity_hash,
                &result.artifact_id,
            )
            .expect("artifact dir");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).expect("read"))
                .expect("artifact manifest json");
        manifest["key_inputs"]["graph_schema"]["plugin_roster_digest"]
            .as_str()
            .expect("plugin_roster_digest present")
            .to_string()
    };

    let include_all = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "ref", "name": "main"}),
        true,
    )
    .await;
    let include_all_digest = roster_digest(&include_all);
    assert!(
        include_all_digest.split(',').any(|id| id == "json"),
        "include_all artifact must be keyed on a roster containing json: {include_all_digest}"
    );

    // Re-index fast path (the manifest now records no json), release the
    // resident handle, and load the same tree again.
    index_with(repo.path(), &create_plugin_manager(), "fast_path_default");
    expect_success(
        &client
            .request(
                "daemon/unloadRevision",
                json!({"revision_id": include_all.status.revision_id, "force": true}),
            )
            .await,
    );
    let fast_path = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "ref", "name": "main"}),
        true,
    )
    .await;
    assert_eq!(
        include_all.resolved.tree_oid, fast_path.resolved.tree_oid,
        "fixture precondition: both loads resolve the same committed tree"
    );
    let fast_path_digest = roster_digest(&fast_path);
    assert!(
        !fast_path_digest.split(',').any(|id| id == "json"),
        "fast-path artifact must not be keyed on json: {fast_path_digest}"
    );
    assert_ne!(include_all_digest, fast_path_digest);
    assert_ne!(
        include_all.artifact_id, fast_path.artifact_id,
        "the same tree built with different rosters must key different artifacts \
         (pre-change both digests were the fast path and the ids were equal)"
    );

    drop(client);
    server.stop().await;
}
