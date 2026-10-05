//! The LSP's rebuild refusals as a client receives them.
//!
//! Each test drives the in-process `LspService` with JSON-RPC requests and
//! answers the server's own requests on the client socket, so what is
//! asserted is the response a client gets (`error.code`, `error.message`,
//! `error.data`) and the work-done progress line it is shown. The
//! function-level test T14L in `shared_graph_acquisition_parity.rs` renders
//! the handler's error with `{:#}`, a format no client receives: before this
//! file `sqry.index` answered every refusal with `InternalError` and the
//! outer context "Failed to build and persist unified graph", and the
//! progress line and the self-heal arm showed only their own outer
//! contexts.
//!
//! A refusal is LSP `RequestFailed` (`-32803`: a valid request that
//! failed), with the refusal's own text and `data.kind`.
//!
//! Round 7 adds every handler that acquires a graph (S4): each answers a
//! refused graph with the same `RequestFailed` and `data.kind`, where some
//! answered `InternalError` with no data, `[]`, or zero counts; and the
//! self-heal reports the refusal's own kind. Every assertion on a
//! notification (`$/progress`, `telemetry/event`) waits for that
//! notification with a bound: the server sends it before the response, but
//! the recorder reads the client socket on its own task (S2).
//!
//! Round 8 adds the refusals a request answers around: `workspace/symbol`
//! over several workspace folders leaves a refused folder out, answers from
//! the others and reports the folder (a `?` had failed the whole request),
//! and the document-level handlers that fall back to the file's own text over
//! a refused graph now show the refusal once per refusal state.
//!
//! Round 9 makes the notice's remedies work for every workspace folder
//! (`sqry.index` accepted only the first, and could not drop a recorded
//! macro option), makes its text say what each request does, and pins the
//! every-folder refusal's `data`, the empty partial answer and every
//! handler that sends a queued notice.
//!
//! Decision D-i8-60 makes the file hermetic: every fixture sits under
//! [`project_tempdir`], whose `.git` stops every ancestor walk, and every
//! workspace folder carries its own `.git` ([`folder_repository`]), so
//! gitRoot resolves it to itself. Before, a folder with no `.git` of its
//! own under a `TMPDIR` with a repository above it resolved to that
//! repository, and the LSP built that repository's index (gitRoot's
//! contract, unchanged). It also pins that the self-heal rewrites only the
//! index at the index root it was given.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsRequest};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_lsp::LspOptions;
use sqry_lsp::session::SessionManager;
use sqry_plugin_registry::{PluginSelectionConfig, UnreadableManifestPolicy};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::{Request, Response};

const REQUEST_FAILED: i64 = -32803;

/// How long a test waits for a notification the server has already sent
/// before it fails. Generous: the wait ends as soon as the recorder has
/// read the message, and only a server that never sends it waits this long.
const NOTIFICATION_WAIT: Duration = Duration::from_secs(60);

fn options_for(root: &Path) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    }
}

/// A temp directory that is its own project: an empty `.git` marker at its
/// root stops every ancestor walk there (the provider's index discovery and
/// the LSP's gitRoot resolution, which takes the nearest `.git`), so
/// nothing above `TMPDIR` (an index, a project marker, a repository) can
/// reach a fixture, and no fixture can write above it. The same idea as
/// `project_tempdir` in `shared_graph_acquisition_parity.rs` (commit
/// `dfdb87944`). A workspace folder under it carries its own `.git`
/// ([`folder_repository`]), or gitRoot would make the temp root every
/// folder's project.
fn project_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join(".git")).expect("project marker");
    dir
}

/// Make the workspace folder `root` a git repository of its own, so
/// gitRoot resolves it to itself (its own project, as it was in a
/// `TMPDIR` with no repository above it) rather than to the temp root's
/// `.git` ([`project_tempdir`]).
fn folder_repository(root: &Path) {
    fs::create_dir_all(root.join(".git")).expect("the folder's own repository");
}

/// A Rust workspace indexed with an expand cache directory recorded, the
/// directory then removed: every rebuild must refuse it.
fn workspace_with_a_missing_recorded_cache() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"r7\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    let cache = root.join("expand-cache");
    fs::create_dir(&cache).expect("cache dir");
    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        &root,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::Refuse,
        "test:r7_lsp_wire",
        &BuildConfig::default(),
        &MacroOptionsRequest::from_flags(&["test".to_string()], Some(&cache), false),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("the fixture index builds");
    fs::remove_dir_all(&cache).expect("remove the cache");
    (tmp, root, cache)
}

/// The server and a client loop answering the server's requests (a work
/// done progress token) and recording every message it sends.
struct Wire {
    service: tower_lsp::LspService<sqry_lsp::SqryLanguageServer>,
    from_server: Arc<Mutex<Vec<Value>>>,
    _session: SessionManager,
}

impl Wire {
    async fn start(root: &Path) -> Self {
        Self::start_with(
            root,
            json!({
                "processId": null,
                "rootUri": format!("file://{}", root.display()),
                "capabilities": {}
            }),
        )
        .await
    }

    /// A session over several workspace folders: `initialize` names each
    /// one, so every graph is a workspace-folder project's.
    async fn start_with_folders(folders: &[&Path]) -> Self {
        let workspace_folders: Vec<Value> = folders
            .iter()
            .map(|folder| {
                json!({
                    "uri": format!("file://{}", folder.display()),
                    "name": folder.file_name().map(|name| name.to_string_lossy().into_owned()),
                })
            })
            .collect();
        Self::start_with(
            folders[0],
            json!({
                "processId": null,
                "rootUri": format!("file://{}", folders[0].display()),
                "workspaceFolders": workspace_folders,
                "capabilities": { "workspace": { "workspaceFolders": true } }
            }),
        )
        .await
    }

    async fn start_with(root: &Path, initialize: Value) -> Self {
        let session = SessionManager::new(options_for(root));
        let (service, socket) = sqry_lsp::build_test_service_with_client(&session);
        let from_server = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&from_server);
        let (mut requests, mut to_server) = socket.split();
        tokio::spawn(async move {
            while let Some(request) = requests.next().await {
                record
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(&request).expect("request json"));
                if let Some(id) = request.id() {
                    let _ = to_server
                        .send(Response::from_ok(id.clone(), Value::Null))
                        .await;
                }
            }
        });
        let mut wire = Self {
            service,
            from_server,
            _session: session,
        };
        let initialized = wire
            .call(
                Request::build("initialize")
                    .params(initialize)
                    .id(0i64)
                    .finish(),
            )
            .await;
        assert!(initialized.get("result").is_some(), "{initialized}");
        wire
    }

    async fn call(&mut self, request: Request) -> Value {
        let response = self
            .service
            .ready()
            .await
            .expect("service ready")
            .call(request)
            .await
            .expect("service call")
            .expect("a response");
        let value = serde_json::to_value(&response).expect("response json");
        println!("response: {value}");
        value
    }

    /// `workspace/executeCommand sqry.index <root> <force>`.
    async fn index(&mut self, root: &Path, force: bool) -> Value {
        self.call(
            Request::build("workspace/executeCommand")
                .params(json!({
                    "command": "sqry.index",
                    "arguments": [root.display().to_string(), force]
                }))
                .id(1i64)
                .finish(),
        )
        .await
    }

    /// `workspace/executeCommand sqry.index` with `arguments` as given.
    async fn index_with(&mut self, arguments: Value, id: i64) -> Value {
        self.call(
            Request::build("workspace/executeCommand")
                .params(json!({ "command": "sqry.index", "arguments": arguments }))
                .id(id)
                .finish(),
        )
        .await
    }

    /// A notification-only method such as `initialized` or
    /// `workspace/didChangeWorkspaceFolders`: there is no response.
    async fn notify(&mut self, method: &'static str, params: Value) {
        let response = self
            .service
            .ready()
            .await
            .expect("service ready")
            .call(Request::build(method).params(params).finish())
            .await
            .expect("service call");
        assert!(response.is_none(), "a notification has no response");
    }

    /// The first message the server sent that `matches`, waiting for the
    /// recorder to read it. The server sends a `$/progress` end or a
    /// telemetry event before it answers the request that caused it, so it
    /// is already on the socket when the response arrives; the recorder
    /// task may not have read it yet, which is why this waits instead of
    /// reading once. Fails, naming what was recorded, after
    /// [`NOTIFICATION_WAIT`].
    async fn wait_for(&self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + NOTIFICATION_WAIT;
        loop {
            if let Some(found) = self
                .from_server
                .lock()
                .unwrap()
                .iter()
                .find(|message| matches(message))
            {
                return found.clone();
            }
            assert!(
                Instant::now() < deadline,
                "{what} never arrived within {NOTIFICATION_WAIT:?}; recorded: {:?}",
                self.from_server.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// How many messages the server has sent so far that `matches`. Only
    /// meaningful after a fence: a later message the test waited for
    /// ([`Self::wait_for`]), which the server sent after every message
    /// counted here.
    fn count(&self, matches: impl Fn(&Value) -> bool) -> usize {
        self.from_server
            .lock()
            .unwrap()
            .iter()
            .filter(|message| matches(message))
            .count()
    }

    /// Wait until the server has sent at least `n` messages that `matches`
    /// ([`Self::wait_for`]'s bound and failure).
    async fn wait_for_count(&self, what: &str, n: usize, matches: impl Fn(&Value) -> bool) {
        let deadline = Instant::now() + NOTIFICATION_WAIT;
        while self.count(&matches) < n {
            assert!(
                Instant::now() < deadline,
                "{n} of {what} never arrived within {NOTIFICATION_WAIT:?}; recorded: {:?}",
                self.from_server.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// `workspace/symbol` for `query`.
    async fn workspace_symbol(&mut self, query: &str, id: i64) -> Value {
        self.call(
            Request::build("workspace/symbol")
                .params(json!({ "query": query }))
                .id(id)
                .finish(),
        )
        .await
    }

    /// The `message` of the first work-done progress `end` that `matches`,
    /// waiting for it ([`Self::wait_for`]).
    async fn progress_end(&self, matches: impl Fn(&str) -> bool) -> String {
        let end = self
            .wait_for("a $/progress end", |message| {
                message["method"] == "$/progress"
                    && message["params"]["value"]["kind"] == "end"
                    && message["params"]["value"]["message"]
                        .as_str()
                        .is_some_and(&matches)
            })
            .await;
        end["params"]["value"]["message"]
            .as_str()
            .expect("an end message")
            .to_owned()
    }
}

fn index_bytes(root: &Path) -> (Vec<u8>, Vec<u8>) {
    let storage = GraphStorage::new(root);
    (
        fs::read(storage.manifest_path()).expect("manifest bytes"),
        fs::read(storage.snapshot_path()).expect("snapshot bytes"),
    )
}

/// `sqry.index` with `force` over a recorded expand cache that is gone:
/// the response is `RequestFailed` naming the root and the directory with
/// the resolver's message and `data.kind`, plus the `sqry.index` arguments
/// that drop the record, the progress line carries the same refusal, and
/// nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_answers_a_missing_recorded_expand_cache_with_request_failed() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    let before = index_bytes(&root);
    let mut wire = Wire::start(&root).await;

    let response = wire.index(&root, true).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with(&format!("rebuild of {} refused: ", root.display()))
            && message.contains(&format!(
                "expand cache directory {} does not exist or is not a directory",
                cache.display()
            ))
            && message.contains("reset_macro_options"),
        "the refusal itself, not an outer context: {message}"
    );
    assert!(
        !message.contains("Failed to build and persist unified graph"),
        "{message}"
    );
    // Round 9: the record is the cause, so the refusal also names the
    // `sqry.index` arguments that drop it, in its text and in `data`.
    let reset_arguments = json!([root.display().to_string(), true, true]);
    assert_eq!(
        error["data"],
        json!({
            "kind": "rebuild_macro_options_unavailable",
            "root": root.display().to_string(),
            "resetArguments": reset_arguments,
        })
    );
    assert!(
        message.ends_with(&format!(
            "; from the editor, the sqry.index command with the arguments {reset_arguments} \
             drops the recorded macro options and rebuilds"
        )),
        "{message}"
    );
    let end = wire
        .progress_end(|end| end.starts_with("✗ Index build failed: "))
        .await;
    assert!(
        end.contains(message),
        "the progress line shows the refusal: {end}"
    );
    assert_eq!(index_bytes(&root), before, "nothing was written");
}

/// `sqry.index` over a manifest naming a plugin id this binary did not
/// compile is `RequestFailed` naming the id, with or without `force`'s
/// build: the roster refusal is not hidden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_answers_an_uncompiled_plugin_id_with_request_failed() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let storage = GraphStorage::new(&root);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(json!("r7-uncompiled-plugin"));
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("plant the id");
    let before = index_bytes(&root);
    let mut wire = Wire::start(&root).await;

    let response = wire.index(&root, true).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains("r7-uncompiled-plugin")
            && message.contains(&storage.manifest_path().display().to_string()),
        "the refusal names the id and the manifest: {message}"
    );
    assert_eq!(error["data"]["kind"], "workspace_incompatible_graph");
    assert_eq!(index_bytes(&root), before, "nothing was written");
}

/// The control: with the recorded directory present, `sqry.index` with
/// `force` rebuilds and reports success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_rebuilds_when_the_recorded_cache_is_present() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let mut wire = Wire::start(&root).await;
    let response = wire.index(&root, true).await;
    assert!(response.get("error").is_none(), "{response}");
    let end = wire.progress_end(|end| end.starts_with("✓ Indexed ")).await;
    assert!(end.contains("symbols"), "{end}");
}

/// The self-heal arm: a corrupt snapshot makes a read rebuild in place, and
/// that rebuild refuses the missing recorded cache. The read answers
/// `RequestFailed` whose message carries the refusal through every context
/// (the index-status context, the self-heal's, the registry's).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_self_heal_refusal_reaches_the_client_whole() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    let storage = GraphStorage::new(&root);
    fs::write(storage.snapshot_path(), b"not a snapshot").expect("corrupt the snapshot");
    let mut wire = Wire::start(&root).await;

    let response = wire
        .call(
            Request::build("sqry/indexStatus")
                .params(json!({ "path": root.display().to_string() }))
                .id(2i64)
                .finish(),
        )
        .await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains("Graph auto-rebuild failed for")
            && message.contains(&format!(
                "expand cache directory {} does not exist or is not a directory",
                cache.display()
            )),
        "the self-heal's refusal is shown whole: {message}"
    );
    assert_eq!(
        error["data"]["kind"], "rebuild_macro_options_unavailable",
        "the refusal's own kind, not workspace_not_ready (S4, round 7)"
    );
}

/// `sqry/semanticDiff` resolves the roster before any worktree exists and
/// refuses a manifest naming an uncompiled plugin id: `RequestFailed` with
/// the registry's text and `data.kind`, where it was `InternalError`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_diff_answers_a_roster_refusal_with_request_failed() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let storage = GraphStorage::new(&root);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(json!("r7-uncompiled-plugin"));
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("plant the id");
    let mut wire = Wire::start(&root).await;
    let response = wire
        .call(
            Request::build("sqry/semanticDiff")
                .params(json!({
                    "base": { "ref": "HEAD~1" },
                    "target": { "ref": "HEAD" },
                    "path": root.display().to_string(),
                }))
                .id(3i64)
                .finish(),
        )
        .await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with("Failed to resolve the workspace plugin roster: ")
            && message.contains("r7-uncompiled-plugin"),
        "{message}"
    );
    assert_eq!(error["data"]["kind"], "workspace_incompatible_graph");
}

/// A build that fails (not a refusal) stays `InternalError`, and both the
/// response and the progress line carry the failure's cause through every
/// context: the graph directory is made read-only so the persistence step
/// fails with the operating system's reason.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_build_shows_its_cause_in_the_response_and_the_progress_line() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    fs::set_permissions(&graph_dir, fs::Permissions::from_mode(0o555)).expect("read-only");
    let mut wire = Wire::start(&root).await;
    let response = wire.index(&root, true).await;
    fs::set_permissions(&graph_dir, fs::Permissions::from_mode(0o755)).expect("restore");

    let error = &response["error"];
    assert_eq!(error["code"], -32603, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with(&format!("rebuild of {} failed: ", root.display()))
            && message.contains("Permission denied"),
        "the cause is shown: {message}"
    );
    let end = wire
        .progress_end(|end| end.starts_with("✗ Index build failed: "))
        .await;
    assert!(
        end.contains("Permission denied"),
        "the progress line shows the cause: {end}"
    );
}

/// Plant `id`, a plugin id this binary did not compile, in the manifest at
/// `root`.
fn plant_uncompiled_id(root: &Path, id: &str) {
    let storage = GraphStorage::new(root);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(json!(id));
    fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("plant the id");
}

/// Every LSP method whose handler acquires a graph (`graph_for_path`), with
/// parameters that reach that acquisition over `root`: the custom methods,
/// call hierarchy in both directions, `workspace/symbol`, `codeLens`, pull
/// diagnostics and `sqry/indexStatus`.
fn graph_methods(root: &Path) -> Vec<(&'static str, Value)> {
    let path = root.display().to_string();
    let lib = root.join("src").join("lib.rs");
    let uri = format!("file://{}", lib.display());
    let item = json!({
        "name": "plain",
        "kind": 12,
        "uri": uri,
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 20 } },
        "selectionRange": { "start": { "line": 0, "character": 7 }, "end": { "line": 0, "character": 12 } },
        "data": {
            "state": "saved",
            "file_path": lib.display().to_string(),
            "qualified_name": "plain",
            "language": "rust",
            "start_line": 1,
            "start_column": 0,
        },
    });
    vec![
        ("sqry/search", json!({ "query": "plain", "path": path })),
        (
            "sqry/references",
            json!({ "relation": "callers", "target": "plain", "path": path }),
        ),
        ("sqry/listFiles", json!({ "path": path })),
        ("sqry/listSymbols", json!({ "path": path })),
        (
            "sqry/listFilesByLanguage",
            json!({ "language": "rust", "path": path }),
        ),
        ("sqry/listCrossLanguageRelations", json!({ "path": path })),
        ("sqry/listDuplicateGroups", json!({ "path": path })),
        ("sqry/listCircularDependencies", json!({ "path": path })),
        ("sqry/listUnusedSymbols", json!({ "path": path })),
        (
            "sqry/hierarchicalSearch",
            json!({ "query": "plain", "path": path }),
        ),
        (
            "sqry/directCallers",
            json!({ "symbol": "plain", "path": path }),
        ),
        (
            "sqry/directCallees",
            json!({ "symbol": "plain", "path": path }),
        ),
        (
            "sqry/batchCallerCalleeCount",
            json!({ "symbols": [{ "name": "plain" }], "path": path }),
        ),
        ("callHierarchy/incomingCalls", json!({ "item": item })),
        ("callHierarchy/outgoingCalls", json!({ "item": item })),
        ("workspace/symbol", json!({ "query": "plain" })),
        (
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        ),
        ("sqry/indexStatus", json!({ "path": path })),
    ]
}

/// S4 (round 7): every handler that acquires a graph answers a refused one
/// (the manifest names a plugin id this binary did not compile) with LSP
/// `RequestFailed`, `data.kind` `workspace_incompatible_graph` and the
/// resolver's words naming the id and the manifest. Before, call hierarchy
/// answered `InternalError` with no data, `workspace/symbol` `[]`, the batch
/// counts zero, and the rest an `InternalError` with the status's debug
/// rendering. The other side: over the same index without the planted id,
/// every method answers without an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_graph_handler_answers_a_refused_graph_with_request_failed() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let methods = graph_methods(&root);

    let mut accepted = Wire::start(&root).await;
    for (id, (method, params)) in (10i64..).zip(&methods) {
        let response = accepted
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "the control: {method} answers over a good index: {response}"
        );
    }
    drop(accepted);

    plant_uncompiled_id(&root, "r7-uncompiled-plugin");
    let manifest = GraphStorage::new(&root)
        .manifest_path()
        .display()
        .to_string();
    let mut refused = Wire::start(&root).await;
    let mut answered = 0;
    for (id, (method, params)) in (10i64..).zip(&methods) {
        let response = refused
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        let error = &response["error"];
        assert_eq!(error["code"], REQUEST_FAILED, "{method}: {response}");
        assert_eq!(
            error["data"]["kind"], "workspace_incompatible_graph",
            "{method}: {response}"
        );
        assert_eq!(
            error["data"]["root"],
            root.display().to_string(),
            "{method}"
        );
        let message = error["message"].as_str().expect("a message");
        assert!(
            message.contains("unknown plugin ids: r7-uncompiled-plugin")
                && message.contains(&manifest)
                && !message.contains("IncompatibleUnknownPluginIds"),
            "{method}: the resolver's words, not a debug rendering: {message}"
        );
        answered += 1;
    }
    assert_eq!(answered, methods.len());
    assert_eq!(answered, 19, "every graph method was driven");
}

/// S4 (round 7): `sqry.index` answers a refused manifest the same way with
/// and without `force`: an uncompiled plugin id is `RequestFailed`,
/// `workspace_incompatible_graph`, with the same message and data on both
/// legs (without `force` it was `InternalError` with no data and a debug
/// rendering). An unreadable manifest without `force` is refused by file,
/// `workspace_not_ready`, and nothing is written; with `force` the rebuild
/// falls back and succeeds (surface parity W1, design D9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_refuses_a_manifest_alike_with_and_without_force() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    plant_uncompiled_id(&root, "r7-uncompiled-plugin");
    let before = index_bytes(&root);
    let mut wire = Wire::start(&root).await;
    let without = wire.index(&root, false).await;
    let with = wire.index(&root, true).await;
    assert_eq!(without["error"]["code"], REQUEST_FAILED, "{without}");
    assert_eq!(
        without["error"]["data"]["kind"],
        "workspace_incompatible_graph"
    );
    assert_eq!(without["error"], with["error"], "both legs answer alike");
    assert_eq!(index_bytes(&root), before, "nothing was written");
    drop(wire);

    let storage = GraphStorage::new(&root);
    fs::write(storage.manifest_path(), b"{").expect("unreadable manifest");
    let mut wire = Wire::start(&root).await;
    let response = wire.index(&root, false).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains(&storage.manifest_path().display().to_string()),
        "the refusal names the manifest: {message}"
    );
    assert_eq!(
        fs::read(storage.manifest_path()).expect("manifest"),
        b"{",
        "nothing was written"
    );
    let response = wire.index(&root, true).await;
    assert!(response.get("error").is_none(), "{response}");
}

/// S4 (round 7), the self-heal over a missing snapshot: an index whose
/// snapshot is gone and whose manifest records an expand cache that is
/// gone. A read cannot load the graph, the self-heal rebuilds and is
/// refused, and the client sees the refusal whole with the refusal's own
/// kind. (The provider finds the manifest, so this is the load-failure arm;
/// the auto-build hook arm, which runs only where no index exists and so
/// has no record to refuse, is driven by the read-only root below.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_self_heal_over_a_missing_snapshot_reports_its_refusal_whole_with_its_kind() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::remove_file(GraphStorage::new(&root).snapshot_path()).expect("remove the snapshot");
    let mut wire = Wire::start(&root).await;
    let response = wire
        .call(
            Request::build("sqry/listFiles")
                .params(json!({ "path": root.display().to_string() }))
                .id(2i64)
                .finish(),
        )
        .await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(
        error["data"]["kind"], "rebuild_macro_options_unavailable",
        "{response}"
    );
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains(&format!(
            "expand cache directory {} does not exist or is not a directory",
            cache.display()
        )),
        "{message}"
    );
}

/// S4 and S9 (round 7), the other side of the kind and the auto-build hook
/// arm: a root with no index makes a read run the hook, whose build fails
/// (not a refusal), so the answer is `workspace_not_ready`, and the cause
/// reaches the client through every context (the hook's reason renders the
/// whole chain; its outer context alone would say only "build and persist
/// failed"). The root is read-only, so creating the index fails with the
/// operating system's reason.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_self_heal_build_is_workspace_not_ready_with_its_cause() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("read-only");
    let mut wire = Wire::start(&root).await;
    let response = wire
        .call(
            Request::build("sqry/listFiles")
                .params(json!({ "path": root.display().to_string() }))
                .id(2i64)
                .finish(),
        )
        .await;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore");
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(message.contains("Permission denied"), "{message}");
}

/// S9 (round 7): an expand cache the manifest records through a UTF-8 link
/// into a directory whose name is not valid UTF-8 cannot be recorded again
/// (design W4-D11): `sqry.index` refuses it as `validation_error`, the kind
/// the MCP hosts give it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_answers_an_unrecordable_expand_cache_as_a_validation_error() {
    use std::os::unix::ffi::OsStringExt;

    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    let mut name = b"cache-".to_vec();
    name.push(0xff);
    let target = root.join(std::ffi::OsString::from_vec(name));
    fs::create_dir(&target).expect("non-UTF-8 dir");
    std::os::unix::fs::symlink(&target, &cache).expect("link at the recorded path");
    let before = index_bytes(&root);
    let mut wire = Wire::start(&root).await;
    let response = wire.index(&root, true).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(error["data"]["kind"], "validation_error", "{response}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("is not valid UTF-8")),
        "{response}"
    );
    assert_eq!(index_bytes(&root), before, "nothing was written");
}

/// S9 (round 7): the auto-index that runs after `initialized` for a root
/// with no index shows a failure's whole cause on its progress line. The
/// root is read-only, so the build fails under the context "rebuild of
/// <root> failed", which alone would hide "Permission denied".
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auto_index_progress_line_shows_the_whole_cause() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = project_tempdir();
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("read-only");
    let mut wire = Wire::start(&root).await;
    wire.notify("initialized", json!({})).await;
    let end = wire
        .progress_end(|end| end.starts_with("Auto-index failed: "))
        .await;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore");
    assert!(
        end.starts_with(&format!(
            "Auto-index failed: rebuild of {} failed: ",
            root.display()
        )) && end.contains("Permission denied"),
        "{end}"
    );
}

/// S9 (round 7): the telemetry events `sqry/search` and `sqry/references`
/// send for a failed request carry the whole chain: the query context and
/// the parser's reason below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_and_relation_telemetry_carry_the_whole_error_chain() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let path = root.display().to_string();
    let mut wire = Wire::start(&root).await;
    for (id, method, params, event, context) in [
        (
            2i64,
            "sqry/search",
            json!({ "query": "kind:(", "path": path }),
            "sqry/search",
            "failed to execute sqry query 'kind:('",
        ),
        (
            3i64,
            "sqry/references",
            json!({ "relation": "callers", "target": "(", "path": path }),
            "sqry/relation",
            "failed to execute relation query",
        ),
    ] {
        let response = wire
            .call(Request::build(method).params(params).id(id).finish())
            .await;
        assert!(response.get("error").is_some(), "{method}: {response}");
        let telemetry = wire
            .wait_for("the error telemetry event", |message| {
                message["method"] == "telemetry/event"
                    && message["params"]["event"] == event
                    && message["params"]["outcome"] == "error"
            })
            .await;
        let reason = telemetry["params"]["reason"].as_str().expect("a reason");
        assert!(
            reason.starts_with(context) && reason.contains(": Parse error: "),
            "{method}: the context and the parser's reason below it: {reason}"
        );
    }
}

// ---------------------------------------------------------------------------
// Round 8: refusals a request answers around
// ---------------------------------------------------------------------------

/// `window/showMessage` and `window/logMessage` type for a warning.
const WARNING: i64 = 2;

/// A Rust workspace folder `name` under `parent`, indexed with no macro
/// options; its `src/lib.rs` defines `plain`.
fn indexed_folder(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("Cargo.toml");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    folder_repository(&root);
    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        &root,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::Refuse,
        "test:r8_lsp_folders",
        &BuildConfig::default(),
        &MacroOptionsRequest::empty(),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("the folder indexes");
    root
}

/// A folder with no index that cannot be written: reading it runs the
/// auto-build, whose build fails with the operating system's reason. The
/// guard makes it writable again on drop, so the temporary directory can
/// be removed even when the test fails.
#[cfg(unix)]
fn read_only_unindexed_folder(parent: &Path, name: &str) -> (PathBuf, Writable) {
    use std::os::unix::fs::PermissionsExt;

    let root = parent.join(name);
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("Cargo.toml");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    folder_repository(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("read-only");
    (root.clone(), Writable(root))
}

#[cfg(unix)]
struct Writable(PathBuf);

#[cfg(unix)]
impl Drop for Writable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// The `location.uri` of every symbol a `workspace/symbol` response holds.
fn result_uris(response: &Value) -> Vec<String> {
    response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a result: {response}"))
        .iter()
        .map(|item| item["location"]["uri"].as_str().expect("a uri").to_owned())
        .collect()
}

fn folder_uri(folder: &Path) -> String {
    format!("file://{}/", folder.display())
}

/// A `method` warning (`window/showMessage` or `window/logMessage`) naming
/// `folder` and every one of `texts`.
fn warning_naming(message: &Value, method: &str, folder: &Path, texts: &[&str]) -> bool {
    message["method"] == method
        && message["params"]["type"] == WARNING
        && message["params"]["message"].as_str().is_some_and(|text| {
            text.contains(&folder.display().to_string())
                && texts.iter().all(|needle| text.contains(needle))
        })
}

/// Round 8 (the S4 regression): over two workspace folders, one refused
/// (its manifest names a plugin id this binary did not compile),
/// `workspace/symbol` answers from the other folder, logs the folder it
/// left out on every request, and shows the refusal once per refusal state:
/// not again for the same refusal, again for the same refusal after the
/// folder's graph was acquired in between, and again for a different one.
/// Before the repair the `?` on the refused folder failed the request for
/// both folders with `-32803`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_leaves_out_a_refused_folder_and_shows_it_once_per_state() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let bad = indexed_folder(&parent, "bad");
    let manifest = GraphStorage::new(&bad).manifest_path().to_path_buf();
    let clean = fs::read(&manifest).expect("clean manifest");
    plant_uncompiled_id(&bad, "r7-uncompiled-plugin");
    let refused_once = fs::read(&manifest).expect("planted manifest");
    let mut wire = Wire::start_with_folders(&[&good, &bad]).await;

    let first = |message: &Value| {
        warning_naming(
            message,
            "window/showMessage",
            &bad,
            &["r7-uncompiled-plugin"],
        ) && !message.to_string().contains("r8-other-plugin")
    };
    let second =
        |message: &Value| warning_naming(message, "window/showMessage", &bad, &["r8-other-plugin"]);
    let left_out = |message: &Value| {
        warning_naming(
            message,
            "window/logMessage",
            &bad,
            &[
                "workspace/symbol: left out the workspace folder",
                "(workspace_incompatible_graph): ",
                "r7-uncompiled-plugin",
            ],
        )
    };

    let response = wire.workspace_symbol("plain", 2).await;
    assert!(response.get("error").is_none(), "{response}");
    let uris = result_uris(&response);
    assert!(
        !uris.is_empty() && uris.iter().all(|uri| uri.starts_with(&folder_uri(&good))),
        "the good folder answers, the refused one is left out: {uris:?}"
    );
    let notice = wire.wait_for("the refusal notice", first).await;
    let text = notice["params"]["message"].as_str().expect("text");
    assert!(
        text.contains(&manifest.display().to_string())
            && !text.contains("..")
            && text.contains("workspace/symbol leaves it out")
            && text.contains(&format!("sqry index --force {}", bad.display())),
        "the notice names the refusal and the remedy: {text}"
    );

    // The same state twice more: the third request's log line is the fence
    // (the server sends every notice a request queues before its response,
    // and the next request's messages after it).
    for id in [3, 4] {
        let response = wire.workspace_symbol("plain", id).await;
        assert_eq!(result_uris(&response), uris, "the same answer");
    }
    wire.wait_for_count("the left-out log line", 3, left_out)
        .await;
    assert_eq!(wire.count(first), 1, "one notice for one refusal state");

    // The folder's graph acquired clears the state: the same refusal is
    // shown again when it returns. The client re-adds the folder so the
    // session drops the graph it cached.
    fs::write(&manifest, &clean).expect("repair the manifest");
    let response = wire.workspace_symbol("plain", 5).await;
    let uris = result_uris(&response);
    assert!(
        uris.iter().any(|uri| uri.starts_with(&folder_uri(&bad)))
            && uris.iter().any(|uri| uri.starts_with(&folder_uri(&good))),
        "both folders answer once the manifest is repaired: {uris:?}"
    );
    fs::write(&manifest, &refused_once).expect("refuse again");
    let folder = json!({ "uri": format!("file://{}", bad.display()), "name": "bad" });
    wire.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({ "event": { "added": [folder.clone()], "removed": [folder] } }),
    )
    .await;
    wire.workspace_symbol("plain", 6).await;
    wire.wait_for_count("the same refusal's notice again", 2, first)
        .await;

    // A different refusal is a new state.
    plant_uncompiled_id(&bad, "r8-other-plugin");
    wire.workspace_symbol("plain", 7).await;
    wire.wait_for("the notice for the new refusal", second)
        .await;
    wire.wait_for_count("the left-out log line", 5, left_out)
        .await;
    assert_eq!(wire.count(first), 2, "the first refusal, twice in all");
    assert_eq!(wire.count(second), 1, "the second state was shown once");
}

/// Round 8: a folder with no index that cannot be written (the auto-build
/// fails) is left out the same way, with the operating system's reason.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_leaves_out_a_folder_whose_build_fails() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let (read_only, _writable) = read_only_unindexed_folder(&parent, "readonly");
    let mut wire = Wire::start_with_folders(&[&good, &read_only]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    assert!(response.get("error").is_none(), "{response}");
    let uris = result_uris(&response);
    assert!(
        !uris.is_empty() && uris.iter().all(|uri| uri.starts_with(&folder_uri(&good))),
        "{uris:?}"
    );
    wire.wait_for("the left-out log line", |message| {
        warning_naming(
            message,
            "window/logMessage",
            &read_only,
            &["Permission denied"],
        )
    })
    .await;
    wire.wait_for("the refusal notice", |message| {
        warning_naming(
            message,
            "window/showMessage",
            &read_only,
            &["Permission denied"],
        )
    })
    .await;
}

/// Round 8: three folders, one good, one refused and one whose build
/// fails: the good one answers and each of the others is reported.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_answers_from_the_good_folder_of_three() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let bad = indexed_folder(&parent, "bad");
    plant_uncompiled_id(&bad, "r7-uncompiled-plugin");
    let (read_only, _writable) = read_only_unindexed_folder(&parent, "readonly");
    let mut wire = Wire::start_with_folders(&[&good, &bad, &read_only]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    assert!(response.get("error").is_none(), "{response}");
    let uris = result_uris(&response);
    assert!(
        !uris.is_empty() && uris.iter().all(|uri| uri.starts_with(&folder_uri(&good))),
        "{uris:?}"
    );
    for (folder, reason) in [
        (&bad, "r7-uncompiled-plugin"),
        (&read_only, "Permission denied"),
    ] {
        wire.wait_for("the left-out log line", |message| {
            warning_naming(message, "window/logMessage", folder, &[reason])
        })
        .await;
        wire.wait_for("the refusal notice", |message| {
            warning_naming(message, "window/showMessage", folder, &[reason])
        })
        .await;
    }
}

/// The `data.folders` entries of an every-folder refusal, in the order the
/// refusal gives them, as (root, kind, message).
fn refused_folders(error: &Value) -> Vec<(String, String, String)> {
    error["data"]["folders"]
        .as_array()
        .unwrap_or_else(|| panic!("data.folders: {error}"))
        .iter()
        .map(|folder| {
            (
                folder["root"].as_str().expect("root").to_owned(),
                folder["kind"].as_str().expect("kind").to_owned(),
                folder["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect()
}

/// Round 8, the other side: when every folder is refused there is nothing
/// to answer from, so the request fails with `RequestFailed` naming each
/// folder and its refusal, the first folder's kind and root in `data`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_fails_when_both_folders_are_refused() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let one = indexed_folder(&parent, "one");
    let two = indexed_folder(&parent, "two");
    plant_uncompiled_id(&one, "r7-uncompiled-plugin");
    plant_uncompiled_id(&two, "r7-uncompiled-plugin");
    let mut wire = Wire::start_with_folders(&[&one, &two]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.starts_with("workspace/symbol: every workspace folder was refused: ")
            && message.contains(&format!("{}: ", one.display()))
            && message.contains(&format!("{}: ", two.display()))
            && message.contains("r7-uncompiled-plugin"),
        "{message}"
    );
    assert_eq!(error["data"]["kind"], "workspace_incompatible_graph");
    // Round 9: the folders in search order, the first folder in `data`.
    assert_eq!(error["data"]["root"], one.display().to_string(), "{error}");
    let folders = refused_folders(error);
    assert_eq!(
        folders
            .iter()
            .map(|(root, kind, _)| (root.clone(), kind.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                one.display().to_string(),
                "workspace_incompatible_graph".to_owned()
            ),
            (
                two.display().to_string(),
                "workspace_incompatible_graph".to_owned()
            ),
        ]
    );
    for (root, _, refusal) in &folders {
        assert!(
            refusal.contains("r7-uncompiled-plugin") && refusal.contains(root.as_str()),
            "each folder's own refusal: {refusal}"
        );
    }
}

/// Round 8: three folders, every one refused (two manifests naming an
/// uncompiled plugin id, one folder whose build fails): one
/// `RequestFailed` naming all three, each with its own kind.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_fails_when_all_three_folders_are_refused() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let one = indexed_folder(&parent, "one");
    let two = indexed_folder(&parent, "two");
    plant_uncompiled_id(&one, "r7-uncompiled-plugin");
    plant_uncompiled_id(&two, "r7-uncompiled-plugin");
    let (read_only, _writable) = read_only_unindexed_folder(&parent, "readonly");
    let mut wire = Wire::start_with_folders(&[&one, &two, &read_only]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    let message = error["message"].as_str().expect("a message");
    for folder in [&one, &two, &read_only] {
        assert!(
            message.contains(&format!("{}: ", folder.display())),
            "{message}"
        );
    }
    assert!(message.contains("Permission denied"), "{message}");
    // Round 9 (plants L-OWN1, L-OWN2, L-OWN3): in search order, in the
    // message and in `data.folders`; `data` names the first folder and its
    // kind, which differs from the last folder's; every entry carries its
    // own refusal.
    let positions: Vec<usize> = [&one, &two, &read_only]
        .iter()
        .map(|folder| {
            message
                .find(&format!("{}: ", folder.display()))
                .expect("named")
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the message names the folders in search order: {message}"
    );
    assert_eq!(error["data"]["kind"], "workspace_incompatible_graph");
    assert_eq!(error["data"]["root"], one.display().to_string());
    let folders = refused_folders(error);
    assert_eq!(
        folders
            .iter()
            .map(|(root, kind, _)| (root.clone(), kind.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                one.display().to_string(),
                "workspace_incompatible_graph".to_owned(),
            ),
            (
                two.display().to_string(),
                "workspace_incompatible_graph".to_owned(),
            ),
            (
                read_only.display().to_string(),
                "workspace_not_ready".to_owned(),
            ),
        ]
    );
    for ((_, _, refusal), reason) in folders.iter().zip([
        "r7-uncompiled-plugin",
        "r7-uncompiled-plugin",
        "Permission denied",
    ]) {
        assert!(refusal.contains(reason), "{reason}: {refusal}");
    }
}

/// Round 9 (plant L-OWN12): a workspace folder deleted after the client
/// named it is refused with no kind of its own (the project cannot be
/// resolved, an internal error); when every folder is so refused, the
/// request names each `workspace_not_ready`, since none can be served.
///
/// The session runs in `workspaceFolder` mode (D-i8-60): under gitRoot a
/// deleted folder resolves to the nearest `.git` above it, the temp root's
/// here and any repository above `TMPDIR` otherwise, and the LSP builds
/// that repository's index instead (an empty `.git` above `TMPDIR` is how
/// this file wrote an index above it). `workspaceFolder` resolves the folder
/// to itself without walking, which is the case this test pins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_names_folders_refused_without_a_kind_workspace_not_ready() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let one = indexed_folder(&parent, "one");
    let two = indexed_folder(&parent, "two");
    let mut wire = Wire::start_with_folders(&[&one, &two]).await;
    wire.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "sqry": { "projectRootMode": "workspaceFolder" } } }),
    )
    .await;
    fs::remove_dir_all(&one).expect("delete the first folder");
    fs::remove_dir_all(&two).expect("delete the second folder");

    let response = wire.workspace_symbol("plain", 2).await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(error["data"]["kind"], "workspace_not_ready", "{response}");
    assert_eq!(error["data"]["root"], one.display().to_string());
    assert_eq!(
        refused_folders(error)
            .into_iter()
            .map(|(root, kind, _)| (root, kind))
            .collect::<Vec<_>>(),
        vec![
            (one.display().to_string(), "workspace_not_ready".to_owned()),
            (two.display().to_string(), "workspace_not_ready".to_owned()),
        ]
    );
}

/// Decision D-i8-60: the gitRoot side of plant L-OWN12, which the test
/// above pins in `workspaceFolder` mode. Under gitRoot a workspace folder
/// deleted after `initialize` resolves to the nearest `.git` above it, here
/// the temp root's own ([`project_tempdir`]), and the LSP builds that
/// repository's missing index (`lsp:project_auto_rebuild`) and answers from
/// it: no refusal, and no symbol, since the folders are gone. Outside a
/// hermetic tree that repository is whatever lies above `TMPDIR`, which is
/// how this file once wrote an index there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_over_deleted_folders_builds_the_repository_above_them() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let one = indexed_folder(&parent, "one");
    let two = indexed_folder(&parent, "two");
    let mut wire = Wire::start_with_folders(&[&one, &two]).await;
    fs::remove_dir_all(&one).expect("delete the first folder");
    fs::remove_dir_all(&two).expect("delete the second folder");
    let storage = GraphStorage::new(&parent);
    assert!(
        !storage.manifest_path().exists(),
        "no index at the repository yet"
    );

    let response = wire.workspace_symbol("plain", 2).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"], json!([]), "{response}");
    let manifest: Value = serde_json::from_slice(
        &fs::read(storage.manifest_path()).expect("the repository's index was built"),
    )
    .expect("manifest json");
    assert_eq!(
        manifest["build_provenance"]["build_command"], "lsp:project_auto_rebuild",
        "{manifest}"
    );
    assert_eq!(
        manifest["root_path"],
        parent.display().to_string(),
        "{manifest}"
    );
}

/// Round 9 (plant L-OWN7): a good folder with no symbol matching the query
/// beside a refused folder answers `[]`, not an error: the request fails
/// only when every folder is refused, not when no folder had a match. The
/// refused folder is still logged and shown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_answers_an_empty_list_when_the_good_folder_has_no_match() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let bad = indexed_folder(&parent, "bad");
    plant_uncompiled_id(&bad, "r7-uncompiled-plugin");
    let mut wire = Wire::start_with_folders(&[&good, &bad]).await;

    let response = wire.workspace_symbol("r9_matches_nothing", 2).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"], json!([]), "{response}");
    wire.wait_for("the left-out log line", |message| {
        warning_naming(
            message,
            "window/logMessage",
            &bad,
            &["(workspace_incompatible_graph): ", "r7-uncompiled-plugin"],
        )
    })
    .await;
    wire.wait_for("the refusal notice", |message| {
        warning_naming(
            message,
            "window/showMessage",
            &bad,
            &["r7-uncompiled-plugin"],
        )
    })
    .await;
}

/// `textDocument/*` requests at `plain` in `src/lib.rs` under `root`, each
/// answered from the file's own text when the graph is refused, with a text
/// its answer carries: the symbol's name, or for a definition its file.
fn document_requests(root: &Path) -> Vec<(&'static str, Value, &'static str)> {
    let uri = format!("file://{}", root.join("src").join("lib.rs").display());
    let at_plain = json!({
        "textDocument": { "uri": uri },
        "position": { "line": 0, "character": 8 },
    });
    vec![
        ("textDocument/hover", at_plain.clone(), "plain"),
        ("textDocument/definition", at_plain.clone(), "src/lib.rs"),
        (
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            "plain",
        ),
        ("textDocument/prepareCallHierarchy", at_plain, "plain"),
    ]
}

/// A fence: `sqry/search`, whose telemetry event the server sends after
/// every message an earlier request sent. Waits for the `n`th such event.
async fn search_fence(wire: &mut Wire, root: &Path, id: i64, n: usize) {
    wire.call(
        Request::build("sqry/search")
            .params(json!({ "query": "plain", "path": root.display().to_string() }))
            .id(id)
            .finish(),
    )
    .await;
    wire.wait_for_count("the search telemetry fence", n, |message| {
        message["method"] == "telemetry/event" && message["params"]["event"] == "sqry/search"
    })
    .await;
}

/// Round 8 (silent degrades): hover, definition, documentSymbol and
/// prepareCallHierarchy answer from the file's own text over a refused graph,
/// as before, but the refusal is now shown: one warning for every request
/// in the same refusal state, a new one when the refusal changes. The
/// other side: over a good index the same requests show nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_handlers_answer_a_refused_graph_from_the_document_and_show_it_once() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let requests = document_requests(&root);
    let notices = |message: &Value| message["method"] == "window/showMessage";

    let mut accepted = Wire::start(&root).await;
    for (id, (method, params, _)) in (10i64..).zip(&requests) {
        let response = accepted
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        assert!(
            response.get("error").is_none() && !response["result"].is_null(),
            "the control: {method} answers over a good index: {response}"
        );
    }
    search_fence(&mut accepted, &root, 20, 1).await;
    assert_eq!(accepted.count(notices), 0, "a good graph shows nothing");
    drop(accepted);

    plant_uncompiled_id(&root, "r7-uncompiled-plugin");
    let mut wire = Wire::start(&root).await;
    for (id, (method, params, carries)) in (10i64..).zip(&requests) {
        let response = wire
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "{method} still answers from the document: {response}"
        );
        assert!(
            response["result"].to_string().contains(carries),
            "{method} answers from the document: {response}"
        );
    }
    search_fence(&mut wire, &root, 20, 1).await;
    let first = |message: &Value| {
        warning_naming(
            message,
            "window/showMessage",
            &root,
            &[
                "r7-uncompiled-plugin",
                "answer from the file's own text only",
                "sqry index --force",
            ],
        )
    };
    assert_eq!(
        wire.count(first),
        1,
        "one warning for four requests in one refusal state: {:?}",
        wire.from_server.lock().unwrap()
    );
    assert_eq!(wire.count(notices), 1, "and no other notice");

    plant_uncompiled_id(&root, "r8-other-plugin");
    let (method, params, _) = &requests[0];
    let response = wire
        .call(
            Request::build(*method)
                .params(params.clone())
                .id(30)
                .finish(),
        )
        .await;
    assert!(response.get("error").is_none(), "{response}");
    wire.wait_for("the notice for the new refusal", |message| {
        warning_naming(message, "window/showMessage", &root, &["r8-other-plugin"])
    })
    .await;
    search_fence(&mut wire, &root, 31, 2).await;
    assert_eq!(wire.count(notices), 2, "one notice per refusal state");
}

/// Round 8 (the `rebuild_index` doc): without `force` over an index whose
/// snapshot is gone, `sqry.index` builds nothing itself and reports the
/// existing index loaded, but the load is the session's, whose self-heal
/// rebuilds and rewrites the snapshot. The doc said nothing was written on
/// this leg. The other side, an intact index left byte for byte, is
/// `handlers_smoke`'s T7.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_without_force_over_a_missing_snapshot_rewrites_it_through_the_self_heal() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    let storage = GraphStorage::new(&root);
    fs::remove_file(storage.snapshot_path()).expect("remove the snapshot");
    let mut wire = Wire::start(&root).await;

    let response = wire.index(&root, false).await;
    assert!(response.get("error").is_none(), "{response}");
    let end = wire
        .progress_end(|end| end.starts_with("✓ Loaded the existing index"))
        .await;
    assert!(end.contains("pass force to rebuild"), "{end}");
    assert!(
        storage.snapshot_path().is_file(),
        "the self-heal rewrote the snapshot"
    );
}

/// A workspace folder `name` under `parent` whose index records an expand
/// cache directory that is then removed, its snapshot corrupted: a read
/// runs the self-heal, whose rebuild refuses the record. Returns the folder
/// and the recorded directory.
fn folder_refusing_its_record(parent: &Path, name: &str) -> (PathBuf, PathBuf) {
    let root = parent.join(name);
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("Cargo.toml");
    fs::write(root.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    folder_repository(&root);
    let cache = root.join("expand-cache");
    fs::create_dir(&cache).expect("cache dir");
    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        &root,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::Refuse,
        "test:r9_lsp_remedy",
        &BuildConfig::default(),
        &MacroOptionsRequest::from_flags(&["test".to_string()], Some(&cache), false),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("the folder indexes");
    fs::remove_dir_all(&cache).expect("remove the cache");
    fs::write(GraphStorage::new(&root).snapshot_path(), b"not a snapshot")
        .expect("corrupt the snapshot");
    (root, cache)
}

/// The macro options a folder's manifest records, `None` when it records
/// none.
fn recorded_macro_options(root: &Path) -> Option<Value> {
    let manifest: Value = serde_json::from_slice(
        &fs::read(GraphStorage::new(root).manifest_path()).expect("manifest"),
    )
    .expect("manifest json");
    manifest
        .get("macro_options")
        .filter(|options| !options.is_null())
        .cloned()
}

/// Round 9 (surfaces round four, finding 3): the remedies the refusal
/// notice names work for a workspace folder other than the first. Three
/// folders: a good one first, then two whose recorded expand cache is gone
/// under a corrupt snapshot. `workspace/symbol` answers from the good one
/// and shows each refusal with both remedies spelled to be run. `sqry.index`
/// with `force` reaches the second folder (it was refused as "outside
/// workspace root", since only the first folder was accepted) and is
/// refused by the record, naming the reset; the reset without `force` over
/// the existing index is refused; the reset arguments as the refusal gives
/// them drop the record and rebuild. The third folder takes the other
/// remedy: its directory restored, `force` rebuilds it. Then every folder
/// answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_refusal_notice_remedies_work_for_every_workspace_folder() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let (reset, _) = folder_refusing_its_record(&parent, "reset");
    let (fix, fix_cache) = folder_refusing_its_record(&parent, "fix");
    let mut wire = Wire::start_with_folders(&[&good, &reset, &fix]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    let uris = result_uris(&response);
    assert!(
        !uris.is_empty() && uris.iter().all(|uri| uri.starts_with(&folder_uri(&good))),
        "the good folder answers, the refused ones are left out: {uris:?}"
    );
    for folder in [&reset, &fix] {
        let rebuild = json!([folder.display().to_string(), true]).to_string();
        let drop_record = json!([folder.display().to_string(), true, true]).to_string();
        let notice = wire
            .wait_for("the refusal notice", |message| {
                warning_naming(
                    message,
                    "window/showMessage",
                    folder,
                    &["expand cache directory"],
                )
            })
            .await;
        let text = notice["params"]["message"].as_str().expect("text");
        for remedy in [
            format!("`sqry index --force {}`", folder.display()),
            format!("the sqry.index command with the arguments {rebuild}"),
            format!(
                "`sqry index --force --no-macro-options {}`",
                folder.display()
            ),
            format!("the sqry.index command with the arguments {drop_record}"),
        ] {
            assert!(text.contains(&remedy), "the notice names {remedy}: {text}");
        }
    }

    // `force` alone reaches the folder and reuses the record: refused by
    // the record, with the arguments that drop it.
    let response = wire
        .index_with(json!([reset.display().to_string(), true]), 3)
        .await;
    let error = &response["error"];
    assert_eq!(error["code"], REQUEST_FAILED, "{response}");
    assert_eq!(error["data"]["kind"], "rebuild_macro_options_unavailable");
    assert_eq!(error["data"]["root"], reset.display().to_string());
    let reset_arguments = error["data"]["resetArguments"].clone();
    assert_eq!(
        reset_arguments,
        json!([reset.display().to_string(), true, true])
    );
    let message = error["message"].as_str().expect("a message");
    assert!(
        !message.contains("outside workspace root")
            && message.contains(&format!(
                "the sqry.index command with the arguments {reset_arguments}"
            )),
        "{message}"
    );

    // The reset without `force` over the existing index builds nothing, so
    // it is refused rather than dropped.
    let response = wire
        .index_with(json!([reset.display().to_string(), false, true]), 4)
        .await;
    assert_eq!(response["error"]["code"], REQUEST_FAILED, "{response}");
    assert_eq!(response["error"]["data"]["kind"], "validation_error");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("needs force")),
        "{response}"
    );
    assert!(
        recorded_macro_options(&reset).is_some(),
        "nothing was built"
    );

    // The arguments as the refusal gives them drop the record and rebuild.
    let response = wire.index_with(reset_arguments, 5).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        recorded_macro_options(&reset),
        None,
        "the record was dropped"
    );

    // The other remedy: the cause fixed, `force` rebuilds the third folder.
    fs::create_dir(&fix_cache).expect("restore the cache");
    let response = wire
        .index_with(json!([fix.display().to_string(), true]), 6)
        .await;
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        recorded_macro_options(&fix).is_some(),
        "force alone keeps the record"
    );

    let response = wire.workspace_symbol("plain", 7).await;
    let uris = result_uris(&response);
    for folder in [&good, &reset, &fix] {
        assert!(
            uris.iter().any(|uri| uri.starts_with(&folder_uri(folder))),
            "{} answers once its index is rebuilt: {uris:?}",
            folder.display()
        );
    }
}

/// Round 9 (surfaces round four, finding 3): a workspace folder nested in
/// a git repository is indexed at the repository root, so the refusal
/// notice names that root; `sqry.index` accepts it, though it is no
/// workspace folder (it is the index root of a folder's project). Before,
/// it answered that the root was outside the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_accepts_the_repository_root_a_nested_folder_is_indexed_at() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let repo = parent.join("repo");
    fs::create_dir_all(repo.join(".git")).expect("a git repository");
    let (_, cache) = folder_refusing_its_record(&parent, "repo");
    let app = repo.join("src");
    let mut wire = Wire::start_with_folders(&[&good, &app]).await;

    wire.workspace_symbol("plain", 2).await;
    let notice = wire
        .wait_for("the refusal notice", |message| {
            warning_naming(
                message,
                "window/showMessage",
                &repo,
                &["expand cache directory"],
            )
        })
        .await;
    let text = notice["params"]["message"].as_str().expect("text");
    assert!(
        text.starts_with(&format!(
            "sqry cannot use the code graph for {}: ",
            repo.display()
        )),
        "the notice names the repository root: {text}"
    );

    fs::create_dir(&cache).expect("restore the cache");
    let response = wire
        .index_with(json!([repo.display().to_string(), true]), 3)
        .await;
    assert!(response.get("error").is_none(), "{response}");

    // A directory the session serves no project at is still refused.
    let outside = parent.join("outside");
    fs::create_dir(&outside).expect("outside");
    let response = wire
        .index_with(json!([outside.display().to_string(), true]), 4)
        .await;
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("outside workspace root")),
        "{response}"
    );
}

/// Round 9 (surfaces round four, finding 4): the refusal notice says what
/// each request does over the refused graph, and each does it. It said
/// every request for a file "answers from the open document only", but
/// references, code lenses and diagnostics answered the refusal, and so did
/// `workspace/symbol` over a single root. The requests that only need one
/// file's symbols answer from its text; those whose answer is a graph fact
/// answer with the refusal (an empty answer would claim there is none);
/// `workspace/symbol` with one root, refused, answers with the refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_refusal_notice_names_what_each_request_does_and_each_does_it() {
    let (_tmp, root, cache) = workspace_with_a_missing_recorded_cache();
    fs::create_dir(&cache).expect("restore the cache");
    plant_uncompiled_id(&root, "r7-uncompiled-plugin");
    let lib = root.join("src").join("lib.rs");
    let uri = format!("file://{}", lib.display());
    let at_plain = json!({
        "textDocument": { "uri": uri },
        "position": { "line": 0, "character": 8 },
    });
    let location = json!([{ "uri": uri, "position": { "line": 0, "character": 8 } }]);
    let item = json!({
        "name": "plain",
        "kind": 12,
        "uri": uri,
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 20 } },
        "selectionRange": { "start": { "line": 0, "character": 7 }, "end": { "line": 0, "character": 12 } },
        "data": {
            "state": "saved",
            "file_path": lib.display().to_string(),
            "qualified_name": "plain",
            "language": "rust",
            "start_line": 1,
            "start_column": 0,
        },
    });
    // Each with a text its answer carries: the symbol's name, or for a
    // definition its file.
    let from_text: Vec<(&str, Value, &str)> = vec![
        ("textDocument/hover", at_plain.clone(), "plain"),
        ("textDocument/definition", at_plain.clone(), "src/lib.rs"),
        (
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            "plain",
        ),
        (
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 8 } },
                "context": { "diagnostics": [] },
            }),
            "plain",
        ),
        (
            "textDocument/prepareCallHierarchy",
            at_plain.clone(),
            "plain",
        ),
        (
            "workspace/executeCommand",
            json!({ "command": "sqry.explainSymbol", "arguments": location }),
            "plain",
        ),
    ];
    let refused: Vec<(&str, Value)> = vec![
        (
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 8 },
                "context": { "includeDeclaration": true },
            }),
        ),
        (
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        ),
        ("callHierarchy/incomingCalls", json!({ "item": item })),
        ("callHierarchy/outgoingCalls", json!({ "item": item })),
        (
            "workspace/executeCommand",
            json!({ "command": "sqry.showCallers", "arguments": location }),
        ),
        (
            "workspace/executeCommand",
            json!({ "command": "sqry.showReferences", "arguments": location }),
        ),
        ("workspace/symbol", json!({ "query": "plain" })),
    ];

    let mut wire = Wire::start(&root).await;
    for (id, (method, params, carries)) in (10i64..).zip(&from_text) {
        let response = wire
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        assert!(
            response.get("error").is_none() && response["result"].to_string().contains(carries),
            "{method} answers from the file's text: {response}"
        );
    }
    for (id, (method, params)) in (30i64..).zip(&refused) {
        let response = wire
            .call(
                Request::build(*method)
                    .params(params.clone())
                    .id(id)
                    .finish(),
            )
            .await;
        assert_eq!(
            response["error"]["code"], REQUEST_FAILED,
            "{method} answers with the refusal: {response}"
        );
        assert_eq!(
            response["error"]["data"]["kind"], "workspace_incompatible_graph",
            "{method}: {response}"
        );
    }

    let notice = wire
        .wait_for("the refusal notice", |message| {
            warning_naming(
                message,
                "window/showMessage",
                &root,
                &["r7-uncompiled-plugin"],
            )
        })
        .await;
    let text = notice["params"]["message"].as_str().expect("text");
    for claim in [
        "for files under it, hover, definition, document symbols, code actions, call hierarchy \
         preparation and sqry.explainSymbol answer from the file's own text only;",
        "references, code lenses, diagnostics, call hierarchy calls and every other request that \
         needs the graph answer with this refusal;",
        "and workspace/symbol leaves it out while another workspace folder answers, and answers \
         with this refusal when none does.",
    ] {
        assert!(text.contains(claim), "the notice says {claim:?}: {text}");
    }
}

/// Round 9 (plant L-OWN6): every handler whose request can queue a refusal
/// notice sends it before it answers, so the first request of a session
/// over a refused graph shows it, whichever request that is. Each request
/// runs alone in a fresh session, so no other handler can send the notice
/// in its place (before, nothing pinned that code lenses send it: a later
/// request did). `workspace/symbol` queues its notice only when another
/// folder answers, so it runs over a good folder and the refused one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_handler_that_can_queue_a_notice_sends_it_before_it_answers() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let bad = indexed_folder(&parent, "bad");
    plant_uncompiled_id(&bad, "r7-uncompiled-plugin");
    let uri = format!("file://{}", bad.join("src").join("lib.rs").display());
    let at_plain = json!({
        "textDocument": { "uri": uri },
        "position": { "line": 0, "character": 8 },
    });
    let location = json!([{ "uri": uri, "position": { "line": 0, "character": 8 } }]);
    let requests: Vec<(&str, Value)> = vec![
        ("textDocument/hover", at_plain.clone()),
        ("textDocument/definition", at_plain.clone()),
        (
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 8 },
                "context": { "includeDeclaration": true },
            }),
        ),
        (
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 8 } },
                "context": { "diagnostics": [] },
            }),
        ),
        (
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        ),
        ("textDocument/prepareCallHierarchy", at_plain),
        (
            "workspace/executeCommand",
            json!({ "command": "sqry.explainSymbol", "arguments": location }),
        ),
    ];
    let notice = |message: &Value| {
        warning_naming(
            message,
            "window/showMessage",
            &bad,
            &["r7-uncompiled-plugin"],
        )
    };

    for (method, params) in &requests {
        let mut wire = Wire::start(&bad).await;
        wire.call(
            Request::build(*method)
                .params(params.clone())
                .id(2i64)
                .finish(),
        )
        .await;
        wire.wait_for(&format!("the notice {method} queued"), notice)
            .await;
    }

    let mut wire = Wire::start_with_folders(&[&good, &bad]).await;
    wire.workspace_symbol("plain", 2).await;
    wire.wait_for("the notice workspace/symbol queued", notice)
        .await;
}

/// Round 9 (surfaces round four, finding 6): a writable workspace folder
/// with no index is built when `workspace/symbol` first reads it and
/// contributes its symbols; the doc said it contributed nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_builds_a_folder_with_no_index_and_answers_from_it() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let good = indexed_folder(&parent, "good");
    let unindexed = parent.join("unindexed");
    fs::create_dir_all(unindexed.join("src")).expect("src");
    fs::write(
        unindexed.join("Cargo.toml"),
        "[package]\nname = \"unindexed\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    fs::write(unindexed.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    folder_repository(&unindexed);
    let storage = GraphStorage::new(&unindexed);
    assert!(!storage.manifest_path().exists(), "no index yet");
    let mut wire = Wire::start_with_folders(&[&good, &unindexed]).await;

    let response = wire.workspace_symbol("plain", 2).await;
    let uris = result_uris(&response);
    assert!(
        uris.iter().any(|uri| uri.starts_with(&folder_uri(&good)))
            && uris
                .iter()
                .any(|uri| uri.starts_with(&folder_uri(&unindexed))),
        "both folders answer: {uris:?}"
    );
    assert!(
        storage.manifest_path().is_file() && storage.snapshot_path().is_file(),
        "the read built the folder's index"
    );
}

/// Round 9 (surfaces round four, finding 3): `sqry.index` accepts any
/// workspace folder, and a path under one, before any request has read it
/// (no project is held for it yet, so only the folder itself vouches for
/// the path). A path under no folder is still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqry_index_accepts_a_workspace_folder_before_any_read() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let first = indexed_folder(&parent, "first");
    let second = indexed_folder(&parent, "second");
    let mut wire = Wire::start_with_folders(&[&first, &second]).await;

    let response = wire
        .index_with(json!([second.display().to_string(), true]), 2)
        .await;
    assert!(response.get("error").is_none(), "{response}");
    let response = wire
        .index_with(json!([second.join("src").display().to_string(), true]), 3)
        .await;
    assert!(response.get("error").is_none(), "{response}");
    let response = wire
        .index_with(json!([parent.display().to_string(), true]), 4)
        .await;
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("outside workspace root")),
        "{response}"
    );
}

/// Decision D-i8-60: the self-heal rewrites only the index at the index
/// root it was given. A single root `outer/sub` reads the index of `outer`
/// (a Cargo project with no repository of its own, so the provider's walk
/// finds its index inside the same project boundary); when that snapshot
/// cannot be loaded, the request is refused with the load failure and both
/// remedies, and `outer`'s index is left as it was. Before, the self-heal
/// rebuilt `outer`'s index, a directory above the root the editor opened
/// that is no index root the session serves. Under gitRoot a project's
/// index root is its repository root, which bounds the walk, so this arm
/// never meets a repository's own index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_root_does_not_self_heal_an_index_above_it() {
    let tmp = project_tempdir();
    let parent = tmp.path().canonicalize().expect("canonical parent");
    let outer = parent.join("outer");
    fs::create_dir_all(outer.join("src")).expect("src");
    fs::write(
        outer.join("Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    fs::write(outer.join("src").join("lib.rs"), "pub fn plain() {}\n").expect("lib.rs");
    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        &outer,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::Refuse,
        "test:d_i8_60",
        &BuildConfig::default(),
        &MacroOptionsRequest::empty(),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("outer indexes");
    fs::write(GraphStorage::new(&outer).snapshot_path(), b"not a snapshot")
        .expect("corrupt the snapshot");
    let outer_index = index_bytes(&outer);
    let sub = outer.join("src");
    let mut wire = Wire::start(&sub).await;

    let response = wire.workspace_symbol("plain", 2).await;
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!("Failed to load graph at {}", outer.display()))
            && message.contains("does not rebuild it")
            && message.contains(&format!("sqry index --force {}", outer.display())),
        "{response}"
    );
    assert_eq!(
        index_bytes(&outer),
        outer_index,
        "the index above the root is not rewritten"
    );
    assert!(
        !GraphStorage::new(&sub).manifest_path().exists(),
        "and nothing is built at the root unasked"
    );
}
