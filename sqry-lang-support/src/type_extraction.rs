//! Shared type name extraction from doc-comment type strings.
//!
//! Languages that embed type annotations in documentation comments (`JSDoc`, `PHPDoc`,
//! `YARD`) all follow the same extraction pattern: tokenize on delimiters, then
//! filter tokens via an `is_type_name` predicate.
//!
//! This module provides a configurable [`TypeExtractionConfig`] and a shared
//! [`extract_type_names`] function that replaces the hand-rolled implementations
//! in `sqry-lang-javascript`, `sqry-lang-php`, and `sqry-lang-ruby`.
//!
//! # Design Rationale
//!
//! AST-based type extractors (`C`, `C#`, `Kotlin`, `Scala`, and similar languages) walk language-specific
//! tree-sitter node kinds and cannot be meaningfully generalized. This module
//! targets **only** the string-based tokenizers where the algorithm skeleton is
//! identical across languages.

/// Configuration for string-based type name extraction.
///
/// Each language provides its own delimiter set, builtin types, null-like
/// exclusions, and case-sensitivity rules.
///
/// # Examples
///
/// ```
/// use sqry_lang_support::type_extraction::{TypeExtractionConfig, extract_type_names};
///
/// let config = TypeExtractionConfig::javascript();
/// let types = extract_type_names("User|null|string", &config);
/// assert!(types.contains(&"User".to_string()));
/// assert!(types.contains(&"string".to_string()));
/// assert!(!types.contains(&"null".to_string()));
/// ```
pub struct TypeExtractionConfig {
    /// Characters that split type strings into tokens.
    pub delimiters: &'static [char],

    /// Built-in types recognized by the language (e.g., `"string"`, `"int"`).
    /// These are accepted as valid type names even when they start with lowercase.
    pub builtin_types: &'static [&'static str],

    /// Null-like tokens excluded from Reference edges (e.g., `"null"`, `"undefined"`, `"nil"`).
    pub null_exclusions: &'static [&'static str],

    /// Whether builtin type matching is case-insensitive.
    /// JavaScript/PHP use `token.to_lowercase()` for matching; Ruby does not.
    pub case_insensitive_builtins: bool,
}

impl TypeExtractionConfig {
    /// Configuration for JavaScript/JSDoc type strings.
    ///
    /// - Delimiters: `| & , < > [ ] ( ) { } : = ? ! . ' " / \` and space
    /// - Builtins: `string`, `number`, `boolean`, `symbol`, `bigint`, `object`,
    ///   `any`, `void`, `never`, `unknown`, `true`, `false`
    /// - Null exclusions: `null`, `undefined`
    /// - Case-insensitive builtin matching
    #[must_use]
    pub fn javascript() -> Self {
        Self {
            delimiters: &[
                '|', '&', ',', '<', '>', '[', ']', '(', ')', '{', '}', ':', '=', '?', '!', ' ',
                '.', '\'', '"', '/', '\\',
            ],
            builtin_types: &[
                "string", "number", "boolean", "symbol", "bigint", "object", "any", "void",
                "never", "unknown", "true", "false",
            ],
            null_exclusions: &["null", "undefined"],
            case_insensitive_builtins: true,
        }
    }

    /// Configuration for PHP/PHPDoc type strings.
    ///
    /// - Delimiters: same as JavaScript plus `\n` and `\t`
    /// - Builtins: `string`, `int`, `float`, `bool`, `array`, `object`,
    ///   `callable`, `iterable`, `mixed`, `void`, `never`, `true`, `false`,
    ///   `resource`, `numeric`
    /// - Null exclusions: `null`
    /// - Case-insensitive builtin matching
    #[must_use]
    pub fn php() -> Self {
        Self {
            delimiters: &[
                '|', '&', ',', '<', '>', '[', ']', '(', ')', '{', '}', ':', '=', '?', '!', ' ',
                '.', '\'', '"', '/', '\\', '\n', '\t',
            ],
            builtin_types: &[
                "string", "int", "float", "bool", "array", "object", "callable", "iterable",
                "mixed", "void", "never", "true", "false", "resource", "numeric",
            ],
            null_exclusions: &["null"],
            case_insensitive_builtins: true,
        }
    }

    /// Configuration for Ruby/YARD type strings.
    ///
    /// - Delimiters: `, < > { } = ? # : ( ) [ ]` and space, `\n`, `\t`
    /// - Builtins: `String`, `Integer`, `Float`, `Boolean`, `Array`, `Hash`,
    ///   `Symbol`, `Range`, `Regexp`, `Time`, `Date`, `DateTime`, `Proc`,
    ///   `Lambda`, `Method`, `TrueClass`, `FalseClass`, `NilClass`, `Numeric`,
    ///   `Object`, `Class`, `Module`, `Struct`, `Set`, `Fiber`, `Thread`,
    ///   `Mutex`, `Queue`, `File`, `Dir`, `IO`, `StringIO`, `Enumerator`,
    ///   `Enumerable`, `Comparable`, `Kernel`
    /// - Null exclusions: `nil`
    /// - Case-sensitive builtin matching (Ruby types are `PascalCase`)
    /// - Excludes duck types (tokens starting with `#`)
    #[must_use]
    pub fn ruby() -> Self {
        Self {
            delimiters: &[
                ',', '<', '>', '{', '}', '=', '?', '#', ':', '(', ')', '[', ']', ' ', '\n', '\t',
            ],
            builtin_types: &[
                "String",
                "Integer",
                "Float",
                "Boolean",
                "Array",
                "Hash",
                "Symbol",
                "Range",
                "Regexp",
                "Time",
                "Date",
                "DateTime",
                "Proc",
                "Lambda",
                "Method",
                "TrueClass",
                "FalseClass",
                "NilClass",
                "Numeric",
                "Object",
                "Class",
                "Module",
                "Struct",
                "Set",
                "Fiber",
                "Thread",
                "Mutex",
                "Queue",
                "File",
                "Dir",
                "IO",
                "StringIO",
                "Enumerator",
                "Enumerable",
                "Comparable",
                "Kernel",
            ],
            null_exclusions: &["nil"],
            case_insensitive_builtins: false,
        }
    }
}

/// Extract all referenced type names from a doc-comment type string.
///
/// Tokenizes on the configured delimiters, then filters each token through
/// [`is_type_name`]. Results are sorted and deduplicated.
///
/// # Arguments
///
/// * `type_str` - The raw type string from a doc comment (e.g., `"User|null|string"`)
/// * `config` - Language-specific extraction configuration
///
/// # Returns
///
/// Sorted, deduplicated vector of valid type names.
///
/// # Examples
///
/// ```
/// use sqry_lang_support::type_extraction::{TypeExtractionConfig, extract_type_names};
///
/// // JavaScript
/// let js = TypeExtractionConfig::javascript();
/// assert_eq!(extract_type_names("User", &js), vec!["User"]);
///
/// let mut result = extract_type_names("User|Admin", &js);
/// assert_eq!(result, vec!["Admin", "User"]);
///
/// // PHP
/// let php = TypeExtractionConfig::php();
/// let result = extract_type_names("string|null|int", &php);
/// assert_eq!(result, vec!["int", "string"]);
///
/// // Ruby
/// let ruby = TypeExtractionConfig::ruby();
/// let result = extract_type_names("String, nil, Integer", &ruby);
/// assert_eq!(result, vec!["Integer", "String"]);
/// ```
#[must_use]
pub fn extract_type_names(type_str: &str, config: &TypeExtractionConfig) -> Vec<String> {
    let mut type_names = Vec::new();
    let mut current_token = String::new();

    for ch in type_str.chars() {
        if config.delimiters.contains(&ch) {
            if !current_token.is_empty() && is_type_name(&current_token, config) {
                type_names.push(current_token.clone());
            }
            current_token.clear();
        } else {
            current_token.push(ch);
        }
    }

    // Add final token
    if !current_token.is_empty() && is_type_name(&current_token, config) {
        type_names.push(current_token);
    }

    // Deduplicate and sort
    type_names.sort();
    type_names.dedup();
    type_names
}

/// Check if a token is a valid type name for `Reference` edges.
///
/// A token is a valid type name if:
/// 1. It is non-empty
/// 2. It is not in the null exclusion list
/// 3. It matches a builtin type (with optional case-insensitive matching), or
/// 4. It starts with an uppercase letter (`PascalCase` custom type)
///
/// # Arguments
///
/// * `token` - The candidate token to check
/// * `config` - Language-specific extraction configuration
///
/// # Returns
///
/// `true` if the token should be included as a type name.
pub fn is_type_name(token: &str, config: &TypeExtractionConfig) -> bool {
    if token.is_empty() {
        return false;
    }

    // Check null exclusions
    for &exclusion in config.null_exclusions {
        if token == exclusion {
            return false;
        }
    }

    // Check builtin types
    if config.case_insensitive_builtins {
        let lower = token.to_lowercase();
        if config
            .builtin_types
            .iter()
            .any(|bt| bt.to_lowercase() == lower)
        {
            return true;
        }
    } else if config.builtin_types.contains(&token) {
        return true;
    }

    // Check PascalCase (starts with uppercase)
    token.chars().next().is_some_and(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JavaScript tests ----

    #[test]
    fn js_simple_types() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(extract_type_names("User", &config), vec!["User"]);
        assert_eq!(extract_type_names("string", &config), vec!["string"]);
    }

    #[test]
    fn js_union_types() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(
            extract_type_names("string|number|boolean", &config),
            vec!["boolean", "number", "string"]
        );
        assert_eq!(
            extract_type_names("User|Admin|Guest", &config),
            vec!["Admin", "Guest", "User"]
        );
    }

    #[test]
    fn js_generic_types() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(
            extract_type_names("Array<User>", &config),
            vec!["Array", "User"]
        );
        assert_eq!(
            extract_type_names("Promise<Result<Data>>", &config),
            vec!["Data", "Promise", "Result"]
        );
        assert_eq!(
            extract_type_names("Map<string, User>", &config),
            vec!["Map", "User", "string"]
        );
    }

    #[test]
    fn js_object_types() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(
            extract_type_names("{id: string, user: User}", &config),
            vec!["User", "string"]
        );
    }

    #[test]
    fn js_function_types() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(
            extract_type_names("(User) => boolean", &config),
            vec!["User", "boolean"]
        );
        assert_eq!(
            extract_type_names("function(string, number): User", &config),
            vec!["User", "number", "string"]
        );
    }

    #[test]
    fn js_qualified_types() {
        let config = TypeExtractionConfig::javascript();
        // import('./models').User → extracts User
        let result = extract_type_names("import('./models').User", &config);
        assert!(result.contains(&"User".to_string()));

        // React.Component → extracts both
        assert_eq!(
            extract_type_names("React.Component", &config),
            vec!["Component", "React"]
        );

        // API.Response<Data>
        assert_eq!(
            extract_type_names("API.Response<Data>", &config),
            vec!["API", "Data", "Response"]
        );
    }

    #[test]
    fn js_exclude_null_undefined() {
        let config = TypeExtractionConfig::javascript();
        assert_eq!(
            extract_type_names("string|null|undefined", &config),
            vec!["string"]
        );
        assert_eq!(extract_type_names("User|null", &config), vec!["User"]);
    }

    #[test]
    fn js_is_type_name() {
        let config = TypeExtractionConfig::javascript();
        assert!(is_type_name("User", &config));
        assert!(is_type_name("UserService", &config));
        assert!(is_type_name("string", &config));
        assert!(is_type_name("number", &config));
        assert!(is_type_name("boolean", &config));
        assert!(is_type_name("any", &config));

        assert!(!is_type_name("null", &config));
        assert!(!is_type_name("undefined", &config));
        assert!(!is_type_name("", &config));
        assert!(!is_type_name("camelCase", &config));
    }

    // ---- PHP tests ----

    #[test]
    fn php_simple_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(extract_type_names("User", &config), vec!["User"]);
        assert_eq!(extract_type_names("string", &config), vec!["string"]);
    }

    #[test]
    fn php_union_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(
            extract_type_names("string|int|bool", &config),
            vec!["bool", "int", "string"]
        );
    }

    #[test]
    fn php_nullable_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(extract_type_names("?User", &config), vec!["User"]);
        assert_eq!(extract_type_names("User|null", &config), vec!["User"]);
    }

    #[test]
    fn php_array_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(extract_type_names("User[]", &config), vec!["User"]);
        assert_eq!(
            extract_type_names("array<string, User>", &config),
            vec!["User", "array", "string"]
        );
    }

    #[test]
    fn php_generic_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(
            extract_type_names("Collection<User>", &config),
            vec!["Collection", "User"]
        );
        assert_eq!(
            extract_type_names("Nullable<Result<Data>>", &config),
            vec!["Data", "Nullable", "Result"]
        );
    }

    #[test]
    fn php_exclude_null() {
        let config = TypeExtractionConfig::php();
        assert_eq!(
            extract_type_names("string|null|int", &config),
            vec!["int", "string"]
        );
    }

    #[test]
    fn php_qualified_types() {
        let config = TypeExtractionConfig::php();
        assert_eq!(
            extract_type_names("App\\Models\\User", &config),
            vec!["App", "Models", "User"]
        );
    }

    #[test]
    fn php_is_type_name() {
        let config = TypeExtractionConfig::php();
        assert!(is_type_name("User", &config));
        assert!(is_type_name("string", &config));
        assert!(is_type_name("int", &config));
        assert!(is_type_name("bool", &config));
        assert!(is_type_name("array", &config));
        assert!(is_type_name("mixed", &config));

        assert!(!is_type_name("null", &config));
        assert!(!is_type_name("", &config));
        assert!(!is_type_name("camelCase", &config));
    }

    // ---- Ruby tests ----

    #[test]
    fn ruby_simple_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(extract_type_names("User", &config), vec!["User"]);
        assert_eq!(extract_type_names("String", &config), vec!["String"]);
    }

    #[test]
    fn ruby_union_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(
            extract_type_names("String, Integer, Boolean", &config),
            vec!["Boolean", "Integer", "String"]
        );
    }

    #[test]
    fn ruby_nullable_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(extract_type_names("String?", &config), vec!["String"]);
        assert_eq!(extract_type_names("String, nil", &config), vec!["String"]);
    }

    #[test]
    fn ruby_array_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(
            extract_type_names("Array<User>", &config),
            vec!["Array", "User"]
        );
    }

    #[test]
    fn ruby_hash_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(
            extract_type_names("Hash{String => Integer}", &config),
            vec!["Hash", "Integer", "String"]
        );
    }

    #[test]
    fn ruby_exclude_nil() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(
            extract_type_names("String, nil, Integer", &config),
            vec!["Integer", "String"]
        );
    }

    #[test]
    fn ruby_exclude_duck_types() {
        let config = TypeExtractionConfig::ruby();
        // Duck types start with # which is a delimiter in Ruby config,
        // so "#to_s" splits into empty + "to_s", and "to_s" is lowercase → excluded
        assert_eq!(extract_type_names("#to_s", &config), Vec::<String>::new());
        assert_eq!(extract_type_names("String, #to_s", &config), vec!["String"]);
    }

    #[test]
    fn ruby_qualified_types() {
        let config = TypeExtractionConfig::ruby();
        // :: is split by : delimiter
        assert_eq!(
            extract_type_names("MyModule::MyClass", &config),
            vec!["MyClass", "MyModule"]
        );
    }

    #[test]
    fn ruby_is_type_name() {
        let config = TypeExtractionConfig::ruby();
        assert!(is_type_name("User", &config));
        assert!(is_type_name("String", &config));
        assert!(is_type_name("Integer", &config));
        assert!(is_type_name("Boolean", &config));
        assert!(is_type_name("Array", &config));
        assert!(is_type_name("Hash", &config));

        assert!(!is_type_name("nil", &config));
        assert!(!is_type_name("", &config));
        assert!(!is_type_name("snake_case", &config));
    }

    #[test]
    fn ruby_complex_types() {
        let config = TypeExtractionConfig::ruby();
        assert_eq!(
            extract_type_names("Hash{Symbol => Array<User>}", &config),
            vec!["Array", "Hash", "Symbol", "User"]
        );
        assert_eq!(
            extract_type_names("Proc(String, Integer) => Boolean", &config),
            vec!["Boolean", "Integer", "Proc", "String"]
        );
    }
}
