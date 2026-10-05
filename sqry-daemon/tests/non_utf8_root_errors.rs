//! An error naming a root that is not valid UTF-8 is answered, not a
//! crashed handler (integration round 7, DAEMON_FOLLOWUP: `json!` of a
//! `PathBuf` in the error data).
//!
//! A request path is JSON text, so it is UTF-8, but its canonical form need
//! not be: a link with a UTF-8 name can point at a directory whose name is
//! not. Every error's data built its `root` with `json!` of the `PathBuf`,
//! which unwraps a `Serialize` that fails on such a path, so the handler
//! panicked and the client saw its connection close with no answer. The
//! data now renders the path lossily.

#![cfg(unix)]

mod support;

use std::os::unix::ffi::OsStringExt;

use serde_json::json;
use support::ipc::{TestServer, expect_error};

/// `daemon/rebuild` through a link to a directory whose name is not UTF-8,
/// which is not loaded: `-32004` naming the root lossily. The control: the
/// same request through a UTF-8 directory is answered the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_root_that_is_not_utf8_is_named_in_the_answer() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut name = b"ws-".to_vec();
    name.push(0xff);
    let target = dir.path().join(std::ffi::OsString::from_vec(name));
    std::fs::create_dir(&target).expect("non-UTF-8 dir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("link");
    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).expect("plain dir");

    let mut client = support::ipc::TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    for (label, path, lossy) in [("control", &plain, false), ("non-UTF-8", &link, true)] {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.request("daemon/rebuild", json!({ "path": path.to_string_lossy() })),
        )
        .await
        .unwrap_or_else(|_| panic!("{label}: no answer"));
        let err = expect_error(&resp).clone();
        let root = err
            .data
            .as_ref()
            .and_then(|data| data["root"].as_str())
            .unwrap_or_default()
            .to_string();
        println!("{label}: code={} root={root:?}", err.code);
        assert_eq!(err.code, -32004, "{label}");
        assert_eq!(root.contains('\u{fffd}'), lossy, "{label}: {root}");
    }
    drop(client);
    server.stop().await;
}
