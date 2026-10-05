//! The macro options a daemon rebuild request carries, end to end.
//!
//! The daemon has no working directory of its client, so a relative
//! `expand_cache` must resolve against the directory being rebuilt and
//! never against the daemon's own working directory. Until this file that
//! rule was pinned only by `macro_options.rs` unit tests: anchoring
//! `RebuildParams::macro_request` (IPC) or the daemon-hosted MCP's parse of
//! `rebuild_index` to the daemon's working directory passed every
//! daemon test. Both surfaces are driven here through the real IPC server
//! with the real builder. The tests run in-process, so the daemon's working
//! directory is this test's, the `sqry-daemon` crate, which holds `tests`
//! and no `cachedir`: a request for `cachedir` (present only under the
//! root) must be accepted, and one for `tests` (present only in the working
//! directory) must be refused naming the root's `tests`.
//!
//! The file also pins the code of the empty expand cache refusal on the
//! daemon-hosted MCP (`-32602`, `validation_error`), which the existing
//! test asserts only by text, and compares the daemon-hosted MCP's wire
//! error with the standalone `rebuild_index`'s for every refusal both hosts
//! give.
//!
//! Round 7 adds the refusals whose envelopes the standalone server now
//! builds through the shared constructors in `sqry_mcp::error`: a path that
//! names no workspace, a nested index without `force`, an unusable expand
//! cache described by shape with an MCP remedy, and the schema both hosts
//! advertise. The daemon host builds each of those envelopes through the
//! same constructor, so each is compared whole.

#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use sqry_core::graph::unified::build::{BuildConfig, MacroBuildOptions};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_daemon::DaemonConfig;
use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tokio::net::UnixStream;

const LIB_RS: &str = "#[cfg(test)]\npub fn gated() {}\npub fn plain() {}\n";

/// A server whose builder is the production one, so rebuilds resolve the
/// macro options and persist them.
async fn real_builder_server() -> TestServer {
    let resolver = Arc::new(sqry_daemon::WorkspaceRosterResolver::new());
    let builder: Arc<dyn sqry_daemon::WorkspaceBuilder> = Arc::new(
        sqry_daemon::RealWorkspaceBuilder::new(Arc::clone(&resolver)),
    );
    TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver).await
}

/// An indexed workspace (fast-path roster, no macro options) at a
/// canonical root, with a `cachedir` under it.
///
/// The root carries its own project marker (an empty `.git`), so the
/// ancestor walk in `discover_workspace_root` stops at the root and the
/// project boundary is the root wherever the temp directory lives: a
/// `.git`, `Cargo.toml` or `.sqry` above `TMPDIR` cannot move it.
fn indexed_workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    std::fs::create_dir(root.join(".git")).expect("project marker");
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("src").join("lib.rs"), LIB_RS).expect("lib.rs");
    std::fs::create_dir(root.join("cachedir")).expect("cachedir");
    let plugins = sqry_plugin_registry::create_plugin_manager();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
        &root,
        &plugins,
        &BuildConfig {
            macro_options: MacroBuildOptions::default(),
            ..BuildConfig::default()
        },
        "test:r7_anchoring",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("fixture index");
    (dir, root)
}

fn recorded_expand_cache(root: &Path) -> Option<String> {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
        .and_then(|record| record.expand_cache_dir)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn precondition_the_daemon_directory_holds_tests_and_no_cachedir(root: &Path) {
    assert!(
        Path::new("tests").is_dir() && !Path::new("cachedir").exists(),
        "precondition: the daemon's working directory (this crate) holds tests and no cachedir"
    );
    assert!(
        !root.join("tests").exists() && root.join("cachedir").is_dir(),
        "precondition: the root holds cachedir and no tests"
    );
}

async fn ipc_client(server: &TestServer) -> TestIpcClient {
    let mut client = TestIpcClient::connect(&server.path).await;
    let hello = client.hello(1).await;
    assert!(hello.compatible);
    client
}

/// IPC `daemon/rebuild` anchors a relative `expand_cache` to the workspace
/// root: `cachedir` (under the root only) is accepted and recorded as the
/// root's canonical `cachedir`; `tests` (in the daemon's working directory
/// only) is refused with `-32022` naming the root's `tests`, and the record
/// is unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_rebuild_anchors_a_relative_expand_cache_to_the_workspace_root() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    precondition_the_daemon_directory_holds_tests_and_no_cachedir(&root);
    let mut client = ipc_client(&server).await;
    let loaded = client
        .request("daemon/load", json!({ "index_root": path_text(&root) }))
        .await;
    expect_success(&loaded);

    let accepted = client
        .request(
            "daemon/rebuild",
            json!({ "path": path_text(&root), "force": true, "expand_cache": "cachedir" }),
        )
        .await;
    expect_success(&accepted);
    assert_eq!(
        recorded_expand_cache(&root).as_deref(),
        Some(path_text(&root.join("cachedir")).as_str()),
        "the root's cachedir is recorded"
    );

    let refused = client
        .request(
            "daemon/rebuild",
            json!({ "path": path_text(&root), "force": true, "expand_cache": "tests" }),
        )
        .await;
    let error = expect_error(&refused);
    println!("refusal: {error:?}");
    assert_eq!(
        error.code,
        sqry_daemon::JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE
    );
    // The IPC error data is the flat `DaemonError::error_data` object.
    let data = error.data.clone().expect("error data");
    assert_eq!(
        data["expand_cache_dir"],
        path_text(&root.join("tests")),
        "the root's tests is named, not the daemon's: {data}"
    );
    assert_eq!(
        recorded_expand_cache(&root).as_deref(),
        Some(path_text(&root.join("cachedir")).as_str()),
        "the refusal wrote nothing"
    );
    server.stop().await;
}

fn call_tool_request(arguments: Value) -> rmcp::model::CallToolRequestParams {
    rmcp::model::CallToolRequestParams::new("rebuild_index")
        .with_arguments(arguments.as_object().expect("an object").clone())
}

/// Connect an rmcp client to the daemon-hosted MCP over the shim.
async fn mcp_client(server: &TestServer) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let stream = UnixStream::connect(&server.path).await.expect("connect");
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
    assert!(ack.accepted, "{:?}", ack.reason);
    rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize")
}

/// The daemon-hosted MCP's wire error for one `rebuild_index` call:
/// `(code, message, data)`.
async fn daemon_hosted_refusal(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    arguments: Value,
) -> (i32, String, Value) {
    match client.peer().call_tool(call_tool_request(arguments)).await {
        Err(rmcp::ServiceError::McpError(error)) => (
            error.code.0,
            error.message.to_string(),
            error.data.unwrap_or(Value::Null),
        ),
        other => panic!("the daemon-hosted call must be refused: {other:?}"),
    }
}

/// The daemon-hosted `rebuild_index` anchors a relative `expand_cache` to
/// the directory `path` names: `cachedir` under the root is accepted,
/// `tests` (the daemon's working directory only) is refused naming the
/// root's `tests`, and for a subdirectory `path` the subdirectory is the
/// anchor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_host_rebuild_index_anchors_a_relative_expand_cache_to_the_path() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    precondition_the_daemon_directory_holds_tests_and_no_cachedir(&root);
    let client = mcp_client(&server).await;

    let accepted = client
        .peer()
        .call_tool(call_tool_request(
            json!({ "path": path_text(&root), "force": true, "expand_cache": "cachedir" }),
        ))
        .await
        .expect("the root's cachedir is accepted");
    assert_ne!(accepted.is_error, Some(true), "{accepted:?}");

    let (code, _message, data) = daemon_hosted_refusal(
        &client,
        json!({ "path": path_text(&root), "force": true, "expand_cache": "tests" }),
    )
    .await;
    assert_eq!(code, -32602);
    assert_eq!(data["kind"], "rebuild_macro_options_unavailable");
    assert_eq!(
        data["details"]["expand_cache_dir"],
        path_text(&root.join("tests")),
        "the root's tests is named, not the daemon's: {data}"
    );

    let sub = root.join("sub");
    std::fs::create_dir_all(sub.join("subcache")).expect("subcache");
    std::fs::write(sub.join("lib.rs"), "pub fn in_sub() {}\n").expect("sub lib.rs");
    let accepted = client
        .peer()
        .call_tool(call_tool_request(
            json!({ "path": path_text(&sub), "force": true, "expand_cache": "subcache" }),
        ))
        .await
        .expect("the subdirectory's subcache is accepted");
    assert_ne!(accepted.is_error, Some(true), "{accepted:?}");
    let (_code, _message, data) = daemon_hosted_refusal(
        &client,
        json!({ "path": path_text(&sub), "force": true, "expand_cache": "cachedir" }),
    )
    .await;
    assert_eq!(
        data["details"]["expand_cache_dir"],
        path_text(&sub.join("cachedir")),
        "a subdirectory path anchors to the subdirectory: {data}"
    );

    drop(client);
    server.stop().await;
}

/// The empty expand cache is an invalid argument on both daemon surfaces:
/// `-32602` with `validation_error` on the daemon-hosted MCP, `-32602` on
/// IPC. Mapping it to a build failure (`-32603`, `workspace_not_ready`)
/// fails here, where the existing MCP-host test checks only the text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_empty_expand_cache_is_an_invalid_argument_by_code() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();

    let client = mcp_client(&server).await;
    let (code, message, data) = daemon_hosted_refusal(
        &client,
        json!({ "path": path_text(&root), "force": true, "expand_cache": "" }),
    )
    .await;
    assert_eq!(code, -32602, "{message}");
    assert_eq!(data["kind"], "validation_error", "{data}");
    assert!(
        message.contains("the expand cache directory is empty"),
        "{message}"
    );
    drop(client);

    let mut ipc = ipc_client(&server).await;
    expect_success(
        &ipc.request("daemon/load", json!({ "index_root": path_text(&root) }))
            .await,
    );
    let refused = ipc
        .request(
            "daemon/rebuild",
            json!({ "path": path_text(&root), "force": true, "expand_cache": "" }),
        )
        .await;
    assert_eq!(expect_error(&refused).code, -32602);
    server.stop().await;
}

/// The standalone `rebuild_index` arguments `arguments` expresses.
fn standalone_args(arguments: &Value) -> sqry_mcp::tool_args::RebuildIndexArgs {
    sqry_mcp::tool_args::RebuildIndexArgs {
        path: arguments["path"].as_str().expect("path").to_string(),
        force: arguments["force"].as_bool().unwrap_or(true),
        cfg_flags: arguments.get("cfg_flags").map(|flags| {
            flags
                .as_array()
                .expect("an array")
                .iter()
                .map(|flag| flag.as_str().expect("a string").to_string())
                .collect()
        }),
        expand_cache: arguments
            .get("expand_cache")
            .map(|dir| PathBuf::from(dir.as_str().expect("a string"))),
        reset_macro_options: arguments
            .get("reset_macro_options")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// The wire triple for a shared constructor's envelope.
fn wire(rpc: sqry_mcp::error::RpcError) -> (i32, String, Value) {
    let wire = sqry_mcp::error::rpc_error_to_mcp(rpc);
    (
        wire.code.0,
        wire.message.to_string(),
        wire.data.unwrap_or(Value::Null),
    )
}

/// The standalone `rebuild_index`'s wire error for the same request,
/// derived in-process: the executor's `RpcError` through the server's own
/// bridge (`sqry_mcp::error::rpc_error_to_mcp`), which the standalone
/// wire tests in `sqry-mcp/tests/rebuild_index_refusal_wire.rs` drive over
/// stdio.
fn standalone_refusal(arguments: &Value) -> (i32, String, Value) {
    use std::num::NonZeroUsize;
    sqry_mcp::test_setup::init_discovery_cache(NonZeroUsize::new(64).unwrap());
    sqry_mcp::test_setup::init_engine_cache(NonZeroUsize::new(8).unwrap());
    let args = standalone_args(arguments);
    let err = match sqry_mcp::execution::execute_rebuild_index(&args) {
        Ok(_) => panic!("the standalone call must be refused: {arguments}"),
        Err(err) => err,
    };
    let rpc = err
        .downcast::<sqry_mcp::error::RpcError>()
        .unwrap_or_else(|err| panic!("the refusal must be an RpcError, got: {err:#}"));
    let wire = sqry_mcp::error::rpc_error_to_mcp(rpc);
    (
        wire.code.0,
        wire.message.to_string(),
        wire.data.unwrap_or(Value::Null),
    )
}

/// The two MCP hosts answer each refusal they share with the same code,
/// message and data: macro arguments beside `force=false` (the shared
/// constructor's text, true on both hosts), an empty and a non-UTF-8
/// expand cache, a manifest naming an uncompiled plugin id (both legs), and
/// an unreadable manifest without `force`. An unusable expand cache is
/// compared below, where the daemon host's message still differs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_mcp_hosts_answer_each_refusal_identically() {
    use std::os::unix::ffi::OsStringExt;

    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    std::fs::write(root.join("a-file"), b"x").expect("file");
    let mut name = b"cache-".to_vec();
    name.push(0xff);
    let non_utf8 = root.join(std::ffi::OsString::from_vec(name));
    std::fs::create_dir(&non_utf8).expect("non-UTF-8 dir");
    std::os::unix::fs::symlink(&non_utf8, root.join("cache-link")).expect("symlink");
    let client = mcp_client(&server).await;
    let path = path_text(&root);

    let need_force = wire(sqry_mcp::error::RpcError::macro_options_need_force(&root));
    let mut cases = vec![
        (
            json!({ "path": path, "force": false, "reset_macro_options": true }),
            Some(&need_force),
        ),
        (
            json!({ "path": path, "force": false, "cfg_flags": ["test"] }),
            Some(&need_force),
        ),
        (
            json!({ "path": path, "force": true, "expand_cache": "" }),
            None,
        ),
        (
            json!({ "path": path, "force": true, "expand_cache": "cache-link" }),
            None,
        ),
    ];
    let mut compared = 0;
    for (arguments, constructed) in cases.drain(..) {
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        let standalone = standalone_refusal(&arguments);
        println!("{arguments}\n  daemon:     {daemon:?}\n  standalone: {standalone:?}");
        assert_eq!(standalone, daemon, "{arguments}");
        if let Some(constructed) = constructed {
            assert_eq!(
                &standalone, constructed,
                "the shared constructor: {arguments}"
            );
        }
        compared += 1;
    }

    let storage = GraphStorage::new(&root);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("ids")
        .push(json!("r7-uncompiled-plugin"));
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("plant the id");
    for force in [false, true] {
        let arguments = json!({ "path": path, "force": force });
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        let standalone = standalone_refusal(&arguments);
        println!("{arguments}\n  daemon:     {daemon:?}\n  standalone: {standalone:?}");
        assert_eq!(daemon.2["kind"], "workspace_incompatible_graph");
        assert_eq!(standalone, daemon, "{arguments}");
        compared += 1;
    }

    std::fs::write(storage.manifest_path(), b"{").expect("unreadable manifest");
    let arguments = json!({ "path": path, "force": false });
    let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
    let standalone = standalone_refusal(&arguments);
    println!("{arguments}\n  daemon:     {daemon:?}\n  standalone: {standalone:?}");
    assert_eq!(daemon.2["kind"], "workspace_not_ready");
    assert_eq!(standalone, daemon, "{arguments}");
    compared += 1;

    assert_eq!(compared, 7, "every case was compared");
    drop(client);
    server.stop().await;
}

/// The arguments for each unusable expand cache both hosts refuse: a
/// missing directory, a file and a dangling link the request names.
fn unusable_cache_cases(root: &Path) -> Vec<Value> {
    std::fs::write(root.join("a-file"), b"x").expect("file");
    std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).expect("symlink");
    let path = path_text(root);
    ["missing-cache", "a-file", "dangling"]
        .into_iter()
        .map(|name| json!({ "path": path, "force": true, "expand_cache": name }))
        .collect()
}

/// An unusable expand cache: both hosts refuse it with the same code,
/// `kind`, retryability, root and anchored directory (the parts the daemon
/// host already shares), and the standalone envelope is exactly the shared
/// constructor's (S10, round 7: described by shape, an MCP remedy for the
/// requested directory). The full identity of the two envelopes is the
/// test below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_mcp_hosts_refuse_an_unusable_expand_cache_alike() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    let client = mcp_client(&server).await;
    let cases = unusable_cache_cases(&root);
    for arguments in &cases {
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        let standalone = standalone_refusal(arguments);
        println!("{arguments}\n  daemon:     {daemon:?}\n  standalone: {standalone:?}");
        assert_eq!(standalone.0, daemon.0, "code: {arguments}");
        for key in ["kind", "retryable", "retry_after_ms"] {
            assert_eq!(standalone.2[key], daemon.2[key], "{key}: {arguments}");
        }
        for key in ["root", "expand_cache_dir"] {
            assert_eq!(
                standalone.2["details"][key], daemon.2["details"][key],
                "details.{key}: {arguments}"
            );
        }
        let dir = root.join(arguments["expand_cache"].as_str().expect("a name"));
        assert_eq!(
            standalone,
            wire(
                sqry_mcp::error::RpcError::rebuild_macro_options_unavailable(
                    &root,
                    &dir,
                    sqry_mcp::error::ExpandCacheOrigin::Requested,
                )
            ),
            "{arguments}"
        );
    }
    assert_eq!(cases.len(), 3, "every case was compared");
    drop(client);
    server.stop().await;
}

/// The full identity for an unusable expand cache, requested and recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_mcp_hosts_answer_an_unusable_expand_cache_identically() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    let client = mcp_client(&server).await;
    let mut cases = unusable_cache_cases(&root);
    let storage = GraphStorage::new(&root);
    let mut compared = 0;
    for arguments in cases.drain(..) {
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        assert_eq!(standalone_refusal(&arguments), daemon, "{arguments}");
        compared += 1;
    }
    for recorded in ["missing-cache", ""] {
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
                .expect("json");
        manifest["macro_options"] = json!({ "cfg_flags": [], "expand_cache_dir": recorded });
        std::fs::write(
            storage.manifest_path(),
            serde_json::to_vec_pretty(&manifest).expect("json"),
        )
        .expect("hand edit");
        let arguments = json!({ "path": path_text(&root), "force": true });
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        assert_eq!(
            standalone_refusal(&arguments),
            daemon,
            "recorded {recorded:?}"
        );
        compared += 1;
    }
    assert_eq!(compared, 5, "every case was compared");
    drop(client);
    server.stop().await;
}

/// Daemon-only (F3): a workspace `daemon/load` holds in memory with no index
/// on disk refuses macro arguments beside `force=false` with exactly the
/// shared constructor's envelope, whose text is true for that graph too;
/// the call without them is the accepted control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_host_refuses_macro_arguments_over_a_resident_graph_with_the_shared_text() {
    let server = real_builder_server().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("src").join("lib.rs"), LIB_RS).expect("lib.rs");
    let mut ipc = ipc_client(&server).await;
    expect_success(
        &ipc.request("daemon/load", json!({ "index_root": path_text(&root) }))
            .await,
    );
    assert!(
        !GraphStorage::new(&root).exists(),
        "precondition: the load wrote no index"
    );
    let client = mcp_client(&server).await;
    let refused = daemon_hosted_refusal(
        &client,
        json!({ "path": path_text(&root), "force": false, "cfg_flags": ["test"] }),
    )
    .await;
    assert_eq!(
        refused,
        wire(sqry_mcp::error::RpcError::macro_options_need_force(&root))
    );
    let accepted = client
        .peer()
        .call_tool(call_tool_request(
            json!({ "path": path_text(&root), "force": false }),
        ))
        .await
        .expect("the resident graph is reported");
    assert_ne!(accepted.is_error, Some(true), "{accepted:?}");
    assert!(!GraphStorage::new(&root).exists(), "nothing was written");
    drop(client);
    server.stop().await;
}

/// S5 (round 7): a `path` that names no workspace is an invalid argument on
/// both hosts, `-32602` with `validation_error`, naming the path: an
/// absolute path that does not exist, and a relative one (the daemon host
/// refuses every relative path, having no client working directory, issue
/// #566; the standalone server resolves a relative path, and refuses one
/// that resolves nowhere). It was `-32600` with no data on the standalone
/// server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_mcp_hosts_refuse_a_path_naming_no_workspace_as_an_invalid_argument() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    let client = mcp_client(&server).await;
    let missing = path_text(&root.join("nope"));
    for (arguments, named) in [
        (json!({ "path": missing, "force": true }), missing.clone()),
        (
            json!({ "path": "r7-no-such-dir/deeper", "force": true }),
            "r7-no-such-dir".to_string(),
        ),
    ] {
        let daemon = daemon_hosted_refusal(&client, arguments.clone()).await;
        let standalone = standalone_refusal(&arguments);
        println!("{arguments}\n  daemon:     {daemon:?}\n  standalone: {standalone:?}");
        for (host, (code, message, data)) in [("daemon", &daemon), ("standalone", &standalone)] {
            assert_eq!(*code, -32602, "{host}: {arguments}");
            assert_eq!(data["kind"], "validation_error", "{host}: {arguments}");
            assert!(message.contains(&named), "{host} names the path: {message}");
            assert_eq!(
                format!(
                    "invalid argument: {}",
                    data["details"]["reason"].as_str().unwrap()
                ),
                *message,
                "{host}"
            );
        }
    }
    drop(client);
    server.stop().await;
}

/// S5 (round 7), the standalone half: `force=false` on a subdirectory of
/// an indexed project is refused with the shared constructor's envelope
/// (cluster E, design E.3: `force` is the MCP opt-in to a nested index),
/// and nothing is built; `force=true` builds (the accepted side).
#[test]
fn the_standalone_host_refuses_a_nested_index_without_force() {
    let (_dir, root) = indexed_workspace();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).expect("sub");
    std::fs::write(sub.join("m.rs"), "pub fn in_sub() {}\n").expect("m.rs");
    let refused = standalone_refusal(&json!({ "path": path_text(&sub), "force": false }));
    assert_eq!(
        refused,
        wire(sqry_mcp::error::RpcError::nested_index_refused(
            &sub,
            GraphStorage::new(&root).graph_dir(),
            &root,
        ))
    );
    assert!(!GraphStorage::new(&sub).exists(), "nothing was built");
    sqry_mcp::execution::execute_rebuild_index(&standalone_args(
        &json!({ "path": path_text(&sub), "force": true }),
    ))
    .expect("force builds the nested index");
    assert!(GraphStorage::new(&sub).exists());
}

/// S5 (round 7), the daemon half of the nested-index refusal: the same
/// envelope, nothing built or persisted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_host_refuses_a_nested_index_without_force() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).expect("sub");
    std::fs::write(sub.join("m.rs"), "pub fn in_sub() {}\n").expect("m.rs");
    let client = mcp_client(&server).await;
    let refused =
        daemon_hosted_refusal(&client, json!({ "path": path_text(&sub), "force": false })).await;
    assert_eq!(
        refused,
        wire(sqry_mcp::error::RpcError::nested_index_refused(
            &sub,
            GraphStorage::new(&root).graph_dir(),
            &root,
        ))
    );
    assert!(!GraphStorage::new(&sub).exists(), "nothing was built");
    drop(client);
    server.stop().await;
}

/// S6 (round 7): both hosts advertise one `rebuild_index` input schema,
/// generated from the shared `RebuildIndexParams`: it refuses unknown
/// fields and states that an empty, blank or padded cfg flag is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_mcp_hosts_advertise_the_shared_rebuild_index_schema() {
    let server = real_builder_server().await;
    let client = mcp_client(&server).await;
    let daemon = client
        .peer()
        .list_all_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .find(|tool| tool.name == "rebuild_index")
        .expect("the daemon host advertises rebuild_index");
    let standalone = sqry_mcp::tools_schema::daemon_supported_tools()
        .into_iter()
        .find(|tool| tool.name == "rebuild_index")
        .expect("the standalone server advertises rebuild_index");
    assert_eq!(daemon.input_schema, standalone.input_schema);
    let generated = Value::Object(
        (*rmcp::handler::server::common::schema_for_type::<
            sqry_mcp::daemon_params::RebuildIndexParams,
        >())
        .clone(),
    );
    let advertised = Value::Object((*daemon.input_schema).clone());
    assert_eq!(advertised, generated, "the schema is the shared type's");
    assert_eq!(advertised["additionalProperties"], json!(false));
    let cfg = advertised["properties"]["cfg_flags"]["description"]
        .as_str()
        .expect("a description");
    assert!(
        cfg.contains("An empty or blank flag") && cfg.contains("leading or trailing whitespace"),
        "{cfg}"
    );
    drop(client);
    server.stop().await;
}

/// S6 (round 7): the daemon host refuses what the schema it advertises
/// refuses, as the standalone server does: an unknown field, an empty, a
/// blank and a padded cfg flag, each `-32602` with nothing built.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_host_enforces_the_rebuild_index_schema_it_advertises() {
    let server = real_builder_server().await;
    let (_dir, root) = indexed_workspace();
    let client = mcp_client(&server).await;
    let before = std::fs::read(GraphStorage::new(&root).snapshot_path()).expect("snapshot");
    let path = path_text(&root);
    for arguments in [
        json!({ "path": path, "force": true, "bogus": 1 }),
        json!({ "path": path, "force": true, "cfg_flags": [""] }),
        json!({ "path": path, "force": true, "cfg_flags": ["  "] }),
        json!({ "path": path, "force": true, "cfg_flags": [" test"] }),
    ] {
        let (code, message, _data) = daemon_hosted_refusal(&client, arguments.clone()).await;
        assert_eq!(code, -32602, "{arguments}: {message}");
    }
    assert_eq!(
        std::fs::read(GraphStorage::new(&root).snapshot_path()).expect("snapshot"),
        before,
        "nothing was built"
    );
    drop(client);
    server.stop().await;
}
