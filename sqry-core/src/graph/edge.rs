//! Edge types for the unified code graph
//!
//! This module defines edges representing relationships between code entities:
//! calls, imports, exports, inheritance, HTTP requests, FFI calls, etc.

use super::node::{NodeId, Span};
use crate::relations::CallIdentityMetadata;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for an edge
///
/// Uses atomic counter for globally unique edge IDs across the codebase.
/// The newtype pattern prevents accidentally mixing edge IDs with other numeric types.
///
/// # Examples
///
/// ```
/// use sqry_core::graph::edge::EdgeId;
///
/// let edge1 = EdgeId::new();
/// let edge2 = EdgeId::new();
/// assert_ne!(edge1, edge2); // Each edge gets a unique ID
/// ```
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct EdgeId(u64);

impl EdgeId {
    /// Create a new edge ID with a globally unique value
    ///
    /// Uses an atomic counter to ensure thread-safe unique ID generation.
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    /// Get the raw u64 value of this edge ID
    ///
    /// This should only be used for serialization or interop with external systems.
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    /// Create an `EdgeId` from a raw u64 value
    ///
    /// # Safety
    ///
    /// This should only be used when deserializing or reconstructing IDs from
    /// external systems. Using this incorrectly could create duplicate IDs.
    #[must_use]
    pub const fn from_u64(id: u64) -> Self {
        Self(id)
    }
}

impl Default for EdgeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "edge#{}", self.0)
    }
}

/// Type of relationship between nodes
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeKind {
    /// Function call
    Call {
        /// Number of arguments passed
        argument_count: usize,
        /// Whether the call is async/await
        is_async: bool,
    },
    /// Module import
    Import {
        /// Import alias (e.g., "import foo as bar")
        alias: Option<String>,
        /// Whether this is a wildcard import (e.g., "import *")
        is_wildcard: bool,
    },
    /// Class inheritance
    Inherits,
    /// Interface implementation
    Implements,
    /// HTTP request (cross-language)
    HTTPRequest {
        /// HTTP method (GET, POST, etc.)
        method: String,
        /// HTTP endpoint path
        endpoint: String,
    },
    /// Foreign Function Interface call (cross-language)
    FFICall {
        /// Type of FFI mechanism used
        ffi_type: FFIType,
    },
    /// Field access
    FieldAccess {
        /// Name of the accessed field
        field_name: String,
    },
    /// Database table read operation (SQL)
    TableRead {
        /// Name of the table being read
        table_name: String,
        /// Optional schema/database name
        schema: Option<String>,
    },
    /// Database table write operation (SQL)
    TableWrite {
        /// Name of the table being written
        table_name: String,
        /// Optional schema/database name
        schema: Option<String>,
        /// Type of write operation (INSERT, UPDATE, DELETE)
        operation: TableWriteOp,
    },
    /// Database trigger relationship (SQL)
    TriggeredBy {
        /// Name of the trigger
        trigger_name: String,
        /// Optional schema/database name
        schema: Option<String>,
    },
    /// Flutter `MethodChannel` invocation (Dart)
    ChannelInvoke {
        /// Name of the platform channel
        channel_name: String,
        /// Method being invoked on the channel
        method: String,
    },
    /// Flutter widget parent-child relationship (Dart)
    WidgetChild {
        /// Type of the child widget
        widget_type: String,
    },
    /// Module export
    ///
    /// Represents an export statement that makes symbols available to other modules.
    /// Supports all major export patterns including named, default, namespace, and
    /// wildcard re-exports.
    ///
    /// # Per-Kind Field Semantics
    ///
    /// | ExportKind | symbol | alias | from_module | Example |
    /// |------------|--------|-------|-------------|---------|
    /// | Named | Some("foo") | None | None | `export { foo }` |
    /// | Named | Some("foo") | Some("bar") | None | `export { foo as bar }` |
    /// | Named | Some("foo") | None | Some("./mod") | `export { foo } from './mod'` |
    /// | NamedTypeOnly | Some("Foo") | None | None | `export type { Foo }` |
    /// | Default | Some("MyClass") | None | None | `export default MyClass` |
    /// | Default | Some("default") | None | None | `export default function() {}` |
    /// | Namespace | Some("ns") | None | Some("./mod") | `export * as ns from './mod'` |
    /// | NamespaceTypeOnly | Some("Types") | None | Some("./types") | `export type * as Types from './types'` |
    /// | AllFromModule | None | None | Some("./mod") | `export * from './mod'` |
    /// | AllFromModuleTypeOnly | None | None | Some("./types") | `export type * from './types'` |
    /// | Assignment | Some("Foo") | None | None | `export = Foo` |
    /// | GlobalNamespace | Some("MyLib") | None | None | `export as namespace MyLib` |
    Export {
        /// Export kind discriminator (determines how other fields are interpreted)
        kind: ExportKind,
        /// Node being exported (optional - None for AllFromModule/AllFromModuleTypeOnly)
        ///
        /// - Named/NamedTypeOnly: the exported symbol name (required)
        /// - Default: name of exported item, or "default" for anonymous (required)
        /// - Namespace/NamespaceTypeOnly: the namespace binding name (required)
        /// - AllFromModule/AllFromModuleTypeOnly: None (wildcard, no specific symbol)
        /// - Assignment/GlobalNamespace: the exported binding name (required)
        symbol: Option<String>,
        /// Optional alias for re-exports (`export { foo as bar }`)
        ///
        /// First-class field (not metadata) for type safety.
        alias: Option<String>,
        /// Optional source module for re-exports (`export { foo } from './bar'`)
        ///
        /// First-class field (not metadata) for type safety.
        from_module: Option<String>,
    },
}

/// Type of table write operation
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TableWriteOp {
    /// INSERT statement
    Insert,
    /// UPDATE statement
    Update,
    /// DELETE statement
    Delete,
    /// MERGE/UPSERT statement
    Merge,
}

/// Discriminator for different export semantics
///
/// This enum distinguishes between various export patterns found across languages,
/// enabling precise semantic representation of module exports.
///
/// # Examples
///
/// ```
/// use sqry_core::graph::edge::ExportKind;
///
/// // Named export: `export { foo }`
/// let named = ExportKind::Named;
///
/// // Default export: `export default MyClass`
/// let default = ExportKind::Default;
///
/// // Wildcard re-export: `export * from './mod'`
/// let all = ExportKind::AllFromModule;
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportKind {
    /// Named export: `export { foo }` or `export { foo as bar }`
    ///
    /// - symbol = "foo" (required)
    /// - alias = Some("bar") if renamed
    Named,
    /// Named type-only export: `export type { Foo }` (TypeScript)
    ///
    /// Type-only semantics inherent in variant (no metadata needed).
    /// - symbol = "Foo" (required)
    /// - alias = Some("Bar") if renamed
    NamedTypeOnly,
    /// Default export: `export default foo`
    ///
    /// - symbol = name of exported item, or "default" for anonymous
    Default,
    /// Namespace re-export: `export * as ns from './mod'`
    ///
    /// - symbol = Some("ns") (the namespace name, required)
    /// - `from_module` = "./mod"
    Namespace,
    /// Type-only namespace re-export: `export type * as Types from './types'` (TypeScript)
    ///
    /// - symbol = Some("Types") (required)
    /// - `from_module` = "./types"
    NamespaceTypeOnly,
    /// Wildcard re-export: `export * from './mod'`
    ///
    /// - symbol = None (NO sentinel string)
    /// - `from_module` = "./mod"
    AllFromModule,
    /// Type-only wildcard re-export: `export type * from './types'` (TypeScript 5.0+)
    ///
    /// - symbol = None (NO sentinel string)
    /// - `from_module` = "./types"
    AllFromModuleTypeOnly,
    /// TypeScript assignment export: `export = Foo` (UMD/CJS interop)
    ///
    /// - symbol = "Foo" (the exported binding, required)
    Assignment,
    /// TypeScript global namespace augmentation: `export as namespace Foo`
    ///
    /// - symbol = "Foo" (the global namespace name, required)
    GlobalNamespace,
}

impl fmt::Display for ExportKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExportKind::Named => write!(f, "named"),
            ExportKind::NamedTypeOnly => write!(f, "named-type-only"),
            ExportKind::Default => write!(f, "default"),
            ExportKind::Namespace => write!(f, "namespace"),
            ExportKind::NamespaceTypeOnly => write!(f, "namespace-type-only"),
            ExportKind::AllFromModule => write!(f, "all-from-module"),
            ExportKind::AllFromModuleTypeOnly => write!(f, "all-from-module-type-only"),
            ExportKind::Assignment => write!(f, "assignment"),
            ExportKind::GlobalNamespace => write!(f, "global-namespace"),
        }
    }
}

/// Type of FFI mechanism
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FFIType {
    /// JavaScript node-ffi
    NodeFFI,
    /// Python ctypes
    Ctypes,
    /// Python cffi
    CFFI,
    /// Rust extern "C"
    RustExtern,
    /// JNI (Java Native Interface)
    JNI,
    /// Elixir NIFs or Erlang interop (:`erlang.module()`)
    ElixirNIF,
    /// R .`Call()` or .`External()` interface
    RDotCall,
    /// R Rcpp (C++ interface for R)
    Rcpp,
    /// Other/unknown FFI
    Other(String),
}

impl fmt::Display for FFIType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FFIType::NodeFFI => write!(f, "node-ffi"),
            FFIType::Ctypes => write!(f, "ctypes"),
            FFIType::CFFI => write!(f, "cffi"),
            FFIType::RustExtern => write!(f, "extern-C"),
            FFIType::JNI => write!(f, "JNI"),
            FFIType::ElixirNIF => write!(f, "elixir-nif"),
            FFIType::RDotCall => write!(f, "r-dotcall"),
            FFIType::Rcpp => write!(f, "rcpp"),
            FFIType::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Detection strategy for an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetectionMethod {
    /// Derived directly from AST analysis (tree-sitter queries / traversal).
    ASTAnalysis,
    /// Derived from type inference or static type information.
    TypeInference,
    /// Derived using heuristic detection (e.g., pattern matching on strings).
    Heuristic,
    /// Added manually by a user or external annotation.
    Manual,
    /// Strategy not recorded / unknown.
    #[default]
    Unknown,
}

/// Metadata for an edge
#[derive(Debug, Clone)]
pub struct EdgeMetadata {
    /// Optional source span for the edge (call site, import location, etc.)
    pub span: Option<Span>,
    /// Confidence score for detected edges (0.0 to 1.0).
    /// 1.0 = certain (static analysis)
    /// 0.7-0.9 = high confidence (template literals)
    /// 0.5-0.7 = medium confidence (variable endpoint)
    /// <0.5 = low confidence (skip edge creation)
    pub confidence: f32,
    /// How this edge was detected.
    pub detection_method: DetectionMethod,
    /// Human-readable reason for edge detection.
    pub reason: Option<String>,
    /// Caller identity metadata (populated by language-specific `GraphBuilders`).
    /// Contains qualified name, simple name, namespace, and method kind.
    pub caller_identity: Option<CallIdentityMetadata>,
    /// Callee identity metadata (populated by language-specific `GraphBuilders`).
    /// Contains qualified name, simple name, namespace, and method kind.
    pub callee_identity: Option<CallIdentityMetadata>,
}

impl Default for EdgeMetadata {
    fn default() -> Self {
        Self {
            span: None,
            confidence: 1.0,
            detection_method: DetectionMethod::Unknown,
            reason: None,
            caller_identity: None,
            callee_identity: None,
        }
    }
}

/// An edge in the code graph representing a relationship
#[derive(Debug, Clone)]
pub struct CodeEdge {
    /// Unique identifier
    pub id: EdgeId,
    /// Source node
    pub from: NodeId,
    /// Target node
    pub to: NodeId,
    /// Edge type
    pub kind: EdgeKind,
    /// Additional metadata
    pub metadata: EdgeMetadata,
}

impl CodeEdge {
    /// Create a new code edge
    #[must_use]
    pub fn new(from: NodeId, to: NodeId, kind: EdgeKind) -> Self {
        Self {
            id: EdgeId::new(),
            from,
            to,
            kind,
            metadata: EdgeMetadata::default(),
        }
    }

    /// Create a new code edge with metadata
    #[must_use]
    pub fn with_metadata(from: NodeId, to: NodeId, kind: EdgeKind, metadata: EdgeMetadata) -> Self {
        Self {
            id: EdgeId::new(),
            from,
            to,
            kind,
            metadata,
        }
    }

    /// Check if this is a cross-language edge
    ///
    /// An edge is considered cross-language if:
    /// - The source and target nodes are in different languages, OR
    /// - The edge represents an HTTP request (service boundary), OR
    /// - The edge represents an FFI call (language interop)
    #[must_use]
    pub fn is_cross_language(&self) -> bool {
        // Different languages
        if self.from.language != self.to.language {
            return true;
        }

        // HTTP requests are always cross-language (service boundaries)
        if matches!(self.kind, EdgeKind::HTTPRequest { .. }) {
            return true;
        }

        // FFI calls are always cross-language (language interop)
        if matches!(self.kind, EdgeKind::FFICall { .. }) {
            return true;
        }

        false
    }

    /// Get HTTP method if this is an HTTP request
    #[must_use]
    pub fn http_method(&self) -> Option<&str> {
        match &self.kind {
            EdgeKind::HTTPRequest { method, .. } => Some(method),
            _ => None,
        }
    }

    /// Get HTTP endpoint if this is an HTTP request
    #[must_use]
    pub fn http_endpoint(&self) -> Option<&str> {
        match &self.kind {
            EdgeKind::HTTPRequest { endpoint, .. } => Some(endpoint),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use approx::assert_abs_diff_eq;

    #[test]
    fn test_edge_id_unique() {
        let id1 = EdgeId::new();
        let id2 = EdgeId::new();
        let id3 = EdgeId::new();

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_edge_creation() {
        let from = NodeId::new(Language::JavaScript, "api.js", "fetchUsers");
        let to = NodeId::new(Language::Python, "api.py", "get_users");

        let edge = CodeEdge::new(
            from.clone(),
            to.clone(),
            EdgeKind::HTTPRequest {
                method: "GET".to_string(),
                endpoint: "/api/users".to_string(),
            },
        );

        assert_eq!(edge.from, from);
        assert_eq!(edge.to, to);
        assert!(edge.is_cross_language());
    }

    #[test]
    fn test_cross_language_detection() {
        let js_node = NodeId::new(Language::JavaScript, "api.js", "fetch");
        let py_node = NodeId::new(Language::Python, "api.py", "handler");
        let js_node2 = NodeId::new(Language::JavaScript, "utils.js", "helper");

        let cross_language = CodeEdge::new(
            js_node.clone(),
            py_node,
            EdgeKind::HTTPRequest {
                method: "POST".to_string(),
                endpoint: "/api/data".to_string(),
            },
        );

        let same_lang = CodeEdge::new(
            js_node,
            js_node2,
            EdgeKind::Call {
                argument_count: 2,
                is_async: true,
            },
        );

        assert!(cross_language.is_cross_language());
        assert!(!same_lang.is_cross_language());
    }

    #[test]
    fn test_http_requests_are_cross_language() {
        // HTTP requests should be cross-language even within same language
        // because they represent service boundaries
        let from = NodeId::new(Language::JavaScript, "api.js", "fetchUsers");
        let to = NodeId::new(Language::JavaScript, "api.js", "httpGet");

        let http_edge = CodeEdge::new(
            from.clone(),
            to.clone(),
            EdgeKind::HTTPRequest {
                method: "GET".to_string(),
                endpoint: "/api/users".to_string(),
            },
        );

        // HTTP request should be cross-language
        assert!(http_edge.is_cross_language());

        // Regular call between same nodes should NOT be cross-language
        let call_edge = CodeEdge::new(
            from,
            to,
            EdgeKind::Call {
                argument_count: 1,
                is_async: true,
            },
        );
        assert!(!call_edge.is_cross_language());
    }

    #[test]
    fn test_ffi_calls_are_cross_language() {
        // FFI calls should be cross-language even within same language
        // because they represent language interop boundaries
        let from = NodeId::new(Language::Python, "api.py", "authenticate");
        let to = NodeId::new(Language::Python, "api.py", "validate_token");

        let ffi_edge = CodeEdge::new(
            from.clone(),
            to.clone(),
            EdgeKind::FFICall {
                ffi_type: FFIType::Ctypes,
            },
        );

        // FFI call should be cross-language
        assert!(ffi_edge.is_cross_language());

        // Regular call between same nodes should NOT be cross-language
        let call_edge = CodeEdge::new(
            from,
            to,
            EdgeKind::Call {
                argument_count: 1,
                is_async: false,
            },
        );
        assert!(!call_edge.is_cross_language());
    }

    #[test]
    fn test_http_helpers() {
        let from = NodeId::new(Language::JavaScript, "api.js", "fetch");
        let to = NodeId::new(Language::Http, "api", "/users");

        let edge = CodeEdge::new(
            from,
            to,
            EdgeKind::HTTPRequest {
                method: "GET".to_string(),
                endpoint: "/api/users".to_string(),
            },
        );

        assert_eq!(edge.http_method(), Some("GET"));
        assert_eq!(edge.http_endpoint(), Some("/api/users"));
    }

    #[test]
    fn test_edge_metadata() {
        let from = NodeId::new(Language::JavaScript, "api.js", "fetch");
        let to = NodeId::new(Language::Http, "api", "/users");

        let metadata = EdgeMetadata {
            span: None,
            confidence: 0.8,
            detection_method: DetectionMethod::Heuristic,
            reason: Some("Template literal with interpolation".to_string()),
            ..Default::default()
        };

        let edge = CodeEdge::with_metadata(
            from,
            to,
            EdgeKind::HTTPRequest {
                method: "GET".to_string(),
                endpoint: "/api/users/${id}".to_string(),
            },
            metadata,
        );

        assert_abs_diff_eq!(edge.metadata.confidence, 0.8, epsilon = 1e-10);
        assert!(edge.metadata.reason.is_some());
        assert!(edge.metadata.span.is_none());
    }

    #[test]
    fn test_ffi_type_display() {
        assert_eq!(FFIType::NodeFFI.to_string(), "node-ffi");
        assert_eq!(FFIType::Ctypes.to_string(), "ctypes");
        assert_eq!(FFIType::RustExtern.to_string(), "extern-C");
        assert_eq!(FFIType::Other("custom".to_string()).to_string(), "custom");
    }
}
