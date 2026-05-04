//! Smoke test for the `--cfg-filter` / `--macro-boundaries` macro-boundary
//! search flags wired into `run_search`.
//!
//! This test indexes a tiny Rust fixture that contains a non-cfg-gated
//! function plus two cfg-gated functions and exercises the `sqry search`
//! CLI surface end-to-end. It deliberately covers ONLY the two flags that
//! are observable through the production `sqry index` writer today —
//! `--include-generated` is covered by structural unit tests in
//! `sqry-cli/src/commands/search.rs::tests` because no production
//! `sqry-lang-rust` writer currently sets `macro_generated == Some(true)`.
//!
//! The integration assertions are intentionally tolerant to whether
//! today's Rust plugin propagates `cfg_condition` through the
//! `NodeMetadataStore` for the indexed fixture. When metadata IS
//! propagated, every assertion in `cfg_filter_drops_non_matching_results`
//! verifies the live filter at the CLI layer; when it is not, the
//! `--cfg-filter <bogus>` invocation still has to drop every node that
//! lacks matching metadata (the production filter contract — missing
//! metadata is dropped when a filter is set), which keeps the test a
//! true end-to-end gate. The structural unit tests in
//! `sqry-cli/src/commands/search.rs` cover the in-memory behaviour
//! precisely.

mod common;

use common::sqry_bin;
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn write_fixture(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("create src");
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "macro-search-fixture"
version = "0.0.1"
edition = "2024"

[features]
alpha = []
beta = []

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(
        dir.join("src/lib.rs"),
        r#"pub fn always_present() -> i32 {
    42
}

#[cfg(feature = "alpha")]
pub fn alpha_only() -> &'static str {
    "alpha"
}

#[cfg(feature = "beta")]
pub fn beta_only() -> &'static str {
    "beta"
}
"#,
    )
    .expect("write src/lib.rs");
}

fn run<I, S>(project: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(sqry_bin())
        .args(args)
        .current_dir(project)
        .env("NO_COLOR", "1")
        .env("SQRY_NO_HISTORY", "1")
        .env("SQRY_REDACTION_PRESET", "none")
        .env("HOME", project.join(".home"))
        .env("XDG_CONFIG_HOME", project.join(".xdg/config"))
        .env("XDG_CACHE_HOME", project.join(".xdg/cache"))
        .env("XDG_DATA_HOME", project.join(".xdg/data"))
        .env("XDG_RUNTIME_DIR", project.join(".xdg/runtime"))
        .env(
            "SQRY_DAEMON_SOCKET",
            project.join(".xdg/runtime/sqryd.sock"),
        )
        .output()
        .expect("run sqry")
}

fn assert_success(name: &str, out: &Output) {
    assert!(
        out.status.success(),
        "{name} failed (exit={:?})\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn parse_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "expected JSON\nerror: {err}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        )
    })
}

fn results(value: &Value) -> &[Value] {
    value
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn setup() -> TempDir {
    let temp = TempDir::new().expect("create tempdir");
    let project = temp.path();
    fs::create_dir_all(project.join(".home")).unwrap();
    fs::create_dir_all(project.join(".xdg/config")).unwrap();
    fs::create_dir_all(project.join(".xdg/cache")).unwrap();
    fs::create_dir_all(project.join(".xdg/data")).unwrap();
    fs::create_dir_all(project.join(".xdg/runtime")).unwrap();
    write_fixture(project);
    let index = run(project, ["index", "."]);
    assert_success("index", &index);
    temp
}

/// Baseline: searching for `always_present` with no flags returns at
/// least one hit. Confirms the fixture indexes successfully and the
/// search engine wiring still works after the macro-boundary refactor.
#[test]
fn search_finds_always_present_baseline() {
    let temp = setup();
    let out = run(temp.path(), ["--json", "search", "always_present", "."]);
    assert_success("search baseline", &out);
    let value = parse_json(&out);
    let hits = results(&value);
    assert!(
        hits.iter()
            .any(|r| r.get("name").and_then(Value::as_str) == Some("always_present")),
        "expected always_present in results: {value}"
    );
}

/// `--cfg-filter` drops every result whose live `cfg_condition` does not
/// match. The fixture's `always_present` function carries no cfg
/// metadata, so a `--cfg-filter <bogus>` invocation MUST drop it under
/// the production filter contract (`macro_boundary_keeps_node` returns
/// false when a cfg filter is set and the node has no matching
/// metadata). This is the end-to-end observable signal that the flag
/// reaches the engine.
#[test]
fn cfg_filter_drops_non_matching_results() {
    let temp = setup();
    let out = run(
        temp.path(),
        [
            "--json",
            "search",
            "always_present",
            ".",
            "--cfg-filter",
            "feature = \"this-cfg-does-not-exist\"",
        ],
    );
    assert_success("search --cfg-filter bogus", &out);
    let value = parse_json(&out);
    let hits = results(&value);
    let always_present_hits: usize = hits
        .iter()
        .filter(|r| r.get("name").and_then(Value::as_str) == Some("always_present"))
        .count();
    assert_eq!(
        always_present_hits, 0,
        "always_present must be dropped under --cfg-filter when its cfg metadata does not match: {value}"
    );
}

/// `--macro-boundaries` reordering: every emitted symbol carries the
/// `macro_boundary_group` metadata key. When no symbols carry
/// `macro_source` (the default for plain Rust code without proc-macro
/// expansion writers), every symbol's group is the empty string, but
/// the key MUST still be present — that is the wire contract grouping
/// consumers depend on.
#[test]
fn macro_boundaries_emits_group_key_on_every_result() {
    let temp = setup();
    let out = run(
        temp.path(),
        [
            "--json",
            "search",
            "always_present",
            ".",
            "--macro-boundaries",
        ],
    );
    assert_success("search --macro-boundaries", &out);
    let value = parse_json(&out);
    let hits = results(&value);
    assert!(!hits.is_empty(), "expected non-empty hits: {value}");
    for hit in hits {
        let metadata = hit.get("metadata").and_then(Value::as_object);
        assert!(
            metadata.is_some_and(|m| m.contains_key("macro_boundary_group")),
            "every macro-boundary result must carry macro_boundary_group: {hit}"
        );
    }
}
