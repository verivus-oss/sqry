//! RWS12 dirty snapshot validation tests.

mod support;

use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use sqry_daemon::{
    DaemonError,
    workspace::revision::{DirtySnapshotOptions, DirtySnapshotSource},
};
use support::{git, revision_git_repo};

#[test]
fn dirty_snapshot_detects_toctou_mutation_and_reports_typed_error() {
    let repo = revision_git_repo();
    for index in 0..250 {
        let path = repo.path().join(format!("src/file_{index}.rs"));
        fs::write(path, format!("pub fn file_{index}() {{}}\n")).expect("write source");
    }
    git(repo.path(), &["add", "src"]);
    git(repo.path(), &["commit", "-m", "many files"]);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let mutate_path = repo.path().join("src/lib.rs");
    let writer = thread::spawn(move || {
        let mut count = 0_u64;
        while !stop_thread.load(Ordering::Relaxed) {
            let bytes = format!("pub fn changed_{count}() {{}}\n");
            fs::write(&mutate_path, bytes).expect("mutate tracked file");
            count = count.wrapping_add(1);
            thread::sleep(Duration::from_millis(1));
        }
    });

    let options = DirtySnapshotOptions::new(repo.path());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed = None;
    while Instant::now() < deadline {
        if let Err(DaemonError::DirtySnapshotChanged { root }) =
            DirtySnapshotSource::capture(&options)
        {
            observed = Some(root);
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer joins");

    assert_eq!(
        observed.as_deref(),
        Some(repo.path()),
        "dirty snapshot capture should reject a repeatedly mutating worktree"
    );
}

#[test]
fn dirty_snapshot_hashes_exact_bytes_and_ignores_mtime_only_rewrites() {
    let repo = revision_git_repo();
    let options = DirtySnapshotOptions::new(repo.path());

    let first = DirtySnapshotSource::capture(&options).expect("first capture");
    fs::write(repo.path().join("src/lib.rs"), b"pub fn original() {}\n").expect("rewrite bytes");
    let rewritten = DirtySnapshotSource::capture(&options).expect("rewritten capture");
    assert_eq!(
        first.fingerprint().snapshot_digest,
        rewritten.fingerprint().snapshot_digest
    );

    fs::write(repo.path().join("src/lib.rs"), b"pub fn changed() {}\n").expect("change bytes");
    let changed = DirtySnapshotSource::capture(&options).expect("changed capture");
    assert_ne!(
        first.fingerprint().snapshot_digest,
        changed.fingerprint().snapshot_digest
    );
}
