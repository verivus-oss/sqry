//! Cross-system parity tests: CLI produces consistent results across query scenarios.
//!
//! These tests verify that the `sqry` CLI produces coherent, consistent results when
//! querying the same multi-language fixture project across four key scenarios:
//!
//! 1. **Function query parity** — `kind:function` surfaces expected Rust function names.
//! 2. **Index stats consistency** — `graph stats` reports non-zero node and edge counts.
//! 3. **Search result parity** — `name:process` locates the known `process` function.
//! 4. **Multi-language surfacing** — indexing Rust + Python + TypeScript yields symbols from
//!    all three languages.
//!
//! The fixture under `test-fixtures/e2e-scenarios/multi-lang/` contains:
//! - `src/main.rs`   — Rust binary (`main`)
//! - `src/lib.rs`    — Rust library (`process`, `helper`)
//! - `src/server.py` — Python module (`handle_request`, `transform`, `RequestHandler`)
//! - `src/utils.ts`  — TypeScript module (`formatOutput`, `Formatter`)
//! - `Cargo.toml`    — Minimal Rust package config

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ── helpers ────────────────────────────────────────────────────────────────

fn sqry_cmd() -> Command {
    Command::new(sqry_bin())
}

/// Copy a fixture directory into a fresh `TempDir` so tests do not mutate shared state.
fn copy_fixture(relative: &str) -> TempDir {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root exists")
        .to_path_buf();
    let source = workspace_root.join(relative);
    assert!(
        source.exists(),
        "fixture directory {} not found — expected at {relative}",
        source.display()
    );

    let dest = TempDir::new().expect("create temp dir");
    copy_dir_all(&source, dest.path()).expect("copy fixture into temp dir");
    dest
}

/// Recursive directory copy, normalising CRLF → LF in text files so tree-sitter
/// produces identical byte offsets on all platforms.
fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            let content = fs::read(entry.path())?;
            let normalised: Vec<u8> = if content.contains(&b'\r') {
                content.into_iter().filter(|&b| b != b'\r').collect()
            } else {
                content
            };
            let mut f = fs::File::create(&dest_path)?;
            f.write_all(&normalised)?;
        }
    }
    Ok(())
}

/// Index a project directory with `sqry index --force`.
fn index_project(path: &Path) {
    sqry_cmd()
        .arg("index")
        .arg("--force")
        .arg(path)
        .assert()
        .success();
}

/// Parse stdout bytes as JSON.  Panics with the raw stderr on failure.
fn parse_json(stdout: &[u8], stderr: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON output: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(stdout),
            String::from_utf8_lossy(stderr)
        )
    })
}

/// Extract the set of `name` strings from a JSON query result's `results` array.
fn extract_names(json: &Value) -> BTreeSet<String> {
    json["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| entry["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Fixture path relative to workspace root.
const FIXTURE: &str = "test-fixtures/e2e-scenarios/multi-lang";

// ── scenario 1: function query parity ──────────────────────────────────────

/// Verify that `sqry query "kind:function"` surfaces the expected Rust function names.
///
/// The Rust source files in the fixture define `main`, `process`, and `helper`.
/// All three must appear in the query result set after indexing.
#[test]
fn parity_function_query() {
    let project = copy_fixture(FIXTURE);
    index_project(project.path());

    let output = sqry_cmd()
        .args(["--json", "query", "kind:function"])
        .arg(project.path())
        .output()
        .expect("sqry query kind:function");

    assert!(
        output.status.success(),
        "sqry query kind:function failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json(&output.stdout, &output.stderr);
    let names = extract_names(&json);

    assert!(
        names.contains("main"),
        "expected 'main' in function query results; got: {names:?}"
    );
    assert!(
        names.contains("process"),
        "expected 'process' in function query results; got: {names:?}"
    );
    assert!(
        names.contains("helper"),
        "expected 'helper' in function query results; got: {names:?}"
    );
}

// ── scenario 2: index stats consistency ────────────────────────────────────

/// Verify that `sqry graph stats` reports non-zero node and edge counts after indexing
/// the multi-language fixture.
///
/// A well-formed index of even a minimal project must contain at least one node and
/// at least one structural edge (e.g. `Defines` / `Contains`).
#[test]
fn parity_index_stats() {
    let project = copy_fixture(FIXTURE);
    index_project(project.path());

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .output()
        .expect("sqry graph stats");

    assert!(
        output.status.success(),
        "sqry graph stats failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json(&output.stdout, &output.stderr);

    let node_count = json["node_count"]
        .as_u64()
        .expect("stats JSON must contain 'node_count'");
    assert!(
        node_count > 0,
        "expected non-zero node_count in graph stats; got {node_count}"
    );

    let edge_count = json["edge_count"]
        .as_u64()
        .expect("stats JSON must contain 'edge_count'");
    assert!(
        edge_count > 0,
        "expected non-zero edge_count in graph stats; got {edge_count}"
    );
}

// ── scenario 3: search result name parity ──────────────────────────────────

/// Verify that `sqry query "name:process"` finds the `process` function.
///
/// `process` is defined in `src/lib.rs` (Rust) and in `src/server.py` (Python, as a method).
/// At minimum the Rust-level `process` symbol must appear in the output.
#[test]
fn parity_search_results() {
    let project = copy_fixture(FIXTURE);
    index_project(project.path());

    let output = sqry_cmd()
        .args(["--json", "query", "name:process"])
        .arg(project.path())
        .output()
        .expect("sqry query name:process");

    assert!(
        output.status.success(),
        "sqry query name:process failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains("process"),
        "expected 'process' in search results; stdout:\n{stdout_str}"
    );

    // Verify the result set is non-empty via the structured JSON stats field.
    let json = parse_json(&output.stdout, &output.stderr);
    let total_matches = json["stats"]["total_matches"].as_u64().unwrap_or(0);
    assert!(
        total_matches >= 1,
        "expected at least 1 match for 'name:process'; got {total_matches}"
    );
}

// ── scenario 4: multi-language symbol surfacing ─────────────────────────────

/// Verify that `sqry query "kind:function"` surfaces symbols from all three source
/// languages present in the fixture: Rust, Python, and TypeScript.
///
/// Expected symbols per language:
/// - Rust      → `main`, `process`, `helper`
/// - Python    → `handle_request`, `transform`
/// - TypeScript → `formatOutput`
///
/// The assertion strategy uses sorted-set membership so ordering and count differences
/// between the systems do not cause false failures.
#[test]
fn parity_multi_language() {
    let project = copy_fixture(FIXTURE);
    index_project(project.path());

    let output = sqry_cmd()
        .args(["--json", "query", "kind:function"])
        .arg(project.path())
        .output()
        .expect("sqry query kind:function (multi-language)");

    assert!(
        output.status.success(),
        "sqry query kind:function (multi-language) failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json(&output.stdout, &output.stderr);
    let names = extract_names(&json);

    // Rust functions must be present.
    assert!(
        names.contains("main") || names.contains("process"),
        "expected at least one Rust function ('main' or 'process') in results; got: {names:?}"
    );

    // Python functions must be present (handle_request or transform).
    assert!(
        names.contains("handle_request") || names.contains("transform"),
        "expected at least one Python function ('handle_request' or 'transform') in results; got: {names:?}"
    );

    // TypeScript function must be present.
    assert!(
        names.contains("formatOutput"),
        "expected TypeScript function 'formatOutput' in results; got: {names:?}"
    );
}
