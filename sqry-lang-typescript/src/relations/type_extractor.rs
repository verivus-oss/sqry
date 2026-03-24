//! Type extraction utilities for TypeScript `TypeOf` and Reference edges.
//!
//! This module handles extraction of type information from TypeScript AST nodes,
//! supporting the full TypeScript type system including generics, unions, intersections,
//! and complex nested types.

use tree_sitter::Node;

/// Extracts the complete type signature as a string from a TypeScript type annotation.
///
/// This is used for creating `TypeOf` edges with the full type information.
///
/// # Examples
/// - `string` → "string"
/// - `Array<User>` → "`Array<User>`"
/// - `string | number` → "string | number"
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(clean_type_string)
}

/// Extracts the primary type name from a TypeScript type annotation.
///
/// For complex types (unions, intersections), this returns the first/primary type.
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
        // Type annotation wrapper: `: Type`
        "type_annotation" => {
            // Recurse into the actual type
            let actual_type = type_node.named_child(0)?;
            extract_type_name_from_annotation(actual_type, content)
        }

        // Simple type identifier: User, string, number
        "type_identifier" | "predefined_type" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| s.trim().to_string()),

        // Generic type: Array<User> → Array
        "generic_type" => {
            // Extract base type (first named child is usually the type name)
            let base_type = type_node
                .child_by_field_name("name")
                .or_else(|| type_node.named_child(0))?;
            base_type
                .utf8_text(content)
                .ok()
                .map(|s| s.trim().to_string())
        }

        // Array type: string[] → string
        "array_type" => {
            // Extract element type (first named child)
            let element_type = type_node.named_child(0)?;
            extract_type_name_from_annotation(element_type, content)
        }

        // Union type: string | number → string (first type only)
        "union_type" => {
            // Take first type from union
            let first_type = type_node.named_child(0)?;
            extract_type_name_from_annotation(first_type, content)
        }

        // Intersection type: A & B → A (first type only)
        // Parenthesized type: (User) → User
        "intersection_type" | "parenthesized_type" => {
            let first_type = type_node.named_child(0)?;
            extract_type_name_from_annotation(first_type, content)
        }

        // Function types: (x: number) => string → Function
        "function_type" | "constructor_type" => Some("Function".to_string()),

        // Skip complex types that don't have stable identifiers
        "object_type" | "tuple_type" | "literal_type" | "template_type" => None,

        // Fallback: only accept if it looks like an identifier
        _ => {
            type_node.utf8_text(content).ok().and_then(|s| {
                let trimmed = s.trim().trim_start_matches(':').trim();
                // Only accept simple identifier-like strings
                if !trimmed.is_empty()
                    && !trimmed.contains('{')
                    && !trimmed.contains('(')
                    && !trimmed.contains('<')
                    && trimmed.len() < 50
                {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
        }
    }
}

/// Extract all type names from a TypeScript type annotation, including all branches
/// of unions and intersections.
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
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub fn extract_all_type_names_from_annotation(type_node: Node<'_>, content: &[u8]) -> Vec<String> {
    match type_node.kind() {
        // Simple type identifier
        "type_identifier" | "predefined_type" => type_node
            .utf8_text(content)
            .ok()
            .map(|s| vec![s.trim().to_string()])
            .unwrap_or_default(),

        // Generic type: extract base type AND type arguments
        // Array<User> → Array, User
        "generic_type" => {
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
            if let Some(type_args) = type_node.child_by_field_name("type_arguments") {
                let mut cursor = type_args.walk();
                for child in type_args.named_children(&mut cursor) {
                    types.extend(extract_all_type_names_from_annotation(child, content));
                }
            }

            types
        }

        // Array type: extract element type
        "array_type" => {
            if let Some(element_type) = type_node.named_child(0) {
                extract_all_type_names_from_annotation(element_type, content)
            } else {
                Vec::new()
            }
        }

        // Type annotation wrapper: recurse
        // Parenthesized type: unwrap
        // Union type: extract ALL types (not just first)
        // Intersection type: extract ALL types
        "type_annotation" | "parenthesized_type" | "union_type" | "intersection_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Function type: extract parameter types and return type
        // (x: number) => string → [Function, number, string]
        "function_type" => {
            let mut types = vec!["Function".to_string()];

            // Extract parameter types
            if let Some(params) = type_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    if let Some(type_ann) = param.child_by_field_name("type") {
                        types.extend(extract_all_type_names_from_annotation(type_ann, content));
                    }
                }
            }

            // Extract return type
            if let Some(return_type) = type_node.child_by_field_name("return_type") {
                types.extend(extract_all_type_names_from_annotation(return_type, content));
            }

            // Extract type parameters (for generic methods)
            if let Some(type_params) = type_node.child_by_field_name("type_parameters") {
                let mut cursor = type_params.walk();
                for type_param in type_params.named_children(&mut cursor) {
                    // Extract constraint type if present (e.g., T extends User)
                    if let Some(constraint) = type_param.child_by_field_name("constraint") {
                        types.extend(extract_all_type_names_from_annotation(constraint, content));
                    }
                }
            }

            types
        }

        // Constructor type: similar to function type
        // new (id: number) => User → [Function, number, User]
        "constructor_type" => {
            let mut types = vec!["Function".to_string()];

            if let Some(params) = type_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    if let Some(type_ann) = param.child_by_field_name("type") {
                        types.extend(extract_all_type_names_from_annotation(type_ann, content));
                    }
                }
            }

            if let Some(return_type) = type_node.child_by_field_name("type") {
                types.extend(extract_all_type_names_from_annotation(return_type, content));
            }

            types
        }

        // Object type: extract types from properties
        // { name: string; age: number } → [string, number]
        // Also handles mapped types: { [K in keyof T]: V } → [T, V]
        "object_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                // Recurse into all named children to handle:
                // - Property signatures (via type field)
                // - Index signatures (including mapped_type_clause)
                // - Method signatures
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Tuple type: [string, number] → [string, number]
        "tuple_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Indexed access type: Type["key"] → Type
        "index_type" | "lookup_type" | "indexed_access_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Type operator: keyof T → T, readonly T → T
        // Index type query (keyof operator): keyof T → T
        // Index signature: [key: string]: Type or [K in keyof T]: V
        "type_operator" | "index_type_query" | "index_signature" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Property signature: name: string → string
        "property_signature" => {
            if let Some(type_ann) = type_node.child_by_field_name("type") {
                extract_all_type_names_from_annotation(type_ann, content)
            } else {
                Vec::new()
            }
        }

        // Method signatures: extract parameter types and return type
        "method_signature" | "abstract_method_signature" | "call_signature" => {
            let mut types = Vec::new();

            if let Some(params) = type_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.named_children(&mut cursor) {
                    if let Some(type_ann) = param.child_by_field_name("type") {
                        types.extend(extract_all_type_names_from_annotation(type_ann, content));
                    }
                }
            }

            if let Some(return_type) = type_node.child_by_field_name("return_type") {
                types.extend(extract_all_type_names_from_annotation(return_type, content));
            }

            if let Some(type_params) = type_node.child_by_field_name("type_parameters") {
                let mut cursor = type_params.walk();
                for param in type_params.named_children(&mut cursor) {
                    if let Some(constraint) = param.child_by_field_name("constraint") {
                        types.extend(extract_all_type_names_from_annotation(constraint, content));
                    }
                }
            }

            types
        }

        // Mapped type clause: K in keyof T → T
        "mapped_type_clause" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Mapped type: { [K in keyof T]: string } → T, string
        "mapped_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() == "type_parameter" {
                    if let Some(constraint) = child.child_by_field_name("constraint") {
                        types.extend(extract_all_type_names_from_annotation(constraint, content));
                    }
                } else if let Some(type_ann) = child.child_by_field_name("type") {
                    types.extend(extract_all_type_names_from_annotation(type_ann, content));
                }
            }
            types
        }

        // Template literal type: `prefix-${Foo}-${Bar}` → Foo, Bar
        "template_literal_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                if child.kind() != "string_fragment" {
                    types.extend(extract_all_type_names_from_annotation(child, content));
                }
            }
            types
        }

        // Conditional type: T extends string ? number : boolean → T, string, number, boolean
        "conditional_type" => {
            let mut types = Vec::new();
            let mut cursor = type_node.walk();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }

        // Optional type: T? → T
        "optional_type" | "opting_type_annotation" => {
            if let Some(inner_type) = type_node.named_child(0) {
                extract_all_type_names_from_annotation(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Rest type: ...T → T
        "rest_type" => {
            if let Some(inner_type) = type_node.named_child(0) {
                extract_all_type_names_from_annotation(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Type query: typeof expressions
        "type_query" => {
            if let Some(operand) = type_node.child_by_field_name("operand") {
                if let Ok(text) = operand.utf8_text(content) {
                    vec![text.trim().to_string()]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }

        // Skip literals and string fragments (no meaningful type references)
        "literal_type" | "string_fragment" => Vec::new(),

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
        "type_annotation"
            | "type_identifier"
            | "predefined_type"
            | "generic_type"
            | "array_type"
            | "union_type"
            | "intersection_type"
            | "function_type"
            | "constructor_type"
            | "object_type"
            | "tuple_type"
            | "parenthesized_type"
            | "literal_type"
            | "template_type"
            | "template_literal_type"
            | "index_type"
            | "lookup_type"
            | "indexed_access_type"
            | "type_operator"
            | "index_type_query"
            | "index_signature"
            | "property_signature"
            | "method_signature"
            | "abstract_method_signature"
            | "call_signature"
            | "mapped_type_clause"
            | "mapped_type"
            | "conditional_type"
            | "optional_type"
            | "opting_type_annotation"
            | "rest_type"
            | "type_query"
    )
}

/// Cleans up type strings by normalizing whitespace.
fn clean_type_string(type_str: &str) -> String {
    // Remove leading colon from type annotations
    let trimmed = type_str.trim().trim_start_matches(':').trim();

    // Normalize whitespace
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Basic unit tests for type extraction
    #[test]
    fn test_clean_type_string() {
        assert_eq!(clean_type_string(": string"), "string");
        assert_eq!(clean_type_string("  Array<User>  "), "Array<User>");
        assert_eq!(clean_type_string("string | number"), "string | number");
    }

    #[test]
    fn test_is_type_node() {
        assert!(is_type_node("type_identifier"));
        assert!(is_type_node("generic_type"));
        assert!(is_type_node("union_type"));
        assert!(!is_type_node("identifier"));
        assert!(!is_type_node("function_declaration"));
    }
}
