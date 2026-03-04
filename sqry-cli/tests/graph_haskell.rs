//! Haskell `GraphBuilder` smoke test (Phase 5C-1)
//!
//! Verifies that the CLI registers Haskell and reports nodes in graph stats.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    Command::new(path)
}

#[test]
fn haskell_graph_stats_reports_nodes() {
    // Create a temporary Haskell project
    let temp = TempDir::new().expect("create temp dir");
    let root: &PathBuf = &temp.path().to_path_buf();
    let main_hs = root.join("Main.hs");

    let source = r"module Demo where

add :: Int -> Int -> Int
add x y = x + y

main :: IO ()
main = print (add 1 2)
";
    fs::write(&main_hs, source).expect("write Main.hs");

    // Run graph stats by language
    let output = sqry_cmd()
        .arg("graph")
        .arg("--path")
        .arg(root)
        .arg("--format")
        .arg("json")
        .arg("stats")
        .arg("--by-language")
        .output()
        .expect("run sqry graph stats");
    assert!(
        output.status.success(),
        "graph stats command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stats: Value = serde_json::from_slice(&output.stdout).expect("stats output valid JSON");
    let language_summary = stats["nodes_by_language"]
        .as_object()
        .expect("nodes_by_language map missing");

    assert!(
        language_summary.keys().any(|lang| lang == "Haskell"),
        "expected Haskell nodes in language summary, got {language_summary:?}"
    );
}
