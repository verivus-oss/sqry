//! Unified graph test helpers for inspecting staged call edges.

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use std::collections::HashMap;

/// Details about a staged call edge resolved to caller/callee names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdgeInfo {
    pub caller: String,
    pub callee: String,
    pub argument_count: u8,
    pub is_async: bool,
}

/// Collect all call edges from staging operations with resolved names.
#[must_use]
pub fn collect_call_edges(staging: &StagingGraph) -> Vec<CallEdgeInfo> {
    let node_names = build_node_name_lookup(staging);
    collect_call_edges_with_lookup(staging, &node_names)
}

/// Collect all call edges using language-native display names.
#[must_use]
pub fn collect_call_edges_for_language(
    staging: &StagingGraph,
    language: Language,
) -> Vec<CallEdgeInfo> {
    let node_names = build_node_display_name_lookup(staging, language);
    collect_call_edges_with_lookup(staging, &node_names)
}

fn collect_call_edges_with_lookup(
    staging: &StagingGraph,
    node_names: &HashMap<u32, String>,
) -> Vec<CallEdgeInfo> {
    staging
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
                    },
                ..
            } = op
            {
                let source_name = node_names
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                let target_name = node_names
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                Some(CallEdgeInfo {
                    caller: source_name,
                    callee: target_name,
                    argument_count: *argument_count,
                    is_async: *is_async,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Find a call edge by matching substrings in caller and callee names.
#[must_use]
pub fn find_call_edge(
    staging: &StagingGraph,
    source_substring: &str,
    target_substring: &str,
) -> Option<CallEdgeInfo> {
    collect_call_edges(staging).into_iter().find(|edge| {
        edge.caller.contains(source_substring) && edge.callee.contains(target_substring)
    })
}

/// Find a call edge by matching substrings in language-native display names.
#[must_use]
pub fn find_call_edge_for_language(
    staging: &StagingGraph,
    language: Language,
    source_substring: &str,
    target_substring: &str,
) -> Option<CallEdgeInfo> {
    collect_call_edges_for_language(staging, language)
        .into_iter()
        .find(|edge| {
            edge.caller.contains(source_substring) && edge.callee.contains(target_substring)
        })
}

/// Assert a call edge exists, returning its resolved metadata.
///
/// # Panics
///
/// Panics if no call edge matches the provided caller/callee substrings.
#[allow(
    clippy::must_use_candidate,
    reason = "assert helpers are typically used for their side effects in tests"
)]
pub fn assert_has_call_edge(
    staging: &StagingGraph,
    source_substring: &str,
    target_substring: &str,
) -> CallEdgeInfo {
    if let Some(edge) = find_call_edge(staging, source_substring, target_substring) {
        return edge;
    }

    let call_edges = collect_call_edges(staging);
    let formatted = if call_edges.is_empty() {
        "  (none)".to_string()
    } else {
        call_edges
            .iter()
            .map(|edge| {
                format!(
                    "  {caller} -> {callee} (args={argument_count}, async={is_async})",
                    caller = edge.caller,
                    callee = edge.callee,
                    argument_count = edge.argument_count,
                    is_async = edge.is_async
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    panic!(
        "Expected call edge matching '{source_substring}' -> '{target_substring}' not found.\nStaged call edges:\n{formatted}"
    );
}

/// Assert a call edge exists using language-native display names.
#[allow(
    clippy::must_use_candidate,
    reason = "assert helpers are typically used for their side effects in tests"
)]
pub fn assert_has_call_edge_for_language(
    staging: &StagingGraph,
    language: Language,
    source_substring: &str,
    target_substring: &str,
) -> CallEdgeInfo {
    if let Some(edge) =
        find_call_edge_for_language(staging, language, source_substring, target_substring)
    {
        return edge;
    }

    let call_edges = collect_call_edges_for_language(staging, language);
    let formatted = if call_edges.is_empty() {
        "  (none)".to_string()
    } else {
        call_edges
            .iter()
            .map(|edge| {
                format!(
                    "  {caller} -> {callee} (args={argument_count}, async={is_async})",
                    caller = edge.caller,
                    callee = edge.callee,
                    argument_count = edge.argument_count,
                    is_async = edge.is_async
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    panic!(
        "Expected call edge matching '{source_substring}' -> '{target_substring}' not found.\nStaged call edges:\n{formatted}"
    );
}

/// Assert a call edge exists and includes span metadata.
///
/// # Panics
///
/// Panics if no matching call edge exists or if the matching edge lacks a span.
#[allow(
    clippy::must_use_candidate,
    reason = "assert helpers are typically used for their side effects in tests"
)]
pub fn assert_call_edge_has_span(
    staging: &StagingGraph,
    source_substring: &str,
    target_substring: &str,
) -> sqry_core::graph::Span {
    let node_names = build_node_name_lookup(staging);
    let mut matched_without_span = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Calls { .. },
            spans,
            ..
        } = op
        {
            let source_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| "<unknown>".to_string());
            let target_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| "<unknown>".to_string());

            if source_name.contains(source_substring) && target_name.contains(target_substring) {
                if let Some(span) = spans.first() {
                    return *span;
                }
                matched_without_span.push((source_name, target_name));
            }
        }
    }

    if matched_without_span.is_empty() {
        let call_edges = collect_call_edges(staging);
        let formatted = if call_edges.is_empty() {
            "  (none)".to_string()
        } else {
            call_edges
                .iter()
                .map(|edge| {
                    format!(
                        "  {caller} -> {callee} (args={argument_count}, async={is_async})",
                        caller = edge.caller,
                        callee = edge.callee,
                        argument_count = edge.argument_count,
                        is_async = edge.is_async
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "Expected call edge matching '{source_substring}' -> '{target_substring}' not found.\nStaged call edges:\n{formatted}"
        );
    }

    let formatted = matched_without_span
        .iter()
        .map(|(caller, callee)| format!("  {caller} -> {callee}"))
        .collect::<Vec<_>>()
        .join("\n");

    panic!(
        "Call edge(s) matching '{source_substring}' -> '{target_substring}' missing span metadata:\n{formatted}"
    );
}

/// Assert a call edge exists and includes span metadata using display names.
#[allow(
    clippy::must_use_candidate,
    reason = "assert helpers are typically used for their side effects in tests"
)]
pub fn assert_call_edge_has_span_for_language(
    staging: &StagingGraph,
    language: Language,
    source_substring: &str,
    target_substring: &str,
) -> sqry_core::graph::Span {
    let node_names = build_node_display_name_lookup(staging, language);
    let mut matched_without_span = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::Calls { .. },
            spans,
            ..
        } = op
        {
            let source_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| "<unknown>".to_string());
            let target_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| "<unknown>".to_string());

            if source_name.contains(source_substring) && target_name.contains(target_substring) {
                if let Some(span) = spans.first() {
                    return *span;
                }
                matched_without_span.push((source_name, target_name));
            }
        }
    }

    if matched_without_span.is_empty() {
        let call_edges = collect_call_edges_for_language(staging, language);
        let formatted = if call_edges.is_empty() {
            "  (none)".to_string()
        } else {
            call_edges
                .iter()
                .map(|edge| {
                    format!(
                        "  {caller} -> {callee} (args={argument_count}, async={is_async})",
                        caller = edge.caller,
                        callee = edge.callee,
                        argument_count = edge.argument_count,
                        is_async = edge.is_async
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "Expected call edge matching '{source_substring}' -> '{target_substring}' not found.\nStaged call edges:\n{formatted}"
        );
    }

    let formatted = matched_without_span
        .iter()
        .map(|(caller, callee)| format!("  {caller} -> {callee}"))
        .collect::<Vec<_>>()
        .join("\n");

    panic!(
        "Call edge(s) matching '{source_substring}' -> '{target_substring}' missing span metadata:\n{formatted}"
    );
}

#[must_use]
pub fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map from staged `AddNode` operations.
///
/// Nodes missing an expected ID are skipped to avoid index collisions.
#[must_use]
pub fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map using language-native display names.
#[must_use]
pub fn build_node_display_name_lookup(
    staging: &StagingGraph,
    language: Language,
) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = staging.resolve_node_display_name(language, entry)?;
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}
