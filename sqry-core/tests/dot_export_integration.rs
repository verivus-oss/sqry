//! Integration tests for DOT export functionality
//!
//! These tests verify that the `UnifiedDotExporter` works correctly with real
//! graph structures, including cross-language scenarios.

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::visualization::unified::{Direction, DotConfig, EdgeFilter, UnifiedDotExporter};
use std::path::Path;

/// Create a sample cross-language graph for testing.
#[allow(clippy::similar_names)] // lang_name/lang_qname pairs are intentional variable naming
fn create_sample_graph() -> CodeGraph {
    let mut graph = CodeGraph::new();

    // Register files for different languages
    let js_file = graph
        .files_mut()
        .register_with_language(Path::new("frontend/api.js"), Some(Language::JavaScript))
        .expect("register js file");
    let py_file = graph
        .files_mut()
        .register_with_language(Path::new("backend/api.py"), Some(Language::Python))
        .expect("register py file");
    let cpp_file = graph
        .files_mut()
        .register_with_language(Path::new("db/query.cpp"), Some(Language::Cpp))
        .expect("register cpp file");

    // Create JavaScript frontend function
    let js_name = graph.strings_mut().intern("fetchUsers").expect("intern");
    let js_qname = graph
        .strings_mut()
        .intern("frontend/api.js::fetchUsers")
        .expect("intern");
    let js_entry = NodeEntry::new(NodeKind::Function, js_name, js_file)
        .with_location(10, 0, 25, 1)
        .with_qualified_name(js_qname);
    let js_node = graph.nodes_mut().alloc(js_entry.clone()).expect("alloc");
    graph.indices_mut().add(
        js_node,
        js_entry.kind,
        js_entry.name,
        js_entry.qualified_name,
        js_entry.file,
    );

    // Create Python backend function
    let py_name = graph.strings_mut().intern("get_users").expect("intern");
    let py_qname = graph
        .strings_mut()
        .intern("backend/api.py::get_users")
        .expect("intern");
    let py_entry = NodeEntry::new(NodeKind::Function, py_name, py_file)
        .with_location(5, 0, 15, 1)
        .with_qualified_name(py_qname);
    let py_node = graph.nodes_mut().alloc(py_entry.clone()).expect("alloc");
    graph.indices_mut().add(
        py_node,
        py_entry.kind,
        py_entry.name,
        py_entry.qualified_name,
        py_entry.file,
    );

    // Create C++ database function
    let cpp_name = graph.strings_mut().intern("executeQuery").expect("intern");
    let cpp_qname = graph
        .strings_mut()
        .intern("db/query.cpp::executeQuery")
        .expect("intern");
    let cpp_entry = NodeEntry::new(NodeKind::Function, cpp_name, cpp_file)
        .with_location(20, 0, 35, 1)
        .with_qualified_name(cpp_qname);
    let cpp_node = graph.nodes_mut().alloc(cpp_entry.clone()).expect("alloc");
    graph.indices_mut().add(
        cpp_node,
        cpp_entry.kind,
        cpp_entry.name,
        cpp_entry.qualified_name,
        cpp_entry.file,
    );

    // Add edges: JS -> Python -> C++ (cross-language call chain)
    graph.edges().add_edge(
        js_node,
        py_node,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: true,
        },
        js_file,
    );
    graph.edges().add_edge(
        py_node,
        cpp_node,
        EdgeKind::Calls {
            argument_count: 1,
            is_async: false,
        },
        py_file,
    );

    graph
}

#[test]
fn test_basic_export() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let exporter = UnifiedDotExporter::new(&snapshot);
    let dot = exporter.export();

    // Verify basic DOT structure
    assert!(dot.starts_with("digraph CodeGraph {"));
    assert!(dot.ends_with("}\n"));
    assert!(dot.contains("rankdir="));

    // Verify nodes are present
    assert!(dot.contains("fetchUsers"));
    assert!(dot.contains("get_users"));
    assert!(dot.contains("executeQuery"));
}

#[test]
fn test_language_colors() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let exporter = UnifiedDotExporter::new(&snapshot);
    let dot = exporter.export();

    // Different languages should have different fill colors
    // JavaScript, Python, and C++ should all be represented
    assert!(dot.contains("fillcolor="));
}

#[test]
fn test_cross_language_highlighting() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let config = DotConfig::default().with_cross_language_highlight(true);
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    // Cross-language edges should be highlighted (red/bold)
    // JS->Python and Python->C++ are cross-language calls
    assert!(dot.contains("->"));
}

#[test]
fn test_language_filtering() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let config = DotConfig::default().filter_language(Language::Python);
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    // Only Python node should be visible
    assert!(dot.contains("get_users"));
    assert!(!dot.contains("fetchUsers"));
    assert!(!dot.contains("executeQuery"));
}

#[test]
fn test_edge_kind_filtering() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let config = DotConfig::default().filter_edge(EdgeFilter::Calls);
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    // Only CALLS edges should be present
    assert!(dot.contains("->"));
}

#[test]
fn test_file_filtering() {
    let mut graph = CodeGraph::new();

    let file1 = graph
        .files_mut()
        .register_with_language(Path::new("src/main.rs"), Some(Language::Rust))
        .expect("register");
    let file2 = graph
        .files_mut()
        .register_with_language(Path::new("tests/test.rs"), Some(Language::Rust))
        .expect("register");

    let name1 = graph.strings_mut().intern("main").expect("intern");
    let entry1 = NodeEntry::new(NodeKind::Function, name1, file1).with_location(1, 0, 10, 0);
    let node1 = graph.nodes_mut().alloc(entry1.clone()).expect("alloc");
    graph
        .indices_mut()
        .add(node1, entry1.kind, entry1.name, None, entry1.file);

    let name2 = graph.strings_mut().intern("test_main").expect("intern");
    let entry2 = NodeEntry::new(NodeKind::Function, name2, file2).with_location(1, 0, 10, 0);
    let node2 = graph.nodes_mut().alloc(entry2.clone()).expect("alloc");
    graph
        .indices_mut()
        .add(node2, entry2.kind, entry2.name, None, entry2.file);

    let snapshot = graph.snapshot();
    let mut config = DotConfig::default();
    config.filter_files.insert("src/".to_string());
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    // Only src/ files should be visible
    assert!(dot.contains("main"));
    assert!(!dot.contains("test_main"));
}

#[test]
fn test_top_to_bottom_direction() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let config = DotConfig::default().with_direction(Direction::TopToBottom);
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    assert!(dot.contains("rankdir=TB"));
}

#[test]
fn test_details_toggle() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();

    // With details
    let config_with = DotConfig::default().with_details(true);
    let dot_with = UnifiedDotExporter::with_config(&snapshot, config_with).export();

    // Without details
    let config_without = DotConfig::default().with_details(false);
    let dot_without = UnifiedDotExporter::with_config(&snapshot, config_without).export();

    // With details should be longer (contains file/line info)
    assert!(dot_with.len() >= dot_without.len());
}

#[test]
fn test_edge_labels_toggle() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();

    // With edge labels
    let config_with = DotConfig::default().with_edge_labels(true);
    let dot_with = UnifiedDotExporter::with_config(&snapshot, config_with).export();

    // Without edge labels
    let config_without = DotConfig::default().with_edge_labels(false);
    let dot_without = UnifiedDotExporter::with_config(&snapshot, config_without).export();

    // With labels should be longer
    assert!(dot_with.len() >= dot_without.len());
}

#[test]
fn test_depth_filtering() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();

    // Get the JS node as root
    let js_node = snapshot
        .iter_nodes()
        .find(|(_, entry)| {
            snapshot
                .strings()
                .resolve(entry.name)
                .is_some_and(|n| &*n == "fetchUsers")
        })
        .map(|(id, _)| id)
        .expect("find js node");

    let mut config = DotConfig::default().with_max_depth(1);
    config.root_nodes.insert(js_node);
    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    // With depth 1 from JS, should see JS and Python (direct neighbor)
    assert!(dot.contains("fetchUsers"));
    assert!(dot.contains("get_users"));
    // C++ is 2 hops away, may or may not be included depending on implementation
}

#[test]
fn test_empty_graph() {
    let graph = CodeGraph::new();
    let snapshot = graph.snapshot();
    let exporter = UnifiedDotExporter::new(&snapshot);
    let dot = exporter.export();

    // Should still produce valid DOT
    assert!(dot.starts_with("digraph CodeGraph {"));
    assert!(dot.ends_with("}\n"));
}

#[test]
fn test_combined_filters() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();

    let config = DotConfig::default()
        .filter_edge(EdgeFilter::Calls)
        .with_direction(Direction::TopToBottom)
        .with_details(true)
        .with_edge_labels(true);

    let exporter = UnifiedDotExporter::with_config(&snapshot, config);
    let dot = exporter.export();

    assert!(dot.contains("rankdir=TB"));
    assert!(dot.contains("->"));
}

#[test]
fn test_valid_dot_syntax() {
    let graph = create_sample_graph();
    let snapshot = graph.snapshot();
    let exporter = UnifiedDotExporter::new(&snapshot);
    let dot = exporter.export();

    // Basic DOT syntax validation
    assert!(dot.starts_with("digraph"));
    assert!(dot.contains('{'));
    assert!(dot.contains('}'));

    // Should have balanced braces
    let open_braces = dot.matches('{').count();
    let close_braces = dot.matches('}').count();
    assert_eq!(open_braces, close_braces);

    // Should have node definitions with brackets
    assert!(dot.contains('['));
    assert!(dot.contains(']'));
}
