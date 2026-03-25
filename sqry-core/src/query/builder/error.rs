//! Error types for the Query Builder API

use crate::query::Operator;
use std::fmt;

/// Errors that can occur when building a query
#[derive(Debug, Clone)]
pub enum BuildError {
    /// Unknown field name (not in registry)
    UnknownField {
        /// The field name that was not found
        field: String,
        /// List of available field names
        available: String,
    },

    /// Operator not valid for the field type
    InvalidOperator {
        /// The field name
        field: String,
        /// The operator that was used
        operator: Operator,
        /// The field type that doesn't support this operator
        field_type: String,
    },

    /// Value type doesn't match field type
    ValueTypeMismatch {
        /// The field name
        field: String,
        /// The expected value type
        expected: String,
        /// The actual value type provided
        actual: String,
    },

    /// Invalid enum value for an enum field
    InvalidEnumValue {
        /// The field name
        field: String,
        /// The invalid value provided
        value: String,
        /// List of valid enum values
        valid: String,
    },

    /// Invalid regex pattern (from standard regex crate)
    InvalidRegex(regex::Error),

    /// Invalid regex pattern with lookaround (from fancy-regex crate)
    InvalidFancyRegex {
        /// The invalid pattern
        pattern: String,
        /// The error message
        error: String,
    },

    /// Empty query (no conditions specified)
    EmptyQuery,

    /// Multiple validation errors
    Multiple(Vec<BuildError>),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::UnknownField { field, available } => {
                write!(f, "unknown field '{field}', available fields: {available}")
            }
            BuildError::InvalidOperator {
                field,
                operator,
                field_type,
            } => {
                write!(
                    f,
                    "operator '{operator:?}' not valid for field '{field}' (type: {field_type})"
                )
            }
            BuildError::ValueTypeMismatch {
                field,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "value type mismatch for field '{field}': expected {expected}, got {actual}"
                )
            }
            BuildError::InvalidEnumValue {
                field,
                value,
                valid,
            } => {
                write!(
                    f,
                    "invalid enum value '{value}' for field '{field}', valid values: {valid}"
                )
            }
            BuildError::InvalidRegex(e) => {
                write!(f, "invalid regex pattern: {e}")
            }
            BuildError::InvalidFancyRegex { pattern, error } => {
                write!(f, "invalid regex pattern '{pattern}': {error}")
            }
            BuildError::EmptyQuery => {
                write!(f, "empty query: no conditions specified")
            }
            BuildError::Multiple(errors) => {
                write!(f, "multiple validation errors:")?;
                for (i, err) in errors.iter().enumerate() {
                    write!(f, "\n  {}: {err}", i + 1)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BuildError::InvalidRegex(e) => Some(e),
            _ => None,
        }
    }
}

impl From<regex::Error> for BuildError {
    fn from(err: regex::Error) -> Self {
        BuildError::InvalidRegex(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_field_display() {
        let err = BuildError::UnknownField {
            field: "foo".to_string(),
            available: "kind, name, path".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "unknown field 'foo', available fields: kind, name, path"
        );
    }

    #[test]
    fn test_invalid_operator_display() {
        let err = BuildError::InvalidOperator {
            field: "kind".to_string(),
            operator: Operator::Greater,
            field_type: "Enum".to_string(),
        };
        assert!(err.to_string().contains("operator"));
        assert!(err.to_string().contains("kind"));
    }

    #[test]
    fn test_value_type_mismatch_display() {
        let err = BuildError::ValueTypeMismatch {
            field: "name".to_string(),
            expected: "String".to_string(),
            actual: "Number".to_string(),
        };
        assert!(err.to_string().contains("value type mismatch"));
        assert!(err.to_string().contains("expected String"));
    }

    #[test]
    fn test_invalid_enum_value_display() {
        let err = BuildError::InvalidEnumValue {
            field: "kind".to_string(),
            value: "invalid".to_string(),
            valid: "function, class, method".to_string(),
        };
        assert!(err.to_string().contains("invalid enum value"));
        assert!(err.to_string().contains("function, class, method"));
    }

    #[test]
    fn test_empty_query_display() {
        let err = BuildError::EmptyQuery;
        assert_eq!(err.to_string(), "empty query: no conditions specified");
    }

    #[test]
    fn test_multiple_errors_display() {
        let err = BuildError::Multiple(vec![
            BuildError::EmptyQuery,
            BuildError::UnknownField {
                field: "foo".to_string(),
                available: "kind".to_string(),
            },
        ]);
        let msg = err.to_string();
        assert!(msg.contains("multiple validation errors"));
        assert!(msg.contains("empty query"));
        assert!(msg.contains("unknown field"));
    }

    #[test]
    #[allow(clippy::invalid_regex)] // Intentionally invalid regex to test error conversion
    fn test_regex_error_conversion() {
        let regex_err = regex::Regex::new("[invalid").unwrap_err();
        let build_err: BuildError = regex_err.into();
        assert!(matches!(build_err, BuildError::InvalidRegex(_)));
        assert!(build_err.to_string().contains("invalid regex pattern"));
    }
}
