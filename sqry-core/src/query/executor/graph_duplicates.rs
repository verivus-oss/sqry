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
}

impl Default for DuplicateConfig {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            max_results: 1000,
            is_exact_only: false,
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
    /// Node IDs of duplicates in this group
    pub node_ids: Vec<NodeId>,
}

/// Compute a hash for duplicate detection based on type
fn compute_hash(graph: &CodeGraph, node_id: NodeId, dup_type: DuplicateType) -> Option<u64> {
    let entry = graph.nodes().get(node_id)?;
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

    // Group nodes by hash
    for (node_id, entry) in graph.nodes().iter() {
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
        .map(|(hash, node_ids)| {
            // For body duplicates, extract the full 128-bit body_hash from the first node
            let body_hash_128 = if dup_type == DuplicateType::Body {
                node_ids
                    .first()
                    .and_then(|&id| graph.nodes().get(id).and_then(|entry| entry.body_hash))
            } else {
                None
            };

            DuplicateGroup {
                hash,
                body_hash_128,
                node_ids,
            }
        })
        .collect();

    // Sort by group size (largest first)
    groups.sort_by(|a, b| b.node_ids.len().cmp(&a.node_ids.len()));

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
}
