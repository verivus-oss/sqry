//! `sqry index --force`, `sqry update`, a `sqry watch` iteration and the
//! query auto-build hook publish the record current at their publication
//! (decisions D-i8-1 and D-i8-4).
//!
//! Deterministic reproductions, each run against the real binary while
//! this test process holds the index's persist lock. Each waits until the
//! CLI is blocked on that lock (its waiter line in `/proc/locks`), so the
//! interleaving does not depend on how fast the CLI starts; the file is
//! Linux-only for that reason.
//!
//! - a set-aside window: an in-process rebuild pauses with the manifest
//!   moved aside (the mid-persist hook) while `sqry index --force` starts;
//! - a publication race (`index --force`, `update`, a watch iteration, a
//!   pipeline query's auto-build): this process holds the lock while the
//!   CLI starts, then publishes a new record (`cfg=test`) before it
//!   releases. Without the CLI holding the lock from resolution to
//!   publication, the CLI had already resolved the old record and
//!   published it over the new one.
//!
//! The other waits are on real conditions too: the watcher's ready line
//! on its stdout, and the manifest naming the CLI's build command.

#![cfg(target_os = "linux")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use common::sqry_bin;
use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsRequest};
use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock, write_guard};
use sqry_core::progress::no_op_reporter;
use sqry_plugin_registry::{
    HighCostMode, PluginSelectionConfig, UnreadableManifestPolicy,
    build_and_persist_with_workspace_roster,
};

const PLUGIN_ENV: [&str; 4] = [
    "SQRY_INCLUDE_HIGH_COST",
    "SQRY_EXCLUDE_HIGH_COST",
    "SQRY_ENABLE_PLUGINS",
    "SQRY_DISABLE_PLUGINS",
];

static PAUSE: Mutex<Option<PathBuf>> = Mutex::new(None);
static STATE: Mutex<(bool, bool)> = Mutex::new((false, false));
static CHANGED: Condvar = Condvar::new();

fn pause_once(graph_dir: &Path) {
    let armed = {
        let mut pause = PAUSE.lock().unwrap();
        if pause.as_deref() == Some(graph_dir) {
            pause.take();
            true
        } else {
            false
        }
    };
    if !armed {
        return;
    }
    let mut state = STATE.lock().unwrap();
    state.0 = true;
    CHANGED.notify_all();
    while !state.1 {
        state = CHANGED.wait(state).unwrap();
    }
}

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .unwrap();
    fs::write(root.join("config.json"), r#"{"name": "fixture"}"#).unwrap();
}

fn persist(root: &Path, fallback: &PluginSelectionConfig, request: &MacroOptionsRequest) {
    build_and_persist_with_workspace_roster(
        root,
        fallback,
        UnreadableManifestPolicy::Refuse,
        "test:other-writer",
        &BuildConfig::default(),
        request,
        no_op_reporter(),
    )
    .expect("the in-process writer persists");
}

fn cfg_test() -> MacroOptionsRequest {
    MacroOptionsRequest {
        cfg_flags: Some(vec!["test".to_string()]),
        ..MacroOptionsRequest::empty()
    }
}

/// `(records json, recorded cfg flags, build command)`.
fn recorded(root: &Path) -> (bool, Vec<String>, String) {
    let manifest = GraphStorage::new(root).load_manifest().unwrap();
    let json = manifest
        .plugin_selection
        .is_some_and(|s| s.active_plugin_ids.iter().any(|id| id == "json"));
    let cfg = manifest
        .macro_options
        .map(|m| m.cfg_flags)
        .unwrap_or_default();
    (json, cfg, manifest.build_provenance.build_command)
}

fn spawn_index_force(root: &Path) -> Child {
    spawn_sqry(root, &["index", "--force", "."])
}

fn spawn_sqry(root: &Path, args: &[&str]) -> Child {
    let mut command = Command::new(sqry_bin());
    command
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .env("SQRY_AUTO_INDEX", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in PLUGIN_ENV {
        command.env_remove(key);
    }
    command.spawn().expect("spawn sqry")
}

/// Wait until `pid` is blocked waiting for a `flock` (a `->` waiter line in
/// `/proc/locks`). The CLI takes no other `flock`, so this is the index's
/// persist lock this test holds.
fn wait_until_blocked_on_lock(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let locks = fs::read_to_string("/proc/locks").expect("read /proc/locks");
        let blocked = locks.lines().any(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            fields
                .iter()
                .position(|field| *field == "->")
                .and_then(|at| fields.get(at + 4))
                .is_some_and(|waiter| waiter.parse::<u32>().ok() == Some(pid))
        });
        if blocked {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sqry (pid {pid}) never waited for the persist lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn finish(child: Child) -> std::process::Output {
    let output = child.wait_with_output().expect("sqry exits");
    assert!(
        output.status.success(),
        "sqry failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn index_force_after_another_writers_publication_keeps_its_record() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    assert_eq!(recorded(&root).1, Vec::<String>::new(), "precondition");

    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    let child = spawn_index_force(&root);
    // The CLI has resolved the old record lock-free and now waits for this
    // hold: to resolve again and build (fixed), or to persist what it
    // already built (not).
    wait_until_blocked_on_lock(child.id());
    // Another writer publishes cfg=test while the CLI waits.
    persist(&root, &PluginSelectionConfig::default(), &cfg_test());
    drop(held);
    finish(child);

    let (_, cfg, command) = recorded(&root);
    assert_eq!(command, "cli:index", "the CLI published last");
    assert_eq!(
        cfg,
        vec!["test".to_string()],
        "sqry index --force published the record it resolved before another writer's publication"
    );
}

/// `sqry update` (a rebuild over an existing index) publishes through the
/// same path: a record another writer publishes while it waits for the
/// lock is the one it records.
#[test]
fn update_after_another_writers_publication_keeps_its_record() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    assert_eq!(recorded(&root).1, Vec::<String>::new(), "precondition");

    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    let child = spawn_sqry(&root, &["update", "."]);
    wait_until_blocked_on_lock(child.id());
    persist(&root, &PluginSelectionConfig::default(), &cfg_test());
    drop(held);
    finish(child);

    let (_, cfg, command) = recorded(&root);
    assert_eq!(command, "cli:update", "the CLI published last");
    assert_eq!(
        cfg,
        vec!["test".to_string()],
        "sqry update published the record it resolved before another writer's publication"
    );
}

#[test]
fn index_force_during_another_transactions_window_keeps_the_record() {
    write_guard::set_mid_persist_hook(pause_once);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    let include_all = PluginSelectionConfig {
        high_cost_mode: HighCostMode::IncludeAll,
        ..PluginSelectionConfig::default()
    };
    persist(&root, &include_all, &cfg_test());
    assert!(recorded(&root).0, "precondition: json recorded");

    *PAUSE.lock().unwrap() = Some(GraphStorage::new(&root).graph_dir().to_path_buf());
    let a = {
        let root = root.clone();
        std::thread::spawn(move || {
            persist(
                &root,
                &PluginSelectionConfig::default(),
                &MacroOptionsRequest::empty(),
            );
        })
    };
    {
        let mut state = STATE.lock().unwrap();
        while !state.0 {
            state = CHANGED.wait(state).unwrap();
        }
    }
    assert!(!GraphStorage::new(&root).manifest_path().exists());
    let child = spawn_index_force(&root);
    // The CLI found the manifest set aside and waits for the transaction.
    wait_until_blocked_on_lock(child.id());
    STATE.lock().unwrap().1 = true;
    CHANGED.notify_all();
    a.join().expect("the in-process rebuild");
    finish(child);

    let (json, cfg, command) = recorded(&root);
    assert_eq!(command, "cli:index", "the CLI published last");
    assert!(json, "sqry index --force dropped the recorded json plugin");
    assert_eq!(
        cfg,
        vec!["test".to_string()],
        "sqry index --force dropped the recorded cfg flag"
    );
}

/// Wait until the manifest at `root` was published by `build_command`.
fn wait_until_published_by(root: &Path, build_command: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let published = GraphStorage::new(root)
            .load_manifest()
            .is_ok_and(|m| m.build_provenance.build_command == build_command);
        if published {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no manifest published by {build_command}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `sqry watch` iteration publishes the record current at its
/// publication: the iteration starts while this process holds the lock,
/// and another writer records `cfg=test` before the lock is released.
#[test]
fn watch_iteration_after_another_writers_publication_keeps_its_record() {
    use std::io::{BufRead, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    let mut child = spawn_sqry(&root, &["watch", ".", "--debounce", "100"]);
    // The watcher is armed once it prints its prompt.
    let stdout = child.stdout.take().expect("piped stdout");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut ready = Some(ready_tx);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.contains("Press Ctrl+C to stop")
                && let Some(tx) = ready.take()
            {
                let _ = tx.send(());
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("sqry watch started watching");

    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 3 }\n",
    )
    .unwrap();
    // The iteration resolved the old record lock-free and now waits for
    // this hold.
    wait_until_blocked_on_lock(child.id());
    persist(&root, &PluginSelectionConfig::default(), &cfg_test());
    drop(held);
    wait_until_published_by(&root, "cli:watch");
    child.kill().expect("stop sqry watch");
    child.wait().expect("sqry watch exits");
    reader.join().expect("stdout reader");

    let (_, cfg, _) = recorded(&root);
    assert_eq!(
        cfg,
        vec!["test".to_string()],
        "a sqry watch iteration published the record it resolved before another writer's publication"
    );
}

/// The query auto-build hook publishes the record current at its
/// publication. A pipeline query (`sqry query "kind:function | count"`)
/// builds through the hook when the root has no manifest (the executor
/// asks `GraphStorage::exists`); here the index directory exists without
/// one while this process holds the lock, and another writer records
/// `json` and `cfg=test` before it releases.
#[test]
fn query_auto_build_after_another_writers_publication_keeps_its_record() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    assert!(
        !GraphStorage::new(&root).manifest_path().exists(),
        "precondition: no index"
    );
    let child = spawn_sqry(&root, &["query", "kind:function | count", "."]);
    // The hook resolved "no record" lock-free and now waits for this hold.
    wait_until_blocked_on_lock(child.id());
    let include_all = PluginSelectionConfig {
        high_cost_mode: HighCostMode::IncludeAll,
        ..PluginSelectionConfig::default()
    };
    persist(&root, &include_all, &cfg_test());
    drop(held);
    finish(child);

    let (json, cfg, command) = recorded(&root);
    assert_eq!(command, "cli:auto_index", "the auto-build published last");
    assert!(
        json,
        "the auto-build dropped the json plugin another writer recorded"
    );
    assert_eq!(
        cfg,
        vec!["test".to_string()],
        "the auto-build dropped the cfg flag another writer recorded"
    );
}

/// Audit item F (round 8): `sqry index` reports the inputs it built with.
/// The CLI resolves the old record (fast path, no macro options) without
/// the lock and waits; another writer records `json` (include-all) and
/// `cfg=test`; the CLI resolves again under the lock and builds with
/// those. Its "Plugin selection:" and "Macro options:" lines must say so,
/// not repeat the first resolution.
#[test]
fn index_force_reports_the_inputs_it_built_with() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    let child = spawn_index_force(&root);
    wait_until_blocked_on_lock(child.id());
    // Another writer (a `sqry index --include-high-cost --cfg test`)
    // records new inputs: `cfg=test` through the helper, then the selection
    // a high-cost index records (the helper keeps a recorded selection).
    persist(&root, &PluginSelectionConfig::default(), &cfg_test());
    let manifest_path = GraphStorage::new(&root).manifest_path().to_path_buf();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(serde_json::json!("json"));
    manifest["plugin_selection"]["high_cost_mode"] = serde_json::json!("include_all");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    drop(held);
    let output = finish(child);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (json, cfg, _) = recorded(&root);
    assert!(
        json && cfg == vec!["test".to_string()],
        "precondition: built with the new record: json={json} cfg={cfg:?}\nstdout:\n{stdout}"
    );
    let line = |prefix: &str| {
        stdout
            .lines()
            .find(|line| line.trim_start().starts_with(prefix))
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        line("Macro options:"),
        "Macro options: cfg [test] (recorded in the manifest)",
        "stdout:\n{stdout}"
    );
    assert!(
        line("Plugin selection:").starts_with("Plugin selection: include_all,"),
        "the selection line describes the first resolution: {:?}\nstdout:\n{stdout}",
        line("Plugin selection:")
    );
}

/// Like [`spawn_sqry`], with `env` set after the plugin variables are cleared.
fn spawn_sqry_with_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Child {
    let mut command = Command::new(sqry_bin());
    command
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .env("SQRY_AUTO_INDEX", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in PLUGIN_ENV {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command.spawn().expect("spawn sqry")
}

/// Run a pipeline query whose auto-build hook resolved "no record"
/// lock-free and waits for this test's hold of the persist lock; `under_lock`
/// changes the record while the lock is held. Returns the query's output,
/// which must be a refusal.
fn refused_auto_build(
    root: &Path,
    env: &[(&str, &str)],
    under_lock: impl FnOnce(&Path),
) -> std::process::Output {
    let graph_dir = GraphStorage::new(root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    assert!(
        !GraphStorage::new(root).manifest_path().exists(),
        "precondition: no index"
    );
    let child = spawn_sqry_with_env(root, &["query", "kind:function | count", "."], env);
    wait_until_blocked_on_lock(child.id());
    under_lock(root);
    drop(held);
    let output = child.wait_with_output().expect("sqry exits");
    assert!(
        !output.status.success(),
        "the auto-build built over a record its resolution under the lock refuses:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Decision D-i8-4, the hook's second resolution: a manifest another writer
/// leaves unreadable while the hook waits for the lock is refused under the
/// lock, by file, and nothing is built or rewritten. A hook that swallowed
/// that refusal (falling back to the fast path) would rewrite the manifest.
#[test]
fn query_auto_build_refuses_a_manifest_made_unreadable_under_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    let manifest_path = GraphStorage::new(&root).manifest_path().to_path_buf();
    let output = refused_auto_build(&root, &[], |_| {
        fs::write(&manifest_path, b"{}").unwrap();
    });
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&manifest_path.display().to_string()),
        "the refusal names the manifest: {stderr}"
    );
    assert_eq!(
        fs::read(&manifest_path).unwrap(),
        b"{}",
        "a refused auto-build must not rewrite the manifest"
    );
    assert!(
        !GraphStorage::new(&root).snapshot_path().exists(),
        "a refused auto-build must build nothing"
    );
}

/// The hook's second resolution refuses a record, made while it waits,
/// naming a plugin this binary did not compile. A resolution that accepted
/// recorded ids it cannot serve (the full compiled roster beside the
/// recorded selection), or that swallowed the refusal, would publish.
#[test]
fn query_auto_build_refuses_an_uncompiled_plugin_recorded_under_the_lock() {
    const PLANTED_ID: &str = "w1-r8-planted-plugin";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    let manifest_path = GraphStorage::new(&root).manifest_path().to_path_buf();
    let output = refused_auto_build(&root, &[], |root| {
        persist(
            root,
            &PluginSelectionConfig::default(),
            &MacroOptionsRequest::empty(),
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["plugin_selection"]["active_plugin_ids"]
            .as_array_mut()
            .expect("recorded ids")
            .push(serde_json::json!(PLANTED_ID));
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    });
    // The hook's `BuildFailed` renders the whole error chain, so the
    // refusal names the id, as the first resolution's refusal does.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(PLANTED_ID),
        "the refusal names the uncompiled id: {stderr}"
    );
    let manifest = GraphStorage::new(&root).load_manifest().unwrap();
    assert_eq!(
        manifest.build_provenance.build_command, "test:other-writer",
        "a refused auto-build must not publish over the other writer's record"
    );
}

/// The hook's second resolution is read-only: plugin-selection overrides
/// that conflict with a record made while it waits are refused, as on every
/// read-only path. The first resolution saw no index and took the override;
/// a second resolution in the fresh-write mode would let the override win
/// and publish its own selection over the other writer's.
#[test]
fn query_auto_build_refuses_an_override_that_conflicts_with_a_record_made_under_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    let output = refused_auto_build(&root, &[("SQRY_INCLUDE_HIGH_COST", "1")], |root| {
        persist(
            root,
            &PluginSelectionConfig::default(),
            &MacroOptionsRequest::empty(),
        );
    });
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflict with the persisted index selection"),
        "the refusal is the read-only conflict: {stderr}"
    );
    let (json, _, command) = recorded(&root);
    assert_eq!(
        command, "test:other-writer",
        "a refused auto-build must not publish over the other writer's record"
    );
    assert!(!json, "the other writer's fast-path record is kept");
}

/// Audit item F (round 8): a warning only the resolution under the persist
/// lock has is printed too. The CLI resolves a readable record without the
/// lock and waits; another writer leaves the manifest unreadable; the CLI
/// resolves again under the lock, falls back to the fast-path default as
/// `sqry index` does over an unreadable manifest, and must say so.
#[test]
fn index_force_warns_of_a_manifest_made_unreadable_under_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let manifest_path = GraphStorage::new(&root).manifest_path().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    let child = spawn_index_force(&root);
    wait_until_blocked_on_lock(child.id());
    fs::write(&manifest_path, b"{}").unwrap();
    drop(held);
    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("warning: ") && line.contains("manifest"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "the unreadable manifest the build fell back over is warned about once:\n{stderr}"
    );
    // The CLI runs with `.` as its root, so it names the manifest relatively.
    assert!(
        warnings[0].contains(".sqry/graph/manifest.json") && warnings[0].contains("cannot be read"),
        "the warning names the manifest: {}",
        warnings[0]
    );
    let (_, _, command) = recorded(&root);
    assert_eq!(command, "cli:index", "the index published last");
}

/// A watch iteration whose resolution under the persist lock is refused
/// prints the refusal with its cause. The iteration resolves the old record
/// lock-free and waits; another writer records an expand cache directory and
/// the directory is removed before the lock is released, so the inputs
/// resolved again under the lock (D-i8-4) are refused: the macro options'
/// context over the missing recorded directory. The printed line must carry
/// that inner cause.
#[test]
fn watch_iteration_refused_under_the_lock_names_its_cause() {
    use std::io::{BufRead, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest::empty(),
    );
    let mut child = spawn_sqry(&root, &["watch", ".", "--debounce", "100"]);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut ready = Some(ready_tx);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.contains("Press Ctrl+C to stop")
                && let Some(tx) = ready.take()
            {
                let _ = tx.send(());
            }
        }
    });
    let (error_tx, error_rx) = std::sync::mpsc::channel();
    let error_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if line.contains("Error updating graph") {
                let _ = error_tx.send(line);
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("sqry watch started watching");

    let cache_home = tempfile::tempdir().unwrap();
    let cache = cache_home.path().join("expand-cache");
    fs::create_dir_all(&cache).unwrap();
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    let held = IndexWriteLock::acquire(&graph_dir).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 3 }\n",
    )
    .unwrap();
    wait_until_blocked_on_lock(child.id());
    persist(
        &root,
        &PluginSelectionConfig::default(),
        &MacroOptionsRequest {
            expand_cache_dir: Some(cache.clone()),
            ..MacroOptionsRequest::empty()
        },
    );
    fs::remove_dir_all(&cache).unwrap();
    drop(held);
    let line = error_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("the iteration refused under the lock printed its error");
    child.kill().expect("stop sqry watch");
    child.wait().expect("sqry watch exits");
    reader.join().expect("stdout reader");
    error_reader.join().expect("stderr reader");
    assert!(
        line.contains("recorded in the index manifest"),
        "the watch iteration's error must name its cause (the recorded expand cache): {line}"
    );
}
