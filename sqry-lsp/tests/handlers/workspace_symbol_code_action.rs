use anyhow::Result;
use serde_json::json;
use std::path::Path;
use tower_lsp::lsp_types::{
    CodeActionContext, CodeActionParams, Position, Range, TextDocumentIdentifier, Url,
    WorkspaceSymbolParams,
};

use super::common;

fn new_session(root: &Path) -> sqry_lsp::session::SessionManager {
    common::ensure_index(root).expect("index build");
    let options = sqry_lsp::LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
    };
    sqry_lsp::session::SessionManager::new(options)
}

#[test]
fn workspace_symbol_returns_results_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = WorkspaceSymbolParams {
        query: "process_data".into(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    let page = result.expect("workspace symbol response");
    assert!(!page.items.is_empty());
    assert!(
        page.items
            .iter()
            .any(|item| item.info.name == "process_data")
    );
    Ok(())
}

#[test]
fn workspace_symbol_respects_language_filter_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = WorkspaceSymbolParams {
        query: "lang:typescript describe".into(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    let page = result.expect("workspace symbol response");
    assert!(!page.items.is_empty());
    assert!(page.items.iter().all(|item| item.language == "typescript"));
    Ok(())
}

#[test]
fn workspace_symbol_paginates_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = WorkspaceSymbolParams {
        query: "lang:rust page_size:1 helper".into(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let first_page = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?
        .expect("workspace symbol response");
    assert_eq!(first_page.items.len(), 1);
    let token = first_page
        .next_page_token
        .as_ref()
        .expect("next page token available");

    let next_params = WorkspaceSymbolParams {
        query: format!("lang:rust page_size:1 page:{token}"),
        ..params
    };
    let second_page = sqry_lsp::handlers::workspace_symbol::handle(&session, &next_params)?
        .expect("workspace symbol response");
    assert_eq!(second_page.items.len(), 1);
    assert!(
        second_page.offset > first_page.offset,
        "expected second page offset to advance"
    );
    Ok(())
}

#[test]
fn code_action_offers_find_callers_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");
    let text = std::fs::read_to_string(&path)?;
    session
        .documents()
        .open(&path, Some("rust".into()), 1, &text, &common::test_limits())
        .expect("open document for test");

    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(&path).unwrap(),
        },
        range: Range::new(Position::new(26, 13), Position::new(26, 13)),
        context: CodeActionContext {
            diagnostics: vec![],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::code_action::handle(&session, &params)?;
    let actions = response.expect("code action response");
    let has_callers = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(action) => {
            action.command.as_ref().map(|cmd| cmd.command.as_str())
                == Some(sqry_lsp::handlers::code_action::COMMAND_SHOW_CALLERS)
        }
        _ => false,
    });
    assert!(has_callers);
    Ok(())
}

#[test]
fn execute_command_returns_callers_payload_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");
    let uri = Url::from_file_path(&path).unwrap();
    let position = Position::new(26, 13);

    let args = vec![json!({
        "uri": uri,
        "position": { "line": position.line, "character": position.character }
    })];

    let value = sqry_lsp::handlers::execute_command::execute(
        &session,
        sqry_lsp::handlers::code_action::COMMAND_SHOW_CALLERS,
        args,
    )?;

    let payload = value.expect("execute command payload");
    let object = payload.as_object().expect("payload should be an object");
    assert_eq!(
        object.get("command").and_then(|v| v.as_str()),
        Some(sqry_lsp::handlers::code_action::COMMAND_SHOW_CALLERS)
    );
    let results = object
        .get("results")
        .and_then(|v| v.as_object())
        .expect("results object present");
    assert!(
        results
            .get("locations")
            .and_then(|v| v.as_array())
            .is_some()
    );
    Ok(())
}

#[test]
fn code_action_handles_missing_symbol_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(root.join("src/lib.rs")).unwrap(),
        },
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        context: CodeActionContext {
            diagnostics: vec![],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let response = sqry_lsp::handlers::code_action::handle(&session, &params)?;
    assert!(response.is_none());
    Ok(())
}
