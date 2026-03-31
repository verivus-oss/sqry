//! Type name extraction from `PHPDoc` type strings
//!
//! Delegates to `sqry_lang_support::type_extraction` for the shared
//! tokenize-and-filter algorithm. Only `canonical_type_string` remains
//! here because nullable-prefix normalization (`?T` → `T`) is PHP-specific.

use sqry_lang_support::type_extraction::{self, TypeExtractionConfig};

/// Extract all referenced type names from a `PHPDoc` type string
///
/// Examples:
/// - `User` → `["User"]`
/// - `string|int` → `["int", "string"]`
/// - `User[]` → `["User"]`
/// - `array<string, User>` → `["User", "array", "string"]`
/// - `?User` → `["User"]`
/// - `User|null` → `["User"]`
pub fn extract_type_names(type_str: &str) -> Vec<String> {
    type_extraction::extract_type_names(type_str, &TypeExtractionConfig::php())
}

/// Check if a token is a valid type name for Reference edges (test helper).
#[cfg(test)]
fn is_type_name(token: &str) -> bool {
    type_extraction::is_type_name(token, &TypeExtractionConfig::php())
}

/// Parse the full type string and return the canonical type representation
/// This is used for the `TypeOf` edge target
/// Normalizes variadic param types: ...$T → T (variadic semantics captured in metadata)
/// Normalizes nullable types: ?T → T (nullable semantics not needed for `TypeOf`)
pub fn canonical_type_string(type_str: &str) -> String {
    let trimmed = type_str.trim();

    // Strip leading ? from nullable types (nullable semantics captured in parameter context)
    let stripped = if let Some(s) = trimmed.strip_prefix('?') {
        s
    } else {
        trimmed
    };

    stripped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple_types() {
        assert_eq!(extract_type_names("User"), vec!["User"]);
        assert_eq!(extract_type_names("string"), vec!["string"]);
    }

    #[test]
    fn test_extract_union_types() {
        let result = extract_type_names("string|int|bool");
        assert_eq!(result, vec!["bool", "int", "string"]);

        let result = extract_type_names("User|Admin|Guest");
        assert_eq!(result, vec!["Admin", "Guest", "User"]);
    }

    #[test]
    fn test_extract_nullable_types() {
        let result = extract_type_names("?User");
        assert_eq!(result, vec!["User"]);

        let result = extract_type_names("?string|int");
        assert_eq!(result, vec!["int", "string"]);

        let result = extract_type_names("User|null");
        assert_eq!(result, vec!["User"]);
    }

    #[test]
    fn test_extract_array_types() {
        let result = extract_type_names("User[]");
        assert_eq!(result, vec!["User"]);

        let result = extract_type_names("string[]");
        assert_eq!(result, vec!["string"]);

        let result = extract_type_names("array<string, User>");
        assert_eq!(result, vec!["User", "array", "string"]);
    }

    #[test]
    fn test_extract_generic_types() {
        let result = extract_type_names("Collection<User>");
        assert_eq!(result, vec!["Collection", "User"]);

        let result = extract_type_names("Map<string, User>");
        assert_eq!(result, vec!["Map", "User", "string"]);

        let result = extract_type_names("Nullable<Result<Data>>");
        assert_eq!(result, vec!["Data", "Nullable", "Result"]);
    }

    #[test]
    fn test_exclude_null() {
        let result = extract_type_names("string|null|int");
        assert_eq!(result, vec!["int", "string"]);

        let result = extract_type_names("User|null");
        assert_eq!(result, vec!["User"]);
    }

    #[test]
    fn test_extract_qualified_types() {
        // Namespace-qualified: App\Models\User → extracts User, Models, App
        let result = extract_type_names("App\\Models\\User");
        assert_eq!(result, vec!["App", "Models", "User"]);

        // Aliased: Illuminate\Support\Collection → extracts Collection, Support, Illuminate
        let mut result = extract_type_names("Illuminate\\Support\\Collection");
        result.sort();
        assert_eq!(result, vec!["Collection", "Illuminate", "Support"]);
    }

    #[test]
    fn test_canonical_type_string() {
        assert_eq!(canonical_type_string("string"), "string");
        assert_eq!(canonical_type_string("  User  "), "User");
        assert_eq!(canonical_type_string("?User"), "User");
        assert_eq!(canonical_type_string("?array<string>"), "array<string>");
    }

    #[test]
    fn test_is_type_name() {
        // PascalCase
        assert!(is_type_name("User"));
        assert!(is_type_name("UserService"));

        // Built-ins
        assert!(is_type_name("string"));
        assert!(is_type_name("int"));
        assert!(is_type_name("bool"));
        assert!(is_type_name("array"));
        assert!(is_type_name("mixed"));

        // Not types
        assert!(!is_type_name("null"));
        assert!(!is_type_name(""));
        assert!(!is_type_name("camelCase"));
    }

    #[test]
    fn test_extract_mixed_types() {
        let result = extract_type_names("{User|Admin}");
        assert_eq!(result, vec!["Admin", "User"]);

        let result = extract_type_names("(User) => bool");
        assert_eq!(result, vec!["User", "bool"]);
    }
}
