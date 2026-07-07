//! Regression coverage for verivus-oss/sqry#511.
//!
//! `sqry search --help` advertises `--exact`, and `sqry --exact <pattern>`
//! works at the top level, but `sqry search <pattern> --exact` used to be
//! rejected: `--exact` only bound on the top-level `Cli` struct, never on the
//! `search` subcommand. The flag now lives on the `search` variant too and its
//! value folds into the same exact-match path the shorthand drives, so both
//! spellings return identical, byte-literal results.
//!
//! These tests lock the post-fix contract end to end (real index + real
//! search), not just the clap parse (that is covered by the wiring tests in
//! `sqry-cli/src/main.rs`).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Two symbols whose names share a common substring so a regex vs literal
/// distinction is observable: a literal `Grok` matches nothing (there is no
/// symbol named exactly `Grok`), while the regex `Grok` matches `GrokClient`.
fn write_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub struct GrokClient {
    pub id: u32,
}

pub fn grok_helper() -> u32 {
    7
}
",
    )
    .unwrap();
}

fn index(root: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

/// `sqry search <PAT> <path> --exact` (flag AFTER the subcommand) must succeed
/// and return the exact-name match. This is the exact invocation from #511.
#[test]
fn search_exact_after_subcommand_succeeds() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("search")
        .arg("GrokClient")
        .arg(temp.path())
        .arg("--exact")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`search --exact` must not be rejected: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("GrokClient"),
        "exact search should find the GrokClient symbol; got: {stdout}"
    );
}

/// The short form `-x` after the subcommand must behave identically.
#[test]
fn search_exact_short_flag_after_subcommand_succeeds() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("search")
        .arg("GrokClient")
        .arg(temp.path())
        .arg("-x")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`search -x` must not be rejected: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("GrokClient"));
}

/// `--exact` is literal, not regex: a bare `Grok` under `--exact` finds no
/// symbol (there is none named exactly `Grok`), proving the subcommand flag
/// actually drives the exact-match path rather than being silently ignored.
#[test]
fn search_exact_after_subcommand_is_literal_not_regex() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let exact = Command::new(sqry_bin())
        .arg("search")
        .arg("Grok")
        .arg(temp.path())
        .arg("--exact")
        .output()
        .expect("command failed");
    assert!(
        exact.status.success(),
        "exact search must succeed even with zero matches: stderr={}",
        String::from_utf8_lossy(&exact.stderr)
    );
    let exact_out = format!(
        "{}{}",
        String::from_utf8_lossy(&exact.stdout),
        String::from_utf8_lossy(&exact.stderr),
    );
    assert!(
        exact_out.contains("No matches found"),
        "literal `Grok` should match zero symbols under --exact; got: {exact_out}"
    );

    // Without --exact, the same pattern is a regex and matches GrokClient.
    let regex = Command::new(sqry_bin())
        .arg("search")
        .arg("Grok")
        .arg(temp.path())
        .output()
        .expect("command failed");
    assert!(regex.status.success());
    assert!(
        String::from_utf8_lossy(&regex.stdout).contains("GrokClient"),
        "regex `Grok` should match GrokClient without --exact"
    );
}
