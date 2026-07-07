//! RWS09 revision daemon API and query-routing integration tests.

mod support;

use std::{path::Path, process::Command, sync::Arc};

use serde_json::{Value, json};
use sqry_daemon::{EmptyGraphBuilder, JSONRPC_REVISION_SELECTOR_AMBIGUOUS, WorkspaceBuilder};
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
