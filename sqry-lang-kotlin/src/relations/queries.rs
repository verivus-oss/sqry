//! Tree-sitter queries for Kotlin code analysis.
//!
//! Provides query definitions for extracting:
//! - Class declarations (regular classes, objects, companion objects, data classes)
//! - Function declarations (regular, suspend, inline, extension functions)
//! - Call expressions (regular calls, method calls, extension calls)

use tree_sitter::{Language, Query, QueryError};

/// Compiled tree-sitter queries for Kotlin constructs.
pub struct KotlinQueries {
    /// Query for class, object, and companion object declarations
    pub classes: Query,
    /// Query for function and property declarations
    pub functions: Query,
    /// Query for call expressions (function and method calls)
    pub calls: Query,
}

impl KotlinQueries {
    /// Create new `KotlinQueries` from the given tree-sitter language.
    ///
    /// # Errors
    ///
    /// Returns `QueryError` if any query fails to compile.
    pub fn new(language: &Language) -> Result<Self, QueryError> {
        Ok(Self {
            classes: Query::new(language, CLASS_QUERY)?,
            functions: Query::new(language, FUNCTION_QUERY)?,
            calls: Query::new(language, CALL_QUERY)?,
        })
    }
}

/// Query for extracting class declarations.
///
/// Captures:
/// - Regular classes: `class User { ... }`
/// - Objects: `object Singleton { ... }`
/// - Companion objects: `companion object { ... }`
/// - Data classes: `data class Person(...)`
/// - Sealed classes: `sealed class Result<T>`
///
/// Capture groups:
/// - `@class.name`: Class/object identifier
/// - `@class`: Full `class_declaration` node
/// - `@object.name`: Object identifier
/// - `@object`: Full `object_declaration` node
const CLASS_QUERY: &str = r"
; Regular class declarations
(class_declaration
  (type_identifier) @class.name) @class

; Object declarations (singletons)
(object_declaration
  (type_identifier) @object.name) @object

; Companion objects (may not have explicit name)
(companion_object) @companion
";

/// Query for extracting function and property declarations.
///
/// Captures:
/// - Functions: `fun calculate(x: Int): Int { ... }`
/// - Suspend functions: `suspend fun fetchData() { ... }`
/// - Extension functions: `fun String.isPalindrome(): Boolean { ... }`
/// - Properties: `val name: String`, `var count: Int`
///
/// Capture groups:
/// - `@func.name`: Function identifier
/// - `@func`: Full `function_declaration` node
/// - `@prop.name`: Property identifier
/// - `@prop`: Full `property_declaration` node
const FUNCTION_QUERY: &str = r"
; Function declarations
(function_declaration
  (simple_identifier) @func.name) @func

; Property declarations (val/var)
(property_declaration
  (variable_declaration
    (simple_identifier) @prop.name)) @prop
";

/// Query for extracting call expressions.
///
/// Captures:
/// - Simple calls: `println("hello")`
/// - Method calls: `user.getName()`
/// - Extension calls: `"hello".uppercase()`
/// - Constructor calls: `User("Alice")`
///
/// Capture groups:
/// - `@call.name`: Function name in `call_expression`
/// - `@call`: Full `call_expression` node
const CALL_QUERY: &str = r"
; Direct call expressions
(call_expression
  (simple_identifier) @call.name) @call

; Navigation expressions (method calls)
(navigation_expression
  (navigation_suffix
    (simple_identifier) @call.name)) @call
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queries_compile() {
        let language = tree_sitter_kotlin_sqry::language();
        let queries = KotlinQueries::new(&language);
        assert!(
            queries.is_ok(),
            "Kotlin queries should compile successfully"
        );
    }

    #[test]
    fn test_class_query_compiles() {
        let language = tree_sitter_kotlin_sqry::language();
        let query = Query::new(&language, CLASS_QUERY);
        assert!(query.is_ok(), "CLASS_QUERY should compile");
    }

    #[test]
    fn test_function_query_compiles() {
        let language = tree_sitter_kotlin_sqry::language();
        let query = Query::new(&language, FUNCTION_QUERY);
        assert!(query.is_ok(), "FUNCTION_QUERY should compile");
    }

    #[test]
    fn test_call_query_compiles() {
        let language = tree_sitter_kotlin_sqry::language();
        let query = Query::new(&language, CALL_QUERY);
        assert!(query.is_ok(), "CALL_QUERY should compile");
    }
}
