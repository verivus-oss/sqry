mod common;

use assert_cmd::Command;
use common::sqry_bin;
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::{index, relations, search};
use sqry_lsp::protocol::{
    RelationKind, SqryListCrossLanguageRelationsParams, SqryRelationParams, SqrySearchParams,
};
use sqry_lsp::session::SessionManager;
use std::env;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn copy_fixture_dir(relative: &str) -> TempDir {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let source = workspace_root.join(relative);
    let temp = TempDir::new().expect("create temp dir");
    copy_dir(&source, temp.path()).expect("copy fixture");
    temp
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
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
    let options = LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(path.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
    };
    SessionManager::new(options)
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
            .is_none_or(|m| m.is_empty()),
        "single-language fixture should have no relation counts by pair"
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
