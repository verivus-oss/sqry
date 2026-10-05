//! `sqry daemon rebuild` as the daemon receives it.
//!
//! A fake daemon on a temporary Unix socket answers the hello handshake and
//! records every JSON-RPC request the CLI sends, so the tests assert the
//! request on the wire: the CLI makes a relative `--expand-cache` absolute
//! against the caller's working directory, sends `reset_macro_options` for
//! `--no-macro-options`, and sends nothing it was not given. Before this
//! file the CLI-to-client mapping had no test at all.
//!
//! The same fake pins the exit contract: `--timeout 0` exits 0 once the
//! request is delivered, an elapsed `--timeout` exits 2, and a request the
//! client cannot encode (a path that is not valid UTF-8) is refused with
//! exit 1 and never sent, where it used to panic with exit 101.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::sqry_bin;
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum Reply {
    /// Answer every request with a completed rebuild.
    Completed,
    /// Never answer; keep the connection open.
    Never,
}

struct FakeDaemon {
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    _dir: TempDir,
}

fn read_frame(stream: &mut UnixStream) -> Option<Value> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_frame(stream: &mut UnixStream, value: &Value) {
    let body = serde_json::to_vec(value).expect("json");
    let len = u32::try_from(body.len()).expect("frame size");
    stream.write_all(&len.to_le_bytes()).expect("write length");
    stream.write_all(&body).expect("write body");
    stream.flush().expect("flush");
}

impl FakeDaemon {
    fn start(reply: Reply) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("fake.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let record = Arc::clone(&record);
                std::thread::spawn(move || {
                    // The CLI's reachability probe connects and closes.
                    let Some(hello) = read_frame(&mut stream) else {
                        return;
                    };
                    assert!(hello.get("client_version").is_some(), "{hello}");
                    write_frame(
                        &mut stream,
                        &json!({
                            "compatible": true,
                            "daemon_version": "fake",
                            "envelope_version": sqry_daemon_protocol::ENVELOPE_VERSION,
                        }),
                    );
                    while let Some(request) = read_frame(&mut stream) {
                        record.lock().unwrap().push(request.clone());
                        if matches!(reply, Reply::Completed) {
                            write_frame(
                                &mut stream,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": { "result": {
                                        "duration_ms": 1, "nodes": 3,
                                        "edges": 0, "files_indexed": 1
                                    } },
                                }),
                            );
                        }
                    }
                });
            }
        });
        Self {
            socket,
            requests,
            _dir: dir,
        }
    }

    /// The `daemon/rebuild` requests received, waiting up to five seconds
    /// for at least `count`.
    fn rebuild_requests(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let found: Vec<Value> = self
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request["method"] == "daemon/rebuild")
                .cloned()
                .collect();
            if found.len() >= count || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn all_requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> std::process::Command {
        let config = cwd.join("r7-empty-daemon.toml");
        if !config.exists() {
            std::fs::write(&config, "").expect("empty daemon.toml");
        }
        let mut command = std::process::Command::new(sqry_bin());
        command
            .args(["daemon", "rebuild"])
            .args(args)
            .current_dir(cwd)
            .env("SQRY_DAEMON_SOCKET", &self.socket)
            .env("SQRY_DAEMON_CONFIG", &config)
            .env("XDG_RUNTIME_DIR", self._dir.path())
            .env("NO_COLOR", "1");
        command
    }

    fn rebuild(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self
            .command(cwd, args)
            .output()
            .expect("run sqry daemon rebuild");
        print_output(args, &output);
        output
    }

    /// `rebuild`, but a CLI still running after `bound` is killed and the
    /// test fails, so a CLI that waits for an answer the fake never sends
    /// fails the test instead of hanging it.
    fn rebuild_within(&self, cwd: &Path, args: &[&str], bound: Duration) -> Output {
        let mut child = self
            .command(cwd, args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sqry daemon rebuild");
        let deadline = Instant::now() + bound;
        loop {
            if child.try_wait().expect("poll the CLI").is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill the waiting CLI");
                let output = child.wait_with_output().expect("reap");
                print_output(args, &output);
                panic!(
                    "sqry daemon rebuild {args:?} was still running after {bound:?}: it waited \
                     for the outcome"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().expect("collect output");
        print_output(args, &output);
        output
    }
}

fn print_output(args: &[&str], output: &Output) {
    println!(
        "sqry daemon rebuild {args:?}: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn workspace() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir
        .path()
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    (dir, root)
}

/// The macro flags reach the wire as the daemon reads them: the cfg flags
/// in order, `--expand-cache` absolute against the caller's working
/// directory (not the daemon's), and `reset_macro_options` for
/// `--no-macro-options`.
#[test]
fn daemon_rebuild_sends_the_macro_flags_with_an_absolute_expand_cache() {
    let daemon = FakeDaemon::start(Reply::Completed);
    let (_ws, root) = workspace();
    let caller = TempDir::new().expect("caller cwd");
    std::fs::create_dir(caller.path().join("cache")).expect("cache");
    let output = daemon.rebuild(
        caller.path(),
        &[
            &root,
            "--force",
            "--cfg",
            "test",
            "--cfg",
            "feature=x",
            "--expand-cache",
            "cache",
            "--no-macro-options",
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let requests = daemon.rebuild_requests(1);
    assert_eq!(requests.len(), 1, "{requests:?}");
    let expected_cache = caller
        .path()
        .canonicalize()
        .expect("canonical cwd")
        .join("cache");
    assert_eq!(
        requests[0]["params"],
        json!({
            "path": root,
            "force": true,
            "cfg_flags": ["test", "feature=x"],
            "expand_cache": expected_cache.to_string_lossy(),
            "reset_macro_options": true,
        })
    );
}

/// Without macro flags the request carries `path` and `force` alone, so the
/// daemon reuses the recorded options.
#[test]
fn daemon_rebuild_sends_only_the_fields_given() {
    let daemon = FakeDaemon::start(Reply::Completed);
    let (ws, root) = workspace();
    let output = daemon.rebuild(ws.path(), &[&root]);
    assert_eq!(output.status.code(), Some(0));
    let requests = daemon.rebuild_requests(1);
    assert_eq!(
        requests[0]["params"],
        json!({ "path": root, "force": false })
    );
}

/// `--timeout 0` sends the request, does not wait for a reply that never
/// comes, and exits 0 saying the request was delivered; the daemon holds
/// the request. `--json` reports `status: sent`. Each call is bounded: the
/// fake never answers, so a CLI that waited would run until killed, and the
/// bound fails the test instead of hanging it.
#[test]
fn daemon_rebuild_timeout_zero_delivers_and_exits_zero() {
    const BOUND: Duration = Duration::from_secs(30);
    let daemon = FakeDaemon::start(Reply::Never);
    let (ws, root) = workspace();
    let output = daemon.rebuild_within(
        ws.path(),
        &[&root, "--timeout", "0", "--cfg", "test"],
        BOUND,
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("delivered to the daemon"),
        "the message says what happened"
    );
    let requests = daemon.rebuild_requests(1);
    assert_eq!(requests.len(), 1, "the request was delivered");
    assert_eq!(requests[0]["params"]["cfg_flags"], json!(["test"]));

    let output = daemon.rebuild_within(ws.path(), &[&root, "--timeout", "0", "--json"], BOUND);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("json report");
    assert_eq!(report["status"], "sent");
    assert_eq!(daemon.rebuild_requests(2).len(), 2);
}

/// A `--timeout` that elapses before the outcome exits 2.
#[test]
fn daemon_rebuild_elapsed_timeout_exits_two() {
    let daemon = FakeDaemon::start(Reply::Never);
    let (ws, root) = workspace();
    let output = daemon.rebuild(ws.path(), &[&root, "--timeout", "1"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
}

/// From a working directory whose name is not valid UTF-8, a relative
/// `--expand-cache` makes a path JSON text cannot carry. The CLI refuses it
/// with exit 1 naming the field, with and without `--timeout 0`, and sends
/// no request; it used to panic (exit 101) inside the client.
#[test]
fn daemon_rebuild_refuses_a_non_utf8_expand_cache_without_sending() {
    use std::os::unix::ffi::OsStringExt;

    let daemon = FakeDaemon::start(Reply::Completed);
    let (_ws, root) = workspace();
    let outer = TempDir::new().expect("outer");
    let mut name = b"caller-".to_vec();
    name.push(0xff);
    let caller = outer.path().join(std::ffi::OsString::from_vec(name));
    std::fs::create_dir_all(caller.join("cache")).expect("non-UTF-8 cwd");
    for extra in [None, Some("0")] {
        let mut args = vec![root.as_str(), "--force", "--expand-cache", "cache"];
        if let Some(timeout) = extra {
            args.extend(["--timeout", timeout]);
        }
        let output = daemon.rebuild(&caller, &args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{extra:?}: a refusal, not a panic"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot encode `expand_cache`") && stderr.contains("nothing was sent"),
            "{extra:?}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
    assert!(
        daemon
            .all_requests()
            .iter()
            .all(|request| request["method"] != "daemon/rebuild"),
        "nothing was sent: {:?}",
        daemon.all_requests()
    );
}

/// An empty, a blank and a padded `--cfg` value are each a usage error
/// (exit 2) with the core's message, with or without `--timeout 0`, and
/// none reaches the daemon; a blank one used to be exit 1 and a padded one
/// was sent as given. A real flag is the accepted control: it is sent.
#[test]
fn daemon_rebuild_refuses_an_empty_blank_or_padded_cfg_flag_without_sending() {
    let daemon = FakeDaemon::start(Reply::Completed);
    let (ws, root) = workspace();
    for (value, says) in [
        ("", "a cfg flag is empty"),
        (" ", "a cfg flag is empty"),
        (" test", "has leading or trailing whitespace"),
        ("test\t", "has leading or trailing whitespace"),
    ] {
        for timeout in ["1", "0"] {
            let output = daemon.rebuild(ws.path(), &[&root, "--timeout", timeout, "--cfg", value]);
            assert_eq!(output.status.code(), Some(2), "{value:?} {timeout}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(says),
                "{value:?} {timeout}"
            );
        }
    }
    assert!(
        daemon
            .all_requests()
            .iter()
            .all(|request| request["method"] != "daemon/rebuild"),
        "nothing was sent"
    );
    let output = daemon.rebuild(ws.path(), &[&root, "--cfg", "test"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        daemon.rebuild_requests(1)[0]["params"]["cfg_flags"],
        serde_json::json!(["test"])
    );
}
