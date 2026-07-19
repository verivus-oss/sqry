//! C073b: verify the LSP server advertises `workspace/executeCommand` only
//! for the server-owned `sqry.*` command-style actions it surfaces itself.
//!
//! Closes audit findings A095/A096/A097/A098/A099 (audit row C073b in
//! `docs/reviews/cli-help-impl-alignment-2026-05-04/audit.md`). Prior to C073a
//! the server had no `executeCommandProvider` field even though the
//! `Self::execute_command` dispatcher was wired.
//!
//! Correction: `sqry.index` must NOT be advertised. It is a client-owned UI
//! command that the `sqry-vscode` extension registers itself (status bar,
//! keybinding, palette, walkthrough) and drives via a direct
//! `workspace/executeCommand` request. `vscode-languageclient`'s
//! `ExecuteCommandFeature` calls `vscode.commands.registerCommand` for every
//! advertised id, so advertising `sqry.index` collides with the extension's own
//! registration ("command 'sqry.index' already exists") and aborts language
//! client startup on every platform. The `execute_command` handler still
//! services `sqry.index` regardless of advertisement. Only the three commands
//! the server surfaces through CodeLens/CodeActions (`sqry.showCallers`,
//! `sqry.showReferences`, `sqry.explainSymbol`) belong in the advertised set.

use anyhow::Result;
use serde_json::json;
use std::collections::HashSet;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{InitializeParams, InitializeResult};

mod common;

#[tokio::test(flavor = "current_thread")]
async fn server_advertises_only_server_owned_execute_commands() -> Result<()> {
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
        "sqry.showCallers",
        "sqry.showReferences",
        "sqry.explainSymbol",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    assert_eq!(
        advertised, expected,
        "executeCommandProvider must advertise exactly the server-owned sqry.* commands"
    );

    assert!(
        !advertised.contains("sqry.index"),
        "sqry.index is client-owned and must not be advertised: advertising it \
         collides with the extension's own registration and aborts client startup"
    );

    Ok(())
}
