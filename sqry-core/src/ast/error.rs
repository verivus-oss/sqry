//! Error types for AST query system

use thiserror::Error;

/// Errors that can occur during AST query operations
#[derive(Debug, Error)]
pub enum AstQueryError {
    /// Query parsing failed
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Unknown predicate in query
    #[error(
        "Unknown predicate: '{predicate}'. Available: kind, name~=, parent, in, depth, path, lang"
    )]
    UnknownPredicate {
        /// The unknown predicate name
        predicate: String,
    },

    /// Invalid regex pattern
    #[error("Invalid regex pattern: '{pattern}'")]
    InvalidRegex {
        /// The invalid regex pattern
        pattern: String,
        /// The underlying regex error
        #[source]
        source: regex::Error,
    },

    /// Invalid depth value
    #[error("Invalid depth value: '{value}' (must be a positive number)")]
    InvalidDepth {
        /// The invalid depth value
        value: String,
    },

    /// Unexpected token during parsing
    #[error("Unexpected token: expected {expected}, got {actual}")]
    UnexpectedToken {
        /// Expected token description
        expected: String,
        /// Actual token received
        actual: String,
    },

    /// Context extraction failed
    #[error("Context extraction failed: {0}")]
    ContextExtraction(String),

    /// I/O error
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience type alias for Results
pub type Result<T> = std::result::Result<T, AstQueryError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn test_error_display_messages() {
        let cases = vec![
            (
                AstQueryError::UnknownPredicate {
                    predicate: "foo".to_string(),
                },
                "Unknown predicate: 'foo'",
            ),
            (
                AstQueryError::InvalidDepth {
                    value: "abc".to_string(),
                },
                "Invalid depth value: 'abc'",
            ),
            (
                AstQueryError::ParseError("test".to_string()),
                "Parse error: test",
            ),
        ];

        for (error, expected) in cases {
            assert!(
                error.to_string().contains(expected),
                "Error message '{error}' should contain '{expected}'"
            );
        }
    }

    #[test]
    #[allow(clippy::invalid_regex)]
    fn test_error_source_chaining() {
        let regex_err = regex::Regex::new("[invalid").unwrap_err();
        let ast_err = AstQueryError::InvalidRegex {
            pattern: "[invalid".to_string(),
            source: regex_err,
        };

        assert!(ast_err.source().is_some(), "Error should have source");
    }
}
