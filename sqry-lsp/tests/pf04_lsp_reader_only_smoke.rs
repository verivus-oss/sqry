//! PF04 — surface proof that the LSP `textDocument/codeLens` handler is a
//! derived-cache READER only.
//!
//! Wires a real `TestServer` (which builds a V10 snapshot via
//! `common::ensure_index` + `build_code_graph`) and drives a code-lens
//! request through the full LSP handler pipeline. Asserts that the
//! handler never creates a `derived.sqry` companion file under
//! `<workspace>/.sqry/graph/`.
//!
//! The handler internally calls
//! [`sqry_db::queries::dispatch::make_query_db_cold`], which is allowed
//! to *delete* a stale/corrupt derived-cache file but must never *write*
//! one. CLI, LSP, and MCP are reader-only by contract; the writer lives
//! exclusively in the daemon's `QueryDbHook` (PF03B).
//!
//! Spec: docs/reviews/generational-design-analysis/2026-05-07/codex_in_code_verification_2026-05-07T030441Z.md
//! Plan: docs/development/generational-analysis-platform/priority-followups/03_IMPLEMENTATION_PLAN.md (unit PF04)

use anyhow::Result;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::Url;

mod common;

fn rust_callgraph_workspace() -> Result<TempDir> {
    let tmp = tempfile::tempdir()?;
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir)?;
    fs::write(
        src_dir.join("lib.rs"),
        r"
pub fn helper() -> u32 {
    42
}

pub fn caller() -> u32 {
    helper() + helper()
}
",
    )?;
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lsp-pf04-reader-only\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    Ok(tmp)
}

#[tokio::test(flavor = "current_thread")]
async fn pf04_lsp_codelens_handler_does_not_create_derived_sqry() -> Result<()> {
    let workspace = rust_callgraph_workspace()?;
    let derived_path = workspace
        .path()
        .join(".sqry")
        .join("graph")
        .join("derived.sqry");

    // Sanity: the index-build step in `TestServer::new` does NOT create a
    // derived.sqry — only a snapshot.sqry. If this assertion ever fires it
    // means the LSP build pipeline silently regressed into a writer.
    let mut server = common::TestServer::new(workspace.path());
    let snapshot_path = workspace
        .path()
        .join(".sqry")
        .join("graph")
        .join("snapshot.sqry");
    assert!(
        snapshot_path.exists(),
        "preconditon: index build must produce snapshot.sqry; got missing path {}",
        snapshot_path.display()
    );
    assert!(
        !derived_path.exists(),
        "precondition: index build must NOT produce derived.sqry; \
         derived-cache writer leaked into LSP startup path"
    );

    // Drive the initialize handshake.
    let initialize = Request::build("initialize".to_string())
        .params(json!({
            "processId": null,
            "rootUri": format!("file://{}", workspace.path().display()),
            "capabilities": {}
        }))
        .id(0i64)
        .finish();
    let _ = server
        .send_request(initialize)
        .await?
        .expect("initialize response");

    // Drive a code-lens request — this exercises `make_query_db_cold`
    // inside the handler.
    let lib_path = workspace.path().join("src").join("lib.rs");
    let lib_path = std::fs::canonicalize(&lib_path)?;
    let lib_uri = Url::from_file_path(&lib_path).expect("file URI");

    let code_lens_request = Request::build("textDocument/codeLens".to_string())
        .params(json!({ "textDocument": { "uri": lib_uri } }))
        .id(1i64)
        .finish();
    let response = server
        .send_request(code_lens_request)
        .await?
        .expect("code_lens response");
    let (_, body) = response.into_parts();
    // The response body decode is not the contract being tested here — we
    // only need the handler to have actually run. Decoding it confirms
    // that.
    let _value: Value = body?;

    assert!(
        !derived_path.exists(),
        "PF04 contract violation: `textDocument/codeLens` handler created \
         derived.sqry at {}. CLI/LSP/MCP must be reader-only.",
        derived_path.display()
    );

    Ok(())
}
