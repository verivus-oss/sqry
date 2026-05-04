//! C073b — verify the LSP server advertises `workspace/executeCommand`
//! for the four wired `sqry.*` command-style actions.
//!
//! Closes audit findings A095/A096/A097/A098/A099 (audit row C073b in
//! `docs/reviews/cli-help-impl-alignment-2026-05-04/audit.md`). Prior
//! to C073a the server had no `executeCommandProvider` field even
//! though the `Self::execute_command` dispatcher was wired and the
//! four commands (`sqry.index`, `sqry.showCallers`,
//! `sqry.showReferences`, `sqry.explainSymbol`) were handled.

use anyhow::Result;
use serde_json::json;
use std::collections::HashSet;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{InitializeParams, InitializeResult};

mod common;

#[tokio::test(flavor = "current_thread")]
async fn server_advertises_execute_command_provider_with_four_sqry_commands() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let mut server = common::TestServer::new(&root);

    let request = Request::build("initialize")
        .params(json!(InitializeParams::default()))
        .id(1)
        .finish();

    let response = server
        .send_request(request)
        .await?
        .expect("initialize response");
    let (_, body) = response.into_parts();
    let result: InitializeResult = serde_json::from_value(body?)?;
    let capabilities = result.capabilities;

    let provider = capabilities
        .execute_command_provider
        .expect("server must advertise executeCommandProvider after C073a");

    let advertised: HashSet<String> = provider.commands.into_iter().collect();
    let expected: HashSet<String> = [
        "sqry.index",
        "sqry.showCallers",
        "sqry.showReferences",
        "sqry.explainSymbol",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    assert_eq!(
        advertised, expected,
        "executeCommandProvider must advertise exactly the four wired sqry.* commands"
    );

    Ok(())
}
