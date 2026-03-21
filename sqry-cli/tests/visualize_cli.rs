mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn build_sample_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let ts_code = r"
function helper() {
    return 42;
}

export function main() {
    return helper();
}
";
    fs::write(project.path().join("sample.ts"), ts_code).unwrap();

    let path = sqry_bin();

    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_single_function_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let ts_code = r"
function main() {
    return 1;
}
";
    fs::write(project.path().join("single.ts"), ts_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_import_export_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let a_code = r"
export function helper() {
    return 1;
}
";
    let b_code = r#"
import { helper } from "react";
"#;
    fs::write(project.path().join("a.ts"), a_code).unwrap();
    fs::write(project.path().join("b.ts"), b_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_import_alias_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let rs_code = r"
use std::fmt as fmt_alias;

pub fn main() {
    let _ = fmt_alias::Error;
}
";
    fs::write(project.path().join("lib.rs"), rs_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_multi_import_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let a_code = r#"
import { foo } from "react";
"#;
    let b_code = r#"
import { bar } from "react";
"#;
    fs::write(project.path().join("a.ts"), a_code).unwrap();
    fs::write(project.path().join("b.ts"), b_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_import_exact_and_substring_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let a_code = r#"
import { foo } from "react";
"#;
    let b_code = r#"
import { bar } from "react-dom";
"#;
    fs::write(project.path().join("a.ts"), a_code).unwrap();
    fs::write(project.path().join("b.ts"), b_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_export_alias_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let ts_code = r"
function helper() {
    return 1;
}

export { helper as public_helper };
";
    fs::write(project.path().join("exports.ts"), ts_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn build_rust_qualified_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let rs_code = r"
mod utils {
    pub fn helper() {
        let _ = 1;
    }
}

pub fn qualified_caller() {
    utils::helper();
}
";
    fs::write(project.path().join("lib.rs"), rs_code).unwrap();

    let path = sqry_bin();
    Command::new(path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    project
}

fn remove_symbol_index(root: &Path) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if file_name.to_string_lossy().starts_with(".sqry-index") {
                remove_path(&entry.path());
            }
        }
    }
}

fn remove_graph_snapshot(root: &Path) {
    let snapshot = root.join(".sqry/graph/snapshot.sqry");
    remove_path(&snapshot);
}

fn remove_path(path: &Path) {
    if path.is_dir() {
        fs::remove_dir_all(path).unwrap();
    } else if path.is_file() {
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn visualize_mermaid_text_output() {
    let project = build_sample_project();

    let path = sqry_bin();

    Command::new(path)
        .args([
            "visualize",
            "callers:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("graph"));
}

#[test]
fn visualize_works_without_symbol_index() {
    let project = build_sample_project();
    remove_symbol_index(project.path());

    let path = sqry_bin();
    Command::new(path)
        .args([
            "visualize",
            "callers:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("graph"));
}

#[test]
fn visualize_errors_when_snapshot_missing_but_manifest_exists() {
    let project = build_sample_project();
    remove_graph_snapshot(project.path());

    // Under manifest-last persistence, manifest present + snapshot missing = corruption.
    // The loader should NOT silently rebuild — user must run `sqry index --force`.
    let path = sqry_bin();
    Command::new(path)
        .args([
            "visualize",
            "callers:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("corrupted or incomplete"));
}

#[test]
fn visualize_errors_on_empty_graph() {
    let project = TempDir::new().unwrap();
    let path = sqry_bin();

    Command::new(path)
        .args([
            "visualize",
            "callers:main",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Graph is empty"));
}

#[test]
fn visualize_warns_on_missing_relations() {
    let project = build_single_function_project();
    let path = sqry_bin();

    Command::new(path)
        .args([
            "visualize",
            "callers:main",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("main"))
        .stderr(predicate::str::contains("No relations found"));
}

#[test]
fn visualize_callers_and_callees_include_expected_symbols() {
    let project = build_sample_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "callers:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper"))
        .stdout(predicate::str::contains("main"));

    Command::new(&path)
        .args([
            "visualize",
            "callees:main",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper"));
}

#[test]
fn visualize_qualified_name_match() {
    let project = build_rust_qualified_project();
    let path = sqry_bin();

    Command::new(path)
        .args([
            "visualize",
            "callers:utils::helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("utils::helper"))
        .stdout(predicate::str::contains("qualified_caller"));
}

#[test]
fn visualize_imports_exports_use_unified_edges() {
    let project = build_import_export_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "imports:eac",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("react"))
        .stdout(predicate::str::contains("b.ts"));

    Command::new(&path)
        .args([
            "visualize",
            "exports:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper"))
        .stdout(predicate::str::contains("a.ts"));
}

#[test]
fn visualize_imports_aggregate_multiple_importers() {
    let project = build_multi_import_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "imports:react",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("a.ts"))
        .stdout(predicate::str::contains("b.ts"));
}

#[test]
fn visualize_imports_include_exact_and_substring_matches() {
    let project = build_import_exact_and_substring_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "imports:react",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("react"))
        .stdout(predicate::str::contains("react-dom"))
        .stdout(predicate::str::contains("a.ts"))
        .stdout(predicate::str::contains("b.ts"));
}

#[test]
fn visualize_imports_render_alias_label() {
    let project = build_import_alias_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "imports:std::fmt",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("-- as fmt_alias -->"));
}

#[test]
fn visualize_exports_render_alias_label() {
    let project = build_export_alias_project();
    let path = sqry_bin();

    Command::new(&path)
        .args([
            "visualize",
            "exports:helper",
            "--format",
            "mermaid",
            "--path",
            project.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("-- as public_helper -->"));
}

#[test]
fn visualize_help_includes_examples() {
    let path = sqry_bin();

    Command::new(path)
        .args(["visualize", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "sqry visualize \"callers:main\" --format mermaid",
        ));
}

#[test]
fn visualize_invalid_relation_errors() {
    let path = sqry_bin();

    Command::new(path)
        .args(["visualize", "norel"])
        .assert()
        .failure();
}
