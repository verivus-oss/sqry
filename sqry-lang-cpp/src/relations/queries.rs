//! Tree-sitter queries for C++ code analysis.
//!
//! Provides query definitions for extracting:
//! - Class and struct declarations (regular, templated, namespaced)
//! - Function declarations (free functions, methods, template functions)
//! - Call expressions (direct calls, qualified calls, member function calls)

use tree_sitter::{Language, Query, QueryError};

/// Compiled tree-sitter queries for Cpp constructs.
pub struct CppQueries {
    /// Query for class, object, and namespace declarations
    pub classes: Query,
    /// Query for function and property declarations
    pub functions: Query,
    /// Query for call expressions (function and method calls)
    pub calls: Query,
}

impl CppQueries {
    /// Create new `CppQueries` from the given tree-sitter language.
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

/// Query for extracting class and struct declarations.
///
/// Captures:
/// - Class specifiers: `class User { ... }`
/// - Struct specifiers: `struct Person { ... }`
/// - Templated classes: `template<typename T> class Container { ... }`
/// - Templated structs: `template<typename T> struct Pair { ... }`
///
/// Capture groups:
/// - `@class.name`: Class/struct type identifier
/// - `@class`: Full `class_specifier` or `struct_specifier` node
/// - `@template_class`: Full `template_declaration` wrapping a class
/// - `@template_struct`: Full `template_declaration` wrapping a struct
const CLASS_QUERY: &str = r"
; Class specifiers: class Foo { ... };
(class_specifier
  name: (type_identifier) @class.name) @class

; Struct specifiers: struct Foo { ... };
(struct_specifier
  name: (type_identifier) @class.name) @class

; Class specifier inside template declaration
(template_declaration
  (class_specifier
    name: (type_identifier) @class.name) @class) @template_class

; Struct specifier inside template declaration
(template_declaration
  (struct_specifier
    name: (type_identifier) @class.name) @class) @template_struct
";

/// Query for extracting function and method declarations.
///
/// Captures:
/// - Free functions: `int calculate(int x) { ... }`
/// - Methods: `virtual void fetchData() { ... }`
/// - Template functions: `template<typename T> T max(T a, T b) { ... }`
/// - Field declarations with function declarators (method signatures)
///
/// Capture groups:
/// - `@func.name`: Function/method identifier
/// - `@func`: Full `function_definition` or `field_declaration` node
/// - `@template_func`: Full `template_declaration` wrapping a function
const FUNCTION_QUERY: &str = r"
; Function definitions (free functions and methods)
(function_definition
  declarator: (function_declarator
    declarator: [(identifier) (field_identifier)] @func.name)) @func

; Methods declared inside classes/structs without inline bodies
(field_declaration
  declarator: (function_declarator
    declarator: [(field_identifier) (identifier)] @func.name)) @func

; Template function declarations
(template_declaration
  (function_definition
    declarator: (function_declarator
      declarator: (identifier) @func.name)) @func) @template_func
";

/// Query for extracting call expressions.
///
/// Captures:
/// - Direct calls: `printf("hello")`
/// - Qualified calls: `std::move(x)`
/// - Member function calls: `obj.method()`
///
/// Capture groups:
/// - `@call.name`: Function name in direct or qualified `call_expression`
/// - `@call.method`: Method name in member `call_expression`
/// - `@call`: Full `call_expression` node
const CALL_QUERY: &str = r"
[
  ; Direct function calls: foo()
  (call_expression
    function: (identifier) @call.name) @call

  ; Qualified/scoped function calls: std::move()
  (call_expression
    function: (qualified_identifier) @call.name) @call

  ; Member function calls: object.method()
  (call_expression
    function: (field_expression
      field: [(field_identifier) (identifier)] @call.method)) @call
]
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queries_compile() {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let queries = CppQueries::new(&language);
        assert!(queries.is_ok(), "Cpp queries should compile successfully");
    }

    #[test]
    fn test_class_query_compiles() {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let query = Query::new(&language, CLASS_QUERY);
        assert!(query.is_ok(), "CLASS_QUERY should compile");
    }

    #[test]
    fn test_function_query_compiles() {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let query = Query::new(&language, FUNCTION_QUERY);
        assert!(query.is_ok(), "FUNCTION_QUERY should compile");
    }

    #[test]
    fn test_call_query_compiles() {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let query = Query::new(&language, CALL_QUERY);
        assert!(query.is_ok(), "CALL_QUERY should compile");
    }
}
