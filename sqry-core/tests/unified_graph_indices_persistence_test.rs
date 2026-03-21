//! Integration tests for unified graph indices persistence.
//!
//! These tests verify that `AuxiliaryIndices` are correctly:
//! 1. Populated during graph building
//! 2. Serialized when saving the graph
//! 3. Deserialized when loading the graph
//! 4. Queryable after loading
//!
//! This addresses a regression where indices were not being populated during
//! the build phase, causing queries to return empty results despite the
//! underlying node data being present.
//!
//! # Usage
//!
//! ```bash
//! cargo test -p sqry-core --test unified_graph_indices_persistence_test
//! ```

use sqry_core::graph::CodeGraph;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::test_support::verbosity;
use std::path::Path;
use std::sync::Once;
use tempfile::TempDir;

static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Create a graph with various node types for testing.
fn create_test_graph() -> CodeGraph {
    let mut graph = CodeGraph::new();

    // Register test files
    let file1_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/lib.rs"), Some(Language::Rust))
        .expect("Failed to register file1");
    let file2_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/utils.rs"), Some(Language::Rust))
        .expect("Failed to register file2");

    // Create node entries for various kinds
    let nodes_data = vec![
        // Functions
        (
            "helper",
            "test::helper",
            NodeKind::Function,
            file1_id,
            1,
            10,
        ),
        ("fetch", "test::fetch", NodeKind::Function, file1_id, 12, 20),
        (
            "add",
            "test::utils::add",
            NodeKind::Function,
            file2_id,
            1,
            5,
        ),
        // Structs
        ("User", "test::User", NodeKind::Struct, file1_id, 22, 30),
        (
            "Calculator",
            "test::utils::Calculator",
            NodeKind::Struct,
            file2_id,
            7,
            15,
        ),
        // Methods
        ("new", "test::User::new", NodeKind::Method, file1_id, 32, 35),
        (
            "get_name",
            "test::User::get_name",
            NodeKind::Method,
            file1_id,
            37,
            40,
        ),
        // Constants
        (
            "MAX_SIZE",
            "test::MAX_SIZE",
            NodeKind::Constant,
            file1_id,
            42,
            42,
        ),
        // Module
        ("utils", "test::utils", NodeKind::Module, file2_id, 0, 20),
    ];

    for (name, qname, kind, file_id, start_line, end_line) in nodes_data {
        let name_id = graph
            .strings_mut()
            .intern(name)
            .expect("Failed to intern name");
        let qname_id = graph
            .strings_mut()
            .intern(qname)
            .expect("Failed to intern qname");

        let entry = NodeEntry::new(kind, name_id, file_id)
            .with_location(start_line, 0, end_line, 0)
            .with_qualified_name(qname_id);

        // Add node to arena
        let node_id = graph
            .nodes_mut()
            .alloc(entry.clone())
            .expect("Failed to alloc node");

        // CRITICAL: Also add to indices - this is what we're testing was missing
        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );
    }

    graph
}

/// Create a graph with nodes that have visibility metadata.
fn create_graph_with_visibility() -> CodeGraph {
    let mut graph = CodeGraph::new();

    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/lib.rs"), Some(Language::Rust))
        .expect("Failed to register file");

    // Create a public function
    let pub_name_id = graph
        .strings_mut()
        .intern("public_func")
        .expect("Failed to intern name");
    let pub_vis_id = graph
        .strings_mut()
        .intern("pub")
        .expect("Failed to intern visibility");

    let pub_entry = NodeEntry::new(NodeKind::Function, pub_name_id, file_id)
        .with_location(1, 0, 5, 0)
        .with_visibility(pub_vis_id);

    let pub_node_id = graph
        .nodes_mut()
        .alloc(pub_entry.clone())
        .expect("Failed to alloc pub node");
    graph.indices_mut().add(
        pub_node_id,
        pub_entry.kind,
        pub_entry.name,
        pub_entry.qualified_name,
        pub_entry.file,
    );

    // Create a private function (no visibility)
    let priv_name_id = graph
        .strings_mut()
        .intern("private_func")
        .expect("Failed to intern name");

    let priv_entry =
        NodeEntry::new(NodeKind::Function, priv_name_id, file_id).with_location(7, 0, 10, 0);

    let priv_node_id = graph
        .nodes_mut()
        .alloc(priv_entry.clone())
        .expect("Failed to alloc priv node");
    graph.indices_mut().add(
        priv_node_id,
        priv_entry.kind,
        priv_entry.name,
        priv_entry.qualified_name,
        priv_entry.file,
    );

    graph
}

/// Verify indices are correctly populated in a graph.
fn verify_indices(graph: &CodeGraph, expected_counts: &[(NodeKind, usize)], context: &str) {
    let indices = graph.indices();

    // Verify total node count in indices
    let total_in_indices = indices.len();
    let expected_total: usize = expected_counts.iter().map(|(_, c)| c).sum();
    assert_eq!(
        total_in_indices, expected_total,
        "[{context}] Expected {expected_total} nodes in indices, got {total_in_indices}"
    );

    // Verify counts by kind
    for (kind, expected_count) in expected_counts {
        let actual = indices.by_kind(*kind).len();
        assert_eq!(
            actual, *expected_count,
            "[{context}] Expected {expected_count} {kind:?} nodes in indices, got {actual}"
        );
    }
}

fn find_node_id_by_name(
    graph: &CodeGraph,
    name: &str,
) -> Option<sqry_core::graph::unified::node::NodeId> {
    let strings = graph.strings();
    let name_id = strings.get(name)?;
    graph.indices().by_name(name_id).first().copied()
}

fn assert_node_kind(graph: &CodeGraph, name: &str, expected_kind: NodeKind) {
    let node_id = find_node_id_by_name(graph, name).unwrap_or_else(|| {
        panic!("Expected node named '{name}' to exist in indices");
    });
    let entry = graph
        .nodes()
        .get(node_id)
        .unwrap_or_else(|| panic!("Expected node entry for '{name}'"));
    assert_eq!(
        entry.kind, expected_kind,
        "Expected '{name}' to have kind {expected_kind:?}"
    );
}

fn assert_node_visibility(graph: &CodeGraph, name: &str, expected_visibility: Option<&str>) {
    let node_id = find_node_id_by_name(graph, name).unwrap_or_else(|| {
        panic!("Expected node named '{name}' to exist in indices");
    });
    let entry = graph
        .nodes()
        .get(node_id)
        .unwrap_or_else(|| panic!("Expected node entry for '{name}'"));
    let visibility = entry
        .visibility
        .and_then(|id| graph.strings().resolve(id))
        .map(|value| value.to_string());

    assert_eq!(
        visibility.as_deref(),
        expected_visibility,
        "Expected '{name}' to have visibility {:?}",
        expected_visibility
    );
}

#[test]
fn indices_populated_after_building() {
    init_logging();
    log::info!("Testing that indices are populated during graph building");

    let graph = create_test_graph();

    // Verify nodes are in the arena
    assert_eq!(graph.nodes().len(), 9, "Expected 9 nodes in arena");

    // Verify indices are populated
    verify_indices(
        &graph,
        &[
            (NodeKind::Function, 3),
            (NodeKind::Struct, 2),
            (NodeKind::Method, 2),
            (NodeKind::Constant, 1),
            (NodeKind::Module, 1),
        ],
        "after building",
    );

    log::info!("Indices correctly populated during building");
}

#[test]
fn indices_persist_through_save_load_cycle() {
    init_logging();
    log::info!("Testing indices persistence through save/load cycle");

    let graph = create_test_graph();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("snapshot.sqry");

    // Verify indices before save
    verify_indices(
        &graph,
        &[
            (NodeKind::Function, 3),
            (NodeKind::Struct, 2),
            (NodeKind::Method, 2),
            (NodeKind::Constant, 1),
            (NodeKind::Module, 1),
        ],
        "before save",
    );

    // Save graph
    save_to_path(&graph, &snapshot_path).expect("Failed to save graph");
    log::info!("Graph saved to {:?}", snapshot_path);

    // Load graph
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load graph");
    log::info!("Graph loaded from {:?}", snapshot_path);

    // Verify nodes are still in the arena
    assert_eq!(
        loaded_graph.nodes().len(),
        9,
        "Expected 9 nodes in loaded arena"
    );

    // Verify indices are still populated after loading
    verify_indices(
        &loaded_graph,
        &[
            (NodeKind::Function, 3),
            (NodeKind::Struct, 2),
            (NodeKind::Method, 2),
            (NodeKind::Constant, 1),
            (NodeKind::Module, 1),
        ],
        "after load",
    );

    log::info!("Indices correctly persisted through save/load cycle");
}

#[test]
fn indices_lookup_works_after_loading() {
    init_logging();
    log::info!("Testing that indices and lookups work with loaded graph");

    let graph = create_test_graph();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("snapshot.sqry");

    // Save and reload
    save_to_path(&graph, &snapshot_path).expect("Failed to save graph");
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load graph");

    // Verify indices are populated and by-name lookup works
    let indices = loaded_graph.indices();
    assert_eq!(indices.len(), 9, "Expected 9 nodes in indices after load");

    assert_node_kind(&loaded_graph, "helper", NodeKind::Function);
    assert_node_kind(&loaded_graph, "fetch", NodeKind::Function);
    assert_node_kind(&loaded_graph, "add", NodeKind::Function);
    assert_node_kind(&loaded_graph, "User", NodeKind::Struct);
    assert_node_kind(&loaded_graph, "Calculator", NodeKind::Struct);

    log::info!("Indices correctly resolve nodes by name after load");
}

#[test]
fn indices_iter_kinds_works_after_loading() {
    init_logging();
    log::info!("Testing that indices.iter_kinds() works after loading");

    let graph = create_test_graph();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("snapshot.sqry");

    // Save and reload
    save_to_path(&graph, &snapshot_path).expect("Failed to save graph");
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load graph");

    // Verify iter_kinds returns all kinds
    let indices = loaded_graph.indices();
    let kinds: Vec<(NodeKind, usize)> = indices.iter_kinds().collect();

    // Should have 5 different kinds
    assert!(
        kinds.len() >= 5,
        "Expected at least 5 node kinds, got {}",
        kinds.len()
    );

    // Verify each kind has the expected count
    let kind_map: std::collections::HashMap<NodeKind, usize> = kinds.into_iter().collect();
    assert_eq!(
        kind_map.get(&NodeKind::Function),
        Some(&3),
        "Expected 3 functions"
    );
    assert_eq!(
        kind_map.get(&NodeKind::Struct),
        Some(&2),
        "Expected 2 structs"
    );
    assert_eq!(
        kind_map.get(&NodeKind::Method),
        Some(&2),
        "Expected 2 methods"
    );
    assert_eq!(
        kind_map.get(&NodeKind::Constant),
        Some(&1),
        "Expected 1 constant"
    );
    assert_eq!(
        kind_map.get(&NodeKind::Module),
        Some(&1),
        "Expected 1 module"
    );

    log::info!("indices.iter_kinds() correctly enumerates node kinds after loading");
}

#[test]
fn indices_by_file_works_after_loading() {
    init_logging();
    log::info!("Testing that indices.by_file() works after loading");

    let graph = create_test_graph();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("snapshot.sqry");

    // Save and reload
    save_to_path(&graph, &snapshot_path).expect("Failed to save graph");
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load graph");

    // Get file IDs
    let file1_id = loaded_graph.files().get(Path::new("/test/lib.rs"));
    let file2_id = loaded_graph.files().get(Path::new("/test/utils.rs"));

    assert!(file1_id.is_some(), "Expected file1 to be registered");
    assert!(file2_id.is_some(), "Expected file2 to be registered");

    let file1_id = file1_id.unwrap();
    let file2_id = file2_id.unwrap();

    // Query by file
    let file1_nodes = loaded_graph.indices().by_file(file1_id);
    let file2_nodes = loaded_graph.indices().by_file(file2_id);

    // lib.rs should have 6 nodes (helper, fetch, User, new, get_name, MAX_SIZE)
    assert_eq!(
        file1_nodes.len(),
        6,
        "Expected 6 nodes in lib.rs, got {}",
        file1_nodes.len()
    );

    // utils.rs should have 3 nodes (add, Calculator, utils module)
    assert_eq!(
        file2_nodes.len(),
        3,
        "Expected 3 nodes in utils.rs, got {}",
        file2_nodes.len()
    );

    log::info!("indices.by_file() correctly queries by file after loading");
}

#[test]
fn empty_graph_has_empty_indices() {
    init_logging();
    log::info!("Testing that empty graph has empty indices");

    let graph = CodeGraph::new();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("snapshot.sqry");

    // Verify empty before save
    assert_eq!(graph.nodes().len(), 0, "Expected 0 nodes in empty graph");
    assert_eq!(
        graph.indices().len(),
        0,
        "Expected 0 nodes in empty indices"
    );

    // Save and reload empty graph
    save_to_path(&graph, &snapshot_path).expect("Failed to save empty graph");
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load empty graph");

    // Verify still empty after load
    assert_eq!(
        loaded_graph.nodes().len(),
        0,
        "Expected 0 nodes in loaded empty graph"
    );
    assert_eq!(
        loaded_graph.indices().len(),
        0,
        "Expected 0 nodes in loaded empty indices"
    );

    log::info!("Empty graph correctly has empty indices");
}

/// Test that visibility metadata survives the save/load cycle.
#[test]
fn visibility_metadata_persists_through_save_load() {
    init_logging();
    log::info!("Testing visibility metadata persistence through save/load cycle");

    let graph = create_graph_with_visibility();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let snapshot_path = temp_dir.path().join("visibility_snapshot.sqry");

    // Verify visibility is set before saving
    assert_node_visibility(&graph, "public_func", Some("pub"));
    assert_node_visibility(&graph, "private_func", None);

    // Save graph
    save_to_path(&graph, &snapshot_path).expect("Failed to save graph");
    log::info!(
        "Graph with visibility metadata saved to {:?}",
        snapshot_path
    );

    // Load graph
    let loaded_graph = load_from_path(&snapshot_path, None).expect("Failed to load graph");
    log::info!("Graph with visibility metadata loaded");

    // Verify visibility survives the load
    assert_node_visibility(&loaded_graph, "public_func", Some("pub"));
    assert_node_visibility(&loaded_graph, "private_func", None);

    log::info!("Visibility metadata correctly persisted through save/load cycle");
}
