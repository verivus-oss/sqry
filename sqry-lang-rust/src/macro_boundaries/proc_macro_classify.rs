//! Proc-macro function classification (4.5c).
//!
//! Classifies proc-macro functions by their attribute signature:
//! - `#[proc_macro]` → `FunctionLike`
//! - `#[proc_macro_derive(Name, attributes(...))]` → `Derive`
//! - `#[proc_macro_attribute]` → `Attribute`
//!
//! Stores classification in [`NodeMetadataStore`] for downstream consumption
//! by search, CLI, and MCP tools.

use sqry_core::graph::unified::{NodeId, NodeMetadataStore, ProcMacroFunctionKind};
use tree_sitter::Node;

/// Classify a function node as a proc-macro if it carries a proc-macro attribute.
///
/// Only functions with one of the three proc-macro attributes are classified:
/// - `#[proc_macro]` — function-like proc-macro
/// - `#[proc_macro_derive(Name, attributes(helper1, helper2))]` — derive macro
/// - `#[proc_macro_attribute]` — attribute macro
///
/// If the function has no proc-macro attribute, this is a no-op.
///
/// # Arguments
///
/// * `func_node` — tree-sitter node for a `function_item`
/// * `content` — source file bytes for extracting text
/// * `func_id` — the graph `NodeId` already assigned to this function
/// * `metadata_store` — sparse metadata store for recording classification
pub fn classify_proc_macro(
    func_node: Node,
    content: &[u8],
    func_id: NodeId,
    metadata_store: &mut NodeMetadataStore,
) {
    debug_assert_eq!(
        func_node.kind(),
        "function_item",
        "classify_proc_macro expects a function_item node"
    );

    // Walk backwards through preceding siblings to find attribute_item nodes.
    let mut sibling = func_node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() != "attribute_item" {
            // Stop at the first non-attribute sibling — attributes are contiguous
            // preceding siblings of the item they annotate. Comments and other
            // whitespace are filtered out by tree-sitter.
            break;
        }

        if let Some(kind) = classify_attribute_node(sib, content, func_id, metadata_store) {
            log::debug!(
                "Classified proc-macro function {:?} as {:?}",
                func_node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(content).ok()),
                kind,
            );
            // A function can only have one proc-macro attribute — once classified, stop.
            return;
        }

        sibling = sib.prev_sibling();
    }
}

/// Inspect a single `attribute_item` node for proc-macro attributes.
///
/// Returns `Some(kind)` if this attribute is a proc-macro attribute and the
/// classification was stored, `None` otherwise.
fn classify_attribute_node(
    attr_node: Node,
    content: &[u8],
    func_id: NodeId,
    metadata_store: &mut NodeMetadataStore,
) -> Option<ProcMacroFunctionKind> {
    // attribute_item → attribute → (meta_item | ...)
    // Structure: `#[` attribute `]`
    // The attribute child is typically at index 1 (between `#[` and `]`).
    let attr_child = find_attribute_content(attr_node)?;
    let attr_text = attr_child.utf8_text(content).ok()?;

    if attr_text == "proc_macro" {
        let meta = metadata_store.get_or_insert_default(func_id);
        meta.proc_macro_kind = Some(ProcMacroFunctionKind::FunctionLike);
        return Some(ProcMacroFunctionKind::FunctionLike);
    }

    if attr_text == "proc_macro_attribute" {
        let meta = metadata_store.get_or_insert_default(func_id);
        meta.proc_macro_kind = Some(ProcMacroFunctionKind::Attribute);
        return Some(ProcMacroFunctionKind::Attribute);
    }

    if attr_text.starts_with("proc_macro_derive") {
        let meta = metadata_store.get_or_insert_default(func_id);
        meta.proc_macro_kind = Some(ProcMacroFunctionKind::Derive);

        // Extract derive name and helper attributes from the token tree.
        // Pattern: proc_macro_derive(Name) or proc_macro_derive(Name, attributes(a, b))
        // The macro_source field stores "DeriveName" for downstream use.
        if let Some(derive_name) = extract_derive_name(attr_text) {
            meta.macro_source = Some(derive_name);
        }

        return Some(ProcMacroFunctionKind::Derive);
    }

    None
}

/// Find the inner attribute content node within an `attribute_item`.
///
/// tree-sitter-rust parses `#[foo]` as:
/// ```text
/// attribute_item
///   "#"
///   "["
///   attribute
///     (content nodes...)
///   "]"
/// ```
///
/// We want the `attribute` child (or `meta_item` depending on grammar version).
fn find_attribute_content(attr_node: Node) -> Option<Node> {
    let mut cursor = attr_node.walk();
    for child in attr_node.children(&mut cursor) {
        let kind = child.kind();
        // tree-sitter-rust uses "attribute" for the inner content of attribute_item
        if kind == "attribute" || kind == "meta_item" {
            return Some(child);
        }
    }
    // Fallback: if the grammar nests differently, try getting text directly
    // from the attribute_item minus the `#[` and `]` delimiters.
    None
}

/// Extract the derive name from a `proc_macro_derive(...)` attribute text.
///
/// Input examples:
/// - `proc_macro_derive(MyDerive)` → `Some("MyDerive")`
/// - `proc_macro_derive(MyDerive, attributes(helper1, helper2))` → `Some("MyDerive")`
/// - `proc_macro_derive()` → `None`
fn extract_derive_name(attr_text: &str) -> Option<String> {
    let after_paren = attr_text.strip_prefix("proc_macro_derive(")?;
    let inner = after_paren.strip_suffix(')')?;

    // The derive name is the first token before any comma or whitespace.
    let name = inner.split([',', ' ']).next()?.trim();
    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

/// Extract helper attributes from a `proc_macro_derive(..., attributes(...))` attribute.
///
/// Input examples:
/// - `proc_macro_derive(MyDerive, attributes(helper1, helper2))` → `["helper1", "helper2"]`
/// - `proc_macro_derive(MyDerive)` → `[]`
///
/// Helper attributes are not stored in metadata currently but this function is
/// available for future use when helper attribute resolution is needed.
#[must_use]
pub fn extract_helper_attributes(attr_text: &str) -> Vec<String> {
    let Some(after_paren) = attr_text.strip_prefix("proc_macro_derive(") else {
        return Vec::new();
    };
    let Some(inner) = after_paren.strip_suffix(')') else {
        return Vec::new();
    };

    // Find "attributes(" section
    let Some(attr_start) = inner.find("attributes(") else {
        return Vec::new();
    };
    let attrs_content = &inner[attr_start + "attributes(".len()..];
    let Some(end) = attrs_content.find(')') else {
        return Vec::new();
    };
    let attrs_str = &attrs_content[..end];

    attrs_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::NodeId;
    use tree_sitter::Parser;

    fn parse_rust(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    fn find_function_item(tree: &tree_sitter::Tree) -> Option<Node<'_>> {
        find_node_by_kind(tree.root_node(), "function_item")
    }

    fn find_node_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node_by_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_derive_macro_classified() {
        let source = r"
#[proc_macro_derive(MyDerive)]
pub fn my_derive(input: TokenStream) -> TokenStream {
    input
}
";
        let tree = parse_rust(source);
        let func = find_function_item(&tree).unwrap();
        let func_id = NodeId::new(1, 0);
        let mut store = NodeMetadataStore::new();

        classify_proc_macro(func, source.as_bytes(), func_id, &mut store);

        let meta = store.get(func_id).expect("metadata should be stored");
        assert_eq!(meta.proc_macro_kind, Some(ProcMacroFunctionKind::Derive));
        assert_eq!(meta.macro_source.as_deref(), Some("MyDerive"));
    }

    #[test]
    fn test_attribute_macro_classified() {
        let source = r"
#[proc_macro_attribute]
pub fn my_attr(attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
";
        let tree = parse_rust(source);
        let func = find_function_item(&tree).unwrap();
        let func_id = NodeId::new(2, 0);
        let mut store = NodeMetadataStore::new();

        classify_proc_macro(func, source.as_bytes(), func_id, &mut store);

        let meta = store.get(func_id).expect("metadata should be stored");
        assert_eq!(meta.proc_macro_kind, Some(ProcMacroFunctionKind::Attribute));
    }

    #[test]
    fn test_function_macro_classified() {
        let source = r"
#[proc_macro]
pub fn my_macro(input: TokenStream) -> TokenStream {
    input
}
";
        let tree = parse_rust(source);
        let func = find_function_item(&tree).unwrap();
        let func_id = NodeId::new(3, 0);
        let mut store = NodeMetadataStore::new();

        classify_proc_macro(func, source.as_bytes(), func_id, &mut store);

        let meta = store.get(func_id).expect("metadata should be stored");
        assert_eq!(
            meta.proc_macro_kind,
            Some(ProcMacroFunctionKind::FunctionLike)
        );
    }

    #[test]
    fn test_helper_attrs_extracted() {
        let text = "proc_macro_derive(MyDerive, attributes(helper1, helper2))";
        let helpers = extract_helper_attributes(text);
        assert_eq!(helpers, vec!["helper1", "helper2"]);
    }

    #[test]
    fn test_non_proc_macro_fn_ignored() {
        let source = r"
pub fn regular_function() -> u32 {
    42
}
";
        let tree = parse_rust(source);
        let func = find_function_item(&tree).unwrap();
        let func_id = NodeId::new(4, 0);
        let mut store = NodeMetadataStore::new();

        classify_proc_macro(func, source.as_bytes(), func_id, &mut store);

        assert!(store.is_empty(), "no metadata for non-proc-macro function");
    }

    #[test]
    fn test_extract_derive_name_simple() {
        assert_eq!(
            extract_derive_name("proc_macro_derive(Debug)"),
            Some("Debug".to_string())
        );
    }

    #[test]
    fn test_extract_derive_name_with_helpers() {
        assert_eq!(
            extract_derive_name("proc_macro_derive(Serialize, attributes(serde))"),
            Some("Serialize".to_string())
        );
    }

    #[test]
    fn test_extract_derive_name_empty() {
        assert_eq!(extract_derive_name("proc_macro_derive()"), None);
    }

    #[test]
    fn test_extract_helper_attributes_none() {
        let helpers = extract_helper_attributes("proc_macro_derive(MyDerive)");
        assert!(helpers.is_empty());
    }

    #[test]
    fn test_extract_helper_attributes_not_derive() {
        let helpers = extract_helper_attributes("proc_macro_attribute");
        assert!(helpers.is_empty());
    }
}
