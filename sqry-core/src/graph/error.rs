//! Error types used by graph builders.
//!
//! The unified graph architecture expects language-specific builders to surface
//! rich, structured errors instead of raw strings.  This improves debugging,
//! allows callers to decide whether an error is recoverable, and keeps us in

use std::path::PathBuf;

use super::node::Span;
use thiserror::Error;

/// Result alias used throughout the graph builder pipeline.
pub type GraphResult<T> = Result<T, GraphBuilderError>;

/// Errors that can occur while constructing the unified code graph.
#[derive(Debug, Error)]
pub enum GraphBuilderError {
    /// Tree-sitter failed to parse a construct that the builder expected.
    #[error("Failed to parse AST node at {span:?}: {reason}")]
    ParseError {
        /// Source span for the problematic node.
        span: Span,
        /// Human readable error message.
        reason: String,
    },

    /// The builder encountered a language feature that is currently unsupported.
    #[error("Unsupported language construct: {construct} at {span:?}")]
    UnsupportedConstruct {
        /// Description of the unsupported construct (e.g., "async generator").
        construct: String,
        /// Span for the construct.
        span: Span,
    },

    /// A symbol reference could not be resolved to a known definition.
    #[error("Failed to resolve symbol '{symbol}' in {file}")]
    SymbolResolutionError {
        /// Node that the builder attempted to resolve.
        symbol: String,
        /// File where resolution failed.
        file: PathBuf,
    },

    /// Building a cross-language edge failed validation (e.g. inconsistent metadata).
    #[error("Invalid cross-language edge: {reason}")]
    CrossLanguageError {
        /// Why the edge construction failed.
        reason: String,
    },

    /// An IO failure occurred while reading source input.
    #[error("IO error reading {file}: {source}")]
    IoError {
        /// File being read when the failure occurred.
        file: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Position;

    fn make_test_span() -> Span {
        Span::new(Position::new(10, 5), Position::new(10, 25))
    }

    // GraphResult tests
    #[test]
    fn test_graph_result_ok() {
        let result: GraphResult<i32> = Ok(42);
        assert!(result.is_ok());
        if let Ok(value) = result {
            assert_eq!(value, 42);
        }
    }

    #[test]
    fn test_graph_result_err() {
        let result: GraphResult<i32> = Err(GraphBuilderError::ParseError {
            span: make_test_span(),
            reason: "test error".to_string(),
        });
        assert!(result.is_err());
    }

    // ParseError tests
    #[test]
    fn test_parse_error_display() {
        let err = GraphBuilderError::ParseError {
            span: make_test_span(),
            reason: "unexpected token".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Failed to parse AST node"));
        assert!(msg.contains("unexpected token"));
    }

    #[test]
    fn test_parse_error_debug() {
        let err = GraphBuilderError::ParseError {
            span: make_test_span(),
            reason: "test".to_string(),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("ParseError"));
        assert!(debug.contains("span"));
        assert!(debug.contains("reason"));
    }

    // UnsupportedConstruct tests
    #[test]
    fn test_unsupported_construct_display() {
        let err = GraphBuilderError::UnsupportedConstruct {
            construct: "async generator".to_string(),
            span: make_test_span(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Unsupported language construct"));
        assert!(msg.contains("async generator"));
    }

    #[test]
    fn test_unsupported_construct_debug() {
        let err = GraphBuilderError::UnsupportedConstruct {
            construct: "macro".to_string(),
            span: make_test_span(),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("UnsupportedConstruct"));
        assert!(debug.contains("macro"));
    }

    // SymbolResolutionError tests
    #[test]
    fn test_symbol_resolution_error_display() {
        let err = GraphBuilderError::SymbolResolutionError {
            symbol: "MyClass".to_string(),
            file: PathBuf::from("src/main.rs"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Failed to resolve symbol"));
        assert!(msg.contains("MyClass"));
        assert!(msg.contains("src/main.rs"));
    }

    #[test]
    fn test_symbol_resolution_error_debug() {
        let err = GraphBuilderError::SymbolResolutionError {
            symbol: "helper_fn".to_string(),
            file: PathBuf::from("lib.rs"),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("SymbolResolutionError"));
        assert!(debug.contains("helper_fn"));
    }

    // CrossLanguageError tests
    #[test]
    fn test_cross_language_error_display() {
        let err = GraphBuilderError::CrossLanguageError {
            reason: "incompatible metadata formats".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Invalid cross-language edge"));
        assert!(msg.contains("incompatible metadata formats"));
    }

    #[test]
    fn test_cross_language_error_debug() {
        let err = GraphBuilderError::CrossLanguageError {
            reason: "test reason".to_string(),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("CrossLanguageError"));
        assert!(debug.contains("test reason"));
    }

    // IoError tests
    #[test]
    fn test_io_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = GraphBuilderError::IoError {
            file: PathBuf::from("/tmp/missing.rs"),
            source: io_err,
        };
        let msg = format!("{err}");
        assert!(msg.contains("IO error reading"));
        assert!(msg.contains("/tmp/missing.rs"));
    }

    #[test]
    fn test_io_error_source() {
        use std::error::Error;

        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = GraphBuilderError::IoError {
            file: PathBuf::from("/etc/passwd"),
            source: io_err,
        };

        // The #[source] attribute should make this accessible
        let source = err.source();
        assert!(source.is_some());
    }

    #[test]
    fn test_io_error_debug() {
        let io_err = std::io::Error::other("test");
        let err = GraphBuilderError::IoError {
            file: PathBuf::from("test.rs"),
            source: io_err,
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("IoError"));
        assert!(debug.contains("test.rs"));
    }

    // Pattern matching tests
    #[test]
    fn test_error_pattern_matching() {
        let err = GraphBuilderError::ParseError {
            span: make_test_span(),
            reason: "test".to_string(),
        };

        match err {
            GraphBuilderError::ParseError { span, reason } => {
                assert_eq!(span.start.line, 10);
                assert_eq!(reason, "test");
            }
            _ => panic!("Expected ParseError variant"),
        }
    }

    #[test]
    fn test_all_variants_are_error() {
        use std::error::Error;

        let errors: Vec<GraphBuilderError> = vec![
            GraphBuilderError::ParseError {
                span: make_test_span(),
                reason: "test".to_string(),
            },
            GraphBuilderError::UnsupportedConstruct {
                construct: "test".to_string(),
                span: make_test_span(),
            },
            GraphBuilderError::SymbolResolutionError {
                symbol: "test".to_string(),
                file: PathBuf::from("test.rs"),
            },
            GraphBuilderError::CrossLanguageError {
                reason: "test".to_string(),
            },
            GraphBuilderError::IoError {
                file: PathBuf::from("test.rs"),
                source: std::io::Error::other("test"),
            },
        ];

        for err in errors {
            // All variants should implement std::error::Error
            let _: &dyn Error = &err;
            // All variants should have a non-empty Display
            assert!(!format!("{err}").is_empty());
        }
    }
}
