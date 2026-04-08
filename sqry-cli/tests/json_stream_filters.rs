mod common;

use assert_cmd::Command;
use common::sqry_bin;
use std::fs;
use tempfile::tempdir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.env("NO_COLOR", "1");
    cmd
}

/// Build a temp workspace with a Rust file containing a function and a struct,
/// index it, and return the temp dir (kept alive by the caller).
fn build_indexed_workspace() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let src = dir.path().join("lib.rs");
    fs::write(
        &src,
        "pub fn target_function() {}\npub struct TargetStruct {}\n",
    )
    .unwrap();

    sqry_cmd().arg("index").arg(dir.path()).assert().success();

    dir
}

/// Parse NDJSON output into a vec of `serde_json::Value`.
fn parse_ndjson(output: &str) -> Vec<serde_json::Value> {
    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect()
}

fn partial_results(events: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    events
        .iter()
        .filter(|e| e["event"].as_str() == Some("partial_result"))
        .collect()
}

fn final_summaries(events: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    events
        .iter()
        .filter(|e| e["event"].as_str() == Some("final_summary"))
        .collect()
}

#[test]
fn json_stream_respects_kind_filter() {
    let dir = build_indexed_workspace();

    let output = sqry_cmd()
        .args([
            "--fuzzy",
            "--json-stream",
            "--kind",
            "function",
            "search",
            "target",
        ])
        .arg(dir.path())
        .output()
        .expect("command runs");

    assert!(output.status.success(), "exit code should be 0");

    let events = parse_ndjson(&String::from_utf8_lossy(&output.stdout));
    let results = partial_results(&events);
    let summaries = final_summaries(&events);

    assert!(!results.is_empty(), "should have partial results");
    assert_eq!(summaries.len(), 1, "should have exactly one summary");

    // All partial results should be functions (kind filter applied)
    for event in &results {
        let kind = event["result"]["kind"].as_str().unwrap_or("");
        assert_eq!(
            kind.to_lowercase(),
            "function",
            "all results should be functions, got: {kind}"
        );
    }

    // Scores should be real values (not 0.0 placeholder)
    for event in &results {
        let score = event["score"].as_f64().unwrap_or(0.0);
        assert!(score > 0.0, "score should be > 0.0, got: {score}");
    }
}

#[test]
fn json_stream_respects_lang_filter() {
    let dir = build_indexed_workspace();

    // Search with --lang rust should return results (workspace is Rust)
    let output = sqry_cmd()
        .args([
            "--fuzzy",
            "--json-stream",
            "--lang",
            "rust",
            "search",
            "target",
        ])
        .arg(dir.path())
        .output()
        .expect("command runs");

    assert!(output.status.success());

    let events = parse_ndjson(&String::from_utf8_lossy(&output.stdout));
    let results = partial_results(&events);

    assert!(
        !results.is_empty(),
        "should find Rust symbols with --lang rust"
    );

    // Search with --lang python should return zero partial results
    let output = sqry_cmd()
        .args([
            "--fuzzy",
            "--json-stream",
            "--lang",
            "python",
            "search",
            "target",
        ])
        .arg(dir.path())
        .output()
        .expect("command runs");

    assert!(output.status.success());

    let events = parse_ndjson(&String::from_utf8_lossy(&output.stdout));
    let results = partial_results(&events);

    assert!(
        results.is_empty(),
        "should find no Python symbols in a Rust-only workspace"
    );
}
