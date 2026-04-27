//! `STEP_7` MAJOR 2 regression coverage — `mcp__sqry__workspace_status`
//! must consume the per-request thread-local [`LogicalWorkspace`] override
//! and surface the real multi-root structure (source roots, exclusions,
//! identity), not synthesise a single-root view from `workspace_root`.
//!
//! This test exercises [`sqry_mcp::tools::execute_workspace_status`] under
//! a `with_workspace_override` scope that binds an
//! [`anonymous_multi_root`] [`LogicalWorkspace`] backed by two real source
//! root directories on disk plus an injected exclusion. We then assert
//! that the response shape matches the LSP `sqry/workspaceStatus` envelope
//! field-for-field via [`sqry_lsp::session::build_workspace_status_info`]
//! invoked on the same workspace — the parity invariant the DAG calls out
//! ("mirror LSP `sqry/workspaceStatus`").
//!
//! This is the test the codex iter1 BLOCK explicitly requested: pre-fix,
//! the tool always returned one source root; post-fix, it surfaces every
//! root in the bound workspace.

use std::path::Path;
use std::sync::Arc;
use std::sync::Once;

use sqry_core::workspace::LogicalWorkspace;
use sqry_lsp::session::build_workspace_status_info;
use sqry_mcp::test_setup::init_engine_cache;
use sqry_mcp::tool_args::WorkspaceStatusArgs;
use sqry_mcp::tool_handlers::execute_workspace_status;
use sqry_mcp::workspace_session_test_api::with_workspace_override;
use tempfile::TempDir;

static INIT: Once = Once::new();

fn ensure_engine_cache() {
    INIT.call_once(|| {
        init_engine_cache(std::num::NonZeroUsize::new(8).unwrap());
    });
}

fn populate_indexed_root(root: &Path) {
    std::fs::create_dir_all(root.join(".sqry/graph")).expect("create .sqry/graph");
}

fn inject_exclusion(workspace: &LogicalWorkspace, excluded: &Path) -> LogicalWorkspace {
    // Round-trip through serde to set the exclusions list — same trick
    // the redaction-crate integration test uses (and for the same
    // reason: the public constructors do not currently expose a "set
    // exclusions" entry-point that would round-trip a single-root or
    // anonymous-multi-root workspace).
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

#[test]
fn workspace_status_returns_multi_root_structure_from_override() {
    ensure_engine_cache();
    let tmp = TempDir::new().expect("tempdir");
    let root_a = tmp.path().join("repo_a");
    let root_b = tmp.path().join("repo_b");
    let excluded = tmp.path().join("repo_a/secrets");
    populate_indexed_root(&root_a);
    populate_indexed_root(&root_b);
    std::fs::create_dir_all(&excluded).expect("create excluded subdir");

    let workspace = LogicalWorkspace::anonymous_multi_root(vec![root_a.clone(), root_b.clone()])
        .expect("multi-root workspace");
    let workspace = inject_exclusion(&workspace, &excluded.canonicalize().unwrap());
    let workspace = Arc::new(workspace);

    // The MCP execute_workspace_status entry path uses the engine's
    // resolved workspace_root for engine_for_workspace; pass `root_a`
    // explicitly so the thread-local-bound LogicalWorkspace and the
    // engine-resolved root agree on a real on-disk path.
    let args = WorkspaceStatusArgs {
        workspace_id: Some("client-hint".to_string()),
        path: root_a.to_string_lossy().into_owned(),
    };

    let response = with_workspace_override(Some(&root_a), Some(workspace.clone()), || {
        execute_workspace_status(&args).expect("execute_workspace_status")
    });

    let data = &response.data;

    // (1) Source root projection mirrors the bound workspace.
    assert_eq!(
        data.source_roots.len(),
        2,
        "MCP must surface every source root in a multi-root LogicalWorkspace, got {data:?}"
    );

    // (2) Aggregate WorkspaceIndexStatus has one entry per source root.
    assert_eq!(
        data.aggregate.source_root_statuses.len(),
        2,
        "aggregate WorkspaceIndexStatus must report one entry per source root"
    );

    // (3) Exclusions surface from the bound workspace, not the synthetic
    //     single_root fallback (which always emits an empty list).
    assert_eq!(
        data.exclusions.len(),
        1,
        "exclusions must come from the bound LogicalWorkspace, not a synthesised single_root"
    );

    // (4) Identity surfaces match the bound workspace exactly.
    assert_eq!(
        data.workspace_id_full,
        workspace.workspace_id().as_full_hex()
    );
    assert_eq!(
        data.workspace_id_short,
        workspace.workspace_id().as_short_hex()
    );
    assert_eq!(data.requested_workspace_id.as_deref(), Some("client-hint"));

    // (5) Wire-shape parity with the LSP `sqry/workspaceStatus` handler
    //     for the same workspace. We compare every field that both
    //     surfaces expose so the regression class ("MCP and LSP report
    //     different shapes for the same workspace") cannot return.
    let lsp_info = build_workspace_status_info(workspace.as_ref());
    assert_eq!(data.workspace_id_full, lsp_info.workspace_id_full);
    assert_eq!(data.workspace_id_short, lsp_info.workspace_id_short);
    assert_eq!(data.project_root_mode, lsp_info.project_root_mode);
    assert_eq!(data.source_roots, lsp_info.source_roots);
    assert_eq!(data.exclusions, lsp_info.exclusions);
    assert_eq!(
        data.aggregate.source_root_statuses.len(),
        lsp_info.aggregate.source_root_statuses.len()
    );
    for (mcp_entry, lsp_entry) in data
        .aggregate
        .source_root_statuses
        .iter()
        .zip(lsp_info.aggregate.source_root_statuses.iter())
    {
        assert_eq!(
            mcp_entry.path, lsp_entry.path,
            "per-source-root path mismatch between MCP and LSP wire shapes"
        );
    }
}

#[test]
fn workspace_status_falls_back_to_single_root_without_override() {
    // Symmetry guard: when the per-request override is unbound (the
    // legacy entry path), the tool MUST still return a sensible
    // single-root view — the wire shape is uniform regardless of
    // whether a LogicalWorkspace was resolved upstream.
    ensure_engine_cache();
    let tmp = TempDir::new().expect("tempdir");
    populate_indexed_root(tmp.path());

    let args = WorkspaceStatusArgs {
        workspace_id: None,
        path: tmp.path().to_string_lossy().into_owned(),
    };
    // Set workspace_root override but leave logical = None — exercises
    // the inline single_root fallback inside execute_workspace_status.
    let response = with_workspace_override(Some(tmp.path()), None, || {
        execute_workspace_status(&args).expect("execute_workspace_status fallback")
    });

    assert_eq!(response.data.source_roots.len(), 1);
    assert_eq!(response.data.exclusions.len(), 0);
    assert_eq!(response.data.aggregate.source_root_statuses.len(), 1);
}
