//! Cross-crate macro resolution (4.5d).
//!
//! Runs in `entrypoint.rs` after Pass 4 (cross-file resolution) and before
//! Pass 5 (cross-language edges). This is NOT a `GraphBuilder` trait method —
//! it operates on a committed `GraphSnapshot` and `ExportMap` to resolve macro
//! invocations to their definitions in other workspace crates.
//!
//! # Resolution Strategy
//!
//! 1. **Same-workspace macros:** Resolve macro paths against the `ExportMap`.
//!    If a `macro_rules!` or proc-macro function is exported from another crate
//!    in the workspace, create a `Calls` edge from the invocation `CallSite` to
//!    the resolved `Macro` definition.
//!
//! 2. **`#[macro_use] extern crate`:** Resolve unqualified macros against the
//!    crate's `#[macro_export]` items in the `ExportMap`.
//!
//! 3. **External crates:** If the macro path is not in the workspace, annotate
//!    with `external_crate` metadata (no edge — we don't have the definition).

use sqry_core::graph::unified::build::pass4_cross::ExportMap;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::node::{NodeId, NodeKind};

/// A pending cross-crate macro edge to be added to the graph.
///
/// Follows the same pattern as `PendingCrossLanguageEdge` from Pass 5.
#[derive(Debug, Clone)]
pub struct PendingMacroEdge {
    /// Source node (the `CallSite` invoking the macro).
    pub source: NodeId,
    /// Target node (the Macro definition).
    pub target: NodeId,
    /// Edge kind (always `Calls` for macro resolution).
    pub kind: EdgeKind,
    /// File containing the source node.
    pub file: FileId,
}

/// Resolve macro invocations to definitions in other workspace crates.
///
/// Iterates over all `CallSite` nodes in the given Rust files, checks whether
/// their qualified names reference a macro defined elsewhere in the workspace,
/// and returns new edges to add.
///
/// This function does NOT mutate the graph — it returns edges that the caller
/// merges into the graph in the same pattern as Pass 5.
///
/// # Arguments
///
/// * `snapshot` — immutable graph snapshot with all nodes/edges from Passes 1-4
/// * `export_map` — exported symbols from all workspace files (from Pass 4)
/// * `rust_file_ids` — file IDs for Rust source files in the workspace
///
/// # Returns
///
/// Vector of pending edges to merge into the graph.
#[must_use]
pub fn resolve_cross_crate_macros(
    snapshot: &GraphSnapshot,
    export_map: &ExportMap,
    rust_file_ids: &[FileId],
) -> Vec<PendingMacroEdge> {
    let mut new_edges = Vec::new();

    // Iterate over all nodes in the arena and filter to Rust files + CallSite kind.
    let rust_file_set: std::collections::HashSet<FileId> = rust_file_ids.iter().copied().collect();

    for (node_id, entry) in snapshot.nodes().iter() {
        // Gate 0d iter-2 fix: skip unified losers from cross-crate
        // macro resolution. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        // Only process CallSite nodes in Rust files.
        if entry.kind != NodeKind::CallSite {
            continue;
        }
        if !rust_file_set.contains(&entry.file) {
            continue;
        }

        // Resolve the qualified name string.
        let Some(qualified_name) = entry
            .qualified_name
            .and_then(|sid| snapshot.strings().resolve(sid))
        else {
            continue;
        };

        attempt_cross_crate_resolution(
            snapshot,
            export_map,
            node_id,
            &qualified_name,
            entry.file,
            &mut new_edges,
        );
    }

    if !new_edges.is_empty() {
        log::info!(
            "Pass 4.5: resolved {} cross-crate macro edges across {} Rust files",
            new_edges.len(),
            rust_file_ids.len()
        );
    }

    new_edges
}

/// Attempt to resolve a single macro invocation across crates.
///
/// Checks the `ExportMap` for a macro definition matching the callsite's target.
/// If found in a different file, creates a `Calls` edge.
fn attempt_cross_crate_resolution(
    snapshot: &GraphSnapshot,
    export_map: &ExportMap,
    callsite_id: NodeId,
    qualified_name: &str,
    source_file: FileId,
    new_edges: &mut Vec<PendingMacroEdge>,
) {
    // Extract the potential macro path from the callsite qualified name.
    let macro_path = extract_macro_target_path(qualified_name);
    if macro_path.is_empty() {
        return;
    }

    // Look up the macro path in the export map (cross-file resolution).
    if let Some((_target_file, target_node)) =
        export_map.lookup_cross_file(&macro_path, source_file)
    {
        // Verify the target is actually a Macro node.
        if let Some(target_entry) = snapshot.nodes().get(target_node)
            && target_entry.kind == NodeKind::Macro
        {
            // Check if this edge already exists.
            let already_exists = snapshot
                .edges()
                .edges_from(callsite_id)
                .iter()
                .any(|e| e.target == target_node);

            if !already_exists {
                new_edges.push(PendingMacroEdge {
                    source: callsite_id,
                    target: target_node,
                    kind: EdgeKind::Calls {
                        argument_count: 255, // unknown
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    file: source_file,
                });

                log::debug!(
                    "Resolved cross-crate macro: {callsite_id:?} -> {target_node:?} ({macro_path})",
                );
            }
        }
    }
}

/// Extract the macro target path from a callsite qualified name.
///
/// `CallSite` qualified names for attribute macros follow the pattern:
/// `item_qualified::attr_macro_path@line:col`
///
/// For regular macro invocations:
/// `module::macro_name` or just `macro_name`
///
/// This function extracts the macro path that can be looked up in the `ExportMap`.
fn extract_macro_target_path(qualified_name: &str) -> String {
    // Strip the @line:col suffix if present.
    let without_location = qualified_name
        .rsplit_once('@')
        .map_or(qualified_name, |(base, _)| base);

    // Check for attribute macro pattern: look for "::attr_" in the path.
    if let Some(attr_pos) = without_location.find("::attr_") {
        let attr_part = &without_location[attr_pos + "::attr_".len()..];
        // Convert underscores back to :: for path lookup.
        // But only the first set — "tokio_main" → "tokio::main"
        // This is a heuristic; the naming convention uses _ as :: separator.
        return attr_part.replacen('_', "::", 1);
    }

    // For regular macro invocations, the qualified name IS the macro path.
    without_location.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_macro_target_path_attribute() {
        assert_eq!(
            extract_macro_target_path("main::attr_tokio_main@1:0"),
            "tokio::main"
        );
    }

    #[test]
    fn test_extract_macro_target_path_simple() {
        assert_eq!(
            extract_macro_target_path("my_module::my_macro"),
            "my_module::my_macro"
        );
    }

    #[test]
    fn test_extract_macro_target_path_with_location() {
        assert_eq!(extract_macro_target_path("my_macro@5:0"), "my_macro");
    }

    #[test]
    fn test_extract_macro_target_path_no_attr() {
        assert_eq!(extract_macro_target_path("println"), "println");
    }
}
