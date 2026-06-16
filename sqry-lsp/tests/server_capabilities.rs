use anyhow::Result;
use serde_json::json;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{
    CodeActionKind, CodeActionProviderCapability, InitializeParams, InitializeResult, OneOf,
    TextDocumentSyncCapability, TextDocumentSyncKind, WorkspaceSymbolOptions,
};

mod common;

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
#[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
async fn server_reports_phase2_capabilities_new() -> Result<()> {
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
    assert!(capabilities.hover_provider.is_some());
    assert!(capabilities.definition_provider.is_some());
    assert!(capabilities.references_provider.is_some());
    assert!(capabilities.document_symbol_provider.is_some());

    let sync = capabilities
        .text_document_sync
        .expect("text document sync capability");
    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    match sync {
        TextDocumentSyncCapability::Options(options) => {
            assert_eq!(
                options.change,
                Some(TextDocumentSyncKind::INCREMENTAL),
                "server must advertise incremental sync"
            );
        }
        #[allow(clippy::match_wildcard_for_single_variants)]
        // Test covers specific capability variant
        other => panic!("expected sync options, got {other:?}"),
    }

    match capabilities.workspace_symbol_provider {
        Some(OneOf::Right(WorkspaceSymbolOptions {
            work_done_progress_options,
            ..
        })) => {
            assert_eq!(work_done_progress_options.work_done_progress, Some(false));
        }
        other => panic!("expected workspace symbol options, got {other:?}"),
    }

    match capabilities.code_action_provider {
        Some(CodeActionProviderCapability::Options(options)) => {
            let kinds = options
                .code_action_kinds
                .expect("code action kinds announced");
            assert!(
                kinds.contains(&CodeActionKind::REFACTOR) && kinds.contains(&CodeActionKind::EMPTY),
                "expected refactor and generic actions to be advertised"
            );
        }
        other => panic!("expected code action options, got {other:?}"),
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn server_does_not_route_removed_sqry_ask_request() -> Result<()> {
    // The custom `sqry/ask` natural-language request was removed
    // completely. tower-lsp answers an unregistered method with a
    // JSON-RPC MethodNotFound error rather than a result, so the route
    // must no longer resolve to a successful response.
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let mut server = common::TestServer::new(&root);

    let initialize = Request::build("initialize")
        .params(json!(InitializeParams::default()))
        .id(1)
        .finish();
    let _ = server
        .send_request(initialize)
        .await?
        .expect("initialize response");

    let ask = Request::build("sqry/ask")
        .params(json!({ "query": "who calls authenticate" }))
        .id(2)
        .finish();
    if let Some(response) = server.send_request(ask).await? {
        let (_, body) = response.into_parts();
        assert!(
            body.is_err(),
            "removed sqry/ask request must not return a result; expected a \
             JSON-RPC error, got {body:?}"
        );
    }
    Ok(())
}
