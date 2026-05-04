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
//! 2. After the build finishes, `<T>/file_hashes.bin` exists (canonical
//!    `HashIndex::save()` filename per
//!    `sqry-core/src/indexing/incremental.rs:405`).
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
