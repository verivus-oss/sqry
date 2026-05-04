//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — LSP handler
//! classification-gate contract.
//!
//! Asserts the cross-cutting acceptance criteria for the four LSP
//! handlers that gate on
//! [`sqry_lsp::session::SessionManager::evaluate_handler_gate`]:
//!
//! - `code_action`
//! - `hover`
//! - `document_symbol`
//! - `workspace_symbol`
//!
//! For each handler the contract is:
//!
//! 1. Member-folder URI requests return the "empty + partial" shape
//!    for the handler's result type:
//!      - `Ok(None)` for the LSP-standard handlers (`hover`,
//!        `code_action`, `document_symbol`).
//!      - `WorkspaceSymbolResult { items: [], partial: true, .. }`
//!        for the structured handler we own.
//! 2. Excluded-path URI requests return the "empty + excluded" shape:
//!      - `Ok(None)` for the LSP-standard handlers.
//!      - `WorkspaceSymbolResult { items: [], excluded: true, .. }`
//!        for `workspace_symbol`.
//! 3. **No filesystem probe per folder.** The gate consults the
//!    in-memory `LogicalWorkspace.classify(uri)` only — it does not
//!    open files, run `metadata`, or otherwise touch the filesystem
//!    for a member or excluded path. We assert this indirectly by
//!    pointing the gated URIs at paths that **do not exist** on
//!    disk; the handler MUST still return the gated shape rather
//!    than an error or a `not_found`.
//!
//! The fifth and sixth handler kinds the DAG mentions
//! (`diagnostics`, `codelens`) are not implemented as separate
//! handlers in the current `sqry-lsp` server (no `fn diagnostics(…)`,
//! no `fn code_lens(…)` exist; see `sqry-lsp/src/server.rs`). The
//! contract is therefore vacuously satisfied for them — there is no
//! handler surface to gate. Should those handlers land in a future
//! release, the same gate (`SessionManager::evaluate_handler_gate`)
//! is the only recommended call point and a follow-up test should
//! be added here.

use std::path::PathBuf;
use std::sync::Arc;

use sqry_core::workspace::{LogicalWorkspace, MemberFolder, MemberReason};
use sqry_lsp::session::{HandlerGate, PathClassification, PathMemberReason, SessionManager};
use sqry_lsp::{LspOptions, handlers};
use tower_lsp::lsp_types::{
    CodeActionContext, CodeActionParams, DocumentSymbolParams, HoverParams, PartialResultParams,
    Position, Range, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams, WorkspaceSymbolParams,
};

/// Build a session whose logical workspace contains:
///   - one source root  (`<temp>/src-root/`, exists)
///   - one member folder  (`<temp>/member/`, exists)
///   - one excluded folder  (`<temp>/excluded/`)
///
/// The member and excluded folders are real directories so the
/// `LogicalWorkspace` classifier can resolve them, but the URIs the
/// test points at descend into NON-EXISTENT files inside those
/// folders so the "no per-folder filesystem probe" contract is
/// observable (any `fs::metadata` call on those paths would fail
/// distinctively).
fn make_session_with_member_and_excluded() -> (SessionManager, PathBuf, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    let src_root = root.join("src-root");
    let member = root.join("member");
    let excluded = root.join("excluded");
    std::fs::create_dir_all(&src_root).unwrap();
    std::fs::create_dir_all(&member).unwrap();
    std::fs::create_dir_all(&excluded).unwrap();

    let canonical_src = std::fs::canonicalize(&src_root).unwrap();
    let canonical_member = std::fs::canonicalize(&member).unwrap();
    let canonical_excluded = std::fs::canonicalize(&excluded).unwrap();

    // Build a LogicalWorkspace via single_root then graft member /
    // excluded entries through the round-trip serde mutator (the
    // canonical type does not expose mutators for member_folders /
    // exclusions; this is test-only plumbing).
    let mut ws = LogicalWorkspace::single_root(canonical_src.clone()).unwrap();
    ws = with_grafted_members_and_exclusions(
        ws,
        vec![MemberFolder {
            path: canonical_member.clone(),
            reason: MemberReason::OperationalFolder,
        }],
        vec![canonical_excluded.clone()],
    );

    let opts = LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(canonical_src.clone()),
        log_level: "warn".to_string(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    };
    let session = SessionManager::new(opts);
    session.set_logical_workspace(Arc::new(ws));

    // Sanity: classifier sees the three roles correctly.
    assert!(matches!(
        session.classify_path(&canonical_src.join("file.rs")),
        PathClassification::Source
    ));
    assert!(matches!(
        session.classify_path(&canonical_member.join("file.rs")),
        PathClassification::Member { .. }
    ));
    assert!(matches!(
        session.classify_path(&canonical_excluded.join("file.rs")),
        PathClassification::Excluded
    ));

    // Keep the temp dir alive for the duration of the test by
    // leaking it (test-only; the kernel reclaims at exit).
    std::mem::forget(temp);

    (session, canonical_src, canonical_member, canonical_excluded)
}

fn with_grafted_members_and_exclusions(
    ws: LogicalWorkspace,
    member_folders: Vec<MemberFolder>,
    exclusions: Vec<PathBuf>,
) -> LogicalWorkspace {
    let mut json = serde_json::to_value(&ws).expect("serialise");
    json["member_folders"] = serde_json::to_value(&member_folders).expect("serialise members");
    json["exclusions"] = serde_json::to_value(&exclusions).expect("serialise exclusions");
    serde_json::from_value(json).expect("deserialise")
}

fn uri_under(folder: &std::path::Path, leaf: &str) -> Url {
    let path = folder.join(leaf);
    Url::from_file_path(path).expect("file path to URI")
}

// ---------------------------------------------------------------------------
// HandlerGate primitive
// ---------------------------------------------------------------------------

#[test]
fn evaluate_handler_gate_classifies_source_member_excluded() {
    let (session, src_root, member, excluded) = make_session_with_member_and_excluded();

    let src_uri = uri_under(&src_root, "missing.rs");
    let member_uri = uri_under(&member, "missing.rs");
    let excluded_uri = uri_under(&excluded, "missing.rs");

    assert_eq!(
        session.evaluate_handler_gate(&src_uri),
        HandlerGate::Continue
    );
    assert_eq!(
        session.evaluate_handler_gate(&member_uri),
        HandlerGate::Member(PathMemberReason::OperationalFolder)
    );
    assert_eq!(
        session.evaluate_handler_gate(&excluded_uri),
        HandlerGate::Excluded
    );
}

// ---------------------------------------------------------------------------
// hover
// ---------------------------------------------------------------------------

fn hover_params(uri: &Url) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 0),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[test]
fn hover_returns_none_for_member_folder_uri() {
    let (session, _, member, _) = make_session_with_member_and_excluded();
    let params = hover_params(&uri_under(&member, "missing.rs"));
    let result = handlers::hover::handle(&session, &params).expect("handler returns Ok");
    assert!(
        result.is_none(),
        "member-folder URI must return Ok(None); got {result:?}"
    );
}

#[test]
fn hover_returns_none_for_excluded_uri() {
    let (session, _, _, excluded) = make_session_with_member_and_excluded();
    let params = hover_params(&uri_under(&excluded, "missing.rs"));
    let result = handlers::hover::handle(&session, &params).expect("handler returns Ok");
    assert!(
        result.is_none(),
        "excluded URI must return Ok(None); got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// code_action
// ---------------------------------------------------------------------------

fn code_action_params(uri: &Url) -> CodeActionParams {
    CodeActionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        },
        context: CodeActionContext {
            diagnostics: Vec::new(),
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[test]
fn code_action_returns_none_for_member_folder_uri() {
    let (session, _, member, _) = make_session_with_member_and_excluded();
    let params = code_action_params(&uri_under(&member, "missing.rs"));
    let result = handlers::code_action::handle(&session, &params).expect("handler returns Ok");
    assert!(
        result.is_none(),
        "member-folder URI must return Ok(None); got {result:?}"
    );
}

#[test]
fn code_action_returns_none_for_excluded_uri() {
    let (session, _, _, excluded) = make_session_with_member_and_excluded();
    let params = code_action_params(&uri_under(&excluded, "missing.rs"));
    let result = handlers::code_action::handle(&session, &params).expect("handler returns Ok");
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// document_symbol
// ---------------------------------------------------------------------------

fn document_symbol_params(uri: &Url) -> DocumentSymbolParams {
    DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[test]
fn document_symbol_returns_none_for_member_folder_uri() {
    let (session, _, member, _) = make_session_with_member_and_excluded();
    let params = document_symbol_params(&uri_under(&member, "missing.rs"));
    let result = handlers::document_symbol::handle(&session, &params).expect("handler returns Ok");
    assert!(
        result.is_none(),
        "member-folder URI must return Ok(None); got {result:?}"
    );
}

#[test]
fn document_symbol_returns_none_for_excluded_uri() {
    let (session, _, _, excluded) = make_session_with_member_and_excluded();
    let params = document_symbol_params(&uri_under(&excluded, "missing.rs"));
    let result = handlers::document_symbol::handle(&session, &params).expect("handler returns Ok");
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// workspace_symbol — structured result with partial / excluded flags
// ---------------------------------------------------------------------------

fn workspace_symbol_params(query: &str) -> WorkspaceSymbolParams {
    WorkspaceSymbolParams {
        query: query.to_string(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[test]
fn workspace_symbol_marks_partial_when_member_folder_is_a_workspace_folder() {
    // Configure the session so the workspace_symbol search root list
    // includes the member folder. The handler must skip it and set
    // `partial: true` on the response.
    let (session, _src, member, _excluded) = make_session_with_member_and_excluded();
    session.set_workspace_folders(vec![member.clone()]);

    let params = workspace_symbol_params("anything");
    let result = handlers::workspace_symbol::handle(&session, &params)
        .expect("handler returns Ok")
        .expect("Some result");

    assert!(
        result.partial,
        "member folder in search-root list must produce partial: true; got {result:?}"
    );
    assert!(
        !result.excluded,
        "member folder must NOT set excluded: true; got {result:?}"
    );
    assert!(
        result.items.is_empty(),
        "member-folder-only search must return no items; got {} items",
        result.items.len()
    );
}

#[test]
fn workspace_symbol_marks_excluded_when_search_root_is_excluded() {
    let (session, _src, _member, excluded) = make_session_with_member_and_excluded();
    session.set_workspace_folders(vec![excluded.clone()]);

    let params = workspace_symbol_params("anything");
    let result = handlers::workspace_symbol::handle(&session, &params)
        .expect("handler returns Ok")
        .expect("Some result");

    assert!(
        result.excluded,
        "excluded folder in search-root list must produce excluded: true; got {result:?}"
    );
    assert!(
        result.items.is_empty(),
        "excluded-only search must return no items"
    );
}

// ---------------------------------------------------------------------------
// "No per-folder filesystem probe" sweep
// ---------------------------------------------------------------------------

#[test]
fn handlers_do_not_probe_filesystem_for_gated_paths() {
    // Source-grep guard: the gated handler files MUST NOT call
    // `Path::exists`, `Path::is_dir`, `Path::is_file`,
    // `fs::metadata`, `tokio::fs::metadata`, or any other per-folder
    // filesystem probe. They consult `evaluate_handler_gate` only.
    //
    // This is the regression guard for the "no LSP path makes a
    // per-folder filesystem probe" acceptance criterion.
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let handlers_dir = project_root.join("src").join("handlers");
    // STEP_11_4 iter-2 — extended to cover workspace_symbol +
    // diagnostics + codelens so the negative-probe sweep is
    // symmetric across every gated handler surface.
    let handler_files = [
        "hover.rs",
        "code_action.rs",
        "document_symbol.rs",
        "workspace_symbol.rs",
        "diagnostics.rs",
        "codelens.rs",
    ];

    for handler_file in handler_files {
        let path = handlers_dir.join(handler_file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let banned_calls = [
            "Path::exists",
            "Path::is_dir",
            "Path::is_file",
            "fs::metadata",
            "tokio::fs::metadata",
            ".exists()",
            ".is_dir()",
            ".is_file()",
        ];
        for banned in banned_calls {
            assert!(
                !source.contains(banned),
                "{handler_file} contains banned per-folder filesystem probe '{banned}'; \
                 STEP_11_4 acceptance criterion: handlers must consult \
                 evaluate_handler_gate only",
            );
        }
    }
}

#[test]
fn handlers_call_evaluate_handler_gate_at_top() {
    // The mirror of the previous test: the gated handler files MUST
    // mention `evaluate_handler_gate` so the gate is actually
    // wired in (a passing "no-fs-probe" guard plus a missing gate
    // call would trivially satisfy the negative assertion).
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let handlers_dir = project_root.join("src").join("handlers");
    let handler_files = [
        "hover.rs",
        "code_action.rs",
        "document_symbol.rs",
        "diagnostics.rs",
        "codelens.rs",
    ];

    for handler_file in handler_files {
        let path = handlers_dir.join(handler_file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(
            source.contains("evaluate_handler_gate"),
            "{handler_file} must call session.evaluate_handler_gate(...) — STEP_11_4",
        );
    }

    // workspace_symbol takes a different shape (per-search-root
    // filter via classify) so we assert it consults the
    // classifier directly.
    let ws_path = handlers_dir.join("workspace_symbol.rs");
    let ws_source = std::fs::read_to_string(&ws_path).unwrap();
    assert!(
        ws_source.contains("logical_workspace") && ws_source.contains("classify"),
        "workspace_symbol.rs must filter search roots through \
         logical_workspace.classify(...) — STEP_11_4",
    );
}

// ---------------------------------------------------------------------------
// diagnostics + codelens — STEP_11_4 iter-2 (Codex BLOCK fix)
// ---------------------------------------------------------------------------

#[test]
fn diagnostics_returns_partial_for_member_folder_uri() {
    let (session, _, member, _) = make_session_with_member_and_excluded();
    let uri = uri_under(&member, "missing.rs");
    let outcome = handlers::diagnostics::handle(&session, &uri).expect("handler returns Ok");
    assert!(outcome.diagnostics.is_empty());
    assert!(outcome.partial);
    assert!(!outcome.excluded);
}

#[test]
fn diagnostics_returns_excluded_for_excluded_uri() {
    let (session, _, _, excluded) = make_session_with_member_and_excluded();
    let uri = uri_under(&excluded, "missing.rs");
    let outcome = handlers::diagnostics::handle(&session, &uri).expect("handler returns Ok");
    assert!(outcome.diagnostics.is_empty());
    assert!(outcome.excluded);
    assert!(!outcome.partial);
}

#[test]
fn codelens_returns_partial_for_member_folder_uri() {
    use tower_lsp::lsp_types::CodeLensParams;
    let (session, _, member, _) = make_session_with_member_and_excluded();
    let params = CodeLensParams {
        text_document: TextDocumentIdentifier {
            uri: uri_under(&member, "missing.rs"),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let outcome = handlers::codelens::handle(&session, &params).expect("handler returns Ok");
    assert!(outcome.lenses.is_empty());
    assert!(outcome.partial);
    assert!(!outcome.excluded);
}

#[test]
fn codelens_returns_excluded_for_excluded_uri() {
    use tower_lsp::lsp_types::CodeLensParams;
    let (session, _, _, excluded) = make_session_with_member_and_excluded();
    let params = CodeLensParams {
        text_document: TextDocumentIdentifier {
            uri: uri_under(&excluded, "missing.rs"),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let outcome = handlers::codelens::handle(&session, &params).expect("handler returns Ok");
    assert!(outcome.lenses.is_empty());
    assert!(outcome.excluded);
    assert!(!outcome.partial);
}
