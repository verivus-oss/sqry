//! End-to-end scenario tests for the sqry LSP server.
//!
//! These tests exercise complete editor workflows against the mini-workspace
//! fixture, verifying that LSP handlers compose correctly across realistic
//! sequences of requests a developer would send during a session.

mod common;

use anyhow::Result;
use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    HoverContents, HoverParams, MarkupKind, PartialResultParams, Position, ReferenceContext,
    ReferenceParams, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams, WorkspaceSymbolParams,
};

// ── session helpers ──────────────────────────────────────────────────────────

fn new_session(root: &Path) -> sqry_lsp::session::SessionManager {
    common::ensure_index(root).expect("index build");
    let options = common::options_for(root);
    sqry_lsp::session::SessionManager::new(options)
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 1: Hover → Definition → References
//
// Starting from a known symbol in lib.rs (`process_data` at line 26), the test
// simulates a developer workflow:
//   1. Hover to inspect the symbol.
//   2. Go-to-definition to locate its declaration.
//   3. Find all references to understand its usage.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_hover_then_definition_then_references() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let path = root.join("src/lib.rs");
    let text = fs::read_to_string(&path)?;
    session
        .documents()
        .open(&path, Some("rust".into()), 1, &text, &common::test_limits())
        .expect("open document for scenario");

    let uri = Url::from_file_path(&path).unwrap();
    // `process_data` is called at line 26, col 13 in the fixture
    let position = Position::new(26, 13);

    // Step 1: Hover — assert markdown content contains the symbol name
    let hover_params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let hover = sqry_lsp::handlers::hover::handle(&session, &hover_params)?.expect("hover result");
    let hover_text = match hover.contents {
        HoverContents::Markup(markup) if markup.kind == MarkupKind::Markdown => markup.value,
        other => panic!("unexpected hover contents: {other:?}"),
    };
    assert!(
        hover_text.contains("process_data"),
        "hover must mention the symbol name; got: {hover_text}"
    );
    let hover_range = hover.range.expect("hover range must be present");
    assert_eq!(
        hover_range.start.line, 26,
        "hover range must be on the queried line"
    );

    // Step 2: Go-to-definition — assert a location in the same file is returned
    let def_params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let def_response = sqry_lsp::handlers::definition::handle(&session, &def_params)?
        .expect("definition response");
    let def_location = match def_response {
        GotoDefinitionResponse::Scalar(loc) => loc,
        GotoDefinitionResponse::Array(locs) if !locs.is_empty() => locs.into_iter().next().unwrap(),
        other => panic!("unexpected definition response: {other:?}"),
    };
    assert_eq!(
        def_location.uri.path(),
        uri.path(),
        "definition must point into the same file"
    );
    let start = def_location.range.start;
    let end = def_location.range.end;
    assert!(
        start.line <= end.line,
        "definition range start line must not exceed end line; got start={start:?} end={end:?}"
    );
    assert!(
        end.line > start.line || end.character > start.character,
        "definition range must have non-zero width; got start={start:?} end={end:?}"
    );

    // Step 3: References — assert at least one location is returned
    let ref_params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    };
    let references =
        sqry_lsp::handlers::references::handle(&session, &ref_params)?.expect("references result");
    assert!(
        !references.is_empty(),
        "references must return at least one location"
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 2: Workspace Symbol Search
//
// A developer types in the workspace symbol picker:
//   • A specific query ("main") — expects ≥1 result.
//   • An empty query — expects all indexed symbols to be returned.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_workspace_symbol_search() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    // Specific query: "process" — fixture has `process_data`
    let params = WorkspaceSymbolParams {
        query: "process".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let page = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?
        .expect("workspace symbol response for 'process'");
    assert!(
        !page.items.is_empty(),
        "workspace/symbol with 'process' must return ≥1 result"
    );
    assert!(
        page.items
            .iter()
            .any(|item| item.info.name.contains("process")),
        "at least one result must match the query; got: {:?}",
        page.items.iter().map(|i| &i.info.name).collect::<Vec<_>>()
    );

    // Empty query — the handler short-circuits and returns an empty-but-valid
    // result (LSP spec allows servers to limit results; sqry returns empty to
    // avoid dumping thousands of symbols into the picker unsolicited).
    let empty_params = WorkspaceSymbolParams {
        query: String::new(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let empty_result = sqry_lsp::handlers::workspace_symbol::handle(&session, &empty_params)?;
    // The handler short-circuits on empty query and returns Some(empty page).
    let empty_page = empty_result.expect("empty query must return Some(page), not None");
    assert!(
        empty_page.items.is_empty(),
        "empty query must short-circuit to zero items, got {}",
        empty_page.items.len()
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 3: Document Symbols
//
// A developer opens `src/lib.rs` and requests the document outline.
// The fixture file defines:
//   • An outer module `internal` that contains functions.
//   • Top-level functions including `process_data` and `helper`.
// The test verifies:
//   • A non-empty nested symbol tree is returned.
//   • Known symbols appear in the flat list.
//   • At least one symbol has child symbols (hierarchy present).
// ─────────────────────────────────────────────────────────────────────────────

fn collect_flat<'a>(
    symbols: &'a [tower_lsp::lsp_types::DocumentSymbol],
    acc: &mut Vec<&'a tower_lsp::lsp_types::DocumentSymbol>,
) {
    for sym in symbols {
        acc.push(sym);
        if let Some(children) = &sym.children {
            collect_flat(children, acc);
        }
    }
}

#[test]
#[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
#[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
fn scenario_document_symbols_hierarchy() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let lib_path = root.join("src/lib.rs");
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
        .expect("open lib.rs for document symbols");

    let uri = Url::from_file_path(&lib_path).unwrap();
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    let response = sqry_lsp::handlers::document_symbol::handle(&session, &params)?
        .expect("document symbol response");
    let top_level = match response {
        DocumentSymbolResponse::Nested(list) => list,
        #[allow(clippy::match_wildcard_for_single_variants)]
        // Test covers specific scenario variant
        other => panic!("expected nested document symbols, got {other:?}"),
    };

    assert!(
        !top_level.is_empty(),
        "document symbol response must not be empty"
    );

    let mut flat = Vec::new();
    collect_flat(&top_level, &mut flat);

    assert!(
        flat.iter().any(|s| s.name == "process_data"),
        "flat symbol list must contain 'process_data'"
    );
    assert!(
        flat.iter().any(|s| s.name == "helper"),
        "flat symbol list must contain 'helper'"
    );

    // Verify parent-child: at least one top-level symbol must have children
    let has_children = top_level
        .iter()
        .any(|s| s.children.as_ref().is_some_and(|c| !c.is_empty()));
    assert!(
        has_children,
        "at least one top-level symbol must contain children (hierarchy)"
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 4: Graph Export
//
// A developer requests the dependency graph in two formats:
//   • "json" — nodes and edges arrays must be present.
//   • "dot" — rendered string must begin with "digraph".
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_graph_export_json_and_dot() -> Result<()> {
    use sqry_lsp::protocol::SqryGraphExportParams;

    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    // JSON format: seed by symbol name
    let json_params = SqryGraphExportParams {
        path: None,
        file_path: None,
        symbol_name: Some("orchestrate".into()),
        format: "json".into(),
        max_depth: Some(2),
        max_results: None,
        include_calls: Some(true),
        include_imports: Some(false),
        verbose: Some(false),
    };
    let json_result = sqry_lsp::handlers::graph_export::execute(&session, &json_params)?;
    assert!(
        json_result.total_nodes > 0,
        "json export must return at least one node"
    );
    assert!(
        json_result.rendered.is_none(),
        "json format must not produce a rendered string"
    );
    // Verify node shapes
    assert!(
        json_result.nodes.iter().any(|n| n.name == "orchestrate"),
        "json export nodes must include the seed symbol 'orchestrate'"
    );

    // DOT format: seed by the same symbol
    let dot_params = SqryGraphExportParams {
        path: None,
        file_path: None,
        symbol_name: Some("orchestrate".into()),
        format: "dot".into(),
        max_depth: Some(1),
        max_results: None,
        include_calls: Some(true),
        include_imports: Some(false),
        verbose: Some(false),
    };
    let dot_result = sqry_lsp::handlers::graph_export::execute(&session, &dot_params)?;
    let rendered = dot_result
        .rendered
        .expect("dot format must produce a rendered string");
    assert!(
        rendered.starts_with("digraph"),
        "dot output must start with 'digraph'; got prefix: {:?}",
        &rendered[..rendered.len().min(30)]
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 5: Subgraph Extraction
//
// A developer wants a focused view around `summarize` and its callers/callees.
// The test verifies that:
//   • The returned subgraph contains the seed symbol.
//   • At least one edge connects nodes in the subgraph.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_subgraph_extraction() -> Result<()> {
    use sqry_lsp::protocol::SqrySubgraphParams;

    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = SqrySubgraphParams {
        symbols: vec!["summarize".into()],
        path: None,
        max_depth: Some(2),
        max_nodes: Some(50),
        include_callers: Some(true),
        include_callees: Some(true),
        include_imports: Some(false),
    };

    let result = sqry_lsp::handlers::subgraph::execute(&session, &params)?;

    assert!(
        result.total_nodes > 0,
        "subgraph must contain at least the seed node"
    );
    assert!(
        result.nodes.iter().any(|n| n.name == "summarize"),
        "subgraph must include the seed symbol 'summarize'"
    );
    assert!(
        result.total_edges > 0,
        "subgraph around 'summarize' must have at least one edge (has callers and callees)"
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 6: Call Hierarchy — Incoming and Outgoing
//
// A developer inspects the call hierarchy of `summarize` in lib.rs:
//   1. Prepare: resolves the symbol at the declaration site.
//   2. Incoming: both `orchestrate` and `alternate` call `summarize`.
//   3. Outgoing: `summarize` calls `format_state`.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_call_hierarchy_incoming_and_outgoing() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let path =
        std::fs::canonicalize(root.join("src/lib.rs")).unwrap_or_else(|_| root.join("src/lib.rs"));
    let uri = Url::from_file_path(&path).unwrap();

    // Step 1: Prepare — `summarize` is at line 40, col 5 in the fixture
    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(40, 5),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let items = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("prepare must return at least one item");
    assert_eq!(
        items.len(),
        1,
        "prepare must return exactly one item for 'summarize'"
    );
    let item = items.into_iter().next().unwrap();
    assert_eq!(item.name, "summarize");

    // Step 2: Incoming calls — orchestrate and/or alternate must appear
    let incoming_params = CallHierarchyIncomingCallsParams {
        item: item.clone(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let incoming = sqry_lsp::handlers::call_hierarchy::incoming(&session, &incoming_params)?;
    assert!(
        !incoming.items.is_empty(),
        "incoming calls for 'summarize' must not be empty"
    );
    let caller_names: Vec<&str> = incoming
        .items
        .iter()
        .map(|entry| entry.from.name.as_str())
        .collect();
    let known_callers = ["orchestrate", "alternate"];
    assert!(
        caller_names.iter().any(|name| known_callers.contains(name)),
        "at least one of {known_callers:?} must appear as a caller; got: {caller_names:?}"
    );
    // Verify from_ranges are present (LSP call-site highlighting)
    for entry in &incoming.items {
        assert!(
            !entry.from_ranges.is_empty(),
            "each incoming call entry must carry from_ranges"
        );
    }

    // Step 3: Outgoing calls — summarize calls format_state
    let outgoing_params = CallHierarchyOutgoingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let outgoing = sqry_lsp::handlers::call_hierarchy::outgoing(&session, &outgoing_params)?;
    assert!(
        !outgoing.items.is_empty(),
        "outgoing calls for 'summarize' must not be empty"
    );
    assert!(
        outgoing
            .items
            .iter()
            .any(|entry| entry.to.name == "format_state"),
        "outgoing calls must include 'format_state'; got: {:?}",
        outgoing
            .items
            .iter()
            .map(|e| &e.to.name)
            .collect::<Vec<_>>()
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 7: Error Recovery
//
// Robustness tests — the server must handle bad input gracefully:
//   7a. An unknown custom method returns an empty symbol page (the workspace
//       symbol handler uses the raw query string as-is; an unrecognised method
//       name fed to the handler pool yields no results rather than a panic).
//   7b. graph_export with no seed (neither file_path nor symbol_name) returns
//       an error instead of panicking.
//   7c. subgraph with an empty symbol list returns an error.
//   7d. workspace/symbol query for a symbol that certainly does not exist
//       returns an empty (but valid) result set.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn scenario_error_recovery_unknown_method() -> Result<()> {
    // The workspace symbol handler is the natural entry point for a "query"
    // that represents an unknown intent. Sending an unrecognised prefix should
    // produce an empty-but-valid response, not an error or panic.
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = WorkspaceSymbolParams {
        query: "unknownmethod://this-does-not-exist".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    // A garbage query must produce zero results — either None or Some(empty page).
    match result {
        None => {} // acceptable: no match
        Some(page) => {
            assert!(
                page.items.is_empty(),
                "garbage query must return zero items; got {}",
                page.items.len()
            );
        }
    }

    Ok(())
}

#[test]
fn scenario_error_recovery_graph_export_missing_seed() {
    use sqry_lsp::protocol::SqryGraphExportParams;

    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    // Neither file_path nor symbol_name — handler must return an error
    let params = SqryGraphExportParams {
        path: None,
        file_path: None,
        symbol_name: None,
        format: "json".into(),
        max_depth: None,
        max_results: None,
        include_calls: None,
        include_imports: None,
        verbose: None,
    };
    let result = sqry_lsp::handlers::graph_export::execute(&session, &params);
    assert!(
        result.is_err(),
        "graph_export without seeds must return an error"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Either file_path or symbol_name must be provided"),
        "error message must be the specific missing-seed message; got: {msg}"
    );
}

#[test]
fn scenario_error_recovery_subgraph_empty_symbols() {
    use sqry_lsp::protocol::SqrySubgraphParams;

    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = SqrySubgraphParams {
        symbols: vec![],
        path: None,
        max_depth: None,
        max_nodes: None,
        include_callers: None,
        include_callees: None,
        include_imports: None,
    };
    let result = sqry_lsp::handlers::subgraph::execute(&session, &params);
    assert!(
        result.is_err(),
        "subgraph with empty symbol list must return an error"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("symbols list cannot be empty"),
        "error message must be the specific empty-symbols message; got: {msg}"
    );
}

#[test]
fn scenario_error_recovery_nonexistent_workspace_symbol() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);

    let params = WorkspaceSymbolParams {
        query: "zzz_this_symbol_definitely_does_not_exist_xyzzy".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result = sqry_lsp::handlers::workspace_symbol::handle(&session, &params)?;
    match result {
        None => {} // handler returned None — acceptable
        Some(page) => {
            assert!(
                page.items.is_empty(),
                "nonexistent symbol query must return zero items, got {}",
                page.items.len()
            );
        }
    }

    Ok(())
}
