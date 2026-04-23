//! DB20 golden tests for the migrated `semantic_diff` handler.
//!
//! Phase 3C DB20 rewires `semantic_diff` through
//! [`sqry_db::ComparativeQueryDb`]. The wrapper is uncached by design —
//! cross-snapshot results have no meaningful invalidation criterion — so
//! this handler is the only one that does NOT go through `make_query_db`.
//!
//! These tests build a real git repository with two commits containing
//! deliberate semantic changes, invoke [`execute_semantic_diff`], and
//! assert the MCP wire DTO matches the pre-DB20 shape exactly:
//!
//! * Every `change_type` string (`"added"` / `"removed"` / `"modified"` /
//!   `"signature_changed"` / `"renamed"`) flows through verbatim.
//! * `baseLocation` / `targetLocation` are populated per change type
//!   (removed has base only; added has target only; modified has both).
//! * The `filters.change_types` and `filters.symbol_kinds` predicates
//!   behave identically to the pre-DB20 comparator.
//! * Pagination reports `total` / `truncated` / `next_page_token`
//!   consistently.

use anyhow::Result;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    ChangeType, GitVersionRef, PaginationArgs, SemanticDiffArgs, SemanticDiffFilters,
};
use sqry_mcp::tool_handlers::execute_semantic_diff;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

/// Test helper owning a TempDir-backed git repo with controlled history.
struct GitFixture {
    _temp: TempDir,
    path: PathBuf,
}

impl GitFixture {
    fn new() -> Result<Self> {
        let temp = TempDir::new()?;
        let path = temp.path().to_path_buf();
        run_git(&path, &["init", "-q", "-b", "main"])?;
        // Commits need an author; use explicit config so the tests pass
        // even when the ambient user.name / user.email are unset.
        run_git(&path, &["config", "user.name", "DB20 Test"])?;
        run_git(&path, &["config", "user.email", "db20@example.com"])?;
        // `commit.gpgsign` can be set globally on dev machines — disable
        // it here so repo creation does not depend on a signing key.
        run_git(&path, &["config", "commit.gpgsign", "false"])?;
        Ok(Self { _temp: temp, path })
    }

    fn root(&self) -> &Path {
        &self.path
    }

    fn write(&self, rel: &str, content: &str) -> Result<()> {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(full, content)?;
        Ok(())
    }

    fn commit(&self, msg: &str) -> Result<String> {
        run_git(&self.path, &["add", "-A"])?;
        run_git(&self.path, &["commit", "-q", "--allow-empty", "-m", msg])?;
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.path)
            .output()?;
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(cwd).status()?;
    anyhow::ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 100,
    }
}

fn workspace_arg(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn diff_args(
    root: &Path,
    base: &str,
    target: &str,
    filters: SemanticDiffFilters,
    include_unchanged: bool,
    max_results: usize,
    pagination: PaginationArgs,
) -> SemanticDiffArgs {
    SemanticDiffArgs {
        base: GitVersionRef {
            git_ref: base.to_string(),
            file_path: None,
        },
        target: GitVersionRef {
            git_ref: target.to_string(),
            file_path: None,
        },
        path: workspace_arg(root),
        include_unchanged,
        filters,
        max_results,
        pagination,
    }
}

/// Build a fixture with three semantic changes between its two commits:
///
/// * `added_only` is new in HEAD (lives in `src/added.rs`).
/// * `removed_only` is only in HEAD~1 (lives in `src/removed.rs` which
///   is deleted in HEAD).
/// * `body_changed_fn` exists on both sides with the same qualified
///   name but a different line range (its body grows by ~3 lines);
///   the comparator's signature-equal / line-range-differs path
///   classifies that as `modified`.
/// * `stable_helper` is unchanged in both commits.
fn build_three_change_fixture() -> Result<(GitFixture, String, String)> {
    let fx = GitFixture::new()?;
    fx.write(
        "Cargo.toml",
        r#"[package]
name = "db20_diff_fixture"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;

    // Base commit: lib.rs with `stable_helper` + short `body_changed_fn`,
    // plus `src/removed.rs` which disappears in the target.
    fx.write(
        "src/lib.rs",
        r#"pub mod removed;

pub fn stable_helper(x: u32) -> u32 {
    x + 1
}

pub fn body_changed_fn(x: u32) -> u32 {
    x + 1
}
"#,
    )?;
    fx.write(
        "src/removed.rs",
        r#"pub fn removed_only() {}
"#,
    )?;
    let base_sha = fx.commit("base: stable + body_changed_fn + removed_only")?;

    // Target commit: drop `src/removed.rs`, add `src/added.rs` with a
    // new top-level function, and grow `body_changed_fn`'s body by
    // several lines so its (start_line, end_line) shifts. That body
    // shift drives the `modified` classification — the Rust plugin
    // does not currently emit function signatures, so the comparator
    // falls back to line-range comparison for body modifications.
    let removed_path = fx.root().join("src/removed.rs");
    fs::remove_file(&removed_path)?;
    fx.write(
        "src/lib.rs",
        r#"pub mod added;

pub fn stable_helper(x: u32) -> u32 {
    x + 1
}

pub fn body_changed_fn(x: u32) -> u32 {
    let y = x + 1;
    let z = y * 2;
    let w = z - 3;
    w
}
"#,
    )?;
    fx.write(
        "src/added.rs",
        r#"pub fn added_only() {}
"#,
    )?;
    let target_sha =
        fx.commit("target: add added_only, drop removed_only, grow body_changed_fn")?;

    Ok((fx, base_sha, target_sha))
}

/// Function-scoped filter. Every assertion in this file filters by
/// `kind=function` because the Rust plugin also emits graph nodes for
/// parameters / locals. Limiting to `function` keeps the assertions
/// about deliberate top-level changes clean and portable.
fn function_only_filter() -> SemanticDiffFilters {
    SemanticDiffFilters {
        change_types: vec![],
        symbol_kinds: vec!["function".to_string()],
    }
}

/// Fixture with a function that exists only in target — guarantees a
/// deterministic `added` record. Base has no removed functions, so the
/// rename heuristic has nothing to pair against and cannot convert the
/// new function into a `renamed` record.
fn build_pure_added_fixture() -> Result<(GitFixture, String, String)> {
    let fx = GitFixture::new()?;
    fx.write(
        "Cargo.toml",
        r#"[package]
name = "db20_diff_pure_added"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    // Base: only a stable function, no removed candidates.
    fx.write(
        "src/lib.rs",
        r#"pub fn stable_anchor() -> u32 { 1 }
"#,
    )?;
    let base_sha = fx.commit("base: stable_anchor only")?;

    // Target: keep stable_anchor, add new_function_only_in_target.
    // No base function is removed, so rename detection cannot fire.
    fx.write(
        "src/lib.rs",
        r#"pub fn stable_anchor() -> u32 { 1 }

pub fn new_function_only_in_target() -> u32 { 42 }
"#,
    )?;
    let target_sha = fx.commit("target: add new_function_only_in_target")?;
    Ok((fx, base_sha, target_sha))
}

/// Fixture with a function that exists only in base — guarantees a
/// deterministic `removed` record. Target has no new functions, so the
/// rename heuristic has nothing to pair against and cannot convert the
/// deleted function into a `renamed` record.
fn build_pure_removed_fixture() -> Result<(GitFixture, String, String)> {
    let fx = GitFixture::new()?;
    fx.write(
        "Cargo.toml",
        r#"[package]
name = "db20_diff_pure_removed"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    // Base: stable anchor + the function to be deleted.
    fx.write(
        "src/lib.rs",
        r#"pub fn stable_anchor() -> u32 { 1 }

pub fn function_only_in_base() -> u32 { 99 }
"#,
    )?;
    let base_sha = fx.commit("base: stable_anchor + function_only_in_base")?;

    // Target: drop function_only_in_base, keep stable_anchor.
    // No new function is added, so rename detection cannot fire.
    fx.write(
        "src/lib.rs",
        r#"pub fn stable_anchor() -> u32 { 1 }
"#,
    )?;
    let target_sha = fx.commit("target: drop function_only_in_base")?;
    Ok((fx, base_sha, target_sha))
}

/// Fixture that forces a `renamed` record via the heuristic.
///
/// A function present only in base (`old_name_alpha`) and a function
/// present only in target (`new_name_alpha`) both have kind=function,
/// signatures=None, and similar name characteristics. With (None, None)
/// signatures the sig_score is 1.0, and cross-file location yields
/// confidence 0.7*1.0 + 0.3*0.7 = 0.91 ≥ 0.9 (RENAME_CONFIDENCE_THRESHOLD),
/// so they are deterministically paired as a rename.
fn build_rename_fixture() -> Result<(GitFixture, String, String)> {
    let fx = GitFixture::new()?;
    fx.write(
        "Cargo.toml",
        r#"[package]
name = "db20_diff_rename"
version = "0.0.1"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    // Base: one function that will be renamed.
    fx.write(
        "src/lib.rs",
        r#"pub fn old_name_alpha() -> u32 { 1 }
"#,
    )?;
    let base_sha = fx.commit("base: old_name_alpha")?;

    // Target: replace it with a differently-named function at the same
    // conceptual site (same kind, both signatures = None, cross-file
    // confidence 0.91 → rename).
    fx.write(
        "src/lib.rs",
        r#"pub fn new_name_alpha() -> u32 { 1 }
"#,
    )?;
    let target_sha = fx.commit("target: rename old_name_alpha → new_name_alpha")?;
    Ok((fx, base_sha, target_sha))
}

#[test]
fn semantic_diff_detects_added_removed_and_modified() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        500,
        paging(),
    );

    let out = execute_semantic_diff(&args)?;
    let data = &out.data;

    // base/target refs flow through the DTO verbatim.
    assert_eq!(data.base_ref, base);
    assert_eq!(data.target_ref, target);

    let all: Vec<(&str, &str, &str)> = data
        .changes
        .iter()
        .map(|c| {
            (
                c.change_type.as_str(),
                c.symbol_name.as_str(),
                c.qualified_name.as_str(),
            )
        })
        .collect();

    // `added_only` must be present somewhere in the diff. The rename
    // heuristic may fold it and `removed_only` into a single `renamed`
    // record (both kind=function, signatures = None → sig_score 1.0,
    // cross-file location 0.7 → confidence 0.91, above the 0.9
    // threshold). That is pre-DB20 behaviour we deliberately preserve —
    // the important invariant is that the change is visible in SOME
    // `change_type`, not which specific one.
    assert!(
        all.iter()
            .any(|(ct, name, _)| (*ct == "added" || *ct == "renamed") && *name == "added_only"),
        "expected added_only as `added` or `renamed`; got {all:?}"
    );

    // `body_changed_fn` kept its qualified name but its line range
    // shifted. The pre-DB20 comparator falls back to `modified` when
    // signatures are None on both sides and only the line numbers
    // changed — that path must still fire post-DB20.
    assert!(
        all.iter()
            .any(|(ct, name, _)| *ct == "modified" && *name == "body_changed_fn"),
        "expected `modified` for body_changed_fn; got {all:?}"
    );

    // `stable_helper` must NOT appear (unchanged; include_unchanged=false).
    assert!(
        !all.iter().any(|(_, name, _)| *name == "stable_helper"),
        "stable_helper must not appear when include_unchanged=false: {all:?}"
    );

    // Every surviving change is kind=function (filter contract).
    assert!(
        data.changes.iter().all(|c| c.kind == "function"),
        "function filter must only produce kind=function rows; got {:?}",
        data.changes.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );

    // Envelope bookkeeping.
    assert!(out.used_graph);
    assert!(!out.used_index);
    assert_eq!(out.total, Some(data.total));

    Ok(())
}

#[test]
fn semantic_diff_filters_change_types_to_modified_only() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;
    // Filter to `change_types=[Modified]` + `symbol_kinds=["function"]`.
    // `body_changed_fn` is the only function-kind modification the
    // fixture produces, so this exercises BOTH filters while staying
    // independent of the rename heuristic's coin flip on
    // added_only / removed_only.
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        SemanticDiffFilters {
            change_types: vec![ChangeType::Modified],
            symbol_kinds: vec!["function".to_string()],
        },
        false,
        200,
        paging(),
    );

    let out = execute_semantic_diff(&args)?;
    assert!(
        out.data.changes.iter().all(|c| c.change_type == "modified"),
        "change_type filter must exclude non-modified: {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.change_type)
            .collect::<Vec<_>>()
    );
    assert!(
        out.data.changes.iter().all(|c| c.kind == "function"),
        "symbol_kinds filter must exclude non-function: {:?}",
        out.data.changes.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
    assert!(
        out.data
            .changes
            .iter()
            .any(|c| c.symbol_name == "body_changed_fn"),
        "body_changed_fn must survive the modified-only filter"
    );
    // Post-filter summary: only `modified` should be non-zero.
    assert_eq!(out.data.summary.modified, out.data.changes.len() as u64);
    assert_eq!(out.data.summary.added, 0);
    assert_eq!(out.data.summary.removed, 0);
    assert_eq!(out.data.summary.signature_changed, 0);
    assert_eq!(out.data.summary.renamed, 0);
    Ok(())
}

#[test]
fn semantic_diff_filters_symbol_kinds_case_insensitively() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        SemanticDiffFilters {
            change_types: vec![],
            // Upper-case `FUNCTION` exercises the case-insensitive compare
            // path — the pre-DB20 comparator also did `eq_ignore_ascii_case`.
            symbol_kinds: vec!["FUNCTION".to_string()],
        },
        false,
        500,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;
    assert!(
        out.data.changes.iter().all(|c| c.kind == "function"),
        "kind filter must exclude non-function; got {:?}",
        out.data.changes.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
    let names: Vec<&str> = out
        .data
        .changes
        .iter()
        .map(|c| c.symbol_name.as_str())
        .collect();
    assert!(
        names.contains(&"added_only"),
        "expected added_only among function changes: {names:?}"
    );
    assert!(
        names.contains(&"body_changed_fn"),
        "expected body_changed_fn among function changes: {names:?}"
    );
    Ok(())
}

#[test]
fn semantic_diff_empty_when_refs_identical() -> Result<()> {
    init_caches();
    let (fx, base, _target) = build_three_change_fixture()?;
    // Diffing a commit against itself must yield zero changes,
    // regardless of filter.
    let args = diff_args(
        fx.root(),
        &base,
        &base,
        function_only_filter(),
        false,
        100,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;
    assert!(
        out.data.changes.is_empty(),
        "identical refs must produce no changes: {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.symbol_name)
            .collect::<Vec<_>>()
    );
    assert_eq!(out.data.summary.added, 0);
    assert_eq!(out.data.summary.removed, 0);
    assert_eq!(out.data.summary.modified, 0);
    assert_eq!(out.data.summary.signature_changed, 0);
    assert_eq!(out.data.summary.renamed, 0);
    assert_eq!(out.total, Some(0));
    assert_eq!(out.truncated, Some(false));
    Ok(())
}

#[test]
fn semantic_diff_paginates_when_max_results_exceeded() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;
    // First learn how many function-level changes the fixture produces
    // (the rename heuristic may or may not collapse added_only /
    // removed_only into a rename record, so the count is 2 or 3).
    let unlimited = execute_semantic_diff(&diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        500,
        paging(),
    ))?;
    let full_total = unlimited.data.total;
    assert!(
        full_total >= 2,
        "fixture must produce at least 2 function-level changes: {full_total}"
    );

    // Now request only 1 per page — guarantees truncation regardless.
    let paged = execute_semantic_diff(&diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        1,
        paging(),
    ))?;
    assert_eq!(
        paged.total,
        Some(full_total),
        "paged total counts pre-truncation ({full_total})"
    );
    assert_eq!(paged.truncated, Some(true));
    assert!(
        paged.data.changes.len() <= 1,
        "page size respects max_results: got {}",
        paged.data.changes.len()
    );
    Ok(())
}

#[test]
fn semantic_diff_base_and_target_locations_populated_correctly() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        200,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;

    for change in &out.data.changes {
        match change.change_type.as_str() {
            "added" => {
                assert!(
                    change.base_location.is_none(),
                    "added must have no baseLocation: {change:?}"
                );
                assert!(
                    change.target_location.is_some(),
                    "added must have targetLocation: {change:?}"
                );
            }
            "removed" => {
                assert!(
                    change.base_location.is_some(),
                    "removed must have baseLocation: {change:?}"
                );
                assert!(
                    change.target_location.is_none(),
                    "removed must have no targetLocation: {change:?}"
                );
            }
            "modified" | "signature_changed" | "renamed" => {
                assert!(
                    change.base_location.is_some(),
                    "{} must have baseLocation: {change:?}",
                    change.change_type
                );
                assert!(
                    change.target_location.is_some(),
                    "{} must have targetLocation: {change:?}",
                    change.change_type
                );
            }
            other => panic!("unexpected change_type: {other}"),
        }

        // Every populated location must carry a `file://` URI and a
        // non-empty language string. This locks the wire-format contract.
        if let Some(ref loc) = change.base_location {
            assert!(
                loc.file_uri.starts_with("file://"),
                "baseLocation.file_uri must be a file URI: {loc:?}"
            );
            assert!(!loc.language.is_empty(), "language must be populated");
        }
        if let Some(ref loc) = change.target_location {
            assert!(
                loc.file_uri.starts_with("file://"),
                "targetLocation.file_uri must be a file URI: {loc:?}"
            );
            assert!(!loc.language.is_empty(), "language must be populated");
        }
    }

    Ok(())
}

// ============================================================================
// DB20 wire-contract deterministic tests (Codex followup 2026-04-15)
//
// These five tests lock the five change_type strings and the next_page_token
// pagination field deterministically. The main test suite above uses the
// three-change fixture where the rename heuristic may or may not collapse
// added_only/removed_only into a rename, leaving those branches only
// partially covered. The fixtures below are engineered so that exactly one
// branch fires per test.
// ============================================================================

/// Wire-contract: `added` change_type fires when a function exists only in
/// the target and there is no removed function for the rename heuristic to
/// pair it with. Locks the "added" string in the wire DTO.
#[test]
fn semantic_diff_wire_contract_added() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_pure_added_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        SemanticDiffFilters {
            change_types: vec![ChangeType::Added],
            symbol_kinds: vec!["function".to_string()],
        },
        false,
        500,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;
    let names: Vec<&str> = out
        .data
        .changes
        .iter()
        .map(|c| c.symbol_name.as_str())
        .collect();
    assert!(
        names.contains(&"new_function_only_in_target"),
        "expected new_function_only_in_target as 'added'; got {names:?}"
    );
    assert!(
        out.data.changes.iter().all(|c| c.change_type == "added"),
        "change_types=[Added] filter must exclude non-added; got {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.change_type)
            .collect::<Vec<_>>()
    );
    // Summary counter must reflect the added record.
    assert!(out.data.summary.added >= 1, "summary.added must be ≥1");
    Ok(())
}

/// Wire-contract: `removed` change_type fires when a function exists only in
/// the base and there is no added function for the rename heuristic to pair
/// it with. Locks the "removed" string in the wire DTO.
#[test]
fn semantic_diff_wire_contract_removed() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_pure_removed_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        SemanticDiffFilters {
            change_types: vec![ChangeType::Removed],
            symbol_kinds: vec!["function".to_string()],
        },
        false,
        500,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;
    let names: Vec<&str> = out
        .data
        .changes
        .iter()
        .map(|c| c.symbol_name.as_str())
        .collect();
    assert!(
        names.contains(&"function_only_in_base"),
        "expected function_only_in_base as 'removed'; got {names:?}"
    );
    assert!(
        out.data.changes.iter().all(|c| c.change_type == "removed"),
        "change_types=[Removed] filter must exclude non-removed; got {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.change_type)
            .collect::<Vec<_>>()
    );
    // Summary counter must reflect the removed record.
    assert!(out.data.summary.removed >= 1, "summary.removed must be ≥1");
    Ok(())
}

/// Wire-contract: `renamed` change_type fires deterministically.
///
/// The rename fixture has one removed function (old_name_alpha) and one added
/// function (new_name_alpha). With signatures=None on both sides the heuristic
/// produces sig_score=1.0 and confidence=0.91 ≥ 0.9 threshold, so they are
/// always paired as a rename. This locks the "renamed" string in the wire DTO.
#[test]
fn semantic_diff_wire_contract_renamed() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_rename_fixture()?;
    let args = diff_args(
        fx.root(),
        &base,
        &target,
        SemanticDiffFilters {
            change_types: vec![ChangeType::Renamed],
            symbol_kinds: vec!["function".to_string()],
        },
        false,
        500,
        paging(),
    );
    let out = execute_semantic_diff(&args)?;
    assert!(
        !out.data.changes.is_empty(),
        "rename fixture must produce at least one 'renamed' record; \
         got empty changes after filter"
    );
    assert!(
        out.data.changes.iter().all(|c| c.change_type == "renamed"),
        "change_types=[Renamed] filter must exclude non-renamed; got {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.change_type)
            .collect::<Vec<_>>()
    );
    // The target name must be new_name_alpha (post-rename identity).
    let has_target = out
        .data
        .changes
        .iter()
        .any(|c| c.symbol_name == "new_name_alpha");
    assert!(
        has_target,
        "renamed record must have symbol_name==new_name_alpha; got {:?}",
        out.data
            .changes
            .iter()
            .map(|c| &c.symbol_name)
            .collect::<Vec<_>>()
    );
    // Both base and target locations must be populated for a rename.
    for c in &out.data.changes {
        assert!(
            c.base_location.is_some(),
            "renamed record must have baseLocation: {c:?}"
        );
        assert!(
            c.target_location.is_some(),
            "renamed record must have targetLocation: {c:?}"
        );
    }
    // Summary counter must reflect the renamed record.
    assert!(out.data.summary.renamed >= 1, "summary.renamed must be ≥1");
    Ok(())
}

/// Wire-contract: `signature_changed` is the intended change_type when a
/// function's signature changes between commits.
///
/// **Rust plugin limitation**: the sqry Rust plugin currently does not emit
/// function signatures in the graph (signatures are `None` for all Rust
/// functions). The comparator's signature-change detection path at
/// `sqry-db/src/comparative/diff.rs:393-433` compares `signature_before` vs
/// `signature_after`, but because both are always `None` for Rust functions,
/// `signature_changed` never fires for Rust fixtures. Changing a Rust
/// function's parameter list therefore produces `modified` (body/line change),
/// not `signature_changed`.
///
/// This test is ignored until a language plugin that emits signatures is used
/// or until the Rust plugin gains signature extraction. The ignore preserves
/// the test intent as documentation rather than deleting it.
#[test]
#[ignore = "Rust plugin does not emit function signatures; \
            signature_changed cannot fire for Rust fixtures. \
            Re-enable when the Rust plugin emits signatures or \
            when a Python/TypeScript fixture is substituted."]
fn semantic_diff_wire_contract_signature_changed() -> Result<()> {
    // When the Rust (or another) plugin emits signatures, build a fixture
    // here where only the parameter list changes between base and target,
    // keeping the body identical. The comparator should classify that as
    // `signature_changed`, not `modified`. Assert:
    //   out.data.changes.iter().any(|c| c.change_type == "signature_changed")
    //   && out.data.summary.signature_changed >= 1
    Ok(())
}

/// Wire-contract: `next_page_token` is `Some` when `pagination.size` is
/// smaller than the number of changes in the response.
///
/// The three-change fixture produces ≥2 function-level changes. Requesting
/// `size=1` forces the paginator to produce a non-None token for the
/// remainder of the results.
#[test]
fn semantic_diff_wire_contract_next_page_token() -> Result<()> {
    init_caches();
    let (fx, base, target) = build_three_change_fixture()?;

    // First verify the fixture has ≥2 changes (sanity guard).
    let unlimited = execute_semantic_diff(&diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        500,
        PaginationArgs {
            offset: 0,
            size: 500,
        },
    ))?;
    assert!(
        unlimited.data.total >= 2,
        "fixture must have ≥2 function-level changes for pagination test: {}",
        unlimited.data.total
    );

    // Request page of size 1 — guarantees a next_page_token.
    let paged = execute_semantic_diff(&diff_args(
        fx.root(),
        &base,
        &target,
        function_only_filter(),
        false,
        500,
        PaginationArgs { offset: 0, size: 1 },
    ))?;
    assert!(
        paged.next_page_token.is_some(),
        "next_page_token must be Some when page size (1) < total changes ({}); \
         got None",
        paged.data.total
    );
    // Page must contain exactly 1 record.
    assert_eq!(
        paged.data.changes.len(),
        1,
        "page of size 1 must contain exactly 1 change; got {}",
        paged.data.changes.len()
    );
    Ok(())
}
