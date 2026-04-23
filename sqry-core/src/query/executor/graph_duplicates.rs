//! Duplicate detection for unified graph.
//!
//! This module provides duplicate code detection using the unified graph API,
//! replacing the legacy index-based duplicate detection.

use crate::graph::body_hash::BodyHash128;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::{NodeId, NodeKind};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

/// Type of duplicate detection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateType {
    /// Functions with identical/similar bodies (based on signature when body unavailable)
    Body,
    /// Functions with identical signatures (return type only for now)
    Signature,
    /// Structs with similar field layouts
    Struct,
}

impl DuplicateType {
    /// Parse duplicate type from a query value string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl FromStr for DuplicateType {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_lowercase().as_str() {
            "body" | "function" => Ok(Self::Body),
            "signature" | "sig" => Ok(Self::Signature),
            "struct" | "type" => Ok(Self::Struct),
            _ => Err(anyhow::anyhow!("Unknown duplicate type: {value}")),
        }
    }
}

/// Configuration for duplicate detection
#[derive(Debug, Clone)]
pub struct DuplicateConfig {
    /// Minimum similarity threshold (0.0 - 1.0)
    pub threshold: f64,
    /// Maximum results to return
    #[allow(dead_code)]
    pub max_results: usize,
    /// Include exact duplicates only
    pub is_exact_only: bool,
    /// Maximum number of member nodes to include per group (0 = unlimited).
    ///
    /// When a group has more members than this cap, `node_ids` is truncated to
    /// `max_members_per_group` entries and `DuplicateGroup::members_truncated`
    /// is set to `true`. The full pre-truncation count is always recorded in
    /// `DuplicateGroup::total_members`.
    ///
    /// Default: 10.  Opt-out (pre-v9.1 behavior): set to 0.
    pub max_members_per_group: usize,
}

impl Default for DuplicateConfig {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            max_results: 1000,
            is_exact_only: false,
            max_members_per_group: 10,
        }
    }
}

/// A group of duplicate symbols
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    /// Hash identifying this group (64-bit, for backward compatibility)
    pub hash: u64,
    /// Full 128-bit body hash (only set for body duplicate groups)
    ///
    /// This is used for proper hex string output in CLI/MCP.
    /// When set, this is the actual body hash from the indexed nodes.
    pub body_hash_128: Option<BodyHash128>,
    /// Node IDs of duplicates in this group.
    ///
    /// When `members_truncated` is `true` this slice contains only the first
    /// `DuplicateConfig::max_members_per_group` entries (sorted by file path
    /// then node name).  The full pre-truncation count is in `total_members`.
    pub node_ids: Vec<NodeId>,
    /// Total number of members in this group **before** any per-group cap was
    /// applied.  Always equal to `node_ids.len()` when `members_truncated` is
    /// `false`.
    pub total_members: usize,
    /// `true` when `node_ids` was truncated by `DuplicateConfig::max_members_per_group`.
    pub members_truncated: bool,
}

/// Compute a hash for duplicate detection based on type
fn compute_hash(graph: &CodeGraph, node_id: NodeId, dup_type: DuplicateType) -> Option<u64> {
    let entry = graph.nodes().get(node_id)?;
    // Defense-in-depth: even if a caller forgets to filter unified
    // losers at iteration time, refuse to compute a hash for one.
    // See `NodeEntry::is_unified_loser`.
    if entry.is_unified_loser() {
        return None;
    }
    let strings = graph.strings();

    match dup_type {
        DuplicateType::Body => {
            // Primary: use the precomputed body_hash from the NodeEntry
            // This is the 128-bit hash computed from actual body bytes during indexing
            if let Some(body_hash) = entry.body_hash {
                // Convert u128 to u64 by XOR-folding the two halves
                // This preserves collision resistance for grouping purposes
                return Some(body_hash.high ^ body_hash.low);
            }

            // Fallback for nodes without body_hash: use signature if available
            if let Some(sig_id) = entry.signature
                && let Some(sig) = strings.resolve(sig_id)
            {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                sig.hash(&mut hasher);
                entry.kind.hash(&mut hasher);
                return Some(hasher.finish());
            }

            // Last resort: hash qualified name + kind + line span (approximates body size)
            let name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .or_else(|| strings.resolve(entry.name))?;

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            name.hash(&mut hasher);
            entry.kind.hash(&mut hasher);
            // Include line span as proxy for body size
            let lines = entry.end_line.saturating_sub(entry.start_line);
            lines.hash(&mut hasher);
            Some(hasher.finish())
        }
        DuplicateType::Signature => {
            // Hash signature if available, otherwise name + kind
            if let Some(sig_id) = entry.signature
                && let Some(sig) = strings.resolve(sig_id)
            {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                sig.hash(&mut hasher);
                return Some(hasher.finish());
            }

            // Fallback: hash name + kind
            let name = strings.resolve(entry.name)?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            name.hash(&mut hasher);
            entry.kind.hash(&mut hasher);
            Some(hasher.finish())
        }
        DuplicateType::Struct => {
            // Only consider structs/classes
            if !matches!(entry.kind, NodeKind::Struct | NodeKind::Class) {
                return None;
            }

            // Hash based on struct name and fields (using qualified name as proxy)
            let name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .or_else(|| strings.resolve(entry.name))?;

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            name.hash(&mut hasher);
            entry.kind.hash(&mut hasher);
            Some(hasher.finish())
        }
    }
}

/// Find all duplicate groups in the graph.
///
/// Groups nodes by a hash computed from their metadata, based on the duplicate type:
/// - `Body`: Hash includes kind, signature (or name + line span), for functions/methods
/// - `Signature`: Hash includes only the signature string
/// - `Struct`: Hash includes only the name, for structs/classes only
///
/// # Arguments
///
/// * `duplicate_type` - The type of duplication to detect (Body, Signature, or Struct)
/// * `graph` - The code graph to analyze
/// * `config` - Configuration for exact matching and result limits
///
/// # Returns
///
/// A vector of `DuplicateGroup` structs, each containing a list of node IDs
/// that share the same hash. Groups are sorted by size (largest first) and
/// limited by `config.max_results`. Single-node "groups" are filtered out.
#[must_use]
pub fn build_duplicate_groups_graph(
    dup_type: DuplicateType,
    graph: &CodeGraph,
    config: &DuplicateConfig,
) -> Vec<DuplicateGroup> {
    let mut hash_to_nodes: HashMap<u64, Vec<NodeId>> = HashMap::new();

    // Only consider relevant node kinds for each duplicate type
    let relevant_kinds: Vec<NodeKind> = match dup_type {
        DuplicateType::Body | DuplicateType::Signature => {
            vec![NodeKind::Function, NodeKind::Method]
        }
        DuplicateType::Struct => vec![NodeKind::Struct, NodeKind::Class],
    };

    // Group nodes by hash.
    //
    // Gate 0d iter-2 blocker fix: skip Phase 4c-prime unified-away
    // losers. They remain in the arena as inert duplicates so CSR
    // row_ptr sizing stays stable, but publish-visible query
    // evaluation (including `duplicates:`) MUST NOT surface them.
    // `merge_node_into` now also clears `body_hash` and `signature`
    // on losers as defense-in-depth, but the `is_unified_loser()`
    // guard is the canonical exclusion check — keep both.
    // See `NodeEntry::is_unified_loser` and
    // `sqry-core/src/graph/unified/build/unification.rs`.
    for (node_id, entry) in graph.nodes().iter() {
        if entry.is_unified_loser() {
            continue;
        }
        if !relevant_kinds.contains(&entry.kind) {
            continue;
        }

        if let Some(hash) = compute_hash(graph, node_id, dup_type) {
            hash_to_nodes.entry(hash).or_default().push(node_id);
        }
    }

    // Filter to groups with duplicates and apply threshold
    let mut groups: Vec<DuplicateGroup> = hash_to_nodes
        .into_iter()
        .filter(|(_, nodes)| {
            if config.is_exact_only {
                nodes.len() > 1
            } else {
                // For non-exact matching, we'd need fuzzy comparison
                // For now, treat as exact matching
                nodes.len() > 1
            }
        })
        .map(|(hash, mut node_ids)| {
            // For body duplicates, extract the full 128-bit body_hash from the first node.
            // Skip unified losers defensively — `merge_node_into` clears
            // `body_hash` on them so `entry.body_hash` should already be
            // None, but the explicit guard keeps this site honest even if
            // the clearing policy changes later.
            let body_hash_128 = if dup_type == DuplicateType::Body {
                node_ids.first().and_then(|&id| {
                    graph.nodes().get(id).and_then(|entry| {
                        if entry.is_unified_loser() {
                            None
                        } else {
                            entry.body_hash
                        }
                    })
                })
            } else {
                None
            };

            // Sort node_ids deterministically: by file path, then by node name,
            // then by NodeId as a tiebreaker so ordering is fully stable even
            // when two nodes share the same file and name.
            let strings = graph.strings();
            let files = graph.files();
            node_ids.sort_by(|&a, &b| {
                let path_a = graph
                    .nodes()
                    .get(a)
                    .and_then(|e| files.resolve(e.file))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let path_b = graph
                    .nodes()
                    .get(b)
                    .and_then(|e| files.resolve(e.file))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name_a = graph
                    .nodes()
                    .get(a)
                    .and_then(|e| strings.resolve(e.name))
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                let name_b = graph
                    .nodes()
                    .get(b)
                    .and_then(|e| strings.resolve(e.name))
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                path_a
                    .cmp(&path_b)
                    .then_with(|| name_a.cmp(&name_b))
                    .then_with(|| a.cmp(&b))
            });

            // Record the pre-truncation count and apply the per-group cap.
            let total_members = node_ids.len();
            let members_truncated =
                config.max_members_per_group > 0 && node_ids.len() > config.max_members_per_group;
            if members_truncated {
                node_ids.truncate(config.max_members_per_group);
            }

            DuplicateGroup {
                hash,
                body_hash_128,
                node_ids,
                total_members,
                members_truncated,
            }
        })
        .collect();

    // Sort by group size (largest first), using total_members so ordering
    // reflects the full group even when members are truncated.
    groups.sort_by(|a, b| b.total_members.cmp(&a.total_members));

    // Apply max_results limit
    groups.truncate(config.max_results);

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;

    /// Helper to create a test graph with nodes having specific signatures.
    fn create_test_graph_with_signatures(
        nodes: &[(&str, NodeKind, Option<&str>)],
    ) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let mut node_ids = Vec::new();

        for (name, kind, signature) in nodes {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let mut entry = NodeEntry::new(*kind, name_id, file_id).with_qualified_name(name_id);

            if let Some(sig) = signature {
                let sig_id = graph.strings_mut().intern(sig).unwrap();
                entry = entry.with_signature(sig_id);
            }

            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        (graph, node_ids)
    }

    /// Helper to create nodes with line spans (for body-based hashing fallback).
    fn create_test_graph_with_spans(
        nodes: &[(&str, NodeKind, u32, u32)], // name, kind, start_line, end_line
    ) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let mut node_ids = Vec::new();

        for (name, kind, start_line, end_line) in nodes {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(*kind, name_id, file_id)
                .with_qualified_name(name_id)
                .with_location(*start_line, 0, *end_line, 0);

            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        (graph, node_ids)
    }

    #[test]
    fn test_empty_graph() {
        let graph = CodeGraph::new();
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Body, &graph, &config);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_no_duplicates_unique_signatures() {
        // Three functions with unique signatures - no duplicates
        let nodes = [
            ("func_a", NodeKind::Function, Some("fn func_a() -> i32")),
            ("func_b", NodeKind::Function, Some("fn func_b() -> String")),
            (
                "func_c",
                NodeKind::Function,
                Some("fn func_c(x: i32) -> bool"),
            ),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert!(groups.is_empty(), "No duplicates should be found");
    }

    #[test]
    fn test_signature_duplicates() {
        // Two functions with identical signatures
        let nodes = [
            (
                "func_a",
                NodeKind::Function,
                Some("fn process(x: i32) -> i32"),
            ),
            (
                "func_b",
                NodeKind::Function,
                Some("fn process(x: i32) -> i32"),
            ),
            ("func_c", NodeKind::Function, Some("fn other() -> String")),
        ];
        let (graph, node_ids) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1, "Should find one duplicate group");
        assert_eq!(
            groups[0].node_ids.len(),
            2,
            "Group should have 2 duplicates"
        );
        assert!(groups[0].node_ids.contains(&node_ids[0]));
        assert!(groups[0].node_ids.contains(&node_ids[1]));
    }

    #[test]
    fn test_body_duplicates_with_signatures() {
        // Body duplicates are detected via signature + kind
        let nodes = [
            (
                "func_a",
                NodeKind::Function,
                Some("fn compute(x: i32) -> i32"),
            ),
            (
                "func_b",
                NodeKind::Function,
                Some("fn compute(x: i32) -> i32"),
            ),
            (
                "func_c",
                NodeKind::Method,
                Some("fn compute(x: i32) -> i32"),
            ), // Different kind
        ];
        let (graph, node_ids) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Body, &graph, &config);
        // func_a and func_b should be duplicates (same signature, same kind)
        // func_c is Method, not Function, so different hash
        assert_eq!(groups.len(), 1, "Should find one duplicate group");
        assert_eq!(groups[0].node_ids.len(), 2);
        assert!(groups[0].node_ids.contains(&node_ids[0]));
        assert!(groups[0].node_ids.contains(&node_ids[1]));
    }

    #[test]
    fn test_body_duplicates_fallback_to_name_and_span() {
        // When no signature, use name + kind + line span
        let nodes = [
            ("helper", NodeKind::Function, 10, 20), // 10 lines
            ("helper", NodeKind::Function, 30, 40), // 10 lines (same span size)
            ("other", NodeKind::Function, 50, 60),  // Different name
        ];
        let (graph, node_ids) = create_test_graph_with_spans(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Body, &graph, &config);
        // Nodes with same name, kind, and span should be grouped
        assert_eq!(groups.len(), 1, "Should find one duplicate group");
        assert!(groups[0].node_ids.contains(&node_ids[0]));
        assert!(groups[0].node_ids.contains(&node_ids[1]));
    }

    #[test]
    fn test_struct_duplicates() {
        // Only structs/classes are considered for struct duplicates
        let nodes = [
            ("MyStruct", NodeKind::Struct, None),
            ("MyStruct", NodeKind::Struct, None),
            ("MyStruct", NodeKind::Function, None), // Function, not struct - ignored
            ("OtherStruct", NodeKind::Struct, None),
        ];
        let (graph, node_ids) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Struct, &graph, &config);
        // Only the two MyStruct nodes should be grouped
        assert_eq!(groups.len(), 1, "Should find one duplicate group");
        assert!(groups[0].node_ids.contains(&node_ids[0]));
        assert!(groups[0].node_ids.contains(&node_ids[1]));
        assert!(!groups[0].node_ids.contains(&node_ids[2])); // Function ignored
    }

    #[test]
    fn test_class_duplicates() {
        // Classes are also considered for struct duplicates
        let nodes = [
            ("MyClass", NodeKind::Class, None),
            ("MyClass", NodeKind::Class, None),
            ("OtherClass", NodeKind::Class, None),
        ];
        let (graph, node_ids) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Struct, &graph, &config);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].node_ids.contains(&node_ids[0]));
        assert!(groups[0].node_ids.contains(&node_ids[1]));
    }

    #[test]
    fn test_methods_ignored_for_struct_duplicates() {
        // Methods should not be detected as struct duplicates
        let nodes = [
            ("process", NodeKind::Method, Some("fn process()")),
            ("process", NodeKind::Method, Some("fn process()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Struct, &graph, &config);
        assert!(
            groups.is_empty(),
            "Methods should not be considered for struct duplicates"
        );
    }

    #[test]
    fn test_max_results_limit() {
        // Create many duplicate groups and verify limit is respected
        let mut nodes = Vec::new();
        for i in 0..10 {
            // Each pair has same signature within group
            let sig = format!("fn group{i}()");
            nodes.push((format!("func_{i}_a"), NodeKind::Function, Some(sig.clone())));
            nodes.push((format!("func_{i}_b"), NodeKind::Function, Some(sig)));
        }

        let nodes_ref: Vec<(&str, NodeKind, Option<&str>)> = nodes
            .iter()
            .map(|(name, kind, sig)| (name.as_str(), *kind, sig.as_deref()))
            .collect();
        let (graph, _) = create_test_graph_with_signatures(&nodes_ref);
        let config = DuplicateConfig {
            max_results: 3,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 3, "Should respect max_results limit");
    }

    #[test]
    fn test_groups_sorted_by_size() {
        // Create groups of different sizes and verify they're sorted largest first
        let nodes = [
            // Group of 3
            ("large_a", NodeKind::Function, Some("fn large()")),
            ("large_b", NodeKind::Function, Some("fn large()")),
            ("large_c", NodeKind::Function, Some("fn large()")),
            // Group of 2
            ("small_a", NodeKind::Function, Some("fn small()")),
            ("small_b", NodeKind::Function, Some("fn small()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].node_ids.len(), 3, "Largest group should be first");
        assert_eq!(
            groups[1].node_ids.len(),
            2,
            "Smaller group should be second"
        );
    }

    #[test]
    fn test_single_node_not_duplicate() {
        // A single node is not a duplicate
        let nodes = [
            ("unique_func", NodeKind::Function, Some("fn unique()")),
            ("other_func", NodeKind::Function, Some("fn different()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert!(
            groups.is_empty(),
            "Single nodes should not form duplicate groups"
        );
    }

    #[test]
    fn test_mixed_kinds_not_duplicates() {
        // Same signature but different kinds should not be duplicates
        let nodes = [
            ("process", NodeKind::Function, Some("fn process()")),
            ("process", NodeKind::Method, Some("fn process()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        // For Body type, kind matters in the hash
        let groups = build_duplicate_groups_graph(DuplicateType::Body, &graph, &config);
        assert!(
            groups.is_empty(),
            "Different kinds should not be duplicates for body type"
        );

        // For Signature type, only signature matters
        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(
            groups.len(),
            1,
            "Same signature should be duplicates regardless of kind"
        );
    }

    #[test]
    fn test_multiple_duplicate_groups() {
        // Multiple independent duplicate groups
        let nodes = [
            ("func_a1", NodeKind::Function, Some("fn alpha()")),
            ("func_a2", NodeKind::Function, Some("fn alpha()")),
            ("func_b1", NodeKind::Function, Some("fn beta()")),
            ("func_b2", NodeKind::Function, Some("fn beta()")),
            ("unique", NodeKind::Function, Some("fn gamma()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig::default();

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 2, "Should find two duplicate groups");
    }

    #[test]
    fn test_exact_only_config() {
        // When exact_only is true, behavior should be the same (hash-based)
        let nodes = [
            ("func_a", NodeKind::Function, Some("fn exact()")),
            ("func_b", NodeKind::Function, Some("fn exact()")),
        ];
        let (graph, _) = create_test_graph_with_signatures(&nodes);
        let config = DuplicateConfig {
            is_exact_only: true,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1);
    }

    /// Gate 0d iter-2 regression: the public `duplicates:` query path
    /// MUST skip Phase 4c-prime unified losers. Before the fix,
    /// `graph_duplicates.rs:192` walked every arena entry and its
    /// hash path keyed on `entry.body_hash` / `entry.signature`, both
    /// of which remained on the loser post-merge. This test simulates
    /// that exact shape:
    ///
    /// 1. Winner + loser with identical `body_hash` in different files.
    /// 2. Unify via `merge_node_into`.
    /// 3. Run `build_duplicate_groups_graph` with `DuplicateType::Body`.
    /// 4. Assert the loser is NOT a member of any returned group, and
    ///    that no winner-only group with only one member appears
    ///    (single-node groups are filtered).
    #[test]
    fn duplicates_query_excludes_unified_losers() {
        use crate::graph::body_hash::BodyHash128;
        use crate::graph::unified::build::unification::merge_node_into;

        let mut graph = CodeGraph::new();

        let winner_name = graph.strings_mut().intern("shared_fn").unwrap();
        let winner_qn = graph.strings_mut().intern("mod_a::shared_fn").unwrap();
        let sig = graph.strings_mut().intern("fn shared_fn() -> ()").unwrap();
        let body = BodyHash128 {
            high: 0xDEAD_BEEF,
            low: 0xCAFE_F00D,
        };

        let file_a = graph
            .files_mut()
            .register(Path::new("src/a.rs"))
            .expect("register a");
        let file_b = graph
            .files_mut()
            .register(Path::new("src/b.rs"))
            .expect("register b");

        let (winner_id, loser_id) = {
            let arena = graph.nodes_mut();
            let mut w = NodeEntry::new(NodeKind::Function, winner_name, file_a)
                .with_location(10, 0, 20, 0)
                .with_signature(sig)
                .with_body_hash(body);
            w.qualified_name = Some(winner_qn);
            let w_id = arena.alloc(w).unwrap();

            let mut l = NodeEntry::new(NodeKind::Function, winner_name, file_b)
                .with_location(5, 0, 15, 0)
                .with_signature(sig)
                .with_body_hash(body);
            l.qualified_name = Some(winner_qn);
            let l_id = arena.alloc(l).unwrap();

            (w_id, l_id)
        };
        graph.files_mut().record_node(file_a, winner_id);
        graph.files_mut().record_node(file_b, loser_id);

        // Perform the unification merge (same primitive Phase 4c-prime
        // calls). The iter-2 fix additionally clears signature,
        // body_hash, doc, visibility — so the loser's hash inputs
        // should all be None post-merge.
        merge_node_into(graph.nodes_mut(), loser_id, winner_id).unwrap();

        // Sanity: the loser is marked unified, and its content-
        // addressable metadata is cleared per iter-2 fix.
        let loser_entry = graph.nodes().get(loser_id).expect("loser present");
        assert!(loser_entry.is_unified_loser());
        assert!(loser_entry.body_hash.is_none(), "body_hash must be cleared");
        assert!(loser_entry.signature.is_none(), "signature must be cleared");

        graph.rebuild_indices();

        // The duplicates-by-body query is the publish-visible CD
        // predicate that was leaking losers in iter-2.
        let groups =
            build_duplicate_groups_graph(DuplicateType::Body, &graph, &DuplicateConfig::default());

        // The loser must NOT appear in any returned group. With only
        // one winner and the loser filtered, there's no "group of 2"
        // to return, so the result set should be empty.
        for group in &groups {
            assert!(
                !group.node_ids.contains(&loser_id),
                "Unified loser leaked into `duplicates:body` group: {:?}",
                group.node_ids
            );
        }
        assert!(
            groups.is_empty(),
            "After filtering losers, the winner is alone and no duplicate group should \
             be returned; got {} groups: {groups:?}",
            groups.len(),
        );

        // Also verify the signature duplicate path is free of losers.
        let sig_groups = build_duplicate_groups_graph(
            DuplicateType::Signature,
            &graph,
            &DuplicateConfig::default(),
        );
        for group in &sig_groups {
            assert!(
                !group.node_ids.contains(&loser_id),
                "Unified loser leaked into `duplicates:signature` group: {:?}",
                group.node_ids
            );
        }
        assert!(
            sig_groups.is_empty(),
            "After filtering losers, the winner is alone and no signature duplicate \
             group should be returned; got {} groups",
            sig_groups.len(),
        );
    }

    // -------------------------------------------------------------------------
    // DUP_1 — per-group member cap tests
    // -------------------------------------------------------------------------

    /// Create a graph where `member_count` nodes share the same signature so they
    /// all land in one duplicate group.  Nodes are spread across two files so the
    /// deterministic-ordering test has a non-trivial sort key.
    fn create_large_group_graph(member_count: usize) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_a = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let file_b = graph.files_mut().register(Path::new("src/b.rs")).unwrap();
        let shared_sig = graph
            .strings_mut()
            .intern("fn duplicate_fn() -> i32")
            .unwrap();
        let mut node_ids = Vec::new();

        for i in 0..member_count {
            let name = format!("dup_fn_{i}");
            let name_id = graph.strings_mut().intern(&name).unwrap();
            // Alternate between the two files to make ordering non-trivial.
            let file_id = if i % 2 == 0 { file_a } else { file_b };
            let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
                .with_qualified_name(name_id)
                .with_signature(shared_sig);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            node_ids.push(node_id);
        }

        (graph, node_ids)
    }

    /// Truncation: `max_members_per_group = 3` on a group with 10 members.
    /// Expects: `node_ids.len() == 3`, `total_members == 10`,
    /// `members_truncated == true`.
    #[test]
    fn test_per_group_cap_truncation() {
        let (graph, _) = create_large_group_graph(10);
        let config = DuplicateConfig {
            max_members_per_group: 3,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1, "Expected exactly one duplicate group");

        let group = &groups[0];
        assert_eq!(
            group.total_members, 10,
            "total_members must be pre-truncation count"
        );
        assert!(group.members_truncated, "members_truncated must be true");
        assert_eq!(
            group.node_ids.len(),
            3,
            "displayed node_ids must be capped at max_members_per_group"
        );
    }

    /// `max_members_per_group = 0` disables the cap — all members are returned
    /// and `members_truncated` is `false`.
    #[test]
    fn test_per_group_cap_zero_means_unlimited() {
        let (graph, _) = create_large_group_graph(10);
        let config = DuplicateConfig {
            max_members_per_group: 0,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1);

        let group = &groups[0];
        assert_eq!(group.total_members, 10);
        assert!(
            !group.members_truncated,
            "members_truncated must be false when unlimited"
        );
        assert_eq!(
            group.node_ids.len(),
            10,
            "All members must be returned when cap is 0"
        );
    }

    /// Group with fewer members than the cap — no truncation, `members_truncated`
    /// remains `false`, and `total_members == node_ids.len()`.
    #[test]
    fn test_per_group_cap_no_truncation_when_below_cap() {
        let (graph, _) = create_large_group_graph(5);
        let config = DuplicateConfig {
            max_members_per_group: 10,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1);

        let group = &groups[0];
        assert_eq!(group.total_members, 5);
        assert!(
            !group.members_truncated,
            "members_truncated must be false when count <= cap"
        );
        assert_eq!(
            group.node_ids.len(),
            5,
            "All members must be present when below cap"
        );
    }

    /// A group with exactly 1 displayed member after truncation (cap = 1) is
    /// still returned — single-member *truncated* groups are valid because the
    /// caller knows there are more members via `total_members`.
    #[test]
    fn test_per_group_cap_one_member_displayed() {
        let (graph, _) = create_large_group_graph(5);
        let config = DuplicateConfig {
            max_members_per_group: 1,
            ..Default::default()
        };

        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(
            groups.len(),
            1,
            "Group must not be filtered just because only 1 member is displayed"
        );

        let group = &groups[0];
        assert_eq!(group.total_members, 5);
        assert!(group.members_truncated);
        assert_eq!(group.node_ids.len(), 1);
    }

    /// Members in each group are sorted deterministically by (file_path, node_name, NodeId).
    /// Nodes are inserted in reverse-alphabetical order; after sorting the first
    /// element must be the lexicographically smallest (file, name) pair.
    #[test]
    fn test_per_group_deterministic_ordering() {
        let mut graph = CodeGraph::new();
        let file_a = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let file_b = graph.files_mut().register(Path::new("src/b.rs")).unwrap();
        let shared_sig = graph.strings_mut().intern("fn stable_fn() -> i32").unwrap();

        // Insert in "wrong" order (b.rs before a.rs; z before a within a file)
        // to verify stable sorting regardless of insertion/HashMap order.
        let names_and_files: &[(&str, _)] = &[
            ("z_func", file_b),
            ("m_func", file_b),
            ("a_func", file_b),
            ("z_func", file_a),
            ("a_func", file_a),
        ];

        let mut inserted: Vec<(String, String, NodeId)> = Vec::new();
        for (name, file_id) in names_and_files {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(NodeKind::Function, name_id, *file_id)
                .with_qualified_name(name_id)
                .with_signature(shared_sig);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            let path = if *file_id == file_a {
                "src/a.rs".to_owned()
            } else {
                "src/b.rs".to_owned()
            };
            inserted.push((path, (*name).to_owned(), node_id));
        }

        // Compute expected order: sort by (file_path, name, NodeId).
        let mut expected = inserted.clone();
        expected.sort_by(|(pa, na, ia), (pb, nb, ib)| {
            pa.cmp(pb).then_with(|| na.cmp(nb)).then_with(|| ia.cmp(ib))
        });
        let expected_ids: Vec<NodeId> = expected.iter().map(|(_, _, id)| *id).collect();

        let config = DuplicateConfig {
            max_members_per_group: 0, // unlimited — don't truncate
            ..Default::default()
        };
        let groups = build_duplicate_groups_graph(DuplicateType::Signature, &graph, &config);
        assert_eq!(groups.len(), 1, "Expected one duplicate group");
        assert_eq!(
            groups[0].node_ids, expected_ids,
            "node_ids must be in deterministic (file_path, name, NodeId) order"
        );
        assert_eq!(groups[0].total_members, 5);
        assert!(!groups[0].members_truncated);
    }
}
