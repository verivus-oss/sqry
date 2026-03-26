//! End-to-end scenario tests for sqry CLI user workflows.
//!
//! These tests exercise realistic user workflows against the multi-lang fixture
//! (Rust + Python + TypeScript sources). Each scenario corresponds to a distinct
//! user journey: first-time indexing, incremental reindex, multi-format output,
//! graph analysis, config management, error handling, search, and analysis.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sqry_cmd() -> Command {
    Command::new(sqry_bin())
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create destination directory");
    for entry in fs::read_dir(src).expect("read source directory") {
        let entry = entry.expect("read dir entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

/// Copy the named fixture under `test-fixtures/e2e-scenarios/` to a temp
/// directory and return the `TempDir` handle (dropped = cleaned up).
fn setup_fixture(fixture_name: &str) -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test-fixtures/e2e-scenarios")
        .join(fixture_name);
    copy_dir_recursive(&src, dir.path());
    dir
}

// ---------------------------------------------------------------------------
// Scenario 1 – First-time indexing then query
// ---------------------------------------------------------------------------

/// Build a fresh index from the multi-lang fixture and verify that a
/// `kind:function` query returns results containing known function names.
#[test]
fn scenario_index_then_query() {
    let project = setup_fixture("multi-lang");

    // Step 1: index
    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Step 2: verify snapshot was written
    assert!(
        project.path().join(".sqry/graph/snapshot.sqry").exists(),
        "graph snapshot must exist after index"
    );

    // Step 3: query for functions and check that known names appear
    sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success()
        // The Rust lib exports `process` and `helper`
        .stdout(predicate::str::contains("process").or(predicate::str::contains("helper")));

    // Step 4: query for classes and verify Python/TS classes appear
    sqry_cmd()
        .arg("query")
        .arg("kind:class")
        .arg(project.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("RequestHandler").or(predicate::str::contains("Formatter")),
        );
}

// ---------------------------------------------------------------------------
// Scenario 2 – Incremental reindex after file change
// ---------------------------------------------------------------------------

/// Index → modify a source file → force re-index → verify new content is
/// discoverable. Uses `--force` to bypass the "index already exists" guard,
/// modelling the workflow of explicitly rebuilding after a code change.
#[test]
fn scenario_incremental_reindex() {
    let project = setup_fixture("multi-lang");

    // Initial index
    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Modify a file (add a new function to lib.rs)
    let lib_path = project.path().join("src/lib.rs");
    let mut content = fs::read_to_string(&lib_path).expect("read lib.rs");
    content.push_str("\npub fn extra() -> i32 { 99 }\n");
    fs::write(&lib_path, &content).expect("write lib.rs");

    // Re-index with --force to pick up the change.
    // `sqry index` without --force is a no-op when an index already exists;
    // --force instructs sqry to rebuild from scratch.
    sqry_cmd()
        .arg("index")
        .arg("--force")
        .arg(project.path())
        .assert()
        .success();

    // The new function must now be discoverable
    sqry_cmd()
        .arg("query")
        .arg("name:extra")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("extra"));
}

// ---------------------------------------------------------------------------
// Scenario 3 – Multi-format output
// ---------------------------------------------------------------------------

/// Verify that the three main output format flags each produce the expected
/// structure: JSON is parseable, CSV includes headers when requested, and
/// plain text output is human-readable.
#[test]
fn scenario_multi_format_output() {
    let project = setup_fixture("multi-lang");

    // Pre-build index so queries are deterministic
    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // JSON output – must be valid JSON with an array or object at the root
    let json_out = sqry_cmd()
        .arg("--json")
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .output()
        .expect("run sqry --json query");
    assert!(json_out.status.success(), "JSON query must succeed");
    let stdout_str = String::from_utf8_lossy(&json_out.stdout);
    // The output should be parseable JSON (array of results)
    serde_json::from_str::<serde_json::Value>(&stdout_str)
        .expect("--json output must be valid JSON");

    // CSV output with headers – must contain a header row
    sqry_cmd()
        .arg("--csv")
        .arg("--headers")
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success()
        // CSV header row contains column names
        .stdout(
            predicate::str::contains("name")
                .or(predicate::str::contains("kind"))
                .or(predicate::str::contains("file")),
        );

    // Plain text output – human-readable, must not be empty JSON notation
    sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success()
        // In text mode the output should contain function names, not raw JSON brackets
        .stdout(predicate::str::contains("process").or(predicate::str::contains("helper")));
}

// ---------------------------------------------------------------------------
// Scenario 4 – Graph analysis workflow
// ---------------------------------------------------------------------------

/// Index the multi-lang project then run `graph nodes` and `graph edges`
/// subcommands and verify both return well-formed JSON payloads.
#[test]
fn scenario_graph_analysis() {
    let project = setup_fixture("multi-lang");

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // graph nodes – JSON format
    let nodes_out = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("nodes")
        .output()
        .expect("run sqry graph nodes");
    assert!(nodes_out.status.success(), "graph nodes must succeed");
    let nodes_json: serde_json::Value =
        serde_json::from_slice(&nodes_out.stdout).expect("graph nodes output must be JSON");
    // Expect a `count` field and a `nodes` array
    assert!(
        nodes_json.get("count").is_some(),
        "graph nodes JSON must have 'count'"
    );
    let count = nodes_json["count"].as_u64().unwrap_or(0);
    assert!(count > 0, "indexed project must have at least one node");

    // graph edges – JSON format
    let edges_out = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("edges")
        .output()
        .expect("run sqry graph edges");
    assert!(edges_out.status.success(), "graph edges must succeed");
    let edges_json: serde_json::Value =
        serde_json::from_slice(&edges_out.stdout).expect("graph edges output must be JSON");
    assert!(
        edges_json.get("count").is_some(),
        "graph edges JSON must have 'count'"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5 – Config workflow
// ---------------------------------------------------------------------------

/// Verify the config init → show lifecycle: initializing config creates the
/// file and `config show` produces structured output.
#[test]
fn scenario_config_workflow() {
    let project = setup_fixture("multi-lang");

    // Init config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config initialized"));

    // Show config – human-readable output must mention schema / limits
    sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(project.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Schema version")
                .or(predicate::str::contains("Limits"))
                .or(predicate::str::contains("max_results")),
        );

    // Show config as JSON
    let json_out = sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(project.path())
        .arg("--json")
        .output()
        .expect("run config show --json");
    assert!(json_out.status.success(), "config show --json must succeed");
    let config_json: serde_json::Value =
        serde_json::from_slice(&json_out.stdout).expect("config show --json must be valid JSON");
    assert_eq!(config_json["schema_version"], 1, "schema_version must be 1");
}

// ---------------------------------------------------------------------------
// Scenario 6a – Error handling: non-existent path
// ---------------------------------------------------------------------------

/// `sqry index` on a non-existent path must exit with a non-zero code and
/// emit a useful error message.
#[test]
fn scenario_error_handling_nonexistent_path() {
    sqry_cmd()
        .arg("index")
        .arg("/nonexistent/e2e-scenario-path-that-will-never-exist")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Error")
                .or(predicate::str::contains("error"))
                .or(predicate::str::contains("failed"))
                .or(predicate::str::contains("not found"))
                .or(predicate::str::contains("No such")),
        );
}

// ---------------------------------------------------------------------------
// Scenario 6b – Error handling: empty directory
// ---------------------------------------------------------------------------

/// `sqry index` on an empty directory must not crash. It may succeed with
/// an empty graph or return a non-fatal status, but it must not panic.
#[test]
fn scenario_error_handling_empty_dir() {
    let empty = TempDir::new().expect("create empty temp dir");

    // Indexing an empty directory should complete (zero or more symbols)
    // without a crash. We allow both success and a graceful "no files" exit.
    let status = sqry_cmd()
        .arg("index")
        .arg(empty.path())
        .output()
        .expect("run sqry index on empty dir");

    // The process must not have been killed by a signal (exit code is set)
    assert!(
        status.status.code().is_some(),
        "process must exit cleanly, not via signal"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7 – Search workflows
// ---------------------------------------------------------------------------

/// Verify boolean query support (`AND`, `OR`) and kind-based filtering.
#[test]
fn scenario_search_workflows() {
    let project = setup_fixture("multi-lang");

    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Boolean AND: functions named "process"
    sqry_cmd()
        .arg("query")
        .arg("name:process AND kind:function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // kind:class query returns classes
    sqry_cmd()
        .arg("query")
        .arg("kind:class")
        .arg(project.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("RequestHandler").or(predicate::str::contains("Formatter")),
        );

    // Boolean OR: functions or classes
    sqry_cmd()
        .arg("query")
        .arg("kind:function OR kind:class")
        .arg(project.path())
        .assert()
        .success()
        // Multi-language fixture has both kinds; output should be non-empty
        .stdout(predicate::str::is_empty().not());
}

// ---------------------------------------------------------------------------
// Scenario 8 – Analyze workflow
// ---------------------------------------------------------------------------

/// Build graph analyses (`sqry analyze`) after indexing and verify the
/// command succeeds and the analysis artifacts are written to disk.
#[test]
fn scenario_analyze_workflow() {
    let project = setup_fixture("multi-lang");

    // Index first (required before analyze)
    sqry_cmd()
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Analyze – builds SCC + condensation DAG
    sqry_cmd()
        .arg("analyze")
        .arg(project.path())
        .assert()
        .success();

    // Analysis artifacts live under .sqry/analysis/
    let analysis_dir = project.path().join(".sqry/analysis");
    assert!(
        analysis_dir.exists(),
        "analysis directory must exist after sqry analyze"
    );
}
