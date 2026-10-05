//! T4b (surface parity W4, design W4-D4): `sqry watch --stats` renders the
//! `sqry update --stats` block after an iteration. The test indexes a
//! fixture, spawns `sqry watch --stats --debounce 300`, edits a source file,
//! and waits for one iteration. The instrument is checked first: the
//! iteration line (`Graph updated in`) must appear, so a watcher that never
//! fired is an inconclusive run and not this test's red. On the pre-change
//! head the iteration line appeared and no statistics followed.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::sqry_bin;

const ITERATION_TIMEOUT: Duration = Duration::from_secs(90);

fn wait_for(buffer: &Arc<Mutex<String>>, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if buffer.lock().expect("buffer").contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn watch_stats_renders_the_update_statistics_after_an_iteration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lib = dir.path().join("lib.rs");
    std::fs::write(&lib, "pub fn alpha() -> u32 { 1 }\n").expect("write fixture");
    let status = Command::new(sqry_bin())
        .arg("index")
        .arg(dir.path())
        .status()
        .expect("run sqry index");
    assert!(status.success(), "sqry index must succeed");

    let mut child = Command::new(sqry_bin())
        .arg("watch")
        .arg("--stats")
        .args(["--debounce", "300"])
        .arg(dir.path())
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sqry watch");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let buffer = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&buffer);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let mut guard = sink.lock().expect("buffer");
            guard.push_str(&line);
            guard.push('\n');
        }
    });
    let err_sink = Arc::clone(&errors);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut guard = err_sink.lock().expect("errors");
            guard.push_str(&line);
            guard.push('\n');
        }
    });

    // Let the watcher register before the edit, then edit repeatedly until
    // an iteration is observed (a watcher registered after the first write
    // sees the second).
    std::thread::sleep(Duration::from_millis(1500));
    let start = Instant::now();
    let mut iteration_seen = false;
    let mut edits = 0u32;
    while start.elapsed() < ITERATION_TIMEOUT {
        edits += 1;
        std::fs::write(
            &lib,
            format!("pub fn alpha() -> u32 {{ 1 }}\npub fn edit_{edits}() -> u32 {{ {edits} }}\n"),
        )
        .expect("edit fixture");
        if wait_for(&buffer, "Graph updated in", Duration::from_secs(10)) {
            iteration_seen = true;
            break;
        }
    }
    let stats_seen =
        iteration_seen && wait_for(&buffer, "Update statistics:", Duration::from_secs(10));
    let _ = child.kill();
    let _ = child.wait();
    let output = buffer.lock().expect("buffer").clone();
    let stderr = errors.lock().expect("errors").clone();
    println!("edits: {edits}\n--- stdout ---\n{output}\n--- stderr ---\n{stderr}");

    assert!(
        iteration_seen,
        "instrument: no watch iteration was observed within {ITERATION_TIMEOUT:?}; the watcher did not fire, so this run is inconclusive"
    );
    assert!(
        stats_seen,
        "--stats must render the update statistics block after the iteration"
    );
    assert!(
        output.contains("Nodes:") && output.contains("hash-based"),
        "the block carries the node count and the hash-based mode: {output}"
    );
}
