//! Structured error types for LLM tool integration
//!
//! This module provides standardized error codes and responses that enable
//! LLM agents to handle failures gracefully and suggest actionable fixes.

use serde::Serialize;
use std::fmt;

/// Standardized error codes for tool integration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Index file not found - agent should run `sqry index`
    IndexMissing,
    /// Query returned zero results
    NoMatches,
    /// Query syntax is invalid
    InvalidQuery,
    /// Scope too broad (>100k files by default)
    TooManyFiles,
    /// Path doesn't exist or not accessible
    InvalidPath,
    /// Language filter references unsupported language
    UnsupportedLanguage,
    /// Generic internal error
    Internal,
}

impl ErrorCode {
    /// Get the error code as a string
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::IndexMissing => "INDEX_MISSING",
            ErrorCode::NoMatches => "NO_MATCHES",
            ErrorCode::InvalidQuery => "INVALID_QUERY",
            ErrorCode::TooManyFiles => "TOO_MANY_FILES",
            ErrorCode::InvalidPath => "INVALID_PATH",
            ErrorCode::UnsupportedLanguage => "UNSUPPORTED_LANGUAGE",
            ErrorCode::Internal => "INTERNAL",
        }
    }

    /// Get a default message for this error code
    #[must_use]
    pub fn default_message(&self) -> &'static str {
        match self {
            ErrorCode::IndexMissing => "No symbol index found",
            ErrorCode::NoMatches => "No symbols match the query",
            ErrorCode::InvalidQuery => "Query syntax is invalid",
            ErrorCode::TooManyFiles => "Scope too broad - too many files to process",
            ErrorCode::InvalidPath => "Path does not exist or is not accessible",
            ErrorCode::UnsupportedLanguage => "Language is not supported",
            ErrorCode::Internal => "Internal error occurred",
        }
    }

    /// Get a suggested action for this error
    #[must_use]
    pub fn suggestion(&self, context: Option<&str>) -> String {
        match self {
            ErrorCode::IndexMissing => {
                let path = context.unwrap_or(".");
                format!("Run: sqry index {path}")
            }
            ErrorCode::NoMatches => {
                "Try broadening your search or using fuzzy mode with --fuzzy".to_string()
            }
            ErrorCode::InvalidQuery => {
                "Check query syntax. Example: kind:function AND name:test".to_string()
            }
            ErrorCode::TooManyFiles => {
                "Narrow the scope with a more specific path or use filters".to_string()
            }
            ErrorCode::InvalidPath => {
                "Verify the path exists and you have read permissions".to_string()
            }
            ErrorCode::UnsupportedLanguage => {
                "Run 'sqry --list-languages' to see supported languages".to_string()
            }
            ErrorCode::Internal => "Please report this issue on GitHub".to_string(),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Structured error response for JSON output
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    /// Error code (e.g., "`INDEX_MISSING`")
    pub code: String,
    /// Human-readable error message
    pub message: String,
    /// Optional context (e.g., path that failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Suggested action to fix the error
    pub suggestion: String,
}

impl ErrorResponse {
    /// Create a new error response
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str().to_string(),
            message: message.into(),
            path: None,
            suggestion: code.suggestion(None),
        }
    }

    /// Create error response with path context
    pub fn with_path(code: ErrorCode, message: impl Into<String>, path: impl Into<String>) -> Self {
        let path_str = path.into();
        Self {
            code: code.as_str().to_string(),
            message: message.into(),
            suggestion: code.suggestion(Some(&path_str)),
            path: Some(path_str),
        }
    }

    /// Create error response with custom suggestion
    pub fn with_suggestion(
        code: ErrorCode,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_string(),
            message: message.into(),
            path: None,
            suggestion: suggestion.into(),
        }
    }
}

/// Result type using `ErrorResponse`
pub type ToolResult<T> = Result<T, ErrorResponse>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_as_str() {
        assert_eq!(ErrorCode::IndexMissing.as_str(), "INDEX_MISSING");
        assert_eq!(ErrorCode::NoMatches.as_str(), "NO_MATCHES");
        assert_eq!(ErrorCode::InvalidQuery.as_str(), "INVALID_QUERY");
    }

    #[test]
    fn test_error_code_display() {
        assert_eq!(format!("{}", ErrorCode::IndexMissing), "INDEX_MISSING");
    }

    #[test]
    fn test_error_response_new() {
        let err = ErrorResponse::new(ErrorCode::IndexMissing, "Index not found");
        assert_eq!(err.code, "INDEX_MISSING");
        assert_eq!(err.message, "Index not found");
        assert!(err.path.is_none());
        assert!(err.suggestion.starts_with("Run: sqry index"));
    }

    #[test]
    fn test_error_response_with_path() {
        let err = ErrorResponse::with_path(
            ErrorCode::InvalidPath,
            "Path does not exist",
            "/invalid/path",
        );
        assert_eq!(err.code, "INVALID_PATH");
        assert_eq!(err.path, Some("/invalid/path".to_string()));
    }

    #[test]
    fn test_error_response_serialization() {
        let err = ErrorResponse::new(ErrorCode::NoMatches, "No results");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"code\":\"NO_MATCHES\""));
        assert!(json.contains("\"message\":\"No results\""));
    }

    #[test]
    fn test_suggestion_with_context() {
        let suggestion = ErrorCode::IndexMissing.suggestion(Some("/home/project"));
        assert_eq!(suggestion, "Run: sqry index /home/project");
    }
}
