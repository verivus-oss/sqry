//! Type name extraction from Haskell AST nodes.
//!
//! This module provides AST-based extraction of individual type constructor names
//! from Haskell type annotations. It walks the tree-sitter-haskell AST and collects
//! all uppercase-starting identifiers (type constructors), excluding type variables
//! (lowercase identifiers like `a`, `b`, `m`).
//!
//! # Supported Type Constructs
//!
//! - Simple types: `Int`, `String`, `Bool`
//! - Applied types: `Maybe Int`, `Map String Int`
//! - Function types: `Int -> String -> Bool`
//! - Qualified types: `Data.Map.Map`
//! - Constraint contexts: `Show a => a -> String`
//! - Forall quantifiers: `forall a. a -> Int`
//! - Tuple types: `(Int, String)`
//! - List types: `[Int]`
//! - Strict/lazy fields: `!Int`, `~String`
//! - Infix type constructors: operands of `a :+: b` (operator excluded)
//!
//! # Architecture
//!
//! Returns a sorted, deduplicated `Vec<String>` of type constructor names.
//! This enables creating `References` edges from a symbol to each referenced type.
//!
//! # Examples
//!
//! ```text
//! // Int -> String → vec!["Int", "String"]
//! // Maybe (IO String) → vec!["IO", "Maybe", "String"]
//! // a -> Maybe b → vec!["Maybe"]   (type vars excluded)
//! // Data.Map.Map String Int → vec!["Data.Map.Map", "Int", "String"]
//! ```

use tree_sitter::Node;

/// Check whether a text token represents a Haskell type constructor.
///
/// Type constructors start with an uppercase letter (e.g., `Int`, `Maybe`, `Show`).
/// Type variables start with lowercase (e.g., `a`, `b`, `m`) and are excluded.
/// Operator-like tokens (e.g., `:+:`, `->`) are excluded.
fn is_type_constructor(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Extract all type constructor names referenced in a Haskell type AST node.
///
/// Recursively walks the tree-sitter-haskell type node and collects all type
/// constructor names (uppercase-starting identifiers). Type variables (lowercase)
/// are excluded.
///
/// Returns a **sorted, deduplicated** `Vec<String>`.
///
/// # Arguments
///
/// * `node` - A type annotation AST node from tree-sitter-haskell
/// * `content` - The source file bytes for text extraction
///
/// # Node Kind Handling
///
/// | Node Kind | Action |
/// |-----------|--------|
/// | `name` | Collect if starts with uppercase |
/// | `qualified` | Collect full text (e.g., `Data.Map.Map`) |
/// | `variable` | Skip (type variable) |
/// | `constructor` | Collect if starts with uppercase |
/// | `function`, `linear_function` | Recurse `parameter` + `result` |
/// | `forall`, `forall_required` | Skip `quantified_variables`, recurse rest |
/// | `context` | Recurse `context` field + `type` field |
/// | `apply`, `parens`, `tuple`, `list`, `constraints` | Recurse all named children |
/// | `strict_field`, `lazy_field`, `quantified_type` | Recurse all named children |
/// | `infix` | Recurse all named children |
/// | Other | Recurse defensively |
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn extract_type_names_from_haskell_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    collect_type_names(node, content, &mut names);
    names.sort();
    names.dedup();
    names
}

/// Recursive helper that collects type constructor names into the accumulator.
fn collect_type_names(node: Node, content: &[u8], names: &mut Vec<String>) {
    match node.kind() {
        // Leaf: type constructor or type name (e.g., Int, String, Show)
        #[allow(clippy::match_same_arms)] // Type extractor arms separated for documentation clarity
        "name" => {
            if let Ok(text) = node.utf8_text(content) {
                let trimmed = text.trim();
                if is_type_constructor(trimmed) {
                    names.push(trimmed.to_string());
                }
            }
        }

        // Leaf: qualified name (e.g., Data.Map.Map) — preserve full qualification
        "qualified" => {
            if let Ok(text) = node.utf8_text(content) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    names.push(trimmed.to_string());
                }
            }
        }

        // Leaf: type variable (a, b, m) — skip
        "variable" => {}

        // Leaf: constructor (may appear in type contexts)
        "constructor" => {
            if let Ok(text) = node.utf8_text(content) {
                let trimmed = text.trim();
                if is_type_constructor(trimmed) {
                    names.push(trimmed.to_string());
                }
            }
        }

        // Function type: parameter -> result
        "function" | "linear_function" => {
            if let Some(param) = node.child_by_field_name("parameter") {
                collect_type_names(param, content, names);
            }
            if let Some(result) = node.child_by_field_name("result") {
                collect_type_names(result, content, names);
            }
        }

        // Forall quantifier: skip quantified_variables, recurse into type body
        "forall" | "forall_required" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "quantified_variables" {
                    collect_type_names(child, content, names);
                }
            }
        }

        // Context: constraint(s) + inner type
        "context" => {
            if let Some(ctx) = node.child_by_field_name("context") {
                collect_type_names(ctx, content, names);
            }
            if let Some(ty) = node.child_by_field_name("type") {
                collect_type_names(ty, content, names);
            }
        }

        // Container nodes — recurse all named children
        #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
        "apply" | "parens" | "tuple" | "list" | "constraints" | "strict_field" | "lazy_field"
        | "quantified_type" | "infix" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_type_names(child, content, names);
            }
        }

        // Default: recurse defensively into named children
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_type_names(child, content, names);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a Haskell type signature and extract the type AST node for testing.
    fn parse_type_from_signature(code: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_haskell::LANGUAGE.into())
            .expect("Failed to set Haskell language");
        let content = code.as_bytes().to_vec();
        let tree = parser.parse(&content, None).expect("Failed to parse");
        (tree, content)
    }

    /// Find the `signature` node's `type` field from parsed Haskell code.
    fn find_signature_type_node<'a>(
        node: tree_sitter::Node<'a>,
        _content: &[u8],
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == "signature" {
            return node.child_by_field_name("type");
        }
        let mut cursor = node.walk();
        #[allow(clippy::used_underscore_binding)] // Underscore prefix indicates partial use pattern
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_signature_type_node(child, _content) {
                return Some(found);
            }
        }
        None
    }

    /// Extract type names from a Haskell type signature string.
    fn extract_from_sig(code: &str) -> Vec<String> {
        let (tree, content) = parse_type_from_signature(code);
        let type_node = find_signature_type_node(tree.root_node(), &content)
            .expect("No signature type node found");
        extract_type_names_from_haskell_type(type_node, &content)
    }

    #[test]
    fn test_simple_type() {
        let names = extract_from_sig("foo :: Int");
        assert_eq!(names, vec!["Int"]);
    }

    #[test]
    fn test_function_type() {
        let names = extract_from_sig("foo :: Int -> String -> Bool");
        assert_eq!(names, vec!["Bool", "Int", "String"]);
    }

    #[test]
    fn test_applied_type() {
        let names = extract_from_sig("foo :: Maybe Int");
        assert_eq!(names, vec!["Int", "Maybe"]);
    }

    #[test]
    fn test_nested_applied_type() {
        let names = extract_from_sig("foo :: IO (Maybe String)");
        assert_eq!(names, vec!["IO", "Maybe", "String"]);
    }

    #[test]
    fn test_type_variables_excluded() {
        let names = extract_from_sig("foo :: a -> b -> a");
        assert!(names.is_empty(), "Type variables should be excluded");
    }

    #[test]
    fn test_mixed_types_and_vars() {
        let names = extract_from_sig("foo :: a -> Maybe b -> Int");
        assert_eq!(names, vec!["Int", "Maybe"]);
    }

    #[test]
    fn test_constrained_type() {
        let names = extract_from_sig("foo :: Show a => a -> String");
        assert_eq!(names, vec!["Show", "String"]);
    }

    #[test]
    fn test_multi_constraint() {
        let names = extract_from_sig("foo :: (Show a, Ord a) => a -> String");
        assert_eq!(names, vec!["Ord", "Show", "String"]);
    }

    #[test]
    fn test_forall_type() {
        let names = extract_from_sig("foo :: forall a. a -> Int");
        assert_eq!(names, vec!["Int"]);
    }

    #[test]
    fn test_forall_constrained() {
        let names = extract_from_sig("foo :: forall a. Show a => a -> String");
        assert_eq!(names, vec!["Show", "String"]);
    }

    #[test]
    fn test_tuple_type() {
        let names = extract_from_sig("foo :: (Int, String)");
        assert_eq!(names, vec!["Int", "String"]);
    }

    #[test]
    fn test_list_type() {
        let names = extract_from_sig("foo :: [Int]");
        assert_eq!(names, vec!["Int"]);
    }

    #[test]
    fn test_qualified_type() {
        let names = extract_from_sig("foo :: Data.Map.Map String Int");
        assert_eq!(names, vec!["Data.Map.Map", "Int", "String"]);
    }

    #[test]
    fn test_deduplication() {
        // Int appears twice but should only appear once in output
        let names = extract_from_sig("foo :: Int -> Int");
        assert_eq!(names, vec!["Int"]);
    }

    #[test]
    fn test_complex_nested() {
        let names = extract_from_sig("foo :: IO String -> Maybe Int -> Bool");
        assert_eq!(names, vec!["Bool", "IO", "Int", "Maybe", "String"]);
    }
}
