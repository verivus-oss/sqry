//! `EdgeKind` enumeration for the unified graph architecture.
//!
//! This module defines `EdgeKind`, which categorizes all relationship types
//! that can be represented as edges in the graph.
//!
//! # Design (FR-42, Appendix A2)
//!
//! The enumeration covers:
//! - **Structural**: Defines, Contains
//! - **References**: Calls, References, Imports, Exports, `TypeOf`
//! - **OOP**: Inherits, Implements
//! - **Cross-language**: FFI, HTTP, gRPC, WebAssembly, DB queries
//! - **Extended**: `MessageQueue`, WebSocket, GraphQL, `ProcessExec`, `FileIpc`

use std::fmt;

use serde::{Deserialize, Serialize};

use super::super::string::StringId;

/// Context for `TypeOf` edges (parameter, return, field, variable).
///
/// Indicates where a type reference appears in the code structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeOfContext {
    /// Function/method parameter
    Parameter,
    /// Function/method return value
    Return,
    /// Struct/class field
    Field,
    /// Variable declaration
    Variable,
    /// Type parameter (generics)
    TypeParameter,
    /// Type constraint
    Constraint,
}

/// FFI calling convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfiConvention {
    /// Standard C calling convention
    C,
    /// cdecl calling convention
    Cdecl,
    /// stdcall calling convention (Windows)
    Stdcall,
    /// fastcall calling convention
    Fastcall,
    /// System default calling convention
    System,
}

impl Default for FfiConvention {
    fn default() -> Self {
        Self::C
    }
}

/// HTTP method for HTTP request edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET request
    Get,
    /// POST request
    Post,
    /// PUT request
    Put,
    /// DELETE request
    Delete,
    /// PATCH request
    Patch,
    /// HEAD request
    Head,
    /// OPTIONS request
    Options,
    /// ALL methods (wildcard — matches any HTTP method)
    All,
}

impl Default for HttpMethod {
    fn default() -> Self {
        Self::Get
    }
}

impl HttpMethod {
    /// Returns the HTTP method as a string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::All => "ALL",
        }
    }
}

/// Database query type for DB query edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbQueryType {
    /// SELECT query
    Select,
    /// INSERT query
    Insert,
    /// UPDATE query
    Update,
    /// DELETE query
    Delete,
    /// EXECUTE stored procedure/function
    Execute,
}

impl Default for DbQueryType {
    fn default() -> Self {
        Self::Select
    }
}

/// Database table write operation (SQL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableWriteOp {
    /// INSERT operation
    Insert,
    /// UPDATE operation
    Update,
    /// DELETE operation
    Delete,
}

/// Export kind for distinguishing re-exports from declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    /// Direct export of a symbol
    Direct,
    /// Re-export from another module
    Reexport,
    /// Default export (JavaScript/TypeScript)
    Default,
    /// Namespace export (export *)
    Namespace,
}

impl Default for ExportKind {
    fn default() -> Self {
        Self::Direct
    }
}

/// Message queue protocol for async communication edges.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MqProtocol {
    /// Apache Kafka
    Kafka,
    /// AWS SQS
    Sqs,
    /// `RabbitMQ` / AMQP
    RabbitMq,
    /// NATS messaging
    Nats,
    /// Redis Pub/Sub
    Redis,
    /// Other protocol (identified by `StringId`)
    Other(StringId),
}

impl Default for MqProtocol {
    fn default() -> Self {
        Self::Kafka
    }
}

/// Kind of lifetime constraint relationship (Rust-specific).
///
/// Models the various ways lifetimes can constrain other lifetimes or types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeConstraintKind {
    /// `'a: 'b` - lifetime 'a outlives 'b
    Outlives,
    /// `T: 'a` - type T is bounded by lifetime 'a
    TypeBound,
    /// `&'a T` - reference with explicit lifetime
    Reference,
    /// `'static` bound
    Static,
    /// Higher-ranked trait bound: `for<'a> T: Trait<'a>`
    HigherRanked,
    /// Trait object bound: `dyn Trait + 'a`
    TraitObject,
    /// impl Trait bound: `impl Trait + 'a`
    ImplTrait,
    /// Elided lifetime (inferred by compiler, requires RA)
    Elided,
}

impl Default for LifetimeConstraintKind {
    fn default() -> Self {
        Self::Outlives
    }
}

/// Kind of macro expansion (Rust-specific).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroExpansionKind {
    /// Derive macro (`#[derive(...)]`)
    Derive,
    /// Attribute macro (`#[proc_macro]`)
    Attribute,
    /// Declarative macro (`macro_rules!`)
    Declarative,
    /// Function-like macro
    Function,
}

impl Default for MacroExpansionKind {
    fn default() -> Self {
        Self::Declarative
    }
}

/// Enumeration of edge relationship types in the graph.
///
/// Each variant represents a distinct kind of relationship between nodes.
/// The categorization is language-agnostic to support cross-language analysis.
///
/// Note: Uses default externally-tagged enum representation for serialization compatibility.
/// JSON output will be `{"calls": {"argument_count": 0, "is_async": false}}` rather than
/// `{"type": "calls", "argument_count": 0, "is_async": false}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    // ==================== Structural ====================
    /// A symbol defines another (e.g., module defines function).
    Defines,

    /// A container contains another (e.g., class contains method).
    Contains,

    // ==================== References ====================
    /// A function/method calls another.
    Calls {
        /// Number of arguments in the call (0-255)
        argument_count: u8,
        /// Whether the call expression is directly awaited (uses `.await`).
        ///
        /// This indicates an *awaited call site*, not merely "inside an async function".
        is_async: bool,
    },

    /// A symbol references another (read access).
    References,

    /// An import statement brings in a symbol.
    Imports {
        /// Optional alias for the import (e.g., `import { foo as bar }`)
        alias: Option<StringId>,
        /// Whether this is a wildcard import (e.g., `import *`)
        is_wildcard: bool,
    },

    /// An export statement exposes a symbol.
    Exports {
        /// The kind of export (direct, re-export, default, namespace)
        kind: ExportKind,
        /// Optional alias for the export (e.g., `export { foo as bar }`)
        alias: Option<StringId>,
    },

    /// A type reference with optional context metadata.
    ///
    /// Represents relationships like:
    /// - Function parameter types (`context = Parameter`)
    /// - Function return types (`context = Return`)
    /// - Variable types (`context = Variable`)
    /// - Struct field types (`context = Field`)
    ///
    /// The `context`, `index`, and `name` fields provide semantic information
    /// about where and how the type reference appears.
    TypeOf {
        /// Context where this type reference appears
        context: Option<TypeOfContext>,
        /// Position/index (for parameters, returns, fields)
        index: Option<u16>,
        /// Name (for parameters, returns, fields, variables)
        name: Option<StringId>,
    },

    // ==================== OOP ====================
    /// A class inherits from another (extends).
    Inherits,

    /// A class/struct implements an interface/trait.
    Implements,

    // ==================== Rust-Specific ====================
    /// Lifetime constraint relationship.
    ///
    /// Models Rust lifetime bounds. Source and target are `NodeKind::Lifetime`
    /// or `NodeKind::Type` nodes (for type bounds like `T: 'a`).
    ///
    /// Query semantics: NOT included in `callers/callees` results.
    /// Use dedicated query: `--lifetime-constraints`
    LifetimeConstraint {
        /// The kind of lifetime constraint
        constraint_kind: LifetimeConstraintKind,
    },

    /// Trait method binding (call site -> impl method).
    ///
    /// Represents the resolution of a trait method call to a concrete
    /// implementation. This is distinct from `Calls` because it involves
    /// trait method resolution logic.
    ///
    /// Query semantics: NOT included in `callers` by default.
    /// Use `--include-trait-bindings` flag or dedicated query.
    TraitMethodBinding {
        /// The trait providing the method
        trait_name: StringId,
        /// The implementing type
        impl_type: StringId,
        /// Whether this binding is ambiguous (multiple impls match)
        is_ambiguous: bool,
    },

    /// Macro expansion relationship.
    ///
    /// Represents the expansion of a macro invocation to its generated code.
    /// Only available when macro expansion is enabled (security opt-in).
    ///
    /// Query semantics: Included in `callees` when expansion enabled.
    MacroExpansion {
        /// The kind of macro expansion
        expansion_kind: MacroExpansionKind,
        /// Whether the expansion has been verified against source
        is_verified: bool,
    },

    // ==================== Cross-language / Cross-service ====================
    /// Foreign function interface call.
    FfiCall {
        /// The calling convention used
        convention: FfiConvention,
    },

    /// HTTP request to an endpoint.
    HttpRequest {
        /// The HTTP method
        method: HttpMethod,
        /// Optional URL pattern
        url: Option<StringId>,
    },

    /// gRPC service call.
    GrpcCall {
        /// Service name
        service: StringId,
        /// Method name
        method: StringId,
    },

    /// WebAssembly function call.
    WebAssemblyCall,

    /// Database query execution.
    DbQuery {
        /// Query type
        query_type: DbQueryType,
        /// Optional table/collection name
        table: Option<StringId>,
    },

    /// Database table read operation (SQL).
    TableRead {
        /// Name of the table being read
        table_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
    },

    /// Database table write operation (SQL).
    TableWrite {
        /// Name of the table being written
        table_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
        /// Type of write operation (INSERT/UPDATE/DELETE)
        operation: TableWriteOp,
    },

    /// Database trigger relationship (SQL).
    TriggeredBy {
        /// Name of the trigger
        trigger_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
    },

    // ==================== Extended (FR-42) ====================
    /// Message queue publish/subscribe.
    MessageQueue {
        /// Protocol used
        protocol: MqProtocol,
        /// Optional topic/queue name
        topic: Option<StringId>,
    },

    /// WebSocket event communication.
    WebSocket {
        /// Optional event name
        event: Option<StringId>,
    },

    /// GraphQL operation (query/mutation/subscription).
    GraphQLOperation {
        /// Operation name
        operation: StringId,
    },

    /// Process execution (spawn, exec).
    ProcessExec {
        /// Command being executed
        command: StringId,
    },

    /// File-based IPC (pipes, shared memory, temp files).
    FileIpc {
        /// Optional path pattern
        path_pattern: Option<StringId>,
    },

    // ==================== Extensibility ====================
    /// Generic protocol call for extensibility.
    ProtocolCall {
        /// Protocol identifier
        protocol: StringId,
        /// Optional JSON-encoded metadata
        metadata: Option<StringId>,
    },
}

impl EdgeKind {
    /// Returns `true` if this edge represents a function call relationship.
    #[inline]
    #[must_use]
    pub const fn is_call(&self) -> bool {
        matches!(
            self,
            Self::Calls { .. }
                | Self::FfiCall { .. }
                | Self::HttpRequest { .. }
                | Self::GrpcCall { .. }
                | Self::WebAssemblyCall
        )
    }

    /// Returns `true` if this edge represents a structural relationship.
    #[inline]
    #[must_use]
    pub const fn is_structural(&self) -> bool {
        matches!(self, Self::Defines | Self::Contains)
    }

    /// Returns `true` if this edge represents a type relationship.
    #[inline]
    #[must_use]
    pub const fn is_type_relation(&self) -> bool {
        matches!(
            self,
            Self::Inherits | Self::Implements | Self::TypeOf { .. }
        )
    }

    /// Returns `true` if this edge represents a cross-language/service boundary.
    #[inline]
    #[must_use]
    pub const fn is_cross_boundary(&self) -> bool {
        matches!(
            self,
            Self::FfiCall { .. }
                | Self::HttpRequest { .. }
                | Self::GrpcCall { .. }
                | Self::WebAssemblyCall
                | Self::DbQuery { .. }
                | Self::TableRead { .. }
                | Self::TableWrite { .. }
                | Self::TriggeredBy { .. }
                | Self::MessageQueue { .. }
                | Self::WebSocket { .. }
                | Self::GraphQLOperation { .. }
                | Self::ProcessExec { .. }
                | Self::FileIpc { .. }
                | Self::ProtocolCall { .. }
        )
    }

    /// Returns `true` if this is an async/message-based relationship.
    #[inline]
    #[must_use]
    pub const fn is_async(&self) -> bool {
        matches!(
            self,
            Self::MessageQueue { .. } | Self::WebSocket { .. } | Self::GraphQLOperation { .. }
        )
    }

    /// Returns `true` if this is a Rust-specific edge kind.
    ///
    /// These edges are produced by the Rust language plugin and have
    /// specialized query semantics.
    #[inline]
    #[must_use]
    pub const fn is_rust_specific(&self) -> bool {
        matches!(
            self,
            Self::LifetimeConstraint { .. }
                | Self::TraitMethodBinding { .. }
                | Self::MacroExpansion { .. }
        )
    }

    /// Returns `true` if this is a lifetime constraint edge.
    #[inline]
    #[must_use]
    pub const fn is_lifetime_constraint(&self) -> bool {
        matches!(self, Self::LifetimeConstraint { .. })
    }

    /// Returns `true` if this is a trait method binding edge.
    #[inline]
    #[must_use]
    pub const fn is_trait_method_binding(&self) -> bool {
        matches!(self, Self::TraitMethodBinding { .. })
    }

    /// Returns `true` if this is a macro expansion edge.
    #[inline]
    #[must_use]
    pub const fn is_macro_expansion(&self) -> bool {
        matches!(self, Self::MacroExpansion { .. })
    }

    /// Returns the canonical tag name for this edge kind.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Defines => "defines",
            Self::Contains => "contains",
            Self::Calls { .. } => "calls",
            Self::References => "references",
            Self::Imports { .. } => "imports",
            Self::Exports { .. } => "exports",
            Self::TypeOf { .. } => "type_of",
            Self::Inherits => "inherits",
            Self::Implements => "implements",
            Self::LifetimeConstraint { .. } => "lifetime_constraint",
            Self::TraitMethodBinding { .. } => "trait_method_binding",
            Self::MacroExpansion { .. } => "macro_expansion",
            Self::FfiCall { .. } => "ffi_call",
            Self::HttpRequest { .. } => "http_request",
            Self::GrpcCall { .. } => "grpc_call",
            Self::WebAssemblyCall => "web_assembly_call",
            Self::DbQuery { .. } => "db_query",
            Self::TableRead { .. } => "table_read",
            Self::TableWrite { .. } => "table_write",
            Self::TriggeredBy { .. } => "triggered_by",
            Self::MessageQueue { .. } => "message_queue",
            Self::WebSocket { .. } => "web_socket",
            Self::GraphQLOperation { .. } => "graphql_operation",
            Self::ProcessExec { .. } => "process_exec",
            Self::FileIpc { .. } => "file_ipc",
            Self::ProtocolCall { .. } => "protocol_call",
        }
    }

    /// Returns an estimated byte size for this edge kind variant.
    ///
    /// Used for byte-level admission control in the delta buffer.
    /// Estimates are conservative approximations based on variant data.
    #[must_use]
    pub const fn estimated_size(&self) -> usize {
        // Base enum discriminant: 1 byte
        // StringId: 4 bytes each
        // Option<StringId>: 5 bytes (1 discriminant + 4 payload)
        // bool: 1 byte, u8: 1 byte, ExportKind: 1 byte
        match self {
            // Unit variants: just discriminant
            Self::Defines
            | Self::Contains
            | Self::References
            | Self::Inherits
            | Self::Implements
            | Self::WebAssemblyCall => 1,

            // u8 + bool: 1 + 1 + 1
            // MacroExpansionKind + bool: 1 + 1
            Self::Calls { .. } | Self::MacroExpansion { .. } => 3,

            // Option<StringId> + bool: 5 + 1 + 1 (imports/exports)
            // DbQueryType + Option<StringId>: 1 + 5
            Self::Imports { .. } | Self::Exports { .. } | Self::DbQuery { .. } => 7,

            // FfiConvention: 1 byte
            // LifetimeConstraintKind: 1 byte
            Self::FfiCall { .. } | Self::LifetimeConstraint { .. } => 2,

            // StringId + StringId + bool: 4 + 4 + 1
            // StringId + Option<StringId>: 4 + 5
            Self::TraitMethodBinding { .. }
            | Self::TableRead { .. }
            | Self::TriggeredBy { .. }
            | Self::ProtocolCall { .. } => 10,

            // HttpMethod + Option<StringId>: 1 + 5
            // Option<StringId>: 5 (websocket/file IPC)
            Self::HttpRequest { .. } | Self::WebSocket { .. } | Self::FileIpc { .. } => 6,

            // Two StringIds: 4 + 4
            Self::GrpcCall { .. } => 9,

            // StringId + Option<StringId> + TableWriteOp: 4 + 5 + 1
            // Option<TypeOfContext> + Option<u16> + Option<StringId>: 2 + 3 + 5
            Self::TableWrite { .. } | Self::MessageQueue { .. } | Self::TypeOf { .. } => 11,

            // StringId: 4
            Self::GraphQLOperation { .. } | Self::ProcessExec { .. } => 5,
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

impl Default for EdgeKind {
    /// Returns `EdgeKind::Calls` as the default (most common edge type).
    fn default() -> Self {
        Self::Calls {
            argument_count: 0,
            is_async: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a default Calls variant for tests.
    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        }
    }

    /// Helper to create a default Imports variant for tests.
    fn imports() -> EdgeKind {
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        }
    }

    /// Helper to create a default Exports variant for tests.
    fn exports() -> EdgeKind {
        EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        }
    }

    #[test]
    fn test_edge_kind_tag() {
        assert_eq!(calls().tag(), "calls");
        assert_eq!(imports().tag(), "imports");
        assert_eq!(exports().tag(), "exports");
        assert_eq!(EdgeKind::Defines.tag(), "defines");
        assert_eq!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None
            }
            .tag(),
            "http_request"
        );
    }

    #[test]
    fn test_edge_kind_display() {
        assert_eq!(format!("{}", calls()), "calls");
        assert_eq!(format!("{}", imports()), "imports");
        assert_eq!(format!("{}", exports()), "exports");
        assert_eq!(format!("{}", EdgeKind::Inherits), "inherits");
    }

    #[test]
    fn test_is_call() {
        assert!(calls().is_call());
        assert!(
            EdgeKind::Calls {
                argument_count: 5,
                is_async: true
            }
            .is_call()
        );
        assert!(
            EdgeKind::FfiCall {
                convention: FfiConvention::C
            }
            .is_call()
        );
        assert!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Post,
                url: None
            }
            .is_call()
        );
        assert!(!EdgeKind::Defines.is_call());
        assert!(!EdgeKind::Inherits.is_call());
        assert!(!imports().is_call());
        assert!(!exports().is_call());
    }

    #[test]
    fn test_is_structural() {
        assert!(EdgeKind::Defines.is_structural());
        assert!(EdgeKind::Contains.is_structural());
        assert!(!calls().is_structural());
        assert!(!imports().is_structural());
        assert!(!exports().is_structural());
    }

    #[test]
    fn test_is_type_relation() {
        assert!(EdgeKind::Inherits.is_type_relation());
        assert!(EdgeKind::Implements.is_type_relation());
        assert!(
            EdgeKind::TypeOf {
                context: None,
                index: None,
                name: None,
            }
            .is_type_relation()
        );
        assert!(!calls().is_type_relation());
    }

    #[test]
    fn test_is_cross_boundary() {
        assert!(
            EdgeKind::FfiCall {
                convention: FfiConvention::C
            }
            .is_cross_boundary()
        );
        assert!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None
            }
            .is_cross_boundary()
        );
        assert!(
            EdgeKind::GrpcCall {
                service: StringId::INVALID,
                method: StringId::INVALID
            }
            .is_cross_boundary()
        );
        assert!(!calls().is_cross_boundary());
        assert!(!imports().is_cross_boundary());
        assert!(!exports().is_cross_boundary());
    }

    #[test]
    fn test_is_async() {
        assert!(
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Kafka,
                topic: None
            }
            .is_async()
        );
        assert!(EdgeKind::WebSocket { event: None }.is_async());
        assert!(!calls().is_async());
        // Note: EdgeKind::Calls with is_async: true still returns false from is_async()
        // because is_async() refers to async communication patterns, not async function calls
        assert!(
            !EdgeKind::Calls {
                argument_count: 0,
                is_async: true
            }
            .is_async()
        );
    }

    #[test]
    fn test_default() {
        assert_eq!(EdgeKind::default(), calls());
        assert_eq!(HttpMethod::default(), HttpMethod::Get);
        assert_eq!(FfiConvention::default(), FfiConvention::C);
        assert_eq!(DbQueryType::default(), DbQueryType::Select);
        assert_eq!(ExportKind::default(), ExportKind::Direct);
    }

    #[test]
    fn test_http_method_as_str() {
        assert_eq!(HttpMethod::Get.as_str(), "GET");
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Delete.as_str(), "DELETE");
        assert_eq!(HttpMethod::All.as_str(), "ALL");
    }

    #[test]
    fn test_calls_with_metadata() {
        let sync_call = EdgeKind::Calls {
            argument_count: 3,
            is_async: false,
        };
        let async_call = EdgeKind::Calls {
            argument_count: 0,
            is_async: true,
        };
        assert_eq!(sync_call.tag(), "calls");
        assert_eq!(async_call.tag(), "calls");
        assert!(sync_call.is_call());
        assert!(async_call.is_call());
        assert_ne!(sync_call, async_call);
    }

    #[test]
    fn test_imports_with_metadata() {
        let simple = imports();
        let aliased = EdgeKind::Imports {
            alias: Some(StringId::new(42)),
            is_wildcard: false,
        };
        let wildcard = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };

        assert_eq!(simple.tag(), "imports");
        assert_eq!(aliased.tag(), "imports");
        assert_eq!(wildcard.tag(), "imports");
        assert_ne!(simple, aliased);
        assert_ne!(simple, wildcard);
    }

    #[test]
    fn test_exports_with_metadata() {
        let direct = exports();
        let reexport = EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: None,
        };
        let default_export = EdgeKind::Exports {
            kind: ExportKind::Default,
            alias: None,
        };
        let namespace = EdgeKind::Exports {
            kind: ExportKind::Namespace,
            alias: Some(StringId::new(1)),
        };

        assert_eq!(direct.tag(), "exports");
        assert_eq!(reexport.tag(), "exports");
        assert_eq!(default_export.tag(), "exports");
        assert_eq!(namespace.tag(), "exports");
        assert_ne!(direct, reexport);
        assert_ne!(direct, default_export);
    }

    #[test]
    fn test_serde_calls_imports_exports() {
        // Calls with metadata
        let calls = EdgeKind::Calls {
            argument_count: 5,
            is_async: true,
        };
        let json = serde_json::to_string(&calls).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(calls, deserialized);
        assert!(json.contains("\"calls\""));
        assert!(json.contains("\"argument_count\":5"));
        assert!(json.contains("\"is_async\":true"));

        // Imports with alias
        let imports = EdgeKind::Imports {
            alias: Some(StringId::new(10)),
            is_wildcard: false,
        };
        let json = serde_json::to_string(&imports).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(imports, deserialized);

        // Exports with kind
        let exports = EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: None,
        };
        let json = serde_json::to_string(&exports).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(exports, deserialized);
    }

    #[test]
    fn test_serde_complex_variants() {
        // HttpRequest with fields
        let http = EdgeKind::HttpRequest {
            method: HttpMethod::Post,
            url: None,
        };
        let json = serde_json::to_string(&http).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(http, deserialized);

        // GrpcCall with StringIds
        let grpc = EdgeKind::GrpcCall {
            service: StringId::new(1),
            method: StringId::new(2),
        };
        let json = serde_json::to_string(&grpc).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(grpc, deserialized);
    }

    #[test]
    fn test_postcard_roundtrip_simple_enums() {
        // Test postcard roundtrip for component enums used by EdgeKind.

        // FfiConvention
        for conv in [
            FfiConvention::C,
            FfiConvention::Cdecl,
            FfiConvention::Stdcall,
        ] {
            let bytes = postcard::to_allocvec(&conv).unwrap();
            let deserialized: FfiConvention = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(conv, deserialized);
        }

        // HttpMethod
        for method in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Delete,
            HttpMethod::All,
        ] {
            let bytes = postcard::to_allocvec(&method).unwrap();
            let deserialized: HttpMethod = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(method, deserialized);
        }

        // DbQueryType
        for query in [
            DbQueryType::Select,
            DbQueryType::Insert,
            DbQueryType::Update,
        ] {
            let bytes = postcard::to_allocvec(&query).unwrap();
            let deserialized: DbQueryType = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(query, deserialized);
        }

        // ExportKind
        for kind in [
            ExportKind::Direct,
            ExportKind::Reexport,
            ExportKind::Default,
            ExportKind::Namespace,
        ] {
            let bytes = postcard::to_allocvec(&kind).unwrap();
            let deserialized: ExportKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(kind, deserialized);
        }
    }

    #[test]
    fn test_edge_kind_json_compatibility() {
        // EdgeKind is designed for JSON serialization (MCP export).
        // Binary persistence in Phase 6 will use a custom format.
        let kinds = [
            calls(),
            imports(),
            exports(),
            EdgeKind::Defines,
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None,
            },
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Kafka,
                topic: Some(StringId::new(1)),
            },
        ];

        for kind in &kinds {
            // JSON roundtrip should work
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);

            // Postcard roundtrip should also work (required for graph persistence)
            let bytes = postcard::to_allocvec(kind).unwrap();
            let from_postcard: EdgeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(*kind, from_postcard);
        }
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(calls());
        set.insert(imports());
        set.insert(exports());
        set.insert(EdgeKind::Defines);
        set.insert(EdgeKind::HttpRequest {
            method: HttpMethod::Get,
            url: None,
        });

        assert!(set.contains(&calls()));
        assert!(set.contains(&imports()));
        assert!(set.contains(&exports()));
        assert!(!set.contains(&EdgeKind::Inherits));
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn test_ffi_convention_variants() {
        let conventions = [
            FfiConvention::C,
            FfiConvention::Cdecl,
            FfiConvention::Stdcall,
            FfiConvention::Fastcall,
            FfiConvention::System,
        ];

        for conv in conventions {
            let edge = EdgeKind::FfiCall { convention: conv };
            assert!(edge.is_call());
            assert!(edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_mq_protocol_variants() {
        let protocols = [
            MqProtocol::Kafka,
            MqProtocol::Sqs,
            MqProtocol::RabbitMq,
            MqProtocol::Nats,
            MqProtocol::Redis,
            MqProtocol::Other(StringId::new(1)),
        ];

        for proto in protocols {
            let edge = EdgeKind::MessageQueue {
                protocol: proto.clone(),
                topic: None,
            };
            assert!(edge.is_async());
            assert!(edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_export_kind_variants() {
        let kinds = [
            ExportKind::Direct,
            ExportKind::Reexport,
            ExportKind::Default,
            ExportKind::Namespace,
        ];

        for kind in kinds {
            let edge = EdgeKind::Exports { kind, alias: None };
            assert_eq!(edge.tag(), "exports");
            assert!(!edge.is_call());
            assert!(!edge.is_structural());
            assert!(!edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_estimated_size() {
        // Unit variants
        assert_eq!(EdgeKind::Defines.estimated_size(), 1);
        assert_eq!(EdgeKind::Contains.estimated_size(), 1);
        assert_eq!(EdgeKind::References.estimated_size(), 1);

        // Calls: u8 + bool = 3
        assert_eq!(calls().estimated_size(), 3);

        // Imports: Option<StringId> + bool = 7
        assert_eq!(imports().estimated_size(), 7);

        // Exports: ExportKind + Option<StringId> = 7
        assert_eq!(exports().estimated_size(), 7);

        // Rust-specific edges
        assert_eq!(
            EdgeKind::LifetimeConstraint {
                constraint_kind: LifetimeConstraintKind::Outlives
            }
            .estimated_size(),
            2
        );
        assert_eq!(
            EdgeKind::MacroExpansion {
                expansion_kind: MacroExpansionKind::Derive,
                is_verified: true
            }
            .estimated_size(),
            3
        );
        assert_eq!(
            EdgeKind::TraitMethodBinding {
                trait_name: StringId::INVALID,
                impl_type: StringId::INVALID,
                is_ambiguous: false
            }
            .estimated_size(),
            10
        );
    }

    // ==================== Rust-Specific Edge Tests ====================

    #[test]
    fn test_lifetime_constraint_kind_variants() {
        let kinds = [
            LifetimeConstraintKind::Outlives,
            LifetimeConstraintKind::TypeBound,
            LifetimeConstraintKind::Reference,
            LifetimeConstraintKind::Static,
            LifetimeConstraintKind::HigherRanked,
            LifetimeConstraintKind::TraitObject,
            LifetimeConstraintKind::ImplTrait,
            LifetimeConstraintKind::Elided,
        ];

        for constraint_kind in kinds {
            let edge = EdgeKind::LifetimeConstraint { constraint_kind };
            assert!(edge.is_rust_specific());
            assert!(edge.is_lifetime_constraint());
            assert!(!edge.is_call());
            assert!(!edge.is_structural());
            assert_eq!(edge.tag(), "lifetime_constraint");
        }
    }

    #[test]
    fn test_macro_expansion_kind_variants() {
        let kinds = [
            MacroExpansionKind::Derive,
            MacroExpansionKind::Attribute,
            MacroExpansionKind::Declarative,
            MacroExpansionKind::Function,
        ];

        for expansion_kind in kinds {
            let edge = EdgeKind::MacroExpansion {
                expansion_kind,
                is_verified: true,
            };
            assert!(edge.is_rust_specific());
            assert!(edge.is_macro_expansion());
            assert!(!edge.is_call());
            assert_eq!(edge.tag(), "macro_expansion");
        }
    }

    #[test]
    fn test_trait_method_binding() {
        let edge = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(1),
            impl_type: StringId::new(2),
            is_ambiguous: false,
        };

        assert!(edge.is_rust_specific());
        assert!(edge.is_trait_method_binding());
        assert!(!edge.is_call());
        assert_eq!(edge.tag(), "trait_method_binding");

        // Test ambiguous binding
        let ambiguous = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(1),
            impl_type: StringId::new(2),
            is_ambiguous: true,
        };
        assert!(ambiguous.is_trait_method_binding());
    }

    #[test]
    fn test_rust_specific_edges_serde() {
        // LifetimeConstraint
        let lifetime = EdgeKind::LifetimeConstraint {
            constraint_kind: LifetimeConstraintKind::HigherRanked,
        };
        let json = serde_json::to_string(&lifetime).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(lifetime, deserialized);

        // TraitMethodBinding
        let binding = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(10),
            impl_type: StringId::new(20),
            is_ambiguous: true,
        };
        let json = serde_json::to_string(&binding).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(binding, deserialized);

        // MacroExpansion
        let expansion = EdgeKind::MacroExpansion {
            expansion_kind: MacroExpansionKind::Derive,
            is_verified: false,
        };
        let json = serde_json::to_string(&expansion).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(expansion, deserialized);
    }

    #[test]
    fn test_rust_specific_edges_postcard() {
        let edges = [
            EdgeKind::LifetimeConstraint {
                constraint_kind: LifetimeConstraintKind::Outlives,
            },
            EdgeKind::TraitMethodBinding {
                trait_name: StringId::new(5),
                impl_type: StringId::new(6),
                is_ambiguous: false,
            },
            EdgeKind::MacroExpansion {
                expansion_kind: MacroExpansionKind::Attribute,
                is_verified: true,
            },
        ];

        for edge in edges {
            let bytes = postcard::to_allocvec(&edge).unwrap();
            let deserialized: EdgeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(edge, deserialized);
        }
    }

    #[test]
    fn test_lifetime_constraint_kind_defaults() {
        assert_eq!(
            LifetimeConstraintKind::default(),
            LifetimeConstraintKind::Outlives
        );
        assert_eq!(
            MacroExpansionKind::default(),
            MacroExpansionKind::Declarative
        );
    }
}
