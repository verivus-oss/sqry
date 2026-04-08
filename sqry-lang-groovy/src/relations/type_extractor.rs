//! Type extraction from Groovy AST nodes.
//!
//! This module provides functions to extract type names from Groovy type annotation nodes.
//! Groovy has optional type annotations (can use `def` for dynamic typing), enabling
//! direct AST-based type extraction when types are explicitly declared.
//!
//! # Supported Type Constructs
//!
//! - Simple types: `String`, `Integer`, `User`
//! - Builtin types: `int`, `void`, `long`, `double`, `boolean`
//! - Generic types: `List<T>`, `Map<K, V>`, `Closure<Integer>`
//! - Nested generics: `List<Map<String, User>>`
//! - Array types: `String[]`, `int[]`
//! - Dynamic types: `def` (skipped - no type information)
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
//! // Map<String, User> → vec!["Map", "String", "User"]
//! // List<Result<Data, Error>> → vec!["List", "Result", "Data", "Error"]
//! // Closure<Integer> → vec!["Closure", "Integer"]
//! ```

use tree_sitter::Node;

/// Extract all type names referenced in a Groovy type node.
///
/// Returns a vector of type names that should have Reference edges created.
/// For simple types, returns a single element. For complex types (generics, etc.),
/// returns all nested type names.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-groovy
/// * `content` - The source file bytes for text extraction
///
/// # Returns
///
/// Vector of type names referenced in the type annotation.
/// Empty vector if the type cannot be parsed or is dynamic (`def`).
///
/// # Examples
///
/// ```text
/// // Simple type: String
/// extract_all_type_names_from_groovy_type(node, content) → vec!["String"]
///
/// // Builtin: int
/// extract_all_type_names_from_groovy_type(node, content) → vec!["int"]
///
/// // Generic: List<String>
/// extract_all_type_names_from_groovy_type(node, content) → vec!["List", "String"]
///
/// // Map: Map<String, User>
/// extract_all_type_names_from_groovy_type(node, content) → vec!["Map", "String", "User"]
///
/// // Dynamic: def
/// extract_all_type_names_from_groovy_type(node, content) → vec![] (empty)
/// ```
#[must_use]
pub fn extract_all_type_names_from_groovy_type(node: Node, content: &[u8]) -> Vec<String> {
    match node.kind() {
        // Simple type identifiers: String, Integer, User, List
        "identifier" => {
            if let Ok(type_name) = node.utf8_text(content) {
                // Skip "def" keyword (dynamic typing)
                if type_name == "def" {
                    return Vec::new();
                }
                vec![clean_type_name(type_name)]
            } else {
                Vec::new()
            }
        }

        // Builtin types: int, void, long, double, boolean, etc.
        "builtintype" => {
            if let Ok(type_name) = node.utf8_text(content) {
                vec![clean_type_name(type_name)]
            } else {
                Vec::new()
            }
        }

        // Generic types: List<String>, Map<K, V>, Closure<Integer>
        // Generic parameter list: <String>, <K, V>
        // Array types: String[], int[]
        "type_with_generics" | "generics" | "array_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    types.extend(extract_all_type_names_from_groovy_type(child, content));
                }
            }

            types
        }

        // Fallback: try to extract text as a single type name
        _ => {
            if let Ok(text) = node.utf8_text(content) {
                let text = text.trim();
                if !text.is_empty() && is_valid_type_name(text) {
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

/// Extract the full type string from a Groovy type node.
///
/// Used for creating `TypeOf` edges with the complete type signature.
///
/// # Arguments
///
/// * `node` - The type annotation AST node
/// * `content` - The source file bytes
///
/// # Returns
///
/// The full type string (e.g., "List<Map<String, User>>"), or None if extraction fails.
///
/// # Examples
///
/// ```text
/// // Simple: String → "String"
/// // Generic: List<String> → "List<String>"
/// // Nested: List<Map<String, User>> → "List<Map<String, User>>"
/// // Dynamic: def → None (skip dynamic types)
/// ```
#[must_use]
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    // Skip "def" keyword (dynamic typing)
    if let Ok(text) = node.utf8_text(content)
        && text.trim() == "def"
    {
        return None;
    }

    node.utf8_text(content).ok().map(str::to_string)
}

/// Check if a node kind represents a type node.
///
/// In Groovy AST, types can be represented as:
/// - `identifier` - user-defined types (String, User, List)
/// - `builtintype` - primitive types (int, void, boolean)
/// - `type_with_generics` - generic types (`List<String>`)
/// - `generics` - type parameter list
/// - `array_type` - array types (String[])
#[must_use]
pub fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "builtintype" | "type_with_generics" | "generics" | "array_type"
    )
}

/// Check if a string looks like a valid Groovy type name.
///
/// Valid type names:
/// - Start with uppercase letter (by convention for user types)
/// - Or are lowercase builtin types (int, void, boolean, etc.)
/// - Contain only alphanumeric characters and underscores
/// - Exclude Groovy keywords that aren't types
fn is_valid_type_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Skip "def" keyword
    if s == "def" {
        return false;
    }

    // Exclude common keywords that aren't type names
    if matches!(
        s,
        "var"
            | "final"
            | "const"
            | "class"
            | "enum"
            | "extends"
            | "implements"
            | "interface"
            | "abstract"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "break"
            | "continue"
            | "switch"
            | "case"
            | "default"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "import"
            | "package"
            | "as"
            | "in"
            | "new"
            | "this"
            | "super"
            | "null"
            | "true"
            | "false"
            | "static"
            | "private"
            | "protected"
            | "public"
    ) {
        return false;
    }

    // Valid type names start with alphanumeric character
    s.chars().next().is_some_and(char::is_alphanumeric)
}

/// Clean a type name by removing common artifacts.
///
/// - Remove leading/trailing whitespace
/// - Remove array brackets if they're at the end
/// - Remove generic parameter brackets if they're at the end
#[must_use]
pub fn clean_type_name(s: &str) -> String {
    let s = s.trim();

    // If the type has generic parameters, extract just the base name
    let s = if let Some(bracket_pos) = s.find('<') {
        &s[..bracket_pos]
    } else {
        s
    };

    // If the type has array brackets, extract just the base name
    let s = if let Some(bracket_pos) = s.find('[') {
        &s[..bracket_pos]
    } else {
        s
    };

    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::plugin::LanguagePlugin;

    fn parse_groovy_type(type_str: &str) -> tree_sitter::Tree {
        let source = format!("{type_str} x = null");
        let plugin = crate::GroovyPlugin::default();
        plugin
            .parse_ast(source.as_bytes())
            .expect("Failed to parse Groovy code")
    }

    fn extract_from_type_str(type_str: &str) -> Vec<String> {
        let source = format!("{type_str} x = null");
        let tree = parse_groovy_type(type_str);

        // Find the type annotation node - traverse to find type nodes
        #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
        fn find_type_node(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
            if is_type_node(node.kind()) {
                return Some(node);
            }

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(type_node) = find_type_node(child) {
                    return Some(type_node);
                }
            }

            None
        }

        let root = tree.root_node();
        if let Some(type_node) = find_type_node(root) {
            extract_all_type_names_from_groovy_type(type_node, source.as_bytes())
        } else {
            Vec::new()
        }
    }

    #[test]
    fn test_simple_type() {
        let types = extract_from_type_str("String");
        assert_eq!(types, vec!["String"]);
    }

    #[test]
    fn test_builtin_type() {
        let types = extract_from_type_str("int");
        assert_eq!(types, vec!["int"]);
    }

    #[test]
    fn test_void_type() {
        let types = extract_from_type_str("void");
        assert_eq!(types, vec!["void"]);
    }

    #[test]
    fn test_generic_list() {
        let types = extract_from_type_str("List<String>");
        assert!(types.contains(&"List".to_string()));
        assert!(types.contains(&"String".to_string()));
    }

    #[test]
    fn test_generic_map() {
        let types = extract_from_type_str("Map<String, Integer>");
        assert!(types.contains(&"Map".to_string()));
        assert!(types.contains(&"String".to_string()));
        assert!(types.contains(&"Integer".to_string()));
    }

    #[test]
    fn test_clean_type_name() {
        assert_eq!(clean_type_name("String"), "String");
        assert_eq!(clean_type_name("List<String>"), "List");
        assert_eq!(clean_type_name("String[]"), "String");
        assert_eq!(clean_type_name("  String  "), "String");
    }

    #[test]
    fn test_is_valid_type_name() {
        assert!(is_valid_type_name("String"));
        assert!(is_valid_type_name("int"));
        assert!(is_valid_type_name("User"));
        assert!(is_valid_type_name("MyClass123"));

        assert!(!is_valid_type_name(""));
        assert!(!is_valid_type_name("def"));
        assert!(!is_valid_type_name("var"));
        assert!(!is_valid_type_name("class"));
        assert!(!is_valid_type_name("return"));
    }

    #[test]
    fn test_def_keyword_skipped() {
        // When using "def", Groovy doesn't put a type node in the AST at all.
        // So we test that if we encounter "def" as a type identifier, we skip it.
        // This test uses the public API directly rather than find_type_node helper.
        let plugin = crate::GroovyPlugin::default();
        let source = b"class Foo { def bar }";
        let tree = plugin.parse_ast(source).expect("Failed to parse");

        // Find any identifier nodes with text "def"
        #[allow(clippy::items_after_statements)] // Items near usage for clarity
        fn find_def_identifier<'a>(
            node: tree_sitter::Node<'a>,
            content: &[u8],
        ) -> Vec<tree_sitter::Node<'a>> {
            let mut results = Vec::new();
            if node.kind() == "identifier"
                && let Ok(text) = node.utf8_text(content)
                && text == "def"
            {
                results.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                results.extend(find_def_identifier(child, content));
            }
            results
        }

        // If we find a "def" identifier node, extracting types from it should return empty
        let def_nodes = find_def_identifier(tree.root_node(), source);
        for def_node in def_nodes {
            let types = extract_all_type_names_from_groovy_type(def_node, source);
            assert!(
                types.is_empty(),
                "def keyword should not be extracted as a type"
            );
        }
    }
}
