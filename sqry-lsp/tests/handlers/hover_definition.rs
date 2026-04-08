use anyhow::Result;
use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, MarkupKind, Position,
    TextDocumentIdentifier, TextDocumentPositionParams, Url,
};

use super::common;

fn new_session(root: &Path) -> sqry_lsp::session::SessionManager {
    super::common::ensure_index(root).expect("index build");
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
#[allow(clippy::default_trait_access)] // Type inference handles default
fn hover_returns_semantic_metadata_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");
    let text = fs::read_to_string(&path)?;
    session
        .documents()
        .open(&path, Some("rust".into()), 1, &text, &common::test_limits())
        .expect("open document for test");

    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(26, 13),
        },
        work_done_progress_params: Default::default(),
    };

    let hover = sqry_lsp::handlers::hover::handle(&session, &params)?;
    let hover = hover.expect("hover result");
    let value = match hover.contents {
        HoverContents::Markup(markup) if markup.kind == MarkupKind::Markdown => markup.value,
        other => panic!("unexpected hover contents: {other:?}"),
    };
    assert!(value.contains("process_data"));
    assert!(value.contains("Qualified"));
    let hover_range = hover.range.expect("hover range");
    assert_eq!(hover_range.start.line, 26);
    Ok(())
}

#[test]
#[allow(clippy::default_trait_access)] // Type inference handles default
fn definition_returns_function_location_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/utils.ts");

    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(14, 16),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::definition::handle(&session, &params)?;
    let response = response.expect("definition response");
    let location = match response {
        GotoDefinitionResponse::Scalar(loc) => loc,
        other => panic!("unexpected definition response: {other:?}"),
    };
    assert_eq!(location.uri, Url::from_file_path(&path).unwrap());
    assert_eq!(location.range.start.line, 14);
    if location.range.start.line == location.range.end.line {
        assert!(location.range.start.character <= location.range.end.character);
    }
    Ok(())
}
