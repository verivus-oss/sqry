//! Type name extraction from YARD type strings
//!
//! Delegates to `sqry_lang_support::type_extraction` for the shared
//! tokenize-and-filter algorithm. Only `canonical_type_string` remains
//! here because optional/nil suffix normalization is Ruby-specific.

use sqry_lang_support::type_extraction::{self, TypeExtractionConfig};

/// Extract all referenced type names from a YARD type string
///
/// Examples:
/// - `User` → `["User"]`
/// - `String, Integer` → `["Integer", "String"]`
/// - `Array<User>` → `["Array", "User"]`
/// - `Hash{String => Integer}` → `["Hash", "Integer", "String"]`
/// - `String, nil` → `["String"]`
/// - `String?` → `["String"]`
pub fn extract_type_names(type_str: &str) -> Vec<String> {
    type_extraction::extract_type_names(type_str, &TypeExtractionConfig::ruby())
}

/// Check if a token is a valid type name for Reference edges (test helper).
#[cfg(test)]
fn is_type_name(token: &str) -> bool {
    type_extraction::is_type_name(token, &TypeExtractionConfig::ruby())
}

/// Parse the full type string and return the canonical type representation
/// This is used for the `TypeOf` edge target
/// Normalizes nullable types: String, nil → String (nullable semantics not needed for `TypeOf`)
/// Normalizes optional marker: String? → String
pub fn canonical_type_string(type_str: &str) -> String {
    let trimmed = type_str.trim();

    // Strip trailing ? from optional types (e.g., String? → String)
    let stripped = if let Some(s) = trimmed.strip_suffix('?') {
        s
    } else {
        trimmed
    };

    // Remove ", nil" suffix from union types (e.g., String, nil → String)
    let stripped = if let Some(s) = stripped.strip_suffix(", nil") {
        s.trim()
    } else if let Some(s) = stripped.strip_suffix(",nil") {
        s.trim()
    } else {
        stripped
    };

    stripped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple_types() {
        assert_eq!(extract_type_names("User"), vec!["User"]);
        assert_eq!(extract_type_names("String"), vec!["String"]);
    }

    #[test]
    fn test_extract_union_types() {
        let result = extract_type_names("String, Integer, Boolean");
        assert_eq!(result, vec!["Boolean", "Integer", "String"]);

        let result = extract_type_names("User, Admin, Guest");
        assert_eq!(result, vec!["Admin", "Guest", "User"]);
    }

    #[test]
    fn test_extract_nullable_types() {
        let result = extract_type_names("String?");
        assert_eq!(result, vec!["String"]);

        let result = extract_type_names("String, nil");
        assert_eq!(result, vec!["String"]);

        let result = extract_type_names("User, nil");
        assert_eq!(result, vec!["User"]);
    }

    #[test]
    fn test_extract_array_types() {
        let result = extract_type_names("Array<User>");
        assert_eq!(result, vec!["Array", "User"]);

        let result = extract_type_names("Array<String>");
        assert_eq!(result, vec!["Array", "String"]);
    }

    #[test]
    fn test_extract_hash_types() {
        let result = extract_type_names("Hash{String => Integer}");
        assert_eq!(result, vec!["Hash", "Integer", "String"]);

        let result = extract_type_names("Hash{Symbol => User}");
        assert_eq!(result, vec!["Hash", "Symbol", "User"]);
    }

    #[test]
    fn test_extract_generic_types() {
        let result = extract_type_names("Collection<User>");
        assert_eq!(result, vec!["Collection", "User"]);

        let result = extract_type_names("Result<Data, Error>");
        assert_eq!(result, vec!["Data", "Error", "Result"]);
    }

    #[test]
    fn test_exclude_nil() {
        let result = extract_type_names("String, nil, Integer");
        assert_eq!(result, vec!["Integer", "String"]);

        let result = extract_type_names("User, nil");
        assert_eq!(result, vec!["User"]);
    }

    #[test]
    fn test_exclude_duck_types() {
        // Duck types start with # which is a delimiter, so #to_s splits into
        // empty + "to_s" — "to_s" starts lowercase and isn't a builtin → excluded
        let result = extract_type_names("#to_s");
        assert_eq!(result, Vec::<String>::new());

        let result = extract_type_names("String, #to_s");
        assert_eq!(result, vec!["String"]);

        let result = extract_type_names("#each");
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_extract_qualified_types() {
        // Namespace-qualified: MyModule::MyClass → extracts both parts
        let result = extract_type_names("MyModule::MyClass");
        assert_eq!(result, vec!["MyClass", "MyModule"]);

        // Multiple namespace levels
        let mut result = extract_type_names("App::Models::User");
        result.sort();
        assert_eq!(result, vec!["App", "Models", "User"]);
    }

    #[test]
    fn test_canonical_type_string() {
        assert_eq!(canonical_type_string("String"), "String");
        assert_eq!(canonical_type_string("  User  "), "User");
        assert_eq!(canonical_type_string("String?"), "String");
        assert_eq!(canonical_type_string("String, nil"), "String");
        assert_eq!(canonical_type_string("Array<String>"), "Array<String>");
    }

    #[test]
    fn test_is_type_name() {
        // PascalCase
        assert!(is_type_name("User"));
        assert!(is_type_name("UserService"));

        // Built-ins
        assert!(is_type_name("String"));
        assert!(is_type_name("Integer"));
        assert!(is_type_name("Boolean"));
        assert!(is_type_name("Array"));
        assert!(is_type_name("Hash"));

        // Not types
        assert!(!is_type_name("nil"));
        assert!(!is_type_name(""));
        assert!(!is_type_name("snake_case"));
    }

    #[test]
    fn test_extract_complex_types() {
        let result = extract_type_names("Hash{Symbol => Array<User>}");
        assert_eq!(result, vec!["Array", "Hash", "Symbol", "User"]);

        let result = extract_type_names("Proc(String, Integer) => Boolean");
        assert_eq!(result, vec!["Boolean", "Integer", "Proc", "String"]);
    }
}
