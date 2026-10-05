//! U1 (surface parity W4 round 2, design W4-D10): `sqry watch` resolves its
//! build configuration on every iteration, so a recorded expand cache
//! directory that disappears while the watcher runs is refused on the next
//! iteration, nothing is persisted, and the watcher keeps watching.
//!
//! Round 1 resolved the configuration once, above the loop, and every later
//! iteration reused it: the watcher went on rebuilding and persisting with an
//! expand cache that was no longer there, which is the opposite of what
//! W4-D7 requires of every persisting builder.
//!
//! The instrument is checked before the substantive assertions: the manifest
//! must name the cache directory before the deletion, and one iteration must
//! be observed on the untouched fixture. A watcher that never fired makes the
//! run inconclusive, never a pass. The substantive checks are collected and
//! reported together, so a red run prints every failed check (both manifest
//! digests, both snapshot digests and the captured stderr) rather than only
//! the first.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::sqry_bin;
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::persistence::GraphStorage;
use tempfile::TempDir;

/// How long one leg edits the fixture before it gives up.
const LEG_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one edit waits for its effect before the next edit.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// The iteration line `sqry watch` prints after a persisted build.
const ITERATION_LINE: &str = "Graph updated in";
/// The text of the expand-cache refusal (`MacroOptionsError::ExpandCacheMissing`).
const REFUSAL_TEXT: &str = "does not exist";

const LIB_RS: &str = "#[cfg(test)]\npub fn gated() -> u32 { 1 }\n\npub fn always() -> u32 { 2 }\n";
const CARGO_TOML: &str =
    "[package]\nname = \"w4_watch_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// A watched process whose two output streams are drained by background
/// readers, so a full pipe cannot wedge the child.
struct Watcher {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl Watcher {
    fn spawn(root: &Path) -> Self {
        let mut child = Command::new(sqry_bin())
            .arg("watch")
            .args(["--debounce", "300"])
            .arg(root)
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sqry watch");
        let stdout = drain(child.stdout.take().expect("piped stdout"));
        let stderr = drain(child.stderr.take().expect("piped stderr"));
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(mut self) -> (String, String) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Give the drain threads a moment to take the last lines.
        std::thread::sleep(Duration::from_millis(200));
        let out = self.stdout.lock().expect("stdout buffer").clone();
        let err = self.stderr.lock().expect("stderr buffer").clone();
        (out, err)
    }
}

fn drain<R: std::io::Read + Send + 'static>(reader: R) -> Arc<Mutex<String>> {
    let buffer = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&buffer);
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let mut guard = sink.lock().expect("buffer");
            guard.push_str(&line);
            guard.push('\n');
        }
    });
    buffer
}

fn count_of(buffer: &Arc<Mutex<String>>, needle: &str) -> usize {
    buffer.lock().expect("buffer").matches(needle).count()
}

fn wait_for_count(
    buffer: &Arc<Mutex<String>>,
    needle: &str,
    want: usize,
    timeout: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if count_of(buffer, needle) >= want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Edit the fixture until `needle` has been seen `want` times in `buffer`,
/// or the leg's budget runs out. Returns whether it was seen and the number
/// of edits made. Each edit adds a distinct function, so every persisted
/// build changes the snapshot.
fn edit_until(
    root: &Path,
    buffer: &Arc<Mutex<String>>,
    needle: &str,
    want: usize,
    edits: &mut u32,
) -> bool {
    let lib = root.join("src").join("lib.rs");
    let start = Instant::now();
    while start.elapsed() < LEG_TIMEOUT {
        *edits += 1;
        std::fs::write(
            &lib,
            format!("{LIB_RS}pub fn edit_{n}() -> u32 {{ {n} }}\n", n = *edits),
        )
        .expect("edit fixture");
        if wait_for_count(buffer, needle, want, STEP_TIMEOUT) {
            return true;
        }
    }
    false
}

fn fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), CARGO_TOML).expect("write Cargo.toml");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(dir.path().join("src").join("lib.rs"), LIB_RS).expect("write lib.rs");
    dir
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn index(root: &Path, args: &[&OsStr]) {
    let output = Command::new(sqry_bin())
        .arg("index")
        .args(args)
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry index");
    assert!(
        output.status.success(),
        "sqry index {args:?} must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn manifest_document(storage: &GraphStorage) -> serde_json::Value {
    let text = std::fs::read_to_string(storage.manifest_path()).expect("manifest readable");
    serde_json::from_str(&text).expect("manifest is JSON")
}

/// U1: the watcher refuses the iteration whose recorded expand cache is
/// gone, writes nothing, stays alive, and resumes once the directory is
/// back.
#[test]
fn watch_refuses_the_iteration_whose_recorded_expand_cache_is_gone() {
    let project = fixture();
    let root = project.path();
    let cache_home = TempDir::new().expect("cache tempdir");
    let cache = cache_home.path().join("expand-cache");
    std::fs::create_dir_all(&cache).expect("cache dir");

    index(
        root,
        &[
            OsStr::new("--cfg"),
            OsStr::new("test"),
            OsStr::new("--expand-cache"),
            cache.as_os_str(),
        ],
    );

    // Instrument 1: the manifest names the cache directory.
    let storage = GraphStorage::new(root);
    let recorded = manifest_document(&storage)["macro_options"]["expand_cache_dir"]
        .as_str()
        .expect("instrument: the manifest records the expand cache directory")
        .to_string();
    println!("recorded expand cache: {recorded}");
    assert_eq!(
        Path::new(&recorded),
        cache.canonicalize().expect("canonical cache").as_path(),
        "instrument: the record is the canonical directory"
    );

    let mut watcher = Watcher::spawn(root);
    std::thread::sleep(Duration::from_millis(1500));

    // Instrument 2: one iteration is observed on the untouched fixture. A
    // watcher that never fires makes everything below inconclusive.
    let mut first_edits = 0u32;
    if !edit_until(root, &watcher.stdout, ITERATION_LINE, 1, &mut first_edits) {
        let (out, err) = watcher.stop();
        panic!(
            "instrument: no watch iteration within {LEG_TIMEOUT:?} after {first_edits} edit(s); \
             the run is inconclusive, not a pass\n--- stdout ---\n{out}\n--- stderr ---\n{err}"
        );
    }
    println!("first iteration observed after {first_edits} edit(s)");
    let iterations_before_deletion = count_of(&watcher.stdout, ITERATION_LINE);
    println!("iterations before the deletion: {iterations_before_deletion}");

    let manifest_before = sha256_of(storage.manifest_path());
    let snapshot_before = sha256_of(storage.snapshot_path());

    // The refusal leg: the recorded directory disappears mid-run.
    std::fs::remove_dir_all(&cache).expect("remove the recorded cache");
    let mut refusal_edits = 0u32;
    let refused = edit_until(root, &watcher.stderr, REFUSAL_TEXT, 1, &mut refusal_edits);
    // Let any build the edits started finish, so the digests below see what
    // the iteration wrote rather than a write in progress.
    std::thread::sleep(Duration::from_secs(2));
    let manifest_after_refusal = sha256_of(storage.manifest_path());
    let snapshot_after_refusal = sha256_of(storage.snapshot_path());
    let iterations_after_refusal = count_of(&watcher.stdout, ITERATION_LINE);
    let alive_after_refusal = watcher.is_alive();
    let stderr_after_refusal = watcher.stderr.lock().expect("stderr buffer").clone();
    println!(
        "refusal leg: refused {refused} after {refusal_edits} edit(s); iterations {iterations_after_refusal}; \
         alive {alive_after_refusal}"
    );
    println!("manifest sha256 before the deletion: {manifest_before}");
    println!("manifest sha256 after the refusal:   {manifest_after_refusal}");
    println!("snapshot sha256 before the deletion: {snapshot_before}");
    println!("snapshot sha256 after the refusal:   {snapshot_after_refusal}");

    // The control leg: the directory comes back and the watcher resumes.
    let mut resume_edits = 0u32;
    let resumed = if refused {
        std::fs::create_dir_all(&cache).expect("restore the recorded cache");
        edit_until(
            root,
            &watcher.stdout,
            ITERATION_LINE,
            iterations_after_refusal + 1,
            &mut resume_edits,
        )
    } else {
        false
    };
    let manifest_after_resume = sha256_of(storage.manifest_path());
    let alive_at_end = watcher.is_alive();
    let (out, err) = watcher.stop();
    println!(
        "control leg: resumed {resumed} after {resume_edits} edit(s); manifest sha256 after the \
         resume: {manifest_after_resume}; alive at end {alive_at_end}"
    );
    println!("--- stdout ---\n{out}\n--- stderr ---\n{err}");

    let mut failed: Vec<String> = Vec::new();
    if !refused {
        failed.push(format!(
            "the iteration whose recorded expand cache is gone was not refused on stderr \
             within {LEG_TIMEOUT:?}; captured stderr:\n{stderr_after_refusal}"
        ));
    }
    if !stderr_after_refusal.contains(&recorded) {
        failed.push(format!(
            "stderr does not name the directory {recorded}; captured stderr:\n{stderr_after_refusal}"
        ));
    }
    if !(stderr_after_refusal.contains("--expand-cache")
        && stderr_after_refusal.contains("--no-macro-options"))
    {
        failed.push(format!(
            "stderr does not name the two ways out (--expand-cache, --no-macro-options); \
             captured stderr:\n{stderr_after_refusal}"
        ));
    }
    if manifest_after_refusal != manifest_before {
        failed.push(format!(
            "the refused iteration wrote the manifest: sha256 {manifest_before} before, \
             {manifest_after_refusal} after"
        ));
    }
    if snapshot_after_refusal != snapshot_before {
        failed.push(format!(
            "the refused iteration wrote the snapshot: sha256 {snapshot_before} before, \
             {snapshot_after_refusal} after"
        ));
    }
    if iterations_after_refusal != iterations_before_deletion {
        failed.push(format!(
            "an iteration completed while the cache was gone: {iterations_before_deletion} \
             iteration line(s) before the deletion, {iterations_after_refusal} after"
        ));
    }
    if !alive_after_refusal {
        failed.push("the watcher exited after the refused iteration".to_string());
    }
    if !resumed {
        failed.push(
            "restoring the directory did not resume the watcher (no new iteration line)"
                .to_string(),
        );
    }
    if manifest_after_resume == manifest_before {
        failed.push(format!(
            "the resumed iteration wrote no manifest: sha256 {manifest_after_resume} unchanged"
        ));
    }
    if !alive_at_end {
        failed.push("the watcher was not alive at the end of the control leg".to_string());
    }

    println!("failed checks: {}", failed.len());
    assert!(
        failed.is_empty(),
        "{} check(s) failed:\n- {}",
        failed.len(),
        failed.join("\n- ")
    );
}

/// U1 second control leg, invariant I11: a fixture with no recorded macro
/// options builds on every iteration and prints no refusal, so the repair did
/// not turn every watch iteration into a refusal.
#[test]
fn watch_without_recorded_macro_options_builds_every_iteration() {
    let project = fixture();
    let root = project.path();
    index(root, &[]);

    let storage = GraphStorage::new(root);
    let document = manifest_document(&storage);
    let has_record = !document["macro_options"].is_null();
    println!("macro_options key present: {has_record}");
    assert!(
        !has_record,
        "instrument: this fixture records no macro options: {document}"
    );

    let mut watcher = Watcher::spawn(root);
    std::thread::sleep(Duration::from_millis(1500));
    let mut edits = 0u32;
    let first = edit_until(root, &watcher.stdout, ITERATION_LINE, 1, &mut edits);
    let after_first = count_of(&watcher.stdout, ITERATION_LINE);
    let second = first
        && edit_until(
            root,
            &watcher.stdout,
            ITERATION_LINE,
            after_first + 1,
            &mut edits,
        );
    let alive = watcher.is_alive();
    let (out, err) = watcher.stop();
    let iterations = out.matches(ITERATION_LINE).count();
    println!(
        "first {first}, second {second}, edits {edits}, iteration lines {iterations}, alive \
         {alive}\n--- stdout ---\n{out}\n--- stderr ---\n{err}"
    );

    assert!(
        first,
        "instrument: no watch iteration was observed; the run is inconclusive, not a pass"
    );
    assert!(second, "a second iteration must complete as well");
    assert!(
        iterations >= 2,
        "at least the two observed iterations completed: {iterations}"
    );
    assert!(alive, "the watcher is still running");
    assert!(
        !err.contains(REFUSAL_TEXT) && !err.contains("❌"),
        "no refusal and no error without a record: {err}"
    );
}
