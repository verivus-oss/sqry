//! Normalized representation of call edges returned by query helpers.
//!
//! The [`CallSite`] struct captures the essential details about a call edge,
//! including the caller/callee identifiers, source span, and metadata that
//! records detection provenance plus language-specific extras.

use super::edge::{CodeEdge, DetectionMethod, EdgeId};
use super::node::{NodeId, Span};

/// Canonical call-site information returned by the query layer.
#[derive(Debug, Clone, PartialEq)]
pub struct CallSite {
    /// Fully-qualified caller symbol.
    pub caller: NodeId,
    /// Fully-qualified callee symbol.
    pub callee: NodeId,
    /// Stable identifier for the edge that produced this call site.
    pub edge_id: EdgeId,
    /// Source span of the call expression, if available.
    pub span: Option<Span>,
    /// Metadata describing how the call was detected.
    pub metadata: CallSiteMetadata,
}

/// Detection metadata bundled with a [`CallSite`].
#[derive(Debug, Clone, PartialEq)]
pub struct CallSiteMetadata {
    /// Confidence score (0.0-1.0) for this call site.
    pub confidence: f32,
    /// Detection strategy that produced this call (`DetectionMethod` mirrors [`EdgeMetadata`](crate::graph::EdgeMetadata)).
    pub detection_method: DetectionMethod,
    /// Human readable explanation (e.g. `"direct call"`, `"axios.post detector"`).
    pub detector_hint: Option<String>,
    /// Language-specific extras (argument count, template info, async flags, etc.).
    pub extras: CallSiteExtras,
}

/// Language-specific payloads attached to [`CallSiteMetadata`].
#[derive(Debug, Clone, PartialEq)]
pub enum CallSiteExtras {
    /// No extra metadata for this call site.
    None,
    /// Additional details for C++ call sites.
    Cpp {
        /// Number of arguments passed in the call expression.
        argument_count: usize,
        /// Whether the call came from a template instantiation.
        is_template_instantiation: bool,
    },
    /// Additional details for JavaScript/TypeScript call sites.
    JavaScript {
        /// Whether the call occurs inside an async function.
        is_async: bool,
        /// Whether the call expression uses the `await` keyword.
        uses_await: bool,
        /// HTTP call metadata (fetch, axios, etc.) for cross-language edges.
        http_method: Option<String>,
        /// HTTP endpoint for cross-language edges.
        http_endpoint: Option<String>,
    },
    /// Additional details for Python call sites.
    Python {
        /// Whether the function containing the call is declared `async`.
        is_async: bool,
        /// Whether the call is to a method (vs a standalone function).
        is_method: bool,
        /// Whether the function has decorators applied (e.g., @staticmethod).
        uses_decorator: bool,
    },
    /// Additional details for Rust call sites.
    Rust {
        /// Whether the call occurs inside an async function.
        is_async: bool,
        /// Whether the call expression uses `.await`.
        uses_await: bool,
        /// Whether the containing function is marked `unsafe`.
        is_unsafe: bool,
        /// Whether the call is a method call (`receiver.method()` or `self.method()`).
        is_method: bool,
        /// Whether the call occurs inside an `unsafe {}` block.
        is_unsafe_block: bool,
    },
    /// Additional details for Java call sites.
    Java {
        /// Whether the call occurs inside a static method.
        is_static: bool,
    },
    /// Additional details for Go call sites.
    Go {
        /// Whether this call is launched as a goroutine (`go foo()`).
        is_goroutine: bool,
        /// Whether this call is deferred (`defer foo()`).
        is_deferred: bool,
        /// Whether this call is to a Go builtin function (make, new, len, etc.).
        is_builtin: bool,
        /// Whether this call is a method call (`receiver.Method()`).
        is_method: bool,
    },
    /// Additional details for C call sites.
    C {
        /// Whether the call is likely through a function pointer.
        is_function_pointer: bool,
    },
    /// Additional details for C# call sites.
    CSharp {
        /// Whether the call occurs inside an async method.
        is_async: bool,
        /// Whether the call uses the `await` keyword.
        uses_await: bool,
        /// Whether the call is to a property (synthetic getter/setter).
        is_property_access: bool,
    },
    /// Additional details for Lua call sites.
    Lua {
        /// Whether the call is to a method (colon syntax).
        is_method: bool,
    },
    /// Additional details for Perl call sites.
    Perl {
        /// Whether the call is to a method.
        is_method: bool,
    },
    /// Additional details for Shell call sites.
    Shell {
        /// Whether the call is a built-in command.
        is_builtin: bool,
    },
    /// Additional details for Groovy call sites.
    Groovy {
        /// Whether the call is to a closure.
        is_closure: bool,
    },
}

impl CallSite {
    /// Construct a [`CallSite`] from a [`CodeEdge`] and the provided extras.
    #[must_use]
    pub fn from_edge(edge: &CodeEdge, extras: CallSiteExtras) -> Self {
        Self {
            caller: edge.from.clone(),
            callee: edge.to.clone(),
            edge_id: edge.id,
            span: edge.metadata.span,
            metadata: CallSiteMetadata {
                confidence: edge.metadata.confidence,
                detection_method: edge.metadata.detection_method,
                detector_hint: edge.metadata.reason.clone(),
                extras,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::edge::{EdgeKind, EdgeMetadata};
    use crate::graph::node::{Language, Position};

    fn make_test_edge() -> CodeEdge {
        CodeEdge {
            id: EdgeId::new(),
            from: NodeId::new(Language::Rust, "src/main.rs", "main"),
            to: NodeId::new(Language::Rust, "src/lib.rs", "helper"),
            kind: EdgeKind::Call {
                argument_count: 2,
                is_async: false,
            },
            metadata: EdgeMetadata {
                span: Some(Span::new(Position::new(10, 5), Position::new(10, 20))),
                confidence: 0.95,
                detection_method: DetectionMethod::ASTAnalysis,
                reason: Some("direct call".to_string()),
                caller_identity: None,
                callee_identity: None,
            },
        }
    }

    // CallSite tests
    #[test]
    fn test_call_site_from_edge() {
        let edge = make_test_edge();
        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);

        assert_eq!(call_site.caller, edge.from);
        assert_eq!(call_site.callee, edge.to);
        assert_eq!(call_site.edge_id, edge.id);
        assert_eq!(call_site.span, edge.metadata.span);
        assert!((call_site.metadata.confidence - 0.95).abs() < f32::EPSILON);
        assert_eq!(
            call_site.metadata.detection_method,
            DetectionMethod::ASTAnalysis
        );
        assert_eq!(
            call_site.metadata.detector_hint,
            Some("direct call".to_string())
        );
    }

    #[test]
    fn test_call_site_from_edge_no_span() {
        let mut edge = make_test_edge();
        edge.metadata.span = None;
        edge.metadata.reason = None;

        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);
        assert!(call_site.span.is_none());
        assert!(call_site.metadata.detector_hint.is_none());
    }

    #[test]
    fn test_call_site_clone() {
        let edge = make_test_edge();
        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);
        let cloned = call_site.clone();

        assert_eq!(call_site, cloned);
    }

    #[test]
    fn test_call_site_debug() {
        let edge = make_test_edge();
        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);
        let debug_str = format!("{call_site:?}");

        assert!(debug_str.contains("CallSite"));
        assert!(debug_str.contains("caller"));
        assert!(debug_str.contains("callee"));
    }

    // CallSiteMetadata tests
    #[test]
    fn test_call_site_metadata_eq() {
        let meta1 = CallSiteMetadata {
            confidence: 0.9,
            detection_method: DetectionMethod::ASTAnalysis,
            detector_hint: None,
            extras: CallSiteExtras::None,
        };
        let meta2 = CallSiteMetadata {
            confidence: 0.9,
            detection_method: DetectionMethod::ASTAnalysis,
            detector_hint: None,
            extras: CallSiteExtras::None,
        };
        assert_eq!(meta1, meta2);
    }

    #[test]
    fn test_call_site_metadata_ne() {
        let meta1 = CallSiteMetadata {
            confidence: 0.9,
            detection_method: DetectionMethod::ASTAnalysis,
            detector_hint: None,
            extras: CallSiteExtras::None,
        };
        let meta2 = CallSiteMetadata {
            confidence: 0.8,
            detection_method: DetectionMethod::ASTAnalysis,
            detector_hint: None,
            extras: CallSiteExtras::None,
        };
        assert_ne!(meta1, meta2);
    }

    // CallSiteExtras variants tests
    #[test]
    fn test_call_site_extras_none() {
        let extras = CallSiteExtras::None;
        assert_eq!(extras, CallSiteExtras::None);
    }

    #[test]
    fn test_call_site_extras_cpp() {
        let extras = CallSiteExtras::Cpp {
            argument_count: 3,
            is_template_instantiation: true,
        };
        if let CallSiteExtras::Cpp {
            argument_count,
            is_template_instantiation,
        } = extras
        {
            assert_eq!(argument_count, 3);
            assert!(is_template_instantiation);
        } else {
            panic!("Expected Cpp variant");
        }
    }

    #[test]
    fn test_call_site_extras_javascript() {
        let extras = CallSiteExtras::JavaScript {
            is_async: true,
            uses_await: true,
            http_method: Some("POST".to_string()),
            http_endpoint: Some("/api/users".to_string()),
        };
        if let CallSiteExtras::JavaScript {
            is_async,
            uses_await,
            http_method,
            http_endpoint,
        } = extras
        {
            assert!(is_async);
            assert!(uses_await);
            assert_eq!(http_method, Some("POST".to_string()));
            assert_eq!(http_endpoint, Some("/api/users".to_string()));
        } else {
            panic!("Expected JavaScript variant");
        }
    }

    #[test]
    fn test_call_site_extras_python() {
        let extras = CallSiteExtras::Python {
            is_async: true,
            is_method: true,
            uses_decorator: true,
        };
        if let CallSiteExtras::Python {
            is_async,
            is_method,
            uses_decorator,
        } = extras
        {
            assert!(is_async);
            assert!(is_method);
            assert!(uses_decorator);
        } else {
            panic!("Expected Python variant");
        }
    }

    #[test]
    fn test_call_site_extras_rust() {
        let extras = CallSiteExtras::Rust {
            is_async: true,
            uses_await: true,
            is_unsafe: false,
            is_method: true,
            is_unsafe_block: false,
        };
        if let CallSiteExtras::Rust {
            is_async,
            uses_await,
            is_unsafe,
            is_method,
            is_unsafe_block,
        } = extras
        {
            assert!(is_async);
            assert!(uses_await);
            assert!(!is_unsafe);
            assert!(is_method);
            assert!(!is_unsafe_block);
        } else {
            panic!("Expected Rust variant");
        }
    }

    #[test]
    fn test_call_site_extras_java() {
        let extras = CallSiteExtras::Java { is_static: true };
        if let CallSiteExtras::Java { is_static } = extras {
            assert!(is_static);
        } else {
            panic!("Expected Java variant");
        }
    }

    #[test]
    fn test_call_site_extras_go() {
        let extras = CallSiteExtras::Go {
            is_goroutine: true,
            is_deferred: false,
            is_builtin: false,
            is_method: true,
        };
        if let CallSiteExtras::Go {
            is_goroutine,
            is_deferred,
            is_builtin,
            is_method,
        } = extras
        {
            assert!(is_goroutine);
            assert!(!is_deferred);
            assert!(!is_builtin);
            assert!(is_method);
        } else {
            panic!("Expected Go variant");
        }
    }

    #[test]
    fn test_call_site_extras_c() {
        let extras = CallSiteExtras::C {
            is_function_pointer: true,
        };
        if let CallSiteExtras::C {
            is_function_pointer,
        } = extras
        {
            assert!(is_function_pointer);
        } else {
            panic!("Expected C variant");
        }
    }

    #[test]
    fn test_call_site_extras_csharp() {
        let extras = CallSiteExtras::CSharp {
            is_async: true,
            uses_await: true,
            is_property_access: false,
        };
        if let CallSiteExtras::CSharp {
            is_async,
            uses_await,
            is_property_access,
        } = extras
        {
            assert!(is_async);
            assert!(uses_await);
            assert!(!is_property_access);
        } else {
            panic!("Expected CSharp variant");
        }
    }

    #[test]
    fn test_call_site_extras_lua() {
        let extras = CallSiteExtras::Lua { is_method: true };
        if let CallSiteExtras::Lua { is_method } = extras {
            assert!(is_method);
        } else {
            panic!("Expected Lua variant");
        }
    }

    #[test]
    fn test_call_site_extras_perl() {
        let extras = CallSiteExtras::Perl { is_method: true };
        if let CallSiteExtras::Perl { is_method } = extras {
            assert!(is_method);
        } else {
            panic!("Expected Perl variant");
        }
    }

    #[test]
    fn test_call_site_extras_shell() {
        let extras = CallSiteExtras::Shell { is_builtin: true };
        if let CallSiteExtras::Shell { is_builtin } = extras {
            assert!(is_builtin);
        } else {
            panic!("Expected Shell variant");
        }
    }

    #[test]
    fn test_call_site_extras_groovy() {
        let extras = CallSiteExtras::Groovy { is_closure: true };
        if let CallSiteExtras::Groovy { is_closure } = extras {
            assert!(is_closure);
        } else {
            panic!("Expected Groovy variant");
        }
    }

    #[test]
    fn test_call_site_extras_clone() {
        let extras = CallSiteExtras::Rust {
            is_async: true,
            uses_await: true,
            is_unsafe: false,
            is_method: true,
            is_unsafe_block: false,
        };
        let cloned = extras.clone();
        assert_eq!(extras, cloned);
    }

    // Integration test with different detection methods
    #[test]
    fn test_call_site_with_heuristic_detection() {
        let mut edge = make_test_edge();
        edge.metadata.detection_method = DetectionMethod::Heuristic;
        edge.metadata.confidence = 0.7;

        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);
        assert_eq!(
            call_site.metadata.detection_method,
            DetectionMethod::Heuristic
        );
        assert!((call_site.metadata.confidence - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_call_site_with_type_inference_detection() {
        let mut edge = make_test_edge();
        edge.metadata.detection_method = DetectionMethod::TypeInference;

        let call_site = CallSite::from_edge(&edge, CallSiteExtras::None);
        assert_eq!(
            call_site.metadata.detection_method,
            DetectionMethod::TypeInference
        );
    }

    #[test]
    fn test_call_site_from_edge_with_cpp_extras() {
        let edge = make_test_edge();
        let extras = CallSiteExtras::Cpp {
            argument_count: 5,
            is_template_instantiation: false,
        };
        let call_site = CallSite::from_edge(&edge, extras.clone());

        assert_eq!(call_site.metadata.extras, extras);
    }
}
