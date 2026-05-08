// All tests in this file index a cross-language fixture that includes
// ABAP sources, which are feature-gated behind `specialty-plugins`
// after `refactor!: make non-core surfaces opt-in` (commit ecd215385).
// Run with:
//   cargo test -p sqry-cli --features specialty-plugins --test cli_search
#![cfg(feature = "specialty-plugins")]

mod common;

use anyhow::{Context, Result};
use common::sqry_bin;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = dst.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_all(&source_path, &target_path)?;
        } else if source_path.is_file() {
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copy {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn fixture_workspace() -> Result<TempDir> {
    let tmp = tempfile::tempdir()?;
    copy_dir_all(
        &repo_root().join("test-fixtures/cross-language"),
        tmp.path(),
    )?;
    Ok(tmp)
}

fn index_workspace(root: &Path) -> Result<()> {
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .output()
        .context("run sqry index")?;
    assert!(
        output.status.success(),
        "sqry index failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn exact_search(root: &Path, query: &str) -> Result<Value> {
    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("--exact")
        .arg(query)
        .arg(root)
        .output()
        .with_context(|| format!("run sqry --exact {query}"))?;
    assert!(
        output.status.success(),
        "sqry exact search failed for {query}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parse search JSON")
}

fn has_result(value: &Value, query: &str, expected_kind: &str, file_fragment: &str) -> bool {
    value["results"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            let qualified_matches = item["qualified_name"]
                .as_str()
                .is_some_and(|qualified| qualified == query);
            item["kind"] == expected_kind
                && qualified_matches
                && item["file_path"]
                    .as_str()
                    .is_some_and(|file| file.contains(file_fragment))
        })
    })
}

// Cross-language fixture includes an `abap/fields.abap` source whose
// `zcl_ledger.mutable_field` symbol must be present in the qualified
// search result. ABAP is feature-gated behind `specialty-plugins`
// after `refactor!: make non-core surfaces opt-in` (commit ecd215385),
// so a default-feature `sqry index` produces no ABAP nodes and the
// assertion fails. Run with:
//   cargo test -p sqry-cli --features specialty-plugins --test cli_search
#[cfg(feature = "specialty-plugins")]
#[test]
fn test_req_r0015_cli_exact_returns_qualified_field_nodes() -> Result<()> {
    let workspace = fixture_workspace()?;
    index_workspace(workspace.path())?;

    for (query, expected_kind, file_fragment) in [
        ("Ledger.promotedField", "property", "typescript/fields.ts"),
        ("Ledger::mutable_field", "property", "rust/fields.rs"),
        (
            "Ledger.constructorField",
            "property",
            "javascript/fields.js",
        ),
        ("zcl_ledger.mutable_field", "property", "abap/fields.abap"),
    ] {
        let value = exact_search(workspace.path(), query)?;
        assert!(
            has_result(&value, query, expected_kind, file_fragment),
            "--exact {query} must return a {expected_kind} result from {file_fragment}, got {value:#}"
        );
    }

    Ok(())
}
