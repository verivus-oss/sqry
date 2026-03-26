//! Type extraction from Scala AST nodes.
//!
//! This module provides functions to extract type names from Scala type annotation nodes.
//! Scala has native type annotations (like Kotlin and Go), enabling direct AST-based type extraction.
//!
//! # Supported Type Constructs
//!
//! - Simple types: `String`, `Int`, `User`
//! - Generic types: `List[T]`, `Map[K, V]` (uses brackets, not angle brackets)
//! - Tuple types: `(String, Int, Boolean)`
//! - Function types: `String => Int`, `(A, B) => C`
//! - Compound types: `Serializable with Cloneable`
//! - Existential types: `List[_]`, `Map[String, _]` (wildcard types)
//! - Variance annotations: `List[+T]`, `Map[-K, +V]`
//! - Type projections: `Outer#Inner`
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
//! // Map[String, User] → vec!["Map", "String", "User"]
//! // List[Result[Data, Error]] → vec!["List", "Result", "Data", "Error"]
//! // (Int, String) => Boolean → vec!["Int", "String", "Boolean"]
//! ```

use tree_sitter::Node;

/// Extract the full type signature as a string from a Scala type node.
///
/// Returns the complete type annotation text, which is used for creating `TypeOf` edges.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-scala
/// * `content` - The source file bytes for text extraction
///
/// # Returns
///
/// The full type signature as a string, or an empty string if extraction fails.
///
/// # Examples
///
/// ```text
/// // List[String] → "List[String]"
/// // Map[String, User] → "Map[String, User]"
/// // (Int, String) => Boolean → "(Int, String) => Boolean"
/// ```
#[must_use]
pub fn extract_type_string(node: Node, content: &[u8]) -> String {
    node.utf8_text(content)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// Extract all type names referenced in a Scala type node.
///
/// Returns a vector of type names that should have Reference edges created.
/// For simple types, returns a single element. For complex types (generics,
/// function types, etc.), returns all nested type names.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-scala
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
/// // Simple type: String
/// extract_all_type_names_from_scala_type(node, content) → vec!["String"]
///
/// // Generic: List[String]
/// extract_all_type_names_from_scala_type(node, content) → vec!["List", "String"]
///
/// // Map: Map[String, User]
/// extract_all_type_names_from_scala_type(node, content) → vec!["Map", "String", "User"]
///
/// // Function type: (Int, String) => Boolean
/// extract_all_type_names_from_scala_type(node, content) → vec!["Int", "String", "Boolean"]
///
/// // Tuple: (String, Int, Boolean)
/// extract_all_type_names_from_scala_type(node, content) → vec!["String", "Int", "Boolean"]
/// ```
#[must_use]
pub fn extract_all_type_names_from_scala_type(node: Node, content: &[u8]) -> Vec<String> {
    match node.kind() {
        // Simple type identifier: String, Int, User
        "type_identifier" | "identifier" | "stable_identifier" => {
            if let Ok(type_name) = node.utf8_text(content) {
                vec![clean_type_name(type_name)]
            } else {
                Vec::new()
            }
        }

        // Generic type: List[T], Map[K, V]
        // Structure: type_identifier + type_arguments
        "generic_type" | "parameterized_type" => extract_generic_type(node, content),

        // Tuple type: (String, Int, Boolean)
        "tuple_type" => extract_tuple_type(node, content),

        // Function type: String => Int, (A, B) => C
        "function_type" => extract_function_type_names(node, content),

        // Compound type (intersection type): Trait1 with Trait2
        "compound_type" | "infix_type" => extract_compound_type(node, content),

        // Type projection: Outer#Inner
        "type_projection" => extract_type_projection(node, content),

        // Annotated type: type with annotations
        "annotated_type" => {
            // Extract the underlying type, skip annotations
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    return extract_all_type_names_from_scala_type(child, content);
                }
            }
            Vec::new()
        }

        // Existential type: List[_], Map[String, _]
        // The '_' wildcard is not a type reference
        "wildcard_type" | "_" => Vec::new(),

        // Parenthesized type: (Type)
        "parenthesized_type" => {
            if let Some(inner_type) = node.named_child(0) {
                extract_all_type_names_from_scala_type(inner_type, content)
            } else {
                Vec::new()
            }
        }

        // Variance annotations: +T, -T
        // Skip the variance marker, extract the type
        _ if node
            .utf8_text(content)
            .is_ok_and(|t| t.starts_with('+') || t.starts_with('-')) =>
        {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    return extract_all_type_names_from_scala_type(child, content);
                }
            }
            Vec::new()
        }

        // Fallback: try to extract text as a single type name
        _ => {
            if let Ok(text) = node.utf8_text(content) {
                let text = text.trim();
                if !text.is_empty() && !text.starts_with('_') && is_valid_type_name(text) {
                    vec![clean_type_name(text)]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
    }
}

/// Extract type names from a generic/parameterized type node.
///
/// Examples:
/// - `List[String]` → vec!["List", "String"]
/// - `Map[String, User]` → vec!["Map", "String", "User"]
/// - `List[Map[K, V]]` → vec!["List", "Map", "K", "V"]
fn extract_generic_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();

    // Get the base type name (before the brackets)
    if let Some(type_node) = node.child_by_field_name("type") {
        types.extend(extract_all_type_names_from_scala_type(type_node, content));
    } else {
        // Fallback: find first type_identifier
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(child.kind(), "type_identifier" | "identifier")
                && let Ok(name) = child.utf8_text(content)
            {
                types.push(clean_type_name(name));
                break;
            }
        }
    }

    // Extract type arguments (inside brackets)
    if let Some(type_args_node) = node.child_by_field_name("type_arguments") {
        extract_type_arguments(type_args_node, content, &mut types);
    } else {
        // Fallback: look for type_arguments node
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_arguments" {
                extract_type_arguments(child, content, &mut types);
            }
        }
    }

    types
}

/// Extract type names from `type_arguments` node (the part inside brackets).
///
/// Examples:
/// - `[String]` → vec!["String"]
/// - `[String, User]` → vec!["String", "User"]
/// - `[Map[K, V]]` → vec!["Map", "K", "V"]
fn extract_type_arguments(type_args_node: Node, content: &[u8], types: &mut Vec<String>) {
    let mut cursor = type_args_node.walk();
    for child in type_args_node.children(&mut cursor) {
        // Skip brackets and commas
        if child.kind() == "[" || child.kind() == "]" || child.kind() == "," {
            continue;
        }

        // Extract type from each argument
        if is_type_node(child.kind()) {
            types.extend(extract_all_type_names_from_scala_type(child, content));
        }
    }
}

/// Extract type names from a tuple type node.
///
/// Examples:
/// - `(String, Int)` → vec!["String", "Int"]
/// - `(A, B, C)` → vec!["A", "B", "C"]
fn extract_tuple_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Skip parentheses and commas
        if child.kind() == "(" || child.kind() == ")" || child.kind() == "," {
            continue;
        }

        if is_type_node(child.kind()) {
            types.extend(extract_all_type_names_from_scala_type(child, content));
        }
    }

    types
}

/// Extract type names from a function type node.
///
/// Examples:
/// - `String => Int` → vec!["String", "Int"]
/// - `(A, B) => C` → vec!["A", "B", "C"]
/// - `(Int, String) => Boolean` → vec!["Int", "String", "Boolean"]
fn extract_function_type_names(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            // Skip the arrow
            "=>" => {}

            // Parameter types node: (String, Int)
            "parameter_types" => {
                let mut param_cursor = child.walk();
                for param_child in child.children(&mut param_cursor) {
                    if is_type_node(param_child.kind()) {
                        types.extend(extract_all_type_names_from_scala_type(param_child, content));
                    }
                }
            }

            // Direct type nodes (return type or simple parameter)
            _ if is_type_node(child.kind()) => {
                types.extend(extract_all_type_names_from_scala_type(child, content));
            }

            _ => {}
        }
    }

    types
}

/// Extract type names from a compound type (intersection type).
///
/// Examples:
/// - `Serializable with Cloneable` → vec!["Serializable", "Cloneable"]
/// - `A with B with C` → vec!["A", "B", "C"]
fn extract_compound_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Skip "with" keyword
        if child.kind() == "with" {
            continue;
        }

        if is_type_node(child.kind()) {
            types.extend(extract_all_type_names_from_scala_type(child, content));
        }
    }

    types
}

/// Extract type names from a type projection (Outer#Inner).
///
/// Examples:
/// - `Outer#Inner` → vec!["Outer", "Inner"]
fn extract_type_projection(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        // Skip the # symbol
        if child.kind() == "#" {
            continue;
        }

        if is_type_node(child.kind()) {
            types.extend(extract_all_type_names_from_scala_type(child, content));
        }
    }

    types
}

/// Check if a node kind represents a type node in Scala.
#[must_use]
pub fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "identifier"
            | "stable_identifier"
            | "generic_type"
            | "parameterized_type"
            | "tuple_type"
            | "function_type"
            | "compound_type"
            | "infix_type"
            | "type_projection"
            | "annotated_type"
            | "parenthesized_type"
    )
}

/// Check if a string looks like a valid Scala type name.
///
/// Valid type names:
/// - Start with uppercase letter (by convention)
/// - Contain only alphanumeric characters and underscores
/// - Exclude Scala keywords that aren't types
fn is_valid_type_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Exclude common keywords that aren't type names
    if matches!(
        s,
        "val"
            | "var"
            | "def"
            | "class"
            | "object"
            | "trait"
            | "type"
            | "case"
            | "sealed"
            | "abstract"
            | "final"
            | "implicit"
            | "lazy"
            | "override"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "if"
            | "else"
            | "match"
            | "for"
            | "while"
            | "do"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "new"
            | "this"
            | "super"
            | "null"
            | "true"
            | "false"
            | "with"
            | "extends"
            | "import"
            | "package"
            | "yield"
    ) {
        return false;
    }

    // Valid type names start with alphanumeric
    s.chars().next().is_some_and(char::is_alphanumeric)
}

/// Clean a type name by removing common artifacts.
///
/// - Remove leading/trailing whitespace
/// - Remove generic parameter brackets if they're at the end
/// - Remove package qualifiers (keep just the type name)
/// - Remove variance annotations (+, -)
#[must_use]
pub fn clean_type_name(s: &str) -> String {
    let s = s.trim();

    // Remove variance annotations
    let s = s.trim_start_matches('+').trim_start_matches('-');

    // If the type has generic parameters, extract just the base name
    let s = if let Some(bracket_pos) = s.find('[') {
        &s[..bracket_pos]
    } else {
        s
    };

    // Remove package qualifiers (keep just the type name)
    // Example: scala.collection.List → List
    let s = if let Some(dot_pos) = s.rfind('.') {
        &s[dot_pos + 1..]
    } else {
        s
    };

    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_type_name() {
        assert!(is_valid_type_name("String"));
        assert!(is_valid_type_name("Int"));
        assert!(is_valid_type_name("User"));
        assert!(is_valid_type_name("MyType"));

        assert!(!is_valid_type_name("val"));
        assert!(!is_valid_type_name("var"));
        assert!(!is_valid_type_name("def"));
        assert!(!is_valid_type_name(""));
    }

    #[test]
    fn test_clean_type_name() {
        assert_eq!(clean_type_name("String"), "String");
        assert_eq!(clean_type_name("List[String]"), "List");
        assert_eq!(clean_type_name("  User  "), "User");
        assert_eq!(clean_type_name("scala.collection.List"), "List");
        assert_eq!(clean_type_name("+T"), "T");
        assert_eq!(clean_type_name("-K"), "K");
    }

    #[test]
    fn test_is_type_node() {
        assert!(is_type_node("type_identifier"));
        assert!(is_type_node("generic_type"));
        assert!(is_type_node("tuple_type"));
        assert!(is_type_node("function_type"));

        assert!(!is_type_node("val_definition"));
        assert!(!is_type_node("function_definition"));
    }
}
