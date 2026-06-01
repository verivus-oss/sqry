//! Shared traversal result types with `EdgeClassification` and `MaterializedEdge`.
//!
//! These types form the universal output contract for all BFS traversals in sqry.
//! Consumer crates (LSP, MCP, CLI) convert `TraversalResult` into their
//! protocol-specific response types.

#[cfg(test)]
use super::edge::kind::ResolvedVia;
use super::edge::kind::{EdgeKind, ExportKind};
use super::materialize::MaterializedNode;

/// Classification of an edge's semantic intent with preserved metadata.
///
/// Provides a coarse categorization of `EdgeKind` variants for consumers that
/// do not need the full edge semantics. The `From<&EdgeKind>` conversion is
/// exhaustive — no wildcard fallback — so future `EdgeKind` additions produce
/// a compile error forcing conscious classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeClassification {
    /// Function/method call (includes trait method bindings).
    Call {
        /// Whether the call is async (awaited).
        is_async: bool,
        /// Whether the call crosses a language or service boundary.
        is_cross_boundary: bool,
    },
    /// Import statement.
    Import {
        /// Whether this is a wildcard import.
        is_wildcard: bool,
    },
    /// Export statement.
    Export {
        /// Whether this is a re-export.
        is_reexport: bool,
    },
    /// Symbol reference.
    Reference,
    /// Class/trait inheritance.
    Inherits,
    /// Interface/trait implementation.
    Implements,
    /// Structural containment.
    Contains,
    /// Symbol definition.
    Defines,
    /// Type annotation/association.
    TypeOf,
    /// Database access (queries, reads, writes, triggers).
    DatabaseAccess,
    /// Service-level interaction (message queues, websockets, gRPC, etc.).
    ServiceInteraction,
}

impl From<&EdgeKind> for EdgeClassification {
    // Arms are grouped by semantic domain (calls, imports, OOP, JVM classpath, etc.)
    // even when multiple domains map to the same classification variant.
    #[allow(clippy::match_same_arms)]
    fn from(kind: &EdgeKind) -> Self {
        match kind {
            // ---- Calls ----
            EdgeKind::Calls { is_async, .. } => Self::Call {
                is_async: *is_async,
                is_cross_boundary: false,
            },
            EdgeKind::TraitMethodBinding { .. } => Self::Call {
                is_async: false,
                is_cross_boundary: false,
            },
            EdgeKind::FfiCall { .. }
            | EdgeKind::HttpRequest { .. }
            | EdgeKind::GrpcCall { .. }
            | EdgeKind::WebAssemblyCall => Self::Call {
                is_async: false,
                is_cross_boundary: true,
            },

            // ---- Imports / Exports ----
            EdgeKind::Imports { is_wildcard, .. } => Self::Import {
                is_wildcard: *is_wildcard,
            },
            EdgeKind::Exports { kind, .. } => Self::Export {
                is_reexport: matches!(kind, ExportKind::Reexport),
            },

            // ---- References ----
            EdgeKind::References => Self::Reference,

            // ---- OOP ----
            EdgeKind::Inherits | EdgeKind::SealedPermit => Self::Inherits,
            EdgeKind::Implements => Self::Implements,

            // ---- Structural ----
            EdgeKind::Contains | EdgeKind::CompanionOf => Self::Contains,
            EdgeKind::Defines => Self::Defines,

            // ---- Type ----
            EdgeKind::TypeOf { .. } => Self::TypeOf,

            // ---- Database ----
            EdgeKind::DbQuery { .. }
            | EdgeKind::TableRead { .. }
            | EdgeKind::TableWrite { .. }
            | EdgeKind::TriggeredBy { .. } => Self::DatabaseAccess,

            // ---- Service interactions ----
            EdgeKind::MessageQueue { .. }
            | EdgeKind::WebSocket { .. }
            | EdgeKind::GraphQLOperation { .. }
            | EdgeKind::ProcessExec { .. }
            | EdgeKind::FileIpc { .. }
            | EdgeKind::ProtocolCall { .. } => Self::ServiceInteraction,

            // ---- JVM classpath → closest semantic match ----
            EdgeKind::GenericBound | EdgeKind::TypeArgument => Self::TypeOf,
            EdgeKind::AnnotatedWith | EdgeKind::AnnotationParam => Self::Reference,
            EdgeKind::LambdaCaptures | EdgeKind::ExtensionReceiver => Self::Reference,
            EdgeKind::ModuleExports | EdgeKind::ModuleOpens => Self::Export { is_reexport: false },
            EdgeKind::ModuleRequires | EdgeKind::ModuleProvides => {
                Self::Import { is_wildcard: false }
            }

            // ---- Rust-specific ----
            EdgeKind::MacroExpansion { .. } => Self::Reference,
            EdgeKind::LifetimeConstraint { .. } => Self::Reference,

            // ---- T3 error chains (Go) ----
            // Wraps edges are NOT included in standard callers/callees
            // results (T3 design §1.3); explicit `Wraps`-aware consumers
            // walk the edge directly until the Cluster F planner `wraps:`
            // predicate / Cluster G traversal surfaces land. Mapping to
            // `Reference` is the closest match in this taxonomy — same
            // disposition as MacroExpansion / LifetimeConstraint, which
            // are likewise filtered out of call traversal.
            EdgeKind::Wraps { .. } => Self::Reference,
        }
    }
}

/// A materialized edge in a traversal result.
///
/// Indices reference the `nodes` vector in `TraversalResult`. Both
/// `source_idx` and `target_idx` are guaranteed to be `< nodes.len()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedEdge {
    /// Index into `TraversalResult.nodes` for the source node.
    pub source_idx: usize,
    /// Index into `TraversalResult.nodes` for the target node.
    pub target_idx: usize,
    /// Semantic classification with preserved metadata.
    pub classification: EdgeClassification,
    /// Raw edge kind for consumers needing full semantics (e.g., confidence scoring).
    pub raw_kind: EdgeKind,
    /// Traversal depth at which this edge was discovered.
    pub depth: u32,
}

/// Why a traversal was truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationReason {
    /// Maximum traversal depth reached.
    DepthLimit,
    /// Maximum node count reached.
    NodeLimit,
    /// Maximum edge count reached.
    EdgeLimit,
    /// Maximum path count reached (`trace_path`).
    PathLimit,
}

/// Metadata about a completed traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalMetadata {
    /// Why the traversal was truncated, if at all.
    pub truncation: Option<TruncationReason>,
    /// Whether the max depth bound was reached during traversal.
    pub max_depth_reached: bool,
    /// Number of seed nodes the traversal started from.
    pub seed_count: usize,
    /// Total nodes visited during traversal (may exceed nodes in result).
    pub nodes_visited: usize,
    /// Total materialized nodes in the result.
    pub total_nodes: usize,
    /// Total materialized edges in the result.
    pub total_edges: usize,
}

/// Universal traversal result that all BFS implementations produce.
///
/// # Index Invariants
///
/// 1. `nodes` is deduped by `NodeId` — each node appears exactly once.
/// 2. `nodes` is populated in BFS discovery order (first-seen).
/// 3. Every `source_idx`, `target_idx`, and path index is `< nodes.len()`.
/// 4. When any limit triggers, edges and paths referencing truncated nodes are
///    dropped atomically.
/// 5. `metadata.truncation` is `Some(reason)` whenever a limit causes pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalResult {
    /// Materialized nodes (each carries `node_id: NodeId`).
    pub nodes: Vec<MaterializedNode>,
    /// Materialized edges (indices reference `nodes` vector).
    pub edges: Vec<MaterializedEdge>,
    /// Optional ordered paths (indices into `nodes` vector).
    /// Used by `trace_path` for K shortest paths.
    pub paths: Option<Vec<Vec<usize>>>,
    /// Traversal metadata.
    pub metadata: TraversalMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::edge::kind::{
        DbQueryType, ExportKind, FfiConvention, LifetimeConstraintKind, MacroExpansionKind,
    };
    use crate::graph::unified::string::id::StringId;

    /// Helper to create a dummy `StringId` for test edge kinds that require one.
    fn test_string_id() -> StringId {
        StringId::new(1)
    }

    #[test]
    fn calls_async_classification() {
        let edge = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: true,
                is_cross_boundary: false,
            }
        );
    }

    #[test]
    fn ffi_call_cross_boundary() {
        let edge = EdgeKind::FfiCall {
            convention: FfiConvention::C,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: true,
            }
        );
    }

    #[test]
    fn trait_method_binding_classification() {
        let edge = EdgeKind::TraitMethodBinding {
            trait_name: test_string_id(),
            impl_type: test_string_id(),
            is_ambiguous: false,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: false,
            }
        );
    }

    #[test]
    fn exports_reexport_classification() {
        let edge = EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Export { is_reexport: true });
    }

    #[test]
    fn exports_direct_classification() {
        let edge = EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Export { is_reexport: false }
        );
    }

    #[test]
    fn sealed_permit_inherits() {
        let edge = EdgeKind::SealedPermit;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Inherits);
    }

    #[test]
    fn companion_of_contains() {
        let edge = EdgeKind::CompanionOf;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Contains);
    }

    #[test]
    fn generic_bound_type_of() {
        let edge = EdgeKind::GenericBound;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::TypeOf);
    }

    #[test]
    fn module_exports_export() {
        let edge = EdgeKind::ModuleExports;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Export { is_reexport: false }
        );
    }

    #[test]
    fn http_request_cross_boundary() {
        let edge = EdgeKind::HttpRequest {
            method: crate::graph::unified::edge::kind::HttpMethod::Get,
            url: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: true,
            }
        );
    }

    #[test]
    fn db_query_database_access() {
        let edge = EdgeKind::DbQuery {
            query_type: DbQueryType::Select,
            table: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::DatabaseAccess);
    }

    #[test]
    fn macro_expansion_reference() {
        let edge = EdgeKind::MacroExpansion {
            expansion_kind: MacroExpansionKind::Derive,
            is_verified: true,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn lifetime_constraint_reference() {
        let edge = EdgeKind::LifetimeConstraint {
            constraint_kind: LifetimeConstraintKind::Outlives,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn imports_wildcard() {
        let edge = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Import { is_wildcard: true });
    }

    #[test]
    fn inherits_classification() {
        let edge = EdgeKind::Inherits;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Inherits);
    }

    #[test]
    fn implements_classification() {
        let edge = EdgeKind::Implements;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Implements);
    }

    #[test]
    fn references_classification() {
        let edge = EdgeKind::References;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn defines_classification() {
        let edge = EdgeKind::Defines;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Defines);
    }

    #[test]
    fn contains_classification() {
        let edge = EdgeKind::Contains;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Contains);
    }

    #[test]
    fn type_of_classification() {
        let edge = EdgeKind::TypeOf {
            context: None,
            index: None,
            name: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::TypeOf);
    }

    #[test]
    fn message_queue_service_interaction() {
        let edge = EdgeKind::MessageQueue {
            protocol: crate::graph::unified::edge::kind::MqProtocol::Kafka,
            topic: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn websocket_service_interaction() {
        let edge = EdgeKind::WebSocket { event: None };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn grpc_call_cross_boundary() {
        let edge = EdgeKind::GrpcCall {
            service: test_string_id(),
            method: test_string_id(),
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: true,
            }
        );
    }

    #[test]
    fn web_assembly_call_cross_boundary() {
        let edge = EdgeKind::WebAssemblyCall;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: true,
            }
        );
    }

    #[test]
    fn table_read_database_access() {
        let edge = EdgeKind::TableRead {
            table_name: test_string_id(),
            schema: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::DatabaseAccess);
    }

    #[test]
    fn table_write_database_access() {
        let edge = EdgeKind::TableWrite {
            table_name: test_string_id(),
            schema: None,
            operation: crate::graph::unified::edge::kind::TableWriteOp::Insert,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::DatabaseAccess);
    }

    #[test]
    fn triggered_by_database_access() {
        let edge = EdgeKind::TriggeredBy {
            trigger_name: test_string_id(),
            schema: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::DatabaseAccess);
    }

    #[test]
    fn graphql_operation_service_interaction() {
        let edge = EdgeKind::GraphQLOperation {
            operation: test_string_id(),
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn process_exec_service_interaction() {
        let edge = EdgeKind::ProcessExec {
            command: test_string_id(),
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn file_ipc_service_interaction() {
        let edge = EdgeKind::FileIpc { path_pattern: None };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn protocol_call_service_interaction() {
        let edge = EdgeKind::ProtocolCall {
            protocol: test_string_id(),
            metadata: None,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::ServiceInteraction);
    }

    #[test]
    fn annotated_with_reference() {
        let edge = EdgeKind::AnnotatedWith;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn annotation_param_reference() {
        let edge = EdgeKind::AnnotationParam;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn lambda_captures_reference() {
        let edge = EdgeKind::LambdaCaptures;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn extension_receiver_reference() {
        let edge = EdgeKind::ExtensionReceiver;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::Reference);
    }

    #[test]
    fn module_opens_export() {
        let edge = EdgeKind::ModuleOpens;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Export { is_reexport: false }
        );
    }

    #[test]
    fn module_requires_import() {
        let edge = EdgeKind::ModuleRequires;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Import { is_wildcard: false }
        );
    }

    #[test]
    fn module_provides_import() {
        let edge = EdgeKind::ModuleProvides;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Import { is_wildcard: false }
        );
    }

    #[test]
    fn type_argument_type_of() {
        let edge = EdgeKind::TypeArgument;
        let classified = EdgeClassification::from(&edge);
        assert_eq!(classified, EdgeClassification::TypeOf);
    }

    #[test]
    fn calls_sync_classification() {
        let edge = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let classified = EdgeClassification::from(&edge);
        assert_eq!(
            classified,
            EdgeClassification::Call {
                is_async: false,
                is_cross_boundary: false,
            }
        );
    }
}
