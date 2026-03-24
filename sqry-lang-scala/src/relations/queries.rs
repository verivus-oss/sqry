//! Tree-sitter queries for Scala code analysis.
//!
//! Provides query definitions for extracting:
//! - Class declarations (classes, traits, objects, case classes)
//! - Function declarations (def, val functions)
//! - Call expressions (regular calls, infix calls)
//! - Import declarations (simple, wildcard, selective, renamed)

use tree_sitter::{Language, Query, QueryError};

/// Compiled tree-sitter queries for Scala constructs.
pub struct ScalaQueries {
    /// Query for class, trait, and object declarations
    pub classes: Query,
    /// Query for function declarations
    pub functions: Query,
    /// Query for call expressions
    pub calls: Query,
    /// Query for import declarations
    pub imports: Query,
}

impl ScalaQueries {
    /// Create new `ScalaQueries` from the given tree-sitter language.
    ///
    /// # Errors
    ///
    /// Returns `QueryError` if any query fails to compile.
    pub fn new(language: &Language) -> Result<Self, QueryError> {
        Ok(Self {
            classes: Query::new(language, CLASS_QUERY)?,
            functions: Query::new(language, FUNCTION_QUERY)?,
            calls: Query::new(language, CALL_QUERY)?,
            imports: Query::new(language, IMPORT_QUERY)?,
        })
    }
}

/// Query for extracting class declarations.
///
/// Captures:
/// - Regular classes: `class User { ... }`
/// - Traits: `trait Service { ... }`
/// - Objects: `object Singleton { ... }`
/// - Case classes: `case class Person(...)`
///
/// Capture groups:
/// - `@class.name`: Class/trait/object identifier
/// - `@class`: Full definition node
const CLASS_QUERY: &str = r"
; Class definitions
(class_definition
  name: (identifier) @class.name) @class

; Trait definitions
(trait_definition
  name: (identifier) @trait.name) @trait

; Object definitions (singletons)
(object_definition
  name: (identifier) @object.name) @object
";

/// Query for extracting function declarations.
///
/// Captures:
/// - Functions: `def calculate(x: Int): Int = { ... }`
/// - Function declarations: `def method(): Unit`
///
/// Capture groups:
/// - `@func.name`: Function identifier
/// - `@func`: Full `function_definition` node
const FUNCTION_QUERY: &str = r"
; Function definitions
(function_definition
  name: (identifier) @func.name) @func

; Function declarations (abstract/trait methods)
(function_declaration
  name: (identifier) @decl.name) @decl
";

/// Query for extracting call expressions.
///
/// Captures:
/// - Simple calls: `println("hello")`
/// - Method calls: `user.getName()`
/// - Infix calls: `list map func`
///
/// Capture groups:
/// - `@call.name`: Function name in `call_expression`
/// - `@call`: Full `call_expression` node
const CALL_QUERY: &str = r"
; Direct call expressions
(call_expression
  function: (identifier) @call.name) @call

; Infix expressions (Scala-specific)
(infix_expression
  operator: (identifier) @infix.name) @infix
";

/// Query for extracting import declarations.
///
/// Captures:
/// - Simple imports: `import scala.collection.mutable`
/// - Wildcard imports: `import scala.collection._`
/// - Selective imports: `import scala.collection.{List, Map}`
/// - Renamed imports: `import java.util.{List => JavaList}`
///
/// Capture groups:
/// - `@import`: Full `import_declaration` node
const IMPORT_QUERY: &str = r"
; Import declarations
(import_declaration) @import
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queries_compile() {
        let language = tree_sitter_scala::LANGUAGE.into();
        let queries = ScalaQueries::new(&language);
        assert!(queries.is_ok(), "Scala queries should compile successfully");
    }

    #[test]
    fn test_class_query_compiles() {
        let language = tree_sitter_scala::LANGUAGE.into();
        let query = Query::new(&language, CLASS_QUERY);
        assert!(query.is_ok(), "CLASS_QUERY should compile");
    }

    #[test]
    fn test_function_query_compiles() {
        let language = tree_sitter_scala::LANGUAGE.into();
        let query = Query::new(&language, FUNCTION_QUERY);
        assert!(query.is_ok(), "FUNCTION_QUERY should compile");
    }

    #[test]
    fn test_call_query_compiles() {
        let language = tree_sitter_scala::LANGUAGE.into();
        let query = Query::new(&language, CALL_QUERY);
        assert!(query.is_ok(), "CALL_QUERY should compile");
    }

    #[test]
    fn test_import_query_compiles() {
        let language = tree_sitter_scala::LANGUAGE.into();
        let query = Query::new(&language, IMPORT_QUERY);
        assert!(query.is_ok(), "IMPORT_QUERY should compile");
    }
}
