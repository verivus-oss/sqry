//! Type extraction utilities for C# `TypeOf` and Reference edges.
//!
//! This module handles extraction of type information from C# AST nodes,
//! supporting the full C# type system including generics, nullables, arrays,
//! tuples, and qualified names.

use tree_sitter::Node;

/// Extracts the complete type signature as a string from a C# type annotation.
///
/// This is used for creating `TypeOf` edges with the full type information.
///
/// # Examples
/// - `string` → "string"
/// - `List<User>` → "`List<User>`"
/// - `int?` → "int?"
/// - `int[]` → "int[]"
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(clean_type_string)
}

/// Extracts the primary type name from a C# type annotation.
///
/// For complex types, this returns the base type.
/// This is used for backward compatibility with existing `TypeOf` edge creation.
///
/// # Arguments
/// * `type_node` - The type annotation AST node
/// * `content` - Source file content as bytes
///
/// # Returns
/// * `Some(String)` - The extracted base type name
/// * `None` - If type cannot be extracted
#[must_use]
pub fn extract_type_name_from_annotation(type_node: Node<'_>, content: &[u8]) -> Option<String> {
    match type_node.kind() {
        // Simple type identifiers
        "predefined_type" | "identifier" | "identifier_name" | "type_identifier" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),

        // Qualified type: System.Text.StringBuilder
        "qualified_name" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),

        // Generic type: List<User> → List
        "generic_name" => type_node
            .child_by_field_name("name")
            .or_else(|| type_node.named_child(0))
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.trim().to_string()),

        // Nullable type: int? → int
        "nullable_type" => type_node
            .child_by_field_name("type")
            .or_else(|| type_node.named_child(0))
            .and_then(|n| extract_type_name_from_annotation(n, content)),

        // Array type: int[] → int
        "array_type" => type_node
            .child_by_field_name("type")
            .or_else(|| type_node.named_child(0))
            .and_then(|n| extract_type_name_from_annotation(n, content)),

        // Tuple type: (int, string) → ValueTuple
        "tuple_type" => Some("ValueTuple".to_string()),

        // Pointer type: int* → int
        "pointer_type" => type_node
            .named_child(0)
            .and_then(|n| extract_type_name_from_annotation(n, content)),

        // Fallback: only accept if it looks like an identifier
        _ => {
            type_node.utf8_text(content).ok().and_then(|s| {
                let trimmed = s.trim();
                // Only accept simple identifier-like strings
                if !trimmed.is_empty()
                    && !trimmed.contains('{')
                    && !trimmed.contains('(')
                    && trimmed.len() < 100
                {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
        }
    }
}

/// Extract all type names from a C# type annotation, including all nested types
/// from generics, tuples, and other complex constructs.
///
/// This is used for creating Reference edges to capture all type dependencies.
///
/// # Arguments
/// * `type_node` - The type annotation AST node
/// * `content` - Source file content as bytes
///
/// # Returns
/// * `Vec<String>` - All type names found in the annotation
#[must_use]
pub fn extract_all_type_names_from_annotation(type_node: Node<'_>, content: &[u8]) -> Vec<String> {
    match type_node.kind() {
        // Simple type identifiers
        "predefined_type" | "identifier" | "identifier_name" | "type_identifier" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| vec![s.trim().to_string()])
            .unwrap_or_default(),

        // Qualified type: System.Text.StringBuilder
        "qualified_name" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| vec![s.trim().to_string()])
            .unwrap_or_default(),

        // Generic type: extract base type AND type arguments
        // List<User> → [List, User]
        // Dictionary<K, V> → [Dictionary, K, V]
        "generic_name" => {
            let mut types = Vec::new();

            // Extract base type
            if let Some(base_type) = type_node
                .child_by_field_name("name")
                .or_else(|| type_node.named_child(0))
                && let Ok(text) = base_type.utf8_text(content)
            {
                types.push(text.trim().to_string());
            }

            // Extract type arguments
            if let Some(type_args) = type_node.child_by_field_name("type_argument_list") {
                let mut cursor = type_args.walk();
                for child in type_args.named_children(&mut cursor) {
                    types.extend(extract_all_type_names_from_annotation(child, content));
                }
            }

            types
        }

        // Array type: extract element type
        // int[] → [int]
        "array_type" => {
            if let Some(element_type) = type_node
                .child_by_field_name("type")
                .or_else(|| type_node.named_child(0))
            {
                extract_all_type_names_from_annotation(element_type, content)
            } else {
                Vec::new()
            }
        }

        // Nullable type: extract inner type
        // int? → [int]
        "nullable_type" => {
            if let Some(inner_type) = type_node
                .child_by_field_name("type")
                .or_else(|| type_node.named_child(0))
            {
                extract_all_type_names_from_annotation(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Tuple type: extract all element types
        // (int, string, User) → [int, string, User]
        "tuple_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() == "tuple_element"
                    && let Some(type_child) = child.child_by_field_name("type")
                {
                    types.extend(extract_all_type_names_from_annotation(type_child, content));
                }
            }
            types
        }

        // Pointer type: extract pointed-to type
        // int* → [int]
        "pointer_type" => {
            if let Some(inner_type) = type_node.named_child(0) {
                extract_all_type_names_from_annotation(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Type argument list: extract all type arguments
        // Default: recurse into children
        _ => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }
    }
}

/// Checks if a node kind represents a type-related AST node.
#[must_use]
pub fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "predefined_type"
            | "identifier_name"
            | "type_identifier"
            | "generic_name"
            | "array_type"
            | "nullable_type"
            | "qualified_name"
            | "tuple_type"
            | "pointer_type"
            | "type_argument_list"
    )
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

    #[test]
    fn test_clean_type_string() {
        assert_eq!(clean_type_string("  string  "), "string");
        assert_eq!(clean_type_string("List<User>"), "List<User>");
        assert_eq!(clean_type_string("int?"), "int?");
    }

    #[test]
    fn test_is_type_node() {
        assert!(is_type_node("predefined_type"));
        assert!(is_type_node("generic_name"));
        assert!(is_type_node("nullable_type"));
        assert!(!is_type_node("identifier"));
        assert!(!is_type_node("method_declaration"));
    }
}
