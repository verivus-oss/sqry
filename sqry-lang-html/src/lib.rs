// Nested conditionals kept for readability when parsing HTML

//! HTML language plugin
//!
//! Extracts semantic symbols for HTML elements (id/class/template/custom elements)
//! and resource relations (`<script src>`, `<link href>`, etc.) to support semantic
//! cross-language search with CSS and JavaScript assets.

mod relations;

pub use relations::HtmlGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

const LANGUAGE_ID: &str = "html";
const LANGUAGE_NAME: &str = "HTML";
const TREE_SITTER_VERSION: &str = "0.23";

/// HTML language plugin implementation
pub struct HtmlPlugin {
    graph_builder: HtmlGraphBuilder,
}

impl HtmlPlugin {
    /// Creates a new HTML plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: HtmlGraphBuilder,
        }
    }
}

impl Default for HtmlPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for HtmlPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "HTML language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["html", "htm", "xhtml"]
    }

    fn language(&self) -> Language {
        tree_sitter_html::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|err| ParseError::LanguageSetFailed(err.to_string()))?;

        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
    }

    fn extract_scopes(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        Ok(Self::extract_html_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

/// Block-level elements that create meaningful scopes in HTML
const SCOPE_ELEMENTS: &[&str] = &[
    "html",
    "head",
    "body",
    "div",
    "section",
    "article",
    "nav",
    "header",
    "footer",
    "aside",
    "main",
    "form",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "ul",
    "ol",
    "li",
    "dl",
    "details",
    "dialog",
    "fieldset",
    "figure",
    "blockquote",
    "pre",
    "address",
    "template",
    "slot",
    // Embedded content roots (SVG, MathML)
    "svg",
    "math",
];

impl HtmlPlugin {
    /// Extract scopes from HTML - block-level elements, custom elements, script/style blocks
    fn extract_html_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_html_scopes(tree.root_node(), content, file_path, &mut scopes);

        // Sort by position (required for link_nested_scopes)
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships
        link_nested_scopes(&mut scopes);

        scopes
    }

    fn collect_html_scopes(
        node: Node<'_>,
        content: &[u8],
        file_path: &Path,
        scopes: &mut Vec<Scope>,
    ) {
        let scope_info = match node.kind() {
            "element" => Self::extract_element_scope(node, content),
            "script_element" => Some(("script".to_string(), "script".to_string())),
            "style_element" => Some(("style".to_string(), "style".to_string())),
            _ => None,
        };

        if let Some((scope_type, name)) = scope_info {
            let start = node.start_position();
            let end = node.end_position();
            scopes.push(Scope {
                id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                scope_type,
                name,
                file_path: file_path.to_path_buf(),
                start_line: start.row + 1,
                start_column: start.column,
                end_line: end.row + 1,
                end_column: end.column,
                parent_id: None, // Will be set by link_nested_scopes
            });
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_html_scopes(child, content, file_path, scopes);
        }
    }

    fn extract_element_scope(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        // Find the start_tag to get the tag name
        let start_tag = node.child_by_field_name("start_tag").or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|child| child.kind() == "start_tag")
        })?;

        // Get the tag name
        let tag_name_node = start_tag.child_by_field_name("tag_name").or_else(|| {
            let mut cursor = start_tag.walk();
            start_tag
                .children(&mut cursor)
                .find(|child| child.kind() == "tag_name")
        })?;

        let tag_name = tag_name_node.utf8_text(content).ok()?.to_lowercase();

        // Check if this is a scope-creating element
        let is_scope_element =
            SCOPE_ELEMENTS.contains(&tag_name.as_str()) || tag_name.contains('-'); // Custom elements

        if !is_scope_element {
            return None;
        }

        // Build a descriptive name including id/class if present
        let mut name = tag_name.clone();
        let mut cursor = start_tag.walk();
        for child in start_tag.children(&mut cursor) {
            if child.kind() == "attribute"
                && let Some((attr_name, attr_value)) = Self::parse_simple_attribute(child, content)
            {
                if attr_name == "id" {
                    name = format!("{tag_name}#{attr_value}");
                    break; // ID is most specific, use it
                } else if attr_name == "class" && !name.contains('#') {
                    // Use first class if no id
                    if let Some(first_class) = attr_value.split_whitespace().next() {
                        name = format!("{tag_name}.{first_class}");
                    }
                }
            }
        }

        Some((tag_name, name))
    }

    fn parse_simple_attribute(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        let mut attr_name = None;
        let mut attr_value = None;

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "attribute_name" => {
                    attr_name = child.utf8_text(content).ok().map(str::to_lowercase);
                }
                "attribute_value" => {
                    attr_value = child.utf8_text(content).ok().map(str::to_string);
                }
                "quoted_attribute_value" => {
                    if let Ok(text) = child.utf8_text(content) {
                        attr_value = Some(trim_quotes(text).to_string());
                    }
                }
                _ => {}
            }
        }

        Some((attr_name?, attr_value.unwrap_or_default()))
    }
}

fn trim_quotes(input: &str) -> &str {
    let trimmed = input.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn load_fixture(name: &str) -> (Vec<u8>, PathBuf) {
        let path = PathBuf::from("tests/fixtures").join(name);
        let content = fs::read(&path).expect("failed to read fixture");
        (content, path)
    }

    #[test]
    fn extracts_html_graph_nodes() {
        let (content, path) = load_fixture("basic.html");
        let staging = build_graph(&content, &path);

        assert!(
            find_node_entry(&staging, "styles.css", NodeKind::Import).is_some(),
            "stylesheet import node not found"
        );
        assert!(
            find_node_entry(&staging, "app.js", NodeKind::Import).is_some(),
            "script import node not found"
        );
        assert!(
            find_node_entry(&staging, "/assets/logo.png", NodeKind::Variable).is_some(),
            "image asset node not found"
        );
    }

    #[test]
    fn extracts_html_resource_edges() {
        let (content, path) = load_fixture("basic.html");
        let staging = build_graph(&content, &path);

        let module_id = find_node_id(&staging, "html::module", NodeKind::Module)
            .expect("html module node missing");
        let css_id =
            find_node_id(&staging, "styles.css", NodeKind::Import).expect("css import missing");
        let script_id =
            find_node_id(&staging, "app.js", NodeKind::Import).expect("script import missing");
        let asset_id = find_node_id(&staging, "/assets/logo.png", NodeKind::Variable)
            .expect("asset node missing");

        let mut has_css = false;
        let mut has_script = false;
        let mut has_asset_ref = false;
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
            {
                if matches!(kind, EdgeKind::Imports { .. }) && *source == module_id {
                    if *target == css_id {
                        has_css = true;
                    }
                    if *target == script_id {
                        has_script = true;
                    }
                }
                if matches!(kind, EdgeKind::References)
                    && *source == module_id
                    && *target == asset_id
                {
                    has_asset_ref = true;
                }
            }
        }

        assert!(has_css, "missing import edge for styles.css");
        assert!(has_script, "missing import edge for app.js");
        assert!(has_asset_ref, "missing reference edge for logo asset");
    }

    fn find_node_entry<'a>(
        staging: &'a StagingGraph,
        name: &str,
        kind: NodeKind,
    ) -> Option<&'a NodeEntry> {
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op
                && entry.kind == kind
                && staging.resolve_node_canonical_name(entry) == Some(name)
            {
                return Some(entry);
            }
        }
        None
    }

    fn find_node_id(staging: &StagingGraph, name: &str, kind: NodeKind) -> Option<NodeId> {
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == kind
                && staging.resolve_node_canonical_name(entry) == Some(name)
            {
                return *expected_id;
            }
        }
        None
    }

    fn build_graph(content: &[u8], path: &Path) -> StagingGraph {
        let plugin = HtmlPlugin::default();
        let tree = plugin.parse_ast(content).expect("parse html");
        let builder = plugin.graph_builder().expect("graph builder");
        let mut staging = StagingGraph::new();
        builder
            .build_graph(&tree, content, path, &mut staging)
            .expect("build graph");
        staging
    }

    // ========================================================================
    // Scope Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_scopes_basic_elements() {
        let plugin = HtmlPlugin::default();
        let source = b"<html><body><div>content</div></body></html>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        assert!(scopes.len() >= 3, "Should extract html, body, div scopes");

        let html_scope = scopes.iter().find(|s| s.scope_type == "html");
        assert!(html_scope.is_some(), "Should have html scope");

        let body_scope = scopes.iter().find(|s| s.scope_type == "body");
        assert!(body_scope.is_some(), "Should have body scope");

        let div_scope = scopes.iter().find(|s| s.scope_type == "div");
        assert!(div_scope.is_some(), "Should have div scope");
    }

    #[test]
    fn test_extract_scopes_with_id() {
        let plugin = HtmlPlugin::default();
        let source = b"<div id=\"main\">content</div>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let div_scope = scopes.iter().find(|s| s.scope_type == "div");
        assert!(div_scope.is_some(), "Should have div scope");
        assert!(
            div_scope.unwrap().name.contains("#main"),
            "Scope name should include id"
        );
    }

    #[test]
    fn test_extract_scopes_with_class() {
        let plugin = HtmlPlugin::default();
        let source = b"<section class=\"hero main-section\">content</section>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let section_scope = scopes.iter().find(|s| s.scope_type == "section");
        assert!(section_scope.is_some(), "Should have section scope");
        // Uses first class
        assert!(
            section_scope.unwrap().name.contains(".hero"),
            "Scope name should include first class"
        );
    }

    #[test]
    fn test_extract_scopes_custom_elements() {
        let plugin = HtmlPlugin::default();
        let source = b"<my-custom-element>content</my-custom-element>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let custom_scope = scopes.iter().find(|s| s.scope_type == "my-custom-element");
        assert!(
            custom_scope.is_some(),
            "Should extract custom element as scope"
        );
    }

    #[test]
    fn test_extract_scopes_script_style() {
        let plugin = HtmlPlugin::default();
        let source = b"<script>console.log('hello');</script><style>.foo { color: red; }</style>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let script_scope = scopes.iter().find(|s| s.scope_type == "script");
        assert!(script_scope.is_some(), "Should have script scope");

        let style_scope = scopes.iter().find(|s| s.scope_type == "style");
        assert!(style_scope.is_some(), "Should have style scope");
    }

    #[test]
    fn test_extract_scopes_svg() {
        let plugin = HtmlPlugin::default();
        let source =
            b"<svg width=\"100\" height=\"100\"><circle cx=\"50\" cy=\"50\" r=\"40\"/></svg>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let svg_scope = scopes.iter().find(|s| s.scope_type == "svg");
        assert!(svg_scope.is_some(), "Should have svg scope");
    }

    #[test]
    fn test_extract_scopes_math() {
        let plugin = HtmlPlugin::default();
        let source = b"<math><mi>x</mi><mo>=</mo><mn>1</mn></math>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let math_scope = scopes.iter().find(|s| s.scope_type == "math");
        assert!(math_scope.is_some(), "Should have math scope");
    }

    #[test]
    fn test_extract_scopes_nested_divs() {
        let plugin = HtmlPlugin::default();
        let source = br#"
<div id="outer">
    <div id="inner">
        content
    </div>
</div>
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let div_scopes: Vec<_> = scopes.iter().filter(|s| s.scope_type == "div").collect();
        assert_eq!(div_scopes.len(), 2, "Should have 2 div scopes");

        // Check nesting
        let inner = div_scopes.iter().find(|s| s.name.contains("#inner"));
        if let Some(inner) = inner {
            assert!(inner.parent_id.is_some(), "Inner div should have parent_id");
        }
    }

    #[test]
    fn test_extract_scopes_boundaries() {
        let plugin = HtmlPlugin::default();
        let source = br"
<article>
    <p>Paragraph content</p>
</article>
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        let article_scope = scopes.iter().find(|s| s.scope_type == "article");
        assert!(article_scope.is_some());

        let scope = article_scope.unwrap();
        assert!(scope.start_line >= 1);
        assert!(scope.end_line >= scope.start_line);
    }

    #[test]
    fn test_extract_scopes_semantic_elements() {
        let plugin = HtmlPlugin::default();
        let source =
            b"<header>H</header><main>M</main><footer>F</footer><nav>N</nav><aside>A</aside>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        assert!(
            scopes.iter().any(|s| s.scope_type == "header"),
            "Should have header"
        );
        assert!(
            scopes.iter().any(|s| s.scope_type == "main"),
            "Should have main"
        );
        assert!(
            scopes.iter().any(|s| s.scope_type == "footer"),
            "Should have footer"
        );
        assert!(
            scopes.iter().any(|s| s.scope_type == "nav"),
            "Should have nav"
        );
        assert!(
            scopes.iter().any(|s| s.scope_type == "aside"),
            "Should have aside"
        );
    }

    #[test]
    fn test_extract_scopes_empty_file() {
        let plugin = HtmlPlugin::default();
        let source = b"";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        assert!(scopes.is_empty(), "Empty file should have no scopes");
    }

    #[test]
    fn test_extract_scopes_malformed() {
        let plugin = HtmlPlugin::default();
        // Malformed HTML with unclosed tags
        let source = b"<div><span>unclosed";
        let tree = plugin.parse_ast(source).unwrap();
        let result = plugin.extract_scopes(&tree, source, Path::new("test.html"));

        // Should not panic
        assert!(result.is_ok(), "Should handle malformed HTML gracefully");
    }

    #[test]
    fn test_extract_scopes_non_scope_elements_ignored() {
        let plugin = HtmlPlugin::default();
        // span, p are not in SCOPE_ELEMENTS
        let source = b"<span>text</span><p>paragraph</p><strong>bold</strong>";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.html"))
            .unwrap();

        // These elements should not create scopes
        assert!(
            !scopes.iter().any(|s| s.scope_type == "span"),
            "span should not be a scope"
        );
        assert!(
            !scopes.iter().any(|s| s.scope_type == "strong"),
            "strong should not be a scope"
        );
    }
}
