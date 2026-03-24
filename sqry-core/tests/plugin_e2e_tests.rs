//! End-to-end integration tests for the plugin system
//!
//! These tests validate the complete workflow from plugin registration
//! to unified graph builds using real plugins (`RustPlugin`).

use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::{LanguagePlugin, PluginManager};
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

use sqry_core::test_support::verbosity;
use std::sync::Once;
use std::{collections::HashMap, path::Path};

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Scenario 1: Full Rust workflow - register plugin and build graph
#[test]
fn test_full_rust_workflow() {
    init_logging();
    log::info!("Testing test_full_rust_workflow");

    // Create PluginManager and register RustPlugin
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    // Create a test Rust file
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    std::fs::write(
        &file_path,
        r#"
fn main() {
    println!("Hello, world!");
}

struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    fn distance(&self) -> f64 {
        ((self.x * self.x + self.y * self.y) as f64).sqrt()
    }
}

enum Color {
    Red,
    Green,
    Blue,
}

trait Drawable {
    fn draw(&self);
}

const PI: f64 = 3.14159;
static COUNTER: i32 = 0;
type Result<T> = std::result::Result<T, Error>;
"#,
    )
    .unwrap();

    // Get plugin and build graph
    let plugin = manager
        .plugin_for_extension("rs")
        .expect("Rust plugin should be registered");

    let content = std::fs::read_to_string(&file_path).unwrap();
    let staging = build_staging_graph(plugin, &file_path, &content);

    assert!(has_node(&staging, NodeKind::Function, "main"));
    assert!(has_node(&staging, NodeKind::Struct, "Point"));
    assert!(has_node(&staging, NodeKind::Method, "new"));
    assert!(has_node(&staging, NodeKind::Method, "distance"));
    assert!(has_node(&staging, NodeKind::Enum, "Color"));
    assert!(has_node(&staging, NodeKind::Interface, "Drawable"));
    assert!(has_node(&staging, NodeKind::Constant, "PI"));
    assert!(has_node(&staging, NodeKind::Constant, "COUNTER"));
    assert!(has_node(&staging, NodeKind::Type, "Result"));
}

/// Scenario 2: Multi-file extraction
#[test]
fn test_multi_file_extraction() {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    let dir = TempDir::new().unwrap();

    // Create multiple Rust files
    let file1 = dir.path().join("module1.rs");
    std::fs::write(&file1, "fn foo() {} fn bar() {}").unwrap();

    let file2 = dir.path().join("module2.rs");
    std::fs::write(&file2, "struct Baz {} enum Qux { A, B }").unwrap();

    let file3 = dir.path().join("module3.rs");
    std::fs::write(&file3, "trait Trait1 {} const C: i32 = 42;").unwrap();

    // Extract from all files
    let plugin = manager.plugin_for_extension("rs").unwrap();

    let content1 = std::fs::read_to_string(&file1).unwrap();
    let staging1 = build_staging_graph(plugin, &file1, &content1);

    let content2 = std::fs::read_to_string(&file2).unwrap();
    let staging2 = build_staging_graph(plugin, &file2, &content2);

    let content3 = std::fs::read_to_string(&file3).unwrap();
    let staging3 = build_staging_graph(plugin, &file3, &content3);

    assert!(has_node(&staging1, NodeKind::Function, "foo"));
    assert!(has_node(&staging1, NodeKind::Function, "bar"));
    assert!(has_node(&staging2, NodeKind::Struct, "Baz"));
    assert!(has_node(&staging2, NodeKind::Enum, "Qux"));
    assert!(has_node(&staging3, NodeKind::Interface, "Trait1"));
    assert!(has_node(&staging3, NodeKind::Constant, "C"));
}

/// Scenario 3: Error handling - invalid file extension
#[test]
fn test_error_invalid_extension() {
    let manager = PluginManager::new();

    // Try to get plugin for unsupported extension
    let plugin = manager.plugin_for_extension("txt");

    assert!(plugin.is_none(), "Should not find plugin for .txt files");
}

/// Scenario 4: Error handling - malformed Rust syntax
#[test]
fn test_error_malformed_syntax() {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("malformed.rs");
    std::fs::write(&file_path, "fn main() { // Missing closing brace").unwrap();

    let plugin = manager.plugin_for_extension("rs").unwrap();
    let content = std::fs::read_to_string(&file_path).unwrap();

    let result = plugin.parse_ast(content.as_bytes());
    assert!(result.is_ok(), "Should handle malformed syntax gracefully");

    let staging = build_staging_graph(plugin, &file_path, &content);
    assert!(count_nodes(&staging) <= 1);
}

/// Scenario 5: Error handling - empty file
#[test]
fn test_error_empty_file() {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("empty.rs");
    std::fs::write(&file_path, "").unwrap();

    let plugin = manager.plugin_for_extension("rs").unwrap();
    let content = std::fs::read_to_string(&file_path).unwrap();
    let staging = build_staging_graph(plugin, &file_path, &content);

    assert_eq!(count_nodes(&staging), 0, "Empty file should stage no nodes");
}

/// Scenario 6: Plugin lookup patterns
#[test]
fn test_plugin_lookup_patterns() {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    // Lookup by extension
    let plugin_ext = manager.plugin_for_extension("rs");
    assert!(plugin_ext.is_some(), "Should find plugin by extension");
    assert_eq!(plugin_ext.unwrap().metadata().id, "rust");

    // Lookup by language ID
    let plugin_id = manager.plugin_by_id("rust");
    assert!(plugin_id.is_some(), "Should find plugin by ID");
    assert_eq!(plugin_id.unwrap().metadata().name, "Rust");

    // Lookup non-existent extension
    assert!(manager.plugin_for_extension("xyz").is_none());

    // Lookup non-existent ID
    assert!(manager.plugin_by_id("nonexistent").is_none());

    // Enumerate all plugins
    let all_plugins = manager.plugins();
    assert_eq!(all_plugins.len(), 1);
    assert_eq!(all_plugins[0].metadata().id, "rust");
}

/// Scenario 7: Plugin metadata validation
#[test]
fn test_plugin_metadata_validation() {
    let plugin = RustPlugin::default();
    let metadata = plugin.metadata();

    // Verify all metadata fields are populated
    assert!(!metadata.id.is_empty(), "ID should not be empty");
    assert!(!metadata.name.is_empty(), "Name should not be empty");
    assert!(!metadata.version.is_empty(), "Version should not be empty");
    assert!(!metadata.author.is_empty(), "Author should not be empty");
    assert!(
        !metadata.description.is_empty(),
        "Description should not be empty"
    );
    assert!(
        !metadata.tree_sitter_version.is_empty(),
        "Tree-sitter version should not be empty"
    );

    // Verify ID follows conventions
    assert!(
        metadata.id.chars().all(|c| c.is_lowercase() || c == '-'),
        "ID should be lowercase with hyphens"
    );

    // Verify extensions are populated
    let extensions = plugin.extensions();
    assert!(!extensions.is_empty(), "Extensions should not be empty");
    assert!(
        extensions.iter().all(|ext| !ext.is_empty()),
        "No empty extensions"
    );
    assert!(
        extensions.iter().all(|ext| !ext.starts_with('.')),
        "Extensions should not start with dot"
    );
}

/// Scenario 8: Edge cases - unicode, whitespace, special characters
#[test]
fn test_edge_cases() {
    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("edge_cases.rs");
    std::fs::write(
        &file_path,
        r"
// Unicode identifiers (if Rust supports them in the future)
fn hello_世界() {}

// Lots of whitespace
fn   spaced_out  (  )   {   }

// Comment only
// This is a comment

// Empty impl block
impl Foo {}

// Multiple items on one line
fn a() {} fn b() {} fn c() {}
",
    )
    .unwrap();

    let plugin = manager.plugin_for_extension("rs").unwrap();
    let content = std::fs::read_to_string(&file_path).unwrap();
    let staging = build_staging_graph(plugin, &file_path, &content);

    assert!(count_nodes(&staging) >= 4, "Should stage function nodes");
}

/// Integration test: Concurrent access (thread-safety)
#[test]
fn test_concurrent_access() {
    use std::sync::Arc;
    use std::thread;

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(RustPlugin::default()));
    let manager = Arc::new(manager);

    let dir = TempDir::new().unwrap();
    let file_path = Arc::new(dir.path().join("concurrent.rs"));
    std::fs::write(
        file_path.as_ref(),
        "fn test1() {}\nfn test2() {}\nfn test3() {}",
    )
    .unwrap();

    // Spawn multiple threads that all use the same PluginManager
    let mut handles = vec![];
    for i in 0..10 {
        let manager_clone = Arc::clone(&manager);
        let file_clone = Arc::clone(&file_path);

        let handle = thread::spawn(move || {
            let plugin = manager_clone.plugin_for_extension("rs").unwrap();
            let content = std::fs::read_to_string(file_clone.as_ref()).unwrap();
            let staging = build_staging_graph(plugin, file_clone.as_ref(), &content);

            let function_count = count_nodes_by_kind(&staging, NodeKind::Function);
            assert_eq!(function_count, 3, "Thread {i} got wrong function count");
            function_count
        });

        handles.push(handle);
    }

    // Wait for all threads and verify they all succeeded
    for handle in handles {
        let function_count = handle.join().expect("Thread panicked");
        assert_eq!(function_count, 3);
    }
}

fn build_staging_graph(
    plugin: &dyn LanguagePlugin,
    file_path: &Path,
    content: &str,
) -> StagingGraph {
    let tree = plugin
        .parse_ast(content.as_bytes())
        .expect("Failed to parse AST");
    let builder = plugin
        .graph_builder()
        .expect("Rust plugin should provide GraphBuilder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Graph build failed");
    staging
}

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn count_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { .. }))
        .count()
}

fn count_nodes_by_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                entry.kind == kind
            } else {
                false
            }
        })
        .count()
}

fn has_node(staging: &StagingGraph, kind: NodeKind, name: &str) -> bool {
    let strings = build_string_lookup(staging);

    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            if entry.kind != kind {
                return false;
            }
            let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
            return strings.get(&name_idx).is_some_and(|node_name| {
                node_name == name || short_name(node_name).as_str() == name
            });
        }
        false
    })
}

fn short_name(name: &str) -> String {
    if let Some(pos) = name.rfind("::") {
        return name[pos + 2..].to_string();
    }
    if let Some(pos) = name.rfind('.') {
        return name[pos + 1..].to_string();
    }
    name.to_string()
}
