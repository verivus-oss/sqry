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

/// Width-alias normalisation table (DESIGN §3.2 / SPEC §3.2.1).
///
/// Maps every width-aliased C base-type token to its canonical
/// equivalence-class representative. The table covers six equivalence
/// classes:
///
/// | Canonical | Source tokens                                                                          |
/// |-----------|----------------------------------------------------------------------------------------|
/// | `int`     | `signed int`, `unsigned int`, `int`, `int8_t`, `uint8_t`, `int16_t`, `uint16_t`, `int32_t`, `uint32_t` |
/// | `long`    | `long`, `int64_t`, `uint64_t`, `size_t`, `ssize_t`, `ptrdiff_t`, `off_t`, `loff_t`     |
/// | `char`    | `char`, `signed char`, `unsigned char`, `__u8`, `u8`                                   |
/// | `bool`    | `_Bool`, `bool`                                                                        |
/// | `float`   | `float`, `__fp16`                                                                      |
/// | `double`  | `double`, `long double`                                                                |
///
/// Normalisation runs over **base type tokens only** — it never sees
/// declarator structure (pointer depth, array decay, function-pointer
/// opacity) and never participates in qualifier stripping. Tokens not
/// listed here pass through unchanged (identity), so non-aliased types
/// such as `struct foo` or typedef names are unaffected.
///
/// Consumed by [`normalize_width_alias`] and (in a follow-up Phase A
/// unit, `U07_SIGNATURE_BUILDER`) by the canonical type-signature builder
/// in `sqry-lang-c/src/relations/signature_builder.rs`. The unit-tests
/// in `width_alias_tests` exercise every row.
#[allow(dead_code)] // wired by U07_SIGNATURE_BUILDER in a follow-up commit on this branch
const WIDTH_ALIAS_TABLE: &[(&str, &str)] = &[
    // Canonical → `int`
    ("signed int", "int"),
    ("unsigned int", "int"),
    ("int", "int"),
    ("int8_t", "int"),
    ("uint8_t", "int"),
    ("int16_t", "int"),
    ("uint16_t", "int"),
    ("int32_t", "int"),
    ("uint32_t", "int"),
    // Canonical → `long`
    ("long", "long"),
    ("int64_t", "long"),
    ("uint64_t", "long"),
    ("size_t", "long"),
    ("ssize_t", "long"),
    ("ptrdiff_t", "long"),
    ("off_t", "long"),
    ("loff_t", "long"),
    // Canonical → `char`
    ("char", "char"),
    ("signed char", "char"),
    ("unsigned char", "char"),
    ("__u8", "char"),
    ("u8", "char"),
    // Canonical → `bool`
    ("_Bool", "bool"),
    ("bool", "bool"),
    // Canonical → `float`
    ("float", "float"),
    ("__fp16", "float"),
    // Canonical → `double`
    ("double", "double"),
    ("long double", "double"),
];

/// Normalise a C base-type token to its width-alias canonical form.
///
/// Looks `token` up in [`WIDTH_ALIAS_TABLE`]; on a hit returns the
/// canonical equivalence-class representative, on a miss returns the
/// input unchanged. The lookup is a linear scan over fewer than 30
/// entries — measurably cheaper than a `HashMap` round-trip at this
/// size, and keeps the table `const`-allocatable.
///
/// This function operates on base-type tokens only (per DESIGN §3.2);
/// callers are responsible for stripping qualifiers and declarator
/// structure before invoking it.
#[allow(dead_code)] // consumed by U07_SIGNATURE_BUILDER in a follow-up commit on this branch
pub(crate) fn normalize_width_alias(token: &str) -> &str {
    for (source, canonical) in WIDTH_ALIAS_TABLE {
        if *source == token {
            return canonical;
        }
    }
    token
}

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
        #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
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

#[cfg(test)]
mod width_alias_tests {
    //! TEST:c-icall-precision-015 — every row of `WIDTH_ALIAS_TABLE` plus
    //! the identity (no-match) case. Verifies SPEC §3.2.1 / DESIGN §3.2
    //! width-alias equivalence classes.
    use super::{WIDTH_ALIAS_TABLE, normalize_width_alias};

    // ----- Equivalence class: `int` -----

    #[test]
    fn normalizes_signed_int_to_int() {
        assert_eq!(normalize_width_alias("signed int"), "int");
    }

    #[test]
    fn normalizes_unsigned_int_to_int() {
        assert_eq!(normalize_width_alias("unsigned int"), "int");
    }

    #[test]
    fn normalizes_int_to_int_identity() {
        assert_eq!(normalize_width_alias("int"), "int");
    }

    #[test]
    fn normalizes_int8_t_to_int() {
        assert_eq!(normalize_width_alias("int8_t"), "int");
    }

    #[test]
    fn normalizes_uint8_t_to_int() {
        assert_eq!(normalize_width_alias("uint8_t"), "int");
    }

    #[test]
    fn normalizes_int16_t_to_int() {
        assert_eq!(normalize_width_alias("int16_t"), "int");
    }

    #[test]
    fn normalizes_uint16_t_to_int() {
        assert_eq!(normalize_width_alias("uint16_t"), "int");
    }

    #[test]
    fn normalizes_int32_t_to_int() {
        assert_eq!(normalize_width_alias("int32_t"), "int");
    }

    #[test]
    fn normalizes_uint32_t_to_int() {
        assert_eq!(normalize_width_alias("uint32_t"), "int");
    }

    // ----- Equivalence class: `long` -----

    #[test]
    fn normalizes_long_to_long_identity() {
        assert_eq!(normalize_width_alias("long"), "long");
    }

    #[test]
    fn normalizes_int64_t_to_long() {
        assert_eq!(normalize_width_alias("int64_t"), "long");
    }

    #[test]
    fn normalizes_uint64_t_to_long() {
        assert_eq!(normalize_width_alias("uint64_t"), "long");
    }

    #[test]
    fn normalizes_size_t_to_long() {
        assert_eq!(normalize_width_alias("size_t"), "long");
    }

    #[test]
    fn normalizes_ssize_t_to_long() {
        assert_eq!(normalize_width_alias("ssize_t"), "long");
    }

    #[test]
    fn normalizes_ptrdiff_t_to_long() {
        assert_eq!(normalize_width_alias("ptrdiff_t"), "long");
    }

    #[test]
    fn normalizes_off_t_to_long() {
        assert_eq!(normalize_width_alias("off_t"), "long");
    }

    #[test]
    fn normalizes_loff_t_to_long() {
        assert_eq!(normalize_width_alias("loff_t"), "long");
    }

    // ----- Equivalence class: `char` -----

    #[test]
    fn normalizes_char_to_char_identity() {
        assert_eq!(normalize_width_alias("char"), "char");
    }

    #[test]
    fn normalizes_signed_char_to_char() {
        assert_eq!(normalize_width_alias("signed char"), "char");
    }

    #[test]
    fn normalizes_unsigned_char_to_char() {
        assert_eq!(normalize_width_alias("unsigned char"), "char");
    }

    #[test]
    fn normalizes_kernel_u8_to_char() {
        assert_eq!(normalize_width_alias("__u8"), "char");
    }

    #[test]
    fn normalizes_u8_to_char() {
        assert_eq!(normalize_width_alias("u8"), "char");
    }

    // ----- Equivalence class: `bool` -----

    #[test]
    fn normalizes_c99_bool_to_bool() {
        assert_eq!(normalize_width_alias("_Bool"), "bool");
    }

    #[test]
    fn normalizes_bool_to_bool_identity() {
        assert_eq!(normalize_width_alias("bool"), "bool");
    }

    // ----- Equivalence class: `float` -----

    #[test]
    fn normalizes_float_to_float_identity() {
        assert_eq!(normalize_width_alias("float"), "float");
    }

    #[test]
    fn normalizes_fp16_to_float() {
        assert_eq!(normalize_width_alias("__fp16"), "float");
    }

    // ----- Equivalence class: `double` -----

    #[test]
    fn normalizes_double_to_double_identity() {
        assert_eq!(normalize_width_alias("double"), "double");
    }

    #[test]
    fn normalizes_long_double_to_double() {
        assert_eq!(normalize_width_alias("long double"), "double");
    }

    // ----- Identity / negative case -----

    #[test]
    fn unknown_token_returns_itself() {
        assert_eq!(normalize_width_alias("foo"), "foo");
    }

    #[test]
    fn unknown_struct_token_returns_itself() {
        // Real-world non-aliased base tokens (struct tags, typedef
        // identifiers) must pass through unchanged.
        assert_eq!(
            normalize_width_alias("struct file_operations"),
            "struct file_operations"
        );
        assert_eq!(normalize_width_alias("MyTypedef"), "MyTypedef");
        assert_eq!(normalize_width_alias(""), "");
    }

    #[test]
    fn table_covers_every_design_row() {
        // Guard against accidental row deletion: count source tokens
        // listed in DESIGN §3.2 and compare against the table length.
        // 9 (int) + 8 (long) + 5 (char) + 2 (bool) + 2 (float) + 2 (double) = 28.
        assert_eq!(WIDTH_ALIAS_TABLE.len(), 28);
    }

    #[test]
    fn table_canonicals_are_self_normalising() {
        // Every canonical token must normalise to itself — fundamental
        // closure property of an equivalence-class projection.
        for canonical in ["int", "long", "char", "bool", "float", "double"] {
            assert_eq!(normalize_width_alias(canonical), canonical);
        }
    }
}
