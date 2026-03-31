// Nested conditionals kept for readability while walking HTML AST

//! HTML `GraphBuilder` implementation for `CodeGraph` integration.
//!
//! Extracts resource dependency edges from HTML documents:
//! - `<script src="...">` → JavaScript import
//! - `<link rel=\"stylesheet\" href=\"...\">` → CSS import
//! - `<a href="...">` → Navigation link reference
//! - `<img src="...">` → Image asset reference
//! - `<video>`/`<audio>`/`<source>` → Media asset reference
//! - `<iframe src="...">` → Frame reference
//! - `<link rel="modulepreload">` → Module preload
//! - `on*="handler()"` → Event handler function calls (Calls edges)
//!
//! Remote URLs (`http://`, `https://`, `//`) are marked with `Language::Http`.
//! Relative/absolute paths are resolved against the source file.

use std::path::Path;

use sqry_core::graph::{
    GraphBuilder, GraphResult, Language,
    node::{Position, Span},
    unified::{GraphBuildHelper, StagingGraph},
};
use tree_sitter::{Node, Tree};

// ============================================================================
// Constants
// ============================================================================

/// JavaScript keywords that should NOT be treated as function calls.
/// These appear frequently in inline event handlers and must be filtered.
const JS_KEYWORDS: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "break",
    "continue",
    "return",
    "throw",
    "try",
    "catch",
    "finally",
    "new",
    "delete",
    "typeof",
    "instanceof",
    "void",
    "in",
    "of",
    "with",
    "debugger",
    "class",
    "extends",
    "super",
    "import",
    "export",
    "default",
    "yield",
    "await",
    "async",
    "function",
    "var",
    "let",
    "const",
    "true",
    "false",
    "null",
    "undefined",
    "this",
    "arguments",
    "NaN",
    "Infinity",
];

// ============================================================================
// Fast Pre-Check
// ============================================================================

/// Window size for the HTML fast pre-check scan.
///
/// Set to 4 KiB to accommodate files with large license banners,
/// comments, or generated preambles before the first `<` tag.
const PRECHECK_WINDOW: usize = 4096;

/// Fast byte-level pre-check that rejects non-HTML content in <1μs.
///
/// Scans the first 4 KiB for `<`, the fundamental HTML delimiter.
/// Any valid HTML or XHTML file will contain at least one `<` near the
/// start (doctype, `<html>`, `<head>`, etc.).  Binary files and
/// plain-text files misnamed with `.html` are rejected before the
/// graph builder allocates any nodes.
fn html_fast_precheck(content: &[u8]) -> bool {
    let window = &content[..content.len().min(PRECHECK_WINDOW)];
    window.contains(&b'<')
}

/// `GraphBuilder` for HTML documents
#[derive(Debug, Default)]
pub struct HtmlGraphBuilder;

impl HtmlGraphBuilder {
    /// Create a new HTML graph builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl GraphBuilder for HtmlGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        // Fast pre-check: reject files without `<` in the first 512 bytes.
        // This eliminates binary files or non-HTML content misnamed with
        // an .html/.htm extension before allocating graph nodes.
        if !html_fast_precheck(content) {
            return Ok(());
        }

        let mut helper = GraphBuildHelper::new(staging, file, Language::Html);

        // Create module node for the HTML file itself
        let module_id = helper.add_module("html::module", None);

        // Extract DSL nodes (HTML elements and attributes)
        let root = tree.root_node();
        extract_html_dsl_nodes(&root, content, &mut helper, module_id)?;

        // Walk the AST to find resource references
        extract_resources(&root, content, &mut helper, module_id)?;

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Html
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract resource dependencies from HTML AST
fn extract_resources(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "element" => {
                extract_element_resources(&child, content, helper, module_id);
                // Recurse into element children
                extract_resources(&child, content, helper, module_id)?;
            }
            "script_element" => {
                extract_script_resources(&child, content, helper, module_id);
            }
            "self_closing_tag" => {
                extract_self_closing_resources(&child, content, helper, module_id);
            }
            _ => {
                // Recurse into other node types
                extract_resources(&child, content, helper, module_id)?;
            }
        }
    }

    Ok(())
}

/// Extract resources from a regular element (img, link, a, etc.)
fn extract_element_resources(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) {
    let tag_name = get_element_tag_name(node, content);
    let attributes = collect_element_attributes(node, content);

    match tag_name.as_deref() {
        Some("link") => {
            // <link rel="stylesheet" href="...">
            if let Some(href) = attributes.get("href")
                && !href.starts_with("data:")
                && !href.starts_with('#')
            {
                let import_id = helper.add_verbatim_import(href, None);
                helper.add_import_edge(module_id, import_id);
            }
        }
        Some("img") => {
            // <img src="...">
            if let Some(src) = attributes.get("src")
                && !src.starts_with("data:")
            {
                let asset_id = helper.add_verbatim_variable(src, None);
                // Assets are referenced, not imported
                helper.add_reference_edge(module_id, asset_id);
            }
        }
        Some("a") => {
            // <a href="...">
            if let Some(href) = attributes.get("href")
                && !href.starts_with('#')
                && !href.starts_with("javascript:")
                && !href.starts_with("mailto:")
            {
                let link_id = helper.add_verbatim_import(href, None);
                helper.add_import_edge(module_id, link_id);
            }
        }
        Some("iframe") => {
            // <iframe src="...">
            if let Some(src) = attributes.get("src")
                && !src.starts_with("about:")
            {
                let frame_id = helper.add_verbatim_import(src, None);
                helper.add_import_edge(module_id, frame_id);
            }
        }
        _ => {}
    }

    // Extract event handler calls for all elements
    extract_event_handlers(node, content, helper, tag_name.as_deref());
}

/// Extract event handler calls from element attributes.
///
/// Detects `on*` attributes (onclick, onsubmit, onchange, etc.) and extracts
/// function calls from the handler value. Emits `Calls` edges from the element
/// to each handler function.
///
/// # Event Handler Attributes Detected
///
/// - Mouse: onclick, ondblclick, onmousedown, onmouseup, onmouseover, onmouseout, onmousemove
/// - Keyboard: onkeydown, onkeyup, onkeypress
/// - Form: onsubmit, onreset, onchange, oninput, onselect, onfocus, onblur
/// - Window: onload, onunload, onerror, onresize, onscroll
/// - Touch: ontouchstart, ontouchend, ontouchmove
/// - Drag: ondrag, ondragstart, ondragend, ondragover, ondragenter, ondragleave, ondrop
/// - And any other attribute starting with "on"
fn extract_event_handlers(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    tag_name: Option<&str>,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            let mut tag_cursor = child.walk();
            for tag_child in child.children(&mut tag_cursor) {
                if tag_child.kind() == "attribute"
                    && let Some((attr_name, attr_value)) = parse_html_attribute(&tag_child, content)
                {
                    // Check if this is an event handler attribute (starts with "on")
                    if attr_name.starts_with("on") && attr_name.len() > 2 {
                        // Get span for the attribute
                        let span = create_span_from_node(&tag_child);

                        // Create element node as the caller
                        let element_name =
                            format!("html::element::{}", tag_name.unwrap_or("unknown"));
                        let element_id = helper.add_node(
                            &element_name,
                            Some(span),
                            sqry_core::graph::unified::node::NodeKind::CallSite,
                        );

                        // Extract function calls from the handler value
                        let function_calls = extract_function_calls_from_handler(&attr_value);

                        for func_name in function_calls {
                            // Create function node for the handler
                            let handler_id = helper.add_function(&func_name, None, false, false);

                            // Emit Calls edge from element to handler
                            // Use add_call_edge which uses default metadata (255 for unknown arg count)
                            helper.add_call_edge_with_span(element_id, handler_id, vec![span]);
                        }
                    }
                }
            }
        }
    }
}

/// Extract function call names from an event handler attribute value.
///
/// Parses the JavaScript code in the handler string and extracts function names
/// that are being called. Handles various patterns:
///
/// - Simple calls: `handleClick()` → `["handleClick"]`
/// - Method calls: `obj.method()` → `["obj.method"]`
/// - Multiple calls: `func1(); func2()` → `["func1", "func2"]`
/// - With arguments: `doSomething(event)` → `["doSomething"]`
/// - Chained: `this.classList.toggle('active')` → `["this.classList.toggle"]`
///
/// Filters out JavaScript keywords like `if`, `for`, `while`, etc.
pub fn extract_function_calls_from_handler(handler: &str) -> Vec<String> {
    let mut calls = Vec::new();
    let mut in_string = false;
    let mut string_char = ' ';
    let mut current_identifier = String::new();

    for c in handler.chars() {
        // Handle string literals - skip their contents
        if (c == '"' || c == '\'' || c == '`') && !in_string {
            in_string = true;
            string_char = c;
            current_identifier.clear();
            continue;
        }
        if in_string {
            if c == string_char {
                in_string = false;
            }
            continue;
        }

        // Build identifier (including dots for method calls)
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            current_identifier.push(c);
        } else if c == '.' && !current_identifier.is_empty() {
            // Continue building for method calls like `obj.method` or `this.classList.toggle`
            current_identifier.push(c);
        } else if c == '(' && !current_identifier.is_empty() {
            // Found a function call - extract the function name
            let func_name = current_identifier.trim_end_matches('.').to_string();

            // Check if it's not a keyword
            // For dotted names, check if the last segment is a keyword
            let last_segment = func_name.rsplit('.').next().unwrap_or(&func_name);
            if !is_js_keyword(last_segment) && !func_name.is_empty() {
                calls.push(func_name);
            }
            current_identifier.clear();
        } else {
            // Not part of an identifier - reset
            current_identifier.clear();
        }
    }

    calls
}

/// Check if a string is a JavaScript keyword.
#[inline]
fn is_js_keyword(s: &str) -> bool {
    JS_KEYWORDS.contains(&s)
}

/// Create a Span from a tree-sitter Node.
fn create_span_from_node(node: &Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start: Position {
            line: start.row,
            column: start.column,
        },
        end: Position {
            line: end.row,
            column: end.column,
        },
    }
}

/// Extract resources from a self-closing tag (img, link, meta, etc.).
fn extract_self_closing_resources(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) {
    let tag_name = get_self_closing_tag_name(node, content);
    let attributes = collect_self_closing_attributes(node, content);

    match tag_name.as_deref() {
        Some("link") => {
            if let Some(href) = attributes.get("href")
                && !href.starts_with("data:")
                && !href.starts_with('#')
            {
                let import_id = helper.add_verbatim_import(href, None);
                helper.add_import_edge(module_id, import_id);
            }
        }
        Some("img" | "source" | "audio" | "video") => {
            if let Some(src) = attributes.get("src")
                && !src.starts_with("data:")
            {
                let asset_id = helper.add_verbatim_variable(src, None);
                helper.add_reference_edge(module_id, asset_id);
            }
        }
        Some("a") => {
            if let Some(href) = attributes.get("href")
                && !href.starts_with('#')
                && !href.starts_with("javascript:")
                && !href.starts_with("mailto:")
            {
                let link_id = helper.add_verbatim_import(href, None);
                helper.add_import_edge(module_id, link_id);
            }
        }
        Some("iframe") => {
            if let Some(src) = attributes.get("src")
                && !src.starts_with("about:")
            {
                let frame_id = helper.add_verbatim_import(src, None);
                helper.add_import_edge(module_id, frame_id);
            }
        }
        _ => {}
    }
}

/// Extract resources from script elements
fn extract_script_resources(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) {
    let attributes = collect_script_attributes(node, content);

    if let Some(src) = attributes.get("src")
        && !src.starts_with("data:")
    {
        let script_id = helper.add_verbatim_import(src, None);
        helper.add_import_edge(module_id, script_id);
    }
}

/// Get tag name from an element node
fn get_element_tag_name(node: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            let mut tag_cursor = child.walk();
            for tag_child in child.children(&mut tag_cursor) {
                if tag_child.kind() == "tag_name" {
                    return tag_child
                        .utf8_text(content)
                        .ok()
                        .map(std::string::ToString::to_string);
                }
            }
        }
    }
    None
}

fn get_self_closing_tag_name(node: &Node, content: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "tag_name" {
            return child
                .utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string);
        }
    }
    None
}

/// Collect attributes from an element
fn collect_element_attributes(
    node: &Node,
    content: &[u8],
) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            let mut tag_cursor = child.walk();
            for tag_child in child.children(&mut tag_cursor) {
                if tag_child.kind() == "attribute"
                    && let Some((name, value)) = parse_html_attribute(&tag_child, content)
                {
                    attrs.insert(name, value);
                }
            }
        }
    }

    attrs
}

fn collect_self_closing_attributes(
    node: &Node,
    content: &[u8],
) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute"
            && let Some((name, value)) = parse_html_attribute(&child, content)
        {
            attrs.insert(name, value);
        }
    }

    attrs
}

/// Collect attributes from a script element
fn collect_script_attributes(
    node: &Node,
    content: &[u8],
) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            let mut tag_cursor = child.walk();
            for tag_child in child.children(&mut tag_cursor) {
                if tag_child.kind() == "attribute"
                    && let Some((name, value)) = parse_html_attribute(&tag_child, content)
                {
                    attrs.insert(name, value);
                }
            }
        }
    }

    attrs
}

/// Parse an HTML attribute into (name, value)
fn parse_html_attribute(node: &Node, content: &[u8]) -> Option<(String, String)> {
    let mut name = None;
    let mut value = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "attribute_name" => {
                name = child
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
            "attribute_value" | "quoted_attribute_value" => {
                if let Ok(text) = child.utf8_text(content) {
                    value = Some(text.trim_matches(|c| c == '"' || c == '\'').to_string());
                }
            }
            _ => {}
        }
    }

    name.map(|n| (n, value.unwrap_or_default()))
}

// ============================================================================
// DSL Node Extraction (Elements and Attributes)
// ============================================================================

/// Extract HTML DSL nodes (elements and attributes) from the AST.
///
/// Creates:
/// - Element nodes (`NodeKind::CallSite`) for each HTML element
/// - Attribute nodes (`NodeKind::Variable`) for each attribute
/// - Contains edges: module → element, element → attribute
fn extract_html_dsl_nodes(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "element" => {
                extract_html_element(&child, content, helper, module_id)?;
                // Recurse into element children to find nested elements
                extract_html_dsl_nodes(&child, content, helper, module_id)?;
            }
            "self_closing_tag" => {
                extract_html_self_closing_element(&child, content, helper, module_id)?;
            }
            "script_element" => {
                extract_html_script_element(&child, content, helper, module_id)?;
            }
            _ => {
                // Recurse into other node types
                extract_html_dsl_nodes(&child, content, helper, module_id)?;
            }
        }
    }

    Ok(())
}

/// Extract a regular HTML element and its attributes.
fn extract_html_element(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Get tag name from start_tag
    let tag_name = get_element_tag_name(node, content);

    if let Some(tag) = tag_name {
        // Create element node with position-based uniqueness
        let span = create_span_from_node(node);
        let element_name = format!(
            "html::element::{}@{}:{}",
            tag, span.start.line, span.start.column
        );
        let element_id = helper.add_node(
            &element_name,
            Some(span),
            sqry_core::graph::unified::node::NodeKind::CallSite,
        );

        // Add Contains edge from module to element
        helper.add_contains_edge(module_id, element_id);

        // Extract attributes from the element
        extract_attributes_for_element(node, content, helper, element_id)?;
    }

    Ok(())
}

/// Extract a self-closing HTML element and its attributes.
fn extract_html_self_closing_element(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Get tag name
    let tag_name = get_self_closing_tag_name(node, content);

    if let Some(tag) = tag_name {
        // Create element node with position-based uniqueness
        let span = create_span_from_node(node);
        let element_name = format!(
            "html::element::{}@{}:{}",
            tag, span.start.line, span.start.column
        );
        let element_id = helper.add_node(
            &element_name,
            Some(span),
            sqry_core::graph::unified::node::NodeKind::CallSite,
        );

        // Add Contains edge from module to element
        helper.add_contains_edge(module_id, element_id);

        // Extract attributes
        extract_attributes_for_self_closing(node, content, helper, element_id)?;
    }

    Ok(())
}

/// Extract a script element and its attributes.
fn extract_html_script_element(
    node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    module_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    // Create element node for script
    let span = create_span_from_node(node);
    let element_name = format!(
        "html::element::script@{}:{}",
        span.start.line, span.start.column
    );
    let element_id = helper.add_node(
        &element_name,
        Some(span),
        sqry_core::graph::unified::node::NodeKind::CallSite,
    );

    // Add Contains edge from module to element
    helper.add_contains_edge(module_id, element_id);

    // Extract attributes from start_tag
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            extract_attributes_from_tag(&child, content, helper, element_id)?;
        }
    }

    Ok(())
}

/// Extract attributes from a regular element's `start_tag`.
fn extract_attributes_for_element(
    element_node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    element_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = element_node.walk();
    for child in element_node.children(&mut cursor) {
        if child.kind() == "start_tag" {
            extract_attributes_from_tag(&child, content, helper, element_id)?;
        }
    }
    Ok(())
}

/// Extract attributes from a self-closing element.
fn extract_attributes_for_self_closing(
    element_node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    element_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = element_node.walk();
    for child in element_node.children(&mut cursor) {
        if child.kind() == "attribute" {
            extract_single_attribute(&child, content, helper, element_id)?;
        }
    }
    Ok(())
}

/// Extract attributes from a tag node (`start_tag`).
fn extract_attributes_from_tag(
    tag_node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    element_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    let mut cursor = tag_node.walk();
    for child in tag_node.children(&mut cursor) {
        if child.kind() == "attribute" {
            extract_single_attribute(&child, content, helper, element_id)?;
        }
    }
    Ok(())
}

/// Extract a single attribute and create an Attribute node.
#[allow(clippy::unnecessary_wraps)]
fn extract_single_attribute(
    attr_node: &Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    element_id: sqry_core::graph::unified::NodeId,
) -> GraphResult<()> {
    if let Some((name, value)) = parse_html_attribute(attr_node, content) {
        let span = create_span_from_node(attr_node);
        let attr_name = if value.is_empty() {
            format!("html::attribute::{name}")
        } else {
            format!("html::attribute::{name}={value}")
        };
        let attr_id = helper.add_variable(&attr_name, Some(span));

        // Add Contains edge from element to attribute
        helper.add_contains_edge(element_id, attr_id);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use std::path::PathBuf;

    fn parse_html(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_html::LANGUAGE.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    /// Helper to count Calls edges in staging operations
    fn count_calls_edges(staging: &StagingGraph) -> usize {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Helper to get all Calls edges from staging operations
    fn get_calls_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
        staging
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    StagingOp::AddEdge {
                        kind: EdgeKind::Calls { .. },
                        ..
                    }
                )
            })
            .collect()
    }

    fn has_node_canonical_name(staging: &StagingGraph, kind: NodeKind, expected: &str) -> bool {
        staging.operations().iter().any(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                entry.kind == kind && staging.resolve_node_canonical_name(entry) == Some(expected)
            } else {
                false
            }
        })
    }

    // ========================================================================
    // extract_function_calls_from_handler unit tests
    // ========================================================================

    #[test]
    fn test_parser_simple_function_call() {
        let calls = extract_function_calls_from_handler("handleClick()");
        assert_eq!(calls, vec!["handleClick"]);
    }

    #[test]
    fn test_parser_multiple_function_calls() {
        let calls = extract_function_calls_from_handler("func1(); func2()");
        assert_eq!(calls, vec!["func1", "func2"]);
    }

    #[test]
    fn test_parser_method_call() {
        let calls = extract_function_calls_from_handler("obj.method()");
        assert_eq!(calls, vec!["obj.method"]);
    }

    #[test]
    fn test_parser_chained_method_call() {
        let calls = extract_function_calls_from_handler("this.classList.toggle('active')");
        assert_eq!(calls, vec!["this.classList.toggle"]);
    }

    #[test]
    fn test_parser_function_with_arguments() {
        let calls = extract_function_calls_from_handler("doSomething(event, 123)");
        assert_eq!(calls, vec!["doSomething"]);
    }

    #[test]
    fn test_parser_skips_keywords() {
        // 'if' is a keyword and should be skipped
        let calls = extract_function_calls_from_handler("if(condition) { doAction() }");
        // 'if' should not be in the list, only 'doAction'
        assert!(!calls.contains(&"if".to_string()));
        assert!(calls.contains(&"doAction".to_string()));
    }

    #[test]
    fn test_parser_skips_string_content() {
        // Function calls inside strings should be ignored
        let calls = extract_function_calls_from_handler("alert('hello()')");
        assert_eq!(calls, vec!["alert"]);
    }

    #[test]
    fn test_parser_return_statement() {
        let calls = extract_function_calls_from_handler("return validate()");
        // 'return' is a keyword, 'validate' is a function
        assert!(!calls.contains(&"return".to_string()));
        assert!(calls.contains(&"validate".to_string()));
    }

    #[test]
    fn test_parser_console_log() {
        let calls = extract_function_calls_from_handler("console.log('test')");
        assert_eq!(calls, vec!["console.log"]);
    }

    #[test]
    fn test_parser_empty_handler() {
        let calls = extract_function_calls_from_handler("");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parser_no_function_calls() {
        let calls = extract_function_calls_from_handler("x = 5; y = 10");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parser_complex_handler() {
        let calls = extract_function_calls_from_handler(
            "event.preventDefault(); validate() && submit(); console.log('done')",
        );
        assert!(calls.contains(&"event.preventDefault".to_string()));
        assert!(calls.contains(&"validate".to_string()));
        assert!(calls.contains(&"submit".to_string()));
        assert!(calls.contains(&"console.log".to_string()));
    }

    // ========================================================================
    // GraphBuilder integration tests for event handlers
    // ========================================================================

    #[test]
    fn test_onclick_emits_calls_edge() {
        let source = r#"<button onclick="handleClick()">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "onclick should emit 1 Calls edge");
    }

    #[test]
    fn test_onsubmit_emits_calls_edge() {
        let source = r#"<form onsubmit="validateForm()"><input type="submit"></form>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "onsubmit should emit 1 Calls edge");
    }

    #[test]
    fn test_onchange_emits_calls_edge() {
        let source = r#"<input type="text" onchange="updateValue()">"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "onchange should emit 1 Calls edge");
    }

    #[test]
    fn test_multiple_event_handlers_on_different_elements() {
        let source = r#"
<button onclick="handleClick()">Click</button>
<form onsubmit="validateForm()">
    <input onchange="updateValue()">
</form>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 3, "Should emit 3 Calls edges for 3 handlers");
    }

    #[test]
    fn test_multiple_calls_in_single_handler() {
        let source = r#"<button onclick="func1(); func2()">Multi</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 2,
            "Should emit 2 Calls edges for 2 function calls"
        );
    }

    #[test]
    fn test_method_call_in_handler() {
        let source = r#"<button onclick="console.log('test')">Log</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "Method call should emit 1 Calls edge");
    }

    #[test]
    fn test_chained_method_call_in_handler() {
        let source = r#"<button onclick="this.classList.toggle('active')">Toggle</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 1,
            "Chained method call should emit 1 Calls edge"
        );
    }

    #[test]
    fn test_various_event_types() {
        let source = r#"
<div
    onmouseover="hover()"
    onmouseout="unhover()"
    onmousedown="mousedown()"
    onmouseup="mouseup()"
    onkeydown="keydown()"
    onkeyup="keyup()"
    onfocus="focus()"
    onblur="blur()"
>Events</div>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 8,
            "Should emit 8 Calls edges for 8 event handlers"
        );
    }

    #[test]
    fn test_handler_with_event_argument() {
        let source = r#"<button onclick="handleClick(event)">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 1,
            "Handler with argument should emit 1 Calls edge"
        );
    }

    #[test]
    fn test_handler_with_keywords_filtered() {
        let source = r#"<button onclick="if(confirm()) { doAction() }">Confirm</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have 'confirm' and 'doAction', but NOT 'if'
        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 2,
            "Should emit 2 Calls edges (confirm and doAction, not 'if')"
        );
    }

    #[test]
    fn test_empty_handler_no_edges() {
        let source = r#"<button onclick="">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 0, "Empty handler should emit 0 Calls edges");
    }

    #[test]
    fn test_non_event_attributes_ignored() {
        let source = r#"<button class="btn" id="submit" data-action="click">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(
            calls_count, 0,
            "Non-event attributes should not emit Calls edges"
        );
    }

    #[test]
    fn test_calls_edge_has_correct_metadata() {
        let source = r#"<button onclick="handleClick()">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_edges = get_calls_edges(&staging);
        assert_eq!(calls_edges.len(), 1);

        // Verify the edge has Calls kind with expected metadata
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::Calls {
                    argument_count,
                    is_async,
                },
            ..
        } = calls_edges[0]
        {
            assert_eq!(
                *argument_count, 255,
                "argument_count should be 255 (unknown)"
            );
            assert!(!*is_async, "is_async should be false");
        } else {
            panic!("Expected Calls edge");
        }
    }

    #[test]
    fn test_calls_edge_has_span() {
        let source = r#"<button onclick="handleClick()">Click</button>"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_edges = get_calls_edges(&staging);
        assert_eq!(calls_edges.len(), 1);

        // Verify the edge has span information
        if let StagingOp::AddEdge { spans, .. } = calls_edges[0] {
            assert!(!spans.is_empty(), "Calls edge should have span information");
        } else {
            panic!("Expected Calls edge");
        }
    }

    #[test]
    fn test_oninput_emits_calls_edge() {
        let source = r#"<input type="text" oninput="onInputChange()">"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "oninput should emit 1 Calls edge");
    }

    #[test]
    fn test_onload_emits_calls_edge() {
        let source = r#"<img src="test.jpg" onload="imageLoaded()">"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "onload should emit 1 Calls edge");
    }

    #[test]
    fn test_onerror_emits_calls_edge() {
        let source = r#"<img src="test.jpg" onerror="handleError()">"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let calls_count = count_calls_edges(&staging);
        assert_eq!(calls_count, 1, "onerror should emit 1 Calls edge");
    }

    // ========================================================================
    // Resource extraction tests
    // ========================================================================

    #[test]
    fn test_extracts_script_imports() {
        let source = r#"
<!DOCTYPE html>
<html>
<head>
    <script src="./app.js"></script>
    <script type="module" src="./modules/main.js"></script>
</head>
</html>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify import edges are created for script src attributes
        let ops = staging.operations();
        let import_edges: Vec<_> = ops
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
            .collect();

        assert_eq!(
            import_edges.len(),
            2,
            "Expected 2 Imports edges for script elements"
        );
    }

    #[test]
    fn test_extracts_stylesheet_imports() {
        let source = r#"
<!DOCTYPE html>
<html>
<head>
    <link rel="stylesheet" href="./styles.css">
    <link rel="stylesheet" href="https://cdn.example.com/lib.css">
</head>
</html>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify import edges are created for link href attributes
        let ops = staging.operations();
        let import_edges: Vec<_> = ops
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
            .collect();

        assert_eq!(
            import_edges.len(),
            2,
            "Expected 2 Imports edges for link elements"
        );
    }

    #[test]
    fn test_extracts_anchor_import_with_verbatim_filename() {
        let source = r#"
<!DOCTYPE html>
<html>
<body>
    <a href="styles.css">Stylesheet</a>
</body>
</html>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert!(
            has_node_canonical_name(&staging, NodeKind::Import, "styles.css"),
            "Expected anchor import to preserve styles.css verbatim"
        );
    }

    #[test]
    fn test_extracts_query_string_script_import_verbatim() {
        let source = r#"
<!DOCTYPE html>
<html>
<head>
    <script src="app.js?v=2"></script>
</head>
</html>
"#;

        let tree = parse_html(source);
        let mut staging = StagingGraph::new();
        let builder = HtmlGraphBuilder;
        let file = PathBuf::from("index.html");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        assert!(
            has_node_canonical_name(&staging, NodeKind::Import, "app.js?v=2"),
            "Expected script import to preserve app.js?v=2 verbatim"
        );
    }

    // ── Fast pre-check tests ─────────────────────────────────────

    #[test]
    fn test_html_precheck_valid_html() {
        assert!(html_fast_precheck(b"<html><body></body></html>"));
        assert!(html_fast_precheck(b"<!DOCTYPE html><html>"));
        assert!(html_fast_precheck(b"  \n<div>hello</div>"));
    }

    #[test]
    fn test_html_precheck_rejects_binary() {
        assert!(!html_fast_precheck(b"\x00\x01\x02binary data"));
        assert!(!html_fast_precheck(b"no angle brackets here"));
        assert!(!html_fast_precheck(b""));
    }

    #[test]
    fn test_html_precheck_angle_bracket_within_window() {
        // `<` at position 4000 (within 4096 window) — simulates a large license banner
        let mut content = vec![b' '; 4000];
        content.push(b'<');
        assert!(html_fast_precheck(&content));
    }

    #[test]
    fn test_html_precheck_angle_bracket_beyond_window() {
        // `<` at position 5000 (beyond 4096 window)
        let mut content = vec![b' '; 5000];
        content.push(b'<');
        assert!(!html_fast_precheck(&content));
    }
}
// Nested conditionals retained for clarity when parsing attribute shapes
