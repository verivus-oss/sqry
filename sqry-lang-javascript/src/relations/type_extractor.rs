//! Type name extraction from `JSDoc` type strings
//!
//! Delegates to `sqry_lang_support::type_extraction` for the shared
//! tokenize-and-filter algorithm. Only `canonical_type_string` remains
//! here because rest-param normalization (`...T` → `T`) is JS-specific.

use sqry_lang_support::type_extraction::{self, TypeExtractionConfig};

/// Extract all referenced type names from a `JSDoc` type string
///
/// Examples:
/// - `User` → \["User"\]
/// - `User|Admin` → \["Admin", "User"\]
/// - `Array<User>` → \["Array", "User"\]
/// - `Promise<Result<Data>>` → \["Data", "Promise", "Result"\]
/// - `{id: string, user: User}` → \["User", "string"\]
/// - `(User) => boolean` → \["User", "boolean"\]
/// - `import('./models').User` → \["User"\]
/// - `React.Component` → \["Component", "React"\]
/// - `API.Response<Data>` → \["API", "Data", "Response"\]
pub fn extract_type_names(type_str: &str) -> Vec<String> {
    type_extraction::extract_type_names(type_str, &TypeExtractionConfig::javascript())
}

/// Check if a token is a valid type name for Reference edges (test helper).
#[cfg(test)]
fn is_type_name(token: &str) -> bool {
    type_extraction::is_type_name(token, &TypeExtractionConfig::javascript())
}

/// Parse the full type string and return the canonical type representation
/// This is used for the `TypeOf` edge target
/// Normalizes rest param types: ...T → T (rest semantics captured in metadata)
pub fn canonical_type_string(type_str: &str) -> String {
    let trimmed = type_str.trim();

    // Strip leading ... from rest param types
    // Rest/variadic semantics are captured in parameter context metadata, not the type string
    if let Some(stripped) = trimmed.strip_prefix("...") {
        stripped.to_string()
    } else {
        trimmed.to_string()
    }
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
        let mut result = extract_type_names("string|number|boolean");
        result.sort();
        assert_eq!(result, vec!["boolean", "number", "string"]);

        let mut result = extract_type_names("User|Admin|Guest");
        result.sort();
        assert_eq!(result, vec!["Admin", "Guest", "User"]);
    }

    #[test]
    fn test_extract_generic_types() {
        let mut result = extract_type_names("Array<User>");
        result.sort();
        assert_eq!(result, vec!["Array", "User"]);

        let mut result = extract_type_names("Promise<Result<Data>>");
        result.sort();
        assert_eq!(result, vec!["Data", "Promise", "Result"]);

        let mut result = extract_type_names("Map<string, User>");
        result.sort();
        assert_eq!(result, vec!["Map", "User", "string"]);
    }

    #[test]
    fn test_extract_object_types() {
        let mut result = extract_type_names("{id: string, user: User}");
        result.sort();
        assert_eq!(result, vec!["User", "string"]);

        let mut result = extract_type_names("{{id: string, meta: {tags: string[]}}}");
        result.sort();
        assert_eq!(result, vec!["string"]);
    }

    #[test]
    fn test_extract_function_types() {
        let mut result = extract_type_names("(User) => boolean");
        result.sort();
        assert_eq!(result, vec!["User", "boolean"]);

        let mut result = extract_type_names("function(string, number): User");
        result.sort();
        assert_eq!(result, vec!["User", "number", "string"]);
    }

    #[test]
    fn test_extract_qualified_types() {
        // import('./models').User → extracts User
        let result = extract_type_names("import('./models').User");
        assert!(result.contains(&"User".to_string()));

        // React.Component → extracts both React and Component
        let mut result = extract_type_names("React.Component");
        result.sort();
        assert_eq!(result, vec!["Component", "React"]);

        // API.Response<Data> → extracts API, Response, Data
        let mut result = extract_type_names("API.Response<Data>");
        result.sort();
        assert_eq!(result, vec!["API", "Data", "Response"]);
    }

    #[test]
    fn test_exclude_null_undefined() {
        let result = extract_type_names("string|null|undefined");
        assert_eq!(result, vec!["string"]);

        let result = extract_type_names("User|null");
        assert_eq!(result, vec!["User"]);
    }

    #[test]
    fn test_canonical_type_string() {
        assert_eq!(canonical_type_string("string"), "string");
        assert_eq!(canonical_type_string("  User  "), "User");
        assert_eq!(canonical_type_string("...number"), "number");
        assert_eq!(canonical_type_string("...Array<User>"), "Array<User>");
    }

    #[test]
    fn test_is_type_name() {
        // PascalCase
        assert!(is_type_name("User"));
        assert!(is_type_name("UserService"));

        // Built-ins
        assert!(is_type_name("string"));
        assert!(is_type_name("number"));
        assert!(is_type_name("boolean"));
        assert!(is_type_name("any"));

        // Not types
        assert!(!is_type_name("null"));
        assert!(!is_type_name("undefined"));
        assert!(!is_type_name(""));
        assert!(!is_type_name("camelCase"));
    }
}
