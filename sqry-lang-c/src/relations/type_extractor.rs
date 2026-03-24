//! Type extraction for C language.
//!
//! This module provides functions to extract type names from C AST nodes for
//! creating `TypeOf` and Reference edges in the code graph.
//!
//! # C Type System
//!
//! C types consist of:
//! - **Type specifiers**: `int`, `char`, `struct User`, `union Data`, `enum Status`
//! - **Declarators**: `*` (pointer), `[]` (array), `()` (function)
//! - **Qualifiers**: `const`, `volatile`, `restrict`
//!
//! # Examples
//!
//! ```c
//! int x;                    // Simple: "int"
//! int* ptr;                 // Pointer: "int"
//! int arr[10];              // Array: "int"
//! struct User user;         // Struct: "User"
//! int (*callback)(char*);   // Function pointer: "int", "char"
//! ```

use tree_sitter::Node;

/// Extract all type names referenced by a C type node.
///
/// This function recursively traverses C type nodes and declarators to extract
/// all referenced type names. It handles:
/// - Primitive types (`int`, `char`, `float`, etc.)
/// - Type identifiers (`User`, `Status` - typedef'd types)
/// - Struct/union/enum specifiers
/// - Pointer declarators
/// - Array declarators
/// - Function declarators (function pointers)
/// - Sized type specifiers (`int8_t`, `uint32_t`, etc.)
///
/// # Arguments
///
/// * `type_node` - The tree-sitter AST node representing a C type
/// * `content` - The source file content as bytes
///
/// # Returns
///
/// A vector of all type names referenced by this type node.
///
/// # Examples
///
/// ```ignore
/// // For "int* ptr":
/// extract_all_type_names_from_c_type(type_node, content) => vec!["int"]
///
/// // For "struct User* users[]":
/// extract_all_type_names_from_c_type(type_node, content) => vec!["User"]
///
/// // For "int (*callback)(struct Event*)":
/// extract_all_type_names_from_c_type(type_node, content) => vec!["int", "Event"]
/// ```
#[allow(clippy::too_many_lines)]
pub fn extract_all_type_names_from_c_type(type_node: Node, content: &[u8]) -> Vec<String> {
    match type_node.kind() {
        // Primitive types, type identifiers, and sized type specifiers
        // These all extract the type name directly from the node text
        "primitive_type" | "type_identifier" | "sized_type_specifier" => {
            if let Ok(type_name) = type_node.utf8_text(content) {
                vec![type_name.to_string()]
            } else {
                Vec::new()
            }
        }

        // Struct specifiers: struct User, struct { ... }
        "struct_specifier" => {
            // Check for struct tag (name)
            // Preserve "struct" prefix to avoid namespace collisions with typedefs
            if let Some(name_node) = type_node.child_by_field_name("name") {
                if let Ok(struct_name) = name_node.utf8_text(content) {
                    vec![format!("struct {}", struct_name)]
                } else {
                    Vec::new()
                }
            } else {
                // Anonymous struct - no type name to reference
                Vec::new()
            }
        }

        // Union specifiers: union Data, union { ... }
        "union_specifier" => {
            // Check for union tag (name)
            // Preserve "union" prefix to avoid namespace collisions with typedefs
            if let Some(name_node) = type_node.child_by_field_name("name") {
                if let Ok(union_name) = name_node.utf8_text(content) {
                    vec![format!("union {}", union_name)]
                } else {
                    Vec::new()
                }
            } else {
                // Anonymous union - no type name to reference
                Vec::new()
            }
        }

        // Enum specifiers: enum Status, enum { ... }
        "enum_specifier" => {
            // Check for enum tag (name)
            // Preserve "enum" prefix to avoid namespace collisions with typedefs
            if let Some(name_node) = type_node.child_by_field_name("name") {
                if let Ok(enum_name) = name_node.utf8_text(content) {
                    vec![format!("enum {}", enum_name)]
                } else {
                    Vec::new()
                }
            } else {
                // Anonymous enum - no type name to reference
                Vec::new()
            }
        }

        // Pointer declarators: *T
        // Extract types from the underlying declarator
        "pointer_declarator" => {
            let mut types = Vec::new();

            // Traverse declarator tree to find the base type
            // The actual type info is in the parent declaration's type specifiers
            // So we need to recurse through declarators to collect nested types

            // First, check for nested declarators
            if let Some(declarator) = type_node.child_by_field_name("declarator") {
                types.extend(extract_all_type_names_from_c_type(declarator, content));
            }

            types
        }

        // Array declarators: T[N], T[]
        // Extract types from the underlying declarator
        "array_declarator" => {
            let mut types = Vec::new();

            // Recurse into the declarator
            if let Some(declarator) = type_node.child_by_field_name("declarator") {
                types.extend(extract_all_type_names_from_c_type(declarator, content));
            }

            types
        }

        // Function declarators: (params) → used for function pointers
        // Extract types from parameters
        "function_declarator" => {
            let mut types = Vec::new();

            // Extract parameter types from parameter_list
            if let Some(params) = type_node.child_by_field_name("parameters") {
                types.extend(extract_parameter_types(params, content));
            }

            // Recurse into declarator for the return type (via parent's type specifiers)
            if let Some(declarator) = type_node.child_by_field_name("declarator") {
                types.extend(extract_all_type_names_from_c_type(declarator, content));
            }

            types
        }

        // Abstract declarators: used in typedefs and casts
        "abstract_pointer_declarator" | "abstract_array_declarator" => {
            let mut types = Vec::new();

            if let Some(declarator) = type_node.child_by_field_name("declarator") {
                types.extend(extract_all_type_names_from_c_type(declarator, content));
            }

            types
        }

        "abstract_function_declarator" => {
            let mut types = Vec::new();

            // Extract parameter types
            if let Some(params) = type_node.child_by_field_name("parameters") {
                types.extend(extract_parameter_types(params, content));
            }

            if let Some(declarator) = type_node.child_by_field_name("declarator") {
                types.extend(extract_all_type_names_from_c_type(declarator, content));
            }

            types
        }

        // Parenthesized declarators: (declarator)
        "parenthesized_declarator" | "abstract_parenthesized_declarator" => {
            // Recurse into the parenthesized content
            let mut cursor = type_node.walk();
            let mut types = Vec::new();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_c_type(child, content));
            }
            types
        }

        // For other node types, recurse into children
        _ => {
            let mut cursor = type_node.walk();
            let mut types = Vec::new();
            for child in type_node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_c_type(child, content));
            }
            types
        }
    }
}

/// Extract parameter types from a `parameter_list` node.
///
/// This helper function processes function parameter lists to extract all
/// referenced type names.
///
/// # Arguments
///
/// * `param_list` - The tree-sitter node representing a parameter list
/// * `content` - The source file content as bytes
///
/// # Returns
///
/// A vector of all type names referenced in the parameter list.
fn extract_parameter_types(param_list: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = param_list.walk();

    for param in param_list.named_children(&mut cursor) {
        if param.kind() == "parameter_declaration" {
            // Extract type from parameter_declaration
            types.extend(extract_types_from_parameter_declaration(param, content));
        }
        // Variadic parameters (...) have no type to extract - skip them
    }

    types
}

/// Extract types from a `parameter_declaration` or `field_declaration` node.
///
/// This function handles the C declaration syntax where types are specified
/// separately from declarators.
///
/// # Arguments
///
/// * `decl_node` - The `parameter_declaration` or `field_declaration` node
/// * `content` - The source file content as bytes
///
/// # Returns
///
/// A vector of all type names referenced in the declaration.
fn extract_types_from_parameter_declaration(decl_node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = decl_node.walk();

    // Process all children to find type specifiers and declarators
    for child in decl_node.named_children(&mut cursor) {
        match child.kind() {
            // Type specifiers and declarators (may contain nested type references)
            "primitive_type"
            | "type_identifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "sized_type_specifier"
            | "pointer_declarator"
            | "array_declarator"
            | "function_declarator"
            | "abstract_pointer_declarator"
            | "abstract_array_declarator"
            | "abstract_function_declarator"
            | "parenthesized_declarator"
            | "abstract_parenthesized_declarator" => {
                types.extend(extract_all_type_names_from_c_type(child, content));
            }

            // Skip type qualifiers and storage class specifiers
            "type_qualifier" | "storage_class_specifier" => {}

            _ => {
                // Recursively process other children
                types.extend(extract_all_type_names_from_c_type(child, content));
            }
        }
    }

    types
}

/// Extract type specifiers from a declaration node.
///
/// This function finds and processes all type specifier nodes in a declaration,
/// extracting the referenced type names.
///
/// # Arguments
///
/// * `decl_node` - A declaration node (`function_definition`, `declaration`, etc.)
/// * `content` - The source file content as bytes
///
/// # Returns
///
/// A vector of type names from the declaration's type specifiers.
pub fn extract_type_specifiers_from_declaration(decl_node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = decl_node.walk();

    // Iterate through children to find type specifiers
    for child in decl_node.children(&mut cursor) {
        match child.kind() {
            "primitive_type"
            | "type_identifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "sized_type_specifier" => {
                types.extend(extract_all_type_names_from_c_type(child, content));
            }
            // Skip type qualifiers, storage class specifiers, and other nodes
            _ => {}
        }
    }

    types
}

// NOTE: This function is currently unused but kept for future enhancements
// when we need to construct more sophisticated type strings for TypeOf edges.
#[allow(dead_code)]
fn extract_type_string_from_declaration(decl_node: Node, content: &[u8]) -> Option<String> {
    // For now, extract the full declarator text
    // This can be refined to construct proper type strings
    let type_specifiers = extract_type_specifiers_from_declaration(decl_node, content);

    if type_specifiers.is_empty() {
        None
    } else {
        // Return the first (primary) type specifier
        // For complex types, this may need enhancement
        Some(type_specifiers[0].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to parse C code and get the root node
    fn parse_c_code(code: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("Failed to set language");
        parser.parse(code, None).expect("Failed to parse code")
    }

    #[test]
    fn test_extract_primitive_type() {
        let code = "int x;";
        let tree = parse_c_code(code);
        let root = tree.root_node();

        // Find the primitive_type node
        let mut cursor = root.walk();
        let mut primitive_type_node = None;
        for child in root.children(&mut cursor) {
            if child.kind() == "declaration" {
                let mut decl_cursor = child.walk();
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "primitive_type" {
                        primitive_type_node = Some(decl_child);
                        break;
                    }
                }
            }
        }

        let node = primitive_type_node.expect("Should find primitive_type node");
        let types = extract_all_type_names_from_c_type(node, code.as_bytes());
        assert_eq!(types, vec!["int"]);
    }

    #[test]
    fn test_extract_struct_type() {
        let code = "struct User { int id; } user;";
        let tree = parse_c_code(code);
        let root = tree.root_node();

        // Try to find struct_specifier recursively
        fn find_struct_specifier(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
            if node.kind() == "struct_specifier" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_struct_specifier(child) {
                    return Some(found);
                }
            }
            None
        }

        let struct_node = find_struct_specifier(root);

        let node = struct_node.expect("Should find struct_specifier node");
        let types = extract_all_type_names_from_c_type(node, code.as_bytes());
        // Tag names now include struct/union/enum prefix to avoid namespace collisions
        assert_eq!(types, vec!["struct User"]);
    }
}
