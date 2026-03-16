//! Integration tests for AST query system
//!
//! Tests the complete pipeline: symbol extraction → context extraction → query evaluation

use sqry_core::ast::{ContextExtractor, parse_query};
use sqry_core::plugin::PluginManager;
use sqry_core::test_support::verbosity;
use std::fs;
use std::sync::Once;
use tempfile::TempDir;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

#[test]
fn test_find_test_functions() {
    init_logging();
    log::info!("Testing AST query: find test functions by name pattern");

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r#"
fn test_addition() {
    assert_eq!(2 + 2, 4);
}

fn test_subtraction() {
    assert_eq!(5 - 3, 2);
}

fn helper_function() {
    println!("Not a test");
}

fn test_multiplication() {
    assert_eq!(3 * 4, 12);
}
"#,
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();
    println!("matches len: {}", matches.len());
    for m in &matches {
        println!(
            "match: name={} kind={:?} parent={:?}",
            m.name,
            m.context.kind,
            m.context.parent.as_ref().map(|p| p.kind)
        );
    }

    log::debug!("Extracted {} symbols from test file", matches.len());

    // Query for test functions (name contains "test")
    let query = parse_query("name~=test").unwrap();
    let test_funcs: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    assert_eq!(test_funcs.len(), 3);
    assert!(test_funcs.contains(&"test_addition"));
    assert!(test_funcs.contains(&"test_subtraction"));
    assert!(test_funcs.contains(&"test_multiplication"));
    assert!(!test_funcs.contains(&"helper_function"));

    log::info!(
        "✓ Query 'name~=test' matched {} functions: test_addition, test_subtraction, test_multiplication",
        test_funcs.len()
    );
}

#[test]
fn test_find_methods_in_classes() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r#"
struct Calculator {
    value: i32,
}

impl Calculator {
    fn new() -> Self {
        Self { value: 0 }
    }

    fn add(&mut self, n: i32) {
        self.value += n;
    }

    fn get_value(&self) -> i32 {
        self.value
    }
}

fn standalone_function() {
    println!("Not a method");
}
"#,
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for methods (kind:method) or functions inside impl blocks (parent:impl)
    let query = parse_query("kind:method OR parent:impl").unwrap();
    let methods: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    // Should find methods in impl block
    assert!(
        methods.len() >= 2,
        "Should find at least 2 methods, found: {methods:?}"
    );
    assert!(
        methods.contains(&"add") || methods.contains(&"new") || methods.contains(&"get_value"),
        "Should find methods from impl block"
    );
}

#[test]
fn test_find_deeply_nested_functions() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r#"
fn top_level() {
    println!("depth 0");
}

mod outer {
    pub struct Nested {
        value: i32,
    }

    impl Nested {
        pub fn deeply_nested_method(&self) {
            println!("depth >= 2");
        }
    }
}
"#,
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for depth > 1 (excludes top-level which has depth 1)
    let query = parse_query("depth:>1").unwrap();
    let nested: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| (m.name.as_str(), m.context.depth()))
        .collect();

    // Should not include top_level (depth 1)
    assert!(!nested.iter().any(|(name, _)| *name == "top_level"));
    // Should include nested items (depth 2+)
    assert!(!nested.is_empty());
}

#[test]
fn test_complex_query() {
    init_logging();
    log::info!("Testing complex AST query: kind AND name AND in (class scope)");

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.js");
    fs::write(
        &file_path,
        r#"
function topLevel() {
    console.log("top");
}

class TestClass {
    testMethod() {
        console.log("test method");
    }

    helperMethod() {
        console.log("helper");
    }
}

class ProductionClass {
    testMethod() {
        console.log("another test");
    }
}
"#,
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    log::debug!(
        "Extracted {} symbols from JavaScript test file",
        matches.len()
    );

    // Complex query: methods named "testMethod" in TestClass
    let query = parse_query("kind:method AND name~=test AND in:TestClass").unwrap();
    let results: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    // Should find testMethod in TestClass but not in ProductionClass
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "testMethod");

    log::info!(
        "✓ Complex query 'kind:method AND name~=test AND in:TestClass' matched 1 result: testMethod in TestClass only"
    );
}

#[test]
fn test_language_filtering() {
    let dir = TempDir::new().unwrap();

    // Create Rust file
    let rust_file = dir.path().join("test.rs");
    fs::write(&rust_file, "fn rust_function() {}").unwrap();

    // Create JS file
    let js_file = dir.path().join("test.js");
    fs::write(&js_file, "function jsFunction() {}").unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    manager.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);

    // Extract from both files
    let rust_matches = extractor.extract_from_file(&rust_file).unwrap();
    let js_matches = extractor.extract_from_file(&js_file).unwrap();

    // Query for Rust only
    let rust_query = parse_query("lang:rust").unwrap();
    assert!(rust_matches.iter().any(|m| rust_query.matches(m)));
    assert!(!js_matches.iter().any(|m| rust_query.matches(m)));

    // Query for JavaScript only
    let js_query = parse_query("lang:javascript").unwrap();
    assert!(!rust_matches.iter().any(|m| js_query.matches(m)));
    assert!(js_matches.iter().any(|m| js_query.matches(m)));
}

#[test]
fn test_parent_kind_filtering() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(
        &file_path,
        r"
def top_level_function():
    pass

class MyClass:
    def class_method(self):
        pass

    def another_method(self):
        pass
",
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for functions with parent:class
    let query = parse_query("parent:class").unwrap();
    let with_class_parent: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    // Should find class methods but not top-level function
    assert!(
        with_class_parent.contains(&"class_method")
            || with_class_parent.contains(&"another_method")
    );
    assert!(!with_class_parent.contains(&"top_level_function"));
}

#[test]
fn test_path_matching() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r"
struct Outer {
    value: i32,
}

impl Outer {
    fn method_in_outer(&self) {}
}

struct Other {
    value: i32,
}

impl Other {
    fn method_in_other(&self) {}
}
",
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for symbols with "Outer" in path
    let query = parse_query("path:Outer").unwrap();
    let in_outer: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.context.path())
        .collect();

    // Should find method_in_outer path but not method_in_other
    assert!(in_outer.iter().any(|p| p.contains("Outer")));
}

#[test]
fn test_not_operator() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r"
fn regular_function() {}
fn test_function() {}
fn another_test() {}
fn helper() {}
",
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for functions NOT containing "test"
    let query = parse_query("kind:function AND NOT name~=test").unwrap();
    let non_test: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    // Should find regular_function and helper, but not test functions
    assert!(non_test.contains(&"regular_function"));
    assert!(non_test.contains(&"helper"));
    assert!(!non_test.contains(&"test_function"));
    assert!(!non_test.contains(&"another_test"));
}

#[test]
fn test_or_operator() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(
        &file_path,
        r"
fn function_one() {}

struct StructOne {
    value: i32,
}

enum EnumOne {
    Variant,
}

const CONSTANT_ONE: i32 = 42;
",
    )
    .unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query for functions OR structs
    let query = parse_query("kind:function OR kind:struct").unwrap();
    let results: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    // Should find function and struct, but not enum or constant
    assert!(results.contains(&"function_one"));
    assert!(results.contains(&"StructOne"));
    assert!(!results.contains(&"EnumOne"));
    assert!(!results.contains(&"CONSTANT_ONE"));
}

#[test]
fn test_empty_query_results() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(&file_path, "fn some_function() {}").unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_file(&file_path).unwrap();

    // Query that should match nothing
    let query = parse_query("kind:class").unwrap();
    let results: Vec<_> = matches.iter().filter(|m| query.matches(m)).collect();

    assert_eq!(results.len(), 0);
}

#[test]
fn test_directory_extraction() {
    let dir = TempDir::new().unwrap();

    // Create multiple files
    fs::write(dir.path().join("file1.rs"), "fn func1() {}").unwrap();
    fs::write(dir.path().join("file2.rs"), "fn func2() {}").unwrap();
    fs::create_dir(dir.path().join("subdir")).unwrap();
    fs::write(dir.path().join("subdir/file3.rs"), "fn func3() {}").unwrap();

    let mut manager = PluginManager::new();
    manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    let extractor = ContextExtractor::with_plugin_manager(manager);
    let matches = extractor.extract_from_directory(dir.path()).unwrap();

    // Should find functions from all files
    let query = parse_query("kind:function").unwrap();
    let functions: Vec<_> = matches
        .iter()
        .filter(|m| query.matches(m))
        .map(|m| m.name.as_str())
        .collect();

    assert!(functions.contains(&"func1"));
    assert!(functions.contains(&"func2"));
    assert!(functions.contains(&"func3"));
}
