//! A rebuild that runs while another transaction has the manifest moved
//! aside keeps the record (decisions D-i8-1 and D-i8-2).
//!
//! The reviewer's reproduction, made deterministic: an index records
//! `cfg=test` and the json plugin; rebuild A pauses in its set-aside window
//! (through the mid-persist hook), an ordinary rebuild B starts, and A is
//! released once B waits for the persist lock (its waiter line in
//! `/proc/locks`; the file is Linux-only for that reason, since no fixed
//! sleep stands in for the condition elsewhere). Before the fix B read the
//! window as an unindexed root, resolved the fast-path roster and no macro
//! options, and published them over A's record. This file is its own test
//! binary, so the process-wide hook pauses only these transactions.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsRequest};
use sqry_core::graph::unified::persistence::{GraphStorage, write_guard};
use sqry_core::progress::no_op_reporter;
use sqry_plugin_registry::{
    HighCostMode, PersistedBuild, PluginSelectionConfig, UnreadableManifestPolicy,
    build_and_persist_with_workspace_roster,
};

/// The graph directory whose next persist pauses, and whether it arrived.
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

fn wait_until_paused() {
    let mut state = STATE.lock().unwrap();
    while !state.0 {
        state = CHANGED.wait(state).unwrap();
    }
}

/// Wait until a thread of this process is blocked on a `flock` (a `->`
/// waiter line naming this pid in `/proc/locks`): rebuild B, waiting for
/// A's persist lock, so it has resolved nothing from A's window yet. This
/// test binary runs this one test, so no other thread of it takes a lock.
fn wait_until_a_thread_waits_for_the_lock() {
    let pid = std::process::id();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let locks = std::fs::read_to_string("/proc/locks").expect("read /proc/locks");
        let waiting = locks.lines().any(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            fields
                .iter()
                .position(|field| *field == "->")
                .and_then(|at| fields.get(at + 4))
                .is_some_and(|waiter| waiter.parse::<u32>().ok() == Some(pid))
        });
        if waiting {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "rebuild B never waited for the persist lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn release() {
    STATE.lock().unwrap().1 = true;
    CHANGED.notify_all();
}

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .unwrap();
    std::fs::write(root.join("config.json"), br#"{"name": "fixture"}"#).unwrap();
}

fn rebuild(
    root: &Path,
    fallback: &PluginSelectionConfig,
    request: &MacroOptionsRequest,
) -> PersistedBuild {
    build_and_persist_with_workspace_roster(
        root,
        fallback,
        UnreadableManifestPolicy::Refuse,
        "test:concurrent-record",
        &BuildConfig::default(),
        request,
        no_op_reporter(),
    )
    .expect("build and persist")
}

#[test]
fn a_rebuild_during_another_transactions_window_keeps_the_record() {
    write_guard::set_mid_persist_hook(pause_once);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_fixture(&root);

    // The record: json (a high-cost plugin) and cfg=test.
    let include_all = PluginSelectionConfig {
        high_cost_mode: HighCostMode::IncludeAll,
        ..PluginSelectionConfig::default()
    };
    let cfg_test = MacroOptionsRequest {
        cfg_flags: Some(vec!["test".to_string()]),
        ..MacroOptionsRequest::empty()
    };
    rebuild(&root, &include_all, &cfg_test);
    let recorded = |root: &Path| {
        let manifest = GraphStorage::new(root).load_manifest().unwrap();
        let ids = manifest.plugin_selection.unwrap().active_plugin_ids;
        let cfg = manifest
            .macro_options
            .map(|m| m.cfg_flags)
            .unwrap_or_default();
        (ids.iter().any(|id| id == "json"), cfg)
    };
    assert_eq!(
        recorded(&root),
        (true, vec!["test".to_string()]),
        "precondition"
    );

    // Rebuild A pauses with the manifest moved aside.
    *PAUSE.lock().unwrap() = Some(GraphStorage::new(&root).graph_dir().to_path_buf());
    let a = {
        let root = root.clone();
        std::thread::spawn(move || {
            rebuild(
                &root,
                &PluginSelectionConfig::default(),
                &MacroOptionsRequest::empty(),
            )
        })
    };
    wait_until_paused();
    assert!(
        !GraphStorage::new(&root).manifest_path().exists(),
        "precondition: A is inside its set-aside window"
    );

    // Rebuild B, an ordinary rebuild, runs while A is paused.
    let b = {
        let root = root.clone();
        std::thread::spawn(move || {
            rebuild(
                &root,
                &PluginSelectionConfig::default(),
                &MacroOptionsRequest::empty(),
            )
        })
    };
    wait_until_a_thread_waits_for_the_lock();
    release();
    let a = a.join().expect("rebuild A");
    let b = b.join().expect("rebuild B");

    for (name, built) in [("A", &a), ("B", &b)] {
        assert!(
            built
                .roster
                .selection
                .active_plugin_ids
                .iter()
                .any(|id| id == "json"),
            "rebuild {name} dropped the recorded json plugin: {:?}",
            built.roster.selection
        );
        assert_eq!(
            built.macro_options.options.cfg_flags,
            vec!["test".to_string()],
            "rebuild {name} dropped the recorded cfg flag"
        );
    }
    assert_eq!(
        recorded(&root),
        (true, vec!["test".to_string()]),
        "the final manifest lost the recorded inputs"
    );
}
