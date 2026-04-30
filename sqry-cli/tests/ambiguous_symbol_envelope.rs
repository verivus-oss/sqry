//! Integration tests for the `sqry::ambiguous_symbol` CLI envelope.
//!
//! Covers the C_AMBIGUOUS DAG unit: when a bare name resolves to multiple
//! nodes, `sqry impact` and `sqry explain` must surface a typed ambiguity
//! envelope (JSON code `sqry::ambiguous_symbol`) and exit with the
//! canonical ambiguous-symbol exit code (4). The qualified form must
//! resolve unambiguously.

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// Build a Go fixture that triggers the bare-name collision.
///
/// `BadLiveware`-style layout: a struct field named `NeedTags` plus a
/// hand-emitted package-scope variable also called `NeedTags`. The
/// package-scope variable is the only way today's Go plugin produces a
/// graph node whose **simple name** equals `NeedTags` (function-local
/// variables are stored under `NeedTags@<offset>` to avoid colliding
/// across scopes), so this fixture is what makes the resolver actually
/// see two `NeedTags` candidates instead of one.
fn build_ambiguous_go_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("main.go"),
        r#"package main

type SelectorSource struct {
    NeedTags bool
}

var NeedTags = "package-scope shadow"

func useSelector(selector SelectorSource) bool {
    if selector.NeedTags {
        return true
    }
    return false
}

func unrelated() {
    _ = NeedTags
}
"#,
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
fn impact_returns_ambiguous_envelope_for_bare_collision() {
    let dir = build_ambiguous_go_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "--json", "NeedTags"])
        .assert()
        .code(4);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let envelope: Value =
        serde_json::from_str(&stdout).expect("ambiguous envelope must be valid JSON");

    let error = envelope
        .get("error")
        .expect("envelope wraps the ambiguous payload under `error`");
    assert_eq!(error["code"], "sqry::ambiguous_symbol");
    let message = error["message"].as_str().expect("message is a string");
    assert!(
        message.contains("NeedTags") && message.contains("ambiguous"),
        "message must name the symbol and the ambiguity, got {message:?}"
    );
    assert_eq!(error["truncated"], Value::Bool(false));

    let candidates = error["candidates"]
        .as_array()
        .expect("candidates is an array");
    assert!(
        candidates.len() >= 2,
        "expected at least two candidates, got {}",
        candidates.len()
    );
    for candidate in candidates {
        assert!(candidate.get("qualified_name").is_some());
        assert!(candidate.get("kind").is_some());
        assert!(candidate.get("file_path").is_some());
        assert!(candidate.get("start_line").is_some());
        assert!(candidate.get("start_column").is_some());
    }
}

#[test]
fn impact_resolves_qualified_name_unambiguously() {
    let dir = build_ambiguous_go_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "--json", "main.SelectorSource.NeedTags"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(&stdout).expect("success payload must be valid JSON");
    assert_eq!(payload["symbol"], "main.SelectorSource.NeedTags");
    // The successful path returns the impact envelope, NOT the
    // ambiguous-symbol envelope — verify by checking the
    // canonical ImpactOutput fields exist.
    assert!(payload.get("direct").is_some(), "direct[] must be present");
    assert!(payload.get("stats").is_some(), "stats must be present");
}

#[test]
fn impact_surfaces_symbol_not_found_envelope() {
    let dir = build_ambiguous_go_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "--json", "DefinitelyNotASymbol_xyz"])
        .assert()
        .code(2);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let envelope: Value = serde_json::from_str(&stdout).expect("not-found must be valid JSON");
    assert_eq!(envelope["error"]["code"], "sqry::symbol_not_found");
}

#[test]
fn impact_human_output_lists_candidates_on_stderr() {
    let dir = build_ambiguous_go_fixture();
    let assert = Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["impact", "NeedTags"])
        .assert()
        .code(4);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("ambiguous"),
        "stderr must mention ambiguity, got {stderr:?}"
    );
    assert!(
        stderr.contains("Candidates:"),
        "stderr must list candidates, got {stderr:?}"
    );
}
