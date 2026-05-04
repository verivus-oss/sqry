mod common;

use assert_cmd::Command;
use common::sqry_bin;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::{document_symbol, index, relations, search};
use sqry_lsp::protocol::{
    RelationKind, SqryListCrossLanguageRelationsParams, SqryRelationParams, SqrySearchParams,
};
use sqry_lsp::session::SessionManager;
use std::env;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DocumentSymbolParams, DocumentSymbolResponse, TextDocumentIdentifier, Url,
    WorkDoneProgressParams,
};

fn copy_fixture_dir(relative: &str) -> TempDir {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let source = workspace_root.join(relative);
    let temp = TempDir::new().expect("create temp dir");
    copy_dir(&source, temp.path()).expect("copy fixture");
    temp
}

#[allow(clippy::similar_names)] // Domain variable naming is intentional
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        #[allow(clippy::similar_names)] // Test fixture variables
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn build_index(project: &Path) {
    let path = sqry_bin();

    Command::new(path)
        .arg("index")
        .current_dir(project)
        .assert()
        .success();
}

fn session_for(path: &Path) -> SessionManager {
    session_for_with_config(path, None)
}

fn session_for_workspace_folder_mode(path: &Path) -> SessionManager {
    let config = tempfile::NamedTempFile::new().expect("create LSP config");
    // Keep temp workspaces rooted at their explicit folder, not an ambient parent .git such as /tmp/.git.
    fs::write(
        config.path(),
        r#"{"sqry":{"projectRootMode":"workspaceFolder"}}"#,
    )
    .expect("write LSP config");

    session_for_with_config(path, Some(config.path()))
}

fn session_for_with_config(path: &Path, config: Option<&Path>) -> SessionManager {
    let options = LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(path.to_path_buf()),
        log_level: "warn".into(),
        config: config.map(Path::to_path_buf),
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    };
    SessionManager::new(options)
}

fn document_symbol_names(session: &SessionManager, source_path: &Path) -> Vec<String> {
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(source_path).expect("file url"),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = document_symbol::handle(session, &params)
        .expect("document symbols")
        .expect("document symbols response");
    let DocumentSymbolResponse::Nested(symbols) = response else {
        panic!("expected nested document symbols");
    };

    symbols.into_iter().map(|symbol| symbol.name).collect()
}

#[test]
fn search_returns_symbols() {
    let project = copy_fixture_dir("sqry-lang-csharp/tests/fixtures/relation_tracking");
    build_index(project.path());
    let session = session_for(project.path());

    let params = SqrySearchParams {
        query: "name:LoadAsync".into(),
        path: None,
        limit: Some(10),
    };

    let result = search::execute(&session, &params).expect("search executes");
    assert!(!result.results.is_empty(), "expected search results");
}

#[test]
fn relation_callers_returns_results() {
    let project = copy_fixture_dir("sqry-lang-csharp/tests/fixtures/relation_tracking");
    build_index(project.path());
    let session = session_for(project.path());

    let params = SqryRelationParams {
        relation: RelationKind::Callers,
        target: "LoadAsync".into(),
        path: None,
        limit: Some(10),
    };

    let result = relations::execute(&session, params).expect("relation executes");
    assert!(!result.results.is_empty(), "expected relation results");
}

#[test]
fn index_status_transitions_from_missing() {
    let project = copy_fixture_dir("sqry-lang-csharp/tests/fixtures/relation_tracking");
    let session = session_for(project.path());

    let before = index::index_status(&session, None).expect("status");
    assert!(!before.exists, "index should be missing before rebuild");

    let reporter = sqry_core::progress::no_op_reporter();
    // Use force=false to test normal lock behavior
    index::rebuild_index(&session, project.path(), &reporter, false).expect("rebuild");

    let after = index::index_status(&session, None).expect("status");
    assert!(after.exists, "index should exist after rebuild");
}

#[test]
fn index_status_returns_stats() {
    let project = copy_fixture_dir("sqry-lang-csharp/tests/fixtures/relation_tracking");
    build_index(project.path());
    let session = session_for(project.path());

    let status = index::index_status(&session, None).expect("status");
    assert!(status.exists, "index should exist");
    assert!(
        status.symbol_count.unwrap_or(0) > 0,
        "expected symbols in single-language fixture"
    );
    assert!(
        status.file_count.unwrap_or(0) > 0,
        "expected files in single-language fixture"
    );
    assert!(
        status.languages.as_ref().is_some_and(|l| !l.is_empty()),
        "expected at least one language"
    );
    assert!(
        status
            .symbol_counts_by_kind
            .as_ref()
            .is_some_and(|m| !m.is_empty()),
        "expected symbol counts by kind"
    );
    // Single-language fixture: no cross-language relations expected
    assert_eq!(
        status.cross_language_relation_count, None,
        "single-language fixture should have no cross-language relations"
    );
    assert!(
        status
            .relation_counts_by_pair
            .as_ref()
            .is_none_or(std::collections::HashMap::is_empty),
        "single-language fixture should have no relation counts by pair"
    );
}

#[test]
fn multi_workspace_index_status_self_heals_corrupt_graph() {
    let project = TempDir::new().expect("create temp dir");
    fs::write(
        project.path().join("lib.rs"),
        "pub fn recovered_symbol() {}\n",
    )
    .expect("write fixture");

    let storage = GraphStorage::new(project.path());
    fs::create_dir_all(storage.graph_dir()).expect("create graph dir");
    fs::write(storage.manifest_path(), "{}").expect("write manifest");
    fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("write corrupt snapshot");

    let session = session_for_workspace_folder_mode(project.path());
    session.set_workspace_folders(vec![project.path().to_path_buf()]);

    let status =
        index::index_status(&session, Some(&project.path().display().to_string())).expect("status");

    assert!(status.exists, "index should be rebuilt after corrupt load");
    assert!(
        status.symbol_count.unwrap_or(0) > 0,
        "rebuilt index should contain the fixture symbol"
    );
}

#[test]
fn document_symbols_fall_back_to_content_when_multi_workspace_graph_is_corrupt() {
    let project = TempDir::new().expect("create temp dir");
    let source_path = project.path().join("lib.rs");
    fs::write(&source_path, "pub fn recovered_symbol() {}\n").expect("write fixture");

    let storage = GraphStorage::new(project.path());
    fs::create_dir_all(storage.graph_dir()).expect("create graph dir");
    fs::write(storage.manifest_path(), "{}").expect("write manifest");
    fs::create_dir(storage.snapshot_path()).expect("create invalid snapshot directory");

    let session = session_for_workspace_folder_mode(project.path());
    session.set_workspace_folders(vec![project.path().to_path_buf()]);

    let symbols = document_symbol_names(&session, &source_path);

    assert!(
        symbols.iter().any(|symbol| symbol == "recovered_symbol"),
        "expected recovered_symbol from content fallback"
    );
}

#[test]
fn rebuild_index_clears_multi_workspace_project_graph_cache() {
    let project = TempDir::new().expect("create temp dir");
    let source_path = project.path().join("lib.rs");
    fs::write(&source_path, "pub fn before_rebuild() {}\n").expect("write fixture");

    let session = session_for_workspace_folder_mode(project.path());
    session.set_workspace_folders(vec![project.path().to_path_buf()]);
    let reporter = sqry_core::progress::no_op_reporter();
    index::rebuild_index(&session, project.path(), &reporter, false).expect("initial rebuild");

    let before_symbols = document_symbol_names(&session, &source_path);
    assert!(
        before_symbols
            .iter()
            .any(|symbol| symbol == "before_rebuild"),
        "initial graph should expose before_rebuild"
    );

    fs::write(&source_path, "pub fn after_rebuild() {}\n").expect("rewrite fixture");
    index::rebuild_index(&session, project.path(), &reporter, false).expect("second rebuild");

    let after_symbols = document_symbol_names(&session, &source_path);
    assert!(
        after_symbols.iter().any(|symbol| symbol == "after_rebuild"),
        "rebuilt graph should expose after_rebuild"
    );
    assert!(
        !after_symbols
            .iter()
            .any(|symbol| symbol == "before_rebuild"),
        "stale project cache should not expose before_rebuild after rebuild"
    );
}

#[test]
fn index_status_counts_cross_language_edges() {
    let project = copy_fixture_dir("test-fixtures/cross-language-example");
    build_index(project.path());
    let session = session_for(project.path());

    let status = index::index_status(&session, None).expect("status");
    assert!(status.exists, "index should exist");

    let cross_count = status.cross_language_relation_count.unwrap_or(0);
    assert!(
        cross_count > 0,
        "multi-language fixture should have cross-language edges"
    );
    let pair_sum: usize = status
        .relation_counts_by_pair
        .as_ref()
        .map_or(0, |m| m.values().sum());

    // Consistency: total must equal sum of per-pair counts
    assert_eq!(
        cross_count, pair_sum,
        "cross_language_relation_count ({cross_count}) must equal sum of relation_counts_by_pair ({pair_sum})"
    );

    // Cross-check with list endpoint
    let list_params = SqryListCrossLanguageRelationsParams {
        path: None,
        limit: Some(10_000),
        ..SqryListCrossLanguageRelationsParams::default()
    };
    let list_result =
        index::list_cross_language_relations(&session, &list_params).expect("list relations");
    assert_eq!(
        cross_count, list_result.total,
        "cross_language_relation_count ({cross_count}) must match list endpoint total ({})",
        list_result.total
    );
}
