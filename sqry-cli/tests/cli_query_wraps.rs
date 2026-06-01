//! T3 Cluster H1 — CLI surface tests for `sqry plan-query "wraps:..."`.
//!
//! Per `docs/development/go-error-context-buildtags/CLI_INTEGRATION.md`
//! §2.2 (post Cluster G-ext iter-2 rebind from legacy `sqry query` to
//! `sqry plan-query`): the planner exposes both `wraps` (bare,
//! `WrapKindFilter::Any`) and `wraps:<wrap-kind>` (`WrapKindFilter::Kind`)
//! predicates wired in `sqry-db/src/planner/parse.rs:360-378`. Each
//! predicate selects nodes whose outbound edges include a `Wraps` edge
//! of the requested family (see `node_has_wraps_edge` at
//! `sqry-db/src/planner/execute.rs:735`).
//!
//! These tests exercise the surface end-to-end: copy
//! `test-fixtures/go/error_wrapping/` into a TempDir, run
//! `sqry index`, then exercise each documented CLI behaviour.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Locate the canonical `test-fixtures/go/<sub>` directory for a given
/// sub-feature, falling back across plausible workspace layouts so
/// `cargo test` works whether invoked from the workspace root or
/// `sqry-cli/`.
fn fixture_root(sub: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();
    let candidate = workspace.join("test-fixtures").join("go").join(sub);
    assert!(
        candidate.is_dir(),
        "fixture root {} does not exist",
        candidate.display()
    );
    candidate
}

/// Copy every regular file in `src` into `dst` (recursively).
///
/// Skips dot-prefixed entries (`.sqry/`, `.git/`, …). Otherwise a
/// leftover `.sqry/graph/` from a stale dev-loop index would be copied
/// into the tempdir verbatim, and the subsequent `sqry index` would
/// short-circuit with "Index already exists" — leaving the test
/// asserting against a stale snapshot built without `Wraps` edges.
/// Discovered while closing codex iter-3 concern 5 (Cluster H1c).
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

/// Materialise a per-test copy of `test-fixtures/go/error_wrapping/`
/// into a fresh TempDir and run `sqry index --force <tempdir>`. The
/// `--force` flag is defensive: `copy_fixture` already skips dotdirs,
/// but `--force` keeps the test robust against a future fixture layout
/// where the dotdir filter alone is insufficient (e.g. test-fixtures
/// gaining a non-dot-prefixed cache).
fn indexed_error_wrapping_workspace() -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    copy_fixture(&fixture_root("error_wrapping"), temp.path());
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg("--force")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry index");
    assert!(
        output.status.success(),
        "sqry index failed: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let graph_path = temp.path().join(".sqry/graph/snapshot.sqry");
    assert!(
        graph_path.exists(),
        "expected graph snapshot at {} after sqry index --force (stdout={})",
        graph_path.display(),
        String::from_utf8_lossy(&output.stdout),
    );
    temp
}

/// A bare `wraps` predicate against the error_wrapping fixture must
/// surface every WrapKind represented in the fixtures: ErrorfVerb,
/// UnwrapMethod, UnwrapMultiMethod, ErrorsIs, ErrorsAs, ErrorsAsType
/// (gated, may or may not match depending on Go version), and
/// ErrorsJoin. We accept any non-empty result that contains at least
/// the canonical fixture function names — the exact NodeKind printed
/// is `function` (or `method` for receiver methods).
#[test]
fn cli_query_wraps_bare_returns_all_wrap_kinds() {
    let temp = indexed_error_wrapping_workspace();
    // The planner requires the first step to be a NodeScan / SetOp; the
    // bare `wraps` predicate is a Filter and must be anchored on a kind
    // query. `kind:function wraps` covers every wrapper function across
    // every WrapKind.
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function wraps")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.trim().is_empty(),
        "bare wraps must return non-empty results: stdout={stdout:?}, stderr={stderr:?}",
    );
    // Spot-check producer fixtures (ErrorfVerb + ErrorsIs + ErrorsAs +
    // ErrorsJoin all live in functions named below).
    for needle in ["wrap", "check", "extract", "bundle"] {
        assert!(
            stdout.contains(needle),
            "bare wraps must surface {needle} (any WrapKind family); stdout={stdout:?}",
        );
    }
}

/// `wraps:errors_is` must narrow to only those functions whose
/// outbound edges include `Wraps{ErrorsIs}`. In our fixture, `check`
/// is the only function with an errors.Is call site; the multi-w
/// `wrapMulti`, the `bundle` errors.Join site, etc. must NOT appear.
#[test]
fn cli_query_wraps_filtered_by_errors_is() {
    let temp = indexed_error_wrapping_workspace();
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function wraps:errors_is")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("check"),
        "wraps:errors_is must surface `check`; stdout={stdout:?}",
    );
    // Negative invariants: ErrorsJoin / ErrorfVerb sites must NOT
    // surface from a kind-filtered `wraps:errors_is` query.
    assert!(
        !stdout.contains("bundle"),
        "wraps:errors_is must NOT surface ErrorsJoin sites (`bundle`); stdout={stdout:?}",
    );
}

/// Unknown wrap kind triggers a planner parse error. `parse_wrap_kind`
/// at `sqry-db/src/planner/parse.rs:984-996` returns None for unknown
/// kinds, which bubbles up as `ParseError::UnknownIdent`. The CLI
/// exits non-zero with the offending value mentioned in stderr.
#[test]
fn cli_query_wraps_invalid_kind_exits_non_zero() {
    let temp = indexed_error_wrapping_workspace();
    Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function wraps:not_a_kind")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not_a_kind").or(predicate::str::contains("wrap kind")));
}

/// A Rust-only workspace has no `Wraps` edges (the `Wraps` taxonomy is
/// emitted by the Go plugin per 02_DESIGN §2.1). Bare `wraps` must
/// return zero results without erroring. Pins the "wraps predicate
/// against non-Go workspace = exit 0 + empty" contract.
#[test]
fn cli_query_wraps_zero_results_on_non_go_workspace() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(
        temp.path().join("lib.rs"),
        "pub fn foo() -> i32 { 1 }\npub fn bar() -> i32 { 2 }\n",
    )
    .expect("write rust fixture");
    Command::new(sqry_bin())
        .arg("index")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function wraps")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.trim().is_empty(),
        "wraps predicate against a Rust-only workspace must return zero results; stdout={stdout:?}",
    );
}

/// `--json` must serialise to a well-formed array of `PlanQueryHit`
/// objects (per the H1 clarification in CLI_INTEGRATION.md §2.2: the
/// planner's per-node output does not surface `wrap_kind` /
/// `chain_position` since a single node can satisfy multiple
/// WrapKinds; that detail belongs on the edge-walk API).
#[test]
fn cli_query_wraps_json_returns_well_formed_array() {
    let temp = indexed_error_wrapping_workspace();
    let assert = Command::new(sqry_bin())
        .arg("--json")
        .arg("plan-query")
        .arg("kind:function wraps:errorf_verb")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("plan-query --json must emit valid JSON");
    let arr = parsed
        .as_array()
        .expect("plan-query --json must emit a JSON array");
    assert!(
        !arr.is_empty(),
        "wraps:errorf_verb must surface at least one function; stdout={stdout:?}",
    );
    for hit in arr {
        for field in ["name", "qualified_name", "kind", "file", "line"] {
            assert!(
                hit.get(field).is_some(),
                "each PlanQueryHit must carry `{field}`; hit={hit:?}",
            );
        }
    }
}
