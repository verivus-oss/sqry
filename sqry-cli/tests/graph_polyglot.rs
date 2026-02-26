//! Integration test for polyglot (multi-language) graph coverage (FR-2025-009 Phase 3 Sprint 2).
//!
//! This test validates that the graph CLI can handle projects with multiple languages
//! and correctly index/extract nodes from each language.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    Command::new(path)
}

fn index_project(path: &Path) {
    sqry_cmd()
        .arg("index")
        .arg("--force")
        .arg(path)
        .assert()
        .success();
}

fn copy_fixture_dir(relative: &str) -> TempDir {
    let binding = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = binding.parent().expect("workspace root");
    let source = root.join(relative);
    assert!(source.exists(), "fixture directory {source:?} not found");

    let temp = TempDir::new().expect("create temp dir");
    copy_dir_all(&source, temp.path()).expect("copy fixture into temp dir");
    temp
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

#[test]
fn polyglot_project_extracts_nodes_from_multiple_languages() {
    // This test validates that a multi-language project (containing Rust, JavaScript, SQL, Dart)
    // can be indexed and graph statistics correctly report nodes from each language.
    //
    // Current Phase 3 Sprint 2 implementations:
    // - Rust: Full node + edge extraction
    // - JavaScript: Full node + edge extraction
    // - SQL: Node extraction + table operation edges
    // - Dart: Node extraction only
    //
    // This test focuses on node extraction (supported by all languages) rather than
    // cross-language edges (which require specific patterns like MethodChannel, FFI, etc.)

    let project = copy_fixture_dir("tests/fixtures/polyglot_micro");
    index_project(project.path());

    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(project.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .arg("--by-file")
        .output()
        .expect("run sqry graph stats");
    assert!(
        output.status.success(),
        "graph stats command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stats: Value =
        serde_json::from_slice(&output.stdout).expect("stats output should be valid JSON");

    // Verify multi-language node extraction
    let language_summary = stats["nodes_by_language"]
        .as_object()
        .expect("nodes_by_language map missing");

    // Check for Rust nodes
    assert!(
        language_summary
            .keys()
            .any(|lang| lang.eq("Rust") || lang.eq("RUST")),
        "expected Rust nodes in language summary, got {language_summary:?}"
    );

    // Check for JavaScript nodes
    assert!(
        language_summary
            .keys()
            .any(|lang| lang.eq("JavaScript") || lang.eq("JAVASCRIPT") || lang.eq("Js")),
        "expected JavaScript nodes in language summary, got {language_summary:?}"
    );

    // Check for SQL nodes
    assert!(
        language_summary
            .keys()
            .any(|lang| lang.eq("Sql") || lang.eq("SQL")),
        "expected SQL nodes in language summary, got {language_summary:?}"
    );

    // NOTE: Dart GraphBuilder issue discovered during testing
    // Dart files ARE indexed (symbols extracted via Dart plugin)
    // BUT graph stats shows 0 Dart nodes (GraphBuilder not emitting nodes)
    // Investigation needed:
    // - DartPlugin registered in graph/loader.rs line 176
    // - DartGraphBuilder exists and implements GraphBuilder trait
    // - Symbol extraction works (index shows Dart symbols)
    // - Graph node emission doesn't work (graph stats shows 0 Dart nodes)
    //
    // All 4 languages should now work end-to-end:
    // Rust, JavaScript, SQL, Dart all successfully:
    // 1. Get indexed (symbol extraction)
    // 2. Get graph-built (GraphBuilder emits nodes)
    // 3. Appear in graph stats
    //
    // Dart assertion enabled after Phase 1 remediation (generic class/function extraction)
    assert!(
        language_summary
            .keys()
            .any(|lang| lang.eq("Dart") || lang.eq("DART")),
        "expected Dart nodes in language summary, got {language_summary:?}"
    );

    let dart_count = language_summary
        .get("Dart")
        .or_else(|| language_summary.get("DART"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        dart_count >= 2,
        "expected at least 2 Dart nodes (Counter + HelloWidget), found {dart_count}"
    );

    // Verify file-level breakdown includes files from working languages
    let file_summary = stats["nodes_by_file"]
        .as_object()
        .expect("nodes_by_file map missing");

    assert!(
        file_summary.keys().any(|file| file.ends_with(".rs")),
        "expected Rust file (.rs) in nodes_by_file summary"
    );
    assert!(
        file_summary.keys().any(|file| file.ends_with(".js")),
        "expected JavaScript file (.js) in nodes_by_file summary"
    );
    assert!(
        file_summary.keys().any(|file| file.ends_with(".sql")),
        "expected SQL file (.sql) in nodes_by_file summary"
    );

    // Total node count should be at least 3 languages x 1 node minimum
    let total_nodes = stats["node_count"]
        .as_u64()
        .expect("node_count should be a number");
    assert!(
        total_nodes >= 3,
        "expected at least 3 total nodes (Rust+JS+SQL), got {total_nodes}"
    );
}
