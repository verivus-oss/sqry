//! CLI integration tests for graph mode (FR-2025-021 - Unified Graph)
//!
//! These tests verify that graph mode is always enabled (legacy mode removed).
//! The legacy `SQRY_USE_GRAPH` environment variable has been removed and is
//! ignored if set; it has no effect on behavior.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

use sqry_core::graph::unified::persistence::{GraphStorage, Manifest};
use sqry_core::test_support::verbosity;
use std::sync::Once;

static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Create a simple Rust test fixture
fn create_rust_fixture(dir: &std::path::Path) {
    let rust_code = r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn multiply(x: i32, y: i32) -> i32 {
    x * y
}

pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn compute(&self) -> i32 {
        add(self.value, multiply(2, 3))
    }
}
"#;
    fs::write(dir.join("lib.rs"), rust_code).unwrap();
}

#[test]
fn test_graph_mode_always_enabled() {
    init_logging();
    log::info!("Testing that graph mode is always enabled (FR-2025-021)");

    let tmp_dir = TempDir::new().unwrap();
    create_rust_fixture(tmp_dir.path());

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Index with default settings (graph mode is always on)
    cmd.arg("index").arg(tmp_dir.path()).assert().success();

    // Verify query works with graph mode
    let path2 = sqry_bin();
    let mut query_cmd = Command::new(path2);
    query_cmd
        .arg("query")
        .arg("kind:function")
        .arg(tmp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("add"));

    log::info!("✓ Graph mode is always enabled");
}

#[test]
fn test_deprecated_env_var_is_ignored() {
    init_logging();
    log::info!("Testing that SQRY_USE_GRAPH does not break indexing");

    let tmp_dir = TempDir::new().unwrap();
    create_rust_fixture(tmp_dir.path());

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Set deprecated env var - should still work
    cmd.env("SQRY_USE_GRAPH", "1")
        .arg("index")
        .arg(tmp_dir.path())
        .assert()
        .success();

    log::info!("✓ Deprecated env var ignored (index still succeeds)");
}

#[test]
fn test_graph_mode_multi_language() {
    init_logging();
    log::info!("Testing graph mode with multiple languages");

    let tmp_dir = TempDir::new().unwrap();

    // Create Rust file
    create_rust_fixture(tmp_dir.path());

    // Create JavaScript file
    let js_code = r#"
class ShoppingCart {
    constructor() {
        this.items = [];
    }

    addItem(item) {
        this.items.push(item);
    }

    getTotal() {
        return this.items.reduce((sum, item) => sum + item.price, 0);
    }
}

export { ShoppingCart };
"#;
    fs::write(tmp_dir.path().join("cart.js"), js_code).unwrap();

    // Create Python file
    let py_code = r#"
def calculate_total(items):
    return sum(item['price'] for item in items)

class Cart:
    def __init__(self):
        self.items = []

    def add_item(self, item):
        self.items.append(item)
"#;
    fs::write(tmp_dir.path().join("cart.py"), py_code).unwrap();

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Index (graph mode is always on)
    cmd.arg("index").arg(tmp_dir.path()).assert().success();

    // Query each language
    let languages = vec![
        ("Rust", "add"),
        ("JavaScript", "ShoppingCart"),
        ("Python", "Cart"),
    ];

    for (lang, symbol) in languages {
        let path_lang = sqry_bin();
        let mut query_cmd = Command::new(path_lang);
        query_cmd
            .arg("query")
            .arg(format!("name:{symbol}"))
            .arg(tmp_dir.path())
            .assert()
            .success()
            .stdout(predicate::str::contains(symbol));
        log::info!("  ✓ {lang} symbols indexed correctly");
    }

    log::info!("✓ Multi-language graph mode indexing successful");
}

#[test]
fn test_graph_mode_update_command() {
    init_logging();
    log::info!("Testing graph mode with 'update' command");

    let tmp_dir = TempDir::new().unwrap();
    create_rust_fixture(tmp_dir.path());

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Initial index
    cmd.arg("index").arg(tmp_dir.path()).assert().success();

    let graph_storage = GraphStorage::new(tmp_dir.path());
    let manifest_path = graph_storage.manifest_path();
    assert!(
        manifest_path.exists(),
        "Graph manifest should exist after index"
    );
    let manifest_before =
        Manifest::load(manifest_path).expect("Failed to load graph manifest after index");

    // Add a new file
    let new_code = r#"
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}
"#;
    fs::write(tmp_dir.path().join("math.rs"), new_code).unwrap();

    // Update
    let path2 = sqry_bin();
    let mut update_cmd = Command::new(path2);
    update_cmd
        .arg("update")
        .arg(tmp_dir.path())
        .assert()
        .success();

    let manifest_after =
        Manifest::load(manifest_path).expect("Failed to load graph manifest after update");
    assert_ne!(
        manifest_before.snapshot_sha256, manifest_after.snapshot_sha256,
        "Graph snapshot checksum should change after update"
    );
    assert_ne!(
        manifest_before.built_at, manifest_after.built_at,
        "Graph manifest timestamp should change after update"
    );

    // Verify new symbol is indexed
    let path3 = sqry_bin();
    let mut query_cmd = Command::new(path3);
    query_cmd
        .arg("query")
        .arg("name:subtract")
        .arg(tmp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("subtract"));

    log::info!("✓ Graph mode update command successful");
}

#[test]
fn test_graph_mode_large_fixture() {
    init_logging();
    log::info!("Testing graph mode with larger fixture (20 files)");

    let tmp_dir = TempDir::new().unwrap();

    // Create 20 Rust files
    for i in 0..20 {
        let code = format!(
            r#"
pub fn func_{i}_a() -> i32 {{
    {i}
}}

pub fn func_{i}_b() -> i32 {{
    func_{i}_a() + 1
}}

pub struct Struct{i} {{
    pub value: i32,
}}
"#
        );
        fs::write(tmp_dir.path().join(format!("file_{i}.rs")), code).unwrap();
    }

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Index
    cmd.arg("index").arg(tmp_dir.path()).assert().success();

    // Verify all files were indexed
    let path2 = sqry_bin();
    let mut query_cmd = Command::new(path2);
    query_cmd
        .arg("query")
        .arg("kind:function")
        .arg(tmp_dir.path())
        .assert()
        .success();

    log::info!("✓ Large fixture (20 files) indexed successfully with graph mode");
}

#[test]
fn test_graph_mode_with_incremental() {
    init_logging();
    log::info!("Testing graph mode with incremental indexing");

    let tmp_dir = TempDir::new().unwrap();
    create_rust_fixture(tmp_dir.path());

    // Create config enabling incremental (no use_graph_mode - always on)
    let config_content = r#"
enable_incremental = true
"#;
    fs::write(tmp_dir.path().join(".sqry-config.toml"), config_content).unwrap();

    let path = sqry_bin();
    let mut cmd = Command::new(path);

    // Initial index
    cmd.current_dir(tmp_dir.path())
        .arg("index")
        .arg(".")
        .assert()
        .success();

    // Modify a file
    let modified_code = r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn new_function() -> String {
    "Hello".to_string()
}
"#;
    fs::write(tmp_dir.path().join("lib.rs"), modified_code).unwrap();

    // Update with incremental
    let path2 = sqry_bin();
    let mut update_cmd = Command::new(path2);
    update_cmd
        .current_dir(tmp_dir.path())
        .arg("update")
        .arg(".")
        .assert()
        .success();

    // Verify new function is indexed
    let path3 = sqry_bin();
    let mut query_cmd = Command::new(path3);
    query_cmd
        .current_dir(tmp_dir.path())
        .arg("query")
        .arg("name:new_function")
        .arg(".")
        .assert()
        .success()
        .stdout(predicate::str::contains("new_function"));

    log::info!("✓ Graph mode with incremental indexing successful");
}
