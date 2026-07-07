//! Regression coverage for verivus-oss/sqry#520.
//!
//! `sqry update --stats` used to be accepted but was a no-op: it printed
//! "(Detailed stats are not available for unified graph update)" and nothing
//! else. The unified update path rebuilds the whole graph and produces a
//! `BuildResult` full of real metrics, so `--stats` now emits genuine
//! statistics (node / edge counts, registered + workspace file counts,
//! per-language breakdown, threads, active plugins, elapsed) plus the signed
//! delta this update produced against the previous snapshot. It honours
//! `--json` by emitting a single JSON document on stdout.
//!
//! The file delta is header-to-header (registered files on both sides, which
//! includes external / dependency files) so it stays apples-to-apples on
//! classpath / external workspaces. The workspace (non-external) file count is
//! reported separately as an absolute. The exact external-vs-workspace delta
//! semantics are pinned by the deterministic unit tests in
//! `sqry-cli/src/commands/index.rs` (`update_stats_file_delta_is_registered_apples_to_apples`);
//! these end-to-end tests exercise the real CLI surface on a workspace-only
//! fixture where registered == workspace.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

fn write_initial_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub struct GrokClient {
    pub id: u32,
}

pub fn main_entry() {
    let _ = GrokClient { id: 0 };
}
",
    )
    .unwrap();
}

fn index(root: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

/// `sqry update <dir> --stats` must emit real statistics, not the old no-op
/// line. This is the exact invocation from #520.
#[test]
fn update_stats_emits_real_statistics() {
    let temp = TempDir::new().unwrap();
    write_initial_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("update")
        .arg(temp.path())
        .arg("--stats")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`update --stats` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("not available for unified graph update"),
        "the old no-op stats message must be gone; got: {stdout}"
    );
    assert!(
        stdout.contains("Update statistics:"),
        "real stats block must be present; got: {stdout}"
    );
    assert!(
        stdout.contains("Nodes:") && stdout.contains("Canonical edges:"),
        "stats must report node and edge counts; got: {stdout}"
    );
    assert!(
        stdout.contains("Registered files:") && stdout.contains("Workspace files:"),
        "stats must report both registered and workspace file counts; got: {stdout}"
    );
    assert!(
        stdout.contains("Files by language (workspace):"),
        "stats must include a per-language workspace breakdown; got: {stdout}"
    );
}

/// After adding a file, `--stats` must report a positive delta against the
/// previous snapshot. On this workspace-only fixture (no external files) the
/// registered-file delta equals the added-file count.
#[test]
fn update_stats_reports_deltas_after_change() {
    let temp = TempDir::new().unwrap();
    write_initial_fixture(temp.path());
    index(temp.path());

    // Add a brand-new file so the graph gains nodes and one file.
    fs::write(
        temp.path().join("extra.rs"),
        r"
pub fn added_one() {}
pub fn added_two() {}
pub struct AddedStruct;
",
    )
    .unwrap();

    let output = Command::new(sqry_bin())
        .arg("update")
        .arg(temp.path())
        .arg("--stats")
        .output()
        .expect("command failed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("since last index"),
        "text stats must report a delta column; got: {stdout}"
    );
    // The added file must show up as a positive registered-file delta (no
    // externals here, so registered delta == added workspace files).
    assert!(
        stdout.contains("Registered files:") && stdout.contains("(+1 since last index)"),
        "adding one file must be reported as a +1 registered-file delta; got: {stdout}"
    );
}

/// `sqry update <dir> --stats --json` must emit a SINGLE JSON document on
/// stdout with no leading progress chatter (that is routed to stderr). The test
/// parses stdout directly, with no pre-stripping.
#[test]
fn update_stats_json_is_a_single_document_on_stdout() {
    let temp = TempDir::new().unwrap();
    write_initial_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("update")
        .arg(temp.path())
        .arg("--stats")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`update --stats --json` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse the WHOLE stdout as one JSON document. No pre-stripping: this fails
    // if any human-readable line leaks onto stdout.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout must be a single JSON document, got: {stdout:?}\nparse error: {e}")
    });
    // The progress line must have gone to stderr instead.
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Updating index for"),
        "the progress line must be routed to stderr under --json"
    );

    let stats = parsed
        .get("update_stats")
        .unwrap_or_else(|| panic!("missing update_stats object, got {parsed}"));
    for field in [
        "nodes",
        "nodes_delta",
        "canonical_edges",
        "canonical_edges_delta",
        "raw_edges",
        "workspace_files_indexed",
        "registered_files",
        "registered_files_delta",
        "files_by_language",
        "threads_used",
        "active_plugins",
        "built_at",
        "elapsed_seconds",
    ] {
        assert!(
            stats.get(field).is_some(),
            "update_stats JSON must expose `{field}`, got {stats}"
        );
    }
    assert!(
        stats["nodes"].as_u64().unwrap_or(0) > 0,
        "node count must be positive, got {stats}"
    );
    // The registered-file delta must NOT be the old apples-to-oranges wrong
    // number. On a fresh re-index with no file change it is 0.
    assert_eq!(
        stats["registered_files_delta"].as_i64(),
        Some(0),
        "re-indexing unchanged sources must report a zero registered-file delta, got {stats}"
    );
}

/// `sqry update <dir> --stats --json --classpath` must ALSO be a single JSON
/// document on stdout. The classpath pipeline prints its own progress
/// ("Running JVM classpath analysis...", "Classpath: N JARs scanned", "Graph
/// enriched ...") which used to go to stdout unconditionally and broke the
/// single-document contract on JVM projects (#524 round-3 blocker). Those lines
/// are now routed to stderr under `--json`. `jvm-classpath` is a default
/// feature, so `--classpath` triggers the pipeline (here it detects no JVM build
/// system and skips, but the "Running JVM classpath analysis..." line still
/// fires before that check, which is exactly the leak we are guarding against).
#[test]
fn update_stats_json_classpath_is_a_single_document_on_stdout() {
    let temp = TempDir::new().unwrap();
    write_initial_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("update")
        .arg(temp.path())
        .arg("--stats")
        .arg("--json")
        .arg("--classpath")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`update --stats --json --classpath` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse the WHOLE stdout as one JSON document, no pre-stripping. This fails
    // if any classpath progress line leaks onto stdout.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout must be a single JSON document even with --classpath, got: {stdout:?}\nparse error: {e}"
        )
    });
    assert!(
        parsed.get("update_stats").is_some(),
        "update_stats object must be present, got {parsed}"
    );
    // The classpath chatter must have landed on stderr. Both the feature-on
    // ("Running JVM classpath analysis...") and feature-off ("requires the
    // 'jvm-classpath' feature") paths mention "classpath", so this assertion is
    // robust across feature configs; the point is that it is NOT on stdout.
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("classpath"),
        "classpath progress/warning must be routed to stderr, not stdout; stderr was: {stderr}"
    );
}
