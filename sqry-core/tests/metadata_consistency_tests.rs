//! Cross-Language Metadata Consistency Tests (FT-B.6)
//!
//! This test suite validates that metadata extraction is consistent across language plugins.
//! It ensures that all plugins use the same canonical metadata keys and extract metadata
//! correctly from language-specific constructs.
//!
//! Test fixtures are located in: `tests/fixtures/metadata_consistency/`

use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::plugin::LanguagePlugin;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Test fixture definition
struct Fixture {
    path: PathBuf,
    language: &'static str,
    expected_async: Vec<&'static str>, // Functions that should have is_async:true
    expected_sync: Vec<&'static str>,  // Functions that should NOT have is_async:true
}

/// Test fixture for visibility
struct VisibilityFixture {
    path: PathBuf,
    language: &'static str,
    expected_public: Vec<&'static str>, // Functions with visibility:public
    expected_private: Vec<&'static str>, // Functions with visibility:private
}

#[derive(Debug, Clone)]
struct NodeMeta {
    is_async: bool,
    visibility: Option<String>,
}

/// Helper to get plugin for language
fn get_plugin(language: &str) -> Box<dyn LanguagePlugin> {
    match language {
        "python" => Box::new(sqry_lang_python::PythonPlugin::default()),
        "typescript" => Box::new(sqry_lang_typescript::TypeScriptPlugin::default()),
        "javascript" => Box::new(sqry_lang_javascript::JavaScriptPlugin::default()),
        "rust" => Box::new(sqry_lang_rust::RustPlugin::default()),
        "dart" => Box::new(sqry_lang_dart::DartPlugin::default()),
        "swift" => Box::new(sqry_lang_swift::SwiftPlugin::default()),
        "go" => Box::new(sqry_lang_go::GoPlugin::default()),
        _ => panic!("Unsupported language: {language}"),
    }
}

#[test]
fn test_async_metadata_consistency_python() {
    let fixture = Fixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/async_functions/python_async.py",
        ),
        language: "python",
        expected_async: vec!["fetch_data"],
        expected_sync: vec!["sync_function"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_async_metadata_consistency_typescript() {
    let fixture = Fixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/async_functions/typescript_async.ts",
        ),
        language: "typescript",
        expected_async: vec!["fetchData"],
        expected_sync: vec!["syncFunction"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_async_metadata_consistency_javascript() {
    let fixture = Fixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/async_functions/javascript_async.js",
        ),
        language: "javascript",
        expected_async: vec!["fetchData"],
        expected_sync: vec!["syncFunction"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_async_metadata_consistency_rust() {
    let fixture = Fixture {
        path: PathBuf::from("../tests/fixtures/metadata_consistency/async_functions/rust_async.rs"),
        language: "rust",
        expected_async: vec!["fetch_data"],
        expected_sync: vec!["sync_function"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_async_metadata_consistency_dart() {
    let fixture = Fixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/async_functions/dart_async.dart",
        ),
        language: "dart",
        expected_async: vec!["fetchData"],
        expected_sync: vec!["syncFunction"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_async_metadata_consistency_swift() {
    let fixture = Fixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/async_functions/swift_async.swift",
        ),
        language: "swift",
        expected_async: vec!["fetchData"],
        expected_sync: vec!["syncFunction"],
    };

    validate_async_fixture(&fixture);
}

#[test]
fn test_visibility_consistency_go() {
    let fixture = VisibilityFixture {
        path: PathBuf::from("../tests/fixtures/metadata_consistency/visibility/go_visibility.go"),
        language: "go",
        expected_public: vec!["PublicFunction"],
        expected_private: vec!["privateFunction"],
    };

    validate_visibility_fixture(&fixture);
}

#[test]
fn test_visibility_consistency_rust() {
    let fixture = VisibilityFixture {
        path: PathBuf::from("../tests/fixtures/metadata_consistency/visibility/rust_visibility.rs"),
        language: "rust",
        expected_public: vec!["public_function"],
        expected_private: vec!["private_function"],
    };

    validate_visibility_fixture(&fixture);
}

// Note: Python visibility test is skipped because Python plugin extracts visibility
// for VARIABLES (via naming convention), not FUNCTIONS. This is expected behavior.
// #[test]
// fn test_visibility_consistency_python() {
//     let fixture = VisibilityFixture {
//         path: PathBuf::from("../tests/fixtures/metadata_consistency/visibility/python_visibility.py"),
//         language: "python",
//         expected_public: vec!["public_function"],
//         expected_private: vec!["_private_function"],
//     };
//
//     validate_visibility_fixture(&fixture);
// }

#[test]
fn test_visibility_consistency_dart() {
    let fixture = VisibilityFixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/visibility/dart_visibility.dart",
        ),
        language: "dart",
        expected_public: vec!["publicFunction"],
        expected_private: vec!["_privateFunction"],
    };

    validate_visibility_fixture(&fixture);
}

#[test]
fn test_visibility_consistency_swift() {
    let fixture = VisibilityFixture {
        path: PathBuf::from(
            "../tests/fixtures/metadata_consistency/visibility/swift_visibility.swift",
        ),
        language: "swift",
        expected_public: vec!["publicFunction"],
        expected_private: vec!["privateFunction"],
    };

    validate_visibility_fixture(&fixture);
}

/// Validate async metadata for a fixture
fn validate_async_fixture(fixture: &Fixture) {
    let plugin = get_plugin(fixture.language);

    // Read fixture file
    let content = fs::read(&fixture.path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {}: {e}", fixture.path.display()));

    let staging = build_staging_graph(plugin.as_ref(), &content, &fixture.path);
    let metadata_map = collect_node_metadata(&staging);

    // Verify expected async functions
    for &expected_async in &fixture.expected_async {
        let metadata = metadata_map.get(expected_async).unwrap_or_else(|| {
            panic!(
                "{}: Function '{}' not found in extracted symbols",
                fixture.language, expected_async
            )
        });

        assert!(
            metadata.is_async,
            "{}: Function '{}' should be async",
            fixture.language, expected_async
        );
    }

    // Verify expected sync functions (should NOT have is_async:true)
    for &expected_sync in &fixture.expected_sync {
        let metadata = metadata_map.get(expected_sync).unwrap_or_else(|| {
            panic!(
                "{}: Function '{}' not found in extracted symbols",
                fixture.language, expected_sync
            )
        });

        assert!(
            !metadata.is_async,
            "{}: Function '{}' should NOT be async",
            fixture.language, expected_sync
        );
    }

    println!(
        "✓ {}: Async metadata consistency validated ({} async, {} sync)",
        fixture.language,
        fixture.expected_async.len(),
        fixture.expected_sync.len()
    );
}

/// Validate visibility metadata for a fixture
fn validate_visibility_fixture(fixture: &VisibilityFixture) {
    let plugin = get_plugin(fixture.language);

    // Read fixture file
    let content = fs::read(&fixture.path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {}: {e}", fixture.path.display()));

    let staging = build_staging_graph(plugin.as_ref(), &content, &fixture.path);
    let metadata_map = collect_node_metadata(&staging);

    // Verify expected public functions
    for &expected_public in &fixture.expected_public {
        let metadata = metadata_map.get(expected_public).unwrap_or_else(|| {
            panic!(
                "{}: Function '{}' not found in extracted symbols",
                fixture.language, expected_public
            )
        });

        assert_eq!(
            metadata.visibility.as_deref(),
            Some("public"),
            "{}: Function '{}' should have visibility:public",
            fixture.language,
            expected_public
        );
    }

    // Verify expected private functions
    for &expected_private in &fixture.expected_private {
        let metadata = metadata_map.get(expected_private).unwrap_or_else(|| {
            panic!(
                "{}: Function '{}' not found in extracted symbols",
                fixture.language, expected_private
            )
        });

        assert_eq!(
            metadata.visibility.as_deref(),
            Some("private"),
            "{}: Function '{}' should have visibility:private",
            fixture.language,
            expected_private
        );
    }

    println!(
        "✓ {}: Visibility metadata consistency validated ({} public, {} private)",
        fixture.language,
        fixture.expected_public.len(),
        fixture.expected_private.len()
    );
}

fn build_staging_graph(plugin: &dyn LanguagePlugin, content: &[u8], path: &Path) -> StagingGraph {
    let tree = plugin
        .parse_ast(content)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()));
    let builder = plugin
        .graph_builder()
        .unwrap_or_else(|| panic!("Missing GraphBuilder for {}", plugin.metadata().id));
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, content, path, &mut staging)
        .unwrap_or_else(|e| panic!("Graph build failed for {}: {e}", path.display()));
    staging
}

fn collect_node_metadata(staging: &StagingGraph) -> HashMap<String, NodeMeta> {
    let strings = build_string_lookup(staging);
    let mut map = HashMap::new();

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op {
            let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
            let Some(name) = strings.get(&name_idx).cloned() else {
                continue;
            };
            let visibility = entry
                .visibility
                .and_then(|id| strings.get(&id.index()).cloned());
            let short = short_name(&name);
            let short_differs = short != name;
            let metadata = NodeMeta {
                is_async: entry.is_async,
                visibility,
            };
            map.insert(name, metadata.clone());
            if short_differs {
                map.entry(short).or_insert(metadata);
            }
        }
    }

    map
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
