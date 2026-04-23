//! CLI integration tests for `sqry graph resolve`.
//!
//! Indexes a tiny Rust fixture, shells out to the compiled binary, and asserts
//! that both the plain-text and JSON output shapes are correct. The JSON shape
//! (symbol / outcome / bindings / explain.{steps,candidate_count,outcome}) is
//! the documented stable external contract for scripting consumers.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use tempfile::TempDir;

/// Write `contents` to `<dir>/<name>`.
fn write_fixture(dir: &std::path::Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture file");
}

/// Index `dir` with `sqry index`, asserting success.
fn index_dir(dir: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(dir)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `sqry graph resolve bar` prints the plain-text output with `symbol:` and
/// `outcome:` fields for a symbol present in the indexed graph.
#[test]
fn resolve_prints_outcome_for_indexed_symbol() {
    let fixture = TempDir::new().expect("create tempdir");
    write_fixture(fixture.path(), "lib.rs", "pub mod foo { pub fn bar() {} }");
    index_dir(fixture.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(fixture.path())
        .arg("resolve")
        .arg("bar")
        .output()
        .expect("run sqry graph resolve");

    assert!(
        output.status.success(),
        "sqry graph resolve bar failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("symbol: bar"),
        "expected 'symbol: bar' in output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("outcome:"),
        "expected 'outcome:' field in output, got:\n{stdout}"
    );
}

/// `sqry graph resolve hello --explain` appends a `witness trace:` section
/// with numbered steps to the text output.
#[test]
fn resolve_explain_flag_prints_witness_trace() {
    let fixture = TempDir::new().expect("create tempdir");
    write_fixture(fixture.path(), "lib.rs", "pub fn hello() {}");
    index_dir(fixture.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(fixture.path())
        .arg("resolve")
        .arg("hello")
        .arg("--explain")
        .output()
        .expect("run sqry graph resolve --explain");

    assert!(
        output.status.success(),
        "sqry graph resolve hello --explain failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("symbol: hello"),
        "expected 'symbol: hello', got:\n{stdout}"
    );
    assert!(
        stdout.contains("witness trace:"),
        "expected 'witness trace:' section with --explain, got:\n{stdout}"
    );
    // Step 1 must appear — the trace has at least one step.
    assert!(
        stdout.contains("  1."),
        "expected at least one numbered step in witness trace, got:\n{stdout}"
    );
}

/// `sqry graph resolve hello --explain --json` emits a valid JSON document
/// whose top-level shape is the stable external contract:
/// - `symbol` (string)
/// - `outcome` (string — serde repr of `SymbolResolutionOutcome`)
/// - `bindings` (array)
/// - `explain` (object with `steps` and `candidate_count` fields)
#[test]
fn resolve_explain_json_emits_stable_shape() {
    let fixture = TempDir::new().expect("create tempdir");
    write_fixture(fixture.path(), "lib.rs", "pub fn hello() {}");
    index_dir(fixture.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(fixture.path())
        .arg("resolve")
        .arg("hello")
        .arg("--explain")
        .arg("--json")
        .output()
        .expect("run sqry graph resolve --explain --json");

    assert!(
        output.status.success(),
        "sqry graph resolve hello --explain --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    // Top-level stable contract fields.
    assert!(
        parsed.get("symbol").is_some(),
        "JSON must contain 'symbol' field"
    );
    assert!(
        parsed.get("outcome").is_some(),
        "JSON must contain 'outcome' field"
    );
    assert!(
        parsed.get("bindings").is_some() && parsed["bindings"].is_array(),
        "JSON must contain 'bindings' array"
    );

    // `explain` sub-object must be present and non-null when --explain is given.
    let explain = parsed
        .get("explain")
        .expect("JSON must contain 'explain' field");
    assert!(
        !explain.is_null(),
        "explain must be non-null when --explain flag is given"
    );
    assert!(
        explain.get("steps").is_some(),
        "explain object must contain 'steps' field"
    );
    assert!(
        explain.get("candidate_count").is_some(),
        "explain object must contain 'candidate_count' field"
    );
    assert!(
        explain.get("outcome").is_some(),
        "explain object must contain 'outcome' field"
    );
}

/// `sqry graph resolve hello --json` (without `--explain`) emits JSON with
/// a `null` explain field.
#[test]
fn resolve_json_without_explain_has_null_explain() {
    let fixture = TempDir::new().expect("create tempdir");
    write_fixture(fixture.path(), "lib.rs", "pub fn hello() {}");
    index_dir(fixture.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(fixture.path())
        .arg("resolve")
        .arg("hello")
        .arg("--json")
        .output()
        .expect("run sqry graph resolve --json");

    assert!(
        output.status.success(),
        "sqry graph resolve hello --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    let explain = parsed
        .get("explain")
        .expect("JSON must contain 'explain' field");
    assert!(
        explain.is_null(),
        "explain must be null when --explain flag is absent, got: {explain}"
    );
}

/// `sqry graph resolve` for an unindexed symbol does not error (outcome is
/// `NotFound`) and the text output still contains `outcome:`.
#[test]
fn resolve_not_found_symbol_reports_outcome_not_error() {
    let fixture = TempDir::new().expect("create tempdir");
    write_fixture(fixture.path(), "lib.rs", "pub fn hello() {}");
    index_dir(fixture.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(fixture.path())
        .arg("resolve")
        .arg("totally_unknown_symbol_xyz_999")
        .output()
        .expect("run sqry graph resolve (not found)");

    // The command itself succeeds (exit 0) — not-found is an informational
    // outcome, not an error condition.
    assert!(
        output.status.success(),
        "sqry graph resolve for unknown symbol must exit 0, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("outcome:"),
        "expected 'outcome:' in not-found output, got:\n{stdout}"
    );
}
