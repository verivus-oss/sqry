//! Metavariable extraction from `macro_rules!` definitions (4.5b).
//!
//! Extracts `$name:fragment_specifier` patterns from macro rule bodies and
//! creates `Parameter` nodes with `Contains` edges from the parent `Macro` node.
//!
//! # Deduplication
//!
//! Metavariables are deduplicated across multiple arms of the same macro.
//! A metavariable `$x:expr` appearing in three arms produces a single
//! `Parameter` node.
//!
//! # Recursive Macros
//!
//! If a `macro_rules!` body contains another `macro_rules!` definition, the
//! inner macro is extracted as a nested `Macro` node with its own metavariables
//! and a `Contains` edge from the outer macro.
//!
//! # Limitations
//!
//! Definition-site captures, hygiene scopes, and reference edges from the macro
//! to captured symbols require compiler semantics and are out of scope for
//! AST-only analysis.

use std::collections::HashSet;

use sqry_core::graph::Span;
use sqry_core::graph::node::Position;
use sqry_core::graph::unified::{GraphBuildHelper, NodeId, NodeKind};
use tree_sitter::Node;

/// Extract metavariables from a `macro_definition` node (both `macro_rules!` and
/// the `macro` keyword form).
///
/// For each unique `$name:fragment_specifier` pattern found, creates a `Parameter`
/// node with a `Contains` edge from the macro node. Also handles recursive macro
/// definitions by creating nested `Macro` nodes.
///
/// # Arguments
///
/// * `macro_node` — tree-sitter node for a `macro_definition`
/// * `content` — source file bytes
/// * `macro_qualified` — qualified name of the macro (e.g., `"my_crate::my_macro"`)
/// * `macro_id` — the graph `NodeId` already assigned to this macro
/// * `helper` — graph build helper for creating nodes and edges
pub fn extract_metavariables(
    macro_node: Node,
    content: &[u8],
    macro_qualified: &str,
    macro_id: NodeId,
    helper: &mut GraphBuildHelper,
) {
    debug_assert!(
        macro_node.kind() == "macro_definition",
        "extract_metavariables expects a macro_definition node, got {}",
        macro_node.kind()
    );

    let mut seen_metavars: HashSet<String> = HashSet::new();

    // Walk the entire macro body looking for metavariable patterns and nested macros.
    extract_from_subtree(
        macro_node,
        content,
        macro_qualified,
        macro_id,
        helper,
        &mut seen_metavars,
        0, // depth = 0 for the outermost macro
    );

    if seen_metavars.is_empty() {
        log::debug!("No metavariables found in macro '{}'", macro_qualified);
    } else {
        log::debug!(
            "Extracted {} metavariables from macro '{}': {:?}",
            seen_metavars.len(),
            macro_qualified,
            seen_metavars
        );
    }
}

/// Maximum nesting depth for recursive macro definitions to prevent runaway
/// extraction on adversarial inputs.
const MAX_RECURSION_DEPTH: usize = 8;

/// Recursively walk a subtree looking for `token_binding_pattern` nodes
/// (metavariables) and nested macro definitions.
///
/// `depth` tracks the nesting level of recursive macro definitions. At depth 0,
/// we are inside the outermost macro body. When we encounter a nested
/// `macro_definition`, we increment depth and extract its metavariables into a
/// new `Macro` node.
fn extract_from_subtree(
    node: Node,
    content: &[u8],
    parent_qualified: &str,
    parent_id: NodeId,
    helper: &mut GraphBuildHelper,
    seen_metavars: &mut HashSet<String>,
    depth: usize,
) {
    if depth > MAX_RECURSION_DEPTH {
        log::warn!(
            "Maximum recursion depth ({MAX_RECURSION_DEPTH}) exceeded in macro '{}'; \
             skipping deeper nesting",
            parent_qualified
        );
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            // A nested macro_definition inside the current macro body.
            "macro_definition" if depth > 0 || child.id() != node.id() => {
                handle_nested_macro(child, content, parent_qualified, parent_id, helper, depth);
            }

            // tree-sitter-rust groups metavariables into `token_binding_pattern`
            // nodes with children: metavariable ($name), `:`, fragment_specifier.
            "token_binding_pattern" => {
                extract_from_binding_pattern(
                    child,
                    content,
                    parent_qualified,
                    parent_id,
                    helper,
                    seen_metavars,
                );
            }

            _ => {
                // Recurse into all structural children.
                if child.child_count() > 0 {
                    extract_from_subtree(
                        child,
                        content,
                        parent_qualified,
                        parent_id,
                        helper,
                        seen_metavars,
                        depth,
                    );
                }
            }
        }
    }
}

/// Extract a metavariable from a `token_binding_pattern` node.
///
/// tree-sitter-rust represents `$x:expr` as:
/// ```text
/// token_binding_pattern
///   metavariable  "$x"
///   ":"
///   fragment_specifier
///     expr          "expr"
/// ```
fn extract_from_binding_pattern(
    binding_node: Node,
    content: &[u8],
    parent_qualified: &str,
    parent_id: NodeId,
    helper: &mut GraphBuildHelper,
    seen_metavars: &mut HashSet<String>,
) {
    let mut metavar_name: Option<String> = None;
    let mut fragment_spec: Option<String> = None;

    let mut cursor = binding_node.walk();
    for child in binding_node.children(&mut cursor) {
        match child.kind() {
            "metavariable" => {
                // The metavariable text includes the `$` prefix.
                if let Ok(text) = child.utf8_text(content) {
                    // Strip the leading `$` to get the bare name.
                    metavar_name = Some(text.strip_prefix('$').unwrap_or(text).to_string());
                }
            }
            "fragment_specifier" => {
                // The fragment_specifier may have a child with the actual name,
                // or the text itself is the specifier.
                if let Ok(text) = child.utf8_text(content) {
                    fragment_spec = Some(text.to_string());
                }
            }
            _ => {}
        }
    }

    if let (Some(name), Some(fragment)) = (metavar_name, fragment_spec) {
        if !is_valid_fragment_specifier(&fragment) {
            return;
        }

        let key = format!("{name}:{fragment}");
        if seen_metavars.insert(key) {
            let param_qualified = format!("{parent_qualified}::${name}");

            let span = Span::new(
                Position::new(
                    binding_node.start_position().row,
                    binding_node.start_position().column,
                ),
                Position::new(
                    binding_node.end_position().row,
                    binding_node.end_position().column,
                ),
            );

            let param_id = helper.add_node(&param_qualified, Some(span), NodeKind::Parameter);
            helper.add_contains_edge(parent_id, param_id);

            log::debug!(
                "Extracted metavariable ${}:{} in macro '{}'",
                name,
                fragment,
                parent_qualified
            );
        }
    }
}

/// Check if a string is a valid Rust macro fragment specifier.
fn is_valid_fragment_specifier(spec: &str) -> bool {
    matches!(
        spec,
        "block"
            | "expr"
            | "expr_2021"
            | "ident"
            | "item"
            | "lifetime"
            | "literal"
            | "meta"
            | "pat"
            | "pat_param"
            | "path"
            | "stmt"
            | "tt"
            | "ty"
            | "vis"
    )
}

/// Handle a nested macro definition inside another macro body.
///
/// Creates a new `Macro` node for the inner definition, adds a `Contains` edge
/// from the outer macro, and recursively extracts metavariables from the inner
/// macro body.
fn handle_nested_macro(
    inner_macro: Node,
    content: &[u8],
    parent_qualified: &str,
    parent_id: NodeId,
    helper: &mut GraphBuildHelper,
    depth: usize,
) {
    // Extract the inner macro's name.
    let inner_name = inner_macro
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .unwrap_or("<anonymous>");

    let inner_qualified = format!("{parent_qualified}::{inner_name}");
    let span = Span::new(
        Position::new(
            inner_macro.start_position().row,
            inner_macro.start_position().column,
        ),
        Position::new(
            inner_macro.end_position().row,
            inner_macro.end_position().column,
        ),
    );

    let inner_id = helper.add_node(&inner_qualified, Some(span), NodeKind::Macro);
    helper.add_contains_edge(parent_id, inner_id);

    log::debug!(
        "Found nested macro '{}' inside '{}' at depth {}",
        inner_name,
        parent_qualified,
        depth + 1
    );

    // Recursively extract metavariables from the inner macro.
    let mut inner_seen = HashSet::new();
    extract_from_subtree(
        inner_macro,
        content,
        &inner_qualified,
        inner_id,
        helper,
        &mut inner_seen,
        depth + 1,
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

    fn setup_helper_with_macro(source: &str) -> (tree_sitter::Tree, StagingGraph) {
        let tree = parse_rust(source);
        let staging = StagingGraph::new();
        (tree, staging)
    }

    #[test]
    fn test_single_metavar() {
        let source = r#"
macro_rules! my_macro {
    ($x:expr) => { $x };
}
"#;
        let (tree, mut staging) = setup_helper_with_macro(source);
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);

        let macro_node = find_node_by_kind(tree.root_node(), "macro_definition").unwrap();
        let macro_id = helper.add_node("my_macro", None, NodeKind::Macro);

        extract_metavariables(
            macro_node,
            source.as_bytes(),
            "my_macro",
            macro_id,
            &mut helper,
        );

        // Should have at least the macro node + 1 parameter node
        let param_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Parameter)
            .count();
        assert_eq!(param_count, 1, "Expected 1 parameter node for $x:expr");
    }

    #[test]
    fn test_multiple_metavars() {
        let source = r#"
macro_rules! add {
    ($a:expr, $b:expr) => { $a + $b };
}
"#;
        let (tree, mut staging) = setup_helper_with_macro(source);
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);

        let macro_node = find_node_by_kind(tree.root_node(), "macro_definition").unwrap();
        let macro_id = helper.add_node("add", None, NodeKind::Macro);

        extract_metavariables(macro_node, source.as_bytes(), "add", macro_id, &mut helper);

        let param_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Parameter)
            .count();
        assert_eq!(
            param_count, 2,
            "Expected 2 parameter nodes for $a:expr and $b:expr"
        );
    }

    #[test]
    fn test_repeated_metavar() {
        // Same metavar $x:expr in multiple arms should be deduplicated
        let source = r#"
macro_rules! multi {
    ($x:expr) => { $x };
    ($x:expr, $y:expr) => { $x + $y };
}
"#;
        let (tree, mut staging) = setup_helper_with_macro(source);
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);

        let macro_node = find_node_by_kind(tree.root_node(), "macro_definition").unwrap();
        let macro_id = helper.add_node("multi", None, NodeKind::Macro);

        extract_metavariables(
            macro_node,
            source.as_bytes(),
            "multi",
            macro_id,
            &mut helper,
        );

        let param_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Parameter)
            .count();
        // $x:expr appears in both arms but should be counted once. $y:expr is unique.
        assert_eq!(
            param_count, 2,
            "Expected 2 parameter nodes ($x:expr, $y:expr)"
        );
    }

    #[test]
    fn test_multiple_arms() {
        let source = r#"
macro_rules! vec_like {
    () => { Vec::new() };
    ($($x:expr),+) => { { let mut v = Vec::new(); $(v.push($x);)+ v } };
}
"#;
        let (tree, mut staging) = setup_helper_with_macro(source);
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);

        let macro_node = find_node_by_kind(tree.root_node(), "macro_definition").unwrap();
        let macro_id = helper.add_node("vec_like", None, NodeKind::Macro);

        extract_metavariables(
            macro_node,
            source.as_bytes(),
            "vec_like",
            macro_id,
            &mut helper,
        );

        let param_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Parameter)
            .count();
        assert!(
            param_count >= 1,
            "Expected at least 1 parameter node for $x:expr"
        );
    }

    #[test]
    fn test_no_metavars() {
        let source = r#"
macro_rules! noop {
    () => {};
}
"#;
        let (tree, mut staging) = setup_helper_with_macro(source);
        let file = Path::new("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, file, Language::Rust);

        let macro_node = find_node_by_kind(tree.root_node(), "macro_definition").unwrap();
        let macro_id = helper.add_node("noop", None, NodeKind::Macro);

        extract_metavariables(macro_node, source.as_bytes(), "noop", macro_id, &mut helper);

        let param_count = staging
            .nodes()
            .filter(|n| n.entry.kind == NodeKind::Parameter)
            .count();
        assert_eq!(param_count, 0, "Expected 0 parameter nodes");
    }

    #[test]
    fn test_valid_fragment_specifiers() {
        assert!(is_valid_fragment_specifier("expr"));
        assert!(is_valid_fragment_specifier("ident"));
        assert!(is_valid_fragment_specifier("ty"));
        assert!(is_valid_fragment_specifier("tt"));
        assert!(is_valid_fragment_specifier("pat"));
        assert!(is_valid_fragment_specifier("path"));
        assert!(is_valid_fragment_specifier("stmt"));
        assert!(is_valid_fragment_specifier("block"));
        assert!(is_valid_fragment_specifier("item"));
        assert!(is_valid_fragment_specifier("meta"));
        assert!(is_valid_fragment_specifier("literal"));
        assert!(is_valid_fragment_specifier("vis"));
        assert!(is_valid_fragment_specifier("lifetime"));
        assert!(is_valid_fragment_specifier("pat_param"));
        assert!(is_valid_fragment_specifier("expr_2021"));
        assert!(!is_valid_fragment_specifier("unknown"));
        assert!(!is_valid_fragment_specifier(""));
    }
}
