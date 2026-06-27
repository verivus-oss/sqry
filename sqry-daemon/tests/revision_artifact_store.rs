//! RWS12 artifact identity, query default, and source-mode validation tests.

mod support;

use std::{path::Path, sync::Arc};

use serde_json::{Value, json};
use sqry_daemon::{EmptyGraphBuilder, WorkspaceBuilder};
use sqry_daemon_protocol::{ENVELOPE_VERSION, LoadRevisionResult, QueryResult, SourceByteMode};
use support::{
    git,
    ipc::{TestIpcClient, TestServer, expect_success},
    revision_git_repo,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_tree_refs_reuse_artifact_and_omitted_query_uses_live_workspace() {
    let repo = revision_git_repo();
    git(repo.path(), &["branch", "alias"]);

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

    let main = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "ref", "name": "main"}),
    )
    .await;
    let alias = load_revision(
        &mut client,
        repo.path(),
        json!({"kind": "ref", "name": "alias"}),
    )
    .await;

    assert_eq!(main.revision_id, alias.revision_id);
    assert_eq!(main.resolved.tree_oid, alias.resolved.tree_oid);
    assert_eq!(main.artifact_id, alias.artifact_id);
    assert_eq!(
        main.resolved.source_byte_mode,
        SourceByteMode::RawGitObjects
    );

    let live_query = daemon_query(&mut client, repo.path(), "kind:function", None).await;
    assert!(
        live_query.revision.is_none(),
        "omitted daemon/query selector must remain live-workspace-only"
    );

    let explicit_query = daemon_query(
        &mut client,
        repo.path(),
        "kind:function",
        Some(json!({"kind": "revision_id", "revision_id": alias.revision_id})),
    )
    .await;
    let revision = explicit_query
        .revision
        .expect("explicit revision query must include provenance");
    assert_eq!(revision.artifact_id, Some(alias.artifact_id));

    server.stop().await;
}

#[test]
fn checkout_fingerprint_changes_checkout_artifact_key_and_raw_key_stays_distinct() {
    use sqry_daemon::workspace::revision::{
        ArtifactKeyInputs, CheckoutByteFingerprint, CheckoutFilterFingerprint,
        GraphSchemaFingerprint, PathScope, SourceDigest,
    };
    use sqry_daemon_protocol::ObjectFormat;

    let graph_schema = GraphSchemaFingerprint {
        graph_schema_version: 1,
        derived_schema_version: 1,
        sqry_build_version: "22.0.4".to_owned(),
        plugin_roster_digest: "rust@22.0.4".to_owned(),
        graph_config_hash: "config".to_owned(),
    };
    let base = ArtifactKeyInputs {
        repo_identity_hash: "repo-identity-hash".to_owned(),
        source_digest: SourceDigest::Tree {
            tree_oid: "a".repeat(40),
        },
        object_format: ObjectFormat::Sha1,
        path_scope: PathScope::Repository,
        source_byte_mode: SourceByteMode::RawGitObjects,
        checkout_fingerprint: None,
        graph_schema: graph_schema.clone(),
    };
    let mut eol = base.clone();
    eol.source_byte_mode = SourceByteMode::CheckoutBytes;
    eol.checkout_fingerprint = Some(CheckoutByteFingerprint {
        git_version: "git version 2.51.0".to_owned(),
        attributes_fingerprint: "attrs".to_owned(),
        core_autocrlf: Some("false".to_owned()),
        core_eol: Some("lf".to_owned()),
        working_tree_encoding: None,
        filters: Vec::new(),
        sparse_checkout_fingerprint: None,
        lfs_available: false,
        worktree_config_fingerprint: None,
    });
    let mut filter = eol.clone();
    filter.checkout_fingerprint = Some(CheckoutByteFingerprint {
        filters: vec![CheckoutFilterFingerprint {
            name: "lfs".to_owned(),
            config_hash: "lfs-config".to_owned(),
        }],
        ..eol.checkout_fingerprint.clone().expect("fingerprint")
    });

    assert_ne!(base.artifact_id().unwrap(), eol.artifact_id().unwrap());
    assert_ne!(eol.artifact_id().unwrap(), filter.artifact_id().unwrap());
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
) -> LoadRevisionResult {
    let resp = client
        .request(
            "daemon/loadRevision",
            json!({
                "root": root,
                "selector": selector,
                "pin": true,
            }),
        )
        .await;
    serde_json::from_value(expect_success(&resp).clone()).expect("loadRevision response")
}

async fn daemon_query(
    client: &mut TestIpcClient,
    root: &Path,
    query: &str,
    revision: Option<Value>,
) -> QueryResult {
    let mut params = json!({
        "envelope_version": ENVELOPE_VERSION,
        "query": query,
        "search_path": root.to_string_lossy(),
        "limit": 10,
    });
    if let Some(revision) = revision {
        params["revision"] = revision;
    }
    let resp = client.request("daemon/query", params).await;
    let envelope = expect_success(&resp);
    serde_json::from_value(envelope["result"].clone()).expect("QueryResult envelope")
}
