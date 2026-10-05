//! C001b / C001b-core — observable end-to-end smoke for the
//! `sqry index . --cache-dir <T>` flag.
//!
//! Iter1 reviewer flagged that previous coverage stopped at the helper
//! boundary (`persist_hash_index_snapshot` unit test) and never exercised
//! the CLI flag through the dispatcher to assert the on-disk artifact
//! shape. This test launches the actual `sqry` binary, runs `index`
//! with `--cache-dir <T>`, and asserts:
//!
//! 1. The command exits successfully.
//! 2. After the build finishes, `<T>/file_hashes.bin` exists (the
//!    filename `HashIndex::save` in sqry-core `indexing/incremental.rs`
//!    writes).
//! 3. The artifact is non-empty.
//! 4. `HashIndex::load(<T>)` decodes the artifact without error — i.e.
//!    the postcard envelope round-trips through the public load API.
//!
//! Env isolation mirrors the `installed_feature_surface_e2e.rs::run`
//! helper (HOME, XDG_*, `SQRY_NO_HISTORY`, NO_COLOR, isolated daemon
//! socket) so the test never touches host state.

mod common;

use common::sqry_bin;
use sqry_core::indexing::HashIndex;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Mirror of `installed_feature_surface_e2e.rs::run` env shape, narrowed
/// to the surface this single-flag smoke needs.
fn run_isolated(project: &Path, args: &[&str]) -> std::process::Output {
    fs::create_dir_all(project.join(".home")).expect("create isolated home");
    fs::create_dir_all(project.join(".xdg/config")).expect("create isolated config");
    fs::create_dir_all(project.join(".xdg/cache")).expect("create isolated cache");
    fs::create_dir_all(project.join(".xdg/data")).expect("create isolated data");
    fs::create_dir_all(project.join(".xdg/runtime")).expect("create isolated runtime");
    let isolated_socket = project.join(".xdg/runtime/sqryd.sock");
    Command::new(sqry_bin())
        .args(args)
        .current_dir(project)
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("SQRY_REDACTION_PRESET", "none")
        .env("HOME", project.join(".home"))
        .env("XDG_CONFIG_HOME", project.join(".xdg/config"))
        .env("XDG_CACHE_HOME", project.join(".xdg/cache"))
        .env("XDG_DATA_HOME", project.join(".xdg/data"))
        .env("XDG_RUNTIME_DIR", project.join(".xdg/runtime"))
        .env("SQRY_DAEMON_SOCKET", isolated_socket)
        .output()
        .expect("run sqry index")
}

#[test]
fn cache_dir_flag_persists_hash_index_to_target_dir() {
    let project = TempDir::new().expect("create project tempdir");
    let project_path = project.path();

    // Materialise a small Rust project the indexer can chew through.
    fs::write(
        project_path.join("a.rs"),
        "fn alpha() -> u32 { 1 }\nfn beta() -> u32 { 2 }\n",
    )
    .expect("write a.rs");
    fs::write(
        project_path.join("b.rs"),
        "fn gamma() -> u32 { 3 }\nfn delta() -> u32 { 4 }\n",
    )
    .expect("write b.rs");
    fs::write(project_path.join("c.rs"), "fn epsilon() -> u32 { 5 }\n").expect("write c.rs");

    // Cache dir must be under the project tempdir so the test never
    // pollutes host state. The directory does not need to exist before
    // the call — `persist_hash_index_snapshot` creates it.
    let cache_dir = project_path.join("hashindex-cache");

    // C001b: drive the actual CLI flag end-to-end through the binary.
    let output = run_isolated(
        project_path,
        &[
            "index",
            ".",
            "--cache-dir",
            cache_dir.to_str().expect("cache_dir to str"),
        ],
    );

    assert!(
        output.status.success(),
        "sqry index --cache-dir failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Assertion 1 — the canonical `HashIndex::save()` filename
    // (`file_hashes.bin`) lives at the supplied cache-dir root.
    let hash_file = cache_dir.join("file_hashes.bin");
    assert!(
        hash_file.exists(),
        "expected HashIndex artifact at {} after `sqry index --cache-dir`; \
         directory contents: {:?}",
        hash_file.display(),
        fs::read_dir(&cache_dir)
            .map(|rd| rd
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .collect::<Vec<_>>())
            .unwrap_or_default(),
    );

    // Assertion 2 — non-empty: empty-postcard envelopes are still a few
    // bytes (header + magic), so >0 is the load-bearing check.
    let metadata = fs::metadata(&hash_file).expect("stat file_hashes.bin");
    assert!(
        metadata.len() > 0,
        "HashIndex artifact at {} is empty",
        hash_file.display(),
    );

    // Assertion 3 — round-trip the artifact through the public `HashIndex::load`
    // API. Decoding success proves the producer/consumer halves agree on the
    // V2 envelope shape.
    let loaded = HashIndex::load(&cache_dir).expect("HashIndex::load decode");

    // Assertion 4 — the loaded index actually covers the source files
    // we just indexed. The CLI walks the project tempdir, hashes every
    // `.rs` file, and persists the result; the loaded index must carry
    // at least one entry for that work to be observable.
    let entry_count = loaded.len();
    assert!(
        entry_count >= 1,
        "loaded HashIndex covered zero files; expected >=1 entry for the \
         3 .rs files indexed (len={entry_count})",
    );
}

/// The paths under `project` a refused call must leave as they were: the
/// index directory and the hash-index file, each absent or with its bytes.
fn written_state(project: &Path, cache: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let manifest = project.join(".sqry").join("graph").join("manifest.json");
    let snapshot = project.join(".sqry").join("graph").join("snapshot.sqry");
    [manifest, snapshot, cache.to_path_buf()]
        .into_iter()
        .map(|path| (path.display().to_string(), fs::read(&path).ok()))
        .collect()
}

/// A `--cache-dir` that names no directory the hash index can be written
/// into is refused by name before anything is written, by `sqry index` and
/// by `sqry update` alike: a file, a path under a file, and (on Unix) a link
/// to nothing. Each used to build, fail the hash-index write after the build
/// with a log line the CLI does not show, and exit 0 with no hash index
/// written. An existing directory and one that does not exist yet are the
/// accepted controls: both get `file_hashes.bin`.
#[test]
fn a_cache_dir_that_is_not_a_directory_is_refused_before_anything_is_written() {
    let project = TempDir::new().expect("project");
    let root = project.path();
    fs::write(root.join("a.rs"), "fn alpha() -> u32 { 1 }\n").expect("a.rs");
    fs::write(root.join("not-a-dir"), b"a file").expect("file");

    let mut cases = vec![
        ("a file", "not-a-dir".to_string(), "is not a directory"),
        (
            "a path under a file",
            "not-a-dir/cache".to_string(),
            "cannot be read",
        ),
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).expect("link");
        cases.push((
            "a link to nothing",
            "dangling".to_string(),
            "is a link to nothing",
        ));
    }

    for (case, dir, says) in &cases {
        let cache = root.join(dir);
        let before = written_state(root, &cache);
        let output = run_isolated(root, &["index", ".", "--cache-dir", dir]);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "index, {case}: {err}");
        assert!(
            err.contains(&format!("--cache-dir {dir} {says}"))
                && err.contains("nothing was written"),
            "index, {case}: {err}"
        );
        assert_eq!(
            written_state(root, &cache),
            before,
            "index, {case}: nothing was written"
        );
        assert!(
            !root.join(".sqry").exists(),
            "index, {case}: no index was built"
        );
    }

    let output = run_isolated(root, &["index", "."]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (case, dir, says) in &cases {
        let cache = root.join(dir);
        let before = written_state(root, &cache);
        let output = run_isolated(root, &["update", ".", "--cache-dir", dir]);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "update, {case}: {err}");
        assert!(
            err.contains(&format!("--cache-dir {dir} {says}"))
                && err.contains("nothing was written"),
            "update, {case}: {err}"
        );
        assert_eq!(
            written_state(root, &cache),
            before,
            "update, {case}: nothing was written"
        );
    }

    fs::create_dir(root.join("existing")).expect("existing dir");
    for (command, dir) in [("update", "existing"), ("update", "fresh/nested")] {
        let output = run_isolated(root, &[command, ".", "--cache-dir", dir]);
        assert!(
            output.status.success(),
            "{command} {dir}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            root.join(dir).join("file_hashes.bin").is_file(),
            "{command} {dir}: the hash index is written"
        );
    }
}

/// A hash index that cannot be written into an accepted `--cache-dir` fails
/// the command before the index is persisted, instead of being logged at a
/// level the CLI does not show while the command exits 0. The write is made
/// to fail by a directory standing where its temporary file goes.
#[test]
fn a_hash_index_that_cannot_be_written_fails_the_command() {
    let project = TempDir::new().expect("project");
    let root = project.path();
    fs::write(root.join("a.rs"), "fn alpha() -> u32 { 1 }\n").expect("a.rs");
    fs::create_dir_all(root.join("cache").join("file_hashes.bin.tmp")).expect("blocker");

    let output = run_isolated(root, &["index", ".", "--cache-dir", "cache"]);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(
        err.contains(
            "--cache-dir cache: the hash index could not be written; the index was not written"
        ),
        "{err}"
    );
    assert!(
        !root
            .join(".sqry")
            .join("graph")
            .join("manifest.json")
            .exists(),
        "{err}"
    );
}
