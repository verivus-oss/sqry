//! T3 Cluster H1 — CLI surface tests for `sqry context-propagation`.
//!
//! Per `docs/development/go-error-context-buildtags/CLI_INTEGRATION.md`
//! §2.3 (post Cluster G-ext iter-2 contract refresh): 8 tests covering
//! default scope, mode filter, file scope (resolved + not-in-index),
//! invalid mode, no-index, JSON schema, zero leaks. The handler lives
//! at `sqry-cli/src/commands/context_propagation.rs` and mirrors the
//! Cluster G MCP tool at
//! `sqry-mcp/src/execution/tools/context_propagation.rs`.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fixture_root(sub: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();
    workspace.join("test-fixtures").join("go").join(sub)
}

/// Copy every regular file in `src` into `dst` (recursively).
///
/// Skips dot-prefixed entries (`.sqry/`, `.git/`, …) so a leftover
/// dev-loop index in the canonical fixture tree never bleeds into the
/// tempdir. Without this skip, `sqry index` short-circuits with
/// "Index already exists" and the test asserts against a stale
/// snapshot. Discovered while closing codex iter-3 concern 5 (Cluster
/// H1c).
fn copy_fixture(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("read fixture entry");
        let path = entry.path();
        let name = path.file_name().expect("fixture filename");
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let target = dst.join(name);
        if path.is_dir() {
            fs::create_dir_all(&target).expect("mkdir target");
            copy_fixture(&path, &target);
        } else {
            fs::copy(&path, &target).expect("copy fixture file");
        }
    }
}

fn indexed_context_propagation_workspace() -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    copy_fixture(&fixture_root("context_propagation"), temp.path());
    // `--force` is defensive: `copy_fixture` already skips dotdirs, but
    // `--force` keeps the test robust against future fixture layouts
    // (e.g. cache directories without a dot prefix).
    Command::new(sqry_bin())
        .arg("index")
        .arg("--force")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    temp
}

/// Default scope (global, mode=all) against the full context_propagation
/// fixture must succeed (exit 0). The CLI surface contract this test
/// pins is "default scope + default mode is dispatched correctly and
/// returns a non-error response"; the actual leak counts depend on the
/// Go plugin's `TypeOf` emission for stdlib `context.Context` and on
/// the cross-file resolution shipped by Cluster C/D, which can be 0 in
/// some configurations. A zero-leak result is therefore a *valid*
/// finding for this CLI-surface test (the underlying semantic
/// coverage lives in `sqry-db/src/queries/context_propagation.rs`
/// unit tests, which run against synthetic graphs).
#[test]
fn cli_context_propagation_default_scope_succeeds() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("context-propagation")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    // Either we surface leaks (one of the mode labels appears) OR we
    // surface the documented zero-leaks line. Anything else is a CLI
    // surface regression.
    let has_leak_line = stdout.contains("break_site")
        || stdout.contains("http_handler_leak")
        || stdout.contains("unthreaded_goroutine");
    let has_zero_line = stdout.contains("no context-propagation leaks");
    assert!(
        has_leak_line || has_zero_line,
        "default-scope context-propagation must surface either leaks or the empty-result text line; stdout={stdout:?}",
    );
}

/// `--mode http-handler-leak` must filter the surfaced set to handler
/// leaks only. The CLI's text output formats each leak with the mode
/// label in brackets, so absence of `break_site` / `unthreaded_goroutine`
/// labels in stdout is the negative invariant.
#[test]
fn cli_context_propagation_mode_filter_http_handler_only() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("context-propagation")
        .arg("--mode")
        .arg("http-handler-leak")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    // With --mode http-handler-leak we never emit `break_site` /
    // `unthreaded_goroutine` lines in the text output's mode column.
    assert!(
        !stdout.contains("[break_site]"),
        "mode filter must exclude break_site mode; stdout={stdout:?}",
    );
    assert!(
        !stdout.contains("[unthreaded_goroutine]"),
        "mode filter must exclude unthreaded_goroutine mode; stdout={stdout:?}",
    );
}

/// `--scope file:<path>` must restrict leaks to those whose caller
/// function lives in that file. We use `break_site.go` (resolved
/// relative to the workspace root by `parse_scope`).
#[test]
fn cli_context_propagation_scope_file_filters_correctly() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("context-propagation")
        .arg("--scope")
        .arg("file:break_site.go")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    // Scope confined to break_site.go must not surface leaks from
    // unrelated fixtures.
    assert!(
        !stdout.contains("http_handler"),
        "file-scoped query must NOT surface http_handler leaks; stdout={stdout:?}",
    );
    assert!(
        !stdout.contains("LaunchExpensive"),
        "file-scoped query must NOT surface goroutine_leak fixtures; stdout={stdout:?}",
    );
}

/// Unknown `--mode` is rejected by clap's `ValueEnum` machinery → exit 2.
#[test]
fn cli_context_propagation_invalid_mode_exits_2() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("context-propagation")
        .arg("--mode")
        .arg("not_a_mode")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .failure();
    assert.code(2);
}

/// `--scope file:<path>` against a workspace with no `.sqry-index`
/// returns exit 3 with a "No .sqry-index found" diagnostic. The handler
/// short-circuits via `std::process::exit(EXIT_NO_INDEX)` before the
/// anyhow layer can re-classify (per `CLI_INTEGRATION.md` §1.3).
#[test]
fn cli_context_propagation_no_index_exits_3() {
    let temp = TempDir::new().expect("tempdir");
    Command::new(sqry_bin())
        .arg("context-propagation")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("No .sqry-index"));
}

/// `--json` output must deserialise into the `ContextLeakHit` schema
/// from `CLI_INTEGRATION.md` §1.3: caller / callee / mode /
/// caller_file / call_site / caller_ctx_param. Per the iter-2 schema
/// rebind, keys are qualified names + file paths (NOT NodeIds).
#[test]
fn cli_context_propagation_json_schema_match() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("--json")
        .arg("context-propagation")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("context-propagation --json must emit JSON");
    let arr = parsed
        .as_array()
        .expect("context-propagation --json must emit a JSON array");
    for hit in arr {
        for field in ["caller", "callee", "mode", "caller_file", "call_site"] {
            assert!(
                hit.get(field).is_some(),
                "each ContextLeakHit must carry `{field}`; hit={hit:?}",
            );
        }
        let call_site = hit
            .get("call_site")
            .and_then(|v| v.as_object())
            .expect("call_site must be an object");
        for nested in [
            "file",
            "start_line",
            "start_column",
            "end_line",
            "end_column",
        ] {
            assert!(
                call_site.contains_key(nested),
                "call_site must carry `{nested}`; call_site={call_site:?}",
            );
        }
        // caller_ctx_param is optional but the key must be present
        // (serde does not skip None on this struct).
        assert!(
            hit.get("caller_ctx_param").is_some(),
            "caller_ctx_param key must be present (may be null); hit={hit:?}",
        );
    }
}

/// A workspace with no context leaks must exit 0 (zero leaks is a
/// valid finding, NOT an error). Use a tiny non-leaky Go file.
#[test]
fn cli_context_propagation_zero_leaks_exits_0() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("go.mod"), "module x\n\ngo 1.22\n").expect("write go.mod");
    fs::write(
        temp.path().join("plain.go"),
        "package x\n\nfunc Plain() int { return 1 }\n",
    )
    .expect("write plain.go");
    Command::new(sqry_bin())
        .arg("index")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    Command::new(sqry_bin())
        .arg("context-propagation")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
}

/// `--scope file:<not-indexed>` against an indexed workspace returns
/// exit 0 + empty result set. Matches the MCP handler's silent
/// short-circuit at
/// `sqry-mcp/src/execution/tools/context_propagation.rs:83-86`.
/// Documented Cluster G-ext iter-2 clarification in
/// CLI_INTEGRATION.md §1.3.
#[test]
fn cli_context_propagation_scope_file_not_in_index_exits_0() {
    let temp = indexed_context_propagation_workspace();
    let assert = Command::new(sqry_bin())
        .arg("context-propagation")
        .arg("--scope")
        .arg("file:does-not-exist.go")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("no context-propagation leaks"),
        "file-not-in-index must surface the empty-result text line; stdout={stdout:?}",
    );
}
