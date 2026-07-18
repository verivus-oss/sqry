mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn index_status_json_includes_validation_ok() {
    let dir = tempdir().unwrap();
    // Create a small Rust file
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn hello() {}\n").unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Status in JSON with unified graph schema (no legacy validation field)
    Command::new(&path)
        .arg("--json")
        .arg("index")
        .arg("--status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"exists\": true")
                .and(predicate::str::contains("\"symbol_count\"")),
        );
}

#[test]
fn index_gitignore_warning_names_current_path() {
    // #614: when run inside a git repo without `--add-to-gitignore` and with no
    // existing entry, `sqry index` warns to stderr. The warning must name the
    // current `.sqry/` directory, never the legacy `.sqry-index/` path.
    let dir = tempdir().unwrap();
    // handle_gitignore only fires when a git root is found (`.git` present).
    if std::process::Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("git unavailable; skipping warning-text assertion");
        return;
    }
    fs::write(dir.path().join("lib.rs"), "pub fn r() {}\n").unwrap();

    let path = sqry_bin();
    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success()
        .stderr(
            predicate::str::contains(".sqry/").and(predicate::str::contains(".sqry-index").not()),
        );
}

#[test]
fn index_status_json_reports_languages() {
    // #615: a multi-language index must report a non-empty, populated
    // `languages` array and `file_counts_by_language`, not `"languages": []`.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn r() {}\n").unwrap();
    fs::write(dir.path().join("app.py"), "def p():\n    pass\n").unwrap();
    fs::write(dir.path().join("main.js"), "function j() {}\n").unwrap();

    let path = sqry_bin();
    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    let assert = Command::new(&path)
        .arg("--json")
        .arg("index")
        .arg("--status")
        .arg(dir.path())
        .assert()
        .success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON so key ordering (file_counts_by_language is a map) does
    // not make the assertion brittle.
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("status JSON should parse: {e}; stdout was:\n{stdout}"));

    let languages = json["languages"]
        .as_array()
        .expect("`languages` should be a JSON array");
    let mut langs: Vec<&str> = languages.iter().filter_map(|v| v.as_str()).collect();
    langs.sort_unstable();
    // Assert the exact sorted set (not merely non-empty) so a discovery
    // regression collapsing to one language would fail this test.
    assert_eq!(
        langs,
        vec!["javascript", "python", "rust"],
        "expected the three-language set: {stdout}"
    );

    let by_lang = json["file_counts_by_language"]
        .as_object()
        .expect("`file_counts_by_language` should be a JSON object");
    for lang in ["javascript", "python", "rust"] {
        assert_eq!(
            by_lang.get(lang).and_then(serde_json::Value::as_u64),
            Some(1),
            "per-language count for {lang} should be 1: {stdout}"
        );
    }
}

#[test]
fn index_status_json_reports_index_state() {
    let dir = tempdir().unwrap();
    // Create multiple files
    let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    for f in &files {
        fs::write(dir.path().join(f), "pub fn f(){}\n").unwrap();
    }

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Remove one file after indexing
    fs::remove_file(dir.path().join("e.rs")).unwrap();

    // Status should still return valid index information
    // (The unified graph snapshot persists independently of source files)
    Command::new(&path)
        .arg("--json")
        .arg("index")
        .arg("--status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"exists\": true")
                .and(predicate::str::contains("\"symbol_count\"")),
        );
}
