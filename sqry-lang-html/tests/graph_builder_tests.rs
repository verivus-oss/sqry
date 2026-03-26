//! Graph builder tests for the HTML language plugin.
//!
//! Covers:
//! - Module node creation
//! - Script resource edges (<script src>)
//! - Stylesheet resource edges (<link href>)
//! - Image resource edges (<img src>)
//! - Anchor link edges (<a href>)
//! - Inline event handler call edges
//! - Error handling for malformed input

use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language};
use sqry_lang_html::HtmlGraphBuilder;
use std::path::Path;

fn parse_html(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .expect("failed to set HTML language");
    parser
        .parse(source.as_bytes(), None)
        .expect("failed to parse HTML code")
}

fn count_edges_of_kind(staging: &StagingGraph, kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

fn count_import_edges(staging: &StagingGraph) -> usize {
    count_edges_of_kind(staging, |k| matches!(k, EdgeKind::Imports { .. }))
}

fn has_interned_string_containing(staging: &StagingGraph, pattern: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::InternString { value, .. } = op {
            value.contains(pattern)
        } else {
            false
        }
    })
}

// ==================== Basic Tests ====================

#[test]
fn test_empty_file() {
    let source = "";
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Empty HTML file should succeed");
}

#[test]
fn test_minimal_html() {
    let source = r"
<!DOCTYPE html>
<html>
<head><title>Test</title></head>
<body>
  <p>Hello World</p>
</body>
</html>
";
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("index.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Minimal HTML should succeed");

    // Should create module node
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 module node, got {}",
        stats.nodes_staged
    );
}

// ==================== Script Resource Edges ====================

#[test]
fn test_script_src_import() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <script src="main.js"></script>
  <script src="vendor/jquery.min.js"></script>
</head>
<body></body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("index.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges for script src, got {}",
        import_count
    );
    assert!(
        has_interned_string_containing(&staging, "main.js"),
        "Expected 'main.js' in imports"
    );
}

#[test]
fn test_script_src_single() {
    let source = r#"<!DOCTYPE html>
<html>
<head><script src="app.js"></script></head>
<body></body>
</html>"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("page.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge for script src, got {}",
        import_count
    );
}

// ==================== Stylesheet Resource Edges ====================

#[test]
fn test_link_stylesheet_import() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <link rel="stylesheet" href="style.css">
  <link rel="stylesheet" href="components/buttons.css">
</head>
<body></body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("index.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges for stylesheets, got {}",
        import_count
    );
    assert!(
        has_interned_string_containing(&staging, "style.css"),
        "Expected 'style.css' in imports"
    );
}

// ==================== Image References ====================

#[test]
fn test_img_src_reference() {
    let source = r#"
<!DOCTYPE html>
<html>
<body>
  <img src="hero.jpg" alt="Hero">
  <img src="icons/logo.svg" alt="Logo">
</body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("index.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Image references should succeed");
}

// ==================== No External Resources ====================

#[test]
fn test_no_resources() {
    let source = r"
<!DOCTYPE html>
<html>
<head><title>Plain HTML</title></head>
<body>
  <p>No external resources here</p>
  <h1>Just text</h1>
</body>
</html>
";
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("plain.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 0,
        "HTML without resources should have no import edges"
    );
}

// ==================== Remote URLs ====================

#[test]
fn test_remote_url_script() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <script src="https://cdn.example.com/lib.js"></script>
</head>
<body></body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("index.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Remote URL scripts should succeed");
}

// ==================== Module Preload ====================

#[test]
fn test_modulepreload() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <link rel="modulepreload" href="modules/app.js">
  <link rel="preload" href="fonts/roboto.woff2" as="font">
</head>
<body></body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("index.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Module preload links should succeed");
}

// ==================== Combined Resources ====================

#[test]
fn test_all_resource_types_combined() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <title>Full Page</title>
  <link rel="stylesheet" href="styles/main.css">
  <script src="scripts/app.js"></script>
</head>
<body>
  <img src="images/logo.png" alt="Logo">
  <a href="about.html">About</a>
</body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("index.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 2,
        "Expected at least 2 import edges (css + js), got {}",
        import_count
    );
}

// ==================== Builder Properties ====================

#[test]
fn test_builder_language() {
    let builder = HtmlGraphBuilder::new();
    assert_eq!(builder.language(), Language::Html);
}

#[test]
fn test_builder_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HtmlGraphBuilder>();
}

// ==================== Error Handling ====================

#[test]
fn test_malformed_html() {
    // Malformed HTML - tree-sitter is error-tolerant
    let source = r"
<html>
<head>
<title>Broken
<body>
<p>Unclosed tags
";
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    // Should not panic
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("broken.html"),
        &mut staging,
    );
    let _ = result;
}

#[test]
fn test_comments_only() {
    let source = r"
<!-- This is just a comment -->
<!-- Another comment -->
";
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("comments.html"),
        &mut staging,
    );
    assert!(result.is_ok(), "Comments-only HTML should succeed");
}

#[test]
fn test_self_closing_tags() {
    let source = r#"
<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8" />
  <link rel="stylesheet" href="style.css" />
</head>
<body>
  <br />
  <hr />
  <img src="pic.jpg" alt="Picture" />
</body>
</html>
"#;
    let tree = parse_html(source);
    let mut staging = StagingGraph::new();
    let builder = HtmlGraphBuilder::new();

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("page.html"),
            &mut staging,
        )
        .unwrap();

    let import_count = count_import_edges(&staging);
    assert!(
        import_count >= 1,
        "Expected at least 1 import edge from self-closing link, got {}",
        import_count
    );
}
