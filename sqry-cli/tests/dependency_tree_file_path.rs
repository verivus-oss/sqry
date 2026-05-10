//! Integration tests for `sqry graph dependency-tree <file-path>`.
//!
//! Covers verivus-oss/sqry#215: the previous version only tried symbol-name
//! lookup for the `<module>` argument and rejected file paths the rest of the
//! graph surface (`dependency_impact`'s candidate list, `query`, etc.) knows
//! about. After the fix, both repo-relative and absolute paths to indexed
//! files resolve to the nodes defined in that file and the BFS uses them as
//! roots.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn build_minimal_indexed_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("a.c"),
        r"int helper(void) { return 1; }
int caller(void) { return helper(); }
",
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
fn dependency_tree_accepts_relative_file_path() {
    let dir = build_minimal_indexed_repo();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["graph", "--path", ".", "dependency-tree", "a.c"])
        .assert()
        .success();
}

#[test]
fn dependency_tree_accepts_absolute_file_path() {
    let dir = build_minimal_indexed_repo();
    let abs = dir.path().join("a.c");
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args(["graph", "--path", "."])
        .arg("dependency-tree")
        .arg(&abs)
        .assert()
        .success();
}

#[test]
fn dependency_tree_unknown_module_reports_both_lookup_paths() {
    let dir = build_minimal_indexed_repo();
    Command::new(sqry_bin())
        .current_dir(&dir)
        .args([
            "graph",
            "--path",
            ".",
            "dependency-tree",
            "nope_not_a_thing",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("symbol-name lookup")
                .and(predicate::str::contains("file-path lookup")),
        );
}
