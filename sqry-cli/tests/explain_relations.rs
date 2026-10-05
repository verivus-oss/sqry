//! T5 (surface parity W4, design W4-D4): `sqry explain` honours
//! `--no-relations` because it now computes relations. On a fixture where
//! `a` calls `b`, `sqry --json explain fixture.rs b` carries
//! `relations.callers` naming `a` and no `callees` side (the MCP
//! `explain_code` shape: a side with no entries is omitted); for `a` the
//! `callees` side names `b`. `--no-relations` omits the key (the control,
//! green on both heads). On the pre-change head the key was absent in every
//! leg because the CLI never computed relations.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::sqry_bin;
use serde_json::Value;
use tempfile::tempdir;

const FIXTURE: &str = "pub fn a() {\n    b();\n}\n\npub fn b() {}\n";

fn indexed_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fixture.rs"), FIXTURE).expect("write fixture");
    Command::new(sqry_bin())
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();
    dir
}

fn explain_json(root: &Path, symbol: &str, extra: &[&str]) -> Value {
    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("explain")
        .arg("fixture.rs")
        .arg(symbol)
        .args(extra)
        .current_dir(root)
        .output()
        .expect("run sqry explain");
    assert!(
        output.status.success(),
        "sqry explain {symbol} {extra:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "explain output is not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn names(side: &Value) -> Vec<String> {
    side.as_array()
        .unwrap_or_else(|| panic!("a relations side is an array: {side}"))
        .iter()
        .map(|entry| entry["name"].as_str().expect("name").to_string())
        .collect()
}

#[test]
fn explain_json_carries_callers_and_callees_unless_no_relations() {
    let dir = indexed_fixture();

    let b = explain_json(dir.path(), "b", &[]);
    println!("explain b: {b}");
    let relations = b
        .get("relations")
        .unwrap_or_else(|| panic!("`relations` must be present for b: {b}"));
    let callers = names(&relations["callers"]);
    println!("callers of b: {callers:?}");
    assert_eq!(
        callers,
        vec!["a".to_string()],
        "b is called by a: {relations}"
    );
    assert!(
        relations.get("callees").is_none(),
        "b calls nothing, so the callees side is omitted as the MCP omits it: {relations}"
    );

    let a = explain_json(dir.path(), "a", &[]);
    let relations = a
        .get("relations")
        .unwrap_or_else(|| panic!("`relations` must be present for a: {a}"));
    let callees = names(&relations["callees"]);
    println!("callees of a: {callees:?}");
    assert_eq!(callees, vec!["b".to_string()], "a calls b: {relations}");
    assert!(
        relations.get("callers").is_none(),
        "nothing calls a, so the callers side is omitted: {relations}"
    );

    // Control: `--no-relations` omits the key (green on both heads).
    let quiet = explain_json(dir.path(), "b", &["--no-relations"]);
    assert!(
        quiet.get("relations").is_none(),
        "--no-relations must omit the key: {quiet}"
    );
}

#[test]
fn explain_text_lists_the_callers_unless_no_relations() {
    let dir = indexed_fixture();
    let run = |extra: &[&str]| -> String {
        let output = Command::new(sqry_bin())
            .arg("explain")
            .arg("fixture.rs")
            .arg("b")
            .args(extra)
            .current_dir(dir.path())
            .output()
            .expect("run sqry explain");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let text = run(&[]);
    println!("{text}");
    assert!(
        text.contains("Callers (1):") && text.contains("Callees (0):"),
        "the text rendering lists both sides with their counts: {text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("a [Function]")),
        "the caller line names a with its kind: {text}"
    );
    let quiet = run(&["--no-relations"]);
    assert!(
        !quiet.contains("Callers ("),
        "--no-relations must not render the block: {quiet}"
    );
}
