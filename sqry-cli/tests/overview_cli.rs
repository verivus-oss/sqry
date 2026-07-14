//! Integration tests for `sqry overview` and the `sqry graph
//! hubs/subsystems/communities` subcommands.
//!
//! These index a copy of the committed `test-fixtures/sqry-overview` corpus in
//! a tempdir (so the committed fixture is never polluted with a `.sqry/`
//! index), then drive the real `sqry` binary. The report path is deterministic,
//! so the JSON is byte-stable across runs and the Markdown matches a checked-in
//! golden.
//!
//! Regenerating the golden (only after an intentional report/indexer change):
//!   sqry overview <indexed-copy> --redaction relative --sections hubs,subsystems \
//!     > test-fixtures/sqry-overview/golden/hubs-subsystems.relative.md

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Path to the committed fixture source tree.
fn fixture_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test-fixtures/sqry-overview")
}

/// Recursively copy `src` into `dst`, skipping any `golden/` subtree and any
/// pre-existing `.sqry` index.
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "golden" || name == ".sqry" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// Copy the fixture into a fresh tempdir and index it in place.
fn build_indexed_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    copy_tree(&fixture_src(), dir.path());
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["index", "."])
        .assert()
        .success();
    dir
}

fn run_overview(dir: &TempDir, extra: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(sqry_bin());
    cmd.current_dir(dir).arg("overview").arg(".");
    for a in extra {
        cmd.arg(a);
    }
    cmd.assert()
}

// -------------------------------------------------------------------------
// Determinism / golden
// -------------------------------------------------------------------------

#[test]
fn overview_json_is_byte_stable_across_runs() {
    let dir = build_indexed_fixture();
    let first = run_overview(&dir, &["--format", "json"])
        .success()
        .get_output()
        .stdout
        .clone();
    let second = run_overview(&dir, &["--format", "json"])
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        first, second,
        "overview --format json must be byte-stable across runs"
    );
    // Sanity: it is real JSON with the expected top-level sections.
    let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
    for section in [
        "summary",
        "hubs",
        "subsystems",
        "hotspots",
        "issues",
        "suggested_questions",
    ] {
        assert!(
            value.get(section).is_some(),
            "missing section {section} in JSON"
        );
    }
}

#[test]
fn overview_markdown_matches_golden() {
    let dir = build_indexed_fixture();
    let out = run_overview(
        &dir,
        &["--redaction", "relative", "--sections", "hubs,subsystems"],
    )
    .success()
    .get_output()
    .stdout
    .clone();
    let actual = String::from_utf8(out).unwrap();
    let golden_path = fixture_src().join("golden/hubs-subsystems.relative.md");
    let golden = fs::read_to_string(&golden_path).unwrap();
    assert_eq!(
        actual,
        golden,
        "overview markdown drifted from the checked-in golden ({}). If this is an \
         intentional report change, regenerate the golden per the module header.",
        golden_path.display()
    );
}

// -------------------------------------------------------------------------
// Flags
// -------------------------------------------------------------------------

#[test]
fn sections_subset_emits_only_named_sections() {
    let dir = build_indexed_fixture();
    let out = run_overview(&dir, &["--format", "json", "--sections", "hubs,issues"])
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(value.get("hubs").is_some(), "hubs must be present");
    assert!(value.get("issues").is_some(), "issues must be present");
    assert!(value.get("summary").is_none(), "summary must be omitted");
    assert!(
        value.get("subsystems").is_none(),
        "subsystems must be omitted"
    );
    assert!(value.get("hotspots").is_none(), "hotspots must be omitted");
    assert!(
        value.get("suggested_questions").is_none(),
        "questions must be omitted"
    );
}

#[test]
fn top_bounds_ranked_rows() {
    let dir = build_indexed_fixture();
    let out = run_overview(
        &dir,
        &["--format", "json", "--sections", "hubs", "--top", "3"],
    )
    .success()
    .get_output()
    .stdout
    .clone();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let hubs = value.get("hubs").and_then(|h| h.as_array()).unwrap();
    assert!(
        hubs.len() <= 3,
        "--top 3 must bound hubs to 3 rows, got {}",
        hubs.len()
    );
}

#[test]
fn no_index_fails_on_unindexed_repo() {
    // Fresh source-only tree with no `.sqry` index.
    let dir = TempDir::new().unwrap();
    copy_tree(&fixture_src(), dir.path());
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["overview", ".", "--no-index"])
        .assert()
        .failure();
}

#[test]
fn output_writes_file_and_keeps_stdout_silent() {
    let dir = build_indexed_fixture();
    let out_file = dir.path().join("REPORT.md");
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["overview", ".", "--output"])
        .arg(&out_file)
        .assert()
        .success();
    let stdout = &assert.get_output().stdout;
    assert!(
        stdout.is_empty(),
        "--output must keep stdout silent, got {} bytes",
        stdout.len()
    );
    let written = fs::read_to_string(&out_file).unwrap();
    assert!(
        written.contains("# Repository overview"),
        "the report file must contain the rendered report"
    );
}

// -------------------------------------------------------------------------
// Redaction
// -------------------------------------------------------------------------

#[test]
fn default_redaction_never_emits_workspace_root_prefix() {
    let dir = build_indexed_fixture();
    // The canonical workspace-root prefix must never appear under the default
    // (minimal) preset, in any format.
    let root = fs::canonicalize(dir.path()).unwrap();
    let root_str = root.to_string_lossy().into_owned();
    for format in ["md", "json", "text"] {
        let out = run_overview(&dir, &["--format", format])
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains(&root_str),
            "format {format}: default redaction leaked the workspace-root path prefix {root_str}"
        );
    }
}

#[test]
fn none_redaction_reveals_absolute_paths() {
    let dir = build_indexed_fixture();
    let root = fs::canonicalize(dir.path()).unwrap();
    let root_str = root.to_string_lossy().into_owned();
    let out = run_overview(&dir, &["--format", "json", "--redaction", "none"])
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(&root_str),
        "the `none` preset must reveal raw absolute paths for trusted local use"
    );
}

// -------------------------------------------------------------------------
// graph hubs / subsystems / communities subcommands
// -------------------------------------------------------------------------

#[test]
fn graph_hubs_subcommand_runs() {
    let dir = build_indexed_fixture();
    let out = Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "graph", "--path", ".", "hubs", "--top", "3", "--by", "combined",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("Load-bearing hubs"),
        "hubs header expected: {text}"
    );
}

#[test]
fn graph_hubs_json_is_stable() {
    let dir = build_indexed_fixture();
    let run = || {
        Command::new(sqry_bin())
            .current_dir(&dir)
            .args(["graph", "--path", ".", "hubs", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    assert_eq!(run(), run(), "graph hubs --format json must be byte-stable");
}

#[test]
fn graph_subsystems_subcommand_runs() {
    let dir = build_indexed_fixture();
    let out = Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "graph",
            "--path",
            ".",
            "subsystems",
            "--group-depth",
            "2",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(value.get("subsystems").is_some());
    assert!(value.get("couplings").is_some());
}

#[test]
fn graph_communities_subcommand_runs() {
    let dir = build_indexed_fixture();
    // The fixture has no cross-file coupling, so the partition is empty; the
    // command must still succeed and print the deterministic header.
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["graph", "--path", ".", "communities", "--resolution", "1.0"])
        .assert()
        .success();
}

#[test]
fn graph_communities_rejects_nonpositive_resolution() {
    let dir = build_indexed_fixture();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["graph", "--path", ".", "communities", "--resolution", "0"])
        .assert()
        .failure();
}
