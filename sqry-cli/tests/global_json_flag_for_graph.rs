//! Regression coverage for verivus-oss/sqry#79 / verivus-oss/sqry#158.
//!
//! The global `--json` flag is documented as a top-level output-format
//! switch on the `Cli` struct (`sqry-cli/src/args/mod.rs`). Prior to this
//! fix, `--json` was silently ignored on every `graph *` subcommand
//! because the graph dispatcher in `sqry-cli/src/main.rs` only forwarded
//! the graph-level `--format` string into `run_graph` and never consulted
//! `cli.json`. The bug was reported against `graph direct-callers` but
//! affects every graph subcommand.
//!
//! These tests lock the post-fix contract:
//!
//!   1. `sqry --json graph direct-callers <symbol>` produces JSON output.
//!   2. `sqry graph direct-callers <symbol> --json` produces JSON output
//!      (clap promotes the global `--json` past the subcommand boundary).
//!   3. `sqry graph --format json direct-callers <symbol>` produces JSON
//!      output (the explicit per-graph `--format` path still works).
//!   4. `sqry graph --format text direct-callers <symbol> --json` errors
//!      out loudly with a message naming both `--format` and `--json` so
//!      the user can see exactly which two flags are in conflict.
//!
//! The third invocation locks the pre-existing path (regression guard so
//! the dispatcher's new precedence logic does not break it). The fourth
//! invocation locks the conflict diagnostic so callers cannot silently
//! get text output when they asked for both.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Tiny Rust fixture: `fetch` and `process` both call `helper`, so
/// `direct-callers helper` resolves and returns a non-empty caller set.
/// Mirrors the fixture used by `migration_golden_cli_test.rs` so we do
/// not depend on the BadLiveware Go fixture (created by the concurrent
/// `A_GO_SPANS` unit) and remain a stand-alone CLI-surface regression.
fn write_simple_callers_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub fn helper() -> i32 {
    42
}

pub fn fetch() -> i32 {
    helper()
}

pub fn process() -> i32 {
    helper()
}
",
    )
    .unwrap();
}

/// Build a fresh `.sqry/` index for the fixture so the graph commands
/// have a snapshot to load.
fn index(root: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

/// `sqry --json graph direct-callers <symbol>` — global `--json` set
/// **before** the `graph` subcommand must reach the graph renderer.
#[test]
fn global_json_before_graph_threads_into_direct_callers_renderer() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("direct-callers")
        .arg("helper")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "command failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nparse error: {e}"));
    assert_eq!(parsed["symbol"], "helper");
    assert!(parsed["callers"].is_array());
}

/// `sqry graph direct-callers <symbol> --json` — global `--json` set
/// **after** the leaf subcommand must also reach the renderer (clap's
/// `global = true` propagates the flag through every subcommand layer).
#[test]
fn global_json_after_subcommand_threads_into_direct_callers_renderer() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("direct-callers")
        .arg("helper")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "command failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nparse error: {e}"));
    assert_eq!(parsed["symbol"], "helper");
    assert!(parsed["callers"].is_array());
}

/// `sqry graph --format json direct-callers <symbol>` — explicit
/// per-graph `--format json` continues to produce JSON. Regression guard
/// against the new dispatch precedence accidentally suppressing the
/// pre-existing `--format` path.
#[test]
fn explicit_format_json_still_produces_json() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("direct-callers")
        .arg("helper")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "command failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nparse error: {e}"));
    assert_eq!(parsed["symbol"], "helper");
}

/// Run all three documented invocation orders against an arbitrary
/// `graph *` subcommand and assert the output is parseable JSON every
/// time. Each row is then asserted against `assert_json` so callers can
/// pin a stable structural property (e.g. a top-level field).
///
/// Locks the alias contract for the gemini iter-1 BLOCK on `D_JSON_THREAD`:
/// the `--format json` order must produce JSON for `provenance`,
/// `resolve`, and `status` exactly the same way the `--json` orders do.
fn assert_all_three_orders_produce_json<F>(
    fixture_root: &std::path::Path,
    subcommand_args: &[&str],
    assert_json: F,
) where
    F: Fn(&serde_json::Value, &str),
{
    // Order 1: `sqry graph <sub> ... --json`
    let order_1 = {
        let mut cmd = Command::new(sqry_bin());
        cmd.arg("graph").arg("--path").arg(fixture_root);
        for arg in subcommand_args {
            cmd.arg(arg);
        }
        cmd.arg("--json").output().expect("command failed")
    };
    assert!(
        order_1.status.success(),
        "order 1 (`graph <sub> ... --json`) failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&order_1.stdout),
        String::from_utf8_lossy(&order_1.stderr),
    );
    let stdout_1 = String::from_utf8_lossy(&order_1.stdout).to_string();
    let parsed_1: serde_json::Value = serde_json::from_str(&stdout_1)
        .unwrap_or_else(|e| panic!("order 1 expected JSON, got: {stdout_1}\nparse error: {e}"));
    assert_json(&parsed_1, "order 1 (`graph <sub> ... --json`)");

    // Order 2: `sqry --json graph <sub> ...`
    let order_2 = {
        let mut cmd = Command::new(sqry_bin());
        cmd.arg("--json")
            .arg("graph")
            .arg("--path")
            .arg(fixture_root);
        for arg in subcommand_args {
            cmd.arg(arg);
        }
        cmd.output().expect("command failed")
    };
    assert!(
        order_2.status.success(),
        "order 2 (`--json graph <sub> ...`) failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&order_2.stdout),
        String::from_utf8_lossy(&order_2.stderr),
    );
    let stdout_2 = String::from_utf8_lossy(&order_2.stdout).to_string();
    let parsed_2: serde_json::Value = serde_json::from_str(&stdout_2)
        .unwrap_or_else(|e| panic!("order 2 expected JSON, got: {stdout_2}\nparse error: {e}"));
    assert_json(&parsed_2, "order 2 (`--json graph <sub> ...`)");

    // Order 3: `sqry graph --format json <sub> ...`
    let order_3 = {
        let mut cmd = Command::new(sqry_bin());
        cmd.arg("graph")
            .arg("--path")
            .arg(fixture_root)
            .arg("--format")
            .arg("json");
        for arg in subcommand_args {
            cmd.arg(arg);
        }
        cmd.output().expect("command failed")
    };
    assert!(
        order_3.status.success(),
        "order 3 (`graph --format json <sub> ...`) failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&order_3.stdout),
        String::from_utf8_lossy(&order_3.stderr),
    );
    let stdout_3 = String::from_utf8_lossy(&order_3.stdout).to_string();
    let parsed_3: serde_json::Value = serde_json::from_str(&stdout_3)
        .unwrap_or_else(|e| panic!("order 3 expected JSON, got: {stdout_3}\nparse error: {e}"));
    assert_json(&parsed_3, "order 3 (`graph --format json <sub> ...`)");
}

/// `provenance` honors all three invocation orders. Locks the gemini
/// iter-1 BLOCK fix in dispatch arm `GraphOperation::Provenance`.
#[test]
fn provenance_honors_all_three_invocation_orders() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    assert_all_three_orders_produce_json(
        temp.path(),
        &["provenance", "helper"],
        |parsed, label| {
            assert!(
                parsed.get("fact_epoch").is_some(),
                "{label}: provenance JSON should expose `fact_epoch`, got {parsed}"
            );
            assert!(
                parsed.get("nodes").is_some(),
                "{label}: provenance JSON should expose `nodes`, got {parsed}"
            );
        },
    );
}

/// `resolve` honors all three invocation orders. Locks the gemini iter-1
/// BLOCK fix in dispatch arm `GraphOperation::Resolve`.
#[test]
fn resolve_honors_all_three_invocation_orders() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    assert_all_three_orders_produce_json(temp.path(), &["resolve", "helper"], |parsed, label| {
        assert!(
            parsed.is_object() || parsed.is_array(),
            "{label}: resolve output should be a JSON value, got {parsed}"
        );
    });
}

/// `status` honors all three invocation orders. Locks the gemini iter-1
/// BLOCK fix in `run_graph_status_with_format`.
#[test]
fn status_honors_all_three_invocation_orders() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    assert_all_three_orders_produce_json(temp.path(), &["status"], |parsed, label| {
        assert!(
            parsed.is_object(),
            "{label}: status output should be a JSON object, got {parsed}"
        );
    });
}

/// `sqry graph --format text provenance helper --json` — same conflict
/// contract as `direct-callers` but for the now-fixed `provenance` arm.
/// Confirms the conflict detection at `resolve_graph_format` still fires
/// for subcommands whose dispatch arm now ORs `*json` with
/// `format == "json"`.
#[test]
fn provenance_conflicting_format_text_and_json_errors_loudly() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("text")
        .arg("provenance")
        .arg("helper")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        !output.status.success(),
        "expected nonzero exit, got success. stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("--json") && combined.contains("--format"),
        "diagnostic must name both flags: {combined}"
    );
}

/// `sqry graph --format text direct-callers <symbol> --json` — user
/// asked for two conflicting output modes. The dispatcher must reject
/// this with a diagnostic that names both flags so the user can fix it.
#[test]
fn conflicting_format_text_and_json_errors_loudly() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("text")
        .arg("direct-callers")
        .arg("helper")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        !output.status.success(),
        "expected nonzero exit, got success. stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("--json"),
        "diagnostic must name --json: {combined}"
    );
    assert!(
        combined.contains("--format"),
        "diagnostic must name --format: {combined}"
    );
}
