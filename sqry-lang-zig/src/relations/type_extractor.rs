//! Type extraction from Zig AST nodes.
//!
//! This module provides functions to extract type names from Zig type annotation nodes.
//! Zig has native type annotations (like Go and Swift), enabling direct AST-based type extraction.
//!
//! # Supported Type Constructs
//!
//! - Primitive types: `i32`, `u64`, `f32`, `bool`, `void`, `usize`
//! - Pointer types: `*T`, `*const T`, `*volatile T`, `[*]T`, `[*c]T`
//! - Slice types: `[]T`, `[]const T`, `[:0]T`
//! - Array types: `[N]T`
//! - Optional types: `?T`
//! - Error union types: `!T`, `ErrorSet!T`
//! - Struct/union/enum types: Named custom types
//! - Function types: `fn(i32, i32) i32`
//!
//! # Architecture
//!
//! Type extraction returns a `Vec<String>` of all referenced type names.
//! This enables creating both `TypeOf` edges (using the full type string)
//! and Reference edges (using individual type names).
//!
//! # Examples
//!
//! ```text
//! // *User → vec!["User"]
//! // []const u8 → vec!["u8"]
//! // ?*User → vec!["User"]
//! // FileError!User → vec!["FileError", "User"]
//! ```

use tree_sitter::Node;

/// Extract all type names referenced in a Zig type node.
///
/// Returns a vector of type names that should have Reference edges created.
/// For simple types, returns a single element. For complex types (pointers,
/// slices, optionals, error unions), returns all nested type names.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-zig
/// * `content` - The source file bytes for text extraction
///
/// # Returns
///
/// Vector of type names referenced in the type annotation.
/// Empty vector if the type cannot be parsed.
///
/// # Examples
///
/// ```text
/// // Simple type: i32
/// extract_type_names_from_zig_type(node, content) → vec!["i32"]
///
/// // Pointer: *User
/// extract_type_names_from_zig_type(node, content) → vec!["User"]
///
/// // Slice: []const u8
/// extract_type_names_from_zig_type(node, content) → vec!["u8"]
///
/// // Optional: ?User
/// extract_type_names_from_zig_type(node, content) → vec!["User"]
///
/// // Error union: FileError!User
/// extract_type_names_from_zig_type(node, content) → vec!["FileError", "User"]
/// ```
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn extract_type_names_from_zig_type(node: Node, content: &[u8]) -> Vec<String> {
    match node.kind() {
        // Primitive/builtin types: i32, u64, f32, bool, void, etc.
        "builtin_type" => {
            if let Ok(type_name) = node.utf8_text(content) {
                vec![type_name.to_string()]
            } else {
                Vec::new()
            }
        }

        // Simple identifier: User, Point, etc.
        "identifier" => {
            if let Ok(type_name) = node.utf8_text(content) {
                // Only return if it's a valid type name (not a keyword)
                if is_valid_type_name(type_name) {
                    vec![type_name.to_string()]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }

        // Pointer types: *T, *const T, *volatile T, [*]T, [*c]T
        "pointer_type" | "PtrType" => extract_pointer_base_type(node, content),

        // Slice types: []T, []const T, [:0]T
        "slice_type" | "SliceType" => extract_slice_element_type(node, content),

        // Array types: [N]T
        "array_type" | "ArrayType" => extract_array_element_type(node, content),

        // Optional types: ?T
        "optional_type" | "OptionalType" | "nullable_type" => {
            // Extract wrapped type (T from ?T)
            if let Some(wrapped) = find_child_type_node(node) {
                extract_type_names_from_zig_type(wrapped, content)
            } else {
                Vec::new()
            }
        }

        // Error union types: !T, ErrorSet!T
        "error_union_type" | "ErrorUnionType" => {
            let mut types = Vec::new();

            // Count type children to distinguish explicit vs implicit error sets
            let mut type_children: Vec<Node> = vec![];
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    type_children.push(child);
                }
            }

            // If there are 2 type children: ErrorSet!Payload (explicit error set)
            // If there is 1 type child: !Payload (implicit anyerror)
            if type_children.len() == 2 {
                // Explicit error set: extract both
                types.extend(extract_type_names_from_zig_type(type_children[0], content));
                types.extend(extract_type_names_from_zig_type(type_children[1], content));
            } else if type_children.len() == 1 {
                // Implicit anyerror: only extract payload
                types.extend(extract_type_names_from_zig_type(type_children[0], content));
            }

            types
        }

        // Function types: fn(i32, i32) i32
        "function_type" | "FnProto" | "fn_proto" => {
            let mut types = Vec::new();

            // Extract parameter types
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "parameter" || child.kind() == "ParamDecl" {
                    if let Some(param_type) = find_param_type_node(child) {
                        types.extend(extract_type_names_from_zig_type(param_type, content));
                    }
                } else if child.kind() == "parameters" {
                    // Recurse into parameters node
                    let mut param_cursor = child.walk();
                    for param in child.children(&mut param_cursor) {
                        if param.kind() == "parameter"
                            && let Some(param_type) = find_param_type_node(param)
                        {
                            types.extend(extract_type_names_from_zig_type(param_type, content));
                        }
                    }
                }
            }

            // Extract return type (last type child is typically the return type)
            if let Some(return_type) = find_return_type_node(node) {
                types.extend(extract_type_names_from_zig_type(return_type, content));
            }

            types
        }

        // Struct/enum/union declarations
        "struct_declaration" | "enum_declaration" | "union_declaration" => {
            // These are type definitions, not type references
            // Return empty vector as we don't create references to the definition itself
            Vec::new()
        }

        // Error set declarations: error{...}
        "error_set_declaration" | "ErrorSetDecl" => {
            // Error set declaration, not a reference
            Vec::new()
        }

        // Generic/parameterized types: ArrayList(T), HashMap(K, V)
        "call_expression" => {
            let mut types = Vec::new();

            // Get the base type name (e.g., "ArrayList" from "ArrayList(T)")
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // First identifier or field_expression is the type constructor
                if child.kind() == "identifier" || child.kind() == "field_expression" {
                    types.extend(extract_type_names_from_zig_type(child, content));
                    break; // Only extract the first (the type constructor)
                }
            }

            // Extract type arguments (T, K, V, etc.)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // Arguments are not wrapped in "arguments" node for type calls,
                // they appear directly as child nodes between parentheses
                if child.kind() != "identifier"
                    && child.kind() != "field_expression"
                    && child.kind() != "("
                    && child.kind() != ")"
                    && child.kind() != ","
                    && is_type_node(child.kind())
                {
                    types.extend(extract_type_names_from_zig_type(child, content));
                }
            }

            types
        }

        // Namespaced/qualified types: std.mem.Allocator, package.Module.Type
        "field_expression" | "field_access" => {
            // Extract the full qualified name as text
            if let Ok(full_name) = node.utf8_text(content) {
                // Return the full qualified name
                if is_valid_type_name(full_name.trim()) {
                    vec![full_name.trim().to_string()]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }

        // Fallback: Try to recursively extract from children
        _ => {
            // Check if node has type children
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    types.extend(extract_type_names_from_zig_type(child, content));
                }
            }

            // If no children yielded types, try to extract text as identifier
            if types.is_empty() {
                if let Ok(text) = node.utf8_text(content) {
                    let text = text.trim();
                    if !text.is_empty() && is_valid_type_name(text) {
                        vec![text.to_string()]
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                types
            }
        }
    }
}

/// Check if a string is a Zig builtin type.
///
/// Builtin types are primitive types provided by the language.
fn is_builtin_type(s: &str) -> bool {
    matches!(
        s,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f16"
            | "f32"
            | "f64"
            | "f80"
            | "f128"
            | "bool"
            | "void"
            | "noreturn"
            | "type"
            | "anyerror"
            | "anyframe"
            | "anyopaque"
            | "c_char"
            | "c_short"
            | "c_int"
            | "c_long"
            | "c_longlong"
            | "c_uchar"
            | "c_ushort"
            | "c_uint"
            | "c_ulong"
            | "c_ulonglong"
            | "c_longdouble"
            | "comptime_int"
            | "comptime_float"
    )
}

/// Check if a string looks like a valid Zig type name.
///
/// Valid type names:
/// - Start with alphanumeric character or underscore
/// - Not a Zig keyword
/// - Builtin types are valid
fn is_valid_type_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Builtin types are always valid
    if is_builtin_type(s) {
        return true;
    }

    // Exclude Zig keywords that aren't types
    if matches!(
        s,
        "var"
            | "const"
            | "fn"
            | "pub"
            | "export"
            | "extern"
            | "inline"
            | "if"
            | "else"
            | "while"
            | "for"
            | "switch"
            | "return"
            | "break"
            | "continue"
            | "defer"
            | "errdefer"
            | "unreachable"
            | "try"
            | "catch"
            | "and"
            | "or"
            | "orelse"
            | "struct"
            | "enum"
            | "union"
            | "opaque"
            | "error"
            | "test"
            | "comptime"
            | "nosuspend"
            | "resume"
            | "suspend"
            | "await"
            | "async"
            | "threadlocal"
            | "allowzero"
            | "align"
            | "volatile"
            | "packed"
    ) {
        return false;
    }

    // Valid type names start with alphanumeric or underscore
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Extract base type from pointer node, skipping modifiers.
///
/// Handles: *T, *const T, *volatile T, [*]T, [*c]T
/// Returns the base type T.
fn extract_pointer_base_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Skip modifier keywords (const, volatile, align, allowzero, etc.)
        // Extract the actual type node
        if is_type_node(child.kind()) {
            return extract_type_names_from_zig_type(child, content);
        }
    }
    Vec::new()
}

/// Extract element type from slice node.
///
/// Handles: []T, []const T, [:0]T
/// Returns the element type T.
fn extract_slice_element_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Skip brackets, const keyword, sentinel values
        if is_type_node(child.kind()) {
            return extract_type_names_from_zig_type(child, content);
        }
    }
    Vec::new()
}

/// Extract element type from array node.
///
/// Handles: [N]T
/// Returns the element type T.
fn extract_array_element_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    let mut found_size = false;

    for child in node.children(&mut cursor) {
        // Skip brackets
        if child.kind() == "[" || child.kind() == "]" {
            continue;
        }

        // First non-bracket child is typically the size expression
        if !found_size && !is_type_node(child.kind()) {
            found_size = true;
            continue;
        }

        // After size, next type node is the element type
        if is_type_node(child.kind()) {
            return extract_type_names_from_zig_type(child, content);
        }
    }
    Vec::new()
}

/// Check if a node kind represents a type node.
fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "builtin_type"
            | "identifier"
            | "pointer_type"
            | "PtrType"
            | "slice_type"
            | "SliceType"
            | "array_type"
            | "ArrayType"
            | "optional_type"
            | "OptionalType"
            | "nullable_type"
            | "error_union_type"
            | "ErrorUnionType"
            | "function_type"
            | "FnProto"
            | "fn_proto"
            | "error_set_declaration"
            | "ErrorSetDecl"
            | "struct_declaration"
            | "enum_declaration"
            | "union_declaration"
            | "call_expression"      // Generic types: ArrayList(T)
            | "field_expression"      // Namespaced types: std.mem.Allocator
            | "field_access" // Alternative for field access
    )
}

/// Find the first child that is a type node.
fn find_child_type_node(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| is_type_node(child.kind()))
}

/// Find error set node in error union type (left side of !).
///
/// Returns the error set identifier or declaration.
/// Find parameter type node in parameter declaration.
///
/// Extracts the type from: `name: Type`
fn find_param_type_node(node: Node) -> Option<Node> {
    // Parameter structure: identifier, ":", type
    let mut found_colon = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == ":" {
            found_colon = true;
            continue;
        }

        // After colon, next type node is the parameter type
        if found_colon && is_type_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

/// Find return type node in function prototype.
///
/// Returns the return type (after parameters).
fn find_return_type_node(node: Node) -> Option<Node> {
    // Function prototype structure:
    // fn (params) return_type
    // or: fn identifier(params) return_type
    //
    // Return type is typically the last type child after parameters

    let mut found_params = false;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Mark that we've seen the parameters
        if child.kind() == "parameters" || child.kind() == ")" {
            found_params = true;
            continue;
        }

        // After parameters, next type node is the return type
        if found_params && is_type_node(child.kind()) {
            return Some(child);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_zig_code(code: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .expect("Failed to load Zig grammar");
        parser
            .parse(code.as_bytes(), None)
            .expect("Failed to parse Zig code")
    }

    #[test]
    fn test_is_builtin_type() {
        assert!(is_builtin_type("i32"));
        assert!(is_builtin_type("u64"));
        assert!(is_builtin_type("f32"));
        assert!(is_builtin_type("bool"));
        assert!(is_builtin_type("void"));
        assert!(is_builtin_type("usize"));
        assert!(is_builtin_type("anyerror"));

        assert!(!is_builtin_type("User"));
        assert!(!is_builtin_type("Point"));
        assert!(!is_builtin_type("var"));
    }

    #[test]
    fn test_is_valid_type_name() {
        // Builtin types are valid
        assert!(is_valid_type_name("i32"));
        assert!(is_valid_type_name("bool"));

        // Custom types are valid
        assert!(is_valid_type_name("User"));
        assert!(is_valid_type_name("Point"));
        assert!(is_valid_type_name("MyType"));
        assert!(is_valid_type_name("_Private"));

        // Keywords are not valid type names
        assert!(!is_valid_type_name("var"));
        assert!(!is_valid_type_name("const"));
        assert!(!is_valid_type_name("fn"));
        assert!(!is_valid_type_name("struct"));
        assert!(!is_valid_type_name("return"));

        // Empty string is not valid
        assert!(!is_valid_type_name(""));
    }

    #[test]
    fn test_is_type_node() {
        assert!(is_type_node("builtin_type"));
        assert!(is_type_node("identifier"));
        assert!(is_type_node("pointer_type"));
        assert!(is_type_node("slice_type"));
        assert!(is_type_node("array_type"));
        assert!(is_type_node("optional_type"));
        assert!(is_type_node("error_union_type"));

        assert!(!is_type_node("const"));
        assert!(!is_type_node("var"));
        assert!(!is_type_node(":"));
    }

    #[test]
    fn test_extract_builtin_type() {
        let code = "const x: i32 = 42;";
        let tree = parse_zig_code(code);
        let root = tree.root_node();

        // Navigate to the builtin_type node
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "variable_declaration" {
                let mut var_cursor = child.walk();
                for var_child in child.children(&mut var_cursor) {
                    if var_child.kind() == "builtin_type" {
                        let types = extract_type_names_from_zig_type(var_child, code.as_bytes());
                        assert_eq!(types, vec!["i32"]);
                        return;
                    }
                }
            }
        }

        panic!("Failed to find builtin_type node");
    }
}
