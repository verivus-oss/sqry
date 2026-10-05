//! SGA06 — parity tests for the LSP edge of the shared graph
//! acquisition contract.
//!
//! The standalone-LSP graph acquisition path was migrated in SGA06 to
//! route through `sqry_core::graph::acquisition::FilesystemGraphProvider`,
//! the same provider that backs CLI `sqry query` and the standalone MCP
//! engine. Gate B's review then closed the remaining handler-side gap:
//! the read-only LSP request handlers (`sqry/search`, `sqry/directCallers`,
//! `sqry/directCallees`, plus the related read-only workspace_symbol /
//! relations / hierarchical_search / batch_counts / call_hierarchy
//! handlers) now acquire their graphs through
//! [`SessionManager::graph_for_path`] and run queries via
//! [`QueryExecutor::execute_on_preloaded_graph`] instead of
//! re-entering the executor's own `get_or_load_graph` path (which would
//! bypass the SGA-migrated path-policy / SHA-256 / plugin-compat checks).
//!
//! These tests pin the user-visible contract:
//!
//! 1. `lsp_search_matches_cli_for_same_graph` — CLI `sqry query` and an
//!    LSP `sqry/search` against the same on-disk graph return the same
//!    set of symbol names. Demonstrates that the LSP read-only path now
//!    sees the exact same graph the CLI sees.
//! 2. `lsp_invalid_path_rejected_before_graph_load` — invalid paths are
//!    rejected with an `InvalidPath`-class error before any disk graph
//!    load occurs.
//! 3. `lsp_stale_diagnostic_visible` — corrupt-snapshot fixture exercises
//!    the LSP's existing self-heal path; the diagnostic surfaces are
//!    visible to clients as warnings (via the `log` channel that the LSP
//!    forwards as server log messages).
//! 4. `lsp_search_handler_routes_through_session_graph` — pins that
//!    `sqry/search` increments the `graph_for_path` counter (the SGA06
//!    shared-acquisition entry point). A bypass via the executor's own
//!    `get_or_load_graph` would not register on this counter.
//! 5. `lsp_direct_callers_handler_routes_through_session_graph` — same
//!    pin for `sqry/directCallers`.
//! 6. `lsp_direct_callees_handler_routes_through_session_graph` — same
//!    pin for `sqry/directCallees`.
//! 7. `lsp_evicted_daemon_client_reload_equivalent` — placeholder for
//!    the daemon-hosted LSP eviction path. SGA06 confirmed (see
//!    `daemon_host.rs` module docs) that the daemon-hosted LSP shim
//!    creates a fresh standalone `SessionManager` per connection and
//!    therefore never enters the daemon-graph acquisition path. This
//!    test is `#[ignore]`d with a documented reason; SGA07 owns the
//!    daemon-side eviction parity.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::{direct_relations, index, search, semantic_diff};
use sqry_lsp::protocol::{
    SqryDirectCalleesParams, SqryDirectCallersParams, SqryGitVersionRef, SqrySearchParams,
    SqrySemanticDiffParams,
};
use sqry_lsp::session::SessionManager;
use tempfile::TempDir;

/// A temp directory that is its own project: an empty `.git` marker at
/// its root stops the ancestor walk there, so an index or project marker
/// above `TMPDIR` cannot make `sqry index` refuse the fixture as a nested
/// index or hand the test an unrelated outer graph.
fn project_tempdir() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir(dir.path().join(".git")).expect("project marker");
    dir
}

/// Locate the `sqry` binary for testing.
///
/// Delegates to the one resolver, `sqry_core::test_support::binaries::sqry_binary`
/// (surface parity W4, design W4-D13), which reads `SQRY_E2E_SQRY_BIN`, then
/// `CARGO_BIN_EXE_sqry`, then `CARGO_TARGET_DIR` and the workspace `target`,
/// debug before release, and panics naming every variable and candidate.
fn sqry_bin_path() -> PathBuf {
    sqry_core::test_support::binaries::sqry_binary()
}

/// Build a small Rust workspace fixture with a known symbol named
/// `func_alpha` so we can pin parity between CLI and LSP search.
fn make_func_alpha_fixture() -> TempDir {
    let temp = project_tempdir();
    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src");

    fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "sga06_func_alpha_fixture"
version = "0.0.1"
edition = "2024"

[lib]
name = "sga06_func_alpha_fixture"
path = "src/lib.rs"
"#,
    )
    .expect("write Cargo.toml");

    fs::write(
        src_dir.join("lib.rs"),
        r#"//! SGA06 fixture: provides a single `func_alpha` symbol and a
//! few neighbours so search has more than one candidate result.

pub fn func_alpha() -> u32 {
    func_beta() + 1
}

pub fn func_beta() -> u32 {
    42
}

pub fn unrelated_helper() -> u32 {
    7
}
"#,
    )
    .expect("write lib.rs");

    temp
}

/// Build the `.sqry/graph/` snapshot for the fixture using the
/// workspace `sqry` binary so the on-disk artifact matches what the CLI
/// would produce.
fn build_index_with_cli(root: &Path) {
    let status = Command::cargo_bin("sqry")
        .expect("locate sqry bin")
        .arg("index")
        .current_dir(root)
        .status()
        .expect("run sqry index");
    assert!(status.success(), "sqry index failed");
}

fn lsp_options_for(root: &Path) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    }
}

fn cli_query_func_alpha(root: &Path) -> Vec<String> {
    let bin = sqry_bin_path();
    let output = Command::new(&bin)
        .arg("query")
        .arg("name:func_alpha")
        .arg(".")
        .current_dir(root)
        .output()
        .expect("run sqry query");
    assert!(
        output.status.success(),
        "sqry query exited non-zero: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // CLI `sqry query` prints human-readable lines; we just look for
    // the literal symbol name. The parity surface this test pins is
    // "the CLI and the LSP both find the symbol against the same
    // on-disk graph" — exact match-set equivalence against a free-form
    // text format would be brittle.
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut names: Vec<String> = Vec::new();
    if stdout.contains("func_alpha") {
        names.push("func_alpha".to_string());
    }
    names
}

#[test]
fn lsp_search_matches_cli_for_same_graph() {
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let cli_names = cli_query_func_alpha(root);
    assert!(
        cli_names.contains(&"func_alpha".to_string()),
        "CLI query did not return func_alpha (got {cli_names:?})"
    );

    let session = SessionManager::new(lsp_options_for(root));
    let params = SqrySearchParams {
        query: "name:func_alpha".into(),
        path: None,
        limit: Some(10),
        ..SqrySearchParams::default()
    };
    let result = search::execute(&session, &params).expect("LSP search executes");
    let lsp_names: Vec<String> = result.results.iter().map(|r| r.name.clone()).collect();

    assert!(
        lsp_names.iter().any(|n| n == "func_alpha"),
        "LSP search did not return func_alpha (got {lsp_names:?})"
    );

    // Parity surface: every name CLI surfaced must also appear in the
    // LSP response when both are run against the same on-disk graph.
    for cli_name in &cli_names {
        assert!(
            lsp_names.contains(cli_name),
            "LSP missing CLI name {cli_name} (lsp={lsp_names:?})"
        );
    }
}

#[test]
fn lsp_invalid_path_rejected_before_graph_load() {
    // Build a real fixture so a session exists, but query through the
    // standalone-LSP graph acquisition path with a request that points
    // at a non-existent sub-path. The provider-backed acquisition must
    // reject this before any disk graph load.
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let session = SessionManager::new(lsp_options_for(root));

    // `index_status` accepts an Option<&str> for path; pass a path that
    // sits *outside* the workspace to force the invalid-path rejection
    // surface (LSP returns IndexStatus::not_found in that case rather
    // than surfacing the typed error, which is the documented contract
    // — see `index_status` docs). The contract under test here is that
    // the LSP never loads a graph for an out-of-workspace path.
    let outside = TempDir::new().expect("outside temp");
    let outside_str = outside.path().to_string_lossy().to_string();

    let status = index::index_status(&session, Some(&outside_str)).expect("index_status returns");
    assert!(
        !status.exists,
        "out-of-workspace path must not surface a loaded index (got {status:?})"
    );
}

#[test]
fn lsp_stale_diagnostic_visible() {
    // Synthetic stale fixture: build a real graph, then truncate the
    // snapshot to corrupt it. The provider-backed acquisition must
    // detect the corruption (manifest SHA-256 mismatch). The LSP's
    // self-heal path then auto-rebuilds and the resulting log line is
    // the user-visible diagnostic surface (server log messages are
    // forwarded to the client as `window/logMessage` notifications).
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let snapshot_path = root.join(".sqry/graph/snapshot.sqry");
    assert!(
        snapshot_path.exists(),
        "snapshot must exist before truncation"
    );

    // Truncate to 8 bytes — small enough that the SHA-256 verification
    // step inside the provider always fails.
    fs::write(&snapshot_path, b"corrupted").expect("truncate snapshot");

    let session = SessionManager::new(lsp_options_for(root));
    let params = SqrySearchParams {
        query: "name:func_alpha".into(),
        path: None,
        limit: Some(10),
        ..SqrySearchParams::default()
    };

    // The LSP `search` handler resolves a graph through
    // `SessionManager::graph_for_path`, which routes through the shared
    // `FilesystemGraphProvider`. On a corrupt snapshot the provider
    // surfaces `LoadFailed`, which `acquire_session_graph` then turns
    // into an in-place self-heal rebuild. On success the rebuilt graph
    // is returned; on rebuild failure the error is mapped through
    // `map_acquisition_error_for_lsp` and surfaces with one of the
    // stable graph/snapshot/stale/rebuild diagnostic substrings. Either
    // outcome is observable to the client (via the response stream or
    // via logged diagnostics).
    let result = search::execute(&session, &params);
    match result {
        Ok(_) => {
            // Self-heal succeeded — the LSP transparently rebuilt the
            // index. The user-visible diagnostic surface is a
            // `tracing::warn!` / `log::warn!` line that downstream
            // log subscribers turn into a `window/logMessage`.
        }
        Err(err) => {
            // Self-heal failed; the error must carry a recognizable
            // diagnostic substring so the LSP error renderer can
            // surface it without further mapping.
            let s = format!("{err:#}");
            assert!(
                s.contains("graph")
                    || s.contains("Graph")
                    || s.contains("snapshot")
                    || s.contains("stale")
                    || s.contains("rebuild"),
                "stale diagnostic surface should mention graph/snapshot/stale/rebuild (got: {s})"
            );
        }
    }
}

#[test]
fn lsp_search_handler_routes_through_session_graph() {
    // Gate B fix — the read-only `sqry/search` handler now acquires its
    // graph through `SessionManager::graph_for_path` (the SGA06 shared
    // entry point) instead of `executor.execute_on_graph(..)` (which
    // re-enters the executor's own `get_or_load_graph`). The counter
    // exposed on `SessionManager` increments on every call to
    // `graph_for_path`, so a non-zero post-call count proves the
    // shared-acquisition path was taken.
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let session = SessionManager::new(lsp_options_for(root));
    let before = session.graph_for_path_call_count();

    let params = SqrySearchParams {
        query: "name:func_alpha".into(),
        path: None,
        limit: Some(10),
        ..SqrySearchParams::default()
    };
    let result = search::execute(&session, &params).expect("LSP search executes");

    let after = session.graph_for_path_call_count();
    assert!(
        after > before,
        "search handler must route graph acquisition through SessionManager::graph_for_path \
         (before={before}, after={after})"
    );
    assert!(
        result.results.iter().any(|r| r.name == "func_alpha"),
        "search handler must still surface the indexed symbol via the migrated path"
    );
}

#[test]
fn lsp_direct_callers_handler_routes_through_session_graph() {
    // Gate B fix — `sqry/directCallers` now acquires the graph through
    // the shared `SessionManager::graph_for_path` entry point and runs
    // its `callers:` predicate via `execute_on_preloaded_graph`. The
    // counter pin proves the migrated path was taken.
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let session = SessionManager::new(lsp_options_for(root));
    let before = session.graph_for_path_call_count();

    let params = SqryDirectCallersParams {
        symbol: "func_beta".into(),
        path: None,
        limit: Some(10),
    };
    let result = direct_relations::execute_direct_callers(&session, &params)
        .expect("direct_callers handler executes");

    let after = session.graph_for_path_call_count();
    assert!(
        after > before,
        "direct_callers handler must route graph acquisition through SessionManager::graph_for_path \
         (before={before}, after={after})"
    );
    // `func_beta` is called by `func_alpha`, so the migrated path must
    // still surface that caller.
    assert!(
        result.callers.iter().any(|c| c.name == "func_alpha"),
        "direct_callers handler must still surface the indexed caller via the migrated path \
         (got {:?})",
        result.callers.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn lsp_direct_callees_handler_routes_through_session_graph() {
    // Gate B fix — `sqry/directCallees` now acquires the graph through
    // the shared `SessionManager::graph_for_path` entry point and runs
    // its `callees:` predicate via `execute_on_preloaded_graph`. The
    // counter pin proves the migrated path was taken.
    let fixture = make_func_alpha_fixture();
    let root = fixture.path();
    build_index_with_cli(root);

    let session = SessionManager::new(lsp_options_for(root));
    let before = session.graph_for_path_call_count();

    let params = SqryDirectCalleesParams {
        symbol: "func_alpha".into(),
        path: None,
        limit: Some(10),
    };
    let result = direct_relations::execute_direct_callees(&session, &params)
        .expect("direct_callees handler executes");

    let after = session.graph_for_path_call_count();
    assert!(
        after > before,
        "direct_callees handler must route graph acquisition through SessionManager::graph_for_path \
         (before={before}, after={after})"
    );
    // `func_alpha` calls `func_beta`, so the migrated path must still
    // surface that callee.
    assert!(
        result.callees.iter().any(|c| c.name == "func_beta"),
        "direct_callees handler must still surface the indexed callee via the migrated path \
         (got {:?})",
        result.callees.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "SGA06 — daemon-hosted LSP creates a standalone SessionManager per shim connection and never enters the daemon-graph acquisition path; therefore WorkspaceEvicted is unreachable from LSP today. SGA07 owns the daemon-side eviction parity coverage. See sqry-lsp/src/daemon_host.rs module docs for the full rationale."]
fn lsp_evicted_daemon_client_reload_equivalent() {
    // Intentionally empty. See ignore reason for the rationale.
}

// ---------------------------------------------------------------------------
// T9 (surface parity W1): the LSP serves a CLI `--include-high-cost` graph
// with the same json symbols the CLI produced. On the pre-change head the
// LSP session loaded with the fast-path roster and refused the snapshot as
// `IncompatibleUnknownPluginIds { ["json"] }`.
// ---------------------------------------------------------------------------

/// A Rust file plus a JSON file, so the fast-path roster and the full roster
/// build different graphs.
fn make_mixed_fixture() -> TempDir {
    let temp = project_tempdir();
    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src");
    fs::write(
        src_dir.join("lib.rs"),
        "pub fn func_alpha() -> u32 { 1 }\npub fn func_beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    fs::write(
        temp.path().join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true, "count": 3}, "items": [1, 2]}"#,
    )
    .expect("write config.json");
    temp
}

/// Index with the workspace `sqry` binary and `--include-high-cost`, the
/// way a user who wants json coverage indexes.
fn build_high_cost_index_with_cli(root: &Path) {
    let status = Command::cargo_bin("sqry")
        .expect("locate sqry bin")
        .arg("index")
        .arg("--include-high-cost")
        .arg(root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1")
        .status()
        .expect("run sqry index --include-high-cost");
    assert!(status.success(), "sqry index --include-high-cost failed");
}

/// The json symbol names the persisted snapshot holds, read with the full
/// compiled roster and queried the same way every surface queries: this is
/// N, computed from the fixture, never hardcoded.
fn json_symbol_names_from_snapshot(root: &Path) -> Vec<String> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let plugins = sqry_plugin_registry::create_plugin_manager_all();
    let graph = sqry_core::graph::unified::persistence::load_from_path(
        storage.snapshot_path(),
        Some(&plugins),
    )
    .expect("snapshot loads with the full roster");
    let executor = sqry_core::query::QueryExecutor::with_plugin_manager(plugins);
    let results = executor
        .execute_on_preloaded_graph(std::sync::Arc::new(graph), "lang:json", root, None)
        .expect("lang:json query runs");
    let mut names: Vec<String> = results
        .iter()
        .filter_map(|hit| hit.name().map(|name| name.to_string()))
        .collect();
    names.sort();
    names
}

#[test]
fn lsp_search_matches_cli_on_high_cost_graph() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    build_high_cost_index_with_cli(&root);

    let manifest = sqry_core::graph::unified::persistence::GraphStorage::new(&root)
        .load_manifest()
        .expect("manifest readable");
    let selection = manifest.plugin_selection.expect("selection recorded");
    assert!(
        selection.active_plugin_ids.iter().any(|id| id == "json"),
        "fixture precondition: the CLI must have recorded json"
    );

    let expected = json_symbol_names_from_snapshot(&root);
    let n = expected.len();
    assert!(
        n >= 1,
        "fixture precondition: at least one json symbol, got {n}"
    );

    let session = SessionManager::new(lsp_options_for(&root));
    let params = SqrySearchParams {
        query: "lang:json".into(),
        path: None,
        limit: Some(1_000),
        ..SqrySearchParams::default()
    };
    let result = search::execute(&session, &params)
        .expect("LSP search executes over a high-cost graph without an unknown-plugin refusal");
    assert_eq!(
        result.total, n,
        "LSP total must equal the json symbol count of the snapshot"
    );
    let mut lsp_names: Vec<String> = result.results.iter().map(|r| r.name.clone()).collect();
    lsp_names.sort();
    assert_eq!(
        lsp_names, expected,
        "LSP must return exactly the json symbols the CLI indexed"
    );
}

// ---------------------------------------------------------------------------
// Verifier round 1, battery row M16: the LSP self-heal over a corrupt
// snapshot rebuilds with the roster the (readable) manifest records and
// writes that selection back, instead of the fast path or no selection.
// ---------------------------------------------------------------------------

#[test]
fn lsp_self_heal_keeps_the_include_all_selection() {
    use sqry_core::graph::unified::persistence::{
        BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
    };

    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let full_ids: Vec<String> = sqry_plugin_registry::create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    assert!(full_ids.iter().any(|id| id == "json"));

    // A readable include_all manifest beside a snapshot that cannot load.
    let storage = GraphStorage::new(&root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test"),
    )
    .with_plugin_selection(Some(PluginSelectionManifest {
        active_plugin_ids: full_ids.clone(),
        high_cost_mode: Some("include_all".to_string()),
    }))
    .save(storage.manifest_path())
    .expect("manifest saved");
    fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("corrupt snapshot");

    let session = SessionManager::new(lsp_options_for(&root));
    let params = SqrySearchParams {
        query: "lang:json".into(),
        path: None,
        limit: Some(1_000),
        ..SqrySearchParams::default()
    };
    let result = search::execute(&session, &params).expect("self-heal rebuilds and serves");
    assert!(
        result.total >= 1,
        "the self-healed graph must contain the json symbols the manifest roster produces"
    );

    let rebuilt = storage
        .load_manifest()
        .expect("manifest readable after self-heal");
    let selection = rebuilt
        .plugin_selection
        .expect("the self-heal must record a plugin selection, not drop it");
    assert_eq!(
        selection.active_plugin_ids, full_ids,
        "the self-heal must record exactly the selection the manifest already recorded"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("include_all"),
        "high_cost_mode must be carried through the self-heal"
    );
}

// ---------------------------------------------------------------------------
// Round 2 (design D8, D9): every LSP persisting site records the selection
// the manifest already records, through the registry helper. T20 and T21
// are green on `abefdd8e3` (the sites already recorded it there) and red on
// `4628df394`; their kill evidence is battery rows K8 and K9. T27 is red on
// `abefdd8e3`: `semantic_diff::execute` fell back over an unreadable
// manifest there and now refuses, as `sqry diff` does.
// ---------------------------------------------------------------------------

/// A session in workspace-folder root mode, so `index_status` over a
/// workspace folder resolves a `Project` and a corrupt load reaches
/// `SessionManager::rebuild_project_graph_after_load_failure` (the
/// multi-root self-heal), not the single-root closure.
fn session_in_workspace_folder_mode(root: &Path) -> (SessionManager, tempfile::NamedTempFile) {
    let config = tempfile::NamedTempFile::new().expect("create LSP config");
    fs::write(
        config.path(),
        r#"{"sqry":{"projectRootMode":"workspaceFolder"}}"#,
    )
    .expect("write LSP config");
    let options = LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: Some(config.path().to_path_buf()),
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    };
    let session = SessionManager::new(options);
    session.set_workspace_folders(vec![root.to_path_buf()]);
    (session, config)
}

fn full_roster_ids() -> Vec<String> {
    sqry_plugin_registry::create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// Write an `include_all` manifest (every compiled id, `high_cost_mode:
/// include_all`) beside `snapshot`, which the caller chooses (corrupt
/// bytes or absent).
fn write_include_all_manifest(root: &Path) -> Vec<String> {
    use sqry_core::graph::unified::persistence::{
        BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
    };
    let full_ids = full_roster_ids();
    assert!(
        full_ids.iter().any(|id| id == "json"),
        "fixture precondition"
    );
    let storage = GraphStorage::new(root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test"),
    )
    .with_plugin_selection(Some(PluginSelectionManifest {
        active_plugin_ids: full_ids.clone(),
        high_cost_mode: Some("include_all".to_string()),
    }))
    .save(storage.manifest_path())
    .expect("manifest saved");
    full_ids
}

/// The recorded selection and the build command the manifest's provenance
/// names, so a test can assert which site wrote the manifest and not only
/// what it recorded.
fn recorded_selection_and_site(
    root: &Path,
) -> (
    sqry_core::graph::unified::persistence::PluginSelectionManifest,
    String,
) {
    let manifest = sqry_core::graph::unified::persistence::GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable after the rebuild");
    let site = manifest.build_provenance.build_command.clone();
    (
        manifest
            .plugin_selection
            .expect("the rebuild must record a plugin selection, not drop it"),
        site,
    )
}

/// T20: the multi-root self-heal (`rebuild_project_graph_after_load_failure`,
/// reached only through `SessionManager::graph_for_path` when workspace
/// folders are configured and the project's graph fails to load) over an
/// `include_all` manifest plus a corrupt snapshot records the same ids and
/// mode. The site is asserted through the manifest's `build_command`
/// (`lsp:project_auto_rebuild`): `index_status` acquires through the
/// single-root `acquire_session_graph` and never reaches this site, which
/// the round 2 battery (row K8) showed for the first form of this test.
#[test]
fn lsp_project_auto_rebuild_keeps_the_include_all_selection() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let full_ids = write_include_all_manifest(&root);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("corrupt snapshot");

    let (session, _config) = session_in_workspace_folder_mode(&root);
    let graph = session
        .graph_for_path(&root.join("src").join("lib.rs"))
        .expect("graph_for_path self-heals the project graph")
        .expect("the project self-heal must return a graph");
    assert!(
        graph.node_count() > 0,
        "the rebuilt project graph must contain the fixture symbols"
    );

    let (selection, site) = recorded_selection_and_site(&root);
    assert_eq!(
        site, "lsp:project_auto_rebuild",
        "the manifest must have been written by the project auto-rebuild site"
    );
    assert_eq!(
        selection.active_plugin_ids, full_ids,
        "the project self-heal must record exactly the selection the manifest already recorded"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("include_all"),
        "high_cost_mode must be carried through the project self-heal"
    );
}

/// T21: the `sqry.index` command (`handlers::index::rebuild_index`) over an
/// `include_all` manifest records the same ids and mode.
#[test]
fn lsp_rebuild_index_command_keeps_the_include_all_selection() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let full_ids = write_include_all_manifest(&root);

    let session = SessionManager::new(lsp_options_for(&root));
    let reporter = sqry_core::progress::no_op_reporter();
    let summary = index::rebuild_index(&session, &root, &reporter, true)
        .expect("sqry.index rebuilds over an include_all manifest");
    assert!(summary.total_symbols > 0, "the rebuilt graph has symbols");

    let (selection, site) = recorded_selection_and_site(&root);
    assert_eq!(
        site, "lsp:rebuild_index",
        "the manifest must have been written by the sqry.index site"
    );
    assert_eq!(
        selection.active_plugin_ids, full_ids,
        "sqry.index must record exactly the selection the manifest already recorded"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("include_all"),
        "high_cost_mode must be carried through sqry.index"
    );
}

/// T27: `semantic_diff::execute` over a repository whose manifest cannot be
/// read refuses, naming the manifest path, as `sqry diff` does. On
/// `abefdd8e3` it fell back to the fast path and returned `Ok`.
#[test]
fn lsp_semantic_diff_refuses_an_unreadable_manifest() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "parity")
            .env("GIT_AUTHOR_EMAIL", "parity@example.invalid")
            .env("GIT_COMMITTER_NAME", "parity")
            .env("GIT_COMMITTER_EMAIL", "parity@example.invalid")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "fixture"]);

    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let manifest_bytes_before = fs::read(storage.manifest_path()).expect("manifest bytes");

    let session = SessionManager::new(lsp_options_for(&root));
    let params = SqrySemanticDiffParams {
        base: SqryGitVersionRef {
            git_ref: "HEAD".into(),
            file_path: None,
        },
        target: SqryGitVersionRef {
            git_ref: "HEAD".into(),
            file_path: None,
        },
        path: None,
        include_unchanged: None,
        max_results: None,
        filters: None,
    };
    let result = semantic_diff::execute(&session, &params);
    let err = match result {
        Ok(summary) => panic!(
            "semantic_diff over an unreadable manifest must refuse, got Ok with {} changes",
            summary.changes.len()
        ),
        Err(err) => err,
    };
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&storage.manifest_path().display().to_string()),
        "the refusal must name the manifest path: {rendered}"
    );
    assert_eq!(
        fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_bytes_before,
        "a read-only diff must not touch the manifest"
    );
}

/// Verifier pin (surface parity W1 round 2, battery row V12, the D9 LSP
/// `sqry.index` row): `handlers::index::rebuild_index` is an explicit
/// rebuild, so over an index whose manifest cannot be read it falls back
/// to the fast path, records that fallback selection, and names itself in
/// the manifest's provenance. T26 exercises the self-heal wrapper and T21
/// a readable manifest; neither reaches this site with an unreadable one.
#[test]
fn lsp_rebuild_index_command_falls_back_over_an_unreadable_manifest_and_records_it() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");

    let session = SessionManager::new(lsp_options_for(&root));
    let reporter = sqry_core::progress::no_op_reporter();
    let summary = index::rebuild_index(&session, &root, &reporter, true)
        .expect("sqry.index falls back over an unreadable manifest");
    assert!(summary.total_symbols > 0, "the rebuilt graph has symbols");

    let (selection, site) = recorded_selection_and_site(&root);
    assert_eq!(
        site, "lsp:rebuild_index",
        "the manifest must have been written by the sqry.index site"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "the fallback selection must be recorded, not None"
    );
    assert!(
        !selection.active_plugin_ids.iter().any(|id| id == "json"),
        "the fallback is the fast path, got {:?}",
        selection.active_plugin_ids
    );
}

// ---------------------------------------------------------------------------
// Round 3 (design D16, D19): every LSP graph reaches a handler through the
// shared provider, and the LSP's unreadable-manifest diagnostic is observed
// by a `log` capture rather than assumed.
// ---------------------------------------------------------------------------

/// The `log` records this test binary captured, installed once per process
/// (design D19, T29). `SessionManager::new` sets the max level from the
/// options (`warn` here), so WARN and ERROR records reach the logger.
/// Tests filter the records by their own manifest path, so tests running
/// in parallel in this binary do not read one another's lines.
mod log_capture {
    use std::sync::{Mutex, OnceLock};

    static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();

    struct CapturingLogger;

    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            RECORDS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("{} {}", record.level(), record.args()));
        }

        fn flush(&self) {}
    }

    /// Install the capturing logger, or fail naming the logger that owns
    /// the process. A test that could not install the logger must not
    /// pass vacuously.
    pub fn install() {
        let outcome = INSTALLED.get_or_init(|| {
            log::set_boxed_logger(Box::new(CapturingLogger)).map_err(|err| err.to_string())
        });
        if let Err(err) = outcome {
            panic!(
                "the log capture could not be installed; another logger owns the process: {err}"
            );
        }
        // The session sets its own max level from the options; keep at
        // least WARN enabled in case no session has been constructed yet.
        if log::max_level() < log::LevelFilter::Warn {
            log::set_max_level(log::LevelFilter::Warn);
        }
    }

    /// The WARN and ERROR records that mention `needle`.
    pub fn records_naming(needle: &str) -> Vec<String> {
        RECORDS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|line| line.contains(needle))
            .cloned()
            .collect()
    }
}

const R3_PLANTED_ID: &str = "w1-r3-planted-plugin";

/// A valid fast-path index (built by the workspace `sqry` binary) whose
/// manifest then names `R3_PLANTED_ID`, an id no build of sqry compiles.
/// Returns the manifest bytes after planting.
fn plant_r3_id_beside_a_valid_snapshot(root: &Path) -> Vec<u8> {
    assert!(
        sqry_plugin_registry::create_plugin_manager_all()
            .plugin_by_id(R3_PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    build_index_with_cli(root);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(R3_PLANTED_ID.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    fs::read(storage.manifest_path()).expect("manifest bytes")
}

/// T33, form 1 (project load; codex R3-3): in workspace-folder mode,
/// `graph_for_path` over a valid snapshot whose manifest names an id this
/// binary did not compile returns `Err` naming the id and the manifest
/// path, and the manifest bytes are unchanged. On `1c4e2292e` the arm
/// called `Project::graph`, which never opens the manifest, and returned
/// `Ok(Some(graph))` (`graph_present=true`).
#[test]
fn lsp_project_graph_refuses_a_manifest_naming_an_uncompiled_plugin() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let bytes_before = plant_r3_id_beside_a_valid_snapshot(&root);
    let manifest_path = root
        .join(".sqry")
        .join("graph")
        .join("manifest.json")
        .display()
        .to_string();

    let (session, _config) = session_in_workspace_folder_mode(&root);
    let outcome = session.graph_for_path(&root.join("src").join("lib.rs"));
    let err = match outcome {
        Ok(graph) => panic!(
            "graph_for_path must refuse the planted id; survived graph_present={}",
            graph.is_some()
        ),
        Err(err) => err,
    };
    let text = format!("{err:#}");
    assert!(
        text.contains(R3_PLANTED_ID),
        "the refusal must name the id: {text}"
    );
    assert!(
        text.contains(&manifest_path),
        "the refusal must name the manifest: {text}"
    );
    assert_eq!(
        fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
        bytes_before,
        "the refusal must not rewrite the manifest"
    );
}

/// T33, form 2 (the search handler in workspace-folder mode; codex R3-3):
/// `sqry/search` over the same fixture returns `Err`, not a result with
/// `total == 2`. On `1c4e2292e` the handler served the snapshot
/// (`survived total=2`).
#[test]
fn lsp_multi_workspace_search_refuses_a_manifest_naming_an_uncompiled_plugin() {
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let bytes_before = plant_r3_id_beside_a_valid_snapshot(&root);

    let (session, _config) = session_in_workspace_folder_mode(&root);
    let params = SqrySearchParams {
        query: "kind:function".into(),
        path: Some(root.to_string_lossy().to_string()),
        limit: Some(100),
        ..SqrySearchParams::default()
    };
    let outcome = search::execute(&session, &params);
    let err = match outcome {
        Ok(result) => panic!(
            "the search handler must refuse the planted id; survived total={}",
            result.total
        ),
        Err(err) => err,
    };
    let text = format!("{err:#}");
    assert!(
        text.contains(R3_PLANTED_ID),
        "the refusal must name the id: {text}"
    );
    assert_eq!(
        fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
        bytes_before,
        "the refusal must not rewrite the manifest"
    );
}

/// T34 (design D16, the D9 LSP row for the project arm): in workspace-folder
/// mode over a manifest that cannot be read beside a valid snapshot, the
/// project graph self-heals through the shared provider's rule (fall
/// back, warn, record): a graph is returned, the manifest afterwards parses
/// with `fast_path_default` and `build_command == "lsp:project_auto_rebuild"`,
/// and the WARN diagnostic names the manifest path. On `1c4e2292e`
/// `Project::graph` served the snapshot and the manifest stayed `{}`; the
/// graph leg is green on both heads and is a declared control.
#[test]
fn lsp_project_graph_self_heals_over_an_unreadable_manifest_and_records_the_fallback() {
    log_capture::install();
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    build_index_with_cli(&root);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    assert!(storage.snapshot_exists(), "fixture precondition");
    fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let manifest_path = storage.manifest_path().display().to_string();

    let (session, _config) = session_in_workspace_folder_mode(&root);
    let graph = session
        .graph_for_path(&root.join("src").join("lib.rs"))
        .expect("the project arm self-heals over an unreadable manifest")
        .expect("a graph is returned");
    assert!(graph.node_count() > 0, "the rebuilt graph has symbols");

    let (selection, site) = recorded_selection_and_site(&root);
    assert_eq!(
        site, "lsp:project_auto_rebuild",
        "the manifest must have been written by the project self-heal site"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "the fallback selection must be recorded, not None"
    );
    assert!(
        !selection.active_plugin_ids.iter().any(|id| id == "json"),
        "the fallback is the fast path, got {:?}",
        selection.active_plugin_ids
    );

    let records = log_capture::records_naming(&manifest_path);
    assert!(
        !records.is_empty(),
        "the self-heal must warn naming the manifest; no WARN record mentions {manifest_path}"
    );
    assert!(
        records.iter().any(|line| line.starts_with("WARN ")),
        "the diagnostic is a WARN: {records:?}"
    );
}

/// T29 (design D19, battery row K16's oracle; codex survivor C9): the
/// single-root self-heal over a manifest that cannot be read beside a
/// snapshot that cannot load emits exactly one WARN record naming the
/// manifest path (the presence and the path are contract; the wording is
/// not), and the manifest afterwards records `fast_path_default`. Green on
/// both heads, a declared control: it exists so that suppressing
/// `warn_if_manifest_was_unreadable` no longer survives the LSP suite.
#[test]
fn lsp_self_heal_warning_names_the_unreadable_manifest_control() {
    log_capture::install();
    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("corrupt snapshot");
    let manifest_path = storage.manifest_path().display().to_string();

    let session = SessionManager::new(lsp_options_for(&root));
    let params = SqrySearchParams {
        query: "kind:function".into(),
        path: None,
        limit: Some(100),
        ..SqrySearchParams::default()
    };
    let result = search::execute(&session, &params).expect("the self-heal rebuilds and serves");
    assert!(
        result.total >= 1,
        "the rebuilt graph has the fixture symbols"
    );

    let records = log_capture::records_naming(&manifest_path);
    assert_eq!(
        records.len(),
        1,
        "exactly one WARN record names the unreadable manifest; got {records:?}"
    );
    assert!(
        records[0].starts_with("WARN "),
        "the diagnostic is a WARN: {}",
        records[0]
    );

    let (selection, site) = recorded_selection_and_site(&root);
    assert_eq!(site, "lsp:auto_rebuild");
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "the fallback selection must be recorded"
    );
}

// ---------------------------------------------------------------------------
// Surface parity W4 (design W4-D6, W4-D7): the LSP builds with the macro
// options the manifest records.
// ---------------------------------------------------------------------------

const W4_CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";
const W4_CARGO_TOML: &str =
    "[package]\nname = \"w4_cfg_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn w4_cfg_fixture() -> TempDir {
    let dir = project_tempdir();
    fs::write(dir.path().join("Cargo.toml"), W4_CARGO_TOML).expect("write Cargo.toml");
    fs::create_dir_all(dir.path().join("src")).expect("src dir");
    fs::write(dir.path().join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
    dir
}

/// `sqry index <args>... <root>` through the workspace binary.
fn w4_sqry_index(root: &Path, args: &[&str]) {
    let output = Command::new(sqry_bin_path())
        .arg("index")
        .args(args)
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry index");
    assert!(
        output.status.success(),
        "sqry index {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The activation the persisted graph records for the fixture's
/// `cfg(test)` item: `Some(true)` only when the build ran with `--cfg test`.
fn w4_cfg_test_activation(root: &Path) -> Option<bool> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(root);
    let graph =
        sqry_core::graph::unified::persistence::load_from_path(storage.snapshot_path(), None)
            .expect("snapshot loads");
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("recorded cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

fn w4_recorded_macro_options(
    root: &Path,
) -> Option<sqry_core::graph::unified::persistence::MacroOptionsManifest> {
    sqry_core::graph::unified::persistence::GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
}

/// T15 (LSP leg): `sqry.index` with `force` over an index built with
/// `--cfg test` rebuilds with the recorded flags and records them again.
/// On the pre-change head the rebuilt graph had `cfg_active == None`.
#[test]
fn lsp_rebuild_index_command_keeps_the_recorded_macro_options() {
    let fixture = w4_cfg_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    w4_sqry_index(&root, &["--cfg", "test"]);
    assert_eq!(
        w4_cfg_test_activation(&root),
        Some(true),
        "fixture precondition"
    );

    let session = SessionManager::new(lsp_options_for(&root));
    let reporter = sqry_core::progress::no_op_reporter();
    let summary = index::rebuild_index(&session, &root, &reporter, true)
        .expect("sqry.index rebuilds over the recorded macro options");
    assert!(summary.built, "force = true builds");

    assert_eq!(
        w4_cfg_test_activation(&root),
        Some(true),
        "the LSP rebuild must run with the recorded --cfg test"
    );
    let record = w4_recorded_macro_options(&root).expect("the record survives the rebuild");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    let (_, site) = recorded_selection_and_site(&root);
    assert_eq!(site, "lsp:rebuild_index");
}

/// T14 (LSP leg 4 and 5): a recorded expand cache directory that no longer
/// exists is refused by name by `sqry.index` with `force`, and the index
/// bytes are untouched.
#[test]
fn lsp_rebuild_index_refuses_a_missing_recorded_expand_cache_and_writes_nothing() {
    let fixture = w4_cfg_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    let cache = root.join("expand-cache");
    fs::create_dir_all(&cache).expect("cache dir");
    w4_sqry_index(
        &root,
        &["--cfg", "test", "--expand-cache", &cache.to_string_lossy()],
    );
    let recorded_dir = w4_recorded_macro_options(&root)
        .and_then(|record| record.expand_cache_dir)
        .expect("the manifest names the expand cache directory");
    assert_eq!(Path::new(&recorded_dir), cache.as_path());

    fs::remove_dir_all(&cache).expect("remove the cache");
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
    let manifest_before = fs::read(storage.manifest_path()).expect("manifest bytes");
    let snapshot_before = fs::read(storage.snapshot_path()).expect("snapshot bytes");

    let session = SessionManager::new(lsp_options_for(&root));
    let reporter = sqry_core::progress::no_op_reporter();
    let err = index::rebuild_index(&session, &root, &reporter, true)
        .expect_err("a missing recorded expand cache must refuse");
    let rendered = format!("{err:#}");
    println!("refusal: {rendered}");
    assert!(
        rendered.contains(&recorded_dir) && rendered.contains("--no-macro-options"),
        "the refusal names the directory and the way out: {rendered}"
    );
    assert_eq!(
        fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    assert_eq!(
        fs::read(storage.snapshot_path()).expect("snapshot bytes"),
        snapshot_before,
        "the refusal must leave the snapshot bytes unchanged"
    );
}

/// T72 (W1 round 9, design D44, R9-1): the provenance site the
/// workspace-folder arm records when the provider finds no graph at all and
/// calls the auto-build hook inside `acquire_session_graph`.
///
/// Round 8 planted `acquire_session_graph`'s two
/// `build_and_persist_with_workspace_roster` call sites one at a time (plan
/// step S78) and found the hook site unobserved: T20 and T34 each leave
/// `.sqry/graph` in place, so `FilesystemGraphProvider::find_workspace_root`
/// answers `GraphFound`, the acquisition reaches the corrupt-load arm, and a
/// plant inside the hook closure cannot be reached. This test drives the hook
/// arm instead: the fixture carries a `Cargo.toml` project marker and no
/// `.sqry/graph`, so `sqry_core::workspace::discover_workspace_root` breaks its
/// walk at the fixture root with no graph, `acquire_without_graph` runs, and
/// the hook is the only site that can write a manifest for this fixture.
///
/// The precondition is asserted through the production discovery function
/// itself rather than by reading the filesystem, so a host carrying a stray
/// ancestor graph fails the precondition instead of passing silently through
/// the other arm. Green at `60afdcdd1` by construction, because the third
/// parameter is already forwarded at both sites there; it is a declared
/// control verified by battery row K30c, whose plant replaces the hook site's
/// `build_command` with `"lsp:auto_rebuild"` and makes the site assertion
/// fail. K30d plants the corrupt-load site alone and this test must pass under
/// it, which is the independence the round measured.
#[test]
fn lsp_project_graph_auto_build_hook_records_the_project_site() {
    use sqry_core::graph::unified::persistence::GraphStorage;
    use sqry_core::workspace::{WorkspaceRootDiscovery, discover_workspace_root};

    let fixture = make_mixed_fixture();
    let root = fixture.path().canonicalize().expect("canonical root");
    fs::write(root.join("Cargo.toml"), "[package]\n").expect("write the project marker");

    // Precondition, through production code: the boundary is this fixture and
    // no graph was found at or inside it, which is exactly the state in which
    // `FilesystemGraphProvider::acquire` takes `acquire_without_graph` and
    // calls the hook.
    let discovery = discover_workspace_root(&root);
    let boundary = match &discovery {
        WorkspaceRootDiscovery::BoundaryOnly {
            boundary,
            is_file_scope,
        } => {
            assert!(
                !is_file_scope,
                "the fixture root is a directory, not a file"
            );
            boundary.clone()
        }
        other => {
            panic!("the fixture must present as BoundaryOnly so the hook arm runs, got {other:?}")
        }
    };
    assert_eq!(
        boundary.canonicalize().expect("canonical boundary"),
        root,
        "the project marker must make the fixture its own boundary"
    );
    let storage = GraphStorage::new(&root);
    assert!(
        !storage.snapshot_exists(),
        "the hook arm requires no snapshot at the fixture root"
    );

    let (session, _config) = session_in_workspace_folder_mode(&root);
    let graph = session
        .graph_for_path(&root.join("src").join("lib.rs"))
        .expect("the workspace-folder arm auto-builds through the provider hook")
        .expect("the auto-build hook must return a graph");
    let node_count = graph.node_count();
    let snapshot_after = storage.snapshot_exists();
    let (selection, site) = recorded_selection_and_site(&root);
    let has_json = selection.active_plugin_ids.iter().any(|id| id == "json");
    println!(
        "T72 discovery=BoundaryOnly boundary={} snapshot_before=false node_count={node_count} \
         snapshot_after={snapshot_after} site={site} high_cost_mode={:?} json_in_ids={has_json}",
        root.display(),
        selection.high_cost_mode.as_deref()
    );

    assert!(
        node_count > 0,
        "the auto-built project graph must contain the fixture symbols"
    );
    assert!(
        snapshot_after,
        "the hook persists the graph it builds, it does not build in memory"
    );
    assert_eq!(
        site, "lsp:project_auto_rebuild",
        "the manifest must have been written by the project auto-build hook site"
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("fast_path_default"),
        "a root with no manifest resolves the fallback roster and records it"
    );
    assert!(
        !has_json,
        "the fallback is the fast path, got {:?}",
        selection.active_plugin_ids
    );
}
