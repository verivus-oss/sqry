//! A durable persist that fails leaves the index it started from
//! (`persist_durable_graph_transaction`; integration round 7, audit S4).
//!
//! The transaction removed the old manifest first and wrote the new
//! snapshot before the analyses and the manifest. A step that failed after
//! the first left no manifest (and, after the snapshot write, a snapshot
//! the old manifest did not describe), so the next rebuild found no
//! recorded selection and recorded the fallback in its place (audit E11,
//! E3). The transaction now keeps the old pair aside and puts it back on
//! failure. Each test makes one step fail with a read-only directory and
//! checks the manifest and the snapshot byte for byte; the read-only
//! precondition is asserted, so a run with privileges that ignore it fails
//! instead of passing without a measurement.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::plugin::PluginManager;
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

fn plugins() -> PluginManager {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(RustPlugin::default()));
    plugins
}

fn persist(root: &Path, label: &str) -> anyhow::Result<()> {
    build_and_persist_graph_with_progress(
        root,
        &plugins(),
        &BuildConfig::default(),
        label,
        None,
        sqry_core::progress::no_op_reporter(),
    )
    .map(drop)
}

fn indexed() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("lib.rs"), b"pub fn first() {}\n").expect("source");
    persist(tmp.path(), "test:first").expect("the first index persists");
    std::fs::write(
        tmp.path().join("lib.rs"),
        b"pub fn first() {}\npub fn second() {}\n",
    )
    .expect("an edit, so the next snapshot differs");
    tmp
}

/// A short digest of a file's bytes, so a failed comparison prints two
/// numbers instead of two byte vectors.
fn digest(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// The directory refuses a new file, or the test has no measurement.
fn assert_read_only(dir: &Path) {
    let probe = dir.join(".write-probe");
    let refused = std::fs::write(&probe, b"x").is_err();
    let _ = std::fs::remove_file(&probe);
    assert!(
        refused,
        "precondition: {} must refuse a write (running with privileges that ignore modes?)",
        dir.display()
    );
}

/// The graph directory's entries other than the manifest and the snapshot,
/// so a leftover rollback file shows.
fn graph_dir_extras(storage: &GraphStorage) -> Vec<String> {
    std::fs::read_dir(storage.graph_dir())
        .expect("read graph dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "manifest.json" && name != "snapshot.sqry")
        .collect()
}

/// A failure after the old manifest was set aside and the new snapshot
/// written (the analyses cannot be written): the old manifest and the old
/// snapshot are back, byte for byte, and nothing is left beside them.
/// Before the repair the manifest was gone and the snapshot was the new
/// one (E11b).
#[test]
fn a_persist_that_fails_after_the_snapshot_write_restores_the_old_pair() {
    let tmp = indexed();
    let storage = GraphStorage::new(tmp.path());
    let manifest_before = digest(storage.manifest_path()).expect("manifest");
    let snapshot_before = digest(storage.snapshot_path()).expect("snapshot");
    let extras_before = graph_dir_extras(&storage);
    let analysis = storage.analysis_dir().to_path_buf();
    let files: Vec<_> = std::fs::read_dir(&analysis)
        .expect("analysis dir")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    for file in &files {
        set_mode(file, 0o444);
    }
    set_mode(&analysis, 0o555);
    assert_read_only(&analysis);

    let failed = persist(tmp.path(), "test:second");

    set_mode(&analysis, 0o755);
    for file in &files {
        set_mode(file, 0o644);
    }
    let manifest_after = digest(storage.manifest_path());
    let snapshot_after = digest(storage.snapshot_path());
    println!(
        "persist failing at the analyses: failed={} manifest unchanged={} snapshot unchanged={} \
         extras={:?}",
        failed.is_err(),
        manifest_after == Some(manifest_before),
        snapshot_after == Some(snapshot_before),
        graph_dir_extras(&storage)
    );
    assert!(failed.is_err(), "the persist fails at the analyses");
    assert_eq!(
        manifest_after,
        Some(manifest_before),
        "the old manifest is back"
    );
    assert_eq!(
        snapshot_after,
        Some(snapshot_before),
        "the old snapshot is back, so the pair is not torn"
    );
    assert_eq!(
        graph_dir_extras(&storage),
        extras_before,
        "nothing is left beside them"
    );
}

/// A failure at the first step (the graph directory refuses the move):
/// nothing changes. This held before the repair too; it is the control.
#[test]
fn a_persist_that_fails_at_its_first_step_changes_nothing() {
    let tmp = indexed();
    let storage = GraphStorage::new(tmp.path());
    let manifest_before = digest(storage.manifest_path()).expect("manifest");
    let snapshot_before = digest(storage.snapshot_path()).expect("snapshot");
    set_mode(storage.graph_dir(), 0o555);
    assert_read_only(storage.graph_dir());

    let failed = persist(tmp.path(), "test:second");

    set_mode(storage.graph_dir(), 0o755);
    assert!(failed.is_err(), "the persist fails at its first step");
    assert_eq!(digest(storage.manifest_path()), Some(manifest_before));
    assert_eq!(digest(storage.snapshot_path()), Some(snapshot_before));
}

/// The success path from the other side: a persist that succeeds leaves
/// the new pair and no rollback file.
#[test]
fn a_persist_that_succeeds_leaves_no_rollback_file() {
    let tmp = indexed();
    let storage = GraphStorage::new(tmp.path());
    let manifest_before = digest(storage.manifest_path()).expect("manifest");
    let extras_before = graph_dir_extras(&storage);
    persist(tmp.path(), "test:second").expect("persists");
    assert_ne!(
        digest(storage.manifest_path()),
        Some(manifest_before),
        "a new manifest was committed"
    );
    assert_eq!(graph_dir_extras(&storage), extras_before);
}
