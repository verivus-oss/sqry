//! C075b — verify the `textDocument/diagnostic` handler synthesises
//! unused-symbol and cycle-member diagnostics after C075a wired the
//! sqry-db `UnusedQuery` / `CyclesQuery` and the sqry-core
//! `build_duplicate_groups_graph` helper into the gated handler.
//!
//! Closes audit finding A101 (audit row C075b in
//! `docs/reviews/cli-help-impl-alignment-2026-05-04/audit.md`).

use anyhow::Result;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::Url;

mod common;

fn rust_unused_and_cycle_workspace() -> Result<TempDir> {
    let tmp = tempfile::tempdir()?;
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir)?;
    // - `silent_orphan` is a private function with no callers and no
    //   reachable entry-point reference -> unused.
    // - `cycle_a` and `cycle_b` form a 2-function call cycle.
    fs::write(
        src_dir.join("lib.rs"),
        r#"
fn silent_orphan() -> u32 {
    7
}

pub fn cycle_a() -> u32 {
    cycle_b()
}

pub fn cycle_b() -> u32 {
    cycle_a()
}
"#,
    )?;
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lsp-diagnostic-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    Ok(tmp)
}

#[tokio::test(flavor = "current_thread")]
async fn diagnostic_emits_unused_and_cycle_warnings() -> Result<()> {
    let workspace = rust_unused_and_cycle_workspace()?;
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

    let diag_request = Request::build("textDocument/diagnostic".to_string())
        .params(json!({ "textDocument": { "uri": lib_uri } }))
        .id(1i64)
        .finish();
    let response = server
        .send_request(diag_request)
        .await?
        .expect("diagnostic response");
    let (_, body) = response.into_parts();
    let value: Value = body?;

    // LSP 3.17 wraps the report under `kind: "full"` with an `items` array.
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("diagnostic response must carry an `items` array; got {value}"));

    assert!(
        !items.is_empty(),
        "diagnostic must return at least one diagnostic for the unused/cycle fixture; got {value}"
    );

    let messages: Vec<String> = items
        .iter()
        .filter_map(|d| d.get("message").and_then(Value::as_str).map(str::to_string))
        .collect();

    assert!(
        messages.iter().any(|m| m.to_lowercase().contains("unused")),
        "expected at least one diagnostic with `unused` in the message; messages={messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.to_lowercase().contains("cycle")),
        "expected at least one diagnostic with `cycle` in the message; messages={messages:?}"
    );

    Ok(())
}
