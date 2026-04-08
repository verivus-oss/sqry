use anyhow::Result;
use tower_lsp::lsp_types::{PartialResultParams, WorkDoneProgressParams, WorkspaceSymbolParams};

mod common;

fn new_session(root: &std::path::Path) -> sqry_lsp::session::SessionManager {
    common::ensure_index(root).expect("index build");
    sqry_lsp::session::SessionManager::new(common::options_for(root))
}

#[test]
fn workspace_symbol_filter_only_query_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    // Filter-only query: "lang:rust" with no search terms
    let params = WorkspaceSymbolParams {
        query: "lang:rust".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    let result = result.expect("workspace symbol result");

    // Should return results (not crash with regex parse error)
    assert!(
        result.total > 0,
        "expected symbols from filter-only query, got empty result"
    );

    // All results should be Rust symbols
    for item in &result.items {
        assert_eq!(
            item.language, "rust",
            "expected only Rust symbols, found {}",
            item.language
        );
    }

    Ok(())
}

#[test]
fn workspace_symbol_combined_filter_and_search_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    // Combined query: search term + filter
    let params = WorkspaceSymbolParams {
        query: "process lang:rust".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    let result = result.expect("workspace symbol result");

    // Should return results matching "process" in Rust files
    assert!(result.total > 0, "expected results for combined query");

    // Verify filtering worked
    for item in &result.items {
        assert_eq!(item.language, "rust");
        assert!(
            item.qualified_name.to_lowercase().contains("process")
                || item.info.name.to_lowercase().contains("process"),
            "expected symbol name to contain 'process', got '{}'",
            item.info.name
        );
    }

    Ok(())
}
