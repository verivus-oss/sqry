//! Tree-sitter queries for CSS graph extraction.
//!
//! Defines declarative queries for extracting:
//! - CSS custom property declarations (--var-name)
//! - @import statements
//! - `url()` call expressions

use sqry_core::graph::{GraphBuilderError, GraphResult, Span};
use tree_sitter::{Language, Query};

/// Container for all CSS graph extraction queries.
#[derive(Debug)]
#[allow(dead_code)] // Reserved for CSS custom property and import analysis
pub struct CssQueries {
    /// Query for CSS custom property declarations (variables).
    pub custom_properties: Query,
    /// Query for @import statements.
    pub imports: Query,
    /// Query for `url()` call expressions.
    pub url_calls: Query,
}

impl CssQueries {
    /// Create new `CssQueries` by compiling all query patterns.
    ///
    /// # Errors
    /// Returns error if any query fails to compile against the language grammar.
    #[allow(dead_code)] // Reserved for CSS custom property and import analysis
    pub fn new(language: &Language) -> GraphResult<Self> {
        let custom_properties = Query::new(language, CUSTOM_PROPERTY_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile custom_properties query: {e}"),
            }
        })?;

        let imports =
            Query::new(language, IMPORT_QUERY).map_err(|e| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile imports query: {e}"),
            })?;

        let url_calls =
            Query::new(language, URL_CALL_QUERY).map_err(|e| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile url_calls query: {e}"),
            })?;

        Ok(Self {
            custom_properties,
            imports,
            url_calls,
        })
    }
}

/// Query for CSS custom property declarations.
///
/// Matches declarations like `--primary-color: blue;` within rule blocks.
/// Captures:
/// - `@property` - The full declaration node
/// - `@name` - The property name (including --)
///
/// Note: This excludes comments - tree-sitter handles this by not matching
/// comment nodes with `declaration` pattern.
#[allow(dead_code)] // Reserved for CSS custom property analysis
const CUSTOM_PROPERTY_QUERY: &str = r#"
(declaration
    (property_name) @name
    (#match? @name "^--")) @property
"#;

/// Query for @import statements.
///
/// Matches import statements like `@import "reset.css";` or `@import url("theme.css");`
/// Captures:
/// - `@import` - The full `import_statement` node
///
/// The actual URL target is extracted from the node children in the handler.
#[allow(dead_code)] // Reserved for CSS @import analysis
const IMPORT_QUERY: &str = r"
(import_statement) @import
";

/// Query for `url()` call expressions.
///
/// Matches `url()` function calls in CSS property values (case-insensitive).
/// CSS is case-insensitive, so `url()`, `URL()`, `Url()` are all valid.
/// Captures:
/// - `@url_call` - The full `call_expression` node
/// - `@function_name` - The function name (should be "url" in any case)
/// - `@arguments` - The arguments node containing the URL
///
/// Note: This will also match `url()` inside @import statements; the handler
/// filters those out to avoid duplicate edges.
#[allow(dead_code)] // Reserved for CSS url() analysis
const URL_CALL_QUERY: &str = r#"
(call_expression
    (function_name) @function_name
    (arguments) @arguments
    (#match? @function_name "^[uU][rR][lL]$")) @url_call
"#;
