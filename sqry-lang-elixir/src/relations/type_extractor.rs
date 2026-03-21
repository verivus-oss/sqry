//! Type extraction utilities for Elixir `TypeOf` and Reference edges.
//!
//! This module handles extraction of type information from Elixir AST nodes,
//! specifically from @spec annotations and @type definitions.

use tree_sitter::Node;

/// Extracts the complete type signature as a string from an Elixir type node.
///
/// This is used for creating `TypeOf` edges with the full type information.
///
/// # Examples
/// - `String.t()` → "`String.t()`"
/// - `integer()` → "`integer()`"
/// - `{:ok, String.t()}` → "{:ok, `String.t()`}"
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(clean_type_string)
}

/// Extracts all individual type names from an Elixir type annotation.
///
/// This recursively traverses the type AST to find all type references,
/// used for creating Reference edges to each type mentioned.
///
/// # Examples
/// - `String.t()` → \["String"\]
/// - `{:ok, User.t()}` → \["User"\]
/// - `String.t() | integer()` → \["String", "integer"\]
#[must_use]
pub fn extract_all_type_names_from_elixir_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    extract_types_recursive(node, content, &mut types);
    types
}

/// Recursively extracts type names from an Elixir type AST node.
fn extract_types_recursive(node: Node, content: &[u8], types: &mut Vec<String>) {
    match node.kind() {
        // Module-qualified types: String.t(), Enum.t()
        "call" => {
            // Check if this is a module-qualified type (has dot child)
            if let Some(dot_node) = node.child_by_field_name("target")
                && dot_node.kind() == "dot"
                && let Some(alias_node) = dot_node.child_by_field_name("left")
                && alias_node.kind() == "alias"
                && let Ok(module_name) = alias_node.utf8_text(content)
            {
                types.push(clean_type_name(module_name));
            }

            // For simple calls like integer(), atom(), extract identifier
            if let Some(identifier_node) = node.child_by_field_name("target")
                && identifier_node.kind() == "identifier"
                && let Ok(type_name) = identifier_node.utf8_text(content)
                && is_builtin_type(type_name)
            {
                types.push(clean_type_name(type_name));
            }

            // Recurse into arguments for nested types
            if let Some(args_node) = node.child_by_field_name("arguments") {
                extract_types_recursive(args_node, content, types);
            }
        }

        // Simple identifiers (for custom types without parens)
        "identifier" => {
            if let Ok(type_name) = node.utf8_text(content)
                && is_builtin_type(type_name)
            {
                types.push(clean_type_name(type_name));
            }
        }

        // Module aliases
        "alias" => {
            if let Ok(module_name) = node.utf8_text(content) {
                types.push(clean_type_name(module_name));
            }
        }

        // Union types: type1() | type2()
        "binary_operator" => {
            // Check if this is a type union (|)
            if let Some(operator) = node.child(1)
                && let Ok(op_text) = operator.utf8_text(content)
                && op_text == "|"
            {
                // Extract from both sides of the union
                if let Some(left) = node.child_by_field_name("left") {
                    extract_types_recursive(left, content, types);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    extract_types_recursive(right, content, types);
                }
                return; // Don't recurse further
            }

            // For other binary operators, recurse into children
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_types_recursive(child, content, types);
            }
        }

        // Tuple types: {:ok, String.t()}
        // List types: [String.t()]
        // Arguments list - recurse into each argument
        // Default: recurse into all children
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_types_recursive(child, content, types);
            }
        }
    }
}

/// Checks if a node kind represents a type-related AST node.
#[must_use]
pub fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "call"
            | "identifier"
            | "alias"
            | "dot"
            | "binary_operator"
            | "tuple"
            | "list"
            | "map"
            | "arguments"
    )
}

/// Checks if a string is a known Elixir built-in type or type function.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // Basic types
        "integer" | "float" | "number" | "boolean" | "atom" | "binary" | "bitstring"
        | "byte" | "char" | "charlist" | "list" | "map" | "tuple" | "struct"
        | "function" | "fun" | "pid" | "port" | "reference" | "term" | "any"
        | "none" | "timeout" | "module" | "mfa" | "arity" | "iodata" | "iolist"
        | "keyword" | "as_boolean" | "node" | "identifier"
        // Common type functions
        | "t" | "elem" | "key" | "value"
    )
}

/// Removes whitespace and cleans up type names.
fn clean_type_name(name: &str) -> String {
    name.trim().to_string()
}

/// Cleans up type strings by normalizing whitespace.
fn clean_type_string(type_str: &str) -> String {
    // Normalize whitespace
    type_str
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ElixirPlugin;
    use sqry_core::plugin::LanguagePlugin;

    fn parse_elixir(source: &str) -> tree_sitter::Tree {
        let plugin = ElixirPlugin::default();
        plugin.parse_ast(source.as_bytes()).expect("parse failed")
    }

    fn find_type_node<'a>(node: Node<'a>, content: &[u8]) -> Option<Node<'a>> {
        // Find the type node in a @spec annotation (right side of ::)
        match node.kind() {
            "unary_operator" => {
                // Check if this is a spec or type annotation
                if let Some(call_node) = node.named_child(0)
                    && call_node.kind() == "call"
                    && let Some(target) = call_node.named_child(0)
                    && let Ok(target_text) = target.utf8_text(content)
                    && (target_text == "spec" || target_text == "type")
                    && let Some(args) = call_node.named_child(1)
                    && args.kind() == "arguments"
                    && let Some(binary_op) = args.named_child(0)
                    && binary_op.kind() == "binary_operator"
                {
                    // Return type is the second named child
                    return binary_op.named_child(1);
                }
            }
            "source" => {
                // Start from source node, look for unary_operator children
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(found) = find_type_node(child, content) {
                        return Some(found);
                    }
                }
            }
            _ => {
                // Recurse into other nodes
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(found) = find_type_node(child, content) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn test_debug_ast_structure() {
        let source = r#"
@spec test() :: integer()
"#;
        let tree = parse_elixir(source);

        fn print_tree(node: Node, content: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = if node.named_child_count() == 0 {
                node.utf8_text(content).unwrap_or("")
            } else {
                ""
            };
            eprintln!("{}{} '{}'", indent, node.kind(), text);
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                print_tree(child, content, depth + 1);
            }
        }

        eprintln!("\n=== AST STRUCTURE ===\n");
        print_tree(tree.root_node(), source.as_bytes(), 0);
    }

    #[test]
    fn test_extract_simple_builtin_type() {
        let source = r#"
@spec test() :: integer()
"#;
        let tree = parse_elixir(source);
        let type_node = find_type_node(tree.root_node(), source.as_bytes()).expect("type node");

        let types = extract_all_type_names_from_elixir_type(type_node, source.as_bytes());
        assert_eq!(types, vec!["integer"]);
    }

    #[test]
    fn test_extract_module_qualified_type() {
        let source = r#"
@spec test() :: String.t()
"#;
        let tree = parse_elixir(source);
        let type_node = find_type_node(tree.root_node(), source.as_bytes()).expect("type node");

        let types = extract_all_type_names_from_elixir_type(type_node, source.as_bytes());
        assert_eq!(types, vec!["String"]);
    }

    #[test]
    fn test_extract_tuple_type() {
        let source = r#"
@spec test() :: {:ok, String.t()}
"#;
        let tree = parse_elixir(source);
        let type_node = find_type_node(tree.root_node(), source.as_bytes()).expect("type node");

        let types = extract_all_type_names_from_elixir_type(type_node, source.as_bytes());
        assert!(types.contains(&"String".to_string()));
    }

    #[test]
    fn test_extract_union_type() {
        let source = r#"
@spec test() :: String.t() | integer()
"#;
        let tree = parse_elixir(source);
        let type_node = find_type_node(tree.root_node(), source.as_bytes()).expect("type node");

        let types = extract_all_type_names_from_elixir_type(type_node, source.as_bytes());
        assert!(types.contains(&"String".to_string()));
        assert!(types.contains(&"integer".to_string()));
    }
}
