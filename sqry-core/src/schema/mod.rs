//! Canonical schema types for sqry.
//!
//! This module provides the **single source of truth** for all semantic types
//! used across sqry interfaces (CLI, LSP, MCP). Interface-specific wire types
//! should derive from or reference these canonical definitions.
//!
//! # Design Goals
//!
//! 1. **Centralized definitions** - All semantic enums defined once
//! 2. **Cross-interface consistency** - LSP and MCP use same underlying types
//! 3. **Documentation** - Each type is fully documented for API consumers
//! 4. **Serialization** - All types support JSON and postcard serialization
//!
//! # Type Categories
//!
//! ## Graph Types (re-exported from `graph::unified`)
//! - [`NodeKind`](crate::graph::unified::node::NodeKind) - Categories of code symbols (function, class, method, etc.)
//! - [`EdgeKind`](crate::graph::unified::edge::EdgeKind) - Relationship types between symbols (calls, imports, etc.)
//!
//! ## Query Types
//! - [`RelationKind`](crate::schema::RelationKind) - Relation query types (callers, callees, imports, exports, returns)
//! - [`Visibility`](crate::schema::Visibility) - Node visibility filter (public, private)
//!
//! ## Output Types
//! - [`OutputFormat`](crate::schema::OutputFormat) - Graph/result output formats (json, dot, d2, mermaid)
//!
//! ## Analysis Types
//! - [`ChangeKind`](crate::schema::ChangeKind) - Semantic diff change types (added, removed, modified, etc.)
//! - [`CycleKind`](crate::schema::CycleKind) - Cycle detection types (calls, imports, modules)
//! - [`DuplicateKind`](crate::schema::DuplicateKind) - Duplicate detection types (body, signature, struct)
//! - [`UnusedScope`](crate::schema::UnusedScope) - Unused symbol scope (public, private, function, etc.)
//!
//! # Usage
//!
//! Interface packages should import these types and optionally create thin wrappers
//! for JSON Schema generation (MCP) or protocol-specific serialization (LSP):
//!
//! ```rust,ignore
//! use sqry_core::schema::{RelationKind, Visibility, OutputFormat};
//!
//! // MCP can derive schemars::JsonSchema on a wrapper if needed
//! #[derive(schemars::JsonSchema)]
//! #[serde(transparent)]
//! pub struct RelationTypeParam(RelationKind);
//! ```

mod change;
mod cycle;
mod duplicate;
mod format;
mod relation;
mod unused;
mod visibility;

// Re-export all canonical types
pub use change::ChangeKind;
pub use cycle::CycleKind;
pub use duplicate::DuplicateKind;
pub use format::OutputFormat;
pub use relation::RelationKind;
pub use unused::UnusedScope;
pub use visibility::Visibility;

// Re-export graph types as canonical schema types
pub use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
pub use crate::graph::unified::node::NodeKind;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_types_are_exported() {
        // Verify all types are accessible
        let _ = RelationKind::Callers;
        let _ = Visibility::Public;
        let _ = OutputFormat::Json;
        let _ = ChangeKind::Added;
        let _ = CycleKind::Calls;
        let _ = DuplicateKind::Body;
        let _ = UnusedScope::All;
        let _ = NodeKind::Function;
        let _ = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
    }

    #[test]
    fn test_json_serialization() {
        // All types should serialize to JSON consistently
        assert_eq!(
            serde_json::to_string(&RelationKind::Callers).unwrap(),
            "\"callers\""
        );
        assert_eq!(
            serde_json::to_string(&Visibility::Public).unwrap(),
            "\"public\""
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Mermaid).unwrap(),
            "\"mermaid\""
        );
        assert_eq!(
            serde_json::to_string(&ChangeKind::SignatureChanged).unwrap(),
            "\"signature_changed\""
        );
    }
}
