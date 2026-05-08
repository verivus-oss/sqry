//! SGA03 — CLI integration tests for `FilesystemGraphProvider`.
//!
//! These tests assert that `sqry query` now routes graph acquisition through
//! the shared `FilesystemGraphProvider`:
//!
//! 1. A successful query against an indexed workspace returns the matching
//!    symbol's location (provider acquires the graph cleanly, executor runs
//!    on the preloaded graph).
//! 2. A subdirectory invocation still emits the `filtered to <subdir>`
//!    diagnostic — proving the provider preserves the CLI's ancestor-index
//!    scope semantics.
//! 3. A file-path invocation filters results to that exact file.
//! 4. A non-existent path is rejected by the provider's strict path policy
//!    *before* any graph load is attempted; the CLI must emit a path error
//!    and never print the `Used index` summary that only fires on successful
//!    graph acquisition.

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Build a minimal Rust workspace with an indexed graph using the real
/// `sqry index` CLI command. Each test creates its own temp directory so
/// fixtures stay hermetic.
fn build_indexed_workspace() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn func_alpha() -> u32 { 1 }
pub fn func_beta() -> u32 { 2 }
pub fn func_gamma() -> u32 { 3 }
"#,
    )
    .expect("write lib.rs");
    fs::write(
        root.join("src/extra.rs"),
        r#"
pub fn other_function() -> u32 { 10 }
"#,
    )
    .expect("write extra.rs");

    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .env("NO_COLOR", "1")
        .assert()
        .success();

    tmp
}

/// SGA03 acceptance — successful CLI query goes through the filesystem-backed
/// provider and returns the expected match.
#[test]
fn cli_query_uses_filesystem_acquirer_for_existing_graph() {
    let tmp = build_indexed_workspace();
    Command::new(sqry_bin())
        .arg("--semantic")
        .arg("query")
        .arg("name:func_alpha")
        .arg(tmp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("func_alpha"));
}

/// Subdirectory invocation must still report `filtered to ...` — the
/// provider's workspace discovery preserves CLI ancestor-index scope.
#[test]
fn cli_query_from_subdir_preserves_ancestor_scope_filter() {
    let tmp = build_indexed_workspace();
    let subdir = tmp.path().join("src");
    // Use `--semantic` so the CLI does not classify a bare `name:` query as
    // text-only and short-circuit the filtered-to diagnostic.
    Command::new(sqry_bin())
        .arg("--semantic")
        .arg("query")
        .arg("name:func_alpha")
        .arg(&subdir)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        // The query result must still come back …
        .stdout(predicate::str::contains("func_alpha"))
        // … and the diagnostic must report scope filtering through the
        // ancestor index. The exact wording is "filtered to src/**".
        .stderr(predicate::str::contains("filtered to"));
}

/// File-path invocation must apply the existing file-scope filter so only
/// the matching file's symbols are returned.
#[test]
fn cli_query_file_scope_preserves_exact_file_filter() {
    let tmp = build_indexed_workspace();
    let file_path = tmp.path().join("src/lib.rs");
    Command::new(sqry_bin())
        .arg("--semantic")
        .arg("query")
        .arg("kind:function")
        .arg(&file_path)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        // Symbols defined in src/lib.rs are present.
        .stdout(predicate::str::contains("func_alpha"))
        // Symbols defined in the *other* file (extra.rs) must be filtered
        // out by the file-scope predicate.
        .stdout(predicate::str::contains("other_function").not());
}

/// SGA03 strict-path tightening — non-existent paths fail before any graph
/// load. The provider returns `InvalidPath`; the CLI must not emit the
/// "Used index" summary that only fires on successful acquisition.
#[test]
fn cli_invalid_path_rejected_before_graph_load() {
    let tmp = TempDir::new().expect("tempdir");
    let bogus = tmp.path().join("does/not/exist");

    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function")
        .arg(&bogus)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "non-existent path must fail (stderr={stderr})"
    );
    assert!(
        stderr.contains("invalid path") || stderr.to_lowercase().contains("does not exist"),
        "expected invalid-path diagnostic, got: {stderr}"
    );
    assert!(
        !stderr.contains("Used index") && !stderr.contains("Using index from"),
        "no `Used index` line should appear when path validation rejects the request: {stderr}"
    );
}

/// SGA03 Major #4 fix — `sqry query --text` must continue to work on
/// unindexed directories. Pre-fix `acquire_graph_for_cli` ran
/// unconditionally and would have failed with `NoGraph` for any path
/// without a `.sqry/graph` ancestor. Text mode is now graph-free.
#[test]
fn cli_text_mode_does_not_require_graph() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        "fn alpha() {}\nfn lookup_needle() {}\n",
    )
    .expect("write lib.rs");

    // Deliberately do NOT run `sqry index` — text mode must work without
    // any graph artifact present.
    let output = Command::new(sqry_bin())
        .arg("--text")
        .arg("query")
        .arg("needle")
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "--text on an unindexed directory must succeed; stderr={stderr}, stdout={stdout}"
    );
    assert!(
        stdout.contains("lookup_needle") || stdout.contains("needle"),
        "expected text match for `needle`, got stdout={stdout}"
    );
    assert!(
        !stderr.contains("No graph found") && !stderr.contains("Run `sqry index"),
        "text mode must not require a graph; stderr={stderr}"
    );
}

/// SGA03 Major #1 (codex iter2) — the default hybrid mode (neither
/// `--text` nor `--semantic`) must execute the semantic attempt against
/// the provider-acquired graph, not re-load it through the executor's
/// disk-backed cache. We can't directly observe `execute_on_preloaded_graph`
/// from the CLI binary, so this test stands as the integration-level
/// proof that the hybrid path produces the same successful result as
/// the explicit semantic path (the `cli_query_uses_filesystem_acquirer_for_existing_graph`
/// test pins the semantic-only branch). The unit-level proof lives in
/// `sqry-core/src/search/fallback.rs::tests::semantic_only_with_preloaded_graph_uses_caller_graph`.
#[test]
fn cli_hybrid_mode_executes_against_provider_acquired_graph() {
    let tmp = build_indexed_workspace();
    Command::new(sqry_bin())
        .arg("query")
        // No `--semantic` / `--text` — hybrid auto-classify path.
        .arg("name:func_alpha")
        .arg(tmp.path())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("func_alpha"));
}

/// SGA03 regression — error precedence between strict path validation
/// and query parse validation.
///
/// `validate_query_path_strict` runs in `run_query` BEFORE the parse
/// probe in `run_query_non_session` / `run_query_with_session`, so an
/// invalid path must produce the path diagnostic (exit 1) and short-
/// circuit before the query is ever parsed. This pins the precedence
/// contract documented in `probe_validate_query_syntax` so a future
/// reordering can't silently flip the user-visible error.
#[test]
fn cli_invalid_path_takes_precedence_over_invalid_query() {
    let tmp = TempDir::new().expect("tempdir");
    let bogus = tmp.path().join("does/not/exist");

    let output = Command::new(sqry_bin())
        .arg("query")
        // Query has BOTH a parse error (unmatched paren) and would also
        // fail registry validation; either would normally exit 2. The
        // path is also invalid — that error must win.
        .arg("(kind:invalid_kind")
        .arg(&bogus)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    assert!(
        !output.status.success(),
        "invalid path + invalid query must fail (stderr={stderr})"
    );
    // Path errors map through `handle_other_error` → exit 1.
    assert_eq!(
        code,
        Some(1),
        "invalid path must exit 1 (path beats query); stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid path") || stderr.to_lowercase().contains("does not exist"),
        "expected invalid-path diagnostic to win, got: {stderr}"
    );
    // Parse-error diagnostics must NOT appear: path validation short-
    // circuited the parse probe.
    assert!(
        !stderr.contains("sqry::parse") && !stderr.contains("sqry::validation"),
        "no parse/validation diagnostic should appear when path is invalid: {stderr}"
    );
}

/// SGA03 regression — invalid query syntax against a valid but
/// unindexed path must surface as a parse error (exit 2), not as the
/// provider's `NoGraph` acquisition error (exit 1).
///
/// CLI_INTEGRATION.md §4 Exit behavior: invalid query syntax remains
/// a query-parse failure. Without the parse probe added by this fix,
/// `acquire_graph_for_cli` would see the unindexed directory first
/// and emit `No graph found for ...` (exit 1), masking the actual
/// query error.
#[test]
fn cli_invalid_query_reported_as_parse_error_when_path_is_unindexed() {
    let tmp = TempDir::new().expect("tempdir");
    // Valid path, but deliberately NOT indexed (no `.sqry/graph/`).
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
    fs::write(tmp.path().join("src/lib.rs"), "fn alpha() {}\n").expect("write");

    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("(kind:function") // Unmatched paren — parse error.
        .arg(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    assert!(
        !output.status.success(),
        "invalid query must fail (stderr={stderr})"
    );
    assert_eq!(
        code,
        Some(2),
        "invalid query must exit 2 (parse error), not 1 (no-graph); stderr={stderr}"
    );
    assert!(
        stderr.contains("sqry::parse") || stderr.contains("Unmatched"),
        "expected parse-error diagnostic, got: {stderr}"
    );
    // The "no graph found" diagnostic must NOT appear — the parse probe
    // ran first and short-circuited graph acquisition.
    assert!(
        !stderr.contains("No graph found") && !stderr.contains("Run `sqry index"),
        "parse probe must short-circuit graph acquisition: {stderr}"
    );
}

/// SGA03 Major #4 fix (codex iter3) — `sqry query --text` must succeed
/// even when a `.sqry/graph` artifact is present whose persisted plugin
/// selection cannot be honored by the running binary (e.g. it lists ids
/// the registry no longer knows). Pre-fix `run_query_text_only` built
/// its executor through `create_executor_with_plugins_for_cli`, which
/// resolved the manifest's `active_plugin_ids` and failed with
/// `unknown plugin ids: ...`. Text mode is a ripgrep scan and must not
/// touch the manifest at all.
#[test]
fn cli_text_mode_succeeds_with_incompatible_graph_manifest() {
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/lib.rs"), "pub fn func_alpha() {}\n").expect("write lib.rs");

    // Synthesize a `.sqry/graph/manifest.json` whose plugin selection
    // references a plugin id the running binary's registry does not
    // know. The manifest must satisfy `GraphStorage::exists()` (file
    // present) and `Manifest::load`'s serde deserializer (all required
    // fields populated). We do NOT need the snapshot to load — the
    // failure path being tested is hit by the manifest read alone.
    let graph_dir = root.join(".sqry/graph");
    fs::create_dir_all(&graph_dir).expect("mkdir .sqry/graph");
    let manifest = serde_json::json!({
        "schema_version": 1,
        "snapshot_format_version": 2,
        "built_at": "2026-05-08T00:00:00+00:00",
        "root_path": root.to_string_lossy(),
        "node_count": 0,
        "edge_count": 0,
        "snapshot_sha256": "0".repeat(64),
        "build_provenance": {
            "sqry_version": "13.0.0",
            "build_timestamp": "2026-05-08T00:00:00+00:00",
            "build_command": "cli:index",
        },
        "plugin_selection": {
            "active_plugin_ids": ["nonexistent-lang-plugin"]
        }
    });
    fs::write(
        graph_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).expect("manifest json"),
    )
    .expect("write manifest.json");

    let output = Command::new(sqry_bin())
        .arg("--text")
        .arg("query")
        .arg("func_alpha")
        .arg(root)
        .arg("--limit")
        .arg("1")
        .env("NO_COLOR", "1")
        .output()
        .expect("sqry query --text should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "text-only mode must succeed even when persisted plugin selection is incompatible. stderr={stderr} stdout={stdout}"
    );
    assert!(
        stdout.contains("func_alpha"),
        "expected text match for func_alpha; got stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("unknown plugin ids"),
        "text mode must not resolve persisted plugin selection; stderr={stderr}"
    );
}

/// SGA03 Major #3 fix — pipeline-style queries (`base | aggregation`) and
/// join-style queries (`LHS CALLS RHS`) must be subject to the same
/// strict invalid-path validation as the regular semantic path. Before
/// this fix, a malformed pipeline against a non-existent path would
/// reach the executor and produce a "no pipeline matched" or "graph not
/// found" diagnostic instead of `invalid path: ... does not exist`.
#[test]
fn cli_invalid_path_rejected_before_pipeline_dispatch() {
    let tmp = TempDir::new().expect("tempdir");
    let bogus = tmp.path().join("does/not/exist");

    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function | count")
        .arg(&bogus)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "pipeline against non-existent path must fail; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid path") || stderr.to_lowercase().contains("does not exist"),
        "expected invalid-path diagnostic before pipeline dispatch, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// SGA07 — additional plugin-selection / wire-shape parity tests.
// ---------------------------------------------------------------------------

/// SGA07 — manifests advertising an unknown plugin id MUST surface as
/// the dedicated `IncompatibleGraph` diagnostic (not a generic load
/// failure), so operators can distinguish "binary too old / plugin
/// removed" from "snapshot corrupted". Mirrors the standalone-MCP
/// `standalone_mcp_existing_disk_snapshot_uses_provider` test from the
/// MCP side, but exercises the CLI surface end-to-end.
///
/// We intentionally use the SEMANTIC path (`--semantic`) because
/// `--text` deliberately bypasses the manifest plugin-compat check —
/// see `cli_text_mode_succeeds_with_incompatible_graph_manifest`.
#[test]
fn cli_query_unknown_plugin_id_returns_incompatible_graph() {
    let tmp = build_indexed_workspace();
    let manifest_path = tmp.path().join(".sqry/graph/manifest.json");
    let manifest_bytes = fs::read(&manifest_path).expect("read manifest");
    let mut manifest_json: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).expect("parse manifest");
    let plugin_section = manifest_json
        .get_mut("plugin_selection")
        .expect("manifest must record plugin_selection after sqry index");
    let active_ids = plugin_section
        .get_mut("active_plugin_ids")
        .and_then(|v| v.as_array_mut())
        .expect("active_plugin_ids must be an array");
    active_ids.push(serde_json::Value::String(
        "sga07-fake-plugin-that-does-not-exist".to_string(),
    ));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest_json).expect("serialize manifest"),
    )
    .expect("write manifest");

    let output = Command::new(sqry_bin())
        .arg("--semantic")
        .arg("query")
        .arg("name:func_alpha")
        .arg(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "unknown plugin id manifest must reject the query; stdout={stdout} stderr={stderr}"
    );
    // The provider-mapped diagnostic is one of:
    //   - "Incompatible graph"
    //   - "unknown plugin ids"
    //   - the offending plugin id "sga07-fake-plugin-that-does-not-exist"
    // All three signal the IncompatibleGraph class to the operator,
    // distinct from a generic load-failure or path-validation error.
    let lower_stderr = stderr.to_lowercase();
    assert!(
        lower_stderr.contains("incompatible graph")
            || lower_stderr.contains("unknown plugin")
            || stderr.contains("sga07-fake-plugin-that-does-not-exist"),
        "expected IncompatibleGraph diagnostic surface, got stderr={stderr}"
    );
}

/// SGA07 — `sqry --json query` must continue to emit the existing
/// top-level JSON keys after the SGA migration. Wire-shape regression
/// guard for the CLI path; `standalone_mcp_readonly_tools_preserve_wire_shape`
/// covers the MCP side.
///
/// We assert presence of stable, externally-visible fields (`results`
/// and `total`) rather than full snapshot equality so this test is not
/// brittle against orthogonal changes to the textual rendering of an
/// individual result. The point is "no SGA-driven schema breakage".
#[test]
fn cli_query_json_output_schema_unchanged() {
    let tmp = build_indexed_workspace();
    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("--semantic")
        .arg("query")
        .arg("name:func_alpha")
        .arg(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry --json query");

    assert!(
        output.status.success(),
        "sqry --json query must succeed; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("CLI --json output must be parseable: {e}; stdout={stdout}"));

    // The historical contract is an object with `query`, `results`,
    // and `stats` top-level keys (matching the executor's serialized
    // envelope). SGA must not rename, drop, or reshape these.
    assert!(
        parsed.is_object(),
        "CLI --json output must be a JSON object; got={parsed}"
    );
    let obj = parsed.as_object().unwrap();
    for required_key in ["query", "results", "stats"] {
        assert!(
            obj.contains_key(required_key),
            "CLI --json output MUST keep the `{required_key}` top-level key; got keys={:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        obj.get("results")
            .map(serde_json::Value::is_array)
            .unwrap_or(false),
        "`results` must be an array",
    );
    // The query must still surface the indexed symbol.
    assert!(
        stdout.contains("func_alpha"),
        "CLI --json query must still surface func_alpha; stdout={stdout}"
    );
}
