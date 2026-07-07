//! Issue verivus-oss/sqry#469: logical-workspace path-exclusion
//! enforcement across the MCP path-resolution surfaces.
//!
//! Each test maps onto a test case in
//! `docs/development/mcp-logical-workspace-exclusion-enforcement/05_TEST_PLAN`:
//!
//! - `tc1_*`: excluded path rejected (precedence over containment).
//! - `tc2_*`: allowed in-workspace path accepted.
//! - `tc3_*`: traversal / absolute-out-of-workspace still rejected under a
//!   bound workspace (not masked by, and not confused with, an exclusion).
//! - `tc4_*`: empty-exclusions / no-binding parity with the unaware primitive.
//! - `tc5_*`: symlink escape into an excluded directory (unix only).
//! - `tc6_*`: tool-level integration: a production tool executor rejects an
//!   excluded `file_path` with `invalid_params` (`-32602`) before seeding.
//! - `tc7_*`: trailing-slash exclusion entry still rejects the subtree.
//! - `tc9_*`: daemon-hosted enforcement: the daemon's own reconstruction seam
//!   (`resolve_logical_workspace_for_root` + `with_workspace_override`) yields a
//!   policy that enforces identically to standalone mode.
//!
//! TC8 (Surface 2 `workspace_scope`) lives as a unit test inside
//! `sqry-mcp/src/execution/workspace_scope.rs` because the classifier
//! entrypoints (`classify_within`, `subtree_within`) are `pub(crate)` and the
//! deterministic explicit-`logical` seam is only reachable from inside the
//! crate.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Once};
use std::time::Duration;

use sqry_core::workspace::{
    LogicalWorkspace, WorkspaceMetadata, WorkspaceRegistry, WorkspaceRepoId, WorkspaceRepository,
};
use sqry_mcp::daemon_adapter::resolve_logical_workspace_for_root;
use sqry_mcp::engine::{
    WorkspacePathError, canonicalize_in_workspace, canonicalize_in_workspace_enforced,
    canonicalize_in_workspace_with_logical, engine_for_workspace,
};
use sqry_mcp::error::RpcError;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    CallHierarchyArgs, CallHierarchyDirection, GetDocumentSymbolsArgs, PaginationArgs,
    ShowDependenciesArgs,
};
use sqry_mcp::tool_handlers::{
    execute_call_hierarchy, execute_get_dependencies, execute_get_document_symbols,
};
use sqry_mcp::workspace_session_test_api::with_workspace_override;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures / helpers
// ---------------------------------------------------------------------------

fn init_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).unwrap());
        init_engine_cache(NonZeroUsize::new(8).unwrap());
        init_trace_path_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
        init_subgraph_cache(NonZeroUsize::new(64).unwrap(), Duration::from_secs(60));
    });
}

/// A canonical temp source root with `src/main.rs` and
/// `secrets/api_keys.toml` on disk. The root is canonicalized so the paths
/// match the exclusions the redactor and the engine canonicalize to.
struct Fixture {
    _tmp: TempDir,
    root: std::path::PathBuf,
    secrets: std::path::PathBuf,
    src_main: std::path::PathBuf,
}

fn make_fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let secrets = root.join("secrets");
    let src = root.join("src");
    std::fs::create_dir_all(&secrets).expect("create secrets");
    std::fs::create_dir_all(&src).expect("create src");
    std::fs::write(secrets.join("api_keys.toml"), "[k]\nvalue = 1\n").expect("write secret");
    std::fs::write(src.join("main.rs"), "fn main() {}\n").expect("write source");
    Fixture {
        secrets: secrets.canonicalize().expect("canonical secrets"),
        src_main: src.join("main.rs").canonicalize().expect("canonical main"),
        root,
        _tmp: tmp,
    }
}

/// Inject an exclusion path into a `LogicalWorkspace` via a public-API-only
/// serde round trip (the constructors do not expose a "single root +
/// exclusions" seam). Mirrors the redaction-crate and `workspace_status`
/// integration-test helper.
fn inject_exclusion(workspace: &LogicalWorkspace, excluded: &Path) -> LogicalWorkspace {
    let mut value: serde_json::Value = serde_json::to_value(workspace).expect("workspace -> json");
    let exclusions = value
        .get_mut("exclusions")
        .and_then(serde_json::Value::as_array_mut)
        .expect("LogicalWorkspace must serialize an `exclusions` array");
    exclusions.push(serde_json::Value::String(
        excluded.to_string_lossy().into_owned(),
    ));
    serde_json::from_value(value).expect("json -> workspace")
}

/// Single-root workspace over `root` with `secrets` excluded.
fn workspace_with_secrets_excluded(root: &Path, secrets: &Path) -> LogicalWorkspace {
    let base = LogicalWorkspace::single_root(root.to_path_buf()).expect("single_root");
    inject_exclusion(&base, secrets)
}

/// Extract the `Err` from a tool execution result. `ToolExecution<T>` does not
/// implement `Debug`, so `Result::expect_err` cannot be used directly; this
/// panics on `Ok` and returns the `anyhow::Error` on `Err`.
fn expect_tool_err<T>(result: anyhow::Result<T>) -> anyhow::Error {
    match result {
        Ok(_) => panic!("tool call must reject the excluded path, got Ok"),
        Err(err) => err,
    }
}

/// Assert a tool execution result is `Ok`. Mirrors [`expect_tool_err`] for the
/// positive-control arms (`ToolExecution<T>` is not `Debug`, so the `Err` is
/// surfaced instead).
fn expect_tool_ok<T>(result: anyhow::Result<T>) {
    if let Err(err) = result {
        panic!("an allowed path under the same binding must still resolve, got Err: {err:?}");
    }
}

/// Assert an enforced-entry `anyhow` error is the exclusion rejection,
/// carrying `RpcError` code `-32602` (`invalid_params`).
fn assert_excluded_rpc(err: &anyhow::Error) {
    let rpc = err
        .downcast_ref::<RpcError>()
        .unwrap_or_else(|| panic!("expected RpcError, got {err:?}"));
    assert_eq!(
        rpc.code, -32602,
        "excluded path must surface invalid_params"
    );
    assert!(
        rpc.message
            .contains("excluded by the logical workspace policy"),
        "unexpected message: {}",
        rpc.message
    );
}

// ---------------------------------------------------------------------------
// TC1: excluded path rejected (precedence over containment)
// ---------------------------------------------------------------------------

#[test]
fn tc1_excluded_path_rejected_via_with_logical_and_enforced() {
    let fx = make_fixture();
    let ws = workspace_with_secrets_excluded(&fx.root, &fx.secrets);

    // Surface 1 explicit-workspace seam: typed `Excluded` error.
    let err = canonicalize_in_workspace_with_logical("secrets/api_keys.toml", &fx.root, Some(&ws))
        .expect_err("excluded path must be rejected");
    assert!(
        matches!(err, WorkspacePathError::Excluded { .. }),
        "expected Excluded, got {err:?}"
    );

    // Enforced entry (reads the thread-local): same rejection, RpcError -32602.
    let ws = Arc::new(ws);
    let enforced = with_workspace_override(Some(&fx.root), Some(ws), || {
        canonicalize_in_workspace_enforced("secrets/api_keys.toml", &fx.root)
    });
    let err = enforced.expect_err("enforced entry must reject excluded path");
    assert_excluded_rpc(&err);
}

// ---------------------------------------------------------------------------
// TC2: allowed in-workspace path accepted
// ---------------------------------------------------------------------------

#[test]
fn tc2_allowed_in_workspace_path_accepted() {
    let fx = make_fixture();
    let ws = workspace_with_secrets_excluded(&fx.root, &fx.secrets);

    let canon = canonicalize_in_workspace_with_logical("src/main.rs", &fx.root, Some(&ws))
        .expect("allowed path must resolve");
    assert_eq!(canon, fx.src_main);
    assert!(canon.starts_with(&fx.root));

    // Enforced entry accepts the same allowed path under a bound workspace.
    let ws = Arc::new(ws);
    let canon = with_workspace_override(Some(&fx.root), Some(ws), || {
        canonicalize_in_workspace_enforced("src/main.rs", &fx.root)
    })
    .expect("enforced entry must accept an allowed path");
    assert_eq!(canon, fx.src_main);
}

// ---------------------------------------------------------------------------
// TC3: traversal / absolute-out-of-workspace still rejected under a binding
// ---------------------------------------------------------------------------

#[test]
fn tc3_traversal_and_absolute_out_of_workspace_rejected() {
    let fx = make_fixture();
    let ws = Arc::new(workspace_with_secrets_excluded(&fx.root, &fx.secrets));

    with_workspace_override(Some(&fx.root), Some(ws), || {
        // A relative traversal escaping the root is rejected, not masked as an
        // exclusion, and does not panic.
        let err = canonicalize_in_workspace_enforced("../../etc/passwd", &fx.root)
            .expect_err("traversal must be rejected");
        assert!(
            err.downcast_ref::<RpcError>().is_none(),
            "traversal must not surface as the exclusion RpcError: {err:?}"
        );

        // An absolute out-of-workspace path is rejected too.
        let err = canonicalize_in_workspace_enforced("/etc/passwd", &fx.root)
            .expect_err("absolute out-of-workspace path must be rejected");
        assert!(
            err.downcast_ref::<RpcError>().is_none(),
            "out-of-workspace path must not surface as the exclusion RpcError: {err:?}"
        );
    });
}

// ---------------------------------------------------------------------------
// TC4: empty-exclusions / no-binding parity
// ---------------------------------------------------------------------------

#[test]
fn tc4_empty_exclusions_and_no_binding_parity() {
    let fx = make_fixture();
    let baseline = canonicalize_in_workspace("src/main.rs", &fx.root).expect("baseline");

    // Empty exclusions: enforced == unaware primitive.
    let empty = Arc::new(LogicalWorkspace::single_root(fx.root.clone()).expect("single_root"));
    let bound = with_workspace_override(Some(&fx.root), Some(empty), || {
        canonicalize_in_workspace_enforced("src/main.rs", &fx.root)
    })
    .expect("empty-exclusions enforced must accept");
    assert_eq!(bound, baseline);

    // No binding active: the thread-local returns None, so enforced == baseline.
    let unbound =
        canonicalize_in_workspace_enforced("src/main.rs", &fx.root).expect("unbound enforced");
    assert_eq!(unbound, baseline);
}

// ---------------------------------------------------------------------------
// TC5: symlink escape into an excluded directory (unix only)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn tc5_symlink_into_excluded_dir_rejected() {
    let fx = make_fixture();
    // src/link -> ../secrets. Lexically `src/link/api_keys.toml` is not under
    // `secrets`, so only the post-canonicalization re-check catches it.
    let link = fx.root.join("src").join("link");
    std::os::unix::fs::symlink(&fx.secrets, &link).expect("create symlink");

    let ws = workspace_with_secrets_excluded(&fx.root, &fx.secrets);
    let err = canonicalize_in_workspace_with_logical("src/link/api_keys.toml", &fx.root, Some(&ws))
        .expect_err("symlink into excluded dir must be rejected");
    assert!(
        matches!(err, WorkspacePathError::Excluded { .. }),
        "expected Excluded from the post-canonicalization re-check, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// TC6: tool-level integration: excluded file_path rejected with -32602
// ---------------------------------------------------------------------------

#[test]
fn tc6_tool_rejects_excluded_file_path_with_invalid_params() {
    init_caches();
    let fx = make_fixture();
    // Index the fixture so `engine_for_workspace` loads a real graph; the
    // exclusion enforcement must fire on `file_path` before any seeding.
    let engine = engine_for_workspace(Some(&fx.root)).expect("engine_for_workspace");
    let _ = engine.ensure_graph().expect("ensure_graph");

    let ws = Arc::new(workspace_with_secrets_excluded(&fx.root, &fx.secrets));

    let args = ShowDependenciesArgs {
        file_path: Some(
            fx.secrets
                .join("api_keys.toml")
                .to_string_lossy()
                .into_owned(),
        ),
        symbol_name: None,
        path: fx.root.to_string_lossy().into_owned(),
        max_depth: 3,
        max_results: 100,
        pagination: PaginationArgs {
            offset: 0,
            size: 100,
        },
    };

    let err = expect_tool_err(with_workspace_override(Some(&fx.root), Some(ws), || {
        execute_get_dependencies(&args)
    }));
    assert_excluded_rpc(&err);

    // No-read / no-seed proof: the exclusion gate must fire BEFORE the tool
    // consults the file. Running the identical excluded `file_path` with NO
    // policy bound proceeds past the gate into normal processing, which for a
    // non-code TOML target surfaces a *different*, non-exclusion error (or a
    // result), never the `-32602` exclusion rejection. So the `-32602` above is
    // specifically the exclusion gate, and when excluded the tool never reaches
    // the read/seed path this unbound arm reaches.
    let unbound = execute_get_dependencies(&args);
    match unbound {
        Ok(_) => {}
        Err(err) => assert!(
            err.downcast_ref::<RpcError>()
                .is_none_or(|rpc| rpc.code != -32602),
            "without a bound policy the excluded path must NOT hit the exclusion \
             gate; a -32602 here would mean the gate is not what rejects it: {err:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// TC6b: get_document_symbols rejects an excluded file_path (Surface 1)
// ---------------------------------------------------------------------------

#[test]
fn tc6b_get_document_symbols_rejects_excluded_file_path() {
    init_caches();
    let fx = make_fixture();
    let engine = engine_for_workspace(Some(&fx.root)).expect("engine_for_workspace");
    let _ = engine.ensure_graph().expect("ensure_graph");
    let ws = Arc::new(workspace_with_secrets_excluded(&fx.root, &fx.secrets));

    // Excluded file_path: rejected with -32602 before any graph lookup.
    let excluded = GetDocumentSymbolsArgs {
        file_path: fx
            .secrets
            .join("api_keys.toml")
            .to_string_lossy()
            .into_owned(),
        path: fx.root.to_string_lossy().into_owned(),
    };
    let err = expect_tool_err(with_workspace_override(
        Some(&fx.root),
        Some(ws.clone()),
        || execute_get_document_symbols(&excluded),
    ));
    assert_excluded_rpc(&err);

    // Allowed file_path under the same binding resolves (path-scoped, not
    // blanket, rejection).
    let allowed = GetDocumentSymbolsArgs {
        file_path: fx.src_main.to_string_lossy().into_owned(),
        path: fx.root.to_string_lossy().into_owned(),
    };
    expect_tool_ok(with_workspace_override(Some(&fx.root), Some(ws), || {
        execute_get_document_symbols(&allowed)
    }));
}

// ---------------------------------------------------------------------------
// TC6c: call_hierarchy rejects an excluded file_path (Surface 1)
// ---------------------------------------------------------------------------

#[test]
fn tc6c_call_hierarchy_rejects_excluded_file_path() {
    init_caches();
    let fx = make_fixture();
    let engine = engine_for_workspace(Some(&fx.root)).expect("engine_for_workspace");
    let _ = engine.ensure_graph().expect("ensure_graph");
    let ws = Arc::new(workspace_with_secrets_excluded(&fx.root, &fx.secrets));

    let args = CallHierarchyArgs {
        symbol: "main".to_string(),
        file_path: Some(
            fx.secrets
                .join("api_keys.toml")
                .to_string_lossy()
                .into_owned(),
        ),
        direction: CallHierarchyDirection::Incoming,
        path: fx.root.to_string_lossy().into_owned(),
        max_depth: 3,
        max_results: 100,
        pagination: PaginationArgs {
            offset: 0,
            size: 100,
        },
    };
    let err = expect_tool_err(with_workspace_override(Some(&fx.root), Some(ws), || {
        execute_call_hierarchy(&args)
    }));
    assert_excluded_rpc(&err);
}

// ---------------------------------------------------------------------------
// TC10: navigation `path` argument exclusion (Surface 2)
// ---------------------------------------------------------------------------

#[test]
fn tc10_navigation_path_arg_rejects_excluded_subtree() {
    init_caches();
    let fx = make_fixture();
    let engine = engine_for_workspace(Some(&fx.root)).expect("engine_for_workspace");
    let _ = engine.ensure_graph().expect("ensure_graph");
    let ws = Arc::new(workspace_with_secrets_excluded(&fx.root, &fx.secrets));

    // A `path` argument that names the excluded subtree must be rejected rather
    // than swallowed into a discovery fallback (the pre-fix `.ok().flatten()`
    // bug silently accepted it). `get_document_symbols` re-enters no Surface 1
    // check on `path`, so this pins the Surface 2 `resolve_workspace_path`
    // enforcement specifically.
    let args = GetDocumentSymbolsArgs {
        file_path: fx.src_main.to_string_lossy().into_owned(),
        path: fx.secrets.to_string_lossy().into_owned(),
    };
    let err = expect_tool_err(with_workspace_override(Some(&fx.root), Some(ws), || {
        execute_get_document_symbols(&args)
    }));
    assert_excluded_rpc(&err);
}

// ---------------------------------------------------------------------------
// TC7: trailing-slash exclusion entry still rejects the subtree
// ---------------------------------------------------------------------------

#[test]
fn tc7_trailing_slash_exclusion_rejects_subtree() {
    let fx = make_fixture();
    // Inject `secrets/` (trailing slash) rather than the bare `secrets`.
    let base = LogicalWorkspace::single_root(fx.root.clone()).expect("single_root");
    let trailing = {
        let mut s = fx.secrets.to_string_lossy().into_owned();
        s.push('/');
        std::path::PathBuf::from(s)
    };
    let ws = Arc::new(inject_exclusion(&base, &trailing));

    let err = with_workspace_override(Some(&fx.root), Some(ws), || {
        canonicalize_in_workspace_enforced("secrets/api_keys.toml", &fx.root)
    })
    .expect_err("trailing-slash exclusion must still reject the subtree");
    assert_excluded_rpc(&err);
}

// ---------------------------------------------------------------------------
// TC9: daemon-hosted enforcement via the reconstruction seam
// ---------------------------------------------------------------------------

#[test]
fn tc9_daemon_reconstruction_seam_enforces_exclusions() {
    // The sqryd daemon holds only the workspace root at tool-dispatch time.
    // It reconstructs the `LogicalWorkspace` from disk via
    // `resolve_logical_workspace_for_root` and binds it with
    // `with_workspace_override` inside the blocking tool thread (U04). This
    // test drives exactly that seam: a `.sqry-workspace` registry declaring an
    // exclusion must yield a reconstructed policy that rejects the excluded
    // path identically to standalone mode.
    let fx = make_fixture();

    let mut registry = WorkspaceRegistry {
        metadata: WorkspaceMetadata {
            version: 2,
            workspace_name: Some("issue-469-tc9".to_string()),
            default_discovery_mode: None,
            created_at: std::time::SystemTime::now(),
            updated_at: std::time::SystemTime::now(),
        },
        repositories: vec![WorkspaceRepository::new(
            WorkspaceRepoId::new("root"),
            "root".to_string(),
            fx.root.clone(),
            fx.root.join(".sqry-index"),
            None,
        )],
        member_folders: Vec::new(),
        exclusions: vec![fx.secrets.clone()],
        project_root_mode: Default::default(),
    };
    let registry_path = fx.root.join(".sqry-workspace");
    registry.save(&registry_path).expect("save .sqry-workspace");

    // Daemon reconstruction: load the policy from the root alone.
    let bound = resolve_logical_workspace_for_root(&fx.root)
        .expect("daemon reconstruction must yield a workspace");
    assert!(
        !bound.exclusions().is_empty(),
        "reconstructed workspace must carry the registry exclusions"
    );

    // Daemon binding + enforcement: excluded path rejected with -32602.
    let err = with_workspace_override(Some(&fx.root), Some(bound), || {
        canonicalize_in_workspace_enforced("secrets/api_keys.toml", &fx.root)
    })
    .expect_err("daemon-hosted enforcement must reject the excluded path");
    assert_excluded_rpc(&err);

    // Parity: a root without a `.sqry-workspace` reconstructs to a single-root
    // policy with empty exclusions, so enforcement is a no-op.
    let plain = make_fixture();
    let bound = resolve_logical_workspace_for_root(&plain.root)
        .expect("single-root reconstruction must succeed");
    assert!(
        bound.exclusions().is_empty(),
        "single-root reconstruction must have empty exclusions"
    );
    let ok = with_workspace_override(Some(&plain.root), Some(bound), || {
        canonicalize_in_workspace_enforced("src/main.rs", &plain.root)
    })
    .expect("no-exclusion daemon path must accept an allowed file");
    assert_eq!(ok, plain.src_main);
}
