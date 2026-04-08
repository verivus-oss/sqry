//! Graph builder tests for the CSS language plugin.
//!
//! Covers:
//! - Module node creation
//! - CSS variable (custom property) node extraction
//! - @import edge detection
//! - `url()` reference edges
//! - @layer support
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_css::CssGraphBuilder;
use std::path::Path;

fn parse_css(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .expect("failed to set CSS language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse CSS code")
}

fn count_import_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        })
        .count()
}

fn count_nodes(staging: &StagingGraph) -> usize {
    staging.node_count()
}

fn count_variable_nodes(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::node::NodeKind;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddNode { entry, .. } if entry.kind == NodeKind::Variable
            )
        })
        .count()
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty CSS file should succeed");
}

#[test]
fn test_simple_css_rules() {
    let source = r"
body {
    color: red;
    font-size: 16px;
}

.container {
    width: 100%;
    margin: 0 auto;
}
";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("style.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "Simple CSS rules should succeed");
    // Two rule blocks (body + .container) must each produce at least one node
    assert!(
        count_nodes(&staging) >= 2,
        "Expected at least 2 nodes for two CSS rule blocks, got {}",
        count_nodes(&staging)
    );
}

// ==================== @import Edge Detection ====================

#[test]
fn test_import_statement_basic() {
    let source = r#"@import "reset.css";
body { color: black; }
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for @import, got {import_count}"
    );
}

#[test]
fn test_import_statement_multiple() {
    let source = r#"@import "reset.css";
@import "variables.css";
@import "components.css";

body { margin: 0; }
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 3,
        "Expected at least 3 import edges, got {import_count}"
    );
}

#[test]
fn test_import_url_syntax() {
    let source = r#"@import url("theme.css");
body { color: blue; }
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for url() syntax, got {import_count}"
    );
}

#[test]
fn test_repeated_imports_each_produce_edge() {
    // This test verifies each @import statement is processed individually.
    // The builder does not deduplicate; all three identical @imports produce edges.
    let source = r#"@import "reset.css";
@import "reset.css";
@import "reset.css";
body { color: black; }
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.css"),
            &mut staging,
        )
        .unwrap();

    // All 3 @import statements must each produce an import edge
    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Expected exactly 3 import edges for 3 repeated @imports, got {import_count}"
    );
}

#[test]
fn test_no_imports_in_code_only() {
    let source = r"
body { color: black; }
.nav { display: flex; }
";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("style.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 0, "No @import should produce no import edges");
}

// ==================== CSS Custom Properties (Variables) ====================

#[test]
fn test_css_custom_properties() {
    let source = r"
:root {
    --primary-color: #007bff;
    --font-size-base: 16px;
    --spacing-unit: 8px;
}

body {
    color: var(--primary-color);
    font-size: var(--font-size-base);
}
";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("vars.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "CSS custom properties should succeed");
    // Three custom properties in :root must each produce a StyleVariable node
    assert!(
        count_variable_nodes(&staging) >= 3,
        "Expected at least 3 CSS variable nodes for --primary-color, \
         --font-size-base, --spacing-unit, got {}",
        count_variable_nodes(&staging)
    );
}

// ==================== @layer Support ====================

#[test]
fn test_layer_import() {
    let source = r#"@import "base.css" layer(base);
@import "utils.css" layer(utilities);
body { color: black; }
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("layers.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges for layer imports, got {import_count}"
    );
}

#[test]
fn test_layer_declaration() {
    let source = r"
@layer base, utilities, components;

@layer base {
    body { margin: 0; }
}

@layer utilities {
    .flex { display: flex; }
}
";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("layers.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "@layer declarations should succeed");
}

// ==================== url() References ====================

#[test]
fn test_url_background_image() {
    let source = r#"
.hero {
    background-image: url("hero.jpg");
}

.icon {
    background: url('icons/search.svg');
}
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("style.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "url() references should succeed");
}

#[test]
fn test_data_uri_skipped() {
    // data: URIs should not create import edges
    let source = r#"
.icon {
    background: url("data:image/svg+xml;base64,abc123");
}
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("style.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 0, "data: URIs should not create import edges");
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = CssGraphBuilder;
    assert_eq!(builder.language(), Language::Css);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CssGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_css() {
    // Incomplete CSS - tree-sitter is error-tolerant
    let source = r"
body {
    color:
"; // incomplete
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    // Should not panic
    let result = builder.build_graph(&tree, source.as_bytes(), Path::new("bad.css"), &mut staging);
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
/* This is just a comment */
/* Another comment */
";
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.css"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only CSS should succeed");
}

#[test]
fn test_imports_and_rules_combined() {
    let source = r#"@import "normalize.css";
@import "variables.css";

:root {
    --primary: #007bff;
}

body {
    margin: 0;
    color: var(--primary);
}

.container {
    max-width: 1200px;
    margin: 0 auto;
}
"#;
    let tree = parse_css(source);
    let mut staging = StagingGraph::new();
    let builder = CssGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("main.css"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges, got {import_count}"
    );
}
