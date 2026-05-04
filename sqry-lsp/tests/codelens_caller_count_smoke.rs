//! C074b — verify the `textDocument/codeLens` handler emits caller-count
//! lenses for callable nodes after C074a wired
//! `sqry_db::queries::dispatch::mcp_callers_query` into the gated
//! handler.
//!
//! Closes audit findings A100 + A157 (audit row C074b in
//! `docs/reviews/cli-help-impl-alignment-2026-05-04/audit.md`).

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
        r#"
pub fn helper() -> u32 {
    42
}

pub fn caller() -> u32 {
    helper() + helper()
}
"#,
    )?;
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lsp-codelens-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    Ok(tmp)
}

#[tokio::test(flavor = "current_thread")]
async fn code_lens_returns_caller_count_for_called_function() -> Result<()> {
    let workspace = rust_callgraph_workspace()?;
    let mut server = common::TestServer::new(workspace.path());

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
    let value: Value = body?;
    let lenses = value
        .as_array()
        .expect("code_lens result must be an array of CodeLens entries")
        .clone();

    assert!(
        !lenses.is_empty(),
        "code_lens must return at least one lens for a non-empty callable surface; got: {value}"
    );

    // Find the lens for `helper`. C074a keys lookups on the qualified
    // (or fallback simple) name so the `data.name` field carries
    // either `helper` or `<crate>::helper` depending on how the graph
    // builder assigned the qualified name.
    let helper_lens = lenses
        .iter()
        .find(|lens| {
            lens.get("data")
                .and_then(|d| d.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|n| n == "helper" || n.ends_with("::helper"))
        })
        .unwrap_or_else(|| panic!("no caller-count lens for `helper`; lenses={lenses:#?}"));

    let command = helper_lens
        .get("command")
        .expect("helper lens must carry a Command");
    assert_eq!(
        command
            .get("command")
            .and_then(Value::as_str)
            .expect("Command.command field"),
        "sqry.showCallers",
        "helper lens must dispatch through the wired sqry.showCallers command"
    );

    let count = helper_lens
        .get("data")
        .and_then(|d| d.get("count"))
        .and_then(Value::as_u64)
        .expect("helper lens must carry a numeric data.count field");
    assert!(
        count > 0,
        "helper has at least one caller in the fixture; got count={count}"
    );

    Ok(())
}
