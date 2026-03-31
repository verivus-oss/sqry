//! Attribute macro detection (4.5a).
//!
//! Detects attribute macros on items using 3-tier positive identification:
//!
//! 1. **Same-workspace proc-macros** — attribute path resolves to a function in
//!    a proc-macro crate within the workspace. Emits `MacroExpansion{Attribute}`
//!    with `is_verified: true`. (Resolved in cross-crate pass, not here.)
//!
//! 2. **Well-known proc-macro registry** — curated list of confirmed
//!    `#[proc_macro_attribute]` exports. Emits `MacroExpansion{Attribute}` with
//!    `is_verified: false` and confidence limitation.
//!
//! 3. **Unknown attributes** — no `MacroExpansion` edges. Recorded in
//!    `unresolved_attributes` metadata for potential future resolution.
//!
//! # Skipped Attributes
//!
//! - `#[derive()]` — already handled in existing Pass 2
//! - Built-in compiler attributes — not proc-macros
//! - Inner attributes (`#![...]`) — not item-level
//! - `#[cfg()]` / `#[cfg_attr()]` — handled by cfg_analysis (4.5e)
//! - `#[test]`, `#[bench]`, etc. — built-in compiler attributes

use sqry_core::graph::Span;
use sqry_core::graph::node::Position;
use sqry_core::graph::unified::{
    GraphBuildHelper, MacroExpansionKind, NodeId, NodeKind, NodeMetadataStore,
};
use tree_sitter::Node;

/// A well-known proc-macro attribute from the ecosystem registry.
///
/// Each entry is version-pinned to the crate version range where the attribute
/// is known to exist as a proc-macro.
#[derive(Debug, Clone, Copy)]
pub struct WellKnownProcMacroAttr {
    /// Full attribute path (e.g., `"tokio::main"`).
    pub path: &'static str,
    /// Source crate that provides this attribute.
    pub source_crate: &'static str,
    /// Minimum version where this attribute exists as a proc-macro.
    /// `None` means all versions.
    pub min_version: Option<&'static str>,
}

/// Curated registry of well-known proc-macro attributes.
///
/// This list contains confirmed `#[proc_macro_attribute]` exports from widely-used
/// crates. Each entry has been verified to exist as a proc-macro attribute (not a
/// derive macro or function-like macro).
pub const WELL_KNOWN_PROC_MACRO_ATTRS: &[WellKnownProcMacroAttr] = &[
    // tokio
    WellKnownProcMacroAttr {
        path: "tokio::main",
        source_crate: "tokio-macros",
        min_version: Some("0.2"),
    },
    WellKnownProcMacroAttr {
        path: "tokio::test",
        source_crate: "tokio-macros",
        min_version: Some("0.2"),
    },
    // async-trait
    WellKnownProcMacroAttr {
        path: "async_trait::async_trait",
        source_crate: "async-trait",
        min_version: None,
    },
    // tracing
    WellKnownProcMacroAttr {
        path: "tracing::instrument",
        source_crate: "tracing-attributes",
        min_version: None,
    },
    // sqlx
    WellKnownProcMacroAttr {
        path: "sqlx::test",
        source_crate: "sqlx-macros",
        min_version: Some("0.6"),
    },
    // Rocket
    WellKnownProcMacroAttr {
        path: "rocket::get",
        source_crate: "rocket_codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "rocket::post",
        source_crate: "rocket_codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "rocket::put",
        source_crate: "rocket_codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "rocket::delete",
        source_crate: "rocket_codegen",
        min_version: None,
    },
    // Actix-web
    WellKnownProcMacroAttr {
        path: "actix_web::get",
        source_crate: "actix-web-codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "actix_web::post",
        source_crate: "actix-web-codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "actix_web::put",
        source_crate: "actix-web-codegen",
        min_version: None,
    },
    WellKnownProcMacroAttr {
        path: "actix_web::delete",
        source_crate: "actix-web-codegen",
        min_version: None,
    },
    // axum (via axum-macros)
    WellKnownProcMacroAttr {
        path: "axum::debug_handler",
        source_crate: "axum-macros",
        min_version: None,
    },
    // NOTE: serde::Serialize, serde::Deserialize, clap::Parser, thiserror::Error
    // are DERIVE macros, not attribute macros. They are already handled by the
    // existing derive attribute processing in graph_builder.rs. Do NOT add them
    // here — this registry is exclusively for #[proc_macro_attribute] exports.
];

/// Built-in compiler attributes that should NOT be treated as proc-macros.
///
/// These are intrinsic to rustc and are handled directly by the compiler.
const BUILTIN_COMPILER_ATTRS: &[&str] = &[
    "allow",
    "warn",
    "deny",
    "forbid",
    "deprecated",
    "inline",
    "cold",
    "hot",
    "repr",
    "must_use",
    "no_mangle",
    "link",
    "link_name",
    "link_section",
    "doc",
    "path",
    "recursion_limit",
    "windows_subsystem",
    "global_allocator",
    "track_caller",
    "non_exhaustive",
    "automatically_derived",
    // cfg/cfg_attr handled by cfg_analysis (4.5e)
    "cfg",
    "cfg_attr",
    // Testing attributes are built-in compiler attributes
    "test",
    "bench",
    "ignore",
    "should_panic",
    // derive is handled separately in existing Pass 2
    "derive",
];

/// Item types that can be annotated with attributes.
const ATTRIBUTABLE_ITEM_KINDS: &[&str] = &[
    "function_item",
    "struct_item",
    "enum_item",
    "impl_item",
    "mod_item",
    "trait_item",
    "type_item",
    "union_item",
    "const_item",
    "static_item",
    "use_declaration",
    "extern_crate_declaration",
    "foreign_mod_item",
    "let_declaration",
    "expression_statement",
];

/// Detect attribute macros on a given item node.
///
/// Walks backwards through `attribute_item` siblings of the item, skipping
/// `#[derive()]` and built-in compiler attributes. For each remaining attribute:
///
/// 1. Check the well-known proc-macro registry → emit `CallSite` + `Calls` +
///    `MacroExpansion{Attribute}` with `is_verified: false`
/// 2. Otherwise → record in `unresolved_attributes` metadata
///
/// # Arguments
///
/// * `item_node` — tree-sitter node for an attributable item
/// * `content` — source file bytes
/// * `item_qualified` — qualified name of the annotated item
/// * `item_id` — the graph `NodeId` already assigned to this item
/// * `helper` — graph build helper for creating nodes and edges
/// * `metadata_store` — sparse metadata store for recording unresolved attributes
pub fn detect_attribute_macros(
    item_node: Node,
    content: &[u8],
    item_qualified: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
) {
    // Only process attributable item types.
    if !ATTRIBUTABLE_ITEM_KINDS.contains(&item_node.kind()) {
        return;
    }

    // Walk backwards through preceding siblings to find attribute_item nodes.
    let mut sibling = item_node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() != "attribute_item" {
            break;
        }

        // Skip inner attributes (#![...]) — they annotate the enclosing scope, not the item.
        if is_inner_attribute(sib, content) {
            sibling = sib.prev_sibling();
            continue;
        }

        if let Some(attr_path) = extract_attribute_path(sib, content) {
            process_attribute(
                &attr_path,
                sib,
                content,
                item_qualified,
                item_id,
                helper,
                metadata_store,
            );
        }

        sibling = sib.prev_sibling();
    }
}

/// Check if an `attribute_item` is an inner attribute (`#![...]`).
fn is_inner_attribute(attr_node: Node, content: &[u8]) -> bool {
    let text = attr_node.utf8_text(content).unwrap_or("");
    text.starts_with("#!")
}

/// Extract the attribute path from an `attribute_item` node.
///
/// For `#[tokio::main]`, returns `"tokio::main"`.
/// For `#[derive(Debug)]`, returns `"derive"`.
/// For `#[allow(unused)]`, returns `"allow"`.
fn extract_attribute_path(attr_node: Node, content: &[u8]) -> Option<String> {
    // Find the attribute content child.
    let mut cursor = attr_node.walk();
    for child in attr_node.children(&mut cursor) {
        if child.kind() == "attribute" {
            return extract_path_from_attribute(child, content);
        }
    }
    // Fallback: try to extract from the raw text.
    let text = attr_node.utf8_text(content).ok()?;
    let trimmed = text.trim_start_matches("#[").trim_end_matches(']');
    // Take everything before the first `(` or end.
    let path = trimmed.split('(').next()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Extract the path from an `attribute` child node.
///
/// The `attribute` node in tree-sitter-rust may contain:
/// - A simple identifier: `inline`
/// - A scoped path: `tokio::main`
/// - A path with arguments: `derive(Debug, Clone)`
fn extract_path_from_attribute(attr: Node, content: &[u8]) -> Option<String> {
    let mut cursor = attr.walk();
    for child in attr.children(&mut cursor) {
        match child.kind() {
            // Simple identifier or path
            "identifier" | "scoped_identifier" | "crate" => {
                return child.utf8_text(content).ok().map(|s| s.to_string());
            }
            // Meta item with nested content
            "meta_item" => {
                // First child of meta_item is typically the path
                if let Some(path_child) = child.child(0) {
                    return path_child.utf8_text(content).ok().map(|s| s.to_string());
                }
            }
            _ => {}
        }
    }
    // Fallback: use the entire attribute text before any parentheses
    let text = attr.utf8_text(content).ok()?;
    let path = text.split('(').next()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Process a single attribute, classifying it against the registry or recording it
/// as unresolved.
fn process_attribute(
    attr_path: &str,
    attr_node: Node,
    content: &[u8],
    item_qualified: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
    metadata_store: &mut NodeMetadataStore,
) {
    // Extract the base attribute name (first path segment) for builtin check.
    let base_name = attr_path.split("::").next().unwrap_or(attr_path);

    // Skip built-in compiler attributes.
    if BUILTIN_COMPILER_ATTRS.contains(&base_name) {
        return;
    }

    // Check against well-known proc-macro registry.
    if let Some(well_known) = lookup_well_known(attr_path) {
        emit_attribute_macro_edges(
            attr_node,
            content,
            attr_path,
            well_known.source_crate,
            item_qualified,
            item_id,
            helper,
            false, // is_verified: false for registry matches
        );
        log::debug!(
            "Well-known attribute macro '{}' (from {}) on '{}'",
            attr_path,
            well_known.source_crate,
            item_qualified
        );
        return;
    }

    // Unknown attribute — record in metadata for potential future resolution.
    let meta = metadata_store.get_or_insert_default(item_id);
    if !meta.unresolved_attributes.contains(&attr_path.to_string()) {
        meta.unresolved_attributes.push(attr_path.to_string());
    }
    log::debug!(
        "Unresolved attribute '{}' on '{}' — recorded in metadata",
        attr_path,
        item_qualified
    );
}

/// Look up an attribute path in the well-known proc-macro registry.
fn lookup_well_known(attr_path: &str) -> Option<&'static WellKnownProcMacroAttr> {
    WELL_KNOWN_PROC_MACRO_ATTRS
        .iter()
        .find(|entry| entry.path == attr_path)
}

/// Emit graph edges for a detected attribute macro.
///
/// Creates a `CallSite` node for the invocation, a `Macro` node for the target,
/// a `Calls` edge between them, and a `MacroExpansion{Attribute}` edge from the
/// item to the macro.
fn emit_attribute_macro_edges(
    attr_node: Node,
    _content: &[u8],
    attr_path: &str,
    _source_crate: &str,
    item_qualified: &str,
    item_id: NodeId,
    helper: &mut GraphBuildHelper,
    is_verified: bool,
) {
    let span = Span::new(
        Position::new(
            attr_node.start_position().row,
            attr_node.start_position().column,
        ),
        Position::new(
            attr_node.end_position().row,
            attr_node.end_position().column,
        ),
    );

    // Create a CallSite node for the attribute invocation.
    // Naming convention: `item_name::attr_<attr_path>@line:col`
    let callsite_qualified = format!(
        "{}::attr_{}@{}:{}",
        item_qualified,
        attr_path.replace("::", "_"),
        attr_node.start_position().row + 1,
        attr_node.start_position().column
    );
    let callsite_id = helper.add_node(&callsite_qualified, Some(span), NodeKind::CallSite);

    // Create a Macro node for the target attribute macro.
    let macro_qualified = attr_path.to_string();
    let macro_id = helper.add_node(&macro_qualified, None, NodeKind::Macro);

    // Calls edge from CallSite to Macro.
    helper.add_call_edge(callsite_id, macro_id);

    // MacroExpansion{Attribute} edge from item to macro.
    helper.add_macro_expansion_edge(
        item_id,
        macro_id,
        MacroExpansionKind::Attribute,
        is_verified,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::Language;
    use sqry_core::graph::unified::{NodeKind, StagingGraph};
    use std::path::Path;
    use tree_sitter::Parser;

    fn parse_rust(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
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
    fn test_tokio_main_detected() {
        let source = r#"
#[tokio::main]
async fn main() {
    println!("hello");
}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("main", None, NodeKind::Function);

        detect_attribute_macros(
            func,
            source.as_bytes(),
            "main",
            func_id,
            &mut helper,
            &mut metadata_store,
        );

        // Should have created CallSite and Macro nodes.
        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        let macro_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Macro)
            .count();

        assert_eq!(callsite_count, 1, "Expected 1 CallSite node");
        assert_eq!(macro_count, 1, "Expected 1 Macro node");

        // No unresolved attributes — tokio::main is in the well-known registry.
        assert!(
            metadata_store.is_empty(),
            "tokio::main should not be unresolved"
        );
    }

    #[test]
    fn test_builtin_attrs_skipped() {
        let source = r#"
#[inline]
#[must_use]
pub fn fast() -> u32 { 42 }
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("fast", None, NodeKind::Function);

        detect_attribute_macros(
            func,
            source.as_bytes(),
            "fast",
            func_id,
            &mut helper,
            &mut metadata_store,
        );

        // No CallSite or Macro nodes should be created for built-in attrs.
        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        assert_eq!(
            callsite_count, 0,
            "Built-in attrs should not create CallSite nodes"
        );
        assert!(
            metadata_store.is_empty(),
            "Built-in attrs should not be recorded"
        );
    }

    #[test]
    fn test_derive_not_duplicated() {
        let source = r#"
#[derive(Debug, Clone)]
struct Foo { x: u32 }
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let struct_node = find_node_by_kind(tree.root_node(), "struct_item").unwrap();
        let struct_id = helper.add_node("Foo", None, NodeKind::Struct);

        detect_attribute_macros(
            struct_node,
            source.as_bytes(),
            "Foo",
            struct_id,
            &mut helper,
            &mut metadata_store,
        );

        // derive is in BUILTIN_COMPILER_ATTRS, so no edges.
        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        assert_eq!(
            callsite_count, 0,
            "derive should not create attribute macro edges"
        );
    }

    #[test]
    fn test_unknown_attr_recorded() {
        let source = r#"
#[my_custom_attr]
fn decorated() {}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("decorated", None, NodeKind::Function);

        detect_attribute_macros(
            func,
            source.as_bytes(),
            "decorated",
            func_id,
            &mut helper,
            &mut metadata_store,
        );

        // Should not create edges, but should record in unresolved_attributes.
        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        assert_eq!(callsite_count, 0, "Unknown attrs should not create edges");

        let meta = metadata_store.get(func_id).expect("metadata should exist");
        assert!(
            meta.unresolved_attributes
                .contains(&"my_custom_attr".to_string()),
            "Unknown attr should be recorded"
        );
    }

    #[test]
    fn test_inner_attr_skipped() {
        // Inner attributes are not item-level attributes
        let source = r#"
#![allow(unused)]
fn main() {}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("main", None, NodeKind::Function);

        detect_attribute_macros(
            func,
            source.as_bytes(),
            "main",
            func_id,
            &mut helper,
            &mut metadata_store,
        );

        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        assert_eq!(callsite_count, 0, "Inner attrs should be skipped");
    }

    #[test]
    fn test_multiple_attrs_on_item() {
        let source = r#"
#[tokio::main]
#[tracing::instrument]
async fn serve() {}
"#;
        let tree = parse_rust(source);
        let mut staging = StagingGraph::new();
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);
        let mut metadata_store = NodeMetadataStore::new();

        let func = find_node_by_kind(tree.root_node(), "function_item").unwrap();
        let func_id = helper.add_node("serve", None, NodeKind::Function);

        detect_attribute_macros(
            func,
            source.as_bytes(),
            "serve",
            func_id,
            &mut helper,
            &mut metadata_store,
        );

        let callsite_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::CallSite)
            .count();
        assert_eq!(
            callsite_count, 2,
            "Each well-known attr should create a CallSite"
        );
    }

    #[test]
    fn test_well_known_registry_lookup() {
        assert!(lookup_well_known("tokio::main").is_some());
        assert!(lookup_well_known("tokio::test").is_some());
        assert!(lookup_well_known("async_trait::async_trait").is_some());
        assert!(lookup_well_known("tracing::instrument").is_some());
        assert!(lookup_well_known("rocket::get").is_some());
        assert!(lookup_well_known("actix_web::post").is_some());
        assert!(lookup_well_known("nonexistent::attr").is_none());
    }

    #[test]
    fn test_builtin_attrs_list() {
        // Verify all expected built-in attrs are in the list.
        for attr in &[
            "allow",
            "warn",
            "deny",
            "forbid",
            "deprecated",
            "inline",
            "cold",
            "hot",
            "repr",
            "must_use",
            "no_mangle",
            "link",
            "link_name",
            "link_section",
            "doc",
            "path",
            "recursion_limit",
            "windows_subsystem",
            "global_allocator",
            "track_caller",
            "non_exhaustive",
            "automatically_derived",
            "cfg",
            "cfg_attr",
            "test",
            "bench",
            "ignore",
            "should_panic",
            "derive",
        ] {
            assert!(
                BUILTIN_COMPILER_ATTRS.contains(attr),
                "Missing built-in attr: {attr}"
            );
        }
    }
}
