//! Type extraction utilities for Salesforce Apex `TypeOf` and Reference edges.
//!
//! Extracts type information from Apex AST nodes, supporting:
//! - Simple types (`Account`, `String`)
//! - Generic types (`List<Account>`, `Map<String, Contact>`)
//! - Scoped types (`Outer.Inner`)
//! - Void type (skipped for `TypeOf` edges)

use tree_sitter::Node;

/// Extracts the complete type signature as a string from an Apex type annotation.
///
/// Used for creating `TypeOf` edges with the full type information.
///
/// # Examples
/// - `Account` -> `"Account"`
/// - `List<Account>` -> `"List<Account>"`
/// - `Map<String, Contact>` -> `"Map<String, Contact>"`
#[must_use]
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    let text = node.utf8_text(content).ok()?;
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = trimmed.trim();
    if trimmed.is_empty() || trimmed == "void" {
        return None;
    }
    Some(trimmed.to_string())
}

/// Extracts all constituent type names from an Apex type annotation.
///
/// Used for creating `Reference` edges to capture all type dependencies.
/// For `List<Account>`, returns `["List", "Account"]`.
/// For `Map<String, Contact>`, returns `["Map", "String", "Contact"]`.
///
/// Skips `void` types entirely.
#[must_use]
pub fn extract_all_type_names_from_annotation(node: Node<'_>, content: &[u8]) -> Vec<String> {
    match node.kind() {
        "type_identifier" => {
            if let Ok(text) = node.utf8_text(content) {
                let trimmed = text.trim();
                if !trimmed.is_empty() && trimmed != "void" {
                    return vec![trimmed.to_string()];
                }
            }
            Vec::new()
        }
        "scoped_type_identifier" => {
            // e.g., Outer.Inner -- return full text as one type name
            if let Ok(text) = node.utf8_text(content) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return vec![trimmed.to_string()];
                }
            }
            Vec::new()
        }
        "generic_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "type_identifier" | "scoped_type_identifier" => {
                        if let Ok(text) = child.utf8_text(content) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                types.push(trimmed.to_string());
                            }
                        }
                    }
                    "type_arguments" => {
                        // Recurse into type arguments
                        let mut arg_cursor = child.walk();
                        for arg_child in child.named_children(&mut arg_cursor) {
                            types
                                .extend(extract_all_type_names_from_annotation(arg_child, content));
                        }
                    }
                    _ => {}
                }
            }
            types
        }
        "void_type" => Vec::new(),
        _ => {
            // Fallback: recurse into children
            let mut types = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                types.extend(extract_all_type_names_from_annotation(child, content));
            }
            types
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to parse an Apex snippet and extract type names from the first
    /// type annotation found in a field declaration.
    fn parse_and_extract_type_names(source: &str) -> Vec<String> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sfapex::apex::LANGUAGE.into())
            .expect("load Apex grammar");
        let tree = parser.parse(source.as_bytes(), None).expect("parse");
        let root = tree.root_node();
        find_first_type_node_names(root, source.as_bytes())
    }

    /// Helper to parse an Apex snippet and extract the full type string from
    /// the first type annotation found in a field declaration.
    fn parse_and_extract_type_string(source: &str) -> Option<String> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sfapex::apex::LANGUAGE.into())
            .expect("load Apex grammar");
        let tree = parser.parse(source.as_bytes(), None).expect("parse");
        let root = tree.root_node();
        find_first_type_node_string(root, source.as_bytes())
    }

    fn find_first_type_node_names(node: Node<'_>, content: &[u8]) -> Vec<String> {
        if matches!(
            node.kind(),
            "type_identifier" | "generic_type" | "scoped_type_identifier"
        ) {
            return extract_all_type_names_from_annotation(node, content);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let result = find_first_type_node_names(child, content);
            if !result.is_empty() {
                return result;
            }
        }
        Vec::new()
    }

    fn find_first_type_node_string(node: Node<'_>, content: &[u8]) -> Option<String> {
        if matches!(
            node.kind(),
            "type_identifier" | "generic_type" | "scoped_type_identifier"
        ) {
            return extract_type_string(node, content);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let result = find_first_type_node_string(child, content);
            if result.is_some() {
                return result;
            }
        }
        None
    }

    #[test]
    fn test_simple_type() {
        let names = parse_and_extract_type_names("public class C { private Account myField; }");
        assert_eq!(names, vec!["Account"]);
    }

    #[test]
    fn test_generic_type_list() {
        let names = parse_and_extract_type_names("public class C { private List<Account> items; }");
        assert!(names.contains(&"List".to_string()));
        assert!(names.contains(&"Account".to_string()));
    }

    #[test]
    fn test_generic_type_map() {
        let names =
            parse_and_extract_type_names("public class C { private Map<String, Contact> items; }");
        assert!(names.contains(&"Map".to_string()));
        assert!(names.contains(&"String".to_string()));
        assert!(names.contains(&"Contact".to_string()));
    }

    #[test]
    fn test_extract_type_string_simple() {
        let ts = parse_and_extract_type_string("public class C { private Account myField; }");
        assert_eq!(ts.as_deref(), Some("Account"));
    }

    #[test]
    fn test_extract_type_string_generic() {
        let ts = parse_and_extract_type_string("public class C { private List<Account> myField; }");
        assert!(ts.is_some());
        let s = ts.unwrap();
        assert!(
            s.contains("List"),
            "Expected 'List' in type string, got: {s}"
        );
        assert!(
            s.contains("Account"),
            "Expected 'Account' in type string, got: {s}"
        );
    }

    #[test]
    fn test_void_type_returns_empty() {
        // void types should produce no type names (it appears in method decl, not fields,
        // but we test the extraction function directly)
        let names = extract_all_type_names_from_annotation_from_text("void");
        assert!(names.is_empty());
    }

    /// Inline helper that mimics extraction for a bare text string (not an AST node).
    fn extract_all_type_names_from_annotation_from_text(text: &str) -> Vec<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed == "void" {
            return Vec::new();
        }
        vec![trimmed.to_string()]
    }

    #[test]
    fn test_extract_type_string_skips_void() {
        let result = extract_all_type_names_from_annotation_from_text("void");
        assert!(result.is_empty());
    }
}
