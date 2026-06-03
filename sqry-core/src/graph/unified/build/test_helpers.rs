//! Test helpers for asserting on `StagingGraph` operations.
//!
//! This module provides utilities for testing `GraphBuilder` implementations
//! by inspecting staged operations before they are committed to the graph.
//!
//! # Wave 0 Deliverable 0.3
//!
//! These helpers enable language plugin tests to validate edge extraction
//! by asserting on the staged operations rather than querying the committed graph.
#![allow(clippy::similar_names)] // Test helpers use caller/callee and importer/imported wording.
//!
//! # Usage
//!
//! ```ignore
//! use sqry_core::graph::unified::build::test_helpers::*;
//!
//! // After running GraphBuilder::build_graph()
//! let call_edges = collect_call_edges(&staging);
//! assert_eq!(call_edges.len(), 3);
//!
//! // Assert specific edges exist
//! assert_has_call_edge(&staging, "main", "helper");
//! assert_has_import_edge(&staging, "app", "utils");
//! ```

use super::super::edge::{EdgeKind, ResolvedVia};
use super::super::node::NodeKind;
use super::staging::{StagingGraph, StagingOp};

/// Collected edge information for assertions.
#[derive(Debug, Clone)]
pub struct StagedEdge<'a> {
    /// The edge kind from the staging operation.
    pub kind: &'a EdgeKind,
    /// Source node name (if resolvable from staged nodes).
    pub source_name: Option<String>,
    /// Target node name (if resolvable from staged nodes).
    pub target_name: Option<String>,
}

/// Collect all edges of a specific kind from staging operations.
///
/// This function filters `StagingOp::AddEdge` operations that match the
/// provided edge kind discriminant.
///
/// # Arguments
///
/// * `staging` - The staging graph containing operations
/// * `kind_name` - The edge kind name to filter by (e.g., "Calls", "Imports", "Exports")
///
/// # Returns
///
/// A vector of references to matching `StagingOp::AddEdge` operations.
///
/// # Example
///
/// ```ignore
/// let call_ops = collect_edges_by_kind(&staging, "Calls");
/// assert_eq!(call_ops.len(), 5, "Expected 5 call edges");
/// ```
#[must_use]
pub fn collect_edges_by_kind<'a>(staging: &'a StagingGraph, kind_name: &str) -> Vec<&'a StagingOp> {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                edge_kind_name(kind) == kind_name
            } else {
                false
            }
        })
        .collect()
}

/// Collect all call edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for call edges.
///
/// # Example
///
/// ```ignore
/// let calls = collect_call_edges(&staging);
/// for op in &calls {
///     if let StagingOp::AddEdge { kind: EdgeKind::Calls { argument_count, is_async }, .. } = op {
///         println!("Call with {} args, async={}", argument_count, is_async);
///     }
/// }
/// ```
#[must_use]
pub fn collect_call_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Calls")
}

/// Collect all import edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for import edges.
#[must_use]
pub fn collect_import_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Imports")
}

/// Collect all export edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for export edges.
#[must_use]
pub fn collect_export_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Exports")
}

/// Collect all inherits edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for inheritance edges.
#[must_use]
pub fn collect_inherits_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Inherits")
}

/// Collect all implements edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for interface implementation edges.
#[must_use]
pub fn collect_implements_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Implements")
}

/// Collect all `DbQuery` edges from staging operations.
///
/// Convenience wrapper around `collect_edges_by_kind` for database query edges.
#[must_use]
pub fn collect_db_query_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "DbQuery")
}

/// Build a name lookup map from staged `InternString` operations.
///
/// Returns a map from local `StringId` index to the interned string value.
/// This is useful for resolving node names from their `StringId` references.
#[must_use]
pub fn build_string_lookup(staging: &StagingGraph) -> std::collections::HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                // Extract the index from the local StringId
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map from staged `AddNode` operations.
///
/// Returns a map from `NodeId` to the node's name (resolved via string lookup).
/// Prefers `qualified_name` if available, falling back to `name`.
///
/// **Note:** Uses full `NodeId` (including generation) as the key to avoid
/// ambiguous matches when indices collide with different generations.
#[must_use]
pub fn build_node_name_lookup(
    staging: &StagingGraph,
) -> std::collections::HashMap<super::super::node::id::NodeId, String> {
    let strings = build_string_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let node_id = (*expected_id)?;

                // Prefer qualified name if available
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();

                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_id, name))
            } else {
                None
            }
        })
        .collect()
}

/// Assert that a call edge exists between two named nodes.
///
/// This function searches staged operations for a `Calls` edge where:
/// - The source node has the specified caller name
/// - The target node has the specified callee name
///
/// # Panics
///
/// Panics if no matching call edge is found, with a message listing all
/// staged call edges for debugging.
///
/// # Example
///
/// ```ignore
/// // After building graph from: fn main() { helper(); }
/// assert_has_call_edge(&staging, "main", "helper");
/// ```
pub fn assert_has_call_edge(staging: &StagingGraph, caller: &str, callee: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Calls { .. },
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == caller) && target_name.is_some_and(|n| n == callee)
        } else {
            false
        }
    });

    if !found {
        let call_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind:
                        EdgeKind::Calls {
                            argument_count,
                            is_async,
                            resolved_via: ResolvedVia::Direct,
                        },
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!(
                        "  {src} -> {tgt} (args={argument_count}, async={is_async})"
                    ))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected call edge from '{}' to '{}' not found.\nStaged call edges:\n{}",
            caller,
            callee,
            if call_edges.is_empty() {
                "  (none)".to_string()
            } else {
                call_edges.join("\n")
            }
        );
    }
}

/// Assert that an import edge exists between two named nodes.
///
/// This function searches staged operations for an `Imports` edge where:
/// - The source node has the specified importer name
/// - The target node has the specified imported name
///
/// # Panics
///
/// Panics if no matching import edge is found, with a message listing all
/// staged import edges for debugging.
///
/// # Example
///
/// ```ignore
/// // After building graph from: import { helper } from './utils';
/// assert_has_import_edge(&staging, "app", "helper");
/// ```
pub fn assert_has_import_edge(staging: &StagingGraph, importer: &str, imported: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Imports { .. },
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == importer) && target_name.is_some_and(|n| n == imported)
        } else {
            false
        }
    });

    if !found {
        let import_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Imports { alias, is_wildcard },
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!(
                        "  {src} -> {tgt} (alias={alias:?}, wildcard={is_wildcard})"
                    ))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected import edge from '{}' to '{}' not found.\nStaged import edges:\n{}",
            importer,
            imported,
            if import_edges.is_empty() {
                "  (none)".to_string()
            } else {
                import_edges.join("\n")
            }
        );
    }
}

/// Assert that an export edge exists between two named nodes.
///
/// This function searches staged operations for an `Exports` edge where:
/// - The source node has the specified module name
/// - The target node has the specified exported symbol name
///
/// # Panics
///
/// Panics if no matching export edge is found, with a message listing all
/// staged export edges for debugging.
///
/// # Example
///
/// ```ignore
/// // After building graph from: export { helper };
/// assert_has_export_edge(&staging, "utils", "helper");
/// ```
pub fn assert_has_export_edge(staging: &StagingGraph, module: &str, exported: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Exports { .. },
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == module) && target_name.is_some_and(|n| n == exported)
        } else {
            false
        }
    });

    if !found {
        let export_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Exports { kind, alias },
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!("  {src} -> {tgt} (kind={kind:?}, alias={alias:?})"))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected export edge from '{}' to '{}' not found.\nStaged export edges:\n{}",
            module,
            exported,
            if export_edges.is_empty() {
                "  (none)".to_string()
            } else {
                export_edges.join("\n")
            }
        );
    }
}

/// Assert that an inherits edge exists between two named nodes.
///
/// # Panics
///
/// Panics if no matching inherits edge is found.
///
/// # Example
///
/// ```ignore
/// // After building graph from: class Child extends Parent { }
/// assert_has_inherits_edge(&staging, "Child", "Parent");
/// ```
pub fn assert_has_inherits_edge(staging: &StagingGraph, child: &str, parent: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Inherits,
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == child) && target_name.is_some_and(|n| n == parent)
        } else {
            false
        }
    });

    if !found {
        let inherits_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Inherits,
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!("  {src} -> {tgt}"))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected inherits edge from '{}' to '{}' not found.\nStaged inherits edges:\n{}",
            child,
            parent,
            if inherits_edges.is_empty() {
                "  (none)".to_string()
            } else {
                inherits_edges.join("\n")
            }
        );
    }
}

/// Assert that an implements edge exists between two named nodes.
///
/// # Panics
///
/// Panics if no matching implements edge is found.
///
/// # Example
///
/// ```ignore
/// // After building graph from: class Foo implements Bar { }
/// assert_has_implements_edge(&staging, "Foo", "Bar");
/// ```
pub fn assert_has_implements_edge(staging: &StagingGraph, implementor: &str, interface: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Implements,
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == implementor)
                && target_name.is_some_and(|n| n == interface)
        } else {
            false
        }
    });

    if !found {
        let impl_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::Implements,
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!("  {src} -> {tgt}"))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected implements edge from '{}' to '{}' not found.\nStaged implements edges:\n{}",
            implementor,
            interface,
            if impl_edges.is_empty() {
                "  (none)".to_string()
            } else {
                impl_edges.join("\n")
            }
        );
    }
}

/// Assert that at least one staged node has a name containing the given substring.
///
/// Uses `resolve_node_name()` to look up names from staged `InternString` operations.
///
/// # Panics
///
/// Panics if no matching node is found, listing all staged node names for debugging.
pub fn assert_has_node(staging: &StagingGraph, name_substring: &str) {
    let found = staging.nodes().any(|n| {
        staging
            .resolve_node_name(n.entry)
            .is_some_and(|name| name.contains(name_substring))
    });

    if !found {
        let all_names: Vec<String> = staging
            .nodes()
            .map(|n| {
                let name = staging.resolve_node_name(n.entry).unwrap_or("<unresolved>");
                format!("  {:?}: {name}", n.entry.kind)
            })
            .collect();
        panic!(
            "Expected node with name containing '{name_substring}' not found.\nStaged nodes:\n{}",
            if all_names.is_empty() {
                "  (none)".to_string()
            } else {
                all_names.join("\n")
            }
        );
    }
}

/// Assert that at least one staged node has a name containing the given substring
/// and matches the specified `NodeKind`.
///
/// # Panics
///
/// Panics if no matching node is found, listing all staged nodes for debugging.
pub fn assert_has_node_with_kind(staging: &StagingGraph, name_substring: &str, kind: NodeKind) {
    let found = staging.nodes().any(|n| {
        n.entry.kind == kind
            && staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains(name_substring))
    });

    if !found {
        let matching_kind: Vec<String> = staging
            .nodes()
            .filter(|n| n.entry.kind == kind)
            .map(|n| {
                let name = staging.resolve_node_name(n.entry).unwrap_or("<unresolved>");
                format!("  {name}")
            })
            .collect();
        panic!(
            "Expected {kind:?} node with name containing '{name_substring}' not found.\n\
             Staged {kind:?} nodes:\n{}",
            if matching_kind.is_empty() {
                "  (none)".to_string()
            } else {
                matching_kind.join("\n")
            }
        );
    }
}

/// Assert that at least one staged node has a name exactly matching the given string.
///
/// Unlike `assert_has_node` which uses substring matching, this function requires
/// an exact match to prevent false positives (e.g., "main" matching "domain").
///
/// # Panics
///
/// Panics if no exactly matching node is found, listing all staged node names for debugging.
pub fn assert_has_node_exact(staging: &StagingGraph, exact_name: &str) {
    let found = staging.nodes().any(|n| {
        staging
            .resolve_node_name(n.entry)
            .is_some_and(|name| name == exact_name)
    });

    if !found {
        let all_names: Vec<String> = staging
            .nodes()
            .map(|n| {
                let name = staging.resolve_node_name(n.entry).unwrap_or("<unresolved>");
                format!("  {:?}: {name}", n.entry.kind)
            })
            .collect();
        panic!(
            "Expected node with exact name '{exact_name}' not found.\nStaged nodes:\n{}",
            if all_names.is_empty() {
                "  (none)".to_string()
            } else {
                all_names.join("\n")
            }
        );
    }
}

/// Assert that at least one staged node has a name exactly matching the given string
/// and matches the specified `NodeKind`.
///
/// Unlike `assert_has_node_with_kind` which uses substring matching, this function
/// requires an exact match to prevent false positives.
///
/// # Panics
///
/// Panics if no exactly matching node is found, listing all staged nodes of the kind for debugging.
pub fn assert_has_node_with_kind_exact(staging: &StagingGraph, exact_name: &str, kind: NodeKind) {
    let found = staging.nodes().any(|n| {
        n.entry.kind == kind
            && staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name == exact_name)
    });

    if !found {
        let matching_kind: Vec<String> = staging
            .nodes()
            .filter(|n| n.entry.kind == kind)
            .map(|n| {
                let name = staging.resolve_node_name(n.entry).unwrap_or("<unresolved>");
                format!("  {name}")
            })
            .collect();
        panic!(
            "Expected {kind:?} node with exact name '{exact_name}' not found.\n\
             Staged {kind:?} nodes:\n{}",
            if matching_kind.is_empty() {
                "  (none)".to_string()
            } else {
                matching_kind.join("\n")
            }
        );
    }
}

/// Count the number of staged nodes matching the given `NodeKind`.
#[must_use]
pub fn count_nodes_by_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging.nodes().filter(|n| n.entry.kind == kind).count()
}

/// Assert that an `FfiCall` edge exists between two named nodes.
///
/// # Panics
///
/// Panics if no matching FFI call edge is found, listing all staged FFI call edges.
pub fn assert_has_ffi_call_edge(staging: &StagingGraph, caller: &str, callee: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::FfiCall { .. },
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == caller) && target_name.is_some_and(|n| n == callee)
        } else {
            false
        }
    });

    if !found {
        let ffi_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::FfiCall { .. },
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!("  {src} -> {tgt}"))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected FfiCall edge from '{}' to '{}' not found.\nStaged FfiCall edges:\n{}",
            caller,
            callee,
            if ffi_edges.is_empty() {
                "  (none)".to_string()
            } else {
                ffi_edges.join("\n")
            }
        );
    }
}

/// Collect all `FfiCall` edges from staging operations.
#[must_use]
pub fn collect_ffi_call_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "FfiCall")
}

/// Assert that a `References` edge exists between two named nodes.
///
/// # Panics
///
/// Panics if no matching references edge is found.
pub fn assert_has_references_edge(staging: &StagingGraph, from: &str, to: &str) {
    let node_names = build_node_name_lookup(staging);

    let found = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::References,
            ..
        } = op
        {
            let source_name = node_names.get(source);
            let target_name = node_names.get(target);
            source_name.is_some_and(|n| n == from) && target_name.is_some_and(|n| n == to)
        } else {
            false
        }
    });

    if !found {
        let ref_edges: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind: EdgeKind::References,
                    ..
                } = op
                {
                    let src = node_names
                        .get(source)
                        .map_or("<unknown>", std::string::String::as_str);
                    let tgt = node_names
                        .get(target)
                        .map_or("<unknown>", std::string::String::as_str);
                    Some(format!("  {src} -> {tgt}"))
                } else {
                    None
                }
            })
            .collect();

        panic!(
            "Expected References edge from '{}' to '{}' not found.\nStaged References edges:\n{}",
            from,
            to,
            if ref_edges.is_empty() {
                "  (none)".to_string()
            } else {
                ref_edges.join("\n")
            }
        );
    }
}

/// Collect all `Contains` edges from staging operations.
#[must_use]
pub fn collect_contains_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Contains")
}

/// Collect all `Defines` edges from staging operations.
#[must_use]
pub fn collect_defines_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "Defines")
}

/// Collect all `References` edges from staging operations.
#[must_use]
pub fn collect_references_edges(staging: &StagingGraph) -> Vec<&StagingOp> {
    collect_edges_by_kind(staging, "References")
}

/// Get the discriminant name of an `EdgeKind` variant.
fn edge_kind_name(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Defines => "Defines",
        EdgeKind::Contains => "Contains",
        EdgeKind::Calls { .. } => "Calls",
        EdgeKind::References => "References",
        EdgeKind::Imports { .. } => "Imports",
        EdgeKind::Exports { .. } => "Exports",
        EdgeKind::TypeOf { .. } => "TypeOf",
        EdgeKind::Inherits => "Inherits",
        EdgeKind::Implements => "Implements",
        EdgeKind::FfiCall { .. } => "FfiCall",
        EdgeKind::HttpRequest { .. } => "HttpRequest",
        EdgeKind::GrpcCall { .. } => "GrpcCall",
        EdgeKind::WebAssemblyCall => "WebAssemblyCall",
        EdgeKind::DbQuery { .. } => "DbQuery",
        EdgeKind::TableRead { .. } => "TableRead",
        EdgeKind::TableWrite { .. } => "TableWrite",
        EdgeKind::TriggeredBy { .. } => "TriggeredBy",
        EdgeKind::MessageQueue { .. } => "MessageQueue",
        EdgeKind::WebSocket { .. } => "WebSocket",
        EdgeKind::GraphQLOperation { .. } => "GraphQLOperation",
        EdgeKind::ProcessExec { .. } => "ProcessExec",
        EdgeKind::FileIpc { .. } => "FileIpc",
        EdgeKind::ProtocolCall { .. } => "ProtocolCall",
        // Rust-specific edge kinds
        EdgeKind::LifetimeConstraint { .. } => "LifetimeConstraint",
        EdgeKind::TraitMethodBinding { .. } => "TraitMethodBinding",
        EdgeKind::MacroExpansion { .. } => "MacroExpansion",
        // JVM Classpath (Track C)
        EdgeKind::GenericBound => "GenericBound",
        EdgeKind::AnnotatedWith => "AnnotatedWith",
        EdgeKind::AnnotationParam => "AnnotationParam",
        EdgeKind::LambdaCaptures => "LambdaCaptures",
        EdgeKind::ModuleExports => "ModuleExports",
        EdgeKind::ModuleRequires => "ModuleRequires",
        EdgeKind::ModuleOpens => "ModuleOpens",
        EdgeKind::ModuleProvides => "ModuleProvides",
        EdgeKind::TypeArgument => "TypeArgument",
        EdgeKind::ExtensionReceiver => "ExtensionReceiver",
        EdgeKind::CompanionOf => "CompanionOf",
        EdgeKind::SealedPermit => "SealedPermit",
        // T3 error chains (Go)
        EdgeKind::Wraps { .. } => "Wraps",
        // T2.4 / T2.5 Go channel pairing + generic instantiation
        EdgeKind::ChannelPeer { .. } => "ChannelPeer",
        EdgeKind::Instantiates { .. } => "Instantiates",
    }
}

/// Count staged operations by type.
///
/// Returns a summary of operation counts useful for test assertions.
///
/// # Example
///
/// ```ignore
/// let counts = count_operations(&staging);
/// assert_eq!(counts.nodes, 5, "Expected 5 nodes");
/// assert_eq!(counts.edges, 10, "Expected 10 edges");
/// ```
#[derive(Debug, Clone, Default)]
pub struct OperationCounts {
    /// Number of `AddNode` operations.
    pub nodes: usize,
    /// Number of `AddEdge` operations.
    pub edges: usize,
    /// Number of `RegisterFile` operations.
    pub files: usize,
    /// Number of `InternString` operations.
    pub strings: usize,
}

/// Count staging operations by type.
#[must_use]
pub fn count_operations(staging: &StagingGraph) -> OperationCounts {
    let mut counts = OperationCounts::default();

    for op in staging.operations() {
        match op {
            StagingOp::AddNode { .. } => counts.nodes += 1,
            StagingOp::AddEdge { .. } => counts.edges += 1,
            StagingOp::RegisterFile { .. } => counts.files += 1,
            StagingOp::InternString { .. } => counts.strings += 1,
        }
    }

    counts
}

/// Print a debug summary of all staged operations.
///
/// Useful for debugging test failures by showing what was actually staged.
///
/// # Example
///
/// ```ignore
/// // If a test fails, print what we got:
/// debug_print_operations(&staging);
/// ```
pub fn debug_print_operations(staging: &StagingGraph) {
    let node_names = build_node_name_lookup(staging);
    let counts = count_operations(staging);

    eprintln!("=== Staged Operations Summary ===");
    eprintln!(
        "Nodes: {}, Edges: {}, Files: {}, Strings: {}",
        counts.nodes, counts.edges, counts.files, counts.strings
    );
    eprintln!();

    eprintln!("--- Nodes ---");
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op {
            let name = expected_id
                .and_then(|id| node_names.get(&id))
                .map_or("<unknown>", std::string::String::as_str);
            eprintln!("  {:?}: {} ({:?})", expected_id, name, entry.kind);
        }
    }
    eprintln!();

    eprintln!("--- Edges ---");
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            let src = node_names
                .get(source)
                .map_or("<unknown>", std::string::String::as_str);
            let tgt = node_names
                .get(target)
                .map_or("<unknown>", std::string::String::as_str);
            eprintln!("  {src} -> {tgt} ({kind:?})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::StringId;
    use crate::graph::unified::build::StagingGraph;
    use crate::graph::unified::edge::{EdgeKind, ExportKind};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;

    fn make_test_staging() -> StagingGraph {
        let mut staging = StagingGraph::new();

        // Intern strings
        staging.intern_string(StringId::new(0), "main".to_string());
        staging.intern_string(StringId::new(1), "helper".to_string());
        staging.intern_string(StringId::new(2), "utils".to_string());

        // Add nodes
        let main_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(0),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let main_id = staging.add_node(main_entry);

        let helper_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let helper_id = staging.add_node(helper_entry);

        let utils_entry = NodeEntry {
            kind: NodeKind::Module,
            name: StringId::new(2),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 0,
            end_line: 1,
            end_column: 20,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let utils_id = staging.add_node(utils_entry);

        // Add edges
        staging.add_edge(
            main_id,
            helper_id,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(0),
        );

        staging.add_edge(
            main_id,
            utils_id,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            FileId::new(0),
        );

        staging
    }

    #[test]
    fn test_collect_edges_by_kind() {
        let staging = make_test_staging();

        let call_edges = collect_edges_by_kind(&staging, "Calls");
        assert_eq!(call_edges.len(), 1);

        let import_edges = collect_edges_by_kind(&staging, "Imports");
        assert_eq!(import_edges.len(), 1);

        let export_edges = collect_edges_by_kind(&staging, "Exports");
        assert_eq!(export_edges.len(), 0);
    }

    #[test]
    fn test_collect_call_edges() {
        let staging = make_test_staging();
        let calls = collect_call_edges(&staging);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn test_assert_has_call_edge_success() {
        let staging = make_test_staging();
        assert_has_call_edge(&staging, "main", "helper");
    }

    #[test]
    #[should_panic(
        expected = "Expected call edge from 'main' to 'nonexistent' not found.\nStaged call edges:"
    )]
    fn test_assert_has_call_edge_failure() {
        let staging = make_test_staging();
        assert_has_call_edge(&staging, "main", "nonexistent");
    }

    #[test]
    fn test_assert_has_import_edge_success() {
        let staging = make_test_staging();
        assert_has_import_edge(&staging, "main", "utils");
    }

    #[test]
    fn test_count_operations() {
        let staging = make_test_staging();
        let counts = count_operations(&staging);

        assert_eq!(counts.nodes, 3);
        assert_eq!(counts.edges, 2);
        assert_eq!(counts.strings, 3);
    }

    #[test]
    fn test_edge_kind_name() {
        assert_eq!(
            edge_kind_name(&EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }),
            "Calls"
        );
        assert_eq!(
            edge_kind_name(&EdgeKind::Imports {
                alias: None,
                is_wildcard: false
            }),
            "Imports"
        );
        assert_eq!(
            edge_kind_name(&EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None
            }),
            "Exports"
        );
        assert_eq!(edge_kind_name(&EdgeKind::Inherits), "Inherits");
        assert_eq!(edge_kind_name(&EdgeKind::Implements), "Implements");
    }

    /// Create a staging graph with OOP edge types (inherits, implements, exports).
    #[allow(clippy::too_many_lines)] // Comprehensive OOP staging setup requires many node/edge declarations
    fn make_oop_staging() -> StagingGraph {
        let mut staging = StagingGraph::new();

        // Intern strings
        staging.intern_string(StringId::new(0), "Child".to_string());
        staging.intern_string(StringId::new(1), "Parent".to_string());
        staging.intern_string(StringId::new(2), "MyClass".to_string());
        staging.intern_string(StringId::new(3), "MyInterface".to_string());
        staging.intern_string(StringId::new(4), "utils".to_string());
        staging.intern_string(StringId::new(5), "helper".to_string());

        // Add nodes
        let child_entry = NodeEntry {
            kind: NodeKind::Class,
            name: StringId::new(0),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let child_id = staging.add_node(child_entry);

        let parent_entry = NodeEntry {
            kind: NodeKind::Class,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let parent_id = staging.add_node(parent_entry);

        let myclass_entry = NodeEntry {
            kind: NodeKind::Class,
            name: StringId::new(2),
            file: FileId::new(0),
            start_byte: 200,
            end_byte: 300,
            start_line: 21,
            start_column: 0,
            end_line: 30,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let myclass_id = staging.add_node(myclass_entry);

        let interface_entry = NodeEntry {
            kind: NodeKind::Interface,
            name: StringId::new(3),
            file: FileId::new(0),
            start_byte: 300,
            end_byte: 400,
            start_line: 31,
            start_column: 0,
            end_line: 40,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let interface_id = staging.add_node(interface_entry);

        let utils_entry = NodeEntry {
            kind: NodeKind::Module,
            name: StringId::new(4),
            file: FileId::new(0),
            start_byte: 400,
            end_byte: 500,
            start_line: 41,
            start_column: 0,
            end_line: 50,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let utils_id = staging.add_node(utils_entry);

        let helper_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(5),
            file: FileId::new(0),
            start_byte: 500,
            end_byte: 600,
            start_line: 51,
            start_column: 0,
            end_line: 60,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let helper_id = staging.add_node(helper_entry);

        // Add edges: Child extends Parent
        staging.add_edge(child_id, parent_id, EdgeKind::Inherits, FileId::new(0));

        // Add edges: MyClass implements MyInterface
        staging.add_edge(
            myclass_id,
            interface_id,
            EdgeKind::Implements,
            FileId::new(0),
        );

        // Add edges: utils exports helper
        staging.add_edge(
            utils_id,
            helper_id,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            FileId::new(0),
        );

        staging
    }

    /// Create a staging graph with database query edges.
    fn make_db_staging() -> StagingGraph {
        let mut staging = StagingGraph::new();

        // Intern strings
        staging.intern_string(StringId::new(0), "get_users".to_string());
        staging.intern_string(StringId::new(1), "users_table".to_string());

        // Add nodes
        let func_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(0),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let func_id = staging.add_node(func_entry);

        // NodeKind::Other is used for database tables since there's no dedicated
        // NodeKind::Table variant - tables are external entities referenced by queries
        let table_entry = NodeEntry {
            kind: NodeKind::Other,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let table_id = staging.add_node(table_entry);

        // Add DbQuery edge
        staging.add_edge(
            func_id,
            table_id,
            EdgeKind::DbQuery {
                query_type: crate::graph::unified::edge::DbQueryType::Select,
                table: None,
            },
            FileId::new(0),
        );

        staging
    }

    #[test]
    fn test_collect_export_edges() {
        let staging = make_oop_staging();
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 1);
    }

    #[test]
    fn test_collect_inherits_edges() {
        let staging = make_oop_staging();
        let inherits = collect_inherits_edges(&staging);
        assert_eq!(inherits.len(), 1);
    }

    #[test]
    fn test_collect_implements_edges() {
        let staging = make_oop_staging();
        let implements = collect_implements_edges(&staging);
        assert_eq!(implements.len(), 1);
    }

    #[test]
    fn test_collect_db_query_edges() {
        let staging = make_db_staging();
        let db_queries = collect_db_query_edges(&staging);
        assert_eq!(db_queries.len(), 1);
    }

    #[test]
    fn test_assert_has_export_edge_success() {
        let staging = make_oop_staging();
        assert_has_export_edge(&staging, "utils", "helper");
    }

    #[test]
    #[should_panic(
        expected = "Expected export edge from 'utils' to 'nonexistent' not found.\nStaged export edges:"
    )]
    fn test_assert_has_export_edge_failure() {
        let staging = make_oop_staging();
        assert_has_export_edge(&staging, "utils", "nonexistent");
    }

    #[test]
    fn test_assert_has_inherits_edge_success() {
        let staging = make_oop_staging();
        assert_has_inherits_edge(&staging, "Child", "Parent");
    }

    #[test]
    #[should_panic(
        expected = "Expected inherits edge from 'Child' to 'Unknown' not found.\nStaged inherits edges:"
    )]
    fn test_assert_has_inherits_edge_failure() {
        let staging = make_oop_staging();
        assert_has_inherits_edge(&staging, "Child", "Unknown");
    }

    #[test]
    fn test_assert_has_implements_edge_success() {
        let staging = make_oop_staging();
        assert_has_implements_edge(&staging, "MyClass", "MyInterface");
    }

    #[test]
    #[should_panic(
        expected = "Expected implements edge from 'MyClass' to 'Unknown' not found.\nStaged implements edges:"
    )]
    fn test_assert_has_implements_edge_failure() {
        let staging = make_oop_staging();
        assert_has_implements_edge(&staging, "MyClass", "Unknown");
    }

    #[test]
    fn test_debug_print_operations_does_not_panic() {
        let staging = make_test_staging();
        // Just verify it doesn't panic - output goes to stderr
        debug_print_operations(&staging);
    }

    #[test]
    fn test_collect_import_edges() {
        let staging = make_test_staging();
        let imports = collect_import_edges(&staging);
        assert_eq!(imports.len(), 1);
    }

    #[test]
    #[should_panic(
        expected = "Expected import edge from 'main' to 'nonexistent' not found.\nStaged import edges:"
    )]
    fn test_assert_has_import_edge_failure() {
        let staging = make_test_staging();
        assert_has_import_edge(&staging, "main", "nonexistent");
    }

    /// Create a staging graph with aliased export edges.
    fn make_aliased_export_staging() -> StagingGraph {
        let mut staging = StagingGraph::new();

        // Intern strings
        staging.intern_string(StringId::new(0), "myModule".to_string());
        staging.intern_string(StringId::new(1), "internalFunc".to_string());
        staging.intern_string(StringId::new(2), "publicAlias".to_string());

        // Add module node
        let module_entry = NodeEntry {
            kind: NodeKind::Module,
            name: StringId::new(0),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let module_id = staging.add_node(module_entry);

        // Add function node
        let func_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let func_id = staging.add_node(func_entry);

        // Add aliased export: export { internalFunc as publicAlias }
        staging.add_edge(
            module_id,
            func_id,
            EdgeKind::Exports {
                kind: ExportKind::Reexport,
                alias: Some(StringId::new(2)),
            },
            FileId::new(0),
        );

        staging
    }

    /// Create a staging graph with `DbQuery` that has a table name.
    fn make_db_with_table_staging() -> StagingGraph {
        let mut staging = StagingGraph::new();

        // Intern strings
        staging.intern_string(StringId::new(0), "fetchOrders".to_string());
        staging.intern_string(StringId::new(1), "orders".to_string());

        // Add function node
        let func_entry = NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(0),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let func_id = staging.add_node(func_entry);

        // NodeKind::Other for table (no dedicated variant)
        let table_entry = NodeEntry {
            kind: NodeKind::Other,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let table_id = staging.add_node(table_entry);

        // Add DbQuery edge with explicit table reference
        staging.add_edge(
            func_id,
            table_id,
            EdgeKind::DbQuery {
                query_type: crate::graph::unified::edge::DbQueryType::Select,
                table: Some(StringId::new(1)),
            },
            FileId::new(0),
        );

        staging
    }

    #[test]
    fn test_collect_export_edges_with_alias() {
        let staging = make_aliased_export_staging();
        let exports = collect_export_edges(&staging);
        assert_eq!(exports.len(), 1);

        // Verify the edge has the expected alias
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, alias },
            ..
        } = &exports[0]
        {
            assert_eq!(*kind, ExportKind::Reexport);
            assert!(alias.is_some());
        } else {
            panic!("Expected Exports edge");
        }
    }

    #[test]
    fn test_collect_db_query_edges_with_table() {
        let staging = make_db_with_table_staging();
        let db_queries = collect_db_query_edges(&staging);
        assert_eq!(db_queries.len(), 1);

        // Verify the edge has the expected table reference
        if let StagingOp::AddEdge {
            kind: EdgeKind::DbQuery { query_type, table },
            ..
        } = &db_queries[0]
        {
            assert_eq!(
                *query_type,
                crate::graph::unified::edge::DbQueryType::Select
            );
            assert!(table.is_some());
        } else {
            panic!("Expected DbQuery edge");
        }
    }

    // ==================== New Helper Tests ====================

    #[test]
    fn test_assert_has_node_success() {
        let staging = make_test_staging();
        assert_has_node(&staging, "main");
        assert_has_node(&staging, "helper");
        assert_has_node(&staging, "utils");
    }

    #[test]
    #[should_panic(expected = "Expected node with name containing 'nonexistent' not found.")]
    fn test_assert_has_node_failure() {
        let staging = make_test_staging();
        assert_has_node(&staging, "nonexistent");
    }

    #[test]
    fn test_assert_has_node_with_kind_success() {
        let staging = make_test_staging();
        assert_has_node_with_kind(&staging, "main", NodeKind::Function);
        assert_has_node_with_kind(&staging, "utils", NodeKind::Module);
    }

    #[test]
    #[should_panic(expected = "Expected Module node with name containing 'main' not found.")]
    fn test_assert_has_node_with_kind_wrong_kind() {
        let staging = make_test_staging();
        // "main" exists but is a Function, not a Module
        assert_has_node_with_kind(&staging, "main", NodeKind::Module);
    }

    #[test]
    fn test_count_nodes_by_kind() {
        let staging = make_test_staging();
        assert_eq!(count_nodes_by_kind(&staging, NodeKind::Function), 2);
        assert_eq!(count_nodes_by_kind(&staging, NodeKind::Module), 1);
        assert_eq!(count_nodes_by_kind(&staging, NodeKind::Class), 0);
    }

    #[test]
    fn test_collect_ffi_call_edges_empty() {
        let staging = make_test_staging();
        let ffi_edges = collect_ffi_call_edges(&staging);
        assert!(ffi_edges.is_empty());
    }

    #[test]
    fn test_collect_contains_edges() {
        let staging = make_test_staging();
        let contains = collect_contains_edges(&staging);
        assert!(contains.is_empty());
    }

    #[test]
    fn test_collect_defines_edges() {
        let staging = make_test_staging();
        let defines = collect_defines_edges(&staging);
        assert!(defines.is_empty());
    }

    #[test]
    fn test_collect_references_edges() {
        let staging = make_test_staging();
        let refs = collect_references_edges(&staging);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_exact_match_helpers_vs_substring() {
        use crate::graph::unified::StringId;
        use crate::graph::unified::file::FileId;
        use crate::graph::unified::storage::NodeEntry;

        let mut staging = StagingGraph::new();

        // Add nodes with names that could cause false positives with substring matching
        staging.intern_string(StringId::new(0), "main".to_string());
        staging.intern_string(StringId::new(1), "domain".to_string());
        staging.intern_string(StringId::new(2), "main_helper".to_string());

        staging.add_node(NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(0), // "main"
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 0,
            end_line: 3,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        });

        staging.add_node(NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(1), // "domain"
            file: FileId::new(0),
            start_byte: 11,
            end_byte: 20,
            start_line: 4,
            start_column: 0,
            end_line: 6,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        });

        staging.add_node(NodeEntry {
            kind: NodeKind::Function,
            name: StringId::new(2), // "main_helper"
            file: FileId::new(0),
            start_byte: 21,
            end_byte: 30,
            start_line: 7,
            start_column: 0,
            end_line: 9,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        });

        // Substring matching should find all three (they all contain "main")
        assert_has_node(&staging, "main");

        // Exact matching should only find the exact "main" node
        assert_has_node_exact(&staging, "main");
        assert_has_node_with_kind_exact(&staging, "main", NodeKind::Function);

        // Exact match should NOT find partial matches
        // (These would panic if we ran them, which proves they reject substring matches)

        // Verify exact match rejects "domain" when looking for "main"
        let result = std::panic::catch_unwind(|| {
            assert_has_node_exact(&staging, "does_not_exist");
        });
        assert!(
            result.is_err(),
            "Exact match should panic for non-existent node"
        );
    }

    #[test]
    #[should_panic(expected = "Expected node with exact name 'nonexistent'")]
    fn test_assert_has_node_exact_panics_when_not_found() {
        let staging = make_test_staging();
        assert_has_node_exact(&staging, "nonexistent");
    }

    #[test]
    #[should_panic(expected = "Expected Function node with exact name 'nonexistent'")]
    fn test_assert_has_node_with_kind_exact_panics_when_not_found() {
        let staging = make_test_staging();
        assert_has_node_with_kind_exact(&staging, "nonexistent", NodeKind::Function);
    }
}
