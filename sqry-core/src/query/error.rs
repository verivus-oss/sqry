//! Error types for the query language
//!
//! This module defines comprehensive error types for lexing, parsing,
//! validation, and execution of queries, with helpful error messages
//! and position information.

use crate::query::types::{FieldType, Operator, Span, Value};
use miette::{Diagnostic, LabeledSpan, SourceCode};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use thiserror::Error;

/// Top-level query error encompassing all error types
#[derive(Debug, Error)]
pub enum QueryError {
    /// Lexical analysis error
    #[error("Syntax error: {0}")]
    Lex(#[from] LexError),

    /// Parsing error
    #[error("Parse error: {0}")]
    Parse(#[from] ParseError),

    /// Validation error
    #[error("Validation error: {0}")]
    Validation(#[from] ValidationError),

    /// Execution error
    #[error("Execution error: {0}")]
    Execution(#[from] ExecutionError),
}

impl QueryError {
    /// Get CLI exit code for this error
    ///
    /// Returns:
    /// - 2 for syntax/validation errors (user input errors)
    /// - 1 for execution errors (system/runtime errors)
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            QueryError::Lex(_) | QueryError::Parse(_) | QueryError::Validation(_) => 2,
            QueryError::Execution(_) => 1,
        }
    }

    /// Wrap this error with source code context for rich diagnostics
    pub fn with_source(self, source: impl Into<String>) -> RichQueryError {
        RichQueryError {
            error: self,
            source: source.into(),
        }
    }
}

/// Query error with source code context for rich diagnostics
///
/// This wrapper provides beautiful error messages with code snippets,
/// syntax highlighting, and helpful labels using the miette crate.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct RichQueryError {
    #[source]
    error: QueryError,
    source: String,
}

impl RichQueryError {
    /// Create a new rich query error with source context
    pub fn new(error: QueryError, source: impl Into<String>) -> Self {
        Self {
            error,
            source: source.into(),
        }
    }

    /// Get the underlying query error
    #[must_use]
    pub fn inner(&self) -> &QueryError {
        &self.error
    }

    /// Get CLI exit code for this error
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.error.exit_code()
    }

    /// Convert error to structured JSON for automation/MCP workflows
    ///
    /// Returns a JSON object with:
    /// - `code`: Error code (e.g., "`sqry::validation`")
    /// - `message`: Human-readable error message
    /// - `query`: The original query string
    /// - `span`: Error location (start/end positions) if available
    /// - `label`: Descriptive label for the error location
    /// - `help`: Helpful suggestions and guidance
    /// - `suggestion`: Suggested fix if available
    #[must_use]
    pub fn to_json_value(&self) -> JsonValue {
        let code = match &self.error {
            QueryError::Lex(_) => "sqry::syntax",
            QueryError::Parse(_) => "sqry::parse",
            QueryError::Validation(_) => "sqry::validation",
            QueryError::Execution(_) => "sqry::execution",
        };

        let message = self.error.to_string();
        let query = &self.source;

        // Extract span and label information
        let (span, label, suggestion) = self.extract_span_and_label();

        // Build help text
        let help = self.build_help_text();

        // Build JSON object
        let mut json = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
                "query": query,
            }
        });

        // Add optional fields if present
        if let Some(span_value) = span {
            json["error"]["span"] = span_value;
        }
        if let Some(label_value) = label {
            json["error"]["label"] = label_value.into();
        }
        json["error"]["help"] = help.into();
        if let Some(suggestion_value) = suggestion {
            json["error"]["suggestion"] = suggestion_value.into();
        }

        json
    }

    /// Extract span, label, and suggestion from the error
    #[allow(clippy::too_many_lines)] // Large match table keeps diagnostic mappings centralized.
    fn extract_span_and_label(&self) -> (Option<JsonValue>, Option<String>, Option<String>) {
        match &self.error {
            QueryError::Lex(e) => match e {
                LexError::UnterminatedString { span } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("unterminated string literal starts here".to_string()),
                    None,
                ),
                LexError::UnterminatedRegex { span } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("unterminated regex literal starts here".to_string()),
                    None,
                ),
                LexError::InvalidEscape { span, char } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some(format!("invalid escape sequence '\\{char}'")),
                    None,
                ),
                LexError::InvalidUnicodeEscape { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid unicode escape sequence".to_string()),
                    None,
                ),
                LexError::InvalidNumber { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid number format".to_string()),
                    None,
                ),
                LexError::NumberOverflow { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("number too large".to_string()),
                    None,
                ),
                LexError::InvalidRegex { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid regex pattern".to_string()),
                    None,
                ),
                LexError::UnexpectedChar { span, char } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some(format!("unexpected character '{char}'")),
                    None,
                ),
                LexError::UnexpectedEof => (None, None, None),
            },
            QueryError::Parse(e) => match e {
                ParseError::UnmatchedParen { open_span, .. } => (
                    Some(serde_json::json!({ "start": open_span.start, "end": open_span.end })),
                    Some("unmatched opening parenthesis".to_string()),
                    None,
                ),
                ParseError::ExpectedIdentifier { token } => (
                    Some(serde_json::json!({ "start": token.span.start, "end": token.span.end })),
                    Some("expected field name here".to_string()),
                    None,
                ),
                ParseError::ExpectedOperator { token } => (
                    Some(serde_json::json!({ "start": token.span.start, "end": token.span.end })),
                    Some("expected operator here".to_string()),
                    None,
                ),
                ParseError::ExpectedValue { token } => (
                    Some(serde_json::json!({ "start": token.span.start, "end": token.span.end })),
                    Some("expected value here".to_string()),
                    None,
                ),
                ParseError::UnexpectedToken { token, .. } => (
                    Some(serde_json::json!({ "start": token.span.start, "end": token.span.end })),
                    Some("unexpected token".to_string()),
                    None,
                ),
                ParseError::InvalidSyntax { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid syntax".to_string()),
                    None,
                ),
                ParseError::EmptyQuery | ParseError::UnexpectedEof { .. } => (None, None, None),
            },
            QueryError::Validation(e) => match e {
                ValidationError::UnknownField {
                    span, suggestion, ..
                } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("unknown field".to_string()),
                    suggestion.clone(),
                ),
                ValidationError::InvalidOperator { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid operator for this field type".to_string()),
                    None,
                ),
                ValidationError::TypeMismatch { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("type mismatch".to_string()),
                    None,
                ),
                ValidationError::InvalidEnumValue { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid enum value".to_string()),
                    None,
                ),
                ValidationError::InvalidRegexPattern { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("invalid regex pattern".to_string()),
                    None,
                ),
                ValidationError::ImpossibleQuery { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("contradictory condition".to_string()),
                    None,
                ),
                ValidationError::FieldNotAvailable { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("field not available".to_string()),
                    None,
                ),
                ValidationError::UnsafeFuzzyCorrection {
                    span, suggestion, ..
                } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("field requires exact match".to_string()),
                    Some(suggestion.clone()),
                ),
                ValidationError::SubqueryDepthExceeded { span, .. } => (
                    Some(serde_json::json!({ "start": span.start, "end": span.end })),
                    Some("subquery depth exceeded".to_string()),
                    None,
                ),
            },
            QueryError::Execution(_) => (None, None, None),
        }
    }

    /// Build help text for the error
    #[allow(clippy::too_many_lines)] // Large match table keeps help text centralized.
    fn build_help_text(&self) -> String {
        match &self.error {
            QueryError::Lex(e) => match e {
                LexError::UnterminatedString { .. } => {
                    "String literals must be closed with a matching quote".to_string()
                }
                LexError::UnterminatedRegex { .. } => {
                    "Regex literals must be closed with a closing '/'".to_string()
                }
                LexError::InvalidEscape { .. } => {
                    "Valid escape sequences: \\n, \\t, \\r, \\\\, \\\", \\'".to_string()
                }
                LexError::InvalidUnicodeEscape { .. } => {
                    "Unicode escapes should be in the format \\u{XXXX}".to_string()
                }
                LexError::InvalidNumber { .. } => {
                    "Numbers must be valid integers".to_string()
                }
                LexError::NumberOverflow { .. } => {
                    "Number is too large. Maximum value is i64::MAX".to_string()
                }
                LexError::InvalidRegex { .. } => {
                    "Check your regex syntax. Common errors: unbalanced brackets, invalid escapes"
                        .to_string()
                }
                LexError::UnexpectedChar { .. } => {
                    "Remove or escape the unexpected character".to_string()
                }
                LexError::UnexpectedEof => "Query ended unexpectedly".to_string(),
            },
            QueryError::Parse(e) => match e {
                ParseError::UnmatchedParen { .. } => "Add a closing ')'".to_string(),
                ParseError::ExpectedIdentifier { .. } => {
                    "Provide a field name (e.g., 'kind', 'name', 'lang')".to_string()
                }
                ParseError::ExpectedOperator { .. } => {
                    "Add an operator like ':', '~=', '>', '<', '>=', or '<='".to_string()
                }
                ParseError::ExpectedValue { .. } => {
                    "Provide a value (string, number, or boolean)".to_string()
                }
                ParseError::UnexpectedToken { .. } => {
                    "Remove the unexpected token or add 'AND', 'OR', 'NOT' between predicates"
                        .to_string()
                }
                ParseError::InvalidSyntax { .. } => {
                    "Check your query syntax. Use explicit 'AND', 'OR', 'NOT' between predicates"
                        .to_string()
                }
                ParseError::EmptyQuery => "Provide a query expression".to_string(),
                ParseError::UnexpectedEof { .. } => "Query ended unexpectedly".to_string(),
            },
            QueryError::Validation(e) => match e {
                ValidationError::UnknownField { suggestion, .. } => {
                    if let Some(sugg) = suggestion {
                        format!(
                            "Did you mean '{sugg}'? Use 'sqry query --help' to see available fields"
                        )
                    } else {
                        "Use 'sqry query --help' to see available fields".to_string()
                    }
                }
                ValidationError::InvalidOperator {
                    field,
                    valid_operators,
                    ..
                } => format!(
                    "Field '{}' supports these operators: {}",
                    field,
                    valid_operators
                        .iter()
                        .map(|op| format!("{op:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                ValidationError::TypeMismatch { expected, got, .. } => {
                    format!("Expected {expected:?}, got {got:?}")
                }
                ValidationError::InvalidEnumValue { valid_values, .. } => {
                    format!("Valid values: {}", valid_values.join(", "))
                }
                ValidationError::InvalidRegexPattern { error, .. } => {
                    format!("Regex error: {error}")
                }
                ValidationError::ImpossibleQuery { message, .. } => message.clone(),
                ValidationError::FieldNotAvailable { field, reason, .. } => {
                    format!("Field '{field}' is not available: {reason}")
                }
                ValidationError::UnsafeFuzzyCorrection {
                    input, suggestion, ..
                } => format!(
                    "Field '{input}' requires exact spelling. Use '{suggestion}' instead."
                ),
                ValidationError::SubqueryDepthExceeded {
                    depth, max_depth, ..
                } => format!(
                    "Subquery nesting depth ({depth}) exceeds maximum ({max_depth}). Simplify the query."
                ),
            },
            QueryError::Execution(e) => match e {
                ExecutionError::IndexNotFound { .. } => {
                    "Create an index with 'sqry index'".to_string()
                }
                ExecutionError::RelationQueryRequiresIndex { .. } => {
                    "Run 'sqry index' to create an index with relation data".to_string()
                }
                ExecutionError::LegacyIndexMissingRelations { .. } => {
                    "Rebuild the index with 'sqry index --force' to enable relation queries"
                        .to_string()
                }
                ExecutionError::IndexMissingCDSupport { .. } => {
                    "Rebuild the index with 'sqry index --force' to enable CD predicates (duplicates:, circular:, unused:)"
                        .to_string()
                }
                ExecutionError::PluginNotFound { .. } => {
                    "The language may not be supported yet".to_string()
                }
                ExecutionError::FieldEvaluationFailed { .. } => {
                    "Check the field value and type".to_string()
                }
                ExecutionError::TypeMismatch { .. } => {
                    "Ensure the value matches the expected type".to_string()
                }
                ExecutionError::InvalidRegex { .. } | ExecutionError::RegexError(_) => {
                    "Check your regex syntax".to_string()
                }
                ExecutionError::InvalidGlob { .. } => {
                    "Check your glob pattern syntax".to_string()
                }
                ExecutionError::TooManyMatches { .. } => {
                    "Use a more specific pattern or increase the limit".to_string()
                }
                ExecutionError::FileReadError(_) => {
                    "Check file permissions and paths".to_string()
                }
                ExecutionError::Timeout { .. } => {
                    "Consider simplifying the query or increasing the timeout".to_string()
                }
                ExecutionError::Cancelled => {
                    "The query was cancelled by the client".to_string()
                }
                ExecutionError::IndexVersionMismatch { .. } => {
                    "Rebuild the index with 'sqry index --force'".to_string()
                }
            },
        }
    }
}

impl Diagnostic for RichQueryError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        match &self.error {
            QueryError::Lex(_) => Some(Box::new("sqry::syntax")),
            QueryError::Parse(_) => Some(Box::new("sqry::parse")),
            QueryError::Validation(_) => Some(Box::new("sqry::validation")),
            QueryError::Execution(_) => Some(Box::new("sqry::execution")),
        }
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.source as &dyn SourceCode)
    }

    #[allow(clippy::too_many_lines)] // Large match table keeps diagnostic mappings centralized.
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let labels: Vec<LabeledSpan> = match &self.error {
            QueryError::Lex(e) => match e {
                LexError::UnterminatedString { span } => {
                    vec![LabeledSpan::new_with_span(
                        Some("unterminated string literal starts here".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::UnterminatedRegex { span } => {
                    vec![LabeledSpan::new_with_span(
                        Some("unterminated regex literal starts here".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::InvalidEscape { span, char } => {
                    vec![LabeledSpan::new_with_span(
                        Some(format!("invalid escape sequence '\\{char}'")),
                        span_to_source_span(span),
                    )]
                }
                LexError::InvalidUnicodeEscape { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid unicode escape sequence".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::InvalidNumber { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid number format".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::NumberOverflow { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("number too large".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::InvalidRegex { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid regex pattern".to_string()),
                        span_to_source_span(span),
                    )]
                }
                LexError::UnexpectedChar { span, char } => {
                    vec![LabeledSpan::new_with_span(
                        Some(format!("unexpected character '{char}'")),
                        span_to_source_span(span),
                    )]
                }
                LexError::UnexpectedEof => vec![],
            },
            QueryError::Parse(e) => match e {
                ParseError::UnmatchedParen { open_span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("unmatched opening parenthesis".to_string()),
                        span_to_source_span(open_span),
                    )]
                }
                ParseError::ExpectedIdentifier { token } => {
                    vec![LabeledSpan::new_with_span(
                        Some("expected field name here".to_string()),
                        span_to_source_span(&token.span),
                    )]
                }
                ParseError::ExpectedOperator { token } => {
                    vec![LabeledSpan::new_with_span(
                        Some("expected operator here".to_string()),
                        span_to_source_span(&token.span),
                    )]
                }
                ParseError::ExpectedValue { token } => {
                    vec![LabeledSpan::new_with_span(
                        Some("expected value here".to_string()),
                        span_to_source_span(&token.span),
                    )]
                }
                ParseError::UnexpectedToken { token, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("unexpected token".to_string()),
                        span_to_source_span(&token.span),
                    )]
                }
                ParseError::InvalidSyntax { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid syntax".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ParseError::EmptyQuery | ParseError::UnexpectedEof { .. } => vec![],
            },
            QueryError::Validation(e) => match e {
                ValidationError::UnknownField {
                    span, suggestion, ..
                } => {
                    let label = if let Some(sugg) = suggestion {
                        format!("unknown field, did you mean '{sugg}'?")
                    } else {
                        "unknown field".to_string()
                    };
                    vec![LabeledSpan::new_with_span(
                        Some(label),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::InvalidOperator { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid operator for this field type".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::TypeMismatch { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("type mismatch".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::InvalidEnumValue { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid enum value".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::InvalidRegexPattern { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("invalid regex pattern".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::ImpossibleQuery { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("contradictory condition".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::FieldNotAvailable { span, .. } => {
                    vec![LabeledSpan::new_with_span(
                        Some("field not available".to_string()),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::UnsafeFuzzyCorrection {
                    span, suggestion, ..
                } => {
                    vec![LabeledSpan::new_with_span(
                        Some(format!(
                            "requires exact match, did you mean '{suggestion}'?"
                        )),
                        span_to_source_span(span),
                    )]
                }
                ValidationError::SubqueryDepthExceeded {
                    depth,
                    max_depth,
                    span,
                } => {
                    vec![LabeledSpan::new_with_span(
                        Some(format!(
                            "nesting depth {depth} exceeds limit of {max_depth}"
                        )),
                        span_to_source_span(span),
                    )]
                }
            },
            QueryError::Execution(_) => vec![],
        };

        if labels.is_empty() {
            None
        } else {
            Some(Box::new(labels.into_iter()))
        }
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        let help_text = match &self.error {
            QueryError::Lex(e) => match e {
                LexError::UnterminatedString { .. } => {
                    Some("String literals must be closed with a matching quote")
                }
                LexError::UnterminatedRegex { .. } => {
                    Some("Regex literals must be closed with a '/' character")
                }
                LexError::InvalidEscape { .. } => {
                    Some("Valid escape sequences are: \\n \\r \\t \\\\ \\\" \\' \\u{XXXX}")
                }
                LexError::InvalidRegex { .. } => {
                    Some("Check the regex syntax. Use /pattern/ or /pattern/flags format")
                }
                _ => None,
            },
            QueryError::Parse(e) => match e {
                ParseError::UnmatchedParen { .. } => {
                    Some("Add a closing ')' or remove the opening '('")
                }
                ParseError::ExpectedIdentifier { .. } => {
                    Some("Valid fields include: kind, name, path, lang, text, async, etc.")
                }
                ParseError::ExpectedOperator { .. } => {
                    Some("Valid operators: : (equals), ~= (regex), >, <, >=, <=")
                }
                ParseError::EmptyQuery => Some("Provide a query expression like 'kind:function'"),
                _ => None,
            },
            QueryError::Validation(e) => match e {
                ValidationError::UnknownField { .. } => {
                    Some("Use 'sqry query --help' to see available fields")
                }
                ValidationError::InvalidOperator {
                    valid_operators, ..
                } => {
                    let ops: Vec<String> =
                        valid_operators.iter().map(|op| format!("'{op}'")).collect();
                    return Some(Box::new(format!(
                        "Valid operators for this field: {}",
                        ops.join(", ")
                    )) as Box<dyn std::fmt::Display>);
                }
                ValidationError::InvalidEnumValue { valid_values, .. } => {
                    return Some(
                        Box::new(format!("Valid values: {}", valid_values.join(", ")))
                            as Box<dyn std::fmt::Display>,
                    );
                }
                _ => None,
            },
            QueryError::Execution(_) => None,
        };

        help_text.map(|s| Box::new(s.to_string()) as Box<dyn std::fmt::Display>)
    }
}

/// Convert Span to miette's `SourceSpan`
fn span_to_source_span(span: &Span) -> miette::SourceSpan {
    miette::SourceSpan::new(span.start.into(), span.end - span.start)
}

/// Lexical analysis errors
#[derive(Debug, Clone, Error)]
pub enum LexError {
    /// Unterminated string literal
    #[error("Unterminated string literal at position {}", span.start)]
    UnterminatedString {
        /// Location of the unterminated string
        span: Span,
    },

    /// Unterminated regex literal
    #[error("Unterminated regex literal at position {}", span.start)]
    UnterminatedRegex {
        /// Location of the unterminated regex
        span: Span,
    },

    /// Invalid escape sequence
    #[error("Invalid escape sequence '\\{char}' at position {}", span.start)]
    InvalidEscape {
        /// The invalid escape character
        char: char,
        /// Location of the invalid escape
        span: Span,
    },

    /// Invalid Unicode escape sequence
    #[error("Invalid Unicode escape sequence at position {}: expected hex digit, got '{got}'", span.start)]
    InvalidUnicodeEscape {
        /// The character encountered instead of a hex digit
        got: char,
        /// Location of the invalid escape
        span: Span,
    },

    /// Invalid number format
    #[error("Invalid number '{text}' at position {}", span.start)]
    InvalidNumber {
        /// The invalid number text
        text: String,
        /// Location of the invalid number
        span: Span,
    },

    /// Number overflow
    #[error("Number overflow '{text}' at position {}: {error}", span.start)]
    NumberOverflow {
        /// The number text that overflowed
        text: String,
        /// The parse error message
        error: String,
        /// Location of the number
        span: Span,
    },

    /// Invalid regex pattern
    #[error("Invalid regex pattern '{pattern}' at position {}: {error}", span.start)]
    InvalidRegex {
        /// The invalid regex pattern
        pattern: String,
        /// The regex compilation error
        error: String,
        /// Location of the regex
        span: Span,
    },

    /// Unexpected character
    #[error("Unexpected character '{char}' at position {}", span.start)]
    UnexpectedChar {
        /// The unexpected character
        char: char,
        /// Location of the character
        span: Span,
    },

    /// Unexpected end of input
    #[error("Unexpected end of input")]
    UnexpectedEof,
}

/// Parsing errors
#[derive(Debug, Clone, Error)]
pub enum ParseError {
    /// Empty query string
    #[error("Query cannot be empty")]
    EmptyQuery,

    /// Expected an identifier (field name)
    #[error("Expected field name")]
    ExpectedIdentifier {
        /// The unexpected token
        token: crate::query::lexer::Token,
    },

    /// Expected an operator
    #[error("Expected operator")]
    ExpectedOperator {
        /// The unexpected token
        token: crate::query::lexer::Token,
    },

    /// Expected a value
    #[error("Expected value")]
    ExpectedValue {
        /// The unexpected token
        token: crate::query::lexer::Token,
    },

    /// Unmatched opening parenthesis
    #[error("Unmatched opening parenthesis at position {}", open_span.start)]
    UnmatchedParen {
        /// Location of the unmatched parenthesis
        open_span: Span,
        /// Whether we reached EOF
        eof: bool,
    },

    /// Unexpected token
    #[error("Expected {expected}")]
    UnexpectedToken {
        /// The unexpected token
        token: crate::query::lexer::Token,
        /// What was expected
        expected: String,
    },

    /// Invalid syntax
    #[error("Invalid syntax at position {}: {message}", span.start)]
    InvalidSyntax {
        /// Description of the syntax error
        message: String,
        /// Location of the syntax error
        span: Span,
    },

    /// Unexpected end of file
    #[error("Unexpected end of input, expected {expected}")]
    UnexpectedEof {
        /// What was expected
        expected: String,
    },
}

/// Validation errors (semantic checking)
#[derive(Debug, Clone, Error)]
pub enum ValidationError {
    /// Unknown field name
    #[error("Unknown field '{field}' at position {}{}",
        span.start,
        suggestion.as_ref().map(|s| format!(". Did you mean '{s}'?")).unwrap_or_default()
    )]
    UnknownField {
        /// The unknown field name
        field: String,
        /// Suggested field name (if any)
        suggestion: Option<String>,
        /// Location of the field reference
        span: Span,
    },

    /// Invalid operator for field type
    #[error(
        "Operator '{}' not supported for field '{}' at position {}. Valid operators: {}",
        operator,
        field,
        span.start,
        format_operators(valid_operators)
    )]
    InvalidOperator {
        /// The field name
        field: String,
        /// The invalid operator
        operator: Operator,
        /// Valid operators for this field
        valid_operators: Vec<Operator>,
        /// Location of the operator
        span: Span,
    },

    /// Type mismatch between field and value
    #[error(
        "Type mismatch for field '{}' at position {}: expected {}, got {:?}",
        field,
        span.start,
        format_field_type(expected),
        got
    )]
    TypeMismatch {
        /// The field name
        field: String,
        /// Expected field type
        expected: FieldType,
        /// Actual value provided
        got: Value,
        /// Location of the value
        span: Span,
    },

    /// Invalid enum value
    #[error(
        "Invalid value '{}' for field '{}' at position {}. Valid values: {}",
        value,
        field,
        span.start,
        valid_values.join(", ")
    )]
    InvalidEnumValue {
        /// The field name
        field: String,
        /// The invalid value
        value: String,
        /// Valid enum values
        valid_values: Vec<&'static str>,
        /// Location of the value
        span: Span,
    },

    /// Invalid regex pattern
    #[error("Invalid regex pattern '{}' at position {}: {}", pattern, span.start, error)]
    InvalidRegexPattern {
        /// The invalid regex pattern
        pattern: String,
        /// The regex compilation error
        error: String,
        /// Location of the regex
        span: Span,
    },

    /// Impossible query (contradictory conditions)
    #[error("Impossible query at position {}: {}", span.start, message)]
    ImpossibleQuery {
        /// Description of the contradiction
        message: String,
        /// Location of the contradiction
        span: Span,
    },

    /// Field not available (requires plugin or feature)
    #[error("Field '{}' is not available at position {}: {}", field, span.start, reason)]
    FieldNotAvailable {
        /// The unavailable field name
        field: String,
        /// Reason why it's not available
        reason: String,
        /// Location of the field reference
        span: Span,
    },

    /// Fuzzy correction found but field requires exact match
    #[error(
        "Field '{input}' is not recognized at position {}. Did you mean '{suggestion}'? This field requires an exact match.",
        span.start
    )]
    UnsafeFuzzyCorrection {
        /// The field name the user typed
        input: String,
        /// The suggested correct field name
        suggestion: String,
        /// Location of the field reference
        span: Span,
    },

    /// Subquery nesting depth exceeded
    #[error(
        "Subquery nesting depth {depth} exceeds maximum of {max_depth} at position {}",
        span.start
    )]
    SubqueryDepthExceeded {
        /// Current nesting depth
        depth: usize,
        /// Maximum allowed depth
        max_depth: usize,
        /// Location of the offending subquery
        span: Span,
    },
}

/// Execution errors (runtime errors)
#[derive(Debug, Error)]
pub enum ExecutionError {
    /// Index not found
    #[error("Index not found at path: {}", path.display())]
    IndexNotFound {
        /// Path where index was expected
        path: PathBuf,
    },

    /// Relation queries require an index with relation data
    #[error("Relation queries require an index. Run `sqry index` for {path}")]
    RelationQueryRequiresIndex {
        /// Path that was queried without an index
        path: PathBuf,
    },

    /// Relation queries need relation data, but the loaded index lacks it
    #[error(
        "Legacy index at {path} (version {index_version}) lacks relation data. Rebuild with `sqry index --force {path}` to enable relation queries."
    )]
    LegacyIndexMissingRelations {
        /// Path to the workspace whose index is missing relations
        path: PathBuf,
        /// Version string recorded in the legacy index metadata
        index_version: String,
    },

    /// CD predicates require index schema version >= 2 with `body_hash` support
    #[error(
        "CD predicates require index schema version {required_version}+, but index at {path} has version {current_version}. Rebuild with `sqry index --force {path}` to enable CD predicates."
    )]
    IndexMissingCDSupport {
        /// Path to the workspace whose index lacks CD support
        path: PathBuf,
        /// Current index schema version
        current_version: u32,
        /// Required minimum version for CD predicates
        required_version: u32,
    },

    /// Plugin not found
    #[error("Language plugin not found for: {language}")]
    PluginNotFound {
        /// Language identifier
        language: String,
    },

    /// Field evaluation failed
    #[error("Failed to evaluate field '{field}': {error}")]
    FieldEvaluationFailed {
        /// The field that failed to evaluate
        field: String,
        /// The error message
        error: String,
    },

    /// Type mismatch during execution
    #[error("Type mismatch: expected {expected}, got {got}")]
    TypeMismatch {
        /// Expected type
        expected: &'static str,
        /// Actual type
        got: String,
    },

    /// Invalid regex
    #[error("Invalid regex pattern '{pattern}': {error}")]
    InvalidRegex {
        /// The invalid regex pattern
        pattern: String,
        /// The regex compilation error
        error: String,
    },

    /// Invalid glob pattern
    #[error("Invalid glob pattern '{pattern}': {error}")]
    InvalidGlob {
        /// The invalid glob pattern
        pattern: String,
        /// The glob compilation error
        error: String,
    },

    /// Too many glob matches
    #[error("Glob pattern '{pattern}' matched too many files (limit: {limit})")]
    TooManyMatches {
        /// The glob pattern
        pattern: String,
        /// The match limit
        limit: usize,
    },

    /// File read error
    #[error("Failed to read file: {0}")]
    FileReadError(#[from] std::io::Error),

    /// Regex error
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),

    /// Query timeout
    #[error("Query execution timed out after {seconds} seconds")]
    Timeout {
        /// Timeout duration in seconds
        seconds: u64,
    },

    /// Query was cancelled by the client (LSP $/cancelRequest)
    #[error("Query execution was cancelled")]
    Cancelled,

    /// Index version mismatch
    #[error("Index version mismatch: expected {expected}, found {found}")]
    IndexVersionMismatch {
        /// Expected index version
        expected: String,
        /// Found index version
        found: String,
    },
}

/// Format a list of operators for display
fn format_operators(operators: &[Operator]) -> String {
    operators
        .iter()
        .map(|op| format!("'{op}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format a field type for display
fn format_field_type(field_type: &FieldType) -> String {
    match field_type {
        FieldType::String => "string".to_string(),
        FieldType::Bool => "boolean".to_string(),
        FieldType::Number => "number".to_string(),
        FieldType::Enum(values) => format!("enum ({})", values.join(", ")),
        FieldType::Path => "path".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== QueryError tests =====

    #[test]
    fn test_query_error_exit_code_syntax() {
        let lex_err = QueryError::Lex(LexError::UnexpectedEof);
        assert_eq!(lex_err.exit_code(), 2);
    }

    #[test]
    fn test_query_error_exit_code_parse() {
        let parse_err = QueryError::Parse(ParseError::EmptyQuery);
        assert_eq!(parse_err.exit_code(), 2);
    }

    #[test]
    fn test_query_error_exit_code_validation() {
        let val_err = QueryError::Validation(ValidationError::UnknownField {
            field: "test".to_string(),
            suggestion: None,
            span: Span::new(0, 4),
        });
        assert_eq!(val_err.exit_code(), 2);
    }

    #[test]
    fn test_query_error_exit_code_execution() {
        let exec_err = QueryError::Execution(ExecutionError::Cancelled);
        assert_eq!(exec_err.exit_code(), 1);
    }

    #[test]
    fn test_query_error_with_source() {
        let err = QueryError::Lex(LexError::UnexpectedEof);
        let rich = err.with_source("kind:function");
        assert_eq!(rich.exit_code(), 2);
    }

    // ===== RichQueryError tests =====

    #[test]
    fn test_rich_query_error_new() {
        let err = QueryError::Parse(ParseError::EmptyQuery);
        let rich = RichQueryError::new(err, "");
        assert!(matches!(rich.inner(), QueryError::Parse(_)));
    }

    #[test]
    fn test_rich_query_error_to_json_lex() {
        let err = QueryError::Lex(LexError::UnterminatedString {
            span: Span::new(0, 5),
        });
        let rich = RichQueryError::new(err, "\"hello");
        let json = rich.to_json_value();

        assert_eq!(json["error"]["code"], "sqry::syntax");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Unterminated")
        );
        assert_eq!(json["error"]["query"], "\"hello");
        assert!(json["error"]["span"].is_object());
    }

    #[test]
    fn test_rich_query_error_to_json_parse() {
        let err = QueryError::Parse(ParseError::EmptyQuery);
        let rich = RichQueryError::new(err, "");
        let json = rich.to_json_value();

        assert_eq!(json["error"]["code"], "sqry::parse");
    }

    #[test]
    fn test_rich_query_error_to_json_validation() {
        let err = QueryError::Validation(ValidationError::UnknownField {
            field: "knd".to_string(),
            suggestion: Some("kind".to_string()),
            span: Span::new(0, 3),
        });
        let rich = RichQueryError::new(err, "knd:function");
        let json = rich.to_json_value();

        assert_eq!(json["error"]["code"], "sqry::validation");
        assert!(json["error"]["suggestion"].is_string());
    }

    #[test]
    fn test_rich_query_error_to_json_execution() {
        let err = QueryError::Execution(ExecutionError::Cancelled);
        let rich = RichQueryError::new(err, "kind:function");
        let json = rich.to_json_value();

        assert_eq!(json["error"]["code"], "sqry::execution");
    }

    // ===== LexError tests =====

    #[test]
    fn test_lex_error_display() {
        let err = LexError::UnterminatedString {
            span: Span::new(10, 15),
        };
        let msg = err.to_string();
        assert!(msg.contains("Unterminated string"));
        assert!(msg.contains("10"));
    }

    #[test]
    fn test_lex_error_invalid_escape() {
        let err = LexError::InvalidEscape {
            char: 'x',
            span: Span::new(5, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid escape"));
        assert!(msg.contains("\\x"));
        assert!(msg.contains('5'));
    }

    #[test]
    fn test_lex_error_unterminated_regex() {
        let err = LexError::UnterminatedRegex {
            span: Span::new(0, 5),
        };
        let msg = err.to_string();
        assert!(msg.contains("Unterminated regex"));
    }

    #[test]
    fn test_lex_error_invalid_unicode_escape() {
        let err = LexError::InvalidUnicodeEscape {
            got: 'z',
            span: Span::new(0, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid Unicode escape"));
        assert!(msg.contains("'z'"));
    }

    #[test]
    fn test_lex_error_invalid_number() {
        let err = LexError::InvalidNumber {
            text: "123abc".to_string(),
            span: Span::new(0, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid number"));
        assert!(msg.contains("123abc"));
    }

    #[test]
    fn test_lex_error_number_overflow() {
        let err = LexError::NumberOverflow {
            text: "99999999999999999999".to_string(),
            error: "too large".to_string(),
            span: Span::new(0, 20),
        };
        let msg = err.to_string();
        assert!(msg.contains("Number overflow"));
        assert!(msg.contains("too large"));
    }

    #[test]
    fn test_lex_error_invalid_regex() {
        let err = LexError::InvalidRegex {
            pattern: "[unclosed".to_string(),
            error: "bracket not closed".to_string(),
            span: Span::new(0, 9),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid regex"));
        assert!(msg.contains("[unclosed"));
    }

    #[test]
    fn test_lex_error_unexpected_char() {
        let err = LexError::UnexpectedChar {
            char: '@',
            span: Span::new(5, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("Unexpected character"));
        assert!(msg.contains("'@'"));
    }

    #[test]
    fn test_lex_error_unexpected_eof() {
        let err = LexError::UnexpectedEof;
        let msg = err.to_string();
        assert!(msg.contains("Unexpected end of input"));
    }

    // ===== ParseError tests =====

    #[test]
    fn test_parse_error_expected_identifier() {
        use crate::query::lexer::{Token, TokenType};
        let err = ParseError::ExpectedIdentifier {
            token: Token::new(TokenType::NumberLiteral(123), Span::new(0, 3)),
        };
        let msg = err.to_string();
        assert!(msg.contains("Expected field name"));
    }

    #[test]
    fn test_validation_error_unknown_field() {
        let err = ValidationError::UnknownField {
            field: "knd".to_string(),
            suggestion: Some("kind".to_string()),
            span: Span::new(0, 3),
        };
        let msg = err.to_string();
        assert!(msg.contains("Unknown field 'knd'"));
        assert!(msg.contains("Did you mean 'kind'?"));
    }

    #[test]
    fn test_validation_error_unknown_field_no_suggestion() {
        let err = ValidationError::UnknownField {
            field: "xyz".to_string(),
            suggestion: None,
            span: Span::new(0, 3),
        };
        let msg = err.to_string();
        assert!(msg.contains("Unknown field 'xyz'"));
        assert!(!msg.contains("Did you mean"));
    }

    #[test]
    fn test_validation_error_invalid_operator() {
        let err = ValidationError::InvalidOperator {
            field: "kind".to_string(),
            operator: Operator::Greater,
            valid_operators: vec![Operator::Equal, Operator::Regex],
            span: Span::new(5, 6),
        };
        let msg = err.to_string();
        assert!(msg.contains("Operator '>'"));
        assert!(msg.contains("kind"));
        assert!(msg.contains("Valid operators"));
        assert!(msg.contains("':'"));
        assert!(msg.contains("'~='"));
    }

    #[test]
    fn test_validation_error_type_mismatch() {
        let err = ValidationError::TypeMismatch {
            field: "async".to_string(),
            expected: FieldType::Bool,
            got: Value::Number(42),
            span: Span::new(10, 12),
        };
        let msg = err.to_string();
        assert!(msg.contains("Type mismatch"));
        assert!(msg.contains("async"));
        assert!(msg.contains("boolean"));
        assert!(msg.contains("Number"));
    }

    #[test]
    fn test_validation_error_invalid_enum_value() {
        let err = ValidationError::InvalidEnumValue {
            field: "kind".to_string(),
            value: "invalid".to_string(),
            valid_values: vec!["function", "class", "method"],
            span: Span::new(5, 12),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid value 'invalid'"));
        assert!(msg.contains("kind"));
        assert!(msg.contains("function, class, method"));
    }

    #[test]
    fn test_execution_error_type_mismatch() {
        let err = ExecutionError::TypeMismatch {
            expected: "string",
            got: "Number(42)".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Type mismatch"));
        assert!(msg.contains("expected string"));
        assert!(msg.contains("got Number(42)"));
    }

    #[test]
    fn test_execution_error_invalid_glob() {
        let err = ExecutionError::InvalidGlob {
            pattern: "[invalid".to_string(),
            error: "unclosed character class".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid glob pattern"));
        assert!(msg.contains("[invalid"));
        assert!(msg.contains("unclosed"));
    }

    #[test]
    fn test_execution_error_too_many_matches() {
        let err = ExecutionError::TooManyMatches {
            pattern: "**/*.rs".to_string(),
            limit: 10000,
        };
        let msg = err.to_string();
        assert!(msg.contains("too many files"));
        assert!(msg.contains("**/*.rs"));
        assert!(msg.contains("10000"));
    }

    #[test]
    fn test_execution_error_index_not_found() {
        let err = ExecutionError::IndexNotFound {
            path: PathBuf::from("/path/to/project"),
        };
        let msg = err.to_string();
        assert!(msg.contains("Index not found"));
        assert!(msg.contains("/path/to/project"));
    }

    #[test]
    fn test_execution_error_relation_query_requires_index() {
        let err = ExecutionError::RelationQueryRequiresIndex {
            path: PathBuf::from("/project"),
        };
        let msg = err.to_string();
        assert!(msg.contains("Relation queries require an index"));
    }

    #[test]
    fn test_execution_error_legacy_index_missing_relations() {
        let err = ExecutionError::LegacyIndexMissingRelations {
            path: PathBuf::from("/project"),
            index_version: "1.0.0".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Legacy index"));
        assert!(msg.contains("lacks relation data"));
    }

    #[test]
    fn test_execution_error_index_missing_cd_support() {
        let err = ExecutionError::IndexMissingCDSupport {
            path: PathBuf::from("/project"),
            current_version: 1,
            required_version: 2,
        };
        let msg = err.to_string();
        assert!(msg.contains("CD predicates require"));
    }

    #[test]
    fn test_execution_error_plugin_not_found() {
        let err = ExecutionError::PluginNotFound {
            language: "Brainfuck".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Language plugin not found"));
        assert!(msg.contains("Brainfuck"));
    }

    #[test]
    fn test_execution_error_field_evaluation_failed() {
        let err = ExecutionError::FieldEvaluationFailed {
            field: "custom_field".to_string(),
            error: "not implemented".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Failed to evaluate field"));
        assert!(msg.contains("custom_field"));
    }

    #[test]
    fn test_execution_error_invalid_regex() {
        let err = ExecutionError::InvalidRegex {
            pattern: "(unclosed".to_string(),
            error: "unclosed group".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Invalid regex"));
        assert!(msg.contains("(unclosed"));
    }

    #[test]
    fn test_execution_error_timeout() {
        let err = ExecutionError::Timeout { seconds: 30 };
        let msg = err.to_string();
        assert!(msg.contains("timed out"));
        assert!(msg.contains("30"));
    }

    #[test]
    fn test_execution_error_cancelled() {
        let err = ExecutionError::Cancelled;
        let msg = err.to_string();
        assert!(msg.contains("cancelled"));
    }

    #[test]
    fn test_execution_error_index_version_mismatch() {
        let err = ExecutionError::IndexVersionMismatch {
            expected: "2.0".to_string(),
            found: "1.0".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("version mismatch"));
        assert!(msg.contains("expected 2.0"));
        assert!(msg.contains("found 1.0"));
    }

    // ===== Diagnostic impl tests =====

    #[test]
    fn test_diagnostic_code() {
        use miette::Diagnostic;

        let lex_rich = RichQueryError::new(QueryError::Lex(LexError::UnexpectedEof), "test");
        assert!(lex_rich.code().unwrap().to_string().contains("syntax"));

        let parse_rich = RichQueryError::new(QueryError::Parse(ParseError::EmptyQuery), "");
        assert!(parse_rich.code().unwrap().to_string().contains("parse"));
    }

    #[test]
    fn test_query_error_wrapping() {
        let lex_err = LexError::UnexpectedEof;
        let query_err = QueryError::from(lex_err);
        let msg = query_err.to_string();
        assert!(msg.contains("Syntax error"));
    }

    #[test]
    fn test_format_operators() {
        let ops = vec![Operator::Equal, Operator::Regex, Operator::Greater];
        let formatted = format_operators(&ops);
        assert_eq!(formatted, "':', '~=', '>'");
    }

    #[test]
    fn test_format_field_type() {
        assert_eq!(format_field_type(&FieldType::String), "string");
        assert_eq!(format_field_type(&FieldType::Bool), "boolean");
        assert_eq!(format_field_type(&FieldType::Number), "number");
        assert_eq!(format_field_type(&FieldType::Path), "path");

        let enum_type = FieldType::Enum(vec!["function", "class"]);
        assert_eq!(format_field_type(&enum_type), "enum (function, class)");
    }
}
