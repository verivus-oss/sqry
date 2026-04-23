use anyhow::Result;
use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Position, ReferenceContext,
    ReferenceParams, TextDocumentIdentifier, TextDocumentPositionParams, Url,
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
        daemon: false,
        daemon_socket: None,
    };
    sqry_lsp::session::SessionManager::new(options)
}

fn collect_symbols<'a>(symbols: &'a [DocumentSymbol], acc: &mut Vec<&'a DocumentSymbol>) {
    for symbol in symbols {
        acc.push(symbol);
        if let Some(children) = &symbol.children {
            collect_symbols(children, acc);
        }
    }
}

#[test]
#[allow(clippy::default_trait_access)] // Type inference handles default
fn references_include_declaration_when_requested_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");
    let text = fs::read_to_string(&path)?;
    session
        .documents()
        .open(&path, Some("rust".into()), 1, &text, &common::test_limits())
        .expect("open document for test");

    let uri = Url::from_file_path(&path).unwrap();
    let position = Position::new(26, 13);
    let node = session
        .node_at(&uri, position)
        .expect("node lookup succeeded")
        .expect("node found");
    let declaration = sqry_lsp::handlers::definition::node_location(&session, &node)?;

    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    };

    let results =
        sqry_lsp::handlers::references::handle(&session, &params)?.expect("reference results");
    assert!(!results.is_empty());
    assert!(results.iter().any(|loc| loc == &declaration));
    Ok(())
}

#[test]
#[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
#[allow(clippy::default_trait_access)] // Type inference handles default
fn document_symbol_emits_hierarchy_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let lib_path = root.join("src/lib.rs");
    let uri = Url::from_file_path(&lib_path).unwrap();
    let text = fs::read_to_string(&lib_path)?;
    session
        .documents()
        .open(
            &lib_path,
            Some("rust".into()),
            1,
            &text,
            &common::test_limits(),
        )
        .expect("open document for test");

    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::document_symbol::handle(&session, &params)?
        .expect("document symbol response");
    let symbols = match response {
        DocumentSymbolResponse::Nested(list) => list,
        other => panic!("expected nested document symbols, got {other:?}"),
    };

    let mut flat = Vec::new();
    collect_symbols(&symbols, &mut flat);

    assert!(flat.iter().any(|sym| sym.name == "process_data"));
    assert!(flat.iter().any(|sym| sym.name == "helper"));
    Ok(())
}
