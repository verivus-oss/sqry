mod common;

use assert_cmd::Command;
use common::sqry_bin;
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::{index, relations, search};
use sqry_lsp::protocol::{RelationKind, SqryRelationParams, SqrySearchParams};
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
