// Nested conditionals kept for readability when traversing CSS AST

// Nested conditionals kept for readability when traversing CSS AST

//! CSS language plugin
//!
//! Extracts stylesheet rules, at-rules, and custom properties while tracking
//! resource relations (`@import`, `url()`) to support semantic cross-language
//! search between CSS, HTML, and assets.

pub mod relations;

pub use relations::CssGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

const LANGUAGE_ID: &str = "css";
const LANGUAGE_NAME: &str = "CSS";
const TREE_SITTER_VERSION: &str = "0.23";

/// CSS language plugin implementation
pub struct CssPlugin {
    graph_builder: CssGraphBuilder,
}

impl CssPlugin {
    /// Creates a new CSS plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: CssGraphBuilder,
        }
    }
}

impl Default for CssPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for CssPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "CSS language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["css", "scss", "sass", "less"]
    }

    fn language(&self) -> Language {
        tree_sitter_css::LANGUAGE.into()
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
        Ok(Self::extract_css_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl CssPlugin {
    /// Extract scopes from CSS - @media, @supports, @keyframes, and rule sets
    fn extract_css_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_scopes(tree.root_node(), content, file_path, &mut scopes);

        // Sort by position (required for link_nested_scopes)
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships
        link_nested_scopes(&mut scopes);

        scopes
    }

    fn collect_scopes(node: Node<'_>, content: &[u8], file_path: &Path, scopes: &mut Vec<Scope>) {
        let scope_info = match node.kind() {
            "media_statement" => Some(Self::extract_media_scope(node, content)),
            "supports_statement" => Some(Self::extract_supports_scope(node, content)),
            "keyframes_statement" => Some(Self::extract_keyframes_scope(node, content)),
            "rule_set" => Self::extract_ruleset_scope(node, content),
            // Newer CSS at-rules
            "at_rule" => Self::extract_at_rule_scope(node, content),
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
            Self::collect_scopes(child, content, file_path, scopes);
        }
    }

    fn extract_media_scope(node: Node<'_>, content: &[u8]) -> (String, String) {
        // Try to get the media query condition
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "query_list" || child.kind() == "feature_query")
                && let Ok(text) = child.utf8_text(content)
            {
                return ("media".to_string(), format!("@media {}", text.trim()));
            }
        }
        ("media".to_string(), "@media".to_string())
    }

    fn extract_supports_scope(node: Node<'_>, content: &[u8]) -> (String, String) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "feature_query" || child.kind() == "parenthesized_query")
                && let Ok(text) = child.utf8_text(content)
            {
                return ("supports".to_string(), format!("@supports {}", text.trim()));
            }
        }
        ("supports".to_string(), "@supports".to_string())
    }

    fn extract_keyframes_scope(node: Node<'_>, content: &[u8]) -> (String, String) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "keyframes_name"
                && let Ok(text) = child.utf8_text(content)
            {
                return (
                    "keyframes".to_string(),
                    format!("@keyframes {}", text.trim()),
                );
            }
        }
        ("keyframes".to_string(), "@keyframes".to_string())
    }

    fn extract_ruleset_scope(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        // Get the selector(s) as the scope name
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "selectors"
                && let Ok(text) = child.utf8_text(content)
            {
                let selector = text.trim();
                // Truncate long selectors
                let display_name = if selector.len() > 50 {
                    format!("{}...", &selector[..47])
                } else {
                    selector.to_string()
                };
                return Some(("rule_set".to_string(), display_name));
            }
        }
        None
    }

    /// Extract scope from generic at-rules (@container, @layer, @font-face, @page, etc.)
    fn extract_at_rule_scope(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        // First child should be at_keyword (e.g., "@container", "@layer")
        let mut cursor = node.walk();
        let mut at_keyword = None;
        let mut name_parts = Vec::new();

        for child in node.children(&mut cursor) {
            match child.kind() {
                "at_keyword" => {
                    if let Ok(text) = child.utf8_text(content) {
                        at_keyword = Some(text.trim().to_lowercase());
                    }
                }
                // Capture identifier/name after keyword
                "keyword_query" | "plain_value" | "identifier" => {
                    if let Ok(text) = child.utf8_text(content) {
                        name_parts.push(text.trim().to_string());
                    }
                }
                _ => {}
            }
        }

        let keyword = at_keyword?;
        match keyword.as_str() {
            "@container" => {
                let name = if name_parts.is_empty() {
                    "@container".to_string()
                } else {
                    format!("@container {}", name_parts.join(" "))
                };
                Some(("container".to_string(), name))
            }
            "@layer" => {
                let name = if name_parts.is_empty() {
                    "@layer".to_string()
                } else {
                    format!("@layer {}", name_parts.join(" "))
                };
                Some(("layer".to_string(), name))
            }
            "@font-face" => Some(("font_face".to_string(), "@font-face".to_string())),
            "@page" => {
                let name = if name_parts.is_empty() {
                    "@page".to_string()
                } else {
                    format!("@page {}", name_parts.join(" "))
                };
                Some(("page".to_string(), name))
            }
            // Skip other generic at-rules that aren't scope-like
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn load_fixture(name: &str) -> (Vec<u8>, PathBuf) {
        let path = PathBuf::from("tests/fixtures").join(name);
        let content = fs::read(&path).expect("failed to read fixture");
        (content, path)
    }

    fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
        let mut lookup = HashMap::new();
        for op in staging.operations() {
            if let StagingOp::InternString { local_id, value } = op {
                lookup.insert(local_id.index(), value.clone());
            }
        }
        lookup
    }

    fn resolved_node_name(entry: &NodeEntry, strings: &HashMap<u32, String>) -> Option<String> {
        entry
            .qualified_name
            .and_then(|qualified_name_id| strings.get(&qualified_name_id.index()).cloned())
            .or_else(|| strings.get(&entry.name.index()).cloned())
    }

    fn find_node_entry<'a>(
        staging: &'a StagingGraph,
        name: &str,
        kind: NodeKind,
    ) -> Option<&'a NodeEntry> {
        let strings = build_string_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op
                && entry.kind == kind
                && resolved_node_name(entry, &strings).is_some_and(|node_name| node_name == name)
            {
                return Some(entry);
            }
        }
        None
    }

    fn find_node_id(staging: &StagingGraph, name: &str, kind: NodeKind) -> Option<NodeId> {
        let strings = build_string_lookup(staging);
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, expected_id } = op
                && entry.kind == kind
                && resolved_node_name(entry, &strings).is_some_and(|node_name| node_name == name)
            {
                return *expected_id;
            }
        }
        None
    }

    fn build_graph(content: &[u8], path: &Path) -> StagingGraph {
        let plugin = CssPlugin::default();
        let tree = plugin.parse_ast(content).expect("parse css");
        let builder = plugin.graph_builder().expect("graph builder");
        let mut staging = StagingGraph::new();
        builder
            .build_graph(&tree, content, path, &mut staging)
            .expect("build graph");
        staging
    }

    #[test]
    fn extracts_custom_properties_and_assets() {
        let (content, path) = load_fixture("basic.css");
        let staging = build_graph(&content, &path);

        assert!(
            find_node_entry(&staging, "--primary", NodeKind::Variable).is_some(),
            "custom property node not found"
        );
        assert!(
            find_node_entry(&staging, "/assets/bg.png", NodeKind::Variable).is_some(),
            "asset url node not found"
        );
    }

    #[test]
    fn extracts_import_edges() {
        let (content, path) = load_fixture("basic.css");
        let staging = build_graph(&content, &path);

        let module_id =
            find_node_id(&staging, "css::module", NodeKind::Module).expect("module node not found");
        let import_id =
            find_node_id(&staging, "./reset.css", NodeKind::Import).expect("import node not found");

        let mut has_edge = false;
        for op in staging.operations() {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
                && matches!(kind, EdgeKind::Imports { .. })
                && *source == module_id
                && *target == import_id
            {
                has_edge = true;
                break;
            }
        }
        assert!(has_edge, "import edge not found for ./reset.css");
    }

    // ========================================================================
    // Scope Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_scopes_basic_media() {
        let plugin = CssPlugin::default();
        let source = b"@media (max-width: 768px) { .mobile { display: block; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        assert!(!scopes.is_empty(), "Should extract at least media scope");
        let media_scope = scopes.iter().find(|s| s.scope_type == "media");
        assert!(media_scope.is_some(), "Should have media scope");
        assert!(
            media_scope.unwrap().name.contains("@media"),
            "Media scope name should contain @media"
        );
    }

    #[test]
    fn test_extract_scopes_keyframes() {
        let plugin = CssPlugin::default();
        let source = b"@keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        let keyframes_scope = scopes.iter().find(|s| s.scope_type == "keyframes");
        assert!(keyframes_scope.is_some(), "Should have keyframes scope");
        assert!(
            keyframes_scope.unwrap().name.contains("fadeIn"),
            "Keyframes scope should contain animation name"
        );
    }

    #[test]
    fn test_extract_scopes_supports() {
        let plugin = CssPlugin::default();
        let source = b"@supports (display: grid) { .grid { display: grid; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        let supports_scope = scopes.iter().find(|s| s.scope_type == "supports");
        assert!(supports_scope.is_some(), "Should have supports scope");
    }

    #[test]
    fn test_extract_scopes_rule_set() {
        let plugin = CssPlugin::default();
        let source = b".my-class { color: red; } #my-id { color: blue; }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        let rule_sets: Vec<_> = scopes
            .iter()
            .filter(|s| s.scope_type == "rule_set")
            .collect();
        assert_eq!(rule_sets.len(), 2, "Should have 2 rule_set scopes");
        assert!(
            rule_sets.iter().any(|s| s.name == ".my-class"),
            "Should have .my-class selector"
        );
        assert!(
            rule_sets.iter().any(|s| s.name == "#my-id"),
            "Should have #my-id selector"
        );
    }

    #[test]
    fn test_extract_scopes_container_query() {
        let plugin = CssPlugin::default();
        let source = b"@container sidebar (min-width: 400px) { .card { display: flex; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        // Container queries are mandatory - tree-sitter-css 0.23+ required
        let container_scope = scopes.iter().find(|s| s.scope_type == "container");
        assert!(
            container_scope.is_some(),
            "Container scope must be extracted (requires tree-sitter-css 0.23+)"
        );
        assert!(
            container_scope.unwrap().name.contains("@container"),
            "Container scope name should contain @container"
        );
    }

    #[test]
    fn test_extract_scopes_layer() {
        let plugin = CssPlugin::default();
        let source = b"@layer utilities { .flex { display: flex; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        // Cascade layers are mandatory - tree-sitter-css 0.23+ required
        let layer_scope = scopes.iter().find(|s| s.scope_type == "layer");
        assert!(
            layer_scope.is_some(),
            "Layer scope must be extracted (requires tree-sitter-css 0.23+)"
        );
        assert!(
            layer_scope.unwrap().name.contains("@layer"),
            "Layer scope name should contain @layer"
        );
    }

    #[test]
    fn test_extract_scopes_font_face() {
        let plugin = CssPlugin::default();
        let source = b"@font-face { font-family: 'MyFont'; src: url('myfont.woff2'); }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        // Font-face rules are mandatory
        let font_face_scope = scopes.iter().find(|s| s.scope_type == "font_face");
        assert!(
            font_face_scope.is_some(),
            "Font-face scope must be extracted"
        );
        assert_eq!(
            font_face_scope.unwrap().name,
            "@font-face",
            "Font-face scope name must be @font-face"
        );
    }

    #[test]
    fn test_extract_scopes_page() {
        let plugin = CssPlugin::default();
        let source = b"@page :first { margin: 2cm; }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        // Page rules are mandatory
        let page_scope = scopes.iter().find(|s| s.scope_type == "page");
        assert!(page_scope.is_some(), "Page scope must be extracted");
        assert!(
            page_scope.unwrap().name.contains("@page"),
            "Page scope name should contain @page"
        );
    }

    #[test]
    fn test_extract_scopes_nested_media() {
        let plugin = CssPlugin::default();
        let source = br#"
@media screen {
    .container { width: 100%; }
    @media (min-width: 768px) {
        .container { width: 750px; }
    }
}
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        let media_scopes: Vec<_> = scopes.iter().filter(|s| s.scope_type == "media").collect();
        assert!(media_scopes.len() >= 2, "Should have nested media scopes");

        // Check parent-child relationship
        let inner_media = media_scopes.iter().find(|s| s.name.contains("min-width"));
        if let Some(inner) = inner_media {
            assert!(
                inner.parent_id.is_some(),
                "Inner media should have parent_id"
            );
        }
    }

    #[test]
    fn test_extract_scopes_boundaries() {
        let plugin = CssPlugin::default();
        let source = br#"
.my-selector {
    color: red;
    font-size: 14px;
}
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        assert!(!scopes.is_empty(), "Should extract at least one scope");
        let scope = &scopes[0];

        // Verify boundaries are valid
        assert!(scope.start_line >= 1, "start_line should be >= 1");
        assert!(
            scope.end_line >= scope.start_line,
            "end_line should be >= start_line"
        );
    }

    #[test]
    fn test_extract_scopes_long_selector_truncation() {
        let plugin = CssPlugin::default();
        let source =
            b".very-long-selector-name-that-exceeds-fifty-characters-limit { color: red; }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        let rule_set = scopes.iter().find(|s| s.scope_type == "rule_set");
        assert!(rule_set.is_some());
        // Long selectors should be truncated
        assert!(
            rule_set.unwrap().name.len() <= 53,
            "Selector should be truncated"
        );
    }

    #[test]
    fn test_extract_scopes_empty_file() {
        let plugin = CssPlugin::default();
        let source = b"";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.css"))
            .unwrap();

        assert!(scopes.is_empty(), "Empty file should have no scopes");
    }

    #[test]
    fn test_extract_scopes_malformed() {
        let plugin = CssPlugin::default();
        // Malformed CSS with missing closing brace
        let source = b".broken { color: red;";
        let tree = plugin.parse_ast(source).unwrap();
        let result = plugin.extract_scopes(&tree, source, Path::new("test.css"));

        // Should not panic, may return empty or partial results
        assert!(result.is_ok(), "Should handle malformed CSS gracefully");
    }
}
// Nested conditionals retained for readability in parser traversal
