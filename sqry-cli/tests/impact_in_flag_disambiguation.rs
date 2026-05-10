//! Integration tests for `sqry impact --in <file>` disambiguation.
//!
//! Covers verivus-oss/sqry#214: when N candidates share a `qualified_name`
//! (e.g., 11 plain-C functions named `do_exit` in 11 files), the only thing
//! that distinguishes them is the file each is defined in. The CLI must
//! expose a `--in <file>` flag (the counterpart to the MCP
//! `dependency_impact.file_path` argument), the ambiguity error must point
//! the operator at it, and passing the flag must resolve to the candidate in
//! that file.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// Two C files that each define a function named `do_thing`. Plain C has
/// no namespacing, so both nodes land in the resolver under the same
/// simple AND qualified name — the exact shape that broke #214 in the
/// kernel reproduction.
fn build_two_file_collision_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("file_a.c"),
        r"int do_thing(void) { return 1; }
int caller_a(void) { return do_thing(); }
",
    )
    .unwrap();
    fs::write(
        dir.path().join("file_b.c"),
        r"int do_thing(void) { return 2; }
int caller_b(void) { return do_thing(); }
",
    )
    .unwrap();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["index", "."])
        .assert()
        .success();
    dir
}

#[test]
fn impact_ambiguity_message_mentions_in_flag_and_sample_file() {
    let dir = build_two_file_collision_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "--json", "do_thing"])
        .assert()
        .code(4);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let envelope: Value =
        serde_json::from_str(&stdout).expect("ambiguous envelope must be valid JSON");

    let error = &envelope["error"];
    assert_eq!(error["code"], "sqry::ambiguous_symbol");
    let message = error["message"].as_str().expect("message is a string");
    assert!(
        message.contains("`--in <file>`"),
        "message must point at the --in flag, got {message:?}"
    );
    assert!(
        message.contains("e.g., `--in ")
            && (message.contains("file_a.c`") || message.contains("file_b.c`")),
        "message must include a concrete --in <file> example built from a candidate, \
         got {message:?}"
    );

    // Both files must show up in the candidate list — that's the data the
    // operator is supposed to read to choose `--in`.
    let candidates = error["candidates"].as_array().unwrap();
    let files: Vec<&str> = candidates
        .iter()
        .filter_map(|c| c["file_path"].as_str())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("file_a.c"))
            && files.iter().any(|f| f.ends_with("file_b.c")),
        "candidate list must surface both file paths, got {files:?}"
    );
}

#[test]
fn impact_in_flag_resolves_to_the_candidate_in_that_file() {
    let dir = build_two_file_collision_fixture();
    for fname in ["file_a.c", "file_b.c"] {
        let assert = Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["impact", "--json", "do_thing", "--in", fname])
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        let payload: Value = serde_json::from_str(&stdout).expect("success payload is JSON");
        assert_eq!(payload["symbol"], "do_thing");
        assert!(
            payload.get("direct").is_some(),
            "{fname}: expected ImpactOutput.direct, got {payload:?}"
        );
    }
}

#[test]
fn impact_in_flag_with_unknown_file_reports_not_found() {
    let dir = build_two_file_collision_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "do_thing", "--in", "nope_does_not_exist.c"])
        .assert()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("No definition of 'do_thing' found in file 'nope_does_not_exist.c'"),
        "expected file-scoped not-found message, got {stderr:?}"
    );
}
