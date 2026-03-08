//! Tests for unsupported `SearchMode` error handling in the text searcher

use sqry_core::search::{SearchConfig, SearchMode, Searcher};
use std::fs;
use tempfile::TempDir;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

#[test]
fn test_semantic_mode_returns_error() {
    init_logging();
    log::info!("Testing test_semantic_mode_returns_error");

    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("test.rs"), "fn foo() {}").unwrap();

    let searcher = Searcher::new().unwrap();
    let config = SearchConfig {
        mode: SearchMode::Semantic,
        ..Default::default()
    };

    let result = searcher.search("fn", &[dir.path()], &config);

    assert!(result.is_err(), "Semantic mode should return error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported by the text searcher"),
        "Error should mention 'not supported by the text searcher', got: {err_msg}"
    );
    assert!(
        err_msg.contains("Semantic"),
        "Error should mention 'Semantic' mode, got: {err_msg}"
    );
}

#[test]
fn test_fuzzy_mode_returns_error() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("test.rs"), "fn foo() {}").unwrap();

    let searcher = Searcher::new().unwrap();
    let config = SearchConfig {
        mode: SearchMode::Fuzzy,
        ..Default::default()
    };

    let result = searcher.search("fn", &[dir.path()], &config);

    assert!(result.is_err(), "Fuzzy mode should return error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported by the text searcher"),
        "Error should mention 'not supported by the text searcher', got: {err_msg}"
    );
    assert!(
        err_msg.contains("Fuzzy"),
        "Error should mention 'Fuzzy' mode, got: {err_msg}"
    );
}

#[test]
fn test_text_mode_still_works() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("test.rs"), "fn foo() {}").unwrap();

    let searcher = Searcher::new().unwrap();
    let config = SearchConfig {
        mode: SearchMode::Text,
        ..Default::default()
    };

    let result = searcher.search("fn", &[dir.path()], &config);
    assert!(result.is_ok(), "Text mode should work");
    assert!(!result.unwrap().is_empty(), "Text mode should find matches");
}

#[test]
fn test_regex_mode_still_works() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("test.rs"), "fn foo() {}").unwrap();

    let searcher = Searcher::new().unwrap();
    let config = SearchConfig {
        mode: SearchMode::Regex,
        ..Default::default()
    };

    let result = searcher.search("fn", &[dir.path()], &config);
    assert!(result.is_ok(), "Regex mode should work");
    assert!(
        !result.unwrap().is_empty(),
        "Regex mode should find matches"
    );
}
