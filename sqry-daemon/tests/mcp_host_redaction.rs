//! The daemon-hosted MCP redacts its responses as the standalone server
//! does (integration round 7, decision D-i7-5).
//!
//! The daemon host redacted nothing: under the default preset (`minimal`)
//! the standalone server answers with `<workspace>` where the daemon host
//! printed the absolute root, in a success and in every refusal. These
//! tests run the real `sqryd` binary with no redaction setting in its
//! environment, as for an operator who configured nothing, and hold the
//! daemon host to the rules the standalone server's own test holds it to
//! (`standalone_refusals_are_redacted_under_the_default_preset` in
//! `sqry-mcp/tests/rebuild_index_refusal_wire.rs`): no absolute path
//! survives in a success or in a refusal's message or `data`, the envelope
//! keeps its code and `kind`, and a refusal bound to the request's
//! workspace names the root `<workspace>`. The other side: the same refusal
//! from a host with redaction off names the absolute root.
//!
//! Round 8 (B3): the redacting test builds its workspace under a directory
//! outside every prefix the old in-string pattern listed (`/home`, `/Users`,
//! `/var`, `/srv`, `/opt`, `/tmp`, `/etc`), whatever the host's `TMPDIR`
//! is, so a path the redactor finds only by its prefix fails it.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::GraphStorage;
use support::rebuild_fixtures::{
    index_fast_path, mcp_call, mcp_error, mcp_payload, mcp_rebuild_index, mcp_session,
    mcp_session_at, mixed_workspace, real_server,
};

/// The prefixes the in-string pattern matched before round 8.
const OLD_PREFIXES: [&str; 7] = ["/home", "/Users", "/var", "/srv", "/opt", "/tmp", "/etc"];

/// A fresh directory outside every [`OLD_PREFIXES`] entry: under
/// `/dev/shm` (Linux) or `/private/tmp` (macOS), whichever is a writable
/// directory. Fails, never skips, when neither is.
fn outside_the_old_prefixes() -> tempfile::TempDir {
    for base in ["/dev/shm", "/private/tmp"] {
        if !Path::new(base).is_dir() {
            continue;
        }
        let Ok(dir) = tempfile::Builder::new()
            .prefix("sqry-r8-redaction-")
            .tempdir_in(base)
        else {
            continue;
        };
        let canonical = dir.path().canonicalize().expect("canonical");
        assert!(
            !OLD_PREFIXES
                .iter()
                .any(|prefix| canonical.starts_with(prefix)),
            "{} is under an old prefix",
            canonical.display()
        );
        return dir;
    }
    panic!("no writable /dev/shm or /private/tmp to hold the workspace");
}

/// The fixture `mixed_workspace` builds, under [`outside_the_old_prefixes`].
fn mixed_workspace_outside_the_old_prefixes() -> (tempfile::TempDir, PathBuf) {
    let dir = outside_the_old_prefixes();
    let root = dir.path().canonicalize().expect("canonical root");
    support::init_git_repo(&root);
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn func_alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "count": 3}"#,
    )
    .expect("write config.json");
    (dir, root)
}

/// `true` when `text` names the absolute directory `dir` or anything under
/// it.
fn names(text: &str, dir: &Path) -> bool {
    text.contains(dir.to_string_lossy().as_ref())
}

/// One refusal's message and data as text.
fn rendered(err: &rmcp::ErrorData) -> String {
    format!(
        "{} {}",
        err.message,
        err.data.clone().unwrap_or(Value::Null)
    )
}

fn sqryd_binary() -> PathBuf {
    // Cargo sets this for an integration test of the package that builds
    // the `sqryd` binary, at compile time.
    let path = PathBuf::from(env!("CARGO_BIN_EXE_sqryd"));
    assert!(path.is_file(), "{} is not built", path.display());
    path
}

fn wait_for_socket(path: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A `sqryd` process with an empty environment but for its socket, its
/// runtime and home directories: no `SQRY_REDACTION_PRESET`, no
/// `SQRY_REDACT_*`, no MCP configuration.
struct Daemon {
    child: std::process::Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        Self::start_with(&[])
    }

    /// [`Self::start`], with `extra` added to the daemon's environment.
    fn start_with(extra: &[(&str, &str)]) -> Self {
        let (child, socket, dir) = Self::spawn(extra);
        let daemon = Self {
            child,
            socket,
            _dir: dir,
        };
        assert!(
            wait_for_socket(&daemon.socket, Duration::from_secs(30)),
            "sqryd never bound {}",
            daemon.socket.display()
        );
        daemon
    }

    /// Spawn `sqryd foreground` with `extra` in its environment, its log at
    /// `<dir>/sqryd.log`.
    fn spawn(extra: &[(&str, &str)]) -> (std::process::Child, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sqryd.sock");
        let config = dir.path().join("daemon.toml");
        std::fs::write(
            &config,
            format!("[socket]\npath = {:?}\n", socket.to_string_lossy().as_ref()),
        )
        .expect("write config");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let log = std::fs::File::create(dir.path().join("sqryd.log")).expect("log");
        let child = Command::new(sqryd_binary())
            .arg("foreground")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("TMPDIR", dir.path())
            .env("SQRY_DAEMON_CONFIG", &config)
            .env("XDG_RUNTIME_DIR", dir.path())
            .envs(extra.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn sqryd");
        (child, socket, dir)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Record `dir` as the expand cache in the manifest at `root`.
fn record_expand_cache_as(root: &Path, dir: &str) {
    let storage = GraphStorage::new(root);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"] = json!({ "cfg_flags": [], "expand_cache_dir": dir });
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");
}

/// The default preset on the real daemon: successes and refusals carry no
/// absolute path, and the refusal bound to the workspace reads
/// `<workspace>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_host_redacts_under_the_default_preset() {
    let (tmp, root) = mixed_workspace_outside_the_old_prefixes();
    std::fs::create_dir_all(root.join("sub")).expect("sub");
    std::fs::write(root.join("sub").join("m.rs"), "pub fn in_sub() {}\n").expect("m.rs");
    index_fast_path(&root);
    let daemon = Daemon::start();
    let running = mcp_session_at(&daemon.socket).await;
    let peer = running.peer();

    // Successes: the existing index reported, and a query over it.
    let reported = mcp_payload(
        "existing index",
        &mcp_rebuild_index(peer, &root, false, &[]).await,
    );
    let searched = mcp_payload(
        "search",
        &mcp_call(
            peer,
            "semantic_search",
            json!({ "path": root.to_string_lossy(), "query": "func_alpha" }),
        )
        .await,
    );
    println!("success: {reported}\nsearch: {searched}");
    for (label, payload) in [("existing index", &reported), ("search", &searched)] {
        assert!(
            !names(&payload.to_string(), &root),
            "{label}: no absolute path survives: {payload}"
        );
    }
    assert!(
        reported.to_string().contains("<workspace>"),
        "the root reads <workspace>: {reported}"
    );

    let mut compared = 0;
    for (arguments, kind) in [
        (
            json!({ "path": root.to_string_lossy(), "force": true, "expand_cache": "missing-cache" }),
            "rebuild_macro_options_unavailable",
        ),
        (
            json!({ "path": root.to_string_lossy(), "force": false, "cfg_flags": ["test"] }),
            "validation_error",
        ),
        (
            json!({ "path": root.to_string_lossy(), "force": true, "cfg_flags": [" test"] }),
            "validation_error",
        ),
        (
            json!({ "path": root.join("nope").to_string_lossy(), "force": true }),
            "validation_error",
        ),
        (
            json!({ "path": root.join("sub").to_string_lossy(), "force": false }),
            "validation_error",
        ),
    ] {
        let err = mcp_error(
            "refusal",
            mcp_call(peer, "rebuild_index", arguments.clone()).await,
        );
        let text = rendered(&err);
        println!("{arguments}\n  {text}");
        assert_eq!(err.code.0, -32602, "{arguments}");
        assert_eq!(
            err.data.as_ref().map(|data| data["kind"].clone()),
            Some(json!(kind)),
            "{arguments}"
        );
        assert!(
            !names(&text, tmp.path()) && !names(&text, &root),
            "{arguments}: no absolute path survives: {text}"
        );
        compared += 1;
    }
    assert_eq!(compared, 5, "every refusal was checked");

    // Bound to the request's workspace: the root reads `<workspace>` in
    // the message and in the details, as in a success.
    let err = mcp_error(
        "bound",
        mcp_rebuild_index(
            peer,
            &root,
            true,
            &[("expand_cache", json!("missing-cache"))],
        )
        .await,
    );
    assert!(
        err.message.starts_with("rebuild of <workspace> refused: "),
        "{}",
        err.message
    );
    assert_eq!(
        err.data
            .as_ref()
            .map(|data| data["details"]["root"].clone()),
        Some(json!("<workspace>"))
    );

    // A recorded cache, an uncompiled plugin id and an unreadable manifest:
    // the details that carry paths are redacted as well. The one path kept
    // is `details.reset_arguments.path`, the request's own `path` echoed as
    // it was sent (here not the canonical root), so the remedy can be sent
    // back as it stands, as the standalone server keeps it; sent back, it
    // rebuilds.
    record_expand_cache_as(&root, "missing-cache");
    let as_sent = format!("{}/sub/..", root.to_string_lossy());
    let err = mcp_error(
        "recorded",
        mcp_call(
            peer,
            "rebuild_index",
            json!({ "path": as_sent, "force": true }),
        )
        .await,
    );
    let data = err.data.clone().expect("data");
    assert_eq!(data["details"]["origin"], json!("recorded"), "{data}");
    let reset = data["details"]["reset_arguments"].clone();
    assert_eq!(
        reset,
        json!({ "path": as_sent, "force": true, "reset_macro_options": true }),
        "the remedy echoes the path as sent"
    );
    let mut without_the_echo = data.clone();
    without_the_echo["details"]["reset_arguments"]["path"] = Value::Null;
    let text = format!("{} {without_the_echo}", err.message);
    assert!(!names(&text, &root), "{text}");
    let rebuilt = mcp_payload(
        "the remedy sent back",
        &mcp_call(peer, "rebuild_index", reset).await,
    );
    assert!(
        !names(&rebuilt.to_string(), &root),
        "the success is redacted: {rebuilt}"
    );
    let storage = GraphStorage::new(&root);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"] = Value::Null;
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("ids")
        .push(json!("r7-uncompiled-plugin"));
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("plant the id");
    let err = mcp_error(
        "uncompiled",
        mcp_rebuild_index(peer, &root, true, &[]).await,
    );
    assert_eq!(
        err.data.as_ref().map(|data| data["kind"].clone()),
        Some(json!("workspace_incompatible_graph"))
    );
    assert!(!names(&rendered(&err), &root), "{}", rendered(&err));
    std::fs::write(storage.manifest_path(), b"{").expect("unreadable manifest");
    let err = mcp_error(
        "unreadable",
        mcp_rebuild_index(peer, &root, false, &[]).await,
    );
    assert_eq!(
        err.data.as_ref().map(|data| data["kind"].clone()),
        Some(json!("workspace_not_ready"))
    );
    assert!(!names(&rendered(&err), &root), "{}", rendered(&err));
    drop(running);
}

/// The other side: a host with redaction off answers the same refusal with
/// the absolute root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_with_redaction_off_names_the_absolute_root() {
    let (server, _builder) = real_server(sqry_daemon::DaemonConfig::default()).await;
    let (_tmp, root) = mixed_workspace();
    index_fast_path(&root);
    let running = mcp_session(&server).await;
    let err = mcp_error(
        "plain",
        mcp_rebuild_index(
            running.peer(),
            &root,
            true,
            &[("expand_cache", json!("missing-cache"))],
        )
        .await,
    );
    assert!(names(&rendered(&err), &root), "{}", rendered(&err));
    drop(running);
    server.stop().await;
}

/// Round 8 (D-i8-20): a preset name never turns the daemon's redaction off.
/// A daemon started with `SQRY_REDACTION_PRESET=Strict` served every client
/// unredacted (`McpRedaction::from_preset` knew only the five lowercase
/// names and disabled redaction for any other); it now redacts under the
/// strict preset. An unknown name stops `sqryd` before it binds its socket,
/// with an error naming the value, rather than serving unredacted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_miscased_preset_redacts_and_an_unknown_one_stops_the_daemon() {
    let (tmp, root) = mixed_workspace_outside_the_old_prefixes();
    index_fast_path(&root);
    let daemon = Daemon::start_with(&[("SQRY_REDACTION_PRESET", "Strict")]);
    let running = mcp_session_at(&daemon.socket).await;
    let err = mcp_error(
        "strict",
        mcp_rebuild_index(
            running.peer(),
            &root,
            true,
            &[("expand_cache", json!("missing-cache"))],
        )
        .await,
    );
    let text = rendered(&err);
    assert!(
        !names(&text, tmp.path()) && !names(&text, &root),
        "Strict redacts: {text}"
    );
    drop(running);
    drop(daemon);

    let (mut child, socket, dir) = Daemon::spawn(&[("SQRY_REDACTION_PRESET", " bogus ")]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sqryd with an unknown preset kept running");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let log = std::fs::read_to_string(dir.path().join("sqryd.log")).expect("log");
    assert!(!status.success(), "an unknown preset stops sqryd: {log}");
    assert!(
        log.contains("SQRY_REDACTION_PRESET") && log.contains("\" bogus \""),
        "the refusal names the value: {log}"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_err(),
        "nothing serves on {}",
        socket.display()
    );
}
