//! T3 Cluster H2 — CLI surface tests for `sqry plan-query "cfg:..."`.
//!
//! Pins the planner's `cfg:` predicate behaviour at the user-facing
//! CLI boundary. Two grammars are supported (per `docs/cli/query.md` +
//! `docs/development/go-error-context-buildtags/CLI_INTEGRATION.md` §1.1):
//!
//! - Bare: `cfg:<flag>` → `Predicate::CfgCondition(Semantic(Flag(flag)))`.
//!   Cross-language platform-token normalisation in
//!   `sqry-db/src/planner/cfg_match.rs:cfg_match` matches Go-native
//!   stored `"linux"` AND Rust-functional stored
//!   `"target_os = \"linux\""` when the bare flag is a known platform
//!   token, plus compound expressions that contain the positive flag.
//! - Quoted literal: `cfg:"<expr>"` → `Predicate::CfgCondition(Literal(s))`.
//!   Byte-exact comparison only — does NOT normalise, does NOT recognise
//!   parens, does NOT cross-language address. 02_DESIGN §10.4's
//!   cross-language dual-addressing for the quoted form is deferred.
//!
//! The chain-root constraint applies the same way it does for `wraps`:
//! `cfg:` is a `Filter` predicate and a planner chain MUST start with
//! a `NodeScan` / `SetOp`, so every test below prefixes the query with
//! `kind:function`.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Build a Go workspace with four platform-gated functions covering
/// the cfg matcher's three modes (single-flag, compound, byte-exact
/// quoted literal):
///
/// - `linux_only.go` with header `//go:build linux` → stored
///   cfg_condition `"linux"`. Matches `cfg:linux` (Semantic-Flag).
/// - `compound.go` with header `//go:build linux && amd64` →
///   stored cfg_condition `"linux && amd64"`. Matches
///   `cfg:"linux && amd64"` (Literal) and `cfg:linux` (semantic
///   positive-flag containment). Filename
///   intentionally chosen NOT to match the
///   `parse_filename_suffix` pattern (no `_linux_amd64`/`_linux`/
///   `_amd64` suffix) — otherwise conjoin would duplicate the terms
///   and the stored Literal would be `"linux && amd64 && linux && amd64"`,
///   which is a documented but separate behaviour (see
///   `sqry-lang-go/src/relations/build_constraints.rs::conjoin`).
/// - `darwin.go` with header `//go:build darwin` → stored
///   `"darwin"`. Matches neither.
/// - `plain.go` with no constraint → no cfg_condition.
///
/// Returns the indexed TempDir (with `--force` to avoid the stale
/// snapshot bug closed in Cluster H1c).
fn indexed_mixed_platform_workspace() -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("go.mod"), "module cfgtest\n\ngo 1.21\n").expect("write go.mod");
    fs::write(
        temp.path().join("linux_only.go"),
        "//go:build linux\n\npackage cfgtest\n\nfunc LinuxOnly() {}\n",
    )
    .expect("write linux_only.go");
    fs::write(
        temp.path().join("compound.go"),
        "//go:build linux && amd64\n\npackage cfgtest\n\nfunc LinuxAmd64() {}\n",
    )
    .expect("write compound.go");
    fs::write(
        temp.path().join("darwin.go"),
        "//go:build darwin\n\npackage cfgtest\n\nfunc Darwin() {}\n",
    )
    .expect("write darwin.go");
    fs::write(
        temp.path().join("plain.go"),
        "package cfgtest\n\nfunc Plain() {}\n",
    )
    .expect("write plain.go");

    Command::new(sqry_bin())
        .arg("index")
        .arg("--force")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    temp
}

/// Bare `cfg:linux` matches stored cfg_condition expressions that contain a
/// positive `linux` flag. In this fixture that includes both `LinuxOnly`
/// (`"linux"`) and `LinuxAmd64` (`"linux && amd64"`). Darwin and
/// unconstrained symbols must not surface.
#[test]
fn cli_query_cfg_bare_returns_linux_symbols() {
    let temp = indexed_mixed_platform_workspace();
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function cfg:linux")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("LinuxOnly"),
        "bare cfg:linux must surface the single-flag Linux function; stdout={stdout:?}",
    );
    assert!(
        stdout.contains("LinuxAmd64"),
        "bare cfg:linux must surface compound `linux && amd64`; stdout={stdout:?}",
    );
    assert!(
        !stdout.contains("Darwin"),
        "bare cfg:linux must NOT surface Darwin-only function; stdout={stdout:?}",
    );
    assert!(
        !stdout.contains("Plain"),
        "bare cfg:linux must NOT surface unconstrained function; stdout={stdout:?}",
    );
}

/// Quoted `cfg:"<expr>"` does byte-exact literal comparison against the
/// stored AST string. The Go plugin canonicalises `//go:build linux && amd64`
/// to the string `"linux && amd64"`, so the byte-exact form below
/// MUST match `LinuxAmd64`. Anything that differs by even a space — or
/// the `cfg:linux` semantic form — falls through to the literal-mismatch
/// path and surfaces nothing.
#[test]
fn cli_query_cfg_quoted_literal_match_is_exact() {
    let temp = indexed_mixed_platform_workspace();
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function cfg:\"linux && amd64\"")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("LinuxAmd64"),
        "quoted byte-exact `linux && amd64` must surface LinuxAmd64; stdout={stdout:?}",
    );
    assert!(
        !stdout.contains("Darwin"),
        "quoted byte-exact must NOT surface Darwin (no overlap); stdout={stdout:?}",
    );
}

/// The planner's `parse_predicate_value` rejects an unterminated
/// quoted-form `cfg:"…` payload with `ParseError`. The CLI surfaces
/// that as a non-zero exit with a parse error message on stderr.
/// (`cfg:!!badop` is intentionally NOT used here: the bare-form
/// parser is permissive about identifier shapes — `!!` is two
/// `Not` operators, so the predicate is well-formed even though
/// the comparator will never match anything.)
#[test]
fn cli_query_cfg_invalid_predicate_exits_non_zero() {
    let temp = indexed_mixed_platform_workspace();
    Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function cfg:\"unterminated")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .failure()
        .stderr(predicate::str::contains("parse").or(predicate::str::contains("Parse")));
}

/// An unknown but well-formed flag (e.g. `linnnux` — typo) is a
/// valid `Predicate::CfgCondition(Semantic(Flag(...)))`; the comparator
/// just returns false for every node. Result: exit 0 + empty stdout
/// (consistent with every other predicate miss in the planner).
#[test]
fn cli_query_cfg_unknown_flag_zero_results() {
    let temp = indexed_mixed_platform_workspace();
    let assert = Command::new(sqry_bin())
        .arg("plan-query")
        .arg("kind:function cfg:linnnux")
        .arg(temp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.trim().is_empty(),
        "unknown cfg flag must yield zero results (predicate-miss); stdout={stdout:?}",
    );
}

/// `--json` must emit a well-formed array of `PlanQueryHit` rows. The
/// shape match here mirrors the same contract pinned by
/// `cli_query_wraps_json_returns_well_formed_array`: planner per-node
/// output does NOT surface `cfg_condition` (that flows through the
/// semantic_search filter surface instead).
#[test]
fn cli_query_cfg_json_returns_well_formed_array() {
    let temp = indexed_mixed_platform_workspace();
    let assert = Command::new(sqry_bin())
        .arg("--json")
        .arg("plan-query")
        .arg("kind:function cfg:linux")
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
        "cfg:linux must surface LinuxOnly (single-flag match); stdout={stdout:?}",
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
