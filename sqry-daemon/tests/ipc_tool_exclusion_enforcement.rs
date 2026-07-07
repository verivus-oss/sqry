//! Issue verivus-oss/sqry#469: daemon-hosted exclusion enforcement across
//! the real pooled `CpuExecutor` tool seam.
//!
//! TC9 (in `sqry-mcp/tests/issue_469_exclusion_enforcement.rs`) proves the
//! daemon *reconstruction* seam (`resolve_logical_workspace_for_root` plus
//! `with_workspace_override`) yields an enforcing policy, but it binds the
//! policy on the test thread and never crosses the daemon's
//! `tool_core::execute_with_timeout` `CpuExecutor::run` boundary (the dedicated
//! CPU pool that replaced the raw `spawn_blocking` body in issue #503 Phase 2).
//! This test spins up a real sqryd, loads a workspace whose on-disk
//! `.sqry-workspace` registry excludes `secrets/`, and issues a genuine
//! `show_dependencies` tool request with an excluded `file_path`. The daemon
//! must reconstruct the policy inside the pooled tool closure (U04) and surface
//! the same `-32602 Invalid params` ("excluded by the logical workspace
//! policy") the standalone path emits, and an allowed sibling file must still
//! resolve.

#![allow(clippy::too_many_lines)]

mod support;

use serde_json::json;
use sqry_core::workspace::{
    WorkspaceMetadata, WorkspaceRegistry, WorkspaceRepoId, WorkspaceRepository,
};
use sqry_daemon::ipc::protocol::{JsonRpcPayload, JsonRpcResponse};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

/// Assert a JSON-RPC response is NOT the `-32602` exclusion rejection (it may
/// be a success, or a different, tool-specific error). Proves the daemon's
/// reconstructed policy gates the excluded path specifically, not everything.
fn assert_not_excluded(resp: &JsonRpcResponse) {
    if let JsonRpcPayload::Error { error } = &resp.payload {
        assert_ne!(
            error.code, -32602,
            "path must NOT hit the exclusion gate, got {error:?}"
        );
    }
}

/// Build a fixture source root with `src/main.rs`, `secrets/api_keys.toml`, and
/// a `.sqry-workspace` registry excluding the `secrets` subtree. Returns the
/// canonicalized root plus the two canonical file paths.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    secret_file: std::path::PathBuf,
    src_main: std::path::PathBuf,
}

fn make_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let secrets = root.join("secrets");
    let src = root.join("src");
    std::fs::create_dir_all(&secrets).expect("create secrets");
    std::fs::create_dir_all(&src).expect("create src");
    std::fs::write(secrets.join("api_keys.toml"), "[k]\nvalue = 1\n").expect("write secret");
    std::fs::write(src.join("main.rs"), "fn main() {}\n").expect("write source");

    let secrets = secrets.canonicalize().expect("canonical secrets");
    let mut registry = WorkspaceRegistry {
        metadata: WorkspaceMetadata {
            version: 2,
            workspace_name: Some("issue-469-daemon".to_string()),
            default_discovery_mode: None,
            created_at: std::time::SystemTime::now(),
            updated_at: std::time::SystemTime::now(),
        },
        repositories: vec![WorkspaceRepository::new(
            WorkspaceRepoId::new("root"),
            "root".to_string(),
            root.clone(),
            root.join(".sqry-index"),
            None,
        )],
        member_folders: Vec::new(),
        exclusions: vec![secrets.clone()],
        project_root_mode: Default::default(),
    };
    registry
        .save(&root.join(".sqry-workspace"))
        .expect("save .sqry-workspace");

    Fixture {
        secret_file: secrets.join("api_keys.toml"),
        src_main: src.join("main.rs").canonicalize().expect("canonical main"),
        root,
        _tmp: tmp,
    }
}

async fn load_fixture(fx: &Fixture) -> (TestServer, TestIpcClient) {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": fx.root.to_string_lossy() }),
            )
            .await,
    );
    (server, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn show_dependencies_excluded_file_path_emits_32602_over_daemon_seam() {
    let fx = make_fixture();
    let (server, mut client) = load_fixture(&fx).await;

    // Excluded `file_path`: the daemon must reconstruct the `.sqry-workspace`
    // exclusion policy inside the pooled `CpuExecutor` tool closure and reject
    // the path.
    let resp = client
        .request(
            "show_dependencies",
            json!({
                "path": fx.root.to_string_lossy(),
                "file_path": fx.secret_file.to_string_lossy(),
                "max_depth": 3,
                "max_results": 100,
                "offset": 0,
                "size": 100,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "excluded file_path must surface -32602 across the daemon seam, got {err:?}"
    );
    assert!(
        err.message
            .contains("excluded by the logical workspace policy"),
        "daemon-hosted rejection must mirror the standalone message, got {err:?}"
    );

    // Allowed sibling file under the same loaded workspace must still resolve,
    // proving the daemon rejection is path-scoped rather than a blanket failure.
    let resp = client
        .request(
            "show_dependencies",
            json!({
                "path": fx.root.to_string_lossy(),
                "file_path": fx.src_main.to_string_lossy(),
                "max_depth": 3,
                "max_results": 100,
                "offset": 0,
                "size": 100,
            }),
        )
        .await;
    assert_not_excluded(&resp);

    drop(client);
    server.stop().await;
}
