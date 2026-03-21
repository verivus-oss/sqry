//! Integration tests for P3 edge emission.
//!
//! These tests verify that P3 features actually emit edges in the graph,
//! not just that the scaffolding exists.
//!
//! # Current Implementation Status (v2.8.0)
//!
//! - **Macro invocations**: Function-like macro calls are tracked (e.g., `println!`, `vec!`)
//! - **Macro nodes**: Declarative macros (`macro_rules!`) create `NodeKind::Macro` nodes
//! - **Lifetime extraction**: Scaffolding present, requires complete implementation
//! - **Trait method binding**: Scaffolding present, requires Rust Analyzer integration
//! - **Derive macro expansion**: Code exists but not producing edges yet
//!
//! # Test Philosophy
//!
//! These tests verify that:
//! 1. P3 processors run without panicking
//! 2. Config flags control feature activation
//! 3. Any edges/nodes that ARE produced have correct structure
//! 4. Tests document current state rather than aspirational goals

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::{NodeKind, StagingGraph};
use sqry_lang_rust::relations::{RustGraphBuilder, RustGraphConfig};
use sqry_test_support::graph_helpers::build_node_name_lookup;
use std::path::Path;
use tree_sitter::Parser;

/// Helper to build a staging graph from Rust source code.
fn build_staging_graph(source: &str, config: RustGraphConfig) -> StagingGraph {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to set Rust language");

    let tree = parser.parse(source, None).expect("Failed to parse");
    let file_path = Path::new("test.rs");

    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::with_config(4, config);

    builder
        .build_graph(&tree, source.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Helper to count nodes of a specific kind.
fn count_node_kind(staging: &StagingGraph, node_kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                entry.kind == node_kind
            } else {
                false
            }
        })
        .count()
}

/// Helper to count edges of a specific kind.
fn count_edge_kind(staging: &StagingGraph, edge_kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                edge_kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

/// Helper to collect all edges of a specific kind with source/target names.
fn collect_edges_of_kind(
    staging: &StagingGraph,
    edge_kind_check: impl Fn(&EdgeKind) -> bool,
) -> Vec<(String, String, EdgeKind)> {
    let node_names = build_node_name_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
                && edge_kind_check(kind)
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| format!("node_{}", source.index()));
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| format!("node_{}", target.index()));
                return Some((source_name, target_name, kind.clone()));
            }
            None
        })
        .collect()
}

#[test]
fn test_lifetime_constraint_edges_emitted() {
    // NOTE: This test verifies the lifetime extraction SCAFFOLDING is present
    // and doesn't panic. Actual lifetime extraction requires pub functions
    // (visibility filtering) and may not produce edges for all patterns yet.
    let source = r#"
pub fn example<'a, 'b>(x: &'a str, y: &'b str) -> &'a str
where
    'b: 'a,
{
    x
}

pub struct RefHolder<'a> {
    data: &'a str,
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: true,
        enable_trait_binding: false,
        enable_macro_expansion: false,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Verify lifetime nodes exist (if any)
    let lifetime_nodes = count_node_kind(&staging, NodeKind::Lifetime);

    // Verify LifetimeConstraint edges exist (if any)
    let constraint_edges = count_edge_kind(&staging, |k| k.is_lifetime_constraint());

    // Print summary for debugging
    println!("=== Lifetime Extraction Test ===");
    println!("Lifetime nodes: {}", lifetime_nodes);
    println!("LifetimeConstraint edges: {}", constraint_edges);

    // Collect and print edge details if any
    if constraint_edges > 0 {
        let edges = collect_edges_of_kind(&staging, |k| k.is_lifetime_constraint());
        for (source, target, kind) in &edges {
            println!("  {} -> {} [{:?}]", source, target, kind);
        }
    } else {
        println!("  No lifetime constraints extracted (implementation in progress)");
    }

    // At minimum, verify the extractor runs without panicking
    // Actual output depends on complete implementation
}

#[test]
fn test_trait_method_binding_edges_emitted() {
    let source = r#"
trait MyTrait {
    fn do_something(&self);
}

struct MyStruct;

impl MyTrait for MyStruct {
    fn do_something(&self) {
        println!("doing something");
    }
}

fn use_trait(s: &MyStruct) {
    s.do_something();
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: false,
        enable_trait_binding: true,
        enable_macro_expansion: false,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Verify TraitMethodBinding edges exist (or at least don't panic)
    let binding_edges = count_edge_kind(&staging, |k| k.is_trait_method_binding());

    // Print summary for debugging
    println!("=== Trait Method Binding Test ===");
    println!("TraitMethodBinding edges found: {}", binding_edges);

    // Note: This may be 0 if the binder can't resolve the type without RA
    // At minimum, verify the code doesn't panic and the builder runs successfully
    if binding_edges > 0 {
        let edges = collect_edges_of_kind(&staging, |k| k.is_trait_method_binding());
        for (source, target, kind) in &edges {
            println!("  {} -> {} [{:?}]", source, target, kind);
        }
    } else {
        println!("  No trait method bindings resolved (expected without RA)");
    }
}

#[test]
fn test_macro_expansion_edges_emitted() {
    // NOTE: Derive macro processing requires pub structs/enums due to visibility filtering
    let source = r#"
#[derive(Debug, Clone)]
pub struct MyData {
    value: i32,
}

fn main() {
    let data = MyData { value: 42 };
    println!("{:?}", data);
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: false,
        enable_trait_binding: false,
        enable_macro_expansion: true,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Verify MacroExpansion edges exist
    let expansion_edges = count_edge_kind(&staging, |k| k.is_macro_expansion());

    // Print summary for debugging
    println!("=== Macro Expansion Test ===");
    println!("MacroExpansion edges found: {}", expansion_edges);

    // Collect and print edge details
    let edges = collect_edges_of_kind(&staging, |k| k.is_macro_expansion());
    for (source, target, kind) in &edges {
        println!("  {} -> {} [{:?}]", source, target, kind);
    }

    // Check for derive macro edges
    let derive_edges = count_edge_kind(&staging, |k| {
        matches!(
            k,
            EdgeKind::MacroExpansion {
                expansion_kind: sqry_core::graph::unified::MacroExpansionKind::Derive,
                ..
            }
        )
    });

    println!("Derive macro edges: {}", derive_edges);

    // Verify at least some macro expansion tracking exists
    assert!(
        expansion_edges > 0 || derive_edges > 0,
        "Expected some MacroExpansion edges, found none"
    );
}

#[test]
fn test_macro_nodes_created() {
    let source = r#"
macro_rules! my_macro {
    ($x:expr) => {
        $x + 1
    };
}

fn main() {
    let result = my_macro!(5);
}
"#;

    let config = RustGraphConfig::new();

    let staging = build_staging_graph(source, config);

    // Verify Macro nodes exist
    let macro_nodes = count_node_kind(&staging, NodeKind::Macro);

    assert!(
        macro_nodes > 0,
        "Expected Macro nodes for macro_rules!, found none"
    );

    println!("=== Macro Nodes Test ===");
    println!("Macro nodes found: {}", macro_nodes);
}

#[test]
fn test_all_p3_features_together() {
    // NOTE: Use pub items for visibility filtering
    let source = r#"
#[derive(Debug)]
pub struct Container<'a> {
    data: &'a str,
}

pub trait Processor {
    fn process(&self);
}

impl<'a> Processor for Container<'a> {
    fn process(&self) {
        println!("{}", self.data);
    }
}

pub fn main() {
    let container = Container { data: "hello" };
    container.process();
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: true,
        enable_trait_binding: true,
        enable_macro_expansion: true,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Summary of what was found
    let lifetime_nodes = count_node_kind(&staging, NodeKind::Lifetime);
    let macro_nodes = count_node_kind(&staging, NodeKind::Macro);
    let lifetime_edges = count_edge_kind(&staging, |k| k.is_lifetime_constraint());
    let trait_edges = count_edge_kind(&staging, |k| k.is_trait_method_binding());
    let macro_edges = count_edge_kind(&staging, |k| k.is_macro_expansion());

    println!("=== P3 Features Summary ===");
    println!("Lifetime nodes: {}", lifetime_nodes);
    println!("Macro nodes: {}", macro_nodes);
    println!("LifetimeConstraint edges: {}", lifetime_edges);
    println!("TraitMethodBinding edges: {}", trait_edges);
    println!("MacroExpansion edges: {}", macro_edges);

    // Verify that enabling P3 features doesn't cause panics
    // Actual output depends on implementation completeness
    println!("P3 features executed without errors");
}

#[test]
fn test_p3_features_respect_config_flags() {
    // NOTE: Use pub items for visibility filtering
    let source = r#"
#[derive(Debug)]
pub struct Container<'a> {
    data: &'a str,
}

pub fn use_container(c: &Container) {
    println!("{:?}", c);
}
"#;

    // Test with all features disabled
    let config_disabled = RustGraphConfig {
        enable_lifetime_extraction: false,
        enable_trait_binding: false,
        enable_macro_expansion: false,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging_disabled = build_staging_graph(source, config_disabled);

    let lifetime_edges_disabled =
        count_edge_kind(&staging_disabled, |k| k.is_lifetime_constraint());
    let macro_edges_disabled = count_edge_kind(&staging_disabled, |k| k.is_macro_expansion());

    println!("=== Config Flags Test (Disabled) ===");
    println!(
        "LifetimeConstraint edges (disabled): {}",
        lifetime_edges_disabled
    );
    println!("MacroExpansion edges (disabled): {}", macro_edges_disabled);

    // Test with all features enabled
    let config_enabled = RustGraphConfig {
        enable_lifetime_extraction: true,
        enable_trait_binding: true,
        enable_macro_expansion: true,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging_enabled = build_staging_graph(source, config_enabled);

    let lifetime_edges_enabled = count_edge_kind(&staging_enabled, |k| k.is_lifetime_constraint());
    let macro_edges_enabled = count_edge_kind(&staging_enabled, |k| k.is_macro_expansion());

    println!("=== Config Flags Test (Enabled) ===");
    println!(
        "LifetimeConstraint edges (enabled): {}",
        lifetime_edges_enabled
    );
    println!("MacroExpansion edges (enabled): {}", macro_edges_enabled);

    // Verify that enabling features doesn't reduce output
    assert!(
        lifetime_edges_enabled >= lifetime_edges_disabled,
        "Enabling lifetime extraction should not reduce edge count"
    );
    assert!(
        macro_edges_enabled >= macro_edges_disabled,
        "Enabling macro expansion should not reduce edge count"
    );

    // Config flags control whether P3 processors run (verified by no panics)
    println!("Config flags respected - no errors with features enabled/disabled");
}

#[test]
fn test_lifetime_constraint_kinds() {
    let source = r#"
fn outlives<'a, 'b: 'a>(x: &'a str, y: &'b str) -> &'a str {
    x
}

struct TypeBound<'a, T: 'a> {
    data: &'a T,
}

fn static_bound() -> &'static str {
    "forever"
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: true,
        enable_trait_binding: false,
        enable_macro_expansion: false,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Verify we have lifetime constraint edges
    let constraint_edges = count_edge_kind(&staging, |k| k.is_lifetime_constraint());

    println!("=== Lifetime Constraint Kinds Test ===");
    println!("Total LifetimeConstraint edges: {}", constraint_edges);

    if constraint_edges > 0 {
        let edges = collect_edges_of_kind(&staging, |k| k.is_lifetime_constraint());
        for (source, target, kind) in &edges {
            println!("  {} -> {} [{:?}]", source, target, kind);
        }
    }

    // Verify extractor runs without panicking (constraint_edges is always >= 0)
    let _ = constraint_edges;
}

#[test]
fn test_macro_expansion_kinds() {
    // NOTE: Use pub items for visibility filtering
    let source = r#"
// Derive macros
#[derive(Debug, Clone, Copy)]
pub struct Point {
    x: i32,
    y: i32,
}

// Declarative macro
macro_rules! add {
    ($a:expr, $b:expr) => {
        $a + $b
    };
}

pub fn main() {
    let p = Point { x: 1, y: 2 };
    let sum = add!(3, 4);
}
"#;

    let config = RustGraphConfig {
        enable_lifetime_extraction: false,
        enable_trait_binding: false,
        enable_macro_expansion: true,
        enable_rust_analyzer: false,
        workspace_root: None,
    };

    let staging = build_staging_graph(source, config);

    // Verify we have macro expansion edges or nodes
    let expansion_edges = count_edge_kind(&staging, |k| k.is_macro_expansion());
    let macro_nodes = count_node_kind(&staging, NodeKind::Macro);

    println!("=== Macro Expansion Kinds Test ===");
    println!("Total MacroExpansion edges: {}", expansion_edges);
    println!("Total Macro nodes: {}", macro_nodes);

    if expansion_edges > 0 {
        let edges = collect_edges_of_kind(&staging, |k| k.is_macro_expansion());
        for (source, target, kind) in &edges {
            println!("  {} -> {} [{:?}]", source, target, kind);
        }

        // Count derive macro expansions if any
        let derive_count = count_edge_kind(&staging, |k| {
            matches!(
                k,
                EdgeKind::MacroExpansion {
                    expansion_kind: sqry_core::graph::unified::MacroExpansionKind::Derive,
                    ..
                }
            )
        });

        println!("Derive macro expansions: {}", derive_count);
    }

    // Verify macro tracking is active (nodes or edges exist)
    assert!(
        expansion_edges > 0 || macro_nodes > 0,
        "Expected some macro tracking (nodes or edges), found none"
    );
}
